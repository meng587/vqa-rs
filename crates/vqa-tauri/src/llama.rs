//! 内置 llama-server 运行时。
//!
//! 设计目标：主安装包只带 CPU 版 llama.cpp 运行时（约 23 MB），GGUF 模型独立
//! 下载/导入。应用启动模型时在本机回环地址拉起 `llama-server.exe`，再把
//! `AiConfig.local` 指向它的 OpenAI 兼容 `/v1` 端点；停止时只杀子进程，配置保留。
//!
//! 子进程带 `CREATE_NO_WINDOW`，不会弹 cmd 黑窗；日志写到 AI 目录下的
//! `llama-server.log`，启动失败时把最后几行回给前端。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::ai_cmds;

const ENGINE_VERSION: &str = "llama.cpp b12556 (CPU x64)";
const START_TIMEOUT_SECS: u64 = 120;

pub struct LlamaState {
    inner: Arc<Mutex<LlamaRuntime>>,
}

impl LlamaState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LlamaRuntime::default())),
        }
    }
}

#[derive(Default)]
struct LlamaRuntime {
    running: Option<Running>,
    starting: bool,
    error: String,
    generation: u64,
}

struct Running {
    child: Child,
    log_path: PathBuf,
    base_url: String,
    port: u16,
    model_id: String,
    model_name: String,
    model_path: String,
    ready: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LlamaStatus {
    pub engine_present: bool,
    pub engine_version: String,
    pub running: bool,
    pub starting: bool,
    pub base_url: String,
    pub model_id: String,
    pub model_name: String,
    pub model_path: String,
    pub port: u16,
    pub pid: u32,
    pub error: String,
    pub log_tail: String,
}

fn engine_path(app: &AppHandle) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        cands.push(dir.join("bin/llama/llama-server.exe"));
        cands.push(dir.join("resources/bin/llama/llama-server.exe"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("bin/llama/llama-server.exe"));
            cands.push(dir.join("resources/bin/llama/llama-server.exe"));
            if let Some(up) = dir.parent() {
                cands.push(up.join("bin/llama/llama-server.exe"));
            }
        }
    }
    cands.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/llama/llama-server.exe"));
    cands.into_iter().find(|p| p.is_file())
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("查找空闲端口失败：{e}"))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("读取空闲端口失败：{e}"))
}

fn log_path() -> PathBuf {
    ai_cmds::models_dir()
        .parent()
        .map(|p| p.join("llama-server.log"))
        .unwrap_or_else(|| PathBuf::from("llama-server.log"))
}

fn read_log_tail(path: &Path, max_lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..]
        .join("\n")
        .chars()
        .take(2400)
        .collect::<String>()
}

fn health_ok(client: &reqwest::blocking::Client, base_url: &str) -> bool {
    client
        .get(format!("{base_url}/health"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn set_start_failure(
    inner: &Arc<Mutex<LlamaRuntime>>,
    generation: u64,
    message: String,
) -> bool {
    let Ok(mut g) = inner.lock() else {
        return false;
    };
    if g.generation != generation {
        return false;
    }
    g.running = None;
    g.starting = false;
    g.error = message;
    true
}

fn health_watch(
    inner: Arc<Mutex<LlamaRuntime>>,
    generation: u64,
    base_url: String,
    model_id: String,
    log_path: PathBuf,
) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            set_start_failure(&inner, generation, format!("创建健康检查客户端失败：{e}"));
            return;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(START_TIMEOUT_SECS);
    loop {
        if Instant::now() > deadline {
            let tail = read_log_tail(&log_path, 16);
            set_start_failure(
                &inner,
                generation,
                format!("模型加载超时（{START_TIMEOUT_SECS} 秒）。{tail}"),
            );
            return;
        }

        // 先确认子进程还活着，再发 /health，避免对已退出进程空等。
        let mut dead: Option<String> = None;
        {
            let Ok(mut g) = inner.lock() else {
                return;
            };
            if g.generation != generation {
                return;
            }
            let Some(run) = g.running.as_mut() else {
                return;
            };
            match run.child.try_wait() {
                Ok(Some(status)) => {
                    let tail = read_log_tail(&run.log_path, 16);
                    dead = Some(if tail.is_empty() {
                        format!("llama-server 已退出（{status}）")
                    } else {
                        format!("llama-server 已退出（{status}）。{tail}")
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    let tail = read_log_tail(&run.log_path, 16);
                    dead = Some(format!("检查 llama-server 状态失败：{e}。{tail}"));
                }
            }
        }
        if let Some(message) = dead {
            set_start_failure(&inner, generation, message);
            return;
        }

        if health_ok(&client, &base_url) {
            let Ok(mut g) = inner.lock() else {
                return;
            };
            if g.generation != generation {
                return;
            }
            if let Some(run) = g.running.as_mut() {
                run.ready = true;
                g.starting = false;
                g.error.clear();
                // 就绪与写回配置放在同一把锁内：若用户在加载完成瞬间点“取消/停止”，
                // stop 会等这里写完再回收进程，不会把端点写到已停止的实例上。
                let endpoint = format!("{base_url}/v1");
                if let Err(e) = ai_cmds::set_local_endpoint(&endpoint, &model_id) {
                    g.error = format!("模型已启动，但写回 AI 配置失败：{e}");
                }
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn status_from(app: &AppHandle, runtime: &mut LlamaRuntime) -> LlamaStatus {
    let engine = engine_path(app);
    let mut status = LlamaStatus {
        engine_present: engine.is_some(),
        engine_version: ENGINE_VERSION.to_string(),
        ..LlamaStatus::default()
    };

    // 子进程可能已经退出，但之前没人调用过 status；这里顺手收尸。
    let mut dead: Option<String> = None;
    if let Some(run) = runtime.running.as_mut() {
        match run.child.try_wait() {
            Ok(Some(exit)) => {
                let tail = read_log_tail(&run.log_path, 12);
                dead = Some(if tail.is_empty() {
                    format!("llama-server 已退出（{exit}）")
                } else {
                    format!("llama-server 已退出（{exit}）。{tail}")
                });
            }
            Ok(None) => {
                status.running = run.ready;
                status.starting = runtime.starting || !run.ready;
                status.base_url = run.base_url.clone();
                status.model_id = run.model_id.clone();
                status.model_name = run.model_name.clone();
                status.model_path = run.model_path.clone();
                status.port = run.port;
                status.pid = run.child.id();
            }
            Err(e) => {
                let tail = read_log_tail(&run.log_path, 12);
                dead = Some(format!("检查 llama-server 状态失败：{e}。{tail}"));
            }
        }
    }
    if let Some(message) = dead {
        runtime.running = None;
        runtime.starting = false;
        runtime.error = message;
    }
    if runtime.running.is_none() {
        status.starting = runtime.starting;
    }
    status.error = runtime.error.clone();
    if !status.error.is_empty() || status.starting {
        let p = log_path();
        status.log_tail = read_log_tail(&p, 12);
    }
    status
}

#[tauri::command]
pub fn llama_status(app: AppHandle, state: State<'_, LlamaState>) -> LlamaStatus {
    let mut g = state.inner.lock().unwrap_or_else(|e| e.into_inner());
    status_from(&app, &mut g)
}

#[tauri::command]
pub fn llama_start(
    app: AppHandle,
    state: State<'_, LlamaState>,
    model_id: String,
) -> Result<LlamaStatus, String> {
    let model_id = model_id.trim().to_string();
    if model_id.is_empty() {
        return Err("模型 id 为空".into());
    }
    let (model_path, model_name) = ai_cmds::resolve_model(&model_id)?;
    let engine = engine_path(&app).ok_or_else(|| {
        "安装包缺少 llama-server 运行时（bin/llama/llama-server.exe）；请重新安装完整版本".to_string()
    })?;

    let mut g = state.inner.lock().unwrap_or_else(|e| e.into_inner());
    if g.starting {
        return Err("模型正在加载，请稍候".into());
    }
    let same_ready = g
        .running
        .as_ref()
        .map(|r| r.model_id == model_id && r.ready)
        .unwrap_or(false);
    if same_ready {
        return Ok(status_from(&app, &mut g));
    }    // 切换模型：先收掉旧进程；Running 的 Drop 会 kill + wait。
    g.running = None;
    g.error.clear();
    g.starting = true;
    g.generation = g.generation.wrapping_add(1);
    let generation = g.generation;

    let port = match free_port() {
        Ok(p) => p,
        Err(e) => {
            g.starting = false;
            g.error = e.clone();
            return Err(e);
        }
    };
    let base_url = format!("http://127.0.0.1:{port}");
    let log = log_path();
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let stdout = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log)
    {
        Ok(f) => f,
        Err(e) => {
            g.starting = false;
            g.error = format!("创建 llama-server 日志失败：{e}");
            return Err(g.error.clone());
        }
    };
    let stderr = stdout
        .try_clone()
        .map_err(|e| {
            g.starting = false;
            g.error = format!("复制 llama-server 日志句柄失败：{e}");
            g.error.clone()
        })?;

    let mut cmd = Command::new(&engine);
    cmd.current_dir(engine.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .arg("-m")
        .arg(&model_path)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("-c")
        .arg("4096")
        .arg("-np")
        .arg("1")
        .arg("-a")
        .arg(&model_id)
        .arg("--jinja")
        .arg("--reasoning")
        .arg("off")
        .arg("--no-webui");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            g.starting = false;
            g.error = format!("启动 llama-server 失败：{e}");
            return Err(g.error.clone());
        }
    };
    g.running = Some(Running {
        child,
        log_path: log.clone(),
        base_url: base_url.clone(),
        port,
        model_id: model_id.clone(),
        model_name: model_name.clone(),
        model_path: model_path.to_string_lossy().to_string(),
        ready: false,
    });
    let status = status_from(&app, &mut g);
    drop(g);

    let inner = state.inner.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("vqa-llama-health".into())
        .spawn(move || health_watch(inner, generation, base_url, model_id, log))
    {
        let mut g = state.inner.lock().unwrap_or_else(|err| err.into_inner());
        g.running = None;
        g.starting = false;
        g.error = format!("启动健康检查线程失败：{e}");
        return Err(g.error.clone());
    }
    Ok(status)
}

#[tauri::command]
pub fn llama_stop(app: AppHandle, state: State<'_, LlamaState>) -> LlamaStatus {
    let mut g = state.inner.lock().unwrap_or_else(|e| e.into_inner());
    g.generation = g.generation.wrapping_add(1);
    g.running = None;
    g.starting = false;
    g.error.clear();
    status_from(&app, &mut g)
}

/// 退出清理：只停子进程，不改配置。
pub fn shutdown(app: &AppHandle) {
    let state = app.state::<LlamaState>();
    let mut g = state.inner.lock().unwrap_or_else(|e| e.into_inner());
    g.generation = g.generation.wrapping_add(1);
    g.running = None;
    g.starting = false;
}
