//! 全局托管状态 —— 播放器/分析两条链路的共享句柄。
//! 线程模型沿用 egui 版：后台线程 → channel → 前端轮询收割。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vqa_core::player::Mpv;
use vqa_core::probe::QuickProbe;
use vqa_core::bridge::RealtimeBridge;
use vqa_core::live::LiveSession;
use vqa_core::tsfile::{PacketFile, ScanResult, TrOverrides};

use crate::mpv_host::VideoChild;

/// 起播管线结果（probe 线程 → 前端轮询收割）。
/// Bridged 携带桥句柄——收割侧存入 state，否则线程结束时桥被 drop 停掉。
pub enum Launch {
    Direct,
    Bridged { url: String, reason: String, auto_scale: u32, bridge: Box<dyn BridgeHandle> },
    Failed(String),
}

pub enum ProbeMsg {
    Outcome(Launch),
    Info(Option<QuickProbe>),
    /// 桥转码开始（长时间无反馈会让用户以为卡死而重复点击——
    /// 每次重试都作废上一次全片转码重来）
    Bridging(String),
}

/// 桥句柄的统一接口——实时桥（RealtimeBridge）与并行分片桥（ParBridge）
/// 对外行为一致，播放器状态只认这个接口。
pub trait BridgeHandle: Send {
    /// 保留接口：桥地址由 state 侧另存，实现里仍然提供（便于诊断打印）
    #[allow(dead_code)]
    fn url(&self) -> String;
    fn mode(&self) -> String;
    /// 缓存/文件桥：EOF 播完是正常结束（豁免"2 分钟无画面"误报）
    fn is_file(&self) -> bool;
    fn alive(&self) -> usize;
    fn stderr_tail(&self) -> String;
    fn progress(&self) -> String;
    /// mpv 已绑上 UDP 端口 → 放开发送（只有分片桥需要，实时桥空实现）
    fn go(&self) {}
    /// 保留接口：现在的停止路径直接 drop 句柄，实现仍提供
    #[allow(dead_code)]
    fn stop(&self);
}

impl BridgeHandle for RealtimeBridge {
    fn url(&self) -> String {
        RealtimeBridge::url(self)
    }
    fn mode(&self) -> String {
        self.mode.clone()
    }
    fn is_file(&self) -> bool {
        RealtimeBridge::is_file(self)
    }
    fn alive(&self) -> usize {
        RealtimeBridge::alive(self)
    }
    fn stderr_tail(&self) -> String {
        RealtimeBridge::stderr_tail(self)
    }
    fn progress(&self) -> String {
        String::new()
    }
    fn stop(&self) {
        RealtimeBridge::stop(self)
    }
}

impl BridgeHandle for vqa_core::bridge_par::ParBridge {
    fn url(&self) -> String {
        vqa_core::bridge_par::ParBridge::url(self)
    }
    fn mode(&self) -> String {
        vqa_core::bridge_par::ParBridge::mode(self)
    }
    fn is_file(&self) -> bool {
        vqa_core::bridge_par::ParBridge::is_file(self)
    }
    fn alive(&self) -> usize {
        vqa_core::bridge_par::ParBridge::alive(self)
    }
    fn stderr_tail(&self) -> String {
        vqa_core::bridge_par::ParBridge::stderr_tail(self)
    }
    fn progress(&self) -> String {
        vqa_core::bridge_par::ParBridge::progress(self)
    }
    fn go(&self) {
        vqa_core::bridge_par::ParBridge::go(self)
    }
    fn stop(&self) {
        vqa_core::bridge_par::ParBridge::stop(self)
    }
}

/// 播放器链路。
pub struct PlayerState {
    pub child: Mutex<Option<VideoChild>>,
    pub mpv: Mutex<Option<Arc<Mpv>>>,
    pub playing_url: Mutex<String>,
    pub bridge: Mutex<Option<Box<dyn BridgeHandle>>>,
    pub bridge_note: Mutex<String>,
    pub err: Mutex<Option<String>>,
    /// 提示行（文本、设置时刻、时效）——大文件全片转码要几分钟，
    /// 提示时效需要比默认 3s 长
    pub hint: Mutex<Option<(String, std::time::Instant, std::time::Duration)>>,
    pub recording: Mutex<bool>,
    pub rec_path: Mutex<Option<PathBuf>>,
    pub probe_rx: Mutex<Option<std::sync::mpsc::Receiver<ProbeMsg>>>,
    pub probe_info: Mutex<Option<QuickProbe>>,
    /// 起播管线进行中（探测/桥转码）——此窗口内 mpv 的 idle 不是"已播完"，
    /// 重播按钮也不应把原始 AVS2 喂给 mpv（官方 mpv 无 AVS2 解码器，秒退）
    pub preparing: Mutex<bool>,
    /// 桥流自动重连：剩余次数 + 上次重试时刻 + 桥流 URL
    /// （chain10 桥从启动到出流要数秒，mpv loadfile 后会先回 idle——
    /// 实测缺陷：不等出流直接放弃。这里做 idle 自动重试直到出流。）
    pub bridge_retry_left: Mutex<u32>,
    pub bridge_last_retry: Mutex<Option<std::time::Instant>>,
    pub bridge_url: Mutex<String>,
    /// 直放 loadfile 的时刻：用于"有音无画"首帧看门狗（8K 直播首开偶尔
    /// 只认出音频轨，2 秒后自动重载一次，避免用户再切高清再切回来）。
    pub loaded_at: Mutex<Option<std::time::Instant>>,
    /// 本次播放是否已经做过一次"无视频轨"重载（只重载一次，防止死循环）
    pub video_retry_done: Mutex<bool>,
    /// 本次播放选择的网卡 IP（8K 自适应转码时桥要按同一出口入组）
    pub last_nic_ip: Mutex<String>,
    /// 直播流最后推进到的播放时间（毫秒），用于识别"画面停在最后一帧、
    /// mpv 又不回 idle"的断流（--keep-open 或 HTTP 半开连接时常见）
    pub live_last_time: Mutex<i64>,
    /// 直播播放时间连续不推进的起始时刻
    pub live_stall_since: Mutex<Option<std::time::Instant>>,
    /// 直播直放意外回 idle 的起始时刻（mpv 断流后子窗只剩黑场；
    /// 界面只看到黑场；静置 3 秒自动重连，最多 3 次）
    pub live_idle_since: Mutex<Option<std::time::Instant>>,
    /// 直播流自动重连剩余次数（每次新起播重置为 3）
    pub live_retry_left: Mutex<u32>,
    /// 本次直播是否已经出过画面（出过画再回 idle 才算"断流"，
    /// 起播阶段还没认出视频轨时交给"有音无画"看门狗）
    pub live_had_picture: Mutex<bool>,
    /// 桥就绪时刻（2.6 性能诊断的计时起点；起播瞬间 mpv 短暂 idle 不算失败）
    pub bridge_since: Mutex<Option<std::time::Instant>>,
    /// 桥流是否已经喂给 mpv（预转模式下要等全片转完才 loadfile——
    /// mpv 打开 udp 时读不到数据会立刻判"无法识别格式"并放弃，之后不再重试）
    pub bridge_loaded: Mutex<bool>,
    /// 桥开始推流的时刻：loadfile 要等数据流稳定一下再进行，否则 mpv 打开的是空流
    pub bridge_ready_at: Mutex<Option<std::time::Instant>>,
    /// 桥是否已出过画面——流式桥播完 EOF 回 idle 是正常结束，
    /// 不能被 2.6 的"2 分钟无画面"误报
    pub bridge_had_picture: Mutex<bool>,
    /// 循环播放（mpv `loop-file=inf`）。跨 loadfile 保持：
    /// 每次起播/重播后要重新下发一次（loadfile 会重置部分属性）
    pub loop_file: Mutex<bool>,
    /// 本次桥选定的 H.264 编码器（h264_nvenc / h264_qsv / h264_amf / h264_mf / libopenh264）。
    /// 在桥启动时记一次，播放器页"桥格式"条据此显示"转到什么编码"。
    pub bridge_enc: Mutex<String>,
    /// 播放采样节流（诊断包每 5s 采一条 avsync/丢帧/码率）
    pub last_sample: Mutex<Option<std::time::Instant>>,
    /// ★ 开播前预热（后台静默转码，把关的缓存先攒出来）：
    ///   文件一拖入/选定就起桥转码，用户点播放时要么直接命中缓存（0.3~1 秒），
    ///   要么接上已经在跑的桥（数据早已写好，mpv 一读就有）。
    ///   存 (规范化后的源 url, 已经在跑的桥句柄)。
    pub prewarm: Mutex<Option<(String, Box<dyn BridgeHandle>)>>,
    /// 正在预热（还没起成桥）的源。用户在这 1~2 秒里点播放时，靠它避免
    /// 又起一条桥、两条链同时写同一个缓存文件。
    pub prewarm_url: Mutex<String>,
    /// 上一轮采样时的丢帧计数，用于"丢帧突增"事件
    pub last_drop: Mutex<i64>,
    /// 8K 自动降级锁：同一源再次起播时继续走已选宽度，避免又回到 8K 直放
    pub auto_scale_url: Mutex<String>,
    pub auto_scale_w: Mutex<u32>,
    /// 桥进程异常退出的起始时刻与自动重启次数（直播桥 watchdog）
    pub bridge_dead_since: Mutex<Option<std::time::Instant>>,
    pub bridge_auto_restarts: Mutex<u32>,
    /// 最近一次桥进程 stderr 尾部：桥被重启替换时旧句柄会被 drop，
    /// 必须先把原因留下来，否则诊断包只剩一句“桥反复退出”，无法定位。
    pub bridge_stderr_last: Mutex<String>,
    /// 已经成功起过的解码器（"default" 或 "libuavs3d"），重启时优先复用，
    /// 避免 8K AVS3 每次都先拿默认解码器试一遍再回退。
    pub auto_decoder: Mutex<String>,
    /// 抽节目模式的 ffmpeg 子进程（mpv 从 fd://0 读它的输出）
    pub feed: Mutex<Option<std::process::Child>>,
}

impl PlayerState {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            mpv: Mutex::new(None),
            playing_url: Mutex::new(String::new()),
            bridge: Mutex::new(None),
            bridge_note: Mutex::new(String::new()),
            err: Mutex::new(None),
            hint: Mutex::new(None),
            recording: Mutex::new(false),
            rec_path: Mutex::new(None),
            probe_rx: Mutex::new(None),
            probe_info: Mutex::new(None),
            preparing: Mutex::new(false),
            bridge_retry_left: Mutex::new(0),
            bridge_last_retry: Mutex::new(None),
            bridge_url: Mutex::new(String::new()),
            loaded_at: Mutex::new(None),
            video_retry_done: Mutex::new(false),
            last_nic_ip: Mutex::new(String::new()),
            live_idle_since: Mutex::new(None),
            live_retry_left: Mutex::new(0),
            live_had_picture: Mutex::new(false),
            live_last_time: Mutex::new(0),
            live_stall_since: Mutex::new(None),
            bridge_since: Mutex::new(None),
            bridge_loaded: Mutex::new(false),
            bridge_ready_at: Mutex::new(None),
            bridge_had_picture: Mutex::new(false),
            loop_file: Mutex::new(false),
            bridge_enc: Mutex::new(String::new()),
            last_sample: Mutex::new(None),
            prewarm: Mutex::new(None),
            prewarm_url: Mutex::new(String::new()),
            last_drop: Mutex::new(0),
            feed: Mutex::new(None),
            auto_scale_url: Mutex::new(String::new()),
            auto_scale_w: Mutex::new(0),
            bridge_dead_since: Mutex::new(None),
            bridge_auto_restarts: Mutex::new(0),
            bridge_stderr_last: Mutex::new(String::new()),
            auto_decoder: Mutex::new(String::new()),
        }
    }

    pub fn set_hint(&self, s: impl Into<String>) {
        *self.hint.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((s.into(), std::time::Instant::now(), std::time::Duration::from_secs(3)));
    }

    /// 自定义时效的提示（转码等待期等长场景）
    pub fn set_hint_for(&self, s: impl Into<String>, secs: u64) {
        *self.hint.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((s.into(), std::time::Instant::now(), std::time::Duration::from_secs(secs)));
    }
}

/// 分析链路。
pub struct AnalysisState {
    /// 0 = 待机, 1 = 文件扫描中, 2 = 实时分析中
    pub mode: Mutex<u8>,
    pub stop_flag: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    pub progress: Mutex<(u64, u64)>,
    pub result_rx: Mutex<Option<std::sync::mpsc::Receiver<Result<ScanResult, String>>>>,
    pub result: Mutex<Option<Arc<ScanResult>>>,
    pub live: Mutex<Option<LiveSession>>,
    pub live_err: Mutex<Option<String>>,
    pub log: Mutex<std::collections::VecDeque<String>>,
    pub pkt: Mutex<Option<PacketFile>>,
    pub overrides: Arc<Mutex<TrOverrides>>,
}

impl AnalysisState {
    pub fn new() -> Self {
        Self {
            mode: Mutex::new(0),
            stop_flag: Mutex::new(None),
            progress: Mutex::new((0, 0)),
            result_rx: Mutex::new(None),
            result: Mutex::new(None),
            live: Mutex::new(None),
            live_err: Mutex::new(None),
            log: Mutex::new(std::collections::VecDeque::new()),
            pkt: Mutex::new(None),
            overrides: Arc::new(Mutex::new(TrOverrides::new())),
        }
    }

    pub fn log_push(&self, s: impl Into<String>) {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        log.push_back(s.into());
        while log.len() > 500 {
            log.pop_front();
        }
    }
}

/// 合规检测链路（与播放器/视频分析互不影响：自己一路收流、自己一份结果）。
pub struct ComplianceState {
    /// 0 = 待机, 2 = 收流中（表 2 的网络项要收流才判）
    pub mode: Mutex<u8>,
    pub live: Mutex<Option<LiveSession>>,
    pub err: Mutex<Option<String>>,
    /// 节目技术质量项（ffprobe 判定结果；收流模式下由后台探测线程填）
    pub items: std::sync::Arc<Mutex<Option<Vec<vqa_core::compliance::Item>>>>,
    /// 后台探测状态：pending / ok / err:原因（页面据此显示"探测中/探测失败"）
    pub probe_state: std::sync::Arc<Mutex<String>>,
    /// 上一次检测的来源与类型
    pub source: Mutex<(String, String)>,
}

impl ComplianceState {
    pub fn new() -> Self {
        Self {
            mode: Mutex::new(0),
            live: Mutex::new(None),
            err: Mutex::new(None),
            items: std::sync::Arc::new(Mutex::new(None)),
            probe_state: std::sync::Arc::new(Mutex::new(String::new())),
            source: Mutex::new((String::new(), "IPTV".into())),
        }
    }
}

impl Default for ComplianceState {
    fn default() -> Self {
        Self::new()
    }
}

/// 直播转码页的会话（同一时刻只跑一路）
pub struct XcodeState {
    pub sess: Mutex<Option<vqa_core::xcode::XcodeSession>>,
    pub err: Mutex<Option<String>>,
}

impl XcodeState {
    pub fn new() -> Self {
        Self { sess: Mutex::new(None), err: Mutex::new(None) }
    }
}

impl Default for XcodeState {
    fn default() -> Self { Self::new() }
}

/// 微突发检测页的会话
pub struct BurstState {
    pub sess: Mutex<Option<vqa_core::burst::BurstSession>>,
    pub err: Mutex<Option<String>>,
}

impl BurstState {
    pub fn new() -> Self {
        Self { sess: Mutex::new(None), err: Mutex::new(None) }
    }
}

impl Default for BurstState {
    fn default() -> Self { Self::new() }
}
// ═════════ 音画同步 / 一致性比对（一次性分析任务，进度 + 结果托管） ═════════

/// 一路分析任务。前端按页分配任务 id 并复用，支持同一页多路并行。
pub struct AvJob {
    pub pct: std::sync::Arc<std::sync::atomic::AtomicU8>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub rx: Mutex<Option<std::sync::mpsc::Receiver<vqa_core::avsync::AvSyncResult>>>,
    pub last: Mutex<Option<vqa_core::avsync::AvSyncResult>>,
    pub err: std::sync::Arc<Mutex<Option<String>>>,
}

impl AvJob {
    fn new() -> Self {
        Self {
            pct: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rx: Mutex::new(None),
            last: Mutex::new(None),
            err: std::sync::Arc::new(Mutex::new(None)),
        }
    }
}

pub struct AvState {
    pub jobs: Mutex<std::collections::HashMap<String, std::sync::Arc<AvJob>>>,
    /// 开始任务时串行化「查占用 + 置 running」，避免并发点击冲破 MAX_PARALLEL
    pub gate: Mutex<()>,
}

impl AvState {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(std::collections::HashMap::new()),
            gate: Mutex::new(()),
        }
    }

    /// 取任务槽（没有就建一个）。任务 id 由前端复用，槽位数量有限。
    pub fn job(&self, id: &str) -> std::sync::Arc<AvJob> {
        let mut g = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(id.to_string())
            .or_insert_with(|| std::sync::Arc::new(AvJob::new()))
            .clone()
    }

    pub fn running_count(&self) -> usize {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| j.running.load(std::sync::atomic::Ordering::Relaxed))
            .count()
    }
}

impl Default for AvState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ConsistJob {
    pub pct: std::sync::Arc<std::sync::atomic::AtomicU8>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub rx: Mutex<Option<std::sync::mpsc::Receiver<vqa_core::consist::ConsistResult>>>,
    pub last: Mutex<Option<vqa_core::consist::ConsistResult>>,
    pub err: std::sync::Arc<Mutex<Option<String>>>,
}

impl ConsistJob {
    fn new() -> Self {
        Self {
            pct: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rx: Mutex::new(None),
            last: Mutex::new(None),
            err: std::sync::Arc::new(Mutex::new(None)),
        }
    }
}

pub struct ConsistState {
    pub jobs: Mutex<std::collections::HashMap<String, std::sync::Arc<ConsistJob>>>,
    /// 同 AvState：起任务时串行化占用检查
    pub gate: Mutex<()>,
}

impl ConsistState {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(std::collections::HashMap::new()),
            gate: Mutex::new(()),
        }
    }

    pub fn job(&self, id: &str) -> std::sync::Arc<ConsistJob> {
        let mut g = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(id.to_string())
            .or_insert_with(|| std::sync::Arc::new(ConsistJob::new()))
            .clone()
    }

    pub fn running_count(&self) -> usize {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| j.running.load(std::sync::atomic::Ordering::Relaxed))
            .count()
    }
}

impl Default for ConsistState {
    fn default() -> Self {
        Self::new()
    }
}