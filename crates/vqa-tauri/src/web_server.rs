//! 局域网 Web 访问：内嵌前端资源 + 受口令保护的 HTTP API。
//!
//! 设计口径（与桌面端保持一致，少复制业务逻辑）：
//! - 服务跑在 Tauri 主进程里，API 直接调用现有 `#[tauri::command]` 函数；
//! - 浏览器端仍用原前端，`api.ts` 在非 Tauri 环境改走 `/api/invoke`；
//! - 播放器依赖原生 mpv 子窗口，不在 Web 端开放；检测/控制/报告全量可用；
//! - 局域网请求必须带访问码；本机回环地址免登录。

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Mutex, RwLock,
    },
};

use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Query, Request, State as AxumState},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

use crate::{alarm_cmds, burst_cmds, capture_cmds, commands, compliance_cmds, sync_cmds};

static UI: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui/dist");

const DEFAULT_PORT: u16 = 8766;
const CODE_KEY: &str = "web.access_code";
const ENABLED_KEY: &str = "web.enabled";
const PORT_KEY: &str = "web.port";

// ═══════════════════ 状态 ═══════════════════

pub struct WebState {
    running: Arc<AtomicBool>,
    port: Arc<AtomicU16>,
    code: Arc<RwLock<String>>,
    error: Arc<Mutex<Option<String>>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl WebState {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            port: Arc::new(AtomicU16::new(DEFAULT_PORT)),
            code: Arc::new(RwLock::new(String::new())),
            error: Arc::new(Mutex::new(None)),
            shutdown: Mutex::new(None),
        }
    }

    fn ensure_code(&self) -> String {
        let mut g = self.code.write().unwrap_or_else(|e| e.into_inner());
        if g.is_empty() {
            let saved: String = vqa_core::config::shared_get_or(CODE_KEY, String::new());
            *g = if saved.trim().len() >= 4 {
                saved
            } else {
                new_code()
            };
            let _ = vqa_core::config::shared_set(CODE_KEY, json!(&*g));
        }
        g.clone()
    }

    fn set_code(&self, code: String) {
        *self.code.write().unwrap_or_else(|e| e.into_inner()) = code.clone();
        let _ = vqa_core::config::shared_set(CODE_KEY, json!(code));
    }

    fn set_error(&self, e: Option<String>) {
        *self.error.lock().unwrap_or_else(|x| x.into_inner()) = e;
    }
}

fn new_code() -> String {
    format!("{:08}", rand::random::<u32>() % 100_000_000)
}

#[derive(Serialize)]
pub struct WebStatus {
    running: bool,
    enabled: bool,
    port: u16,
    code: String,
    lan_ip: Option<String>,
    url: String,
    local_url: String,
    login_url: String,
    error: Option<String>,
}

fn status_of(st: &WebState) -> WebStatus {
    let code = st.ensure_code();
    let port = if st.running.load(Ordering::Relaxed) {
        st.port.load(Ordering::Relaxed)
    } else {
        vqa_core::config::shared_get_or(PORT_KEY, DEFAULT_PORT)
    };
    let lan_ip = lan_ip();
    let url = match &lan_ip {
        Some(ip) => format!("http://{ip}:{port}"),
        None => format!("http://127.0.0.1:{port}"),
    };
    let login_url = format!("{url}/login?code={code}");
    let enabled: bool = vqa_core::config::shared_get_or(ENABLED_KEY, false);
    WebStatus {
        running: st.running.load(Ordering::Relaxed),
        enabled: enabled || st.running.load(Ordering::Relaxed),
        port,
        code,
        lan_ip,
        url,
        local_url: format!("http://127.0.0.1:{port}"),
        login_url,
        error: st.error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    }
}

fn lan_ip() -> Option<String> {
    let rows: Vec<_> = vqa_core::netif::choices_detail()
        .into_iter()
        .filter(|n| n.up)
        .filter(|n| {
            let ip = n.ip.as_str();
            let Ok(v4) = ip.parse::<std::net::Ipv4Addr>() else {
                return false;
            };
            let o = v4.octets();
            (o[0] == 10)
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 172 && (16..=31).contains(&o[1]))
        })
        .collect();
    // 优先物理网卡，排除 VPN / Hyper-V / VMware / EasyTier 等虚拟接口；
    // 否则“局域网地址”很容易给成 172.18.x 这类只在宿主机内部可达的地址。
    let mut best: Option<(i32, String)> = None;
    for n in &rows {
        let t = format!("{} {}", n.name, n.desc).to_lowercase();
        let virtual_like = [
            "virtual",
            "vmware",
            "hyper-v",
            "default switch",
            "tunnel",
            "vpn",
            "wireguard",
            "tap-",
            "loopback",
            "nodebaby",
            "bluetooth",
        ]
        .iter()
        .any(|k| t.contains(k));
        if virtual_like {
            continue;
        }
        let physical = [
            "realtek",
            "intel",
            "broadcom",
            "atheros",
            "marvell",
            "killer",
            "gbe",
            "wi-fi",
            "wireless",
            "wlan",
            "802.11",
            "以太网",
            "无线",
        ]
        .iter()
        .filter(|k| t.contains(**k))
        .count() as i32;
        let score = physical * 10 + if t.contains("ethernet") { 4 } else { 0 };
        if best.as_ref().map_or(true, |(s, _)| score > *s) {
            best = Some((score, n.ip.clone()));
        }
    }
    best.map(|(_, ip)| ip)
        .or_else(|| rows.first().map(|n| n.ip.clone()))
}

// ═══════════════════ 服务启动 / 停止 ═══════════════════

struct Ctx {
    app: AppHandle,
    code: Arc<RwLock<String>>,
}

impl Ctx {
    fn code(&self) -> String {
        self.code.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

async fn start_server(app: AppHandle, port: u16) -> Result<WebStatus, String> {
    if !(1024..=65535).contains(&port) {
        return Err("端口范围应为 1024–65535".into());
    }
    let st = app.state::<WebState>();
    st.ensure_code();
    let current = st.running.load(Ordering::Relaxed);
    let current_port = st.port.load(Ordering::Relaxed);
    if current && current_port == port {
        return Ok(status_of(&st));
    }
    if current {
        stop_server(&app);
        tokio::time::sleep(std::time::Duration::from_millis(180)).await;
    }

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|e| format!("端口 {port} 启动失败：{e}"))?;
    let (tx, rx) = oneshot::channel::<()>();
    *st.shutdown.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

    let ctx = Arc::new(Ctx {
        app: app.clone(),
        code: st.code.clone(),
    });
    let router = build_router(ctx.clone()).layer(DefaultBodyLimit::max(2 * 1024 * 1024 * 1024));
    let running = st.running.clone();
    let error = st.error.clone();
    running.store(true, Ordering::Relaxed);
    st.port.store(port, Ordering::Relaxed);
    st.set_error(None);

    tauri::async_runtime::spawn(async move {
        let served = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .await;
        running.store(false, Ordering::Relaxed);
        if let Err(e) = served {
            *error.lock().unwrap_or_else(|x| x.into_inner()) = Some(format!("Web 服务异常：{e}"));
        }
    });

    // 给监听器一个落到系统网络栈的机会；绑定本身已经成功，正常无需等待。
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    Ok(status_of(&st))
}

fn stop_server(app: &AppHandle) {
    let st = app.state::<WebState>();
    if let Some(tx) = st.shutdown.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = tx.send(());
    }
    st.running.store(false, Ordering::Relaxed);
}

pub fn autostart(app: &AppHandle) {
    let enabled: bool = vqa_core::config::shared_get_or(ENABLED_KEY, false);
    if !enabled {
        return;
    }
    let port: u16 = vqa_core::config::shared_get_or(PORT_KEY, DEFAULT_PORT);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start_server(app.clone(), port).await {
            let st = app.state::<WebState>();
            st.set_error(Some(e));
            let _ = vqa_core::config::shared_set(ENABLED_KEY, json!(false));
        }
    });
}

pub fn shutdown(app: &AppHandle) {
    stop_server(app);
}

// ═══════════════════ Tauri 命令 ═══════════════════

#[tauri::command]
pub fn web_status(st: State<'_, WebState>) -> WebStatus {
    status_of(&st)
}

#[tauri::command]
pub async fn web_apply(
    app: AppHandle,
    st: State<'_, WebState>,
    enabled: bool,
    port: u16,
) -> Result<WebStatus, String> {
    if !enabled {
        stop_server(&app);
        let _ = vqa_core::config::shared_set(ENABLED_KEY, json!(false));
        let _ = vqa_core::config::shared_set(PORT_KEY, json!(port));
        return Ok(status_of(&st));
    }
    let status = start_server(app, port).await?;
    let _ = vqa_core::config::shared_set(ENABLED_KEY, json!(true));
    let _ = vqa_core::config::shared_set(PORT_KEY, json!(port));
    Ok(status)
}

#[tauri::command]
pub fn web_reset_code(st: State<'_, WebState>) -> String {
    let code = new_code();
    st.set_code(code.clone());
    st.set_error(None);
    code
}

#[tauri::command]
pub fn web_open(st: State<'_, WebState>) -> Result<(), String> {
    let s = status_of(&st);
    if !s.running {
        return Err("Web 访问尚未开启".into());
    }
    let url = format!("{}/login?code={}", s.local_url, s.code);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", &url]);
        c.creation_flags(0x0800_0000);
        c.spawn().map_err(|e| format!("打开浏览器失败：{e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err("当前平台不支持自动打开浏览器".into())
    }
}

// ═══════════════════ 路由与认证 ═══════════════════

fn build_router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/login", get(login_get).post(login_post))
        .route("/api/invoke", post(api_invoke))
        .route("/api/upload", post(api_upload))
        .fallback(static_file)
        .layer(middleware::from_fn_with_state(ctx.clone(), auth))
        .with_state(ctx)
}

fn cookie_value(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        (k == "vqa_web").then(|| v.to_string())
    })
}

fn is_exempt(path: &str) -> bool {
    path == "/login" || path == "/health"
}

async fn auth(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if is_exempt(&path) || addr.ip().is_loopback() {
        return next.run(req).await;
    }
    let want = ctx.code();
    if cookie_value(req.headers()).as_deref() == Some(want.as_str()) {
        return next.run(req).await;
    }
    if path.starts_with("/api/") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "error": "访问码无效或已过期，请重新登录" })),
        )
            .into_response();
    }
    Redirect::to("/login").into_response()
}

#[derive(Deserialize)]
struct LoginQuery {
    code: Option<String>,
}

async fn login_get(AxumState(ctx): AxumState<Arc<Ctx>>, Query(q): Query<LoginQuery>) -> Response {
    let want = ctx.code();
    if let Some(code) = q.code {
        if code == want {
            return with_cookie(Redirect::to("/"), &want);
        }
    }
    Html(login_page(false)).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    code: String,
}

async fn login_post(AxumState(ctx): AxumState<Arc<Ctx>>, Form(f): Form<LoginForm>) -> Response {
    let want = ctx.code();
    if f.code.trim() == want {
        with_cookie(Redirect::to("/"), &want)
    } else {
        Html(login_page(true)).into_response()
    }
}

fn with_cookie(resp: Redirect, code: &str) -> Response {
    let mut r = resp.into_response();
    if let Ok(v) = HeaderValue::from_str(&format!(
        "vqa_web={code}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800"
    )) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

fn login_page(failed: bool) -> String {
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>局域网访问 · 视听质量检测工作站</title>
<style>
*{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#f5efe3;color:#25211b;font:14px/1.6 "Segoe UI","Microsoft YaHei",sans-serif}}
.card{{width:min(420px,92vw);padding:30px 30px 26px;background:#fffdf7;border:1px solid #ded2bd;border-radius:16px;box-shadow:0 18px 50px rgba(54,45,31,.14)}}
.mark{{width:38px;height:38px;border-radius:10px;background:#355f4d;color:#e9c37c;display:grid;place-items:center;font-size:18px;margin-bottom:16px}}
h1{{font:600 19px/1.35 "Microsoft YaHei",sans-serif;margin:0 0 6px}}p{{margin:0 0 18px;color:#746b5e;font-size:12.5px}}
input{{width:100%;height:44px;padding:0 14px;border:1px solid #ded2bd;border-radius:9px;background:#fbf7ee;color:#25211b;font:20px "Consolas",monospace;letter-spacing:.28em;text-align:center;outline:none}}
input:focus{{border-color:#355f4d;box-shadow:0 0 0 3px rgba(53,95,77,.10)}}
button{{width:100%;height:42px;margin-top:12px;border:0;border-radius:9px;background:#355f4d;color:#fff8e8;font:600 13px "Microsoft YaHei",sans-serif;cursor:pointer}}
button:hover{{background:#2c5242}}.err{{margin:0 0 10px;color:#a33d32;font-size:12px}}.hint{{margin-top:14px;color:#8b8172;font-size:11px}}
</style></head><body><form class="card" method="post" action="/login">
<div class="mark">视</div><h1>局域网访问</h1><p>请输入桌面端「设置 → 局域网访问」中显示的 8 位访问码。</p>
{}<input name="code" inputmode="numeric" pattern="[0-9]{{4,12}}" maxlength="12" autocomplete="one-time-code" autofocus/>
<button type="submit">进入检测工作台</button><div class="hint">访问码只用于本机局域网访问；关闭开关或重置口令后立即失效。</div>
</form></body></html>"#,
        if failed {
            "<div class=\"err\">访问码不正确，请重试。</div>"
        } else {
            ""
        }
    )
}

// ═══════════════════ 静态资源 ═══════════════════

async fn static_file(uri: axum::http::Uri) -> Response {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".into();
    }
    let Some(file) = UI.get_file(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mime = match Path::new(&path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    };
    let cache = if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=604800"
    };
    let mut resp = Body::from(file.contents().to_vec()).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    resp
}

// ═══════════════════ 上传（供 Web 端选本地 TS 文件） ═══════════════════

async fn api_upload(mut mp: Multipart) -> Response {
    let dir = vqa_core::config::base_dir().join("web_uploads");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return api_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("建上传目录失败：{e}"),
        );
    }
    let mut saved: Option<PathBuf> = None;
    while let Ok(Some(field)) = mp.next_field().await {
        let orig = field.file_name().unwrap_or("upload.ts").to_string();
        let name = sanitize_filename(&orig);
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        let dest = dir.join(format!("{ts}_{name}"));
        let data = match field.bytes().await {
            Ok(b) => b,
            Err(e) => return api_err(StatusCode::BAD_REQUEST, format!("读取上传内容失败：{e}")),
        };
        if let Err(e) = tokio::fs::write(&dest, &data).await {
            return api_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("写上传文件失败：{e}"),
            );
        }
        saved = Some(dest);
        break;
    }
    match saved {
        Some(p) => Json(json!({ "ok": true, "data": p.to_string_lossy() })).into_response(),
        None => api_err(StatusCode::BAD_REQUEST, "没有收到文件".into()),
    }
}

fn sanitize_filename(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.ts");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else if c.is_alphanumeric() {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.trim().is_empty() {
        "upload.ts".into()
    } else {
        cleaned
    }
}

fn api_err(status: StatusCode, msg: String) -> Response {
    (status, Json(json!({ "ok": false, "error": msg }))).into_response()
}

// ═══════════════════ API 调度 ═══════════════════

#[derive(Deserialize)]
struct InvokeReq {
    cmd: String,
    #[serde(default)]
    args: Value,
}

fn a<'a>(args: &'a Value, key: &str) -> &'a Value {
    args.get(key).unwrap_or(&Value::Null)
}

fn s(args: &Value, key: &str) -> String {
    a(args, key).as_str().unwrap_or("").to_string()
}

fn s2(args: &Value, k1: &str, k2: &str) -> String {
    let v = args.get(k1).or_else(|| args.get(k2));
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn u(args: &Value, key: &str) -> u64 {
    a(args, key).as_u64().unwrap_or(0)
}

fn f(args: &Value, key: &str) -> f64 {
    a(args, key).as_f64().unwrap_or(0.0)
}

fn b(args: &Value, key: &str) -> bool {
    a(args, key).as_bool().unwrap_or(false)
}

async fn api_invoke(AxumState(ctx): AxumState<Arc<Ctx>>, Json(req): Json<InvokeReq>) -> Response {
    match dispatch(&ctx.app, &req.cmd, &req.args).await {
        Ok(v) => Json(json!({ "ok": true, "data": v })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn dispatch(app: &AppHandle, cmd: &str, args: &Value) -> Result<Value, String> {
    let out = match cmd {
        // ── 通用 / 配置 ──
        "app_version" => json!(commands::app_version()),
        "list_nics" => json!(commands::list_nics()),
        "source_history" => json!(commands::source_history(s(args, "key"))),
        "push_source_history" => {
            commands::push_source_history(s(args, "key"), s(args, "url"));
            json!(null)
        }
        "tool_paths" => commands::tool_paths(),
        "app_paths" => commands::app_paths(),
        "ui_pref_get" => sync_cmds::ui_pref_get(s(args, "key")),
        "ui_pref_set" => {
            sync_cmds::ui_pref_set(s(args, "key"), a(args, "value").clone())?;
            json!(null)
        }
        "channels_get" => json!(sync_cmds::channels_get()),
        "channels_save" => {
            let list: Vec<sync_cmds::Channel> =
                serde_json::from_value(a(args, "list").clone()).map_err(|e| e.to_string())?;
            sync_cmds::channels_save(list)?;
            json!(null)
        }
        "thresholds_get" => commands::thresholds_get(app.state::<crate::state::AnalysisState>()),
        "thresholds_set" => {
            let map: std::collections::HashMap<String, f64> =
                serde_json::from_value(a(args, "map").clone()).map_err(|e| e.to_string())?;
            commands::thresholds_set(app.state::<crate::state::AnalysisState>(), map);
            json!(null)
        }
        "player_settings_get" => commands::player_settings_get(),
        "player_settings_set" => json!(commands::player_settings_set(
            s2(args, "snapDir", "snap_dir"),
            s2(args, "recDir", "rec_dir"),
            b(args, "defaultLoop"),
            b(args, "defaultForceBridge"),
            s(args, "defaultNic"),
            app.state::<crate::state::PlayerState>(),
        )?),
        "analysis_settings_get" => commands::analysis_settings_get(),
        "analysis_settings_set" => {
            json!(commands::analysis_settings_set(s2(
                args,
                "reportDir",
                "report_dir"
            ))?)
        }
        "media_info" => commands::media_info(s(args, "path"))?,
        "diag_collect" => commands::diag_collect(
            app.state::<crate::state::PlayerState>(),
            app.state::<crate::state::AnalysisState>(),
        ),
        "diag_has_token" => json!(commands::diag_has_token()),
        "diag_upload" => json!(commands::diag_upload(
            None,
            s(args, "name"),
            s(args, "content"),
        )?),

        // ── 视频分析 ──
        "analysis_start" => {
            commands::analysis_start(
                app.state::<crate::state::AnalysisState>(),
                s(args, "url"),
                s2(args, "nicIp", "nic_ip"),
            )?;
            json!(null)
        }
        "analysis_stop" => {
            commands::analysis_stop(app.state::<crate::state::AnalysisState>());
            json!(null)
        }
        "analysis_progress" => {
            commands::analysis_progress(app.state::<crate::state::AnalysisState>())
        }
        "analysis_log" => json!(commands::analysis_log(
            app.state::<crate::state::AnalysisState>()
        )),
        "analysis_result" => commands::analysis_result(app.state::<crate::state::AnalysisState>())
            .unwrap_or(Value::Null),
        "packet_rows" => commands::packet_rows(
            app.state::<crate::state::AnalysisState>(),
            u(args, "start"),
            u(args, "count"),
        ),

        // ── 合规检测 ──
        "compliance_check" => {
            compliance_cmds::compliance_check(s(args, "path"), s(args, "kind")).await?
        }
        "compliance_live_start" => {
            compliance_cmds::compliance_live_start(
                app.state::<crate::state::ComplianceState>(),
                s(args, "url"),
                s2(args, "nicIp", "nic_ip"),
                s(args, "kind"),
            )
            .await?;
            json!(null)
        }
        "compliance_live_snapshot" => compliance_cmds::compliance_live_snapshot(
            app.state::<crate::state::ComplianceState>(),
            s(args, "kind"),
        ),
        "compliance_live_stop" => {
            compliance_cmds::compliance_live_stop(app.state::<crate::state::ComplianceState>());
            json!(null)
        }

        // ── 微突发 ──
        "burst_start" => json!(
            burst_cmds::burst_start(
                app.state::<crate::state::BurstState>(),
                s(args, "url"),
                s2(args, "nicIp", "nic_ip"),
                f(args, "ratio"),
                u(args, "windowMs"),
            )
            .await?
        ),
        "burst_snapshot" => json!(burst_cmds::burst_snapshot(
            app.state::<crate::state::BurstState>()
        )),
        "burst_stop" => json!(burst_cmds::burst_stop(
            app.state::<crate::state::BurstState>()
        )),

        // ── 音画同步 / 一致性 ──
        "avsync_start" => {
            let cfg = serde_json::from_value(a(args, "cfg").clone())
                .map_err(|e| format!("参数无效：{e}"))?;
            sync_cmds::avsync_start(
                app.state::<crate::state::AvState>(),
                cfg,
                args.get("job").and_then(|v| v.as_str()).map(str::to_string),
            )?;
            json!(null)
        }
        "avsync_progress" => json!(sync_cmds::avsync_progress(
            app.state::<crate::state::AvState>(),
            args.get("job").and_then(|v| v.as_str()).map(str::to_string),
        )),
        "avsync_stop" => {
            sync_cmds::avsync_stop(
                app.state::<crate::state::AvState>(),
                args.get("job").and_then(|v| v.as_str()).map(str::to_string),
            );
            json!(null)
        }
        "consist_start" => {
            let cfg = serde_json::from_value(a(args, "cfg").clone())
                .map_err(|e| format!("参数无效：{e}"))?;
            sync_cmds::consist_start(
                app.state::<crate::state::ConsistState>(),
                cfg,
                args.get("job").and_then(|v| v.as_str()).map(str::to_string),
            )?;
            json!(null)
        }
        "consist_progress" => json!(sync_cmds::consist_progress(
            app.state::<crate::state::ConsistState>(),
            args.get("job").and_then(|v| v.as_str()).map(str::to_string),
        )),
        "consist_stop" => {
            sync_cmds::consist_stop(
                app.state::<crate::state::ConsistState>(),
                args.get("job").and_then(|v| v.as_str()).map(str::to_string),
            );
            json!(null)
        }

        // ── 抓包 ──
        "capture_start" => json!(capture_cmds::capture_start(
            app.state::<capture_cmds::CaptureState>(),
            s(args, "url"),
            s2(args, "nicIp", "nic_ip"),
        )?),
        "capture_stop" => json!(capture_cmds::capture_stop(
            app.state::<capture_cmds::CaptureState>()
        )?),
        "capture_status" => json!(capture_cmds::capture_status(
            app.state::<capture_cmds::CaptureState>()
        )),

        // ── 检测画面预览 ──
        "preview_start" => {
            sync_cmds::preview_start(
                app.state::<crate::preview::Preview>(),
                s(args, "owner"),
                s(args, "src"),
                s2(args, "nicIp", "nic_ip"),
            )?;
            json!(null)
        }
        "preview_stop" => {
            sync_cmds::preview_stop(app.state::<crate::preview::Preview>(), s(args, "owner"));
            json!(null)
        }
        "preview_frame" => json!(sync_cmds::preview_frame(
            app.state::<crate::preview::Preview>(),
            s(args, "owner"),
        )),

        // ── 告警处理 ──
        "alarm_list" => json!(alarm_cmds::alarm_list()),
        "alarm_record" => {
            let input: alarm_cmds::AlarmInput = serde_json::from_value(a(args, "input").clone())
                .map_err(|e| format!("参数无效：{e}"))?;
            json!(alarm_cmds::alarm_record(input)?)
        }
        "alarm_update" => json!(alarm_cmds::alarm_update(
            s(args, "id"),
            s(args, "status"),
            s(args, "conclusion"),
            s(args, "assignee"),
            s(args, "note"),
        )?),
        "alarm_capture" => {
            alarm_cmds::alarm_capture(
                s2(args, "recordId", "record_id"),
                s(args, "source"),
                u(args, "seconds") as u32,
                args.get("startS").and_then(|v| v.as_f64()),
            )?;
            json!(null)
        }
        "alarm_settings_get" => alarm_cmds::alarm_settings_get(),
        "alarm_settings_set" => json!(alarm_cmds::alarm_settings_set(
            s2(args, "logDir", "log_dir"),
            s2(args, "clipDir", "clip_dir"),
            u(args, "retainDays") as u32,
            f(args, "maxGb"),
            b(args, "autoClean"),
        )?),
        "alarm_detect_log" => {
            let _ = alarm_cmds::alarm_detect_log(
                s(args, "channel"),
                s(args, "pair"),
                f(args, "similarity"),
                f(args, "minSim"),
                a(args, "offsetMs").as_i64().unwrap_or(0) as i32,
                u(args, "segmentCount") as usize,
                s(args, "verdict"),
                s(args, "source"),
                s(args, "reference"),
                f(args, "fps"),
                b(args, "bad"),
            );
            json!(null)
        }

        // ── 桌面专属，Web 端明确报错而不是静默失败 ──
        "browse_media_file" | "browse_folder" => {
            return Err("Web 端请使用文件上传，或直接填写服务端可访问的路径".into())
        }
        "reveal_path" | "alarm_open" => return Err("Web 端不能打开本机资源管理器".into()),
        "player_play"
        | "player_prewarm"
        | "player_stop"
        | "player_pause_toggle"
        | "player_seek"
        | "player_set_volume"
        | "player_set_mute"
        | "player_set_speed"
        | "player_set_loop"
        | "player_osd"
        | "player_programs"
        | "player_set_program"
        | "player_snapshot"
        | "player_snapshot_save"
        | "player_record_toggle"
        | "player_adaptive_scale"
        | "place_video" => return Err("Web 端不提供播放器，请回到桌面端操作".into()),
        "xcode_encoders" | "xcode_preview" | "xcode_start" | "xcode_snapshot" | "xcode_stop" => {
            return Err("Web 端不提供直播转码，请回到桌面端操作".into())
        }
        "easytier_config_get"
        | "easytier_config_save"
        | "easytier_start"
        | "easytier_stop"
        | "easytier_status"
        | "easytier_restart_elevated" => {
            return Err("Web 端不提供 EasyTier 组网，请回到桌面端操作".into())
        }
        _ => return Err(format!("Web 端暂不支持命令 {cmd}")),
    };
    Ok(out)
}
