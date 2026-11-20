//! EasyTier 组网页命令 —— 管理随包分发的 easytier-core / easytier-cli。
//!
//! 设计取舍：
//! * 只管理本页启动的内核实例，不接管系统中已有的 EasyTier GUI/服务；
//! * RPC 默认走 127.0.0.1:15889，避开官方 GUI 默认的 15888；
//! * 创建 TUN 需要管理员权限，非管理员时只允许显式启用的 `no_tun` 模式；
//! * 不向前端暴露任意命令行参数，CLI 只允许固定的 node/peer 查询。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};

const CORE: &str = "easytier-core.exe";
const CLI: &str = "easytier-cli.exe";
const DEFAULT_RPC_PORT: u16 = 15889;
const DEFAULT_LISTEN_PORT: u16 = 11010;
const LOG_LINES: usize = 400;
const LOG_TAIL: usize = 140;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EasytierConfig {
    pub network_name: String,
    pub network_secret: String,
    pub ipv4: String,
    pub dhcp: bool,
    pub peers: Vec<String>,
    pub proxy_networks: Vec<String>,
    pub external_node: String,
    pub hostname: String,
    pub rpc_port: u16,
    pub no_listener: bool,
    pub no_tun: bool,
}

impl Default for EasytierConfig {
    fn default() -> Self {
        Self {
            network_name: String::new(),
            network_secret: String::new(),
            ipv4: String::new(),
            dhcp: false,
            peers: Vec::new(),
            proxy_networks: Vec::new(),
            external_node: String::new(),
            hostname: String::new(),
            rpc_port: DEFAULT_RPC_PORT,
            no_listener: false,
            no_tun: false,
        }
    }
}

pub struct EasytierState {
    pub child: Mutex<Option<Child>>,
    pub logs: Arc<Mutex<VecDeque<String>>>,
    pub started: Mutex<Option<Instant>>,
    pub version: Mutex<String>,
    pub admin: bool,
}

impl EasytierState {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            started: Mutex::new(None),
            version: Mutex::new(String::new()),
            admin: detect_admin(),
        }
    }
}

#[derive(Serialize)]
pub struct EasytierStatus {
    running: bool,
    admin: bool,
    version: String,
    config: EasytierConfig,
    uptime_secs: u64,
    pid: Option<u32>,
    node: Option<Value>,
    peers: Vec<Value>,
    log: String,
    error: Option<String>,
}

fn hidden(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// CLI 查询可能因内核 RPC 未就绪或网络阻塞而挂住；状态轮询前必须限时。
fn output_with_timeout(mut c: Command, timeout: Duration) -> Result<Output, String> {
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("启动查询进程失败：{e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("读取查询结果失败：{e}"));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("读取查询进程状态失败：{e}"));
            }
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("查询超过 {} 秒未返回", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// 开发态从仓库 bin 找，安装态从 Tauri resource_dir / exe 同级 bin 找。
fn bin_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        dirs.push(res.join("bin"));
        dirs.push(res.join("bin").join("easytier"));
        dirs.push(res.join("resources").join("bin"));
        dirs.push(res.clone());
    }
    if let Some(p) = std::env::var_os("VQA_EASYTIER_DIR") {
        dirs.push(PathBuf::from(p));
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dirs.push(manifest.join("../../bin/easytier"));
    dirs.push(manifest.join("../../bin"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("bin"));
            dirs.push(parent.join("bin").join("easytier"));
            dirs.push(parent.join("resources").join("bin"));
        }
    }
    for d in dirs {
        if is_file(&d.join(CORE)) && is_file(&d.join(CLI)) {
            return Ok(d);
        }
    }
    Err("未找到内置 EasyTier 内核（easytier-core.exe / easytier-cli.exe）".into())
}

fn config_path() -> Result<EasytierConfig, String> {
    let v = vqa_core::config::load_config("easytier", serde_json::json!({}));
    Ok(serde_json::from_value(v).unwrap_or_default())
}

fn save_cfg(cfg: &EasytierConfig) -> Result<(), String> {
    let v = serde_json::to_value(cfg).map_err(|e| e.to_string())?;
    vqa_core::config::save_config("easytier", &v).map_err(|e| e.to_string())
}

fn detect_admin() -> bool {
    let mut c = Command::new("whoami.exe");
    c.arg("/groups");
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    match c.output() {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout);
            s.contains("S-1-16-12288") || s.contains("High Mandatory Level")
        }
        Err(_) => false,
    }
}

fn core_version(bin: &Path) -> String {
    let mut c = hidden(&bin.join(CORE));
    c.arg("--version").stdin(Stdio::null());
    let out = match output_with_timeout(c, Duration::from_secs(5)) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn push_log(logs: &Arc<Mutex<VecDeque<String>>>, line: String) {
    if line.trim().is_empty() {
        return;
    }
    let mut g = logs.lock().unwrap_or_else(|e| e.into_inner());
    g.push_back(line.trim_end().to_string());
    while g.len() > LOG_LINES {
        g.pop_front();
    }
}

fn pump<R: Read + Send + 'static>(reader: R, logs: Arc<Mutex<VecDeque<String>>>) {
    let mut br = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match br.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => push_log(&logs, line.replace('\r', "")),
            Err(_) => break,
        }
    }
}

fn redact_secret(line: &str) -> String {
    // core 启动日志会打印 TOML，其中包含 network_secret；回传前端前打码。
    if let Some(i) = line.find("network_secret") {
        if let Some(eq) = line[i..].find('=') {
            let start = i + eq + 1;
            if let Some(q1) = line[start..].find('"') {
                let q1 = start + q1;
                if let Some(q2) = line[q1 + 1..].rfind('"') {
                    let q2 = q1 + 1 + q2;
                    let mut out = String::new();
                    out.push_str(&line[..q1 + 1]);
                    out.push_str("***");
                    out.push_str(&line[q2..]);
                    return out;
                }
            }
        }
    }
    line.to_string()
}

fn tail_log(logs: &Arc<Mutex<VecDeque<String>>>) -> String {
    let g = logs.lock().unwrap_or_else(|e| e.into_inner());
    let begin = g.len().saturating_sub(LOG_TAIL);
    g.iter()
        .skip(begin)
        .map(|s| redact_secret(s))
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_arg(args: &mut Vec<String>, key: &str, val: &str) {
    let v = val.trim();
    if !v.is_empty() {
        args.push(key.to_string());
        args.push(v.to_string());
    }
}

fn build_args(cfg: &EasytierConfig) -> Result<Vec<String>, String> {
    let name = cfg.network_name.trim();
    let secret = cfg.network_secret.trim();
    if name.is_empty() {
        return Err("网络名称为空".into());
    }
    if secret.is_empty() {
        return Err("网络密钥为空".into());
    }
    if cfg.rpc_port < 1024 {
        return Err("RPC 端口需在 1024-65535 之间".into());
    }
    if !cfg.no_tun && !cfg.dhcp && cfg.ipv4.trim().is_empty() {
        return Err("请填写虚拟 IPv4，或勾选自动分配 IPv4 / no-TUN 模式".into());
    }
    let mut a = Vec::new();
    push_arg(&mut a, "--network-name", name);
    push_arg(&mut a, "--network-secret", secret);
    if cfg.dhcp {
        a.push("--dhcp".into());
        a.push("true".into());
    } else {
        push_arg(&mut a, "--ipv4", &cfg.ipv4);
    }
    for p in &cfg.peers {
        push_arg(&mut a, "--peers", p);
    }
    for p in &cfg.proxy_networks {
        push_arg(&mut a, "--proxy-networks", p);
    }
    push_arg(&mut a, "--external-node", &cfg.external_node);
    push_arg(&mut a, "--hostname", &cfg.hostname);
    a.push("--rpc-portal".into());
    a.push(format!("127.0.0.1:{}", cfg.rpc_port));
    if cfg.no_listener {
        a.push("--no-listener".into());
    }
    if cfg.no_tun {
        a.push("--no-tun".into());
        a.push("true".into());
    }
    Ok(a)
}

/// core 默认监听 11010；官方 GUI 常驻时先给出明确提示，而不是让内核退出后才看日志。
fn ensure_listener_port_free(cfg: &EasytierConfig) -> Result<(), String> {
    if cfg.no_listener {
        return Ok(());
    }
    let addr = format!("0.0.0.0:{DEFAULT_LISTEN_PORT}");
    let tcp = std::net::TcpListener::bind(&addr).map_err(|_| {
        format!(
            "默认监听端口 {DEFAULT_LISTEN_PORT} 已被占用（可能 EasyTier GUI 正在运行）。请在高级选项勾选“不监听入站端口”，或关闭占用程序后重试。"
        )
    })?;
    let udp = std::net::UdpSocket::bind(&addr).map_err(|_| {
        format!(
            "默认监听端口 {DEFAULT_LISTEN_PORT} 的 UDP 已被占用（可能 EasyTier GUI 正在运行）。请在高级选项勾选“不监听入站端口”，或关闭占用程序后重试。"
        )
    })?;
    drop(tcp);
    drop(udp);
    Ok(())
}

fn query_cli(bin: &Path, port: u16, what: &str) -> Result<Value, String> {
    let mut c = hidden(&bin.join(CLI));
    c.current_dir(bin)
        .args(["-p", &format!("127.0.0.1:{port}"), "-o", "json", what])
        .stdin(Stdio::null());
    let out = output_with_timeout(c, Duration::from_secs(4))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let err = if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        };
        return Err(if err.is_empty() {
            "EasyTier CLI 查询失败".into()
        } else {
            err
        });
    }
    let txt = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(txt.trim()).map_err(|e| format!("EasyTier CLI 返回无法解析：{e}"))
}

fn sanitize_node(v: Value) -> Value {
    // CLI 的 node 视图里带完整 config（含 network_secret），只挑展示所需字段。
    let mut out = serde_json::Map::new();
    if let Some(obj) = v.as_object() {
        for key in [
            "peer_id",
            "ipv4_addr",
            "hostname",
            "stun_info",
            "inst_id",
            "version",
            "proxy_cidrs",
        ] {
            if let Some(val) = obj.get(key) {
                out.insert(key.to_string(), val.clone());
            }
        }
    }
    Value::Object(out)
}

fn sanitize_peers(peers: Vec<Value>) -> Vec<Value> {
    // peer 列表会把自己（cost=Local）也列出来，这里只保留真正的对端。
    peers
        .into_iter()
        .filter(|p| {
            let local = p
                .get("cost")
                .and_then(|v| v.as_str())
                .map(|s| s == "Local")
                .unwrap_or(false);
            !local
        })
        .collect()
}

#[tauri::command]
pub fn easytier_config_get() -> Result<EasytierConfig, String> {
    config_path()
}

#[tauri::command]
pub fn easytier_config_save(cfg: EasytierConfig) -> Result<(), String> {
    save_cfg(&cfg)
}

#[tauri::command]
pub fn easytier_start(
    app: AppHandle,
    st: State<'_, EasytierState>,
    cfg: EasytierConfig,
) -> Result<(), String> {
    let bin = bin_dir(&app)?;
    let needs_tun = !cfg.no_tun && (cfg.dhcp || !cfg.ipv4.trim().is_empty());
    if needs_tun && !st.admin {
        return Err("当前工作站不是管理员权限，创建 TUN 会失败。请点“以管理员身份重启”，或在高级选项勾选 no-TUN。".into());
    }
    {
        let mut g = st.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ch) = g.as_mut() {
            match ch.try_wait() {
                Ok(None) => return Err("EasyTier 已在运行，请先停止".into()),
                _ => {
                    g.take();
                }
            }
        }
    }
    let args = build_args(&cfg)?;
    ensure_listener_port_free(&cfg)?;
    save_cfg(&cfg)?;
    let mut c = hidden(&bin.join(CORE));
    c.current_dir(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("启动 EasyTier 失败：{e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    st.logs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    push_log(&st.logs, "正在启动 EasyTier 内核…".into());
    if let Some(s) = stdout {
        let logs = st.logs.clone();
        std::thread::spawn(move || pump(s, logs));
    }
    if let Some(s) = stderr {
        let logs = st.logs.clone();
        std::thread::spawn(move || pump(s, logs));
    }
    *st.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    *st.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    *st.version.lock().unwrap_or_else(|e| e.into_inner()) = core_version(&bin);
    Ok(())
}

#[tauri::command]
pub fn easytier_stop(st: State<'_, EasytierState>) -> Result<(), String> {
    let mut g = st.child.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = g.take() {
        let _ = child.kill();
        let _ = child.wait();
        push_log(&st.logs, "EasyTier 已停止。".into());
    }
    *st.started.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(())
}

#[tauri::command]
pub fn easytier_status(
    app: AppHandle,
    st: State<'_, EasytierState>,
) -> Result<EasytierStatus, String> {
    let cfg = config_path()?;
    let mut running = false;
    let mut pid = None;
    let mut error: Option<String> = None;
    {
        let mut g = st.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ch) = g.as_mut() {
            match ch.try_wait() {
                Ok(None) => {
                    running = true;
                    pid = Some(ch.id());
                }
                Ok(Some(status)) => {
                    let code = status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "未知".into());
                    error = Some(format!("EasyTier 已退出（退出码 {code}）"));
                    g.take();
                    *st.started.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                Err(e) => {
                    running = true;
                    error = Some(format!("读取 EasyTier 进程状态失败：{e}"));
                }
            }
        }
    }

    let mut node = None;
    let mut peers = Vec::new();
    let mut version = st.version.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if running {
        if let Ok(bin) = bin_dir(&app) {
            if version.is_empty() {
                version = core_version(&bin);
                *st.version.lock().unwrap_or_else(|e| e.into_inner()) = version.clone();
            }
            match query_cli(&bin, cfg.rpc_port, "node") {
                Ok(v) => node = Some(sanitize_node(v)),
                Err(_) => {
                    // 内核刚起时 RPC 可能还没就绪，指标留空等下一轮轮询；
                    // 真正的启动失败由子进程退出码与日志面板呈现。
                }
            }
            match query_cli(&bin, cfg.rpc_port, "peer") {
                Ok(Value::Array(v)) => peers = sanitize_peers(v),
                Ok(v) => peers = sanitize_peers(vec![v]),
                Err(_) => {}
            }
        } else if error.is_none() {
            error = Some("未找到内置 EasyTier 内核".into());
        }
    }

    let uptime_secs = st
        .started
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    let mut visible_cfg = cfg.clone();
    visible_cfg.network_secret.clear();
    Ok(EasytierStatus {
        running,
        admin: st.admin,
        version,
        config: visible_cfg,
        uptime_secs,
        pid,
        node,
        peers,
        log: tail_log(&st.logs),
        error,
    })
}

/// 非管理员时由 UI 调用：延迟 2 秒拉起一个 elevated 的新实例，然后退出当前实例。
/// 延迟是为了让单实例插件先释放旧进程。
#[tauri::command]
pub fn easytier_restart_elevated(app: AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Start-Sleep -Seconds 2; Start-Process -FilePath '{}' -Verb RunAs",
        exe
    );
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script]);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c.spawn().map_err(|e| format!("无法启动管理员实例：{e}"))?;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base_cfg() -> EasytierConfig {
        EasytierConfig {
            network_name: "vqa-test".into(),
            network_secret: "secret-123".into(),
            ipv4: "10.144.144.2".into(),
            ..EasytierConfig::default()
        }
    }

    #[test]
    fn build_args_keeps_core_contract() {
        let args = build_args(&base_cfg()).unwrap();
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--network-name" && w[1] == "vqa-test"));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--ipv4" && w[1] == "10.144.144.2"));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--rpc-portal" && w[1] == "127.0.0.1:15889"));
        assert!(!args.iter().any(|s| s == "--no-tun"));
    }

    #[test]
    fn no_tun_mode_reaches_core() {
        let mut cfg = base_cfg();
        cfg.no_tun = true;
        cfg.no_listener = true;
        let args = build_args(&cfg).unwrap();
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--no-tun" && w[1] == "true"));
        assert!(args.iter().any(|s| s == "--no-listener"));
    }

    #[test]
    fn node_view_never_carries_config_or_secret() {
        let raw = json!({
            "peer_id": 7,
            "ipv4_addr": "10.144.144.2",
            "hostname": "vqa-a",
            "stun_info": {"public_ip": ["203.0.113.7"]},
            "version": "2.6.4",
            "config": "network_secret = \"secret-123\""
        });
        let view = sanitize_node(raw);
        assert_eq!(view.get("peer_id"), Some(&json!(7)));
        assert!(view.get("config").is_none());
        assert!(!view.to_string().contains("secret-123"));
    }

    #[test]
    fn local_peer_is_not_a_connection() {
        let peers = vec![
            json!({"id": "1", "cost": "Local", "hostname": "self"}),
            json!({"id": "2", "cost": "p2p", "hostname": "other"}),
        ];
        let remote = sanitize_peers(peers);
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].get("hostname"), Some(&json!("other")));
    }

    #[test]
    fn log_secret_is_masked() {
        let line = "network_secret = \"secret-123\"";
        let masked = redact_secret(line);
        assert!(masked.contains("***"));
        assert!(!masked.contains("secret-123"));
    }

    #[test]
    fn log_secret_with_escaped_quote_is_fully_masked() {
        let line = "network_secret = \"a\\\"b\"";
        let masked = redact_secret(line);
        assert!(masked.contains("***"));
        assert!(!masked.contains("a\\\"b"));
    }

    #[cfg(windows)]
    #[test]
    fn timed_output_captures_stdout() {
        let mut c = Command::new("cmd.exe");
        c.args(["/C", "echo", "easytier"]);
        let out = output_with_timeout(c, Duration::from_secs(2)).expect("查询应返回");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("easytier"));
    }
}
