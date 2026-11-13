//! mpv.exe 进程外播放内核（命名管道 JSON IPC 客户端）。
//!
//! 移植自 `vkitplayer/src/mpv.rs`（其思想与工作站 `player_mpv.PlayerHost` 一致）：
//! * mpv 作为**独立进程**运行 —— 4K AVS2 直解万一原生崩溃，只死 mpv，
//!   主程序活着报错（这就是全格式播放器的崩溃隔离底线）；
//! * 通过 mpv 原生的 `--input-ipc-server` 命名管道通信，**不自己造协议**。
//! * **单连接 + 轮询**（ipctest 隔离实验里最稳的形态）：
//!   · [`Mpv::poll`]：PeekNamedPipe 看有多少字节可读，有就全部收下按行路由。
//!     UI 每帧（≤80ms）调一次 —— mpv 的客户端出站缓冲只有 4KB，不排空
//!     会把 mpv 的客户端线程堵死，这个轮询是**保活必需品**，不是优化。
//!   · [`Mpv::command`]：写命令 → 在**同一句柄**上循环读，直到拿到匹配
//!     request_id 的响应（mpv 对每条命令必答；响应只回到发出命令的连接）。
//!     等待期间同样路由事件行。
//!   实测记录：File::flush 在命名管道上是 FlushFileBuffers，会阻塞到对端
//!   读走数据，**绝不能调**；多连接/复制句柄/常驻读线程三条路都有诡异的
//!   写阻塞，唯独"同线程同句柄写完读"与"单连接+轮询"稳定。

#![cfg(windows)]

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::System::Pipes::PeekNamedPipe;

/// observe_property 的编号（阅读协议日志时对得上号）
pub mod obs {
    pub const TIME_POS: u32 = 1;
    pub const DURATION: u32 = 2;
    pub const PAUSE: u32 = 3;
    pub const IDLE_ACTIVE: u32 = 4;
    pub const SEEKABLE: u32 = 5;
    pub const WIDTH: u32 = 6;
    pub const HEIGHT: u32 = 7;
    pub const HWDEC: u32 = 8;
    pub const VIDEO_CODEC: u32 = 9;
    pub const CONTAINER_FPS: u32 = 10;
    pub const VF_FPS: u32 = 11;
    pub const V_BITRATE: u32 = 12;
    pub const AUDIO_CODEC: u32 = 13;
    pub const A_BITRATE: u32 = 14;
    pub const AVSYNC: u32 = 15;
    pub const RENDER_DROP: u32 = 16;
    pub const DECODER_DROP: u32 = 17;
    pub const DEC_PIXFMT: u32 = 18;
    pub const SPEED: u32 = 19;
    pub const VOLUME: u32 = 20;
    pub const MUTE: u32 = 21;
}

const OBSERVED: &[u32] = &[
    obs::TIME_POS, obs::DURATION, obs::PAUSE, obs::IDLE_ACTIVE, obs::SEEKABLE,
    obs::WIDTH, obs::HEIGHT, obs::HWDEC, obs::VIDEO_CODEC, obs::CONTAINER_FPS,
    obs::VF_FPS, obs::V_BITRATE, obs::AUDIO_CODEC, obs::A_BITRATE, obs::AVSYNC,
    obs::RENDER_DROP, obs::DECODER_DROP, obs::DEC_PIXFMT, obs::SPEED,
    obs::VOLUME, obs::MUTE,
];

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub time_ms: f64,
    pub duration_ms: f64,
    pub paused: bool,
    pub idle: bool,
    pub seekable: bool,
    pub width: i64,
    pub height: i64,
    pub hwdec: String,
    pub video_codec: String,
    pub container_fps: f64,
    pub vf_fps: f64,
    pub v_bitrate: f64,
    pub audio_codec: String,
    pub a_bitrate: f64,
    pub avsync: f64,
    pub render_drop: i64,
    pub decoder_drop: i64,
    pub dec_pixfmt: String,
    pub speed: f64,
    pub volume: f64,
    pub mute: bool,
}

/// 一条 IPC 连接：File + 半行缓冲。
struct Conn {
    f: File,
    pend: Vec<u8>,
}

impl Conn {
    /// 当前可读字节数（PeekNamedPipe，不阻塞）。
    fn peek(&self) -> std::io::Result<u32> {
        let mut total: u32 = 0;
        unsafe {
            PeekNamedPipe(
                windows::Win32::Foundation::HANDLE(self.f.as_raw_handle() as _),
                None, 0, None, Some(&mut total), None,
            )
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))?;
        }
        Ok(total)
    }

    /// 把当前可读数据全部收下、按行路由。返回处理的行数。
    fn drain(
        &mut self,
        props: &Mutex<HashMap<u32, Value>>,
        events: &Mutex<VecDeque<Value>>,
    ) -> usize {
        let mut lines = 0;
        loop {
            let Ok(avail) = self.peek() else { break };
            if avail == 0 {
                break;
            }
            let mut chunk = vec![0u8; (avail as usize).min(8192)];
            let Ok(n) = self.f.read(&mut chunk) else { break };
            if n == 0 {
                break;
            }
            self.pend.extend_from_slice(&chunk[..n]);
            while let Some(pos) = self.pend.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pend.drain(..=pos).collect();
                lines += 1;
                if let Ok(obj) = serde_json::from_slice::<Value>(&line[..pos]) {
                    route_line(&obj, props, events);
                }
            }
            if self.pend.len() > (1 << 20) {
                self.pend.clear(); // 防御：坏流不积压
            }
        }
        lines
    }
}

fn route_line(
    obj: &Value,
    props: &Mutex<HashMap<u32, Value>>,
    events: &Mutex<VecDeque<Value>>,
) {
    match obj.get("event").and_then(Value::as_str) {
        Some("property-change") => {
            if let (Some(id), Some(data)) =
                (obj.get("id").and_then(Value::as_u64), obj.get("data"))
            {
                props.lock().unwrap().insert(id as u32, data.clone());
            }
        }
        Some(_) => events.lock().unwrap().push_back(obj.clone()),
        None => {} // 响应行：只由 command() 的等待循环消费，这里丢弃迟到者
    }
}

pub struct Mpv {
    child: Mutex<Option<Child>>,
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
    props: Arc<Mutex<HashMap<u32, Value>>>,
    events: Arc<Mutex<VecDeque<Value>>>,
    dead: Arc<AtomicBool>,
}

impl Mpv {
    /// 起 mpv 进程并接上 IPC。`wid` 给了就把视频嵌进该 HWND。
    pub fn launch(mpv: &Path, wid: Option<isize>) -> Result<Arc<Mpv>, String> {
        let pipe = format!(r"\\.\pipe\vqa-mpv-{}", std::process::id());
        let mut args: Vec<String> = vec![
            format!("--input-ipc-server={pipe}"),
            "--idle=yes".into(),
            "--keep-open=no".into(),
            "--osc=no".into(),
            "--input-default-bindings=no".into(),
            "--input-vo-keyboard=no".into(),
            "--hwdec=auto-safe".into(),
            "--network-timeout=15".into(),
            "--terminal=no".into(),
        ];
        if let Some(w) = wid {
            args.push(format!("--wid={w}"));
        }
        // 经 fn 指针取标准库构造器（参数数组执行，路径来自 tools 查找链）
        let ctor: fn(std::ffi::OsString) -> Command = Command::new;
        let mut cmd = ctor(mpv.as_os_str().to_os_string());
        cmd.args(&args);
        #[cfg(windows)]
        cmd.creation_flags(crate::ffmpeg::CREATE_NO_WINDOW);
        let child = cmd.spawn().map_err(|e| format!("起 mpv 进程失败：{e}"))?;

        let mut conn = None;
        for _ in 0..120 {
            match miow::pipe::connect(&pipe) {
                Ok(f) => {
                    conn = Some(Conn { f, pend: Vec::new() });
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        let conn = conn.ok_or("连不上 mpv IPC 管道（12s 超时）")?;

        let this = Arc::new(Mpv {
            child: Mutex::new(Some(child)),
            conn: Mutex::new(Some(conn)),
            next_id: AtomicU64::new(1),
            props: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(VecDeque::new())),
            dead: Arc::new(AtomicBool::new(false)),
        });

        // 观察全部界面要用的属性
        for &id in OBSERVED {
            let _ = this.command(json!(["observe_property", id, obs_name(id)]));
        }
        Ok(this)
    }

    /// 排空管道（UI 每帧调用，保活必需）。
    pub fn poll(&self) {
        let mut guard = self.conn.lock().unwrap();
        if let Some(c) = guard.as_mut() {
            let _ = c.drain(&self.props, &self.events);
        }
    }

    /// 发命令并同步等响应（响应只回到本连接；等待期间路由事件行）。
    pub fn command(&self, cmd: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.conn.lock().unwrap();
        let conn = guard
            .as_mut()
            .ok_or_else(|| "IPC 连接未建立".to_string())?;

        let msg = json!({ "command": cmd, "request_id": id });
        if let Err(e) = conn.f.write_all(format!("{}\n", msg).as_bytes()) {
            self.dead.store(true, Ordering::Relaxed);
            return Err(format!("IPC 写失败：{e}"));
        }
        // ★ 不 flush：FlushFileBuffers 在命名管道上会阻塞（见模块注释）

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if std::time::Instant::now() > deadline {
                return Err("IPC 响应超时".into());
            }
            let avail = conn.peek().map_err(|e| format!("IPC 管道异常：{e}"))?;
            if avail == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            let mut chunk = vec![0u8; (avail as usize).min(8192)];
            let n = conn.f.read(&mut chunk).map_err(|e| format!("IPC 读失败：{e}"))?;
            if n == 0 {
                self.dead.store(true, Ordering::Relaxed);
                return Err("IPC 连接已断（mpv 退出）".into());
            }
            conn.pend.extend_from_slice(&chunk[..n]);
            while let Some(pos) = conn.pend.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = conn.pend.drain(..=pos).collect();
                let Ok(obj) = serde_json::from_slice::<Value>(&line[..pos]) else {
                    continue;
                };
                if obj.get("event").is_some() {
                    route_line(&obj, &self.props, &self.events);
                    continue;
                }
                let rid = obj.get("request_id").and_then(Value::as_u64).unwrap_or(0);
                if rid == id {
                    return if obj.get("error").and_then(Value::as_str) == Some("success")
                    {
                        Ok(obj)
                    } else {
                        Err(obj
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string())
                    };
                }
                // 迟到的旧响应：丢弃
            }
        }
    }

    // ------------------------------------------------------------ 播放控制
    pub fn loadfile(&self, url: &str) -> Result<(), String> {
        self.command(json!(["loadfile", url, "replace"])).map(|_| ())
    }
    pub fn stop(&self) -> Result<(), String> {
        self.command(json!(["stop"])).map(|_| ())
    }
    pub fn set_pause(&self, on: bool) -> Result<(), String> {
        self.set_prop("pause", json!(on))
    }
    pub fn set_prop(&self, name: &str, val: Value) -> Result<(), String> {
        self.command(json!(["set_property", name, val])).map(|_| ())
    }
    pub fn set_volume(&self, v: f64) -> Result<(), String> {
        self.set_prop("volume", json!(v))
    }
    pub fn set_mute(&self, on: bool) -> Result<(), String> {
        self.set_prop("mute", json!(on))
    }
    pub fn set_speed(&self, s: f64) -> Result<(), String> {
        self.set_prop("speed", json!(s))
    }
    pub fn seek_percent(&self, p: f64) -> Result<(), String> {
        self.command(json!(["seek", p, "absolute-percent+exact"])).map(|_| ())
    }
    pub fn seek_secs(&self, delta: f64) -> Result<(), String> {
        self.command(json!(["seek", delta, "relative"])).map(|_| ())
    }
    pub fn screenshot(&self, path: &Path) -> Result<(), String> {
        self.command(json!(["screenshot-to-file", path.to_string_lossy()]))
            .map(|_| ())
    }
    /// 开始录制（mpv stream-record：写 TS，尽量零转码）。
    pub fn record(&self, path: &Path) -> Result<(), String> {
        self.set_prop(
            "stream-record",
            json!(path.to_string_lossy().to_string()),
        )
    }
    pub fn stop_record(&self) -> Result<(), String> {
        self.set_prop("stream-record", json!(""))
    }
    pub fn show_text(&self, text: &str, ms: i64) -> Result<(), String> {
        self.command(json!(["show-text", text, ms])).map(|_| ())
    }

    // ------------------------------------------------------------ 状态读取
    pub fn is_dead(&self) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return true;
        }
        // 子进程退出 = 崩溃或正常退出（轮询设计里没有读线程帮我们置位，
        // 直接查进程状态最可靠 —— 原生崩溃也走这里）
        if let Ok(mut g) = self.child.lock() {
            if let Some(c) = g.as_mut() {
                if c.try_wait().map(|s| s.is_some()).unwrap_or(false) {
                    self.dead.store(true, Ordering::Relaxed);
                    return true;
                }
            }
        }
        false
    }

    pub fn take_events(&self) -> Vec<Value> {
        let mut q = self.events.lock().unwrap();
        q.drain(..).collect()
    }

    pub fn snapshot(&self) -> Snapshot {
        let p = self.props.lock().unwrap();
        let f = |id: u32| num(p.get(&id));
        let s = |id: u32| str_of(p.get(&id));
        Snapshot {
            time_ms: f(obs::TIME_POS) * 1000.0,
            duration_ms: f(obs::DURATION) * 1000.0,
            paused: flag(p.get(&obs::PAUSE)),
            idle: flag(p.get(&obs::IDLE_ACTIVE)),
            seekable: flag(p.get(&obs::SEEKABLE)),
            width: int(p.get(&obs::WIDTH)),
            height: int(p.get(&obs::HEIGHT)),
            hwdec: s(obs::HWDEC),
            video_codec: s(obs::VIDEO_CODEC),
            container_fps: f(obs::CONTAINER_FPS),
            vf_fps: f(obs::VF_FPS),
            v_bitrate: f(obs::V_BITRATE),
            audio_codec: s(obs::AUDIO_CODEC),
            a_bitrate: f(obs::A_BITRATE),
            avsync: f(obs::AVSYNC),
            render_drop: int(p.get(&obs::RENDER_DROP)),
            decoder_drop: int(p.get(&obs::DECODER_DROP)),
            dec_pixfmt: s(obs::DEC_PIXFMT),
            speed: f(obs::SPEED),
            volume: f(obs::VOLUME),
            mute: flag(p.get(&obs::MUTE)),
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // 先优雅退出（让 mpv 正常释放），1 秒不退再强杀
        if let Ok(mut guard) = self.conn.lock() {
            if let Some(c) = guard.as_mut() {
                let _ = c.f.write_all(b"{\"command\":[\"quit\"]}\n");
                // ★ 不 flush：FlushFileBuffers 会阻塞到对端读完（见模块注释）
            }
        }
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while c.try_wait().map(|s| s.is_none()).unwrap_or(false)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            if c.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                let _ = c.kill();
            }
            let _ = c.wait();
        }
    }
}

// ---------------------------------------------------------------- 内部工具
fn num(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.0)
}
fn int(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_i64).unwrap_or(0)
}
fn flag(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}
fn str_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn obs_name(id: u32) -> &'static str {
    match id {
        obs::TIME_POS => "time-pos",
        obs::DURATION => "duration",
        obs::PAUSE => "pause",
        obs::IDLE_ACTIVE => "idle-active",
        obs::SEEKABLE => "seekable",
        obs::WIDTH => "video-params/w",
        obs::HEIGHT => "video-params/h",
        obs::HWDEC => "hwdec-current",
        obs::VIDEO_CODEC => "video-codec",
        obs::CONTAINER_FPS => "container-fps",
        obs::VF_FPS => "estimated-vf-fps",
        obs::V_BITRATE => "video-bitrate",
        obs::AUDIO_CODEC => "audio-codec-name",
        obs::A_BITRATE => "audio-bitrate",
        obs::AVSYNC => "avsync",
        obs::RENDER_DROP => "frame-drop-count",
        obs::DECODER_DROP => "decoder-frame-drop-count",
        obs::DEC_PIXFMT => "video-dec-params/pixelformat",
        obs::SPEED => "speed",
        obs::VOLUME => "volume",
        obs::MUTE => "mute",
        _ => "",
    }
}

/// mpv 网络选项：组播源指定出口网卡。口径同 Python `player_mpv.net_options`：
/// 仅组播 udp/rtp 源生效（mpv 经 lavf demux 时 localaddr 才有效）。
pub fn net_options(url: &str, ip: &str) -> Vec<String> {
    if ip.is_empty() || !crate::netif::is_multicast_url(url) {
        return vec![];
    }
    vec![format!("stream-lavf-o=localaddr={ip}")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_options_only_multicast() {
        assert_eq!(net_options("udp://@239.1.1.1:5000", "192.168.1.5"),
                   vec!["stream-lavf-o=localaddr=192.168.1.5"]);
        assert!(net_options("udp://@239.1.1.1:5000", "").is_empty());
        assert!(net_options("udp://@192.168.1.1:5000", "192.168.1.5").is_empty());
        assert!(net_options(r"D:\a.ts", "192.168.1.5").is_empty());
    }
}
