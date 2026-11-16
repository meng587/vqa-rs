//! Tauri 命令层 —— 全部薄封装 vqa-core，无业务逻辑。
//!
//! 状态流：前端轮询命令（播放器 100ms / 扫描进度 200ms / 实时 1s），
//! 与 egui 版"每帧 poll"模型一一对应。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::State;

use crate::state::{AnalysisState, Launch, PlayerState, ProbeMsg};
use vqa_core::probe;

// ═══════════════════════════ 通用 ═══════════════════════════

#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

#[tauri::command]
pub fn list_nics() -> Vec<String> {
    vqa_core::netif::choices()
}

#[tauri::command]
pub fn source_history(key: String) -> Vec<String> {
    vqa_core::config::shared_get_or(format!("recent_sources.{key}").as_str(), Vec::<String>::new())
}

#[tauri::command]
pub fn push_source_history(key: String, url: String) {
    if url.is_empty() {
        return;
    }
    let k = format!("recent_sources.{key}");
    let mut h: Vec<String> =
        vqa_core::config::shared_get_or(k.as_str(), Vec::new());
    h.retain(|x| x != &url);
    h.insert(0, url);
    h.truncate(5);
    let _ = vqa_core::config::shared_set(k.as_str(), serde_json::json!(h));
}

#[tauri::command]
pub fn browse_media_file(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .add_filter("媒体文件", &["ts", "mpg", "mpeg", "mp4", "mkv", "avi", "flv", "m2t", "mts"])
        .blocking_pick_file()
        .map(|p| p.to_string())
}

#[tauri::command]
pub fn tool_paths() -> serde_json::Value {
    serde_json::json!({
        "ffmpeg": vqa_core::tools::find_ffmpeg().map(|p| p.to_string_lossy().to_string()),
        "ffprobe": vqa_core::tools::find_ffprobe().map(|p| p.to_string_lossy().to_string()),
        "mpv": vqa_core::tools::find_mpv().map(|p| p.to_string_lossy().to_string()),
        "davs2": vqa_core::tools::find_davs2().map(|p| p.to_string_lossy().to_string()),
    })
}

#[tauri::command]
pub fn thresholds_get(st: State<AnalysisState>) -> serde_json::Value {
    let ov = st.overrides.lock().unwrap();
    serde_json::to_value(&ov.map).unwrap_or(serde_json::json!({}))
}

// ═══════════════════════════ 诊断日志 ═══════════════════════════

/// 组装诊断日志（MAC+时间命名；环境+状态+应用日志尾部+崩溃记录）。
#[tauri::command]
pub fn diag_collect(pst: State<PlayerState>, ast: State<AnalysisState>) -> serde_json::Value {
    let player = player_snapshot(pst);
    let mode = *ast.mode.lock().unwrap();
    let err = ast.live_err.lock().unwrap().clone();
    let (name, content) = crate::diag::collect(&player, mode, err);
    serde_json::json!({ "name": name, "content": content })
}

/// 导出到本地（保存对话框）。
#[tauri::command]
pub fn diag_export(
    app: tauri::AppHandle,
    name: String,
    content: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let dest = app
        .dialog()
        .file()
        .add_filter("诊断日志", &["txt"])
        .set_file_name(&name)
        .blocking_save_file()
        .map(|p| p.to_string())
        .ok_or_else(|| "已取消".to_string())?;
    std::fs::write(&dest, content).map_err(|e| format!("写文件失败：{e}"))?;
    Ok(Some(dest))
}

/// 上传到 Gitee Issue（token 首次由前端传入并保存到本地 shared config）。
#[tauri::command]
pub fn diag_upload(token: Option<String>, name: String, content: String) -> Result<String, String> {
    let tok = match token {
        Some(t) if !t.trim().is_empty() => {
            let _ = vqa_core::config::shared_set("diag_token", serde_json::json!(t.trim()));
            t.trim().to_string()
        }
        _ => {
            let saved: String =
                vqa_core::config::shared_get_or("diag_token", String::new());
            if saved.is_empty() {
                return Err("尚未配置 Gitee 私人令牌（粘贴一次后会记住）".into());
            }
            saved
        }
    };
    crate::diag::upload_issue(&tok, &name, &content)
}

/// 已保存的 token 是否存在（前端决定是否显示输入框）。
#[tauri::command]
pub fn diag_has_token() -> bool {
    let saved: String = vqa_core::config::shared_get_or("diag_token", String::new());
    !saved.is_empty()
}

#[tauri::command]
pub fn thresholds_set(st: State<AnalysisState>, map: std::collections::HashMap<String, f64>) {
    let mut ov = st.overrides.lock().unwrap();
    *ov = vqa_core::tsfile::TrOverrides { map };
}

// ═══════════════════════════ 播放器 ═══════════════════════════

/// 视频子窗对齐（前端 getBoundingClientRect × devicePixelRatio 的物理像素）。
#[tauri::command]
pub fn place_video(
    st: State<PlayerState>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    visible: bool,
) {
    if let Some(c) = st.child.lock().unwrap().as_mut() {
        c.place(x, y, w, h, visible);
    }
}

#[tauri::command]
pub fn player_play(
    st: State<'_, PlayerState>,
    url: String,
    nic_ip: String,
    force_bridge: bool,
) -> Result<(), String> {
    let cleaned = crate::clean_stream_url(&url);
    if cleaned.is_empty() {
        return Err("来源为空".into());
    }
    // 停旧桥、清错误
    *st.bridge.lock().unwrap() = None;
    *st.bridge_note.lock().unwrap() = String::new();
    *st.bridge_url.lock().unwrap() = String::new();
    *st.bridge_since.lock().unwrap() = None;
    *st.err.lock().unwrap() = None;
    *st.playing_url.lock().unwrap() = cleaned.clone();

    let (tx, rx) = std::sync::mpsc::channel();
    *st.probe_rx.lock().unwrap() = Some(rx);

    let mpv_exe = vqa_core::tools::find_mpv().ok_or("未找到 mpv.exe（bin/mpv/mpv.exe）")?;
    let wid = st
        .child
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| c.hwnd());

    // 起播放器（懒启动； wid 在窗口创建时就绪）
    let mpv = {
        let mut guard = st.mpv.lock().unwrap();
        if guard.is_none() {
            *guard = Some(vqa_core::player::Mpv::launch(&mpv_exe, wid)?);
        }
        guard.as_ref().unwrap().clone()
    };

    let probe_url = cleaned.clone();
    std::thread::Builder::new()
        .name("vqa-probe".into())
        .spawn(move || {
            // 探测（probe_quick 内部带缓存，重复调用近乎零成本）
            let q = probe::probe_quick(
                &probe_url,
                if vqa_core::bridge::is_local(&probe_url) { 5.0 } else { 6.0 },
                &nic_ip,
            );
            let _ = tx.send(ProbeMsg::Info(q.clone()));
            let outcome = decide_bridge(&q, force_bridge, &probe_url, &nic_ip, &tx);
            let _ = tx.send(ProbeMsg::Outcome(outcome));
        })
        .ok();

    // 加载时机对齐 egui 版口径：探针/桥决策完成后由收割侧 loadfile——
    // 10-bit AVS2 等直放必败的源不在桥起好前喂给 mpv
    st.set_hint("探测中…");
    Ok(())
}

/// 桥接决策 + 起桥（照搬 egui 版 play 管线的决策段）。
fn decide_bridge(
    q: &Option<probe::QuickProbe>,
    force: bool,
    url: &str,
    nic_ip: &str,
    bridge_wait_tx: &std::sync::mpsc::Sender<ProbeMsg>,
) -> Launch {
    let need = vqa_core::bridge::needs_bridge(q, force);
    vqa_core::config::log_line(
        "bridge-decide",
        &format!(
            "force={force} need={need} probe={:?}",
            q.as_ref().map(|x| format!(
                "codec={} {}x{} pix={}",
                x.video.codec, x.video.width, x.video.height, x.video.pix_fmt
            ))
        ),
    );
    if !need {
        return Launch::Direct;
    }
    vqa_core::config::log_line("bridge-decide", "起 10-bit chain10 桥…");
    // ★ 立刻告知前端"转码中"——全片转码大文件要 1-3 分钟，无反馈会被
    //   误判为卡死而重复点击（每次重试都作废上一次转码重来）
    let _ = bridge_wait_tx.send(ProbeMsg::Bridging(
        "AVS 源转码中…全片处理约需 1-3 分钟（取决于文件大小），请勿重复点击".into(),
    ));
    let Some(ffmpeg) = vqa_core::tools::find_ffmpeg() else {
        return Launch::Failed("需要转码桥但未找到 ffmpeg".into());
    };
    // ★ chain10 判定：除 10-bit 外，**编码是 AVS2 就走 chain10**——
    //   ffprobe 内置 davs2 对部分 10-bit AVS2 解码报错后会把 pix_fmt
    //   误报为 yuv420p（8-bit，实测 test_avs2_4k.ts），普通桥会改用
    //   内置 davs2 慢速解码（~8.8fps，供给远低于需求 = 永久卡顿）。
    //   vkit_davs2 对 8/10-bit 都正确且文件直读 43fps。
    let chain10 = q
        .as_ref()
        .map(|q| q.video.codec.contains("avs2") || q.video.pix_fmt.contains("10"))
        .unwrap_or(false);
    let mut davs2_path: Option<PathBuf> = None;
    if chain10 {
        match vqa_core::tools::find_davs2() {
            Some(p) => davs2_path = Some(p),
            None => {
                return Launch::Failed(
                    "10-bit AVS2 需要 vkit_davs2.exe（bin/davs2-10bit/）".into(),
                )
            }
        }
    }
    let davs2 = davs2_path.unwrap_or_else(|| PathBuf::new());
    match vqa_core::bridge::RealtimeBridge::start(&ffmpeg, &davs2, url, chain10, 26, nic_ip) {
        Ok(b) => {
            let reason = format!("AVS 源自动转码桥（{}）", b.mode);
            // ★ chain10 本地文件模式：b.url() 返回全片转码完成后的 TS 文件路径；
            //   直播模式返回 tcp 流地址。统一由 bridge 自己决定播放目标。
            let url = b.url();
            Launch::Bridged { url, reason, bridge: b }
        }
        Err(e) => Launch::Failed(format!("转码桥失败：{e}")),
    }
}

#[tauri::command]
pub fn player_stop(st: State<PlayerState>) {
    *st.playing_url.lock().unwrap() = String::new();
    *st.bridge.lock().unwrap() = None;
    *st.bridge_note.lock().unwrap() = String::new();
    *st.bridge_url.lock().unwrap() = String::new();
    *st.bridge_since.lock().unwrap() = None;
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        m.stop();
    }
}

#[tauri::command]
pub fn player_pause_toggle(st: State<PlayerState>) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let snap = m.snapshot();
        if snap.idle {
            // 重播：桥模式用桥产物（TS 文件/流地址），直放用原地址
            let bridge_u = st.bridge_url.lock().unwrap().clone();
            let url = if bridge_u.is_empty() {
                st.playing_url.lock().unwrap().clone()
            } else {
                bridge_u
            };
            if !url.is_empty() {
                m.loadfile(&url);
            }
        } else {
            m.set_pause(!snap.paused);
        }
    }
}

#[tauri::command]
pub fn player_seek(st: State<PlayerState>, fraction: f64) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        m.seek_percent(fraction * 100.0);
    }
}

#[tauri::command]
pub fn player_set_volume(st: State<PlayerState>, volume: f64) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        m.set_volume(volume);
    }
}

#[tauri::command]
pub fn player_set_mute(st: State<PlayerState>, mute: bool) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        m.set_mute(mute);
    }
}

#[tauri::command]
pub fn player_set_speed(st: State<PlayerState>, speed: f64) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        m.set_speed(speed);
    }
}

#[tauri::command]
pub fn player_snapshot_save(st: State<PlayerState>) -> Result<String, String> {
    let dir = dirs_pictures().join("vqa_snapshots");
    let _ = std::fs::create_dir_all(&dir);
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = dir.join(format!("snap_{ts}.png"));
    let m = st.mpv.lock().unwrap().clone().ok_or("播放器未运行")?;
    m.screenshot(&path).map_err(|e| format!("{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn player_record_toggle(st: State<PlayerState>) -> Result<String, String> {
    let mut rec = st.recording.lock().unwrap();
    let m = st.mpv.lock().unwrap().clone().ok_or("播放器未运行")?;
    if *rec {
        let _ = m.stop_record();
        *rec = false;
        let path = st.rec_path.lock().unwrap().take();
        Ok(path
            .map(|p| format!("已录制 {}", p.display()))
            .unwrap_or_else(|| "已停止录制".into()))
    } else {
        let dir = dirs_videos().join("vqa_recordings");
        let _ = std::fs::create_dir_all(&dir);
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let path = dir.join(format!("rec_{ts}.ts"));
        m.record(&path).map_err(|e| format!("{e}"))?;
        *st.rec_path.lock().unwrap() = Some(path);
        *rec = true;
        Ok("录制中（stream-record 直拷）".into())
    }
}

/// 播放器状态快照（前端 100ms 轮询）。含管道排空 + 探针收割。
#[tauri::command]
pub fn player_snapshot(st: State<PlayerState>) -> serde_json::Value {
    // 1) 管道排空（保活必需）
    let mpv_arc = st.mpv.lock().unwrap().clone();
    let mut died = false;
    if let Some(m) = &mpv_arc {
        m.poll();
        died = m.is_dead();
    }

    // 2) 收割探针消息
    let rx = st.probe_rx.lock().unwrap().take();
    if let Some(rx) = rx {
        loop {
            match rx.try_recv() {
                Ok(ProbeMsg::Info(q)) => {
                    if let Some(q) = q {
                        *st.probe_info.lock().unwrap() = Some(q);
                    }
                }
                Ok(ProbeMsg::Bridging(msg)) => {
                    st.set_hint_for(msg, 300);
                }
                Ok(ProbeMsg::Outcome(launch)) => match launch {
                    Launch::Direct => {
                        if let Some(m) = &mpv_arc {
                            let url = st.playing_url.lock().unwrap().clone();
                            if !url.is_empty() {
                                let _ = m.loadfile(&url);
                                st.set_hint("已加载");
                            }
                        }
                    }
                    Launch::Bridged { url, reason, bridge } => {
                        if st.playing_url.lock().unwrap().is_empty() {
                            // 用户已停止：起好的桥直接丢弃（Drop 会 kill 子进程）
                            drop(bridge);
                        } else if let Some(m) = &mpv_arc {
                            // ★ chain10 本地文件模式：url = 全片转码完成的 TS 文件
                            //   （start 同步等转码完成后返回），loadfile 即完整播放，
                            //   可拖动、零丢包零卡顿。直播源 url 仍是流地址。
                            let _ = m.loadfile(&url);
                            let _ = m.show_text(&reason, 4000);
                            *st.bridge_note.lock().unwrap() = reason;
                            *st.bridge_url.lock().unwrap() = url.clone();
                            *st.bridge_since.lock().unwrap() = Some(std::time::Instant::now());
                            *st.bridge.lock().unwrap() = Some(bridge);
                        } else {
                            drop(bridge);
                        }
                    }
                    Launch::Failed(msg) => {
                        *st.err.lock().unwrap() = Some(msg.clone());
                        // ★ AVS2 源不回退直放：mpv 内置 davs2 不支持 10-bit
                        //   （"Un-supported bit-depth 10"），直放必崩 mpv。
                        //   只显示桥失败原因，让用户看错误行。
                        let is_avs2 = st
                            .probe_info
                            .lock()
                            .unwrap()
                            .as_ref()
                            .map(|q| q.video.codec.contains("avs2"))
                            .unwrap_or(false);
                        if !is_avs2 {
                            if let Some(m) = &mpv_arc {
                                let url = st.playing_url.lock().unwrap().clone();
                                if !url.is_empty() {
                                    // 回退直放
                                    let _ = m.loadfile(&url);
                                }
                            }
                        }
                    }
                },
                Err(_) => {
                    // 管道空了：放回（起播管线运行期间每帧重试）
                    *st.probe_rx.lock().unwrap() = Some(rx);
                    break;
                }
            }
        }
    }

    // 2.5) 桥流自动重连：播放中 + 桥模式 + mpv 已回 idle → 周期性重载桥流
    //（仅直播流桥；本地文件桥播完 EOF 是正常结束，不重播）
    {
        let file_bridge = st
            .bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.is_file())
            .unwrap_or(false);
        let retry_left = *st.bridge_retry_left.lock().unwrap();
        if retry_left > 0 && !file_bridge {
            let idle = mpv_arc.as_ref().map(|m| m.snapshot().idle).unwrap_or(true);
            let due = st
                .bridge_last_retry
                .lock()
                .unwrap()
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(2))
                .unwrap_or(true);
            if idle && due {
                let url = st.bridge_url.lock().unwrap().clone();
                if !url.is_empty() && !st.playing_url.lock().unwrap().is_empty() {
                    if let Some(m) = &mpv_arc {
                        let _ = m.loadfile(&url);
                        *st.bridge_retry_left.lock().unwrap() = retry_left - 1;
                        *st.bridge_last_retry.lock().unwrap() =
                            Some(std::time::Instant::now());
                        st.set_hint(format!("等待转码桥出流…（{}）", retry_left - 1));
                    }
                } else {
                    *st.bridge_retry_left.lock().unwrap() = 0;
                }
            }
        }
    }

    // 2.6) 桥重试窗口耗尽仍无画面 → 给出性能诊断（不是含糊的失败）
    //（本地文件桥不适用：EOF 回 idle 是播完，不是桥没出流）
    {
        let file_bridge = st
            .bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.is_file())
            .unwrap_or(false);
        let retry_left = *st.bridge_retry_left.lock().unwrap();
        if retry_left == 0 && !file_bridge {
            let bridge_on = !st.bridge_note.lock().unwrap().is_empty();
            // ★ 计时 2 分钟：起播瞬间 mpv 短暂 idle 是正常的（loadfile 异步
            //   生效），不立即判死；只有桥就绪超过 2 分钟仍无画面才是真失败
            let waited = st
                .bridge_since
                .lock()
                .unwrap()
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(120))
                .unwrap_or(false);
            let idle = mpv_arc.as_ref().map(|m| m.snapshot().idle).unwrap_or(true);
            if bridge_on && waited && idle && !st.playing_url.lock().unwrap().is_empty() {
                let mode = st.bridge_note.lock().unwrap().clone();
                *st.err.lock().unwrap() = Some(format!(
                    "转码桥（{mode}）超过 2 分钟未出画面——本机解码性能可能不足以实时转码该规格（如 4K 10-bit AVS2）。可尝试较低分辨率/8-bit 的源"
                ));
                // 停桥止血（Drop kill 子进程）
                *st.bridge.lock().unwrap() = None;
                *st.bridge_retry_left.lock().unwrap() = u32::MAX / 2; // 不再重复报
            }
        }
    }

    // 3) mpv 死亡处理
    if died {
        *st.mpv.lock().unwrap() = None;
        *st.bridge_since.lock().unwrap() = None;
        // 若正处于桥重连等待期，按桥的报错情况分级诊断
        let was_bridge_wait = *st.bridge_retry_left.lock().unwrap() > 0
            && *st.bridge_retry_left.lock().unwrap() < u32::MAX / 2;
        *st.bridge_retry_left.lock().unwrap() = 0;
        let bridge_tail = st
            .bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.stderr_tail().trim().to_string())
            .unwrap_or_default();
        *st.bridge.lock().unwrap() = None;
        let existing = st.err.lock().unwrap().clone();
        *st.err.lock().unwrap() = Some(if was_bridge_wait && bridge_tail.is_empty() {
            "源内容播放完毕（转码桥正常结束）".into()
        } else if was_bridge_wait {
            format!("mpv 已退出；转码桥报错：{bridge_tail}")
        } else if existing.is_some() {
            // 桥失败回退直放后 mpv 崩（AVS2 10-bit 直放必崩）——保留桥错误
            existing.unwrap()
        } else {
            "mpv 已退出（崩溃或被关闭）".into()
        });
    }

    // 4) 组装
    let snap = mpv_arc.as_ref().map(|m| m.snapshot());
    let mut hint = st
        .hint
        .lock()
        .unwrap()
        .take()
        .filter(|(_, t, d)| t.elapsed() <= *d)
        .map(|(s, _, _)| s);
    serde_json::json!({
        "playing": !st.playing_url.lock().unwrap().is_empty(),
        "err": *st.err.lock().unwrap(),
        "hint": hint,
        "bridge_note": *st.bridge_note.lock().unwrap(),
        "bridge": st.bridge.lock().unwrap().as_ref().map(|b| {
            serde_json::json!({
                "mode": b.mode,
                "port": b.port,
                "alive": b.alive(),
                "stderr_tail": b.stderr_tail(),
            })
        }),
        "recording": *st.recording.lock().unwrap(),
        "probe": st.probe_info.lock().unwrap().as_ref().map(probe_json),
        "snap": snap.as_ref().map(|s| serde_json::json!({
            "time_ms": s.time_ms,
            "duration_ms": s.duration_ms,
            "paused": s.paused,
            "idle": s.idle,
            "seekable": s.seekable,
            "width": s.width,
            "height": s.height,
            "hwdec": s.hwdec,
            "video_codec": s.video_codec,
            "container_fps": s.container_fps,
            "vf_fps": s.vf_fps,
            "v_bitrate": s.v_bitrate,
            "audio_codec": s.audio_codec,
            "a_bitrate": s.a_bitrate,
            "avsync": s.avsync,
            "render_drop": s.render_drop,
            "decoder_drop": s.decoder_drop,
            "dec_pixfmt": s.dec_pixfmt,
            "speed": s.speed,
            "volume": s.volume,
            "mute": s.mute,
        })),
    })
}

fn probe_json(q: &probe::QuickProbe) -> serde_json::Value {
    serde_json::json!({
        "w": q.video.width, "h": q.video.height,
        "codec": q.video.codec,
        "pix_fmt": q.video.pix_fmt,
        "bit_rate": q.bit_rate,
    })
}

fn dirs_pictures() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|h| PathBuf::from(h).join("Pictures"))
        .unwrap_or_else(|| PathBuf::from("."))
}
fn dirs_videos() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|h| PathBuf::from(h).join("Videos"))
        .unwrap_or_else(|| PathBuf::from("."))
}

// ═══════════════════════════ 视频分析 ═══════════════════════════

#[tauri::command]
pub fn analysis_start(
    ast: State<'_, AnalysisState>,
    url: String,
    nic_ip: String,
) -> Result<(), String> {
    let mut mode = ast.mode.lock().unwrap();
    if *mode != 0 {
        return Ok(()); // 运行中忽略重复启动（Enter+按钮双触发）
    }
    ast.live_err.lock().unwrap().take();
    let cleaned = crate::clean_stream_url(&url);
    let is_live = cleaned.starts_with("udp://") || cleaned.starts_with("rtp://");

    if is_live {
        match vqa_core::live::LiveSession::start(&cleaned, &nic_ip, &ast.overrides.lock().unwrap())
        {
            Ok(s) => {
                *ast.live.lock().unwrap() = Some(s);
                *ast.result.lock().unwrap() = None;
                *mode = 2;
                ast.log_push(format!("实时分析已启动：{cleaned}"));
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e:#}");
                *ast.live_err.lock().unwrap() = Some(msg.clone());
                Err(msg)
            }
        }
    } else if std::path::Path::new(&cleaned).is_file() {
        let (tx, rx) = std::sync::mpsc::channel();
        *ast.result_rx.lock().unwrap() = Some(rx);
        *ast.result.lock().unwrap() = None;
        *ast.progress.lock().unwrap() =
            (0, std::fs::metadata(&cleaned).map(|m| m.len()).unwrap_or(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *ast.stop_flag.lock().unwrap() = Some(Arc::clone(&stop));
        let overrides = Arc::clone(&ast.overrides);
        let path = cleaned.clone();
        *mode = 1;
        ast.log_push(format!("开始扫描：{cleaned}"));
        std::thread::Builder::new()
            .name("vqa-scan".into())
            .spawn(move || {
                let mut res = vqa_core::tsfile::scan_file(
                    std::path::Path::new(&path),
                    Some(&mut |d, t| {
                        PROG_DONE.store(d, Ordering::Relaxed);
                        PROG_TOTAL.store(t, Ordering::Relaxed);
                    }),
                    Some(&|| stop.load(Ordering::Relaxed)),
                    &overrides.lock().unwrap(),
                );
                if res.not_ts {
                    let _ = tx.send(Err("不是 188/204 包长的 TS 流".into()));
                    return;
                }
                if let Some(ffprobe) = vqa_core::tools::find_ffprobe() {
                    let (gops, truncated) =
                        vqa_core::tsfile::gop_list(&ffprobe, &path, 60.0);
                    res.gop = gops;
                    res.gop_note =
                        if truncated { "前 60 秒".into() } else { String::new() };
                }
                let _ = tx.send(Ok(res));
            })
            .ok();
        Ok(())
    } else {
        let msg = format!("来源不存在或不支持：{cleaned}");
        *ast.live_err.lock().unwrap() = Some(msg.clone());
        Err(msg)
    }
}

static PROG_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROG_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[tauri::command]
pub fn analysis_stop(ast: State<AnalysisState>) {
    let mut mode = ast.mode.lock().unwrap();
    match *mode {
        2 => {
            if let Some(l) = ast.live.lock().unwrap().take() {
                l.stop();
            }
            ast.log_push("实时分析已停止");
        }
        _ => {
            if let Some(f) = ast.stop_flag.lock().unwrap().as_ref() {
                f.store(true, Ordering::Relaxed);
            }
            ast.log_push("扫描中断");
        }
    }
    *mode = 0;
}

#[tauri::command]
pub fn analysis_progress(ast: State<AnalysisState>) -> serde_json::Value {
    // 收割扫描线程（完成后回待机态、结果保留）
    let mut mode = ast.mode.lock().unwrap();
    if *mode == 1 {
        let rx = ast.result_rx.lock().unwrap().take();
        if let Some(rx) = rx {
            match rx.try_recv() {
                Ok(Ok(res)) => {
                    ast.log_push(format!(
                        "扫描完成：{} 包 / {:.3}s / {:.0} kbps",
                        res.packets,
                        res.duration_pcr(),
                        res.bitrate_avg() / 1000.0
                    ));
                    let path = res.path.clone();
                    *ast.pkt.lock().unwrap() =
                        vqa_core::tsfile::PacketFile::open(std::path::Path::new(&path)).ok();
                    *ast.result.lock().unwrap() = Some(Arc::new(res));
                    *mode = 0;
                }
                Ok(Err(e)) => {
                    *ast.live_err.lock().unwrap() = Some(e);
                    *mode = 0;
                }
                Err(_) => {
                    // 还没完：放回，下轮轮询再试
                    *ast.result_rx.lock().unwrap() = Some(rx);
                }
            }
        }
    }
    drop(mode);

    let mode_v = *ast.mode.lock().unwrap();
    let live_bytes = ast
        .live
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|l| l.with_core(|c| c.bytes))
        .unwrap_or(0);
    serde_json::json!({
        "mode": mode_v,
        "done": PROG_DONE.load(Ordering::Relaxed),
        "total": PROG_TOTAL.load(Ordering::Relaxed),
        "live_bytes": live_bytes,
        "err": *ast.live_err.lock().unwrap(),
    })
}

#[tauri::command]
pub fn analysis_log(ast: State<AnalysisState>) -> Vec<String> {
    ast.log.lock().unwrap().iter().cloned().collect()
}

/// 扫描/实时结果（slim：码率抽稀 ≤1200、PCR 尾 100、SI 列表前 50 条）。
#[tauri::command]
pub fn analysis_result(ast: State<AnalysisState>) -> Option<serde_json::Value> {
    // 实时模式优先：现场从 LiveCore 提取
    if let Some(l) = ast.live.lock().unwrap().as_ref() {
        let v = l.with_core(|c| slim_result(&c.scanner.res));
        return v;
    }
    ast.result
        .lock()
        .unwrap()
        .as_ref()
        .map(|r| slim_result(r))
}

#[tauri::command]
pub fn packet_rows(ast: State<AnalysisState>, start: u64, count: u64) -> serde_json::Value {
    let pkt = ast.pkt.lock().unwrap();
    let Some(f) = pkt.as_ref() else {
        return serde_json::json!({ "total": 0, "rows": [] });
    };
    let rows: Vec<serde_json::Value> = f
        .iter_from(start, count)
        .iter()
        .map(|(idx, bytes)| {
            let pi = vqa_core::tsfile::packet_info(bytes);
            let hex: String =
                bytes.iter().take(16).map(|b| format!("{b:02X} ")).collect();
            serde_json::json!({
                "idx": idx, "pid": pi.pid, "pusi": pi.pusi,
                "afc": pi.afc, "cc": pi.cc, "hex": hex.trim_end(),
            })
        })
        .collect();
    serde_json::json!({ "total": f.count, "rows": rows })
}

#[tauri::command]
pub fn export_report_md(
    app: tauri::AppHandle,
    ast: State<AnalysisState>,
    path: Option<String>,
) -> Result<Option<String>, String> {
    let res = ast
        .result
        .lock()
        .unwrap()
        .clone()
        .ok_or("没有可导出的扫描结果")?;
    let dest = match path {
        Some(p) => p,
        None => {
            use tauri_plugin_dialog::DialogExt;
            app.dialog()
                .file()
                .add_filter("Markdown", &["md"])
                .set_file_name("TS流质量检测报告.md")
                .blocking_save_file()
                .map(|p| p.to_string())
                .ok_or_else(|| "已取消".to_string())?
        }
    };
    std::fs::write(&dest, build_markdown(&res)).map_err(|e| format!("写报告失败：{e}"))?;
    Ok(Some(dest))
}

fn build_markdown(res: &vqa_core::tsfile::ScanResult) -> String {
    let mut md = String::new();
    md.push_str("# TS 流质量检测报告\n\n");
    md.push_str(&format!(
        "- 来源：{}\n- 大小：{} 字节 · {} 包 · 包长 {}\n- 总码率：{:.3} Mbps（PCR 区间口径）\n- PCR 时长：{:.3} s · 同步错误 {} 字节\n\n",
        res.path, res.file_size, res.packets, res.packet_size,
        res.bitrate_avg() / 1e6, res.duration_pcr(), res.sync_errors
    ));
    md.push_str("## 节目结构\n\n| 节目 | PMT | PCR | 流 |\n|---|---|---|---|\n");
    for p in &res.programs {
        let streams = p
            .streams
            .iter()
            .map(|s| format!("0x{:04X} {}", s.pid, s.type_name()))
            .collect::<Vec<_>>()
            .join("、");
        md.push_str(&format!(
            "| {} | 0x{:04X} | 0x{:04X} | {} |\n",
            p.number, p.pmt_pid, p.pcr_pid, streams
        ));
    }
    md.push_str("\n## TR 101 290\n\n| 级别 | 检测项 | 次数 | 最近发生 |\n|---|---|---|---|\n");
    for t in &res.tr290 {
        md.push_str(&format!(
            "| P{} | {} | {} | {} |\n",
            t.level, t.name, t.count, t.first
        ));
    }
    md
}

// ═══════════════════════════ 序列化辅助 ═══════════════════════════

/// ScanResult → 前端 JSON（大数组瘦身）。
pub fn slim_result(res: &vqa_core::tsfile::ScanResult) -> serde_json::Value {
    use vqa_core::tsfile::SiValue;
    // 码率抽稀：等距取样 ≤1200 点
    let step = (res.rate.len() / 1200).max(1);
    let rate_json: Vec<serde_json::Value> = res
        .rate
        .iter()
        .step_by(step)
        .map(|p| serde_json::json!([p.t0, p.t1, p.bps()]))
        .collect();
    // PCR 采样：最近 100 条
    let pc_start = res.pcr_samples.len().saturating_sub(100);
    let pcr_json: Vec<serde_json::Value> = res.pcr_samples[pc_start..]
        .iter()
        .map(|s| serde_json::json!([s.off, s.t, s.interval_ms, s.oj_ms, s.ac_ms]))
        .collect();
    // SI：列表截前 50 条
    let si_json = res
        .si
        .iter()
        .map(|(k, v)| {
            let val = match v {
                SiValue::List(items) => {
                    serde_json::json!(items.iter().take(50).collect::<Vec<_>>())
                }
                SiValue::Text(t) => serde_json::json!(t),
            };
            (k.clone(), val)
        })
        .collect::<serde_json::Map<_, _>>();
    let gop_json: Vec<serde_json::Value> = res
        .gop
        .iter()
        .take(50)
        .map(|g| serde_json::json!([g.start, g.dur, g.frames]))
        .collect();
    serde_json::json!({
        "path": res.path,
        "file_size": res.file_size,
        "packet_size": res.packet_size,
        "not_ts": res.not_ts,
        "sync_errors": res.sync_errors,
        "packets": res.packets,
        "duration_pcr": res.duration_pcr(),
        "bitrate_avg": res.bitrate_avg(),
        "encrypted": res.encrypted(),
        "pcr_pid": res.pcr_pid,
        "pcr_cbr": res.pcr_cbr,
        "has_scrambled": res.has_scrambled,
        "si_truncated": res.si_truncated,
        "gop_note": res.gop_note,
        "programs": res.programs.iter().map(|p| serde_json::json!({
            "number": p.number, "pmt_pid": p.pmt_pid, "pcr_pid": p.pcr_pid,
            "streams": p.streams.iter().map(|s| serde_json::json!({
                "pid": s.pid, "type_name": s.type_name(), "kind": s.kind,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "pids": res.top_pids(64).iter().map(|p| serde_json::json!({
            "pid": p.pid, "kind": p.kind, "packets": p.packets,
            "bytes": p.bytes, "payload_bytes": p.payload_bytes,
            "scrambled": p.scrambled,
        })).collect::<Vec<_>>(),
        "si": si_json,
        "tr290": res.tr290,
        "rate": rate_json,
        "pcr_samples": pcr_json,
        "gop": gop_json,
    })
}
