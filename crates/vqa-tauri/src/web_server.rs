//! 局域网 Web 访问：内嵌前端资源 + 受口令保护的 HTTP API。
//!
//! 设计口径（与桌面端保持一致，少复制业务逻辑）：
//! - 服务跑在 Tauri 主进程里，API 直接调用现有 `#[tauri::command]` 函数；
//! - 浏览器端仍用原前端，`api.ts` 在非 Tauri 环境改走 `/api/invoke`；
//! - 播放器依赖原生 mpv 子窗口，不在 Web 端开放；检测/控制/报告全量可用；
//! - 局域网请求必须带访问码；本机回环地址免登录。

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::{os::windows::process::CommandExt, process::Command};

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
use tokio_util::io::ReaderStream;

use crate::{alarm_cmds, burst_cmds, capture_cmds, commands, compliance_cmds, sync_cmds};

static UI: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui/dist");

const DEFAULT_PORT: u16 = 8766;
const CODE_KEY: &str = "web.access_code";
const ENABLED_KEY: &str = "web.enabled";
const PORT_KEY: &str = "web.port";
const LOGIN_WINDOW: Duration = Duration::from_secs(600);
const LOGIN_MAX_FAILS: u32 = 10;
const API_WINDOW: Duration = Duration::from_secs(60);
const API_MAX_REQUESTS: u32 = 4000;
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const SESSION_MAX: usize = 1024;
const UPLOAD_RETENTION_DAYS: u64 = 7;
const UPLOAD_EXTS: &[&str] = &[
    "ts", "mpg", "mpeg", "mp4", "mkv", "avi", "flv", "m2t", "m2ts", "mts", "mov", "webm", "wmv",
];

// ═══════════════════ 状态 ═══════════════════

pub struct WebState {
    running: Arc<AtomicBool>,
    port: Arc<AtomicU16>,
    code: Arc<RwLock<String>>,
    sessions: Arc<Mutex<HashMap<String, Instant>>>,
    error: Arc<Mutex<Option<String>>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl WebState {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            port: Arc::new(AtomicU16::new(DEFAULT_PORT)),
            code: Arc::new(RwLock::new(String::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
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

    fn clear_sessions(&self) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn set_error(&self, e: Option<String>) {
        *self.error.lock().unwrap_or_else(|x| x.into_inner()) = e;
    }
}

/// 会话只保存随机 token，不把访问码写进 Cookie；服务重启/关闭/重置口令都会清空。
fn new_session_token() -> String {
    format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    )
}

fn issue_session(sessions: &Mutex<HashMap<String, Instant>>) -> String {
    let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    guard.retain(|_, until| now < *until);
    if guard.len() >= SESSION_MAX {
        guard.clear();
    }
    let token = new_session_token();
    guard.insert(token.clone(), now + SESSION_TTL);
    token
}

fn session_valid(sessions: &Mutex<HashMap<String, Instant>>, token: &str) -> bool {
    let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match guard.get(token) {
        Some(until) if now < *until => true,
        Some(_) => {
            guard.remove(token);
            false
        }
        None => false,
    }
}

fn revoke_session(sessions: &Mutex<HashMap<String, Instant>>, token: &str) {
    sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(token);
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
    let login_url = format!("{url}/login");
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
    sessions: Arc<Mutex<HashMap<String, Instant>>>,
    login_fails: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    api_hits: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl Ctx {
    fn code(&self) -> String {
        self.code.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn login_blocked(&self, ip: IpAddr) -> bool {
        let mut guard = self.login_fails.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if let Some((count, since)) = guard.get(&ip) {
            if now.duration_since(*since) >= LOGIN_WINDOW {
                guard.remove(&ip);
                return false;
            }
            return *count >= LOGIN_MAX_FAILS;
        }
        false
    }

    fn note_login_fail(&self, ip: IpAddr) {
        let mut guard = self.login_fails.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match guard.get_mut(&ip) {
            Some((count, since)) if now.duration_since(*since) < LOGIN_WINDOW => {
                *count = count.saturating_add(1);
            }
            _ => {
                guard.insert(ip, (1, now));
            }
        }
    }

    fn clear_login_fails(&self, ip: IpAddr) {
        self.login_fails
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
    }

    fn api_allowed(&self, ip: IpAddr) -> bool {
        let mut guard = self.api_hits.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if let Some((count, since)) = guard.get_mut(&ip) {
            if now.duration_since(*since) >= API_WINDOW {
                *count = 1;
                *since = now;
                return true;
            }
            if *count >= API_MAX_REQUESTS {
                return false;
            }
            *count = count.saturating_add(1);
            return true;
        }
        guard.insert(ip, (1, now));
        if guard.len() > 4096 {
            guard.retain(|_, (_, since)| now.duration_since(*since) < API_WINDOW);
        }
        true
    }
}

async fn start_server(app: AppHandle, port: u16) -> Result<WebStatus, String> {
    if !(1024..=65535).contains(&port) {
        return Err("端口范围应为 1024–65535".into());
    }
    cleanup_web_uploads();
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
        sessions: st.sessions.clone(),
        login_fails: Mutex::new(HashMap::new()),
        api_hits: Mutex::new(HashMap::new()),
    });
    let router = build_router(ctx.clone()).layer(DefaultBodyLimit::max(64 * 1024 * 1024));
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
    st.clear_sessions();
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
    let was_running = st.running.load(Ordering::Relaxed);
    let old_port = st.port.load(Ordering::Relaxed);
    match start_server(app.clone(), port).await {
        Ok(status) => {
            let _ = vqa_core::config::shared_set(ENABLED_KEY, json!(true));
            let _ = vqa_core::config::shared_set(PORT_KEY, json!(port));
            Ok(status)
        }
        Err(e) => {
            // 自定义端口失败时，把原服务恢复到旧端口，避免一个错误输入把远程访问停掉。
            if was_running && old_port != port {
                let rollback = start_server(app.clone(), old_port).await;
                let st = app.state::<WebState>();
                if let Err(re) = rollback {
                    st.set_error(Some(format!("{e}；旧端口 {old_port} 恢复失败：{re}")));
                } else {
                    st.set_error(Some(format!("{e}；已恢复原端口 {old_port}")));
                }
            } else {
                app.state::<WebState>().set_error(Some(e.clone()));
            }
            Err(e)
        }
    }
}

#[tauri::command]
pub fn web_reset_code(st: State<'_, WebState>) -> String {
    let code = new_code();
    st.set_code(code.clone());
    st.clear_sessions();
    st.set_error(None);
    code
}

#[tauri::command]
pub fn web_open(st: State<'_, WebState>) -> Result<(), String> {
    let s = status_of(&st);
    if !s.running {
        return Err("Web 访问尚未开启".into());
    }
    let url = format!("{}/", s.local_url);
    #[cfg(windows)]
    {
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

/// 在 Windows 防火墙里放行当前 Web 端口（专用网络入站）。
/// 非管理员时通过 UAC 拉起 netsh；返回的是“已请求”，不是规则已生效。
#[tauri::command]
pub fn web_firewall_fix(st: State<'_, WebState>) -> Result<String, String> {
    let port = status_of(&st).port;
    add_firewall_rule(port)
}

#[cfg(windows)]
fn add_firewall_rule(port: u16) -> Result<String, String> {
    const NO_WINDOW: u32 = 0x0800_0000;
    let rule_name = format!("VQA Web {port}");
    let show = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={rule_name}"),
        ])
        .creation_flags(NO_WINDOW)
        .output();
    if let Ok(out) = show {
        let text = String::from_utf8_lossy(&out.stdout);
        if out.status.success() && text.contains(&rule_name) {
            // 老版本建的规则可能还是 RemoteIP=Any，先尝试就地收紧。
            let set = Command::new("netsh")
                .args([
                    "advfirewall",
                    "firewall",
                    "set",
                    "rule",
                    &format!("name={rule_name}"),
                    "new",
                    "remoteip=localsubnet",
                    "profile=domain,private",
                    "enable=yes",
                ])
                .creation_flags(NO_WINDOW)
                .output();
            if let Ok(out) = set {
                if out.status.success() {
                    return Ok(format!("防火墙规则 {rule_name} 已收紧为仅本地子网"));
                }
            }
            let ps = format!(
                "Start-Process -FilePath netsh.exe -Verb RunAs -WindowStyle Hidden -ArgumentList @('advfirewall','firewall','set','rule','name=\"{rule_name}\"','new','remoteip=localsubnet','profile=domain,private','enable=yes')"
            );
            let mut c = Command::new("powershell");
            c.args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &ps,
            ]);
            c.creation_flags(NO_WINDOW);
            c.spawn()
                .map_err(|e| format!("请求管理员权限失败：{e}"))?;
            return Ok(format!(
                "已请求管理员权限把 TCP {port} 规则收紧为仅本地子网，请在 UAC 窗口中确认"
            ));
        }
    }

    let add = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            &format!("name={rule_name}"),
            "dir=in",
            "action=allow",
            "protocol=TCP",
            &format!("localport={port}"),
            "remoteip=localsubnet",
            "profile=domain,private",
            "enable=yes",
        ])
        .creation_flags(NO_WINDOW)
        .output();
    if let Ok(out) = add {
        if out.status.success() {
            return Ok(format!("防火墙已放行 TCP {port}（仅本地子网）"));
        }
    }

    let ps = format!(
        "Start-Process -FilePath netsh.exe -Verb RunAs -WindowStyle Hidden -ArgumentList @('advfirewall','firewall','add','rule','name=\"{rule_name}\"','dir=in','action=allow','protocol=TCP','localport={port}','remoteip=localsubnet','profile=domain,private','enable=yes')"
    );
    let mut c = Command::new("powershell");
    c.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &ps,
    ]);
    c.creation_flags(NO_WINDOW);
    c.spawn().map_err(|e| format!("请求管理员权限失败：{e}"))?;
    Ok(format!(
        "已请求管理员权限放行 TCP {port}，请在 UAC 窗口中确认"
    ))
}

#[cfg(not(windows))]
fn add_firewall_rule(_port: u16) -> Result<String, String> {
    Err("当前平台不支持 Windows 防火墙规则".into())
}

// ═══════════════════ 路由与认证 ═══════════════════

fn build_router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/login", get(login_get).post(login_post))
        .route("/logout", get(logout).post(logout))
        .route("/api/invoke", post(api_invoke))
        .route("/api/upload", post(api_upload))
        .route("/api/download/capture", get(download_capture))
        .route("/api/download/alarm", get(download_alarm))
        .fallback(static_file)
        .layer(middleware::from_fn_with_state(ctx.clone(), auth))
        .layer(middleware::from_fn(security_headers))
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
    if let Some(token) = cookie_value(req.headers()) {
        if session_valid(&ctx.sessions, &token) {
            return next.run(req).await;
        }
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

async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    // 前端只加载同源资源；内联样式是现有页面大量使用的写法，单独放开 style。
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

async fn login_get(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if ctx.login_blocked(addr.ip()) {
        return too_many_logins();
    }
    Html(login_page(false)).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    code: String,
}

fn code_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn login_post(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Form(f): Form<LoginForm>,
) -> Response {
    if ctx.login_blocked(addr.ip()) {
        return too_many_logins();
    }
    let want = ctx.code();
    if code_eq(f.code.trim(), &want) {
        ctx.clear_login_fails(addr.ip());
        let token = issue_session(&ctx.sessions);
        with_cookie(Redirect::to("/"), &token)
    } else {
        ctx.note_login_fail(addr.ip());
        Html(login_page(true)).into_response()
    }
}

fn too_many_logins() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "600")],
        Html(
            r#"<!doctype html><meta charset="utf-8"><title>访问受限</title>
<body style="font:14px 'Microsoft YaHei',sans-serif;padding:40px;background:#f5efe3;color:#25211b">
<h2 style="font-size:18px">尝试次数过多</h2><p>请等待几分钟后再试，或在桌面端重置访问码。</p></body>"#,
        ),
    )
        .into_response()
}

async fn logout(AxumState(ctx): AxumState<Arc<Ctx>>, headers: axum::http::HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers) {
        revoke_session(&ctx.sessions, &token);
    }
    let mut r = Redirect::to("/login").into_response();
    if let Ok(v) = HeaderValue::from_str(
        "vqa_web=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
    ) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

fn with_cookie(resp: Redirect, token: &str) -> Response {
    let mut r = resp.into_response();
    if let Ok(v) = HeaderValue::from_str(&format!(
        "vqa_web={token}; HttpOnly; SameSite=Strict; Path=/"
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
<button type="submit">进入检测工作台</button><div class="hint">登录状态仅在当前浏览器会话有效；关闭开关或重置口令后立即失效。若连续输错多次会暂时限制登录。</div>
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

async fn api_upload(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut mp: Multipart,
) -> Response {
    if !ctx.api_allowed(addr.ip()) {
        return api_err(StatusCode::TOO_MANY_REQUESTS, "请求过于频繁，请稍后再试".into());
    }
    let dir = vqa_core::config::base_dir().join("web_uploads");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return api_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("建上传目录失败：{e}"),
        );
    }
    let mut saved: Option<PathBuf> = None;
    if let Ok(Some(field)) = mp.next_field().await {
        let orig = field.file_name().unwrap_or("upload.ts").to_string();
        let name = sanitize_filename(&orig);
        if !allowed_upload(&name) {
            return api_err(StatusCode::BAD_REQUEST, format!("不支持的文件类型：{name}"));
        }
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
    }
    if saved.is_some() {
        std::thread::spawn(cleanup_web_uploads);
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

fn allowed_upload(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    UPLOAD_EXTS.contains(&ext.as_str())
}

/// Web 端本地来源白名单：只允许本次上传目录，或频道库里已经配置过的本地路径。
/// 网络地址（udp/rtp/rtsp/http/https 等）放行；`file:` 一律拒绝，避免把
/// `/api/invoke` 变成任意本机文件读取入口。
fn web_source_allowed(src: &str) -> bool {
    let src = src.trim();
    if src.is_empty() {
        return false;
    }
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("file:") {
        return false;
    }
    if src.contains("://") {
        return true;
    }
    let path = Path::new(src);
    if !path.is_absolute() {
        return false;
    }
    let Ok(canon) = path.canonicalize() else {
        return false;
    };
    if let Ok(upload) = vqa_core::config::base_dir()
        .join("web_uploads")
        .canonicalize()
    {
        if canon.starts_with(&upload) {
            return true;
        }
    }
    for ch in sync_cmds::channels_get() {
        let url = ch.url.trim();
        if url.contains("://") {
            continue;
        }
        if let Ok(p) = Path::new(url).canonicalize() {
            if p == canon {
                return true;
            }
        }
    }
    false
}

/// 清理过期的 Web 上传文件；只删除本服务自己的 web_uploads，不碰用户目录。
fn cleanup_web_uploads() {
    let dir = vqa_core::config::base_dir().join("web_uploads");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(UPLOAD_RETENTION_DAYS * 86400));
    let Some(cutoff) = cutoff else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ═══════════════════ 下载（抓包 pcap/TS、告警证据片段） ═══════════════════

#[derive(Deserialize)]
struct CaptureDownloadQuery {
    kind: String,
}

async fn download_capture(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    Query(q): Query<CaptureDownloadQuery>,
) -> Response {
    let snapshot = capture_cmds::capture_status(ctx.app.state::<capture_cmds::CaptureState>());
    let Some(snapshot) = snapshot else {
        return api_err(StatusCode::NOT_FOUND, "当前没有抓包任务".into());
    };
    if snapshot.running {
        return api_err(
            StatusCode::CONFLICT,
            "抓包仍在运行，请先停止抓包，待文件落盘后再下载".into(),
        );
    }
    let (path, fallback) = match q.kind.as_str() {
        "pcap" => (snapshot.pcap_path, "capture.pcap"),
        "ts" => (snapshot.ts_path, "capture.ts"),
        _ => return api_err(StatusCode::BAD_REQUEST, "下载类型只能是 pcap 或 ts".into()),
    };
    if path.trim().is_empty() {
        return api_err(StatusCode::NOT_FOUND, "抓包产物尚未生成".into());
    }
    file_response(PathBuf::from(path), fallback, "no-store").await
}

#[derive(Deserialize)]
struct AlarmDownloadQuery {
    id: String,
}

async fn download_alarm(Query(q): Query<AlarmDownloadQuery>) -> Response {
    let record = alarm_cmds::alarm_list().into_iter().find(|r| r.id == q.id);
    let Some(record) = record else {
        return api_err(StatusCode::NOT_FOUND, "告警记录不存在".into());
    };
    if record.clip_path.trim().is_empty() {
        return api_err(StatusCode::NOT_FOUND, "告警证据片段尚未生成".into());
    }
    let fallback = format!("alarm_{}.ts", sanitize_filename(&record.id));
    file_response(PathBuf::from(record.clip_path), &fallback, "no-store").await
}

async fn file_response(path: PathBuf, fallback: &str, cache: &str) -> Response {
    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) if m.is_file() => m,
        _ => return api_err(StatusCode::NOT_FOUND, "文件不存在或已被清理".into()),
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            return api_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("打开文件失败：{e}"),
            )
        }
    };
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let header_name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let header_name = if header_name.trim().is_empty() {
        fallback.to_string()
    } else {
        header_name
    };
    let mime = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "pcap" => "application/vnd.tcpdump.pcap",
        "ts" | "m2ts" | "mts" => "video/mp2t",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        _ => "application/octet-stream",
    };
    let len = meta.len();
    let stream = ReaderStream::new(file);
    let mut resp = Body::from_stream(stream).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    if let Ok(v) = HeaderValue::from_str(&len.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{header_name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_str(cache).unwrap_or(HeaderValue::from_static("no-store")),
    );
    resp
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

async fn api_invoke(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<InvokeReq>,
) -> Response {
    if !ctx.api_allowed(addr.ip()) {
        return api_err(
            StatusCode::TOO_MANY_REQUESTS,
            "请求过于频繁，请稍后再试".into(),
        );
    }
    match dispatch(&ctx.app, &req.cmd, &req.args).await {
        Ok(v) => Json(json!({ "ok": true, "data": v })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

fn is_web_write_denied(cmd: &str) -> bool {
    matches!(
        cmd,
        "diag_upload"
            | "diag_collect"
            | "channels_save"
            | "thresholds_set"
            | "player_settings_set"
            | "analysis_settings_set"
            | "alarm_settings_set"
    )
}

/// Web 端可见的模块频道键；播放器在 Web 端不开放，不纳入。
const WEB_CHANNEL_MODULES: &[&str] = &["analysis", "compliance", "burst", "avsync"];

/// Web 端允许读取的页面偏好键。只放页面实际会读的 UI 状态和只读网卡展示，
/// 避免 `ui_pref_get("web.access_code")` 之类的配置文件读取。
const WEB_UI_PREF_READ_KEYS: &[&str] = &[
    "analysis.nic",
    "compliance.nic",
    "capture.prefs",
    "analysis.poll_secs",
    "compliance.poll_secs",
    "player.poll_secs",
    "analysis.ch.collapsed",
    "compliance.ch.collapsed",
    "player.ch.collapsed",
    "burst.ch.collapsed",
    "avsync.ch.collapsed",
    "burst.settings",
    "burst.window_ms",
    "avsync.prefs",
    "consist.prefs",
    "consist.groups",
    "channels.groups",
];

/// Web 端允许写入的页面偏好键。网卡键不在这里；`avsync.prefs` 和
/// `burst.settings` 中的 `nic` 会在写入前替换为服务端已有值。
const WEB_UI_PREF_WRITE_KEYS: &[&str] = &[
    "analysis.poll_secs",
    "compliance.poll_secs",
    "player.poll_secs",
    "analysis.ch.collapsed",
    "compliance.ch.collapsed",
    "player.ch.collapsed",
    "burst.ch.collapsed",
    "avsync.ch.collapsed",
    "burst.settings",
    "burst.window_ms",
    "avsync.prefs",
    "consist.prefs",
    "consist.groups",
];

fn web_ui_channel_key(key: &str) -> bool {
    if let Some(module) = key.strip_prefix("channels.") {
        if WEB_CHANNEL_MODULES.contains(&module) {
            return true;
        }
    }
    if let Some(module) = key.strip_prefix("channels.groups.") {
        if WEB_CHANNEL_MODULES.contains(&module) {
            return true;
        }
    }
    false
}

fn web_ui_pref_readable(key: &str) -> bool {
    WEB_UI_PREF_READ_KEYS.contains(&key) || web_ui_channel_key(key)
}

fn web_ui_pref_writable(key: &str) -> bool {
    WEB_UI_PREF_WRITE_KEYS.contains(&key) || web_ui_channel_key(key)
}

fn web_ui_nic_key(key: &str) -> bool {
    matches!(key, "capture.prefs" | "analysis.nic" | "compliance.nic")
}

/// 保留服务端已有网卡，只接受浏览器传来的其他字段。
fn keep_host_nic(value: Value, current: Option<&Value>) -> Result<Value, String> {
    let mut value = value;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| "设置格式错误：需要对象".to_string())?;
    let nic = current
        .and_then(|v| v.get("nic"))
        .and_then(Value::as_str)
        .unwrap_or("");
    obj.insert("nic".to_string(), json!(nic));
    Ok(value)
}

async fn dispatch(app: &AppHandle, cmd: &str, args: &Value) -> Result<Value, String> {
    if is_web_write_denied(cmd) {
        return Err("Web 端不开放本机配置写入、诊断收集或上传，请回到桌面端操作".into());
    }
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
        "ui_pref_get" => {
            let key = s(args, "key");
            if !web_ui_pref_readable(&key) {
                return Err("Web 端不开放该配置键的读取，请回到桌面端操作".into());
            }
            sync_cmds::ui_pref_get(key)
        }
        "ui_pref_set" => {
            let key = s(args, "key");
            if !web_ui_pref_writable(&key) {
                return Err(if web_ui_nic_key(&key) {
                    "Web 端不能修改主机网卡，请在桌面端设置".into()
                } else {
                    "Web 端不开放该配置键的写入，请回到桌面端操作".into()
                });
            }
            let value = a(args, "value").clone();
            let value = if matches!(key.as_str(), "avsync.prefs" | "burst.settings") {
                let current = vqa_core::config::shared_get(&key);
                keep_host_nic(value, current.as_ref())?
            } else {
                value
            };
            sync_cmds::ui_pref_set(key, value)?;
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
        "media_info" => {
            let path = s(args, "path");
            if !web_source_allowed(&path) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            commands::media_info(path)?
        }
        "diag_has_token" => json!(commands::diag_has_token()),

        // ── 视频分析 ──
        "analysis_start" => {
            let url = s(args, "url");
            if !web_source_allowed(&url) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            commands::analysis_start(
                app.state::<crate::state::AnalysisState>(),
                url,
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
        "compliance_basis" => json!(compliance_cmds::compliance_basis()),
        "compliance_doc" => compliance_cmds::compliance_doc(),
        "compliance_check" => {
            let path = s(args, "path");
            if !web_source_allowed(&path) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            compliance_cmds::compliance_check(path, s(args, "kind")).await?
        }
        "compliance_live_start" => {
            let url = s(args, "url");
            if !web_source_allowed(&url) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            compliance_cmds::compliance_live_start(
                app.state::<crate::state::ComplianceState>(),
                url,
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
        "burst_start" => {
            let url = s(args, "url");
            if !web_source_allowed(&url) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            json!(
                burst_cmds::burst_start(
                    app.state::<crate::state::BurstState>(),
                    url,
                    s2(args, "nicIp", "nic_ip"),
                    f(args, "ratio"),
                    u(args, "windowMs"),
                )
                .await?
            )
        }
        "burst_snapshot" => json!(burst_cmds::burst_snapshot(
            app.state::<crate::state::BurstState>()
        )),
        "burst_stop" => json!(burst_cmds::burst_stop(
            app.state::<crate::state::BurstState>()
        )),

        // ── 音画同步 / 一致性 ──
        "avsync_start" => {
            let cfg: vqa_core::avsync::AvSyncCfg = serde_json::from_value(a(args, "cfg").clone())
                .map_err(|e| format!("参数无效：{e}"))?;
            if !web_source_allowed(&cfg.source) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
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
            let cfg: vqa_core::consist::ConsistCfg = serde_json::from_value(a(args, "cfg").clone())
                .map_err(|e| format!("参数无效：{e}"))?;
            if !web_source_allowed(&cfg.reference) || !web_source_allowed(&cfg.target) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
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
        "capture_start" => {
            let url = s(args, "url");
            if !web_source_allowed(&url) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            json!(capture_cmds::capture_start(
                app.state::<capture_cmds::CaptureState>(),
                url,
                s2(args, "nicIp", "nic_ip"),
            )?)
        }
        "capture_stop" => json!(capture_cmds::capture_stop(
            app.state::<capture_cmds::CaptureState>()
        )?),
        "capture_status" => json!(capture_cmds::capture_status(
            app.state::<capture_cmds::CaptureState>()
        )),

        // ── 检测画面预览 ──
        "preview_start" => {
            let src = s(args, "src");
            if !web_source_allowed(&src) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            sync_cmds::preview_start(
                app.state::<crate::preview::Preview>(),
                s(args, "owner"),
                src,
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
            let source = s(args, "source");
            if !web_source_allowed(&source) {
                return Err("Web 端只允许网络来源、本次上传文件或频道库中的本地来源".into());
            }
            alarm_cmds::alarm_capture(
                s2(args, "recordId", "record_id"),
                source,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_source_rejects_local_and_file_urls() {
        assert!(web_source_allowed("udp://239.1.1.1:5000"));
        assert!(web_source_allowed("rtp://239.1.1.1:5000"));
        assert!(web_source_allowed("http://example.com/a.m3u8"));
        assert!(!web_source_allowed("file:///C:/Windows/System32/drivers/etc/hosts"));
        assert!(!web_source_allowed("C:\\Windows\\System32\\drivers\\etc\\hosts"));
        assert!(!web_source_allowed("..\\..\\Windows\\win.ini"));
    }

    #[test]
    fn upload_extension_whitelist() {
        assert!(allowed_upload("a.mp4"));
        assert!(allowed_upload("a.TS"));
        assert!(!allowed_upload("a.exe"));
        assert!(!allowed_upload("a"));
    }

    #[test]
    fn session_is_revoked_on_logout() {
        let sessions = Mutex::new(HashMap::new());
        let token = issue_session(&sessions);
        assert!(session_valid(&sessions, &token));
        revoke_session(&sessions, &token);
        assert!(!session_valid(&sessions, &token));
    }

    #[test]
    fn web_ui_prefs_are_scoped() {
        assert!(web_ui_pref_readable("analysis.nic"));
        assert!(!web_ui_pref_writable("analysis.nic"));
        assert!(web_ui_pref_readable("capture.prefs"));
        assert!(!web_ui_pref_writable("capture.prefs"));
        assert!(!web_ui_pref_readable("web.access_code"));
        assert!(!web_ui_pref_writable("web.access_code"));
        assert!(!web_ui_pref_readable("diag_token"));

        assert!(web_ui_pref_readable("channels.groups"));
        assert!(!web_ui_pref_writable("channels.groups"));
        assert!(web_ui_pref_writable("channels.analysis"));
        assert!(web_ui_pref_writable("channels.groups.avsync"));
        assert!(!web_ui_pref_writable("channels.player"));
        assert!(web_ui_pref_writable("avsync.prefs"));
        assert!(web_ui_pref_writable("burst.settings"));
        assert!(web_ui_pref_writable("analysis.poll_secs"));
    }

    #[test]
    fn host_nic_is_kept_for_web_prefs() {
        let current = json!({ "nic": "192.168.1.10", "secs": 20 });
        let out = keep_host_nic(json!({ "nic": "10.0.0.1", "secs": 30 }), Some(&current)).unwrap();
        assert_eq!(out["nic"].as_str(), Some("192.168.1.10"));
        assert_eq!(out["secs"].as_u64(), Some(30));

        let out = keep_host_nic(json!({ "nic": "10.0.0.1" }), None).unwrap();
        assert_eq!(out["nic"].as_str(), Some(""));
    }
}
