//! 微突发巡检页命令 —— 收一路组播，按 1ms 桶统计突发。

use tauri::State;
use vqa_core::burst::{BurstCfg, BurstStats};

use crate::state::BurstState;

#[tauri::command]
pub async fn burst_start(
    st: State<'_, BurstState>,
    url: String,
    nic_ip: String,
    ratio: f64,
) -> Result<BurstStats, String> {
    if url.trim().is_empty() {
        return Err("组播/单播地址为空".into());
    }
    {
        let g = st.sess.lock().unwrap();
        if g.is_some() {
            return Err("巡检已在运行，请先停止".into());
        }
    }
    let cfg = BurstCfg {
        url: url.clone(),
        nic_ip,
        ratio,
    };
    let sess = tauri::async_runtime::spawn_blocking(move || vqa_core::burst::BurstSession::start(&cfg))
        .await
        .map_err(|e| format!("线程异常：{e}"))?
        .map_err(|e| format!("{e}"))?;
    let snap = sess.snapshot();
    *st.sess.lock().unwrap() = Some(sess);
    *st.err.lock().unwrap() = None;
    Ok(snap)
}

#[tauri::command]
pub fn burst_snapshot(st: State<'_, BurstState>) -> BurstStats {
    match st.sess.lock().unwrap().as_ref() {
        Some(s) => s.snapshot(),
        None => BurstStats::default(),
    }
}

#[tauri::command]
pub fn burst_stop(st: State<'_, BurstState>) -> BurstStats {
    if let Some(s) = st.sess.lock().unwrap().take() {
        s.stop();
        let mut snap = s.snapshot();
        snap.running = false;
        return snap;
    }
    BurstStats::default()
}
