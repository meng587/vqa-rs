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
    Bridged { url: String, reason: String, bridge: Box<dyn BridgeHandle> },
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
    fn url(&self) -> String;
    fn mode(&self) -> String;
    /// 缓存/文件桥：EOF 播完是正常结束（豁免"2 分钟无画面"误报）
    fn is_file(&self) -> bool;
    fn alive(&self) -> usize;
    fn stderr_tail(&self) -> String;
    fn progress(&self) -> String;
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
    /// 桥就绪时刻（2.6 性能诊断的计时起点；起播瞬间 mpv 短暂 idle 不算失败）
    pub bridge_since: Mutex<Option<std::time::Instant>>,
    /// 桥是否已出过画面——流式桥播完 EOF 回 idle 是正常结束，
    /// 不能被 2.6 的"2 分钟无画面"误报
    pub bridge_had_picture: Mutex<bool>,
    /// 循环播放（mpv `loop-file=inf`）。跨 loadfile 保持：
    /// 每次起播/重播后要重新下发一次（loadfile 会重置部分属性）
    pub loop_file: Mutex<bool>,
    /// 本次桥选定的 H.264 编码器（h264_nvenc / h264_qsv / h264_amf / libx264）。
    /// 在桥启动时记一次，播放器页"桥格式"条据此显示"转到什么编码"。
    pub bridge_enc: Mutex<String>,
    /// 播放采样节流（诊断包每 5s 采一条 avsync/丢帧/码率）
    pub last_sample: Mutex<Option<std::time::Instant>>,
    /// 上一轮采样时的丢帧计数，用于"丢帧突增"事件
    pub last_drop: Mutex<i64>,
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
            bridge_since: Mutex::new(None),
            bridge_had_picture: Mutex::new(false),
            loop_file: Mutex::new(false),
            bridge_enc: Mutex::new(String::new()),
            last_sample: Mutex::new(None),
            last_drop: Mutex::new(0),
        }
    }

    pub fn set_hint(&self, s: impl Into<String>) {
        *self.hint.lock().unwrap() =
            Some((s.into(), std::time::Instant::now(), std::time::Duration::from_secs(3)));
    }

    /// 自定义时效的提示（转码等待期等长场景）
    pub fn set_hint_for(&self, s: impl Into<String>, secs: u64) {
        *self.hint.lock().unwrap() =
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
        let mut log = self.log.lock().unwrap();
        log.push_back(s.into());
        while log.len() > 500 {
            log.pop_front();
        }
    }
}
