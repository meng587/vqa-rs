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
    /// mpv 启动参数（直放与抽流喂管道两种模式共用）
    fn base_args(pipe: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            format!("--input-ipc-server={pipe}"),
            "--idle=yes".into(),
            "--keep-open=no".into(),
            "--osc=no".into(),
            "--input-default-bindings=no".into(),
            "--input-vo-keyboard=no".into(),
            "--hwdec=auto-safe".into(),
            "--network-timeout=15".into(),
            // ★ 回环转码流的缓冲：桥输出速率有抖动（软解 4K 转硬编），
            //   大 demux 缓冲吸收周期性落后，避免每几秒卡一下
            "--demuxer-max-bytes=384MiB".into(),
            "--demuxer-readahead-secs=30".into(),
            // ★ 软解跟不上流帧率时（如 4K 50fps AVS2 转码流，davs2 实测
            //   ~41fps）丢帧跟上，避免周期性停顿——质量检测以看内容为主。
            //   8K 长播实测：只丢解码帧时，视频 VO 队列仍会被慢解码拖空，
            //   表现为画面越来越滞后、最后黑场；带上 vo 丢帧后，晚到的帧
            //   在渲染端直接丢弃，音画始终跟住音频时钟。
            "--framedrop=decoder+vo".into(),
            // ★ 显式以音频为主时钟（mpv 默认 audio，但用户/旧配置可能带
            //   display-resample）。质检场景优先保证声音连续，画面追音频。
            "--video-sync=audio".into(),
            "--terminal=no".into(),
        ];
        // ★ 自带日志落 scratch（诊断包会取末尾 50 行）——底层解码/渲染错误
        //   不进 stderr，只有 mpv 自己的 log 里才有
        {
            let logf = crate::config::scratch_dir().join("mpv.log");
            if let Some(p) = logf.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            // 每次起 mpv 重开一份：只留本次运行的日志，不会越积越大
            let _ = std::fs::remove_file(&logf);
            args.push(format!("--log-file={}", logf.display()));
            args.push("--msg-level=all=info".into());
        }
        args
    }

    /// 每次起进程用独立的管道名。切节目/重拉播放器时旧 mpv 还没退干净，
    /// 同名管道会让新客户端连到旧实例上（发过去的命令没人应答，界面卡住）。
    fn pipe_name() -> String {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        format!(
            r"\\.\pipe\vqa-mpv-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn connect(pipe: &str) -> Result<Arc<Mpv>, String> {        let mut conn = None;
        for _ in 0..120 {
            match miow::pipe::connect(pipe) {
                Ok(f) => {
                    conn = Some(Conn { f, pend: Vec::new() });
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        let conn = conn.ok_or("连不上 mpv IPC 管道（12s 超时）")?;
        Ok(Arc::new(Mpv {
            child: Mutex::new(None),
            conn: Mutex::new(Some(conn)),
            next_id: AtomicU64::new(1),
            props: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(VecDeque::new())),
            dead: Arc::new(AtomicBool::new(false)),
        }))
    }

    /// 起 mpv 进程并接上 IPC。`wid` 给了就把视频嵌进该 HWND。
    pub fn launch(mpv: &Path, wid: Option<isize>) -> Result<Arc<Mpv>, String> {
        let pipe = Self::pipe_name();
        let mut args = Self::base_args(&pipe);
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

        let mut this = Self::connect(&pipe)?;
        // Arc 已在别处克隆过才用 Mutex；这里尚未共享，直接取出来塞
        Arc::get_mut(&mut this).unwrap().child = Mutex::new(Some(child));

        // 观察全部界面要用的属性
        for &id in OBSERVED {
            let _ = this.command(json!(["observe_property", id, obs_name(id)]));
        }
        Ok(this)
    }

    /// 起 mpv 从 `fd://0` 读流，stdin 接 `ff`（ffmpeg 抽节目/转封装）的输出。
    /// 返回 (mpv, ffmpeg 子进程)——ffmpeg 要由调用方保存并在停止时结束。
    pub fn launch_feed(
        mpv: &Path,
        wid: Option<isize>,
        ff: &Path,
        ff_args: &[String],
    ) -> Result<(Arc<Mpv>, Child), String> {
        use std::process::Stdio;
        let mut ffcmd = Command::new(ff);
        ffcmd.args(ff_args);
        ffcmd.stdout(Stdio::piped());
        ffcmd.stderr(Stdio::null());
        ffcmd.stdin(Stdio::null());
        #[cfg(windows)]
        ffcmd.creation_flags(crate::ffmpeg::CREATE_NO_WINDOW);
        let mut ff_child = ffcmd
            .spawn()
            .map_err(|e| format!("起 ffmpeg 抽节目失败：{e}"))?;
        let ff_out = ff_child
            .stdout
            .take()
            .ok_or_else(|| "拿不到 ffmpeg 输出管道".to_string())?;

        let pipe = Self::pipe_name();
        let mut args = Self::base_args(&pipe);
        if let Some(w) = wid {
            args.push(format!("--wid={w}"));
        }
        args.push("fd://0".into());

        let mut cmd = Command::new(mpv);
        cmd.args(&args);
        cmd.stdin(Stdio::from(ff_out));
        #[cfg(windows)]
        cmd.creation_flags(crate::ffmpeg::CREATE_NO_WINDOW);
        let child = cmd.spawn().map_err(|e| format!("起 mpv（抽流模式）失败：{e}"))?;

        let mut this = Self::connect(&pipe)?;
        Arc::get_mut(&mut this).unwrap().child = Mutex::new(Some(child));

        for &id in OBSERVED {
            let _ = this.command(json!(["observe_property", id, obs_name(id)]));
        }
        Ok((this, ff_child))
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
    /// 只发不等：给会阻塞很久的命令用（sync 模式下响应回来的 request_id
    /// 与后续命令不匹配，command() 会把它当"迟到的旧响应"丢掉，安全）。
    fn command_async(&self, cmd: Value) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.conn.lock().unwrap();
        let conn = guard
            .as_mut()
            .ok_or_else(|| "IPC 连接未建立".to_string())?;
        let msg = json!({ "command": cmd, "request_id": id, "async": true });
        if let Err(e) = conn.f.write_all(format!("{}\n", msg).as_bytes()) {
            self.dead.store(true, Ordering::Relaxed);
            return Err(format!("IPC 写失败：{e}"));
        }
        Ok(())
    }
    pub fn loadfile(&self, url: &str) -> Result<(), String> {
        self.loadfile_with_opts(url, None)
    }

    /// `lavf_opts` 为 Some 时覆盖默认的 TS 探测窗口（8K 直播首开用更大的
    /// probesize，避免只认出音频轨就开播，出现"有声音没画面"）。
    pub fn loadfile_with_opts(&self, url: &str, lavf_opts: Option<&str>) -> Result<(), String> {
        // ★ 必须异步：mpv 的同步 loadfile 会一直阻塞到 udp 探测完成
        //   （实测 6 秒），而这条命令是在 player_snapshot 里发的——同步就
        //   等于把整个播放诊断循环（含桥重连判断）卡死 6 秒。
        //
        // ★★ UDP 桥流还要压 lavf 的分析窗口：mpv 默认
        //   demuxer-lavf-analyzeduration=5 秒、probesize=5MB，也就是"先攒
        //   5 秒流再开播"。桥是按 1.05×实时喂的，这 5 秒纯属干等——实测
        //   `Opening udp` 11.967s → `on_preloaded` 17.266s（5.3 秒），
        //   首帧因此晚了整整 5 秒。TS 结构固定、而且我们从关键帧开始发，
        //   0.6 秒 / 2MB 足够认出来。
        //   8K 源码率高，调用方会显式传更大的窗口（见 loadfile_with_opts）。
        let opts = if let Some(o) = lavf_opts {
            Some(o.to_string())
        } else if url.starts_with("udp://") || url.starts_with("tcp://") {
            Some("demuxer-lavf-analyzeduration=0.6,demuxer-lavf-probesize=2097152".into())
        } else {
            None
        };
        match opts {
            Some(o) => self.command_async(json!(["loadfile", url, "replace", -1, o])),
            None => self.command_async(json!(["loadfile", url, "replace"])),
        }
    }
    pub fn stop(&self) -> Result<(), String> {
        self.command(json!(["stop"])).map(|_| ())
    }
    pub fn set_pause(&self, on: bool) -> Result<(), String> {
        self.set_prop("pause", json!(on))
    }
    /// 直接向 mpv 查询布尔属性。属性变化事件偶尔会滞后或被主轮询吞掉，
    /// 播放/暂停这种需要“按真实状态切换”的操作不能只依赖快照缓存。
    pub fn get_bool(&self, name: &str) -> Result<bool, String> {
        let r = self.command(json!(["get_property", name]))?;
        let v = r
            .get("data")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("属性 {name} 不是布尔值"))?;
        // 把直接查询到的真实值回写观察缓存，轮询里的 snapshot() 才能
        // 立刻跟上（否则 pause 后 UI 仍可能显示“暂停”）。
        let id = match name {
            "pause" => Some(obs::PAUSE),
            "idle-active" => Some(obs::IDLE_ACTIVE),
            "seekable" => Some(obs::SEEKABLE),
            "mute" => Some(obs::MUTE),
            _ => None,
        };
        if let Some(id) = id {
            self.props.lock().unwrap().insert(id, Value::Bool(v));
        }
        Ok(v)
    }
    /// 原子切换暂停：mpv 自己按当前真实状态取反，避免前端拿到旧快照后
    /// 反复 set_pause(true) 导致“暂停后按播放仍是暂停”。
    pub fn cycle_pause(&self) -> Result<(), String> {
        self.command(json!(["cycle", "pause"])).map(|_| ())
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
