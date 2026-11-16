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
    Bridged { url: String, reason: String, bridge: RealtimeBridge },
    Failed(String),
}

pub enum ProbeMsg {
    Outcome(Launch),
    Info(Option<QuickProbe>),
    /// 桥转码开始（长时间无反馈会让用户以为卡死而重复点击——
    /// 每次重试都作废上一次全片转码重来）
    Bridging(String),
}

/// 播放器链路。
pub struct PlayerState {
    pub child: Mutex<Option<VideoChild>>,
    pub mpv: Mutex<Option<Arc<Mpv>>>,
    pub playing_url: Mutex<String>,
    pub bridge: Mutex<Option<RealtimeBridge>>,
    pub bridge_note: Mutex<String>,
    pub err: Mutex<Option<String>>,
    /// 提示行（文本、设置时刻、时效）——大文件全片转码要几分钟，
    /// 提示时效需要比默认 3s 长
    pub hint: Mutex<Option<(String, std::time::Instant, std::time::Duration)>>,
    pub recording: Mutex<bool>,
    pub rec_path: Mutex<Option<PathBuf>>,
    pub probe_rx: Mutex<Option<std::sync::mpsc::Receiver<ProbeMsg>>>,
    pub probe_info: Mutex<Option<QuickProbe>>,
    /// 桥流自动重连：剩余次数 + 上次重试时刻 + 桥流 URL
    /// （chain10 桥从启动到出流要数秒，mpv loadfile 后会先回 idle——
    /// 实测缺陷：不等出流直接放弃。这里做 idle 自动重试直到出流。）
    pub bridge_retry_left: Mutex<u32>,
    pub bridge_last_retry: Mutex<Option<std::time::Instant>>,
    pub bridge_url: Mutex<String>,
    /// 桥就绪时刻（2.6 性能诊断的计时起点；起播瞬间 mpv 短暂 idle 不算失败）
    pub bridge_since: Mutex<Option<std::time::Instant>>,
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
            bridge_retry_left: Mutex::new(0),
            bridge_last_retry: Mutex::new(None),
            bridge_url: Mutex::new(String::new()),
            bridge_since: Mutex::new(None),
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
