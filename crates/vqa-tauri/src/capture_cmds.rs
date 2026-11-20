//! 抓包分析页的 Tauri 命令。
//!
//! 抓包核心在 `vqa-core::capture`：只绑定一路 UDP/RTP 业务流，
//! 写入 pcap 和 TS 样本，不依赖 Npcap/Wireshark DLL。

use std::sync::Mutex;

use tauri::State;

use vqa_core::capture::{CaptureSession, CaptureSnapshot};

pub struct CaptureState {
    pub session: Mutex<Option<CaptureSession>>,
}

impl CaptureState {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }
}

#[tauri::command]
pub fn capture_start(
    st: State<'_, CaptureState>,
    url: String,
    nic_ip: String,
) -> Result<CaptureSnapshot, String> {
    let clean = url.split('?').next().unwrap_or(&url).trim().to_string();
    if clean.is_empty() {
        return Err("请先填写 UDP/RTP 抓包地址".into());
    }
    let nic = nic_ip.trim().to_string();
    let out_dir = vqa_core::config::base_dir().join("captures");

    let mut guard = st.session.lock().map_err(|e| e.to_string())?;
    if guard.as_ref().is_some_and(|s| s.is_running()) {
        return Err("已有抓包任务在运行，请先停止".into());
    }
    let session =
        CaptureSession::start(&clean, &nic, &out_dir).map_err(|e| format!("启动抓包失败：{e}"))?;
    let snap = session.snapshot();
    *guard = Some(session);
    Ok(snap)
}

#[tauri::command]
pub fn capture_stop(st: State<'_, CaptureState>) -> Result<Option<CaptureSnapshot>, String> {
    let mut guard = st.session.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = guard.take() {
        session.stop();
        let snap = session.snapshot();
        *guard = Some(session);
        Ok(Some(snap))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub fn capture_status(st: State<'_, CaptureState>) -> Option<CaptureSnapshot> {
    st.session
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.snapshot()))
}
