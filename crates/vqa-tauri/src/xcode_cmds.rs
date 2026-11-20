//! 直播转码页命令 —— 起一路 ffmpeg 常态化转码，页面轮询统计。

use tauri::State;
use vqa_core::xcode::{XcodeCfg, XcodeStats};

use crate::state::XcodeState;

/// 可用视频编码器（查一次 ffmpeg -encoders，页面下拉用）。
#[tauri::command]
pub async fn xcode_encoders() -> Result<serde_json::Value, String> {
    let list = tauri::async_runtime::spawn_blocking(|| {
        let mut v: Vec<(String, String)> = Vec::new();
        if let Some(ff) = vqa_core::tools::find_ffmpeg() {
            let mut cmd = vqa_core::ffmpeg::hidden(&ff);
            cmd.args(["-hide_banner", "-encoders"]);
            if let Ok(out) = cmd.output() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    // 形如: " V..... libx264   H.264 ..."
                    let t = line.trim_start();
                    if !t.starts_with('V') {
                        continue;
                    }
                    let name = t
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if !(name.contains("264") || name.contains("265") || name.contains("hevc")) {
                        continue;
                    }
                    let label = match name.as_str() {
                        "libx264" => "libx264（软编 H.264）".to_string(),
                        "libopenh264" => "libopenh264（软编 H.264）".to_string(),
                        "h264_nvenc" => "h264_nvenc（NVIDIA 硬编）".to_string(),
                        "hevc_nvenc" => "hevc_nvenc（NVIDIA 硬编 H.265）".to_string(),
                        "h264_qsv" => "h264_qsv（Intel 硬编）".to_string(),
                        "h264_amf" => "h264_amf（AMD 硬编）".to_string(),
                        "h264_mf" => "h264_mf（Media Foundation 硬编）".to_string(),
                        "libx265" => "libx265（软编 H.265）".to_string(),
                        _ => name.clone(),
                    };
                    v.push((name, label));
                }
            }
        }
        v
    })
    .await
    .map_err(|e| format!("线程异常：{e}"))?;
    Ok(serde_json::json!({
        "auto_label": "自动（优先硬编，无则 libopenh264）",
        "encoders": list.into_iter().map(|(id, label)| serde_json::json!({"id": id, "label": label})).collect::<Vec<_>>(),
    }))
}

/// 按当前页面配置生成实际会执行的 ffmpeg 命令（不启动进程）。
#[tauri::command]
pub async fn xcode_preview(cfg: XcodeCfg) -> Result<serde_json::Value, String> {
    let (cmd, encoder) =
        tauri::async_runtime::spawn_blocking(move || vqa_core::xcode::preview(&cfg))
            .await
            .map_err(|e| format!("线程异常：{e}"))?
            .map_err(|e| format!("{e}"))?;
    Ok(serde_json::json!({ "cmd": cmd, "encoder": encoder }))
}

#[tauri::command]
pub async fn xcode_start(st: State<'_, XcodeState>, cfg: XcodeCfg) -> Result<XcodeStats, String> {
    if cfg.input.trim().is_empty() {
        return Err("来源为空".into());
    }
    if cfg.output.trim().is_empty() {
        return Err("输出为空".into());
    }
    {
        let g = st.sess.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().map(|s| s.alive()).unwrap_or(false) {
            return Err("已有一路转码在跑，请先停止".into());
        }
    }
    let sess =
        tauri::async_runtime::spawn_blocking(move || vqa_core::xcode::XcodeSession::start(&cfg))
            .await
            .map_err(|e| format!("线程异常：{e}"))?
            .map_err(|e| format!("{e}"))?;
    let snap = sess.snapshot();
    *st.sess.lock().unwrap_or_else(|e| e.into_inner()) = Some(sess);
    *st.err.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(snap)
}

#[tauri::command]
pub fn xcode_snapshot(st: State<'_, XcodeState>) -> XcodeStats {
    match st.sess.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(s) => s.snapshot(),
        None => XcodeStats::default(),
    }
}

#[tauri::command]
pub fn xcode_stop(st: State<'_, XcodeState>) -> XcodeStats {
    if let Some(s) = st.sess.lock().unwrap_or_else(|e| e.into_inner()).take() {
        s.stop();
        let mut snap = s.snapshot();
        snap.running = false;
        return snap;
    }
    XcodeStats::default()
}
