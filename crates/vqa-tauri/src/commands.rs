//! Tauri 命令层 —— 全部薄封装 vqa-core，无业务逻辑。
//!
//! 状态流：前端轮询命令（播放器 100ms / 扫描进度 200ms / 实时 1s），
//! 与 egui 版"每帧 poll"模型一一对应。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::State;

use crate::state::{AnalysisState, BridgeHandle, Launch, PlayerState, ProbeMsg};
use vqa_core::probe;

// ═══════════════════════════ 通用 ═══════════════════════════

#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

#[tauri::command]
pub fn list_nics() -> Vec<vqa_core::netif::IfRow> {
    // 带网卡名/描述/up 状态，界面显示「IP · 网卡名」；ip 仍是取值依据
    vqa_core::netif::choices_detail()
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

/// 数据/日志目录（关于页「数据与日志」卡）
#[tauri::command]
pub fn app_paths() -> serde_json::Value {
    let logs = vqa_core::config::log_dir();
    serde_json::json!({
        "data": vqa_core::config::base_dir().to_string_lossy(),
        "shared": vqa_core::config::shared_dir().to_string_lossy(),
        "logs": logs.to_string_lossy(),
        "log_file": logs.join("vqa.log").to_string_lossy(),
        "scratch": vqa_core::config::scratch_dir().to_string_lossy(),
        "snap": snap_dir().to_string_lossy(),
        "rec": rec_dir().to_string_lossy(),
        "crash": vqa_core::config::crash_dir().to_string_lossy(),
    })
}

/// 在资源管理器打开目录 / 定位文件（路径不存在时退到其父目录）。
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    let p = PathBuf::from(path.trim());
    if p.as_os_str().is_empty() {
        return Err("路径为空".into());
    }
    let target = if p.exists() {
        p.clone()
    } else {
        p.parent().filter(|x| x.exists()).map(|x| x.to_path_buf()).unwrap_or(p.clone())
    };
    let mut cmd = std::process::Command::new("explorer.exe");
    if target.is_file() {
        cmd.arg(format!("/select,{}", target.to_string_lossy()));
    } else {
        cmd.arg(target.to_string_lossy().to_string());
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn thresholds_get(st: State<AnalysisState>) -> serde_json::Value {
    let ov = st.overrides.lock().unwrap();
    serde_json::to_value(&ov.map).unwrap_or(serde_json::json!({}))
}

// ═══════════════════════════ 诊断日志 ═══════════════════════════

/// 组装诊断日志（MAC+时间命名；环境+状态+分析摘要+事件时间线+采样+日志+崩溃记录）。
#[tauri::command]
pub fn diag_collect(pst: State<PlayerState>, ast: State<AnalysisState>) -> serde_json::Value {
    let player = player_snapshot(pst);
    let mode = *ast.mode.lock().unwrap();
    let err = ast.live_err.lock().unwrap().clone();
    let summary = analysis_block(&ast);
    let (name, content) = crate::diag::collect(&player, mode, err, &summary);
    serde_json::json!({ "name": name, "content": content })
}

/// 分析结果摘要（诊断包用）：把"扫出来的质量问题"带走，不留只留模式号。
fn analysis_block(ast: &AnalysisState) -> String {
    if let Some(l) = ast.live.lock().unwrap().as_ref() {
        let bytes = l.with_core(|c| c.bytes).unwrap_or(0);
        return format!(
            "实时分析中（模式 2）：已接收 {:.1} MB\n（实时流的 TR101 290 计数不随诊断包导出，需停止后看分析页）",
            bytes as f64 / 1048576.0
        );
    }
    let res = ast.result.lock().unwrap().clone();
    let Some(r) = res else {
        return "（尚未扫描）".to_string();
    };

    let mut s = String::new();
    let dur = match (r.first_pcr, r.last_pcr) {
        (Some(a), Some(b)) => (b - a).max(0.0),
        _ => 0.0,
    };
    s.push_str(&format!("来源        : {}\n", r.path));
    s.push_str(&format!(
        "包数 / 包长 : {} / {} 字节\n",
        r.packets, r.packet_size
    ));
    s.push_str(&format!(
        "体积 / 时长 : {:.1} MB / {:.3} s\n",
        r.file_size as f64 / 1048576.0,
        dur
    ));
    if dur > 0.0 {
        s.push_str(&format!(
            "平均码率    : {:.2} Mbps\n",
            r.file_size as f64 * 8.0 / dur / 1e6
        ));
    }
    s.push_str(&format!(
        "PID / 节目  : {} / {}\n",
        r.pids.len(),
        r.programs.len()
    ));
    s.push_str(&format!(
        "同步错误    : {}   PAT={} PMT={}\n",
        r.sync_errors, r.has_pat, r.has_pmt
    ));
    match r.pcr_pid {
        Some(p) => s.push_str(&format!("PCR PID     : 0x{p:04X}\n")),
        None => s.push_str("PCR PID     : 未找到\n"),
    }
    let max_j = r
        .pcr_samples
        .iter()
        .map(|x| x.oj_ms.abs())
        .fold(0.0f64, f64::max);
    s.push_str(&format!(
        "PCR 抖动    : 最大 {:.3} ms（CBR 判定 {}）\n",
        max_j, r.pcr_cbr
    ));
    s.push_str("TR101 290   :\n");
    let mut any = false;
    for it in &r.tr290 {
        if it.count > 0 {
            any = true;
            s.push_str(&format!(
                "  P{} {:<14} {:>7} 次  首次 {}  {}\n",
                it.level, it.name, it.count, it.first, it.detail
            ));
        }
    }
    if !any {
        s.push_str("  （全部正常）\n");
    }
    s
}

/// 媒体信息 —— ffprobe 全量探测（容器 + 每条流的编码/分辨率/帧率/码率/色彩）。
#[tauri::command]
pub fn media_info(path: String) -> Result<serde_json::Value, String> {
    let cleaned = crate::clean_stream_url(&path);
    if cleaned.is_empty() {
        return Err("来源为空".into());
    }
    crate::mediainfo::probe(&cleaned)
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

/// 上传诊断包给开发者（凭据：本机 saved > `VQA_DIAG_TOKEN` > 安装包自带的
/// `bin/diag_secret.json`；界面不再暴露输入框）。
#[tauri::command]
pub fn diag_upload(token: Option<String>, name: String, content: String) -> Result<String, String> {
    let tok = match token {
        Some(t) if !t.trim().is_empty() => {
            let _ = vqa_core::config::shared_set("diag_token", serde_json::json!(t.trim()));
            t.trim().to_string()
        }
        _ => crate::diag::resolve_token().ok_or_else(|| {
            "本机未配置上传凭据，请先用「导出诊断包到本地」把文件发给开发者".to_string()
        })?,
    };
    crate::diag::upload_issue(&tok, &name, &content)
}

/// 是否具备上传能力（本机已存或安装包自带凭据）。
#[tauri::command]
pub fn diag_has_token() -> bool {
    crate::diag::resolve_token().is_some()
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

/// 收掉抽节目模式的 ffmpeg 子进程（mpv 会因 stdin EOF 回 idle）
fn kill_feed(st: &PlayerState) {
    if let Some(mut c) = st.feed.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

// ═══════════════════════════ 播放器：开播前预热 ═══════════════════════════
/// 预热窗口的收尾标记：线程结束（成功/早退/失败）都把 `prewarm_url` 清掉，
/// 免得留下"正在预热"的假状态，让下一次点播放白等 3 秒。
struct PrewarmMark(tauri::AppHandle, String);

impl Drop for PrewarmMark {
    fn drop(&mut self) {
        use tauri::Manager;
        let st = self.0.state::<PlayerState>();
        let mut u = st.prewarm_url.lock().unwrap();
        if *u == self.1 {
            u.clear();
        }
    }
}

/// ★ 文件一拖入/选定就在后台起桥转码，点播放时直接读缓存。
///
/// 起播慢的账是硬账：准备（音频规范化 ∥ 切分片）1.6~1.8 秒 + 首片整链 4.3 秒
/// + mpv 探测出画 0.4~0.6 秒 ≈ 7 秒。这段算力开销压不掉，只能**提前花掉**——
/// 挪到用户拖入文件、看信息、挑时间那段空闲里，点播放时就是缓存命中（0.3~1 秒）。
///
/// 只给真正需要桥的本地 AVS 源预热：能直放的文件本来就秒开，预热纯属白烧 CPU。
#[tauri::command]
pub fn player_prewarm(
    app: tauri::AppHandle,
    url: String,
    nic_ip: String,
    force_bridge: bool,
) -> Result<(), String> {
    use tauri::Manager;
    let cleaned = crate::clean_stream_url(&url);
    if cleaned.is_empty() {
        return Err("来源为空".into());
    }
    {
        // 同一个文件已经在预热：不重来（每重起一次都作废已转好的分片）
        let st = app.state::<PlayerState>();
        if let Some((u, _)) = st.prewarm.lock().unwrap().as_ref() {
            if u == &cleaned {
                return Ok(());
            }
        }
        // 已经在播这个文件：没必要再起一座并行的桥抢 NVENC
        let playing = st.playing_url.lock().unwrap().clone();
        if !playing.is_empty() && playing == cleaned {
            return Ok(());
        }
        // 打上"正在预热"标记：这 1~2 秒里用户点播放，player_play 会等桥就位再接手，
        // 不会另起一条链同时写同一个缓存文件
        *st.prewarm_url.lock().unwrap() = cleaned.clone();
    }
    let app2 = app.clone();
    std::thread::Builder::new()
        .name("vqa-prewarm".into())
        .spawn(move || {
            use tauri::Manager;
            // 线程退出（成功/早退/失败）时把标记清掉，不留假状态
            let _mark = PrewarmMark(app2.clone(), cleaned.clone());
            // 探测（probe_quick 内部带缓存，重复调用近乎零成本）
            let q = probe::probe_quick(
                &cleaned,
                if vqa_core::bridge::is_local(&cleaned) { 5.0 } else { 6.0 },
                &nic_ip,
            );
            if !vqa_core::bridge::needs_bridge(&q, force_bridge) {
                return; // 直放够快，不需要预热
            }
            // 与 decide_bridge 同口径：只给 AVS2/10-bit 的本地源走 chain10 桥，
            // AVS3 必须留给 ffmpeg 的 libuavs3d，不能塞给 vkit_davs2。
            let chain10 = q
                .as_ref()
                .map(|x| {
                    let c = x.video.codec.to_ascii_lowercase();
                    let is_avs2 = c.contains("avs2") || c == "davs2";
                    let is_avs3 = c.contains("avs3") || c == "uavs3d";
                    is_avs2 || (!is_avs3 && x.video.pix_fmt.contains("10"))
                })
                .unwrap_or(false);
            if !chain10 || !vqa_core::bridge::is_local(&cleaned) {
                return;
            }
            let Some(ffmpeg) = vqa_core::tools::find_ffmpeg() else {
                return;
            };
            let Some(davs2) = vqa_core::tools::find_davs2() else {
                return;
            };
            let audio_lead = q.as_ref().map(|x| x.audio_lead()).unwrap_or(0.0);
            match vqa_core::bridge_par::ParBridge::start(&ffmpeg, &davs2, &cleaned, 26, audio_lead) {
                Ok(pb) => {
                    let prog = pb.progress();
                    crate::events::ev("prewarm", &format!("后台预热已起（{prog}）"));
                    let st = app2.state::<PlayerState>();
                    // 这段时间用户可能已经点了播放（桥被认领走）或换了文件：只在
                    // 槽里还空着、且没在播同一个文件时才存下，免得两份同时写缓存
                    let playing = st.playing_url.lock().unwrap().clone();
                    if !playing.is_empty() && playing == cleaned {
                        crate::events::ev("prewarm", "已在播放，丢弃这次预热");
                        return;
                    }
                    // 用户在等预热期间换去播别的文件了：这条预热没意义，收掉让出 NVENC
                    if !playing.is_empty() {
                        crate::events::ev("prewarm", "正在播别的文件，预热让位");
                        return;
                    }
                    {
                        // 槽里若不是同一个文件（更晚的预热占了位），别覆盖
                        let mut pw = st.prewarm.lock().unwrap();
                        match pw.as_ref() {
                            Some((u, _)) if u != &cleaned => return,
                            _ => {}
                        }
                        *pw = Some((cleaned.clone(), Box::new(pb)));
                    }
                    // 盯到转完：完成后把桥收掉，只留磁盘缓存（下次播放直接命中）
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let st2 = app2.state::<PlayerState>();
                        let mut pw = st2.prewarm.lock().unwrap();
                        let (ours, p) = match pw.as_ref() {
                            Some((u, b)) if u == &cleaned => (true, b.progress()),
                            _ => (false, String::new()),
                        };
                        if !ours {
                            break; // 已被播放认领走，或用户换了文件
                        }
                        if !(p.starts_with("准备中")
                            || p.starts_with("预转")
                            || p.starts_with("转码"))
                        {
                            *pw = None; // Drop → stop；已 done 的缓存文件保留
                            crate::events::ev("prewarm", "预热完成，已转成缓存");
                            break;
                        }
                    }
                }
                Err(e) => {
                    vqa_core::config::log_line("prewarm", &format!("预热起不来：{e}"));
                }
            }
        })
        .ok();
    Ok(())
}

/// 8K 直播直接 loadfile 时给 lavf 更大的探测窗口。
/// 默认 0.6s / 2MB 对 8K 高码率太短，首开可能只认出音频轨 → 有声无画；
/// 只对网络源生效，本地文件仍走秒开路径。
fn direct_load_opts(url: &str, q: Option<&probe::QuickProbe>) -> Option<&'static str> {
    if vqa_core::bridge::is_local(url) {
        return None;
    }
    let is_8k = q
        .map(|x| x.video.width >= 7000 && x.video.height >= 4000)
        .unwrap_or(false);
    if is_8k {
        return Some("demuxer-lavf-analyzeduration=1.5,demuxer-lavf-probesize=8388608");
    }
    // 探测没拿到宽高（0x0）时，不能按“普通源”用 0.6s/2MB：
    // 高码率直播往往只有音频头先到，视频参数还没解析出来，正是
    // “有声音没画面”的成因。给一段更大的窗口，让首帧能跟上。
    let unknown_size = q
        .map(|x| x.video.width <= 0 || x.video.height <= 0)
        .unwrap_or(true);
    if unknown_size {
        Some("demuxer-lavf-analyzeduration=1.2,demuxer-lavf-probesize=6291456")
    } else {
        None
    }
}

#[tauri::command]
pub fn player_play(
    st: State<'_, PlayerState>,
    url: String,
    nic_ip: String,
    force_bridge: bool,
) -> Result<(), String> {
    // 上一次若切过节目，先收掉抽流进程（mpv 会回 idle，接着 loadfile 正常播）
    kill_feed(&st);
    let cleaned = crate::clean_stream_url(&url);
    if cleaned.is_empty() {
        return Err("来源为空".into());
    }
    // 8K 自动降级的“锁”：同一个源再次起播时，继续用上一次的降级宽度。
    // 否则用户看到丢帧后重新点“开始”，又会从 8K 直放开始，来回循环。
    let auto_scale = {
        let locked_url = st.auto_scale_url.lock().unwrap().clone();
        if locked_url == cleaned {
            *st.auto_scale_w.lock().unwrap()
        } else {
            *st.auto_scale_url.lock().unwrap() = String::new();
            *st.auto_scale_w.lock().unwrap() = 0;
            *st.bridge_auto_restarts.lock().unwrap() = 0;
            *st.bridge_stderr_last.lock().unwrap() = String::new();
            *st.auto_decoder.lock().unwrap() = String::new();
            0
        }
    };
    // ★ 预热复用：同一个文件已经在后台预热过，就直接接上那座桥——转码早在跑、
    //   缓存也在同步写，用户点播放时要么直接是缓存（0.3~1 秒），要么 mpv 一读
    //   就有数据（不必再等首片 4~7 秒）。换文件则把旧预热丢掉（Drop 会 kill 子进程）。
    let adopted: Option<Box<dyn BridgeHandle>> = {
        // 若此刻预热刚起步（桥还没立起来），等它一下再接手——否则会另起一条
        // 并行链同时写同一个缓存文件，两份输出互相踩。
        let mut waited_ms = 0u64;
        loop {
            {
                let mut pw = st.prewarm.lock().unwrap();
                match pw.as_ref() {
                    Some((u, _)) if u == &cleaned => {
                        let b = pw.take().map(|(_, b)| b);
                        break b;
                    }
                    Some(_) => {
                        // 别的文件在预热：丢掉
                        *pw = None;
                        break None;
                    }
                    None => {}
                }
            }
            let pending = st.prewarm_url.lock().unwrap().clone();
            if pending == cleaned && waited_ms < 3000 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                waited_ms += 100;
                continue;
            }
            break None;
        }
    };
    // 停旧桥、清错误
    *st.bridge.lock().unwrap() = None;
    *st.bridge_note.lock().unwrap() = String::new();
    *st.bridge_url.lock().unwrap() = String::new();
    *st.bridge_since.lock().unwrap() = None;
    *st.bridge_had_picture.lock().unwrap() = false;
    *st.bridge_loaded.lock().unwrap() = false;
    *st.bridge_ready_at.lock().unwrap() = None;
    *st.preparing.lock().unwrap() = true;
    *st.err.lock().unwrap() = None;
    *st.playing_url.lock().unwrap() = cleaned.clone();
    *st.last_nic_ip.lock().unwrap() = nic_ip.clone();
    *st.loaded_at.lock().unwrap() = None;
    *st.video_retry_done.lock().unwrap() = false;
    *st.live_idle_since.lock().unwrap() = None;
    *st.live_retry_left.lock().unwrap() = 3;
    *st.live_had_picture.lock().unwrap() = false;
    *st.live_last_time.lock().unwrap() = 0;
    *st.live_stall_since.lock().unwrap() = None;

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
    // 循环播放是跨 loadfile 的开关，重新起播后要再下发一次
    if *st.loop_file.lock().unwrap() {
        let _ = mpv.set_prop("loop-file", serde_json::Value::String("inf".into()));
    }
    crate::events::ev("player", &format!("起播 {cleaned}"));
    crate::events::clear_samples();

    // ★ 已锁定 8K 降级宽度：直接起降级桥，不再探测/直放。
    //   这是避免“降级 → 重新起播 → 又直放 8K → 再降级”循环的关键。
    if auto_scale > 0 {
        *st.preparing.lock().unwrap() = false;
        crate::events::ev(
            "player",
            &format!("按上次降级宽度 {auto_scale} 直接起桥"),
        );
        *st.bridge_auto_restarts.lock().unwrap() = 0;
        return start_auto_bridge(&st, &cleaned, &nic_ip, auto_scale);
    }

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
            let outcome = decide_bridge(&q, force_bridge, &probe_url, &nic_ip, &tx, adopted);
            let _ = tx.send(ProbeMsg::Outcome(outcome));
        })
        .ok();

    // 加载时机对齐 egui 版口径：探针/桥决策完成后由收割侧 loadfile——
    // 10-bit AVS2 等直放必败的源不在桥起好前喂给 mpv
    st.set_hint("探测中…");
    Ok(())
}

/// 8K 播放性能不足时的自动降级：把当前源转成指定宽度的 H.264 回环流，
/// 让 mpv 只解降级后的画面。调用方（播放器页）按 5 秒平均丢帧触发，
/// 阶梯 3840 → 1920 → 1280。
/// 启动/复用 8K 自动降级桥。调用前 playing_url 必须已经指向源地址，
/// mpv 必须已启动；本函数只负责桥、状态和 loadfile。
fn start_auto_bridge(
    st: &PlayerState,
    url: &str,
    nic_ip: &str,
    scale_w: u32,
) -> Result<(), String> {
    let ffmpeg = vqa_core::tools::find_ffmpeg().ok_or("需要转码降级但未找到 ffmpeg")?;
    // ★ 重启桥时必须先停掉旧桥：旧 ffmpeg 还占着 udp 输入端口，
    //   新桥会报 "No such device or address" 起不来。Drop 会 kill+wait。
    //   同时先把旧桥 stderr 尾部留下来——旧句柄一 drop，退出原因就没了。
    if let Some(old) = st.bridge.lock().unwrap().take() {
        let tail = old.stderr_tail();
        if !tail.trim().is_empty() {
            *st.bridge_stderr_last.lock().unwrap() = tail.clone();
            vqa_core::config::log_line(
                "bridge",
                &format!("旧桥 stderr 尾部：{}", tail.trim()),
            );
        }
        drop(old);
    }
    let q = st.probe_info.lock().unwrap().clone();
    let probe_missing = q.is_none();
    let audio_lead = q.as_ref().map(|x| x.audio_lead()).unwrap_or(0.0);
    let avs3 = q
        .as_ref()
        .map(|x| {
            let c = x.video.codec.to_ascii_lowercase();
            c.contains("avs3") || c == "uavs3d"
        })
        .unwrap_or(false);
    let known_decoder = st.auto_decoder.lock().unwrap().clone();
    // 探测失败时不能赌一种解码器：8K AVS3 必须 libuavs3d，8K H.264
    // 又必须默认解码器。已经成功起过桥的解码器优先复用，避免每次重启
    // 都先拿错的解码器试一遍（8K AVS3 上默认解码器必失败）。
    let mut tries: Vec<Option<&'static str>> = Vec::new();
    if avs3 {
        tries.push(Some("libuavs3d"));
    } else if known_decoder == "libuavs3d" {
        tries.push(Some("libuavs3d"));
        tries.push(None);
    } else if known_decoder == "default" {
        tries.push(None);
        tries.push(Some("libuavs3d"));
    } else if probe_missing {
        // 先赌默认：真实 8K H.264 的 UDP 源 probe 失败时，默认解码器能起；
        // 万一是 AVS3，默认会秒退，再回退 libuavs3d。
        tries.push(None);
        tries.push(Some("libuavs3d"));
    } else {
        tries.push(None);
    }
    let mut bridge = None;
    let mut used_decoder: Option<&str> = None;
    let mut last_err = String::new();
    // 8K 自动降级监看不带源音频：信源 MP2 头损坏时，音频错误会刷满
    // stderr 并让看门狗误判"无推进"，比静音更影响视频监看。探不到格式的
    // 8K 源同样按静音处理，先保证画面能持续出。
    let drop_audio = scale_w > 0 && (avs3 || probe_missing);
    for dec in tries {
        match vqa_core::bridge::RealtimeBridge::start_scaled(
            &ffmpeg,
            std::path::Path::new(""),
            url,
            false,
            26,
            nic_ip,
            audio_lead,
            scale_w,
            dec,
            drop_audio,
        ) {
            Ok(b) => {
                bridge = Some(b);
                used_decoder = dec;
                break;
            }
            Err(e) => last_err = e,
        }
    }
    let b = bridge.ok_or_else(|| {
        format!("8K 转码桥起不来：{last_err}")
    })?;
    let bridge_url = b.url();
    let enc = vqa_core::bridge::pick_h264_encoder(&ffmpeg);
    *st.auto_decoder.lock().unwrap() =
        used_decoder.unwrap_or("default").to_string();
    *st.bridge.lock().unwrap() = Some(Box::new(b));
    *st.bridge_enc.lock().unwrap() = enc;
    *st.bridge_note.lock().unwrap() =
        format!("8K 自动降级：转码输出 {scale_w} 宽 / 25fps");
    *st.bridge_url.lock().unwrap() = bridge_url.clone();
    *st.bridge_since.lock().unwrap() = Some(std::time::Instant::now());
    *st.bridge_loaded.lock().unwrap() = true;
    *st.bridge_ready_at.lock().unwrap() = Some(std::time::Instant::now());
    *st.bridge_had_picture.lock().unwrap() = false;
    *st.bridge_retry_left.lock().unwrap() = 0;
    *st.preparing.lock().unwrap() = false;
    *st.loaded_at.lock().unwrap() = None;
    *st.video_retry_done.lock().unwrap() = false;
    *st.auto_scale_url.lock().unwrap() = url.to_string();
    *st.auto_scale_w.lock().unwrap() = scale_w;
    *st.bridge_dead_since.lock().unwrap() = None;
    *st.err.lock().unwrap() = None;
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.loadfile(&bridge_url);
    }
    st.set_hint_for(format!("8K 已转入 {scale_w} 宽 / 25fps 转码播放"), 30);
    crate::events::ev(
        "player",
        &format!("8K 自动降级：转码输出宽 {scale_w} / 25fps，源 {url}"),
    );
    Ok(())
}

#[tauri::command]
pub fn player_adaptive_scale(st: State<PlayerState>, scale_w: u32) -> Result<(), String> {
    let url = st.playing_url.lock().unwrap().clone();
    if url.is_empty() {
        return Err("当前没有在播的源".into());
    }
    if scale_w == 0 {
        return Err("降级宽度无效".into());
    }
    let nic_ip = st.last_nic_ip.lock().unwrap().clone();
    vqa_core::config::log_line(
        "player",
        &format!("adaptive-scale 请求：scale_w={scale_w} url={url}"),
    );
    *st.bridge_auto_restarts.lock().unwrap() = 0;
    start_auto_bridge(&st, &url, &nic_ip, scale_w)
}


fn decide_bridge(
    q: &Option<probe::QuickProbe>,
    force: bool,
    url: &str,
    nic_ip: &str,
    bridge_wait_tx: &std::sync::mpsc::Sender<ProbeMsg>,
    adopt: Option<Box<dyn BridgeHandle>>,
) -> Launch {
    let need = vqa_core::bridge::needs_bridge(q, force);
    // AVS3 走 ffmpeg 的 libuavs3d；vkit_davs2 只解 AVS2，不能套 chain10。
    let avs3 = q
        .as_ref()
        .map(|x| {
            let c = x.video.codec.to_ascii_lowercase();
            c.contains("avs3") || c == "uavs3d"
        })
        .unwrap_or(false);
    let h264_8k = q
        .as_ref()
        .map(|x| {
            let c = x.video.codec.to_ascii_lowercase();
            x.video.width >= 7000
                && x.video.height >= 4000
                && (c.contains("h264") || c.contains("avc"))
        })
        .unwrap_or(false);
    let avs3_8k = avs3
        && q.as_ref()
            .map(|x| x.video.width >= 7000 && x.video.height >= 4000)
            .unwrap_or(false);
    let big8k_bridge = avs3_8k || h264_8k;
    // ★ 音画同步修正量：源里音频比画面早到多少秒（桥必须裁掉音频头）
    let audio_lead = q.as_ref().map(|x| x.audio_lead()).unwrap_or(0.0);
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
    // ★ 预先预热好的桥优先复用：这座桥从文件拖入那一刻就在转码，缓存也一直在
    //   同步写。命中完整缓存时它 mode=cache（url 就是 .ts 文件）→ 直接秒开；
    //   还在转码时它 mode=par（url 是回环 TCP）→ mpv 一读就有数据，不必等首片。
    if let Some(b) = adopt {
        let prog = b.progress();
        let still_streaming = prog.starts_with("准备中")
            || prog.starts_with("预转")
            || prog.starts_with("转码");
        if still_streaming || b.mode() == "cache" {
            let reason = format!("预热复用（{}，{}）", b.mode(), prog);
            let burl = b.url();
            vqa_core::config::log_line("bridge-decide", &format!("复用预热桥：{reason} → {burl}"));
            let _ = bridge_wait_tx.send(ProbeMsg::Bridging(reason.clone()));
            return Launch::Bridged {
                url: burl,
                reason,
                auto_scale: 0,
                bridge: b,
            };
        }
        // ★ 已经转完的桥：TCP 发送端随时会收工，接上去有打不开的风险。
        //   直接丢掉，让下面的常规流程去命中磁盘缓存（.done 已在，0.3~1 秒）。
        vqa_core::config::log_line("bridge-decide", "预热已转完，改走缓存命中");
        drop(b);
    }
    vqa_core::config::log_line("bridge-decide", "起 10-bit chain10 桥…");
    // ★ 立刻告知前端"转码中"——全片转码大文件要 1-3 分钟，无反馈会被
    //   误判为卡死而重复点击（每次重试都作废上一次转码重来）
    let _ = bridge_wait_tx.send(ProbeMsg::Bridging(
        "AVS 桥已启动，数秒后出画…".into(),
    ));
    let Some(ffmpeg) = vqa_core::tools::find_ffmpeg() else {
        return Launch::Failed("需要转码桥但未找到 ffmpeg".into());
    };
    // ★ chain10 判定：只给 AVS2 用。AVS3 由 ffmpeg 的 libuavs3d 解码，
    //   vkit_davs2 只解 AVS2；把 AVS3 10-bit 错送进 chain10 会解不出画面。
    let chain10 = q
        .as_ref()
        .map(|q| {
            let c = q.video.codec.to_ascii_lowercase();
            let is_avs2 = c.contains("avs2") || c == "davs2";
            let is_avs3 = c.contains("avs3") || c == "uavs3d";
            is_avs2 || (!is_avs3 && q.video.pix_fmt.contains("10"))
        })
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
    // ★ 8K H.264 同样先输出 4K/25fps：H.264 NVDEC 上限 4096 宽，
    //   RTX 4060 Ti 也无法硬解 7680×4320；8K 软件解码后再交给 mpv 渲染
    //   会持续丢帧。AVS3 8K 也一样先降到 4K。
    let initial_scale = if big8k_bridge { 3840 } else { 0 };
    // ★ 本地 AVS 源优先走**并行分片桥**（bridge_par，照搬 Python 版 §31.50）：
    //   单链整链吞吐只有 0.63×实时（"播几秒卡几秒"的根因），并行 4 链叠到
    //   ~1.85×，转码跑在播放前面——秒开、流畅、二次打开走缓存可拖动。
    //   起不来（切不出分片等）回退实时桥。
    if chain10 && vqa_core::bridge::is_local(url) {
        match vqa_core::bridge_par::ParBridge::start(&ffmpeg, &davs2, url, 26, audio_lead) {
            Ok(pb) => {
                let reason = format!("并行转码桥（{}，{}）", pb.mode(), pb.progress());
                let url = pb.url();
                return Launch::Bridged {
                    url,
                    reason,
                    auto_scale: 0,
                    bridge: Box::new(pb),
                };
            }
            Err(e) => {
                vqa_core::config::log_line(
                    "bridge-decide",
                    &format!("并行桥起不来，回退实时桥：{e}"),
                );
            }
        }
    }
    match vqa_core::bridge::RealtimeBridge::start_scaled(
        &ffmpeg,
        &davs2,
        url,
        chain10,
        26,
        nic_ip,
        audio_lead,
        initial_scale,
        if avs3 { Some("libuavs3d") } else { None },
        initial_scale > 0 && (avs3 || q.is_none()),
    ) {
        Ok(b) => {
            let reason = if h264_8k {
                format!("8K H.264→{initial_scale} / 25fps 自动转码桥（{}）", b.mode)
            } else if initial_scale > 0 {
                format!("AVS3 8K→{initial_scale} / 25fps 自动转码桥（{}）", b.mode)
            } else {
                format!("AVS 源自动转码桥（{}）", b.mode)
            };
            let url = b.url();
            Launch::Bridged {
                url,
                reason,
                auto_scale: initial_scale,
                bridge: Box::new(b),
            }
        }
        Err(e) => Launch::Failed(format!("转码桥失败：{e}")),
    }
}

#[tauri::command]
pub fn player_stop(st: State<PlayerState>) {
    kill_feed(&st);
    *st.playing_url.lock().unwrap() = String::new();
    *st.last_nic_ip.lock().unwrap() = String::new();
    *st.bridge.lock().unwrap() = None;
    *st.bridge_enc.lock().unwrap() = String::new();
    *st.bridge_note.lock().unwrap() = String::new();
    *st.bridge_url.lock().unwrap() = String::new();
    *st.bridge_since.lock().unwrap() = None;
    *st.preparing.lock().unwrap() = false;
    *st.loaded_at.lock().unwrap() = None;
    *st.video_retry_done.lock().unwrap() = false;
    *st.live_idle_since.lock().unwrap() = None;
    *st.live_retry_left.lock().unwrap() = 0;
    *st.live_had_picture.lock().unwrap() = false;
    *st.live_last_time.lock().unwrap() = 0;
    *st.live_stall_since.lock().unwrap() = None;
    *st.auto_scale_url.lock().unwrap() = String::new();
    *st.auto_scale_w.lock().unwrap() = 0;
    *st.bridge_dead_since.lock().unwrap() = None;
    *st.bridge_auto_restarts.lock().unwrap() = 0;
    *st.bridge_stderr_last.lock().unwrap() = String::new();
    *st.auto_decoder.lock().unwrap() = String::new();
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.stop();
    }
}

#[tauri::command]
pub fn player_pause_toggle(st: State<PlayerState>) -> Result<Option<bool>, String> {
    let mut replay_via_play = false;
    let mut paused_after = None;
    {
        let guard = st.mpv.lock().unwrap();
        let m = guard.as_ref().ok_or_else(|| "播放器尚未启动".to_string())?;
        // 不能只信 snap.paused：轮询/属性事件可能滞后。直接向 mpv 查真实状态。
        let idle = m
            .get_bool("idle-active")
            .unwrap_or_else(|_| m.snapshot().idle);
        if idle {
            // ★ 起播管线进行中（探测/桥转码）：不要动——此时把原始 AVS2
            //   喂给官方 mpv（无 AVS2 解码器）会秒退，UI 卡在"已播完"
            if *st.preparing.lock().unwrap() {
                return Ok(None);
            }
            // 重播：桥模式用桥产物（TS 文件/流地址），直放用原地址
            let bridge_u = st.bridge_url.lock().unwrap().clone();
            let url = if bridge_u.is_empty() {
                st.playing_url.lock().unwrap().clone()
            } else {
                bridge_u
            };
            if url.starts_with("udp://") {
                // ★ 已发完的 UDP 桥流重播没画面——重新走起播管线：
                //   第二次会命中缓存，直接播本地文件（秒开、可拖动）
                replay_via_play = true;
            } else if !url.is_empty() {
                m.loadfile(&url)?;
                paused_after = Some(false);
            } else {
                return Err("当前没有可播放的来源，请先选择文件或填组播地址".into());
            }
        } else {
            // cycle pause 以 mpv 真实状态为准，避免旧快照导致永远只能暂停。
            m.cycle_pause()
                .map_err(|e| format!("播放/暂停切换失败：{e}"))?;
            paused_after = m.get_bool("pause").ok();
            let _ = vqa_core::config::log_line(
                "player",
                &format!("pause-toggle 已执行：idle=false paused_after={paused_after:?}"),
            );
        }
    }
    if replay_via_play {
        let u2 = st.playing_url.lock().unwrap().clone();
        if !u2.is_empty() {
            player_play(st, u2, String::new(), false)?;
        }
        return Ok(None);
    }
    Ok(paused_after)
}

#[tauri::command]
pub fn player_seek(st: State<PlayerState>, fraction: f64) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.seek_percent(fraction * 100.0);
    }
}

/// 播放画面上的媒体信息浮层（Tab 触发）。
///
/// mpv 是原生子窗、盖在 WebView 之上，任何 HTML 浮层都画不出来；所以走
/// mpv 自己的 OSD —— `osd-overlay` 用 ASS 渲染到视频画面上，关掉只清这一层。
/// ★ mpv 0.42 把 `osd-overlay` 改成了位置参数：
///   (id, format, data, res_x, res_y, z, hidden, compute_bounds)，没有 name 了。
/// `lines` 为空 = 收起。
#[tauri::command]
pub fn player_osd(st: State<PlayerState>, lines: Vec<String>, res_x: u32, res_y: u32) {
    let Some(m) = st.mpv.lock().unwrap().clone() else {
        return;
    };
    if lines.is_empty() {
        let _ = m.command(serde_json::json!([
            "osd-overlay", 1, "none", "", 0, 720, 0, false, false
        ]));
        return;
    }
    let rx = res_x.max(320);
    let ry = res_y.max(180);
    // ★ 8K 视频上直接把 ASS 画到 7680×4320：一张 OSD 纹理约 130MB，
    //   每 2 秒重建一次，长时间播放会把显存/渲染线程拖垮，表现为画面
    //   逐帧落后、最后黑场。这里统一压到 1080p 坐标空间，mpv 再按视频
    //   尺寸缩放显示，字号/边距按比例计算，观感不变但开销降 16 倍。
    let scale = (1920.0 / rx as f64)
        .min(1080.0 / ry as f64)
        .min(1.0);
    let rx = ((rx as f64 * scale).round() as u32).max(320);
    let ry = ((ry as f64 * scale).round() as u32).max(180);
    // 底板：mpv 的 OSD 用「背景框」样式，半透明黑，文字白色
    for (k, v) in [
        ("osd-border-style", serde_json::json!("background-box")),
        ("osd-back-color", serde_json::json!("#C8000000")),
        ("osd-border-size", serde_json::json!(1)),
        ("osd-shadow-offset", serde_json::json!(0)),
    ] {
        let _ = m.set_prop(k, v);
    }
    let text = build_info_osd(&lines, rx, ry);
    let r = m.command(serde_json::json!([
        "osd-overlay",
        1,
        "ass-events",
        text,
        rx,
        ry,
        2,
        false,
        false
    ]));
    let _ = vqa_core::config::log_line(
        "osd",
        &format!(
            "osd-overlay lines={} res={rx}x{ry} -> {}\n  {}",
            lines.len(),
            match &r {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("err: {e}"),
            },
            lines.join(" ⏎ ").chars().take(300).collect::<String>()
        ),
    );
}

/// mpv 的 osd-overlay 只吃「纯文本 + ASS 覆写标签」，不接受 ASS 文件头
/// （实测给整份 ASS 会把 Script Info/Style 原样当文字画出来）。
/// 头部标签负责左上角定位、字体、字号。
fn build_info_osd(lines: &[String], rx: u32, ry: u32) -> String {
    let fs = (ry as f32 / 27.0).round().max(12.0);
    let margin = (rx as f32 / 45.0).round().max(12.0);
    let escape = |s: &str| {
        s.replace('\\', "/")
            .replace('{', "（")
            .replace('}', "）")
    };
    let body = lines
        .iter()
        .map(|l| escape(l))
        .collect::<Vec<_>>()
        .join("\\N");
    format!(
        "{{\\an7\\pos({margin},{margin})\\fnMicrosoft YaHei\\fs{fs}\\bord1\\shad0}}{body}"
    )
}

#[tauri::command]
pub fn player_set_volume(st: State<PlayerState>, volume: f64) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.set_volume(volume);
    }
}

#[tauri::command]
pub fn player_set_mute(st: State<PlayerState>, mute: bool) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.set_mute(mute);
    }
}

#[tauri::command]
pub fn player_set_speed(st: State<PlayerState>, speed: f64) {
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.set_speed(speed);
    }
}

/// 循环播放开关。mpv 的 `loop-file` 属性在 loadfile 之间保持，
/// 但仍在 player_play 里补发一次——避免重启播放器后被重置。
#[tauri::command]
pub fn player_set_loop(st: State<PlayerState>, on: bool) {
    *st.loop_file.lock().unwrap() = on;
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.set_prop(
            "loop-file",
            serde_json::Value::String(if on { "inf" } else { "no" }.into()),
        );
    }
}

/// 列多节目 TS 的节目（播放器「节目」下拉）。本地文件与网络源都支持，
/// 只读 PAT/PMT，超时就不列。
#[tauri::command]
pub fn player_programs(url: String, nic_ip: String) -> Vec<vqa_core::probe::ProgInfo> {
    vqa_core::probe::probe_programs(&url, 3.0, &nic_ip)
}

/// 切节目：mpv 的 TS 解复用只暴露第一个节目的轨道（本版无 `--program` 选项，
/// 运行时改 vid/aid 会把音频丢掉），所以这里用 ffmpeg `-map 0:p:N -c copy` 抽
/// 出选中节目，经管道喂给 mpv（`fd://0`）。代价是重开一次播放（本地文件从头、
/// 组播重新入组），换来音画都正确的节目切换。
#[tauri::command]
pub fn player_set_program(
    st: State<PlayerState>,
    url: String,
    program: i64,
    nic_ip: String,
) -> Result<(), String> {
    if program < 1 {
        return Err("节目序号无效".into());
    }
    if st.bridge.lock().unwrap().is_some() {
        return Err("桥转码中不支持切节目（请先停止）".into());
    }
    let ff = vqa_core::tools::find_ffmpeg().ok_or("未找到 ffmpeg.exe")?;
    let mpv_exe = vqa_core::tools::find_mpv().ok_or("未找到 mpv.exe（bin/mpv/mpv.exe）")?;
    let wid = st.child.lock().unwrap().as_ref().map(|c| c.hwnd());

    // 收掉旧的播放器与抽流进程，避免两条管道抢读同一个组播源
    if let Some(old) = st.mpv.lock().unwrap().take() {
        drop(old);
    }
    if let Some(mut c) = st.feed.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }

    let mut args: Vec<String> = vec!["-hide_banner".into(), "-v".into(), "error".into()];
    if !nic_ip.is_empty() && !vqa_core::bridge::is_local(&url) {
        args.push("-localaddr".into());
        args.push(nic_ip);
    }
    args.extend([
        "-i".into(),
        url.clone(),
        "-map".into(),
        format!("0:p:{program}"),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "mpegts".into(),
        "-".into(),
    ]);

    let (mpv, ff_child) = vqa_core::player::Mpv::launch_feed(&mpv_exe, wid, &ff, &args)?;
    // 循环播放是跨 loadfile 的开关：新起的播放器要再下发一次，否则切节目就丢了
    if *st.loop_file.lock().unwrap() {
        let _ = mpv.set_prop("loop-file", serde_json::Value::String("inf".into()));
    }
    *st.feed.lock().unwrap() = Some(ff_child);
    *st.mpv.lock().unwrap() = Some(mpv);
    st.set_hint_for(format!("正在切到节目 {program}（重新抽流，约 1-2 秒）"), 6);
    crate::events::ev("player", &format!("切节目 {program}（抽流模式）"));
    Ok(())
}

/// 导出分析报告（HTML/CSV）：走系统保存对话框，返回落盘路径。
#[tauri::command]
pub fn report_export(
    app: tauri::AppHandle,
    name: String,
    content: String,
    ext: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (desc, filter) = match ext.as_str() {
        "csv" => ("CSV 表格", "csv"),
        _ => ("HTML 报告", "html"),
    };
    let dest = app
        .dialog()
        .file()
        .add_filter(desc, &[filter])
        .set_file_name(&name)
        .blocking_save_file()
        .map(|p| p.to_string())
        .ok_or_else(|| "已取消".to_string())?;
    std::fs::write(&dest, content).map_err(|e| format!("写文件失败：{e}"))?;
    Ok(Some(dest))
}

/// 编码器 → 人话（播放器页"桥格式"条用）
fn enc_label(enc: &str) -> String {
    match enc {
        "h264_nvenc" => "H.264 / NVENC 硬编".into(),
        "h264_qsv" => "H.264 / QSV 硬编".into(),
        "h264_amf" => "H.264 / AMF 硬编".into(),
        "h264_mf" => "H.264 / MediaFoundation 编码".into(),
        "libx264" => "H.264 / libx264 软编".into(),
        "libopenh264" => "H.264 / OpenH264 软编".into(),
        "" => "H.264".into(),
        other => format!("H.264 / {other}"),
    }
}

#[tauri::command]
pub fn player_snapshot_save(st: State<PlayerState>) -> Result<String, String> {
    let dir = snap_dir();
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
        let dir = rec_dir();
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
                    crate::events::ev("bridge", &format!("转码中：{msg}"));
                    st.set_hint_for(msg, 300);
                }                Ok(ProbeMsg::Outcome(launch)) => {
                    // 起播管线结束（无论结果）：等待态解除
                    *st.preparing.lock().unwrap() = false;
                    match launch {
                    Launch::Direct => {
                        if let Some(m) = &mpv_arc {
                            let url = st.playing_url.lock().unwrap().clone();
                            if !url.is_empty() {
                                let q = st.probe_info.lock().unwrap().clone();
                                let opts = direct_load_opts(&url, q.as_ref());
                                let _ = m.loadfile_with_opts(&url, opts);
                                *st.loaded_at.lock().unwrap() = Some(std::time::Instant::now());
                                *st.video_retry_done.lock().unwrap() = false;
                                st.set_hint("已加载");
                                crate::events::ev("player", "直放（无需转码）");
                            }
                        }
                    }
                    Launch::Bridged { url, reason, auto_scale, bridge } => {
                        if st.playing_url.lock().unwrap().is_empty() {
                            // 用户已停止：起好的桥直接丢弃（Drop 会 kill 子进程）
                            drop(bridge);
                        } else if let Some(m) = &mpv_arc {
                            // ★ chain10 本地文件模式：url = 全片转码完成的 TS 文件
                            //   （start 同步等转码完成后返回），loadfile 即完整播放，
                            //   可拖动、零丢包零卡顿。直播源 url 仍是流地址。
                            // ★ 预转模式（url 是回环 UDP）：此刻流里还没有数据，
                            //   mpv 打开 UDP 读到 0 字节就判"无法识别格式"并放弃，
                            //   之后再发数据也不会重试——所以预转期间不 loadfile，
                            //   等全片转完再由 player_snapshot 的 2.4 段加载。
                            let preload = bridge.progress().starts_with("预转")
                                || bridge.progress().starts_with("准备中");
                            if preload {
                                st.set_hint_for(
                                    format!("{} · 首片转完即播放", bridge.progress()),
                                    300,
                                );
                            } else {
                                let _ = m.loadfile(&url);
                                *st.bridge_loaded.lock().unwrap() = true;
                                let _ = m.show_text(&reason, 4000);
                            }
                            crate::events::ev(
                                "bridge",
                                &format!("桥就绪 → {reason}{}", if preload { "（预转中）" } else { "" }),
                            );
                            *st.bridge_note.lock().unwrap() = reason;
                            *st.bridge_url.lock().unwrap() = url.clone();
                            *st.bridge_since.lock().unwrap() = Some(std::time::Instant::now());
                            *st.bridge.lock().unwrap() = Some(bridge);
                            // 正常探测到 8K 时也在这里起 4K 桥；同样记住降级宽度，
                            // 桥进程中途退出后 watchdog 才能按同一宽度自动重启。
                            if auto_scale > 0 {
                                *st.auto_scale_url.lock().unwrap() =
                                    st.playing_url.lock().unwrap().clone();
                                *st.auto_scale_w.lock().unwrap() = auto_scale;
                                *st.bridge_dead_since.lock().unwrap() = None;
                            }
                            // 记录本次桥用的编码器（决定"转到什么"展示）。
                            // pick_h264_encoder 内部有进程级缓存，起桥时已算过，
                            // 这里取的是同一个结果，几乎零成本。
                            let enc = vqa_core::tools::find_ffmpeg()
                                .map(|f| vqa_core::bridge::pick_h264_encoder(&f))
                                .unwrap_or_default();
                            *st.bridge_enc.lock().unwrap() = enc;
                        } else {
                            drop(bridge);
                        }
                    }
                    Launch::Failed(msg) => {
                        crate::events::ev("player-err", &format!("起播失败：{msg}"));
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
                                    let q = st.probe_info.lock().unwrap().clone();
                                    let opts = direct_load_opts(&url, q.as_ref());
                                    let _ = m.loadfile_with_opts(&url, opts);
                                    *st.loaded_at.lock().unwrap() =
                                        Some(std::time::Instant::now());
                                    *st.video_retry_done.lock().unwrap() = false;
                                }
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

    // 2.4) 预转进度 + 转完才加载 mpv。
    //      不显示进度的话用户对着黑屏干等，会以为卡死而重复点播——而每一次
    //      重复点播都会把整片转码作废重来，越点越等不到。
    //      转完这一刻才 loadfile：UDP 里已经有整片数据，mpv 一打开就有流。
    {
        let prog = st.bridge.lock().unwrap().as_ref().map(|b| b.progress());
        if let Some(prog) = prog {
            if prog.starts_with("预转") || prog.starts_with("准备中") {
                *st.preparing.lock().unwrap() = true;
                st.set_hint_for(format!("{prog} · 首片转完即播放"), 300);
            } else {
                *st.preparing.lock().unwrap() = false;
                if !*st.bridge_loaded.lock().unwrap() {
                    // ★ 原先这里硬等 600ms"让流稳定"，那是在没有发送门限的年代
                    //   补的；现在 send_loop 在 mpv 绑端口前只放 GO_PRIME（256KB）
                    //   探测字节、等 go() 才全速发，mpv 打开时流还停在开头，
                    //   不需要再白等这 0.6 秒。首片一拼完就 loadfile。
                    let u = st.bridge_url.lock().unwrap().clone();
                    if !u.is_empty() {
                        if let Some(m) = &mpv_arc {
                            let _ = m.loadfile(&u);
                            st.set_hint_for("首片就绪，开始播放", 30);
                            crate::events::ev("player", "首片就绪 → 加载播放");
                        }
                        *st.bridge_loaded.lock().unwrap() = true;
                        *st.bridge_since.lock().unwrap() = Some(std::time::Instant::now());
                    }
                }
            }
        }
    }

    // 2.4b) loadfile 之后约 700ms，mpv 才真正绑上 UDP 端口。这之后才放开发送，
    //       否则前面发的都被当成"没人接收"丢掉，mpv 从流中段起播（时长少 2 秒、
    //       音画起点错位）。顺手做幂等，每帧调一次也无妨。
    if *st.bridge_loaded.lock().unwrap() {
        let due = st
            .bridge_since
            .lock()
            .unwrap()
            .map(|t| t.elapsed().as_millis() > 700)
            .unwrap_or(false);
        if due {
            if let Some(b) = st.bridge.lock().unwrap().as_ref() {
                b.go();
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
        // 预转期间 UDP 里还没有数据：这时候 loadfile 只会让 mpv 读到 0 字节
        // 然后判"无法识别格式"，白白烧掉重试次数。
        let preloading = st
            .bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| {
                let p = b.progress();
                p.starts_with("预转") || p.starts_with("准备中")
            })
            .unwrap_or(false);
        // ★ 回环 TCP 的桥不需要这个兜底：TCP 下 mpv 打开流后会阻塞等字节，
        //   "暂时 idle" 是正常的等待，而不是"读到空流就永久放弃"。
        //   不排除掉的话这里会在起播途中把流重载一次（日志实测第二次
        //   Opening tcp 就在播到 20 多秒时出现），画面直接被拉回开头重来。
        let tcp_bridge = st
            .bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.url().starts_with("tcp://"))
            .unwrap_or(false);
        if !preloading && retry_left > 0 && !file_bridge && !tcp_bridge {
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
                        crate::events::ev(
                            "bridge",
                            &format!("桥未出流，重试 loadfile（剩余 {} 次）", retry_left - 1),
                        );
                        st.set_hint(format!("等待转码桥出流…（{}）", retry_left - 1));
                    }
                } else {
                    *st.bridge_retry_left.lock().unwrap() = 0;
                }
            }
        }
    }

    // 2.55) 桥出画面了 → 标记（流式桥播完 EOF 回 idle 不再被 2.6 误报）
    {
        let idle = mpv_arc.as_ref().map(|m| m.snapshot().idle).unwrap_or(true);
        if !idle {
            *st.bridge_had_picture.lock().unwrap() = true;
        }
    }

    // 2.57) 自动降级桥 watchdog：ffmpeg 桥进程退出后 mpv 会等在 TCP 上，
    //       界面看起来就是“播一会儿自己退了”。直播源先自动重启一次；
    //       第二次仍退出就明确报错，不再静默黑屏。
    {
        let playing = !st.playing_url.lock().unwrap().is_empty();
        let url = st.playing_url.lock().unwrap().clone();
        let low = url.to_ascii_lowercase();
        let live = low.starts_with("udp://")
            || low.starts_with("rtp://")
            || low.starts_with("rtsp://")
            || low.starts_with("rtmp://")
            || low.starts_with("rtmps://")
            || low.starts_with("srt://")
            || low.starts_with("mms://");
        let bridge_dead = if playing && live && !*st.preparing.lock().unwrap() {
            st.bridge
                .lock()
                .unwrap()
                .as_ref()
                .map(|b| !b.is_file() && b.mode() == "ffmpeg" && b.alive() == 0)
                .unwrap_or(false)
        } else {
            false
        };
        if bridge_dead {
            // 先把死桥的 stderr 留下来，再让 start_auto_bridge 接管旧句柄。
            let tail = {
                let b = st.bridge.lock().unwrap();
                b.as_ref().map(|b| b.stderr_tail()).unwrap_or_default()
            };
            if !tail.trim().is_empty() {
                *st.bridge_stderr_last.lock().unwrap() = tail.clone();
                vqa_core::config::log_line(
                    "bridge",
                    &format!("桥进程退出 stderr：{}", tail.trim()),
                );
            }
            let since = *st.bridge_dead_since.lock().unwrap();
            let due = since
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(3))
                .unwrap_or(false);
            if due {
                let scale = *st.auto_scale_w.lock().unwrap();
                if scale > 0 {
                    let restarts = *st.bridge_auto_restarts.lock().unwrap();
                    // 第一次按原宽度重启；还退出就把 4K 降到 1920，再退到
                    // 1280。8K 源在弱机上 4K 实时编码不一定撑得住，降一档
                    // 比直接显示“反复退出、已停止播放”更有用。
                    let next_scale = if restarts == 0 {
                        scale
                    } else if scale > 1920 {
                        1920
                    } else if scale > 1280 {
                        1280
                    } else {
                        scale
                    };
                    if restarts < 2 {
                        *st.bridge_auto_restarts.lock().unwrap() = restarts + 1;
                        *st.bridge_dead_since.lock().unwrap() = None;
                        *st.auto_scale_w.lock().unwrap() = next_scale;
                        let nic = st.last_nic_ip.lock().unwrap().clone();
                        // 先收掉已经死掉的桥，避免两座桥同时占同一个端口/解码器
                        *st.bridge.lock().unwrap() = None;
                        crate::events::ev(
                            "bridge",
                            &format!(
                                "转码桥进程退出，按 {next_scale} 宽自动重启（第 {} 次）",
                                restarts + 1
                            ),
                        );
                        if let Err(e) = start_auto_bridge(&st, &url, &nic, next_scale) {
                            *st.err.lock().unwrap() =
                                Some(format!("转码桥重启失败：{e}"));
                        }
                    } else {
                        *st.bridge.lock().unwrap() = None;
                        let stderr = st.bridge_stderr_last.lock().unwrap().clone();
                        *st.err.lock().unwrap() = Some(if stderr.trim().is_empty() {
                            "转码桥进程反复退出，已停止播放。请检查源或降低输出宽度后重试"
                                .into()
                        } else {
                            format!(
                                "转码桥进程反复退出，已停止播放：{}",
                                stderr.trim()
                            )
                        });
                    }
                } else {
                    *st.bridge.lock().unwrap() = None;
                    *st.err.lock().unwrap() = Some(
                        "转码桥进程反复退出，已停止播放。请检查源或降低输出宽度后重试"
                            .into(),
                    );
                }
            } else if since.is_none() {
                *st.bridge_dead_since.lock().unwrap() = Some(std::time::Instant::now());
            }
        } else {
            *st.bridge_dead_since.lock().unwrap() = None;
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
        let had_picture = *st.bridge_had_picture.lock().unwrap();
        let retry_left = *st.bridge_retry_left.lock().unwrap();
        // 预转期间本来就"没有画面"（还没开始发流），不能按 2 分钟判死
        let preloading = st
            .bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| {
                let p = b.progress();
                p.starts_with("预转") || p.starts_with("准备中")
            })
            .unwrap_or(false);
        if retry_left == 0 && !file_bridge && !had_picture && !preloading {
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
        crate::events::ev(
            "mpv",
            &format!(
                "mpv 进程退出（播放中={}）",
                !st.playing_url.lock().unwrap().is_empty()
            ),
        );
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

    // 4.2) 有音无画首帧看门狗：8K 直播首开偶尔只认到音频轨，音频已经出声、
    //      video-params 宽高仍为 0。这里等 2 秒自动重载一次（等价于用户把手
    //      动切高清再切回 8K），不用用户自己发现再操作。
    if let (Some(s), Some(m)) = (snap.as_ref(), mpv_arc.as_ref()) {
        let bridge_on = !st.bridge_note.lock().unwrap().is_empty();
        let playing = !st.playing_url.lock().unwrap().is_empty();
        if playing && !s.idle && !bridge_on && s.width == 0 && !s.audio_codec.is_empty() {
            let retried = *st.video_retry_done.lock().unwrap();
            let due = st
                .loaded_at
                .lock()
                .unwrap()
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(2))
                .unwrap_or(false);
            if !retried && due {
                let url = st.playing_url.lock().unwrap().clone();
                if !url.is_empty() && !vqa_core::bridge::is_local(&url) {
                    let q = st.probe_info.lock().unwrap().clone();
                    let opts = direct_load_opts(&url, q.as_ref());
                    let _ = m.loadfile_with_opts(&url, opts);
                    *st.video_retry_done.lock().unwrap() = true;
                    *st.loaded_at.lock().unwrap() = Some(std::time::Instant::now());
                    crate::events::ev(
                        "player",
                        &format!("首帧有音无画，自动重载一次（audio={}）", s.audio_codec),
                    );
                    st.set_hint_for("正在重新获取视频轨…", 30);
                }
            }
        } else if s.width > 0 {
            *st.loaded_at.lock().unwrap() = None;
        }
    }

    // 4.3) 直播断流看门狗：8K 直播偶发丢包/源端瞬断时 mpv 会回 idle，
    //      视频子窗随即变黑，但界面还停在"播放中"。出过画面后再回 idle
    //      且持续 3 秒，自动重载一次；最多 3 次，避免源端长时间无数据时
    //      无限重连。文件播放正常 EOF 不走这条（时长 > 0）。
    if let (Some(s), Some(m)) = (snap.as_ref(), mpv_arc.as_ref()) {
        let bridge_on = !st.bridge_note.lock().unwrap().is_empty();
        let playing = !st.playing_url.lock().unwrap().is_empty();
        let url = st.playing_url.lock().unwrap().clone();
        let low = url.to_ascii_lowercase();
        let liveish = low.starts_with("udp://")
            || low.starts_with("rtp://")
            || low.starts_with("rtsp://")
            || low.starts_with("rtmp://")
            || low.starts_with("rtmps://")
            || low.starts_with("srt://")
            || low.starts_with("mms://")
            || (!vqa_core::bridge::is_local(&url) && s.duration_ms <= 0.0);
        if !s.idle && s.width > 0 {
            *st.live_had_picture.lock().unwrap() = true;
        }
        let had_picture = *st.live_had_picture.lock().unwrap();
        // 4.3a) 半开连接/keep-open 场景：mpv 不回 idle，但播放时间停在
        //        最后一帧不再推进。连续 3 秒无推进就按断流处理。
        if playing
            && !bridge_on
            && liveish
            && had_picture
            && !s.idle
            && !s.paused
            && !*st.preparing.lock().unwrap()
        {
            let t = s.time_ms.round() as i64;
            let mut last = st.live_last_time.lock().unwrap();
            let mut stall = st.live_stall_since.lock().unwrap();
            if t > *last {
                *last = t;
                *stall = None;
            } else if t > 0 {
                let start = stall.get_or_insert_with(std::time::Instant::now);
                if start.elapsed() >= std::time::Duration::from_secs(3)
                    && *st.live_retry_left.lock().unwrap() > 0
                {
                    *st.live_retry_left.lock().unwrap() -= 1;
                    *stall = None;
                    let q = st.probe_info.lock().unwrap().clone();
                    let opts = direct_load_opts(&url, q.as_ref());
                    let _ = m.loadfile_with_opts(&url, opts);
                    *st.loaded_at.lock().unwrap() = Some(std::time::Instant::now());
                    *st.video_retry_done.lock().unwrap() = false;
                    st.set_hint_for("直播画面停止推进，正在自动重连…", 8);
                    crate::events::ev(
                        "player",
                        &format!(
                            "直播 t={t}ms 停止推进，自动重连（剩余 {} 次）",
                            *st.live_retry_left.lock().unwrap()
                        ),
                    );
                }
            } else {
                *stall = None;
            }
        }
        // 4.3b) 8K 桥进程还活着、但源停了/不再出帧：mpv 时间轴会卡在最后一帧。
        //       直播 8K 只有自动降级桥这一条路径（auto_scale_w > 0），
        //       3 秒无推进就重启一次桥，避免“看着像自动退出/卡死”。
        if playing
            && bridge_on
            && liveish
            && had_picture
            && !s.idle
            && !s.paused
            && !*st.preparing.lock().unwrap()
            && *st.auto_scale_w.lock().unwrap() > 0
        {
            let t = s.time_ms.round() as i64;
            let mut last = st.live_last_time.lock().unwrap();
            let mut stall = st.live_stall_since.lock().unwrap();
            if t > *last {
                *last = t;
                *stall = None;
            } else if t > 0 {
                let start = stall.get_or_insert_with(std::time::Instant::now);
                if start.elapsed() >= std::time::Duration::from_secs(3)
                    && *st.bridge_auto_restarts.lock().unwrap() < 1
                {
                    *st.bridge_auto_restarts.lock().unwrap() += 1;
                    *stall = None;
                    let scale = *st.auto_scale_w.lock().unwrap();
                    let nic = st.last_nic_ip.lock().unwrap().clone();
                    drop(last);
                    drop(stall);
                    st.set_hint_for("8K 转码桥无输出，正在重启…", 8);
                    crate::events::ev(
                        "player",
                        &format!("8K 桥 t={t}ms 无推进，自动重启（宽度 {scale}）"),
                    );
                    if let Err(e) = start_auto_bridge(&st, &url, &nic, scale) {
                        *st.err.lock().unwrap() = Some(format!("8K 桥重启失败：{e}"));
                    }
                }
            } else {
                *stall = None;
            }
        }
        if playing
            && !bridge_on
            && liveish
            && had_picture
            && s.idle
            && !*st.preparing.lock().unwrap()
        {
            let mut since = st.live_idle_since.lock().unwrap();
            let start = since.get_or_insert_with(std::time::Instant::now);
            if start.elapsed() >= std::time::Duration::from_secs(3)
                && *st.live_retry_left.lock().unwrap() > 0
            {
                *st.live_retry_left.lock().unwrap() -= 1;
                *since = None;
                let q = st.probe_info.lock().unwrap().clone();
                let opts = direct_load_opts(&url, q.as_ref());
                let _ = m.loadfile_with_opts(&url, opts);
                *st.loaded_at.lock().unwrap() = Some(std::time::Instant::now());
                *st.video_retry_done.lock().unwrap() = false;
                st.set_hint_for("直播流中断，正在自动重连…", 8);
                crate::events::ev(
                    "player",
                    &format!(
                        "直播回 idle，自动重连（剩余 {} 次）",
                        *st.live_retry_left.lock().unwrap()
                    ),
                );
            }
        } else if !s.idle {
            *st.live_idle_since.lock().unwrap() = None;
        }
    }

    // 4.5) 诊断采样：每 5s 一条（avsync / 丢帧 / 码率 / 帧率），丢帧突增单独记事件
    if let Some(s) = snap.as_ref() {
        let playing_now = !st.playing_url.lock().unwrap().is_empty();
        if playing_now && !s.idle {
            let due = {
                let last = st.last_sample.lock().unwrap();
                last.map(|t| t.elapsed() >= std::time::Duration::from_secs(5))
                    .unwrap_or(true)
            };
            if due {
                *st.last_sample.lock().unwrap() = Some(std::time::Instant::now());
                let drop = s.render_drop.max(0) + s.decoder_drop.max(0);
                let prev = *st.last_drop.lock().unwrap();
                if drop > prev + 5 {
                    crate::events::ev(
                        "drops",
                        &format!(
                            "丢帧突增 {} → {}（+{}，t={:.1}s）",
                            prev,
                            drop,
                            drop - prev,
                            s.time_ms / 1000.0
                        ),
                    );
                }
                *st.last_drop.lock().unwrap() = drop;
                crate::events::sample(format!(
                    "t={:.1}s avsync={:+.1}ms 丢帧={}/{} {:.2}Mbps vf_fps={:.2} 硬解={} {}x{}",
                    s.time_ms / 1000.0,
                    s.avsync,
                    s.render_drop,
                    s.decoder_drop,
                    s.v_bitrate / 1e6,
                    s.vf_fps,
                    s.hwdec,
                    s.width,
                    s.height
                ));
            }
        }
    }
    let hint = st
        .hint
        .lock()
        .unwrap()
        .take()
        .filter(|(_, t, d)| t.elapsed() <= *d)
        .map(|(s, _, _)| s);
    // 桥转码的"目标格式"（前端在播放器页下方画一条 源 → 目标 的说明条）
    let bridge_mode_now = st.bridge.lock().unwrap().as_ref().map(|b| b.mode()).unwrap_or_default();
    let bridge_to = if bridge_mode_now.is_empty() {
        serde_json::Value::Null
    } else {
        let enc = st.bridge_enc.lock().unwrap().clone();
        // chain10 固定压到 25fps（davs2 软解 4K 10-bit 只有 ~41fps，
        // 不降帧会"播几秒停几秒"，见 bridge.rs 注释）；8K 自动降级
        // 单链桥也固定 25fps；分片桥保持源帧率
        let fps = if bridge_mode_now.contains("chain10")
            || *st.auto_scale_w.lock().unwrap() > 0
            || st.bridge_note.lock().unwrap().contains("25fps")
        {
            "25".to_string()
        } else {
            String::new()
        };
        serde_json::json!({
            "mode": bridge_mode_now,
            "enc": enc.clone(),
            "enc_label": enc_label(&enc),
            "pix": "yuv420p",
            "fps": fps,
            "w": snap.as_ref().map(|s| s.width).unwrap_or(0),
            "h": snap.as_ref().map(|s| s.height).unwrap_or(0),
            "audio": "AAC 96k",
            "container": "MPEG-TS",
        })
    };
    serde_json::json!({
        "playing": !st.playing_url.lock().unwrap().is_empty(),
        "err": *st.err.lock().unwrap(),
        "hint": hint,
        "loop": *st.loop_file.lock().unwrap(),
        "bridge_to": bridge_to,
        "bridge_note": *st.bridge_note.lock().unwrap(),
        "bridge_stderr_last": *st.bridge_stderr_last.lock().unwrap(),
        "auto_decoder": *st.auto_decoder.lock().unwrap(),
        "bridge_enc": *st.bridge_enc.lock().unwrap(),
        "bridge": st.bridge.lock().unwrap().as_ref().map(|b| {
            serde_json::json!({
                "mode": b.mode(),
                "progress": b.progress(),
                "alive": b.alive(),
                "stderr_tail": b.stderr_tail(),
            })
        }),
        "recording": *st.recording.lock().unwrap(),
        "probe": st.probe_info.lock().unwrap().as_ref().map(probe_json),
        "preparing": *st.preparing.lock().unwrap(),
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
        "fps": q.video.framerate,
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

// ── 输出目录（截图 / 录像）：设置里改过就用设置，否则回落系统默认 ──

fn default_snap_dir() -> PathBuf {
    dirs_pictures().join("vqa_snapshots")
}
fn default_rec_dir() -> PathBuf {
    dirs_videos().join("vqa_recordings")
}

fn custom_dir(key: &str) -> Option<PathBuf> {
    let s: String = vqa_core::config::shared_get_or(key, String::new());
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(PathBuf::from(t))
    }
}

fn snap_dir() -> PathBuf {
    custom_dir("out_snap_dir").unwrap_or_else(default_snap_dir)
}
fn rec_dir() -> PathBuf {
    custom_dir("out_rec_dir").unwrap_or_else(default_rec_dir)
}

/// 播放器设置（只属于播放器页；分析页有自己的 analysis_settings_*）。
#[tauri::command]
pub fn player_settings_get() -> serde_json::Value {
    serde_json::json!({
        "snap_dir": snap_dir().to_string_lossy(),
        "rec_dir": rec_dir().to_string_lossy(),
        "snap_default": default_snap_dir().to_string_lossy(),
        "rec_default": default_rec_dir().to_string_lossy(),
        "snap_custom": custom_dir("out_snap_dir").map(|p| p.to_string_lossy().to_string()),
        "rec_custom": custom_dir("out_rec_dir").map(|p| p.to_string_lossy().to_string()),
        "default_loop": vqa_core::config::shared_get_or("player.default_loop", false),
        "default_force_bridge": vqa_core::config::shared_get_or("player.default_force_bridge", false),
        "default_nic": vqa_core::config::shared_get_or("player.default_nic", String::new()),
    })
}

/// 保存播放器设置。目录留空 = 用默认；任一项校验失败整体不改。
#[tauri::command]
pub fn player_settings_set(
    snap_dir: String,
    rec_dir: String,
    default_loop: bool,
    default_force_bridge: bool,
    default_nic: String,
    st: State<PlayerState>,
) -> Result<serde_json::Value, String> {
    let snap = if snap_dir.trim().is_empty() {
        None
    } else {
        Some(check_dir("截图目录", &snap_dir)?)
    };
    let rec = if rec_dir.trim().is_empty() {
        None
    } else {
        Some(check_dir("录像目录", &rec_dir)?)
    };
    vqa_core::config::shared_set(
        "out_snap_dir",
        serde_json::json!(snap.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
    )
    .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set(
        "out_rec_dir",
        serde_json::json!(rec.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
    )
    .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set("player.default_loop", serde_json::json!(default_loop))
        .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set(
        "player.default_force_bridge",
        serde_json::json!(default_force_bridge),
    )
    .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set("player.default_nic", serde_json::json!(default_nic.trim()))
        .map_err(|e| format!("写配置失败：{e}"))?;
    // 默认循环立刻作用于当前播放器（mpv 未起时只记状态）
    *st.loop_file.lock().unwrap() = default_loop;
    if let Some(m) = st.mpv.lock().unwrap().as_ref() {
        let _ = m.set_prop(
            "loop-file",
            serde_json::Value::String(if default_loop { "inf" } else { "no" }.into()),
        );
    }
    Ok(player_settings_get())
}

/// 分析报告默认输出目录；未设置则每次导出时弹保存对话框。
fn report_dir() -> Option<PathBuf> {
    custom_dir("analysis.report_dir")
}

/// 视频分析设置（只属于分析页）。
#[tauri::command]
pub fn analysis_settings_get() -> serde_json::Value {
    serde_json::json!({
        "report_dir": report_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "report_note": "留空 = 每次导出时弹保存对话框",
    })
}

/// 保存视频分析设置（目录留空 = 每次弹保存对话框）。
#[tauri::command]
pub fn analysis_settings_set(report_dir: String) -> Result<serde_json::Value, String> {
    let saved = if report_dir.trim().is_empty() {
        String::new()
    } else {
        check_dir("报告目录", &report_dir)?
            .to_string_lossy()
            .to_string()
    };
    vqa_core::config::shared_set("analysis.report_dir", serde_json::json!(saved))
        .map_err(|e| format!("写配置失败：{e}"))?;
    Ok(analysis_settings_get())
}

/// 校验单个目录：必须是绝对路径、能建出来、能写进去。
fn check_dir(label: &str, raw: &str) -> Result<PathBuf, String> {
    let t = raw.trim();
    let p = PathBuf::from(t);
    if !p.is_absolute() {
        return Err(format!("{label}要填绝对路径（如 D:\\vqa-output\\snap）：{t}"));
    }
    std::fs::create_dir_all(&p)
        .map_err(|e| format!("{label}建不出来 {}：{e}", p.display()))?;
    let probe = p.join(".vqa_write_test");
    std::fs::write(&probe, b"ok")
        .map_err(|e| format!("{label}不可写 {}：{e}", p.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(p)
}

/// 选择文件夹（设置页"浏览"按钮）。
#[tauri::command]
pub fn browse_folder(app: tauri::AppHandle, title: String, start: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut d = app.dialog().file().set_title(&title);
    if !start.trim().is_empty() {
        d = d.set_directory(start.trim());
    }
    d.blocking_pick_folder().map(|p| p.to_string())
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
                crate::events::ev("analysis", &format!("实时分析启动 {cleaned}"));
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e:#}");
                crate::events::ev("analysis-err", &format!("实时分析启动失败：{msg}"));
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
        crate::events::ev("analysis", &format!("开始扫描 {cleaned}"));
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
            crate::events::ev("analysis", "实时分析停止");
        }
        _ => {
            if let Some(f) = ast.stop_flag.lock().unwrap().as_ref() {
                f.store(true, Ordering::Relaxed);
            }
            ast.log_push("扫描中断");
            crate::events::ev("analysis", "扫描被用户中断");
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
                        "扫描完成：{} 包 / {:.3}s / {:.2} Mbps",
                        res.packets,
                        res.duration_pcr(),
                        res.bitrate_avg() / 1e6
                    ));
                    let path = res.path.clone();
                    *ast.pkt.lock().unwrap() =
                        vqa_core::tsfile::PacketFile::open(std::path::Path::new(&path)).ok();
                    crate::events::ev(
                        "analysis",
                        &format!(
                            "扫描完成 {} 包 / {:.3}s / {:.2} Mbps",
                            res.packets,
                            res.duration_pcr(),
                            res.bitrate_avg() / 1e6
                        ),
                    );
                    *ast.result.lock().unwrap() = Some(Arc::new(res));
                    *mode = 0;
                }
                Ok(Err(e)) => {
                    crate::events::ev("analysis-err", &format!("扫描失败：{e}"));
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
        // 分析页设置里配了报告目录 → 直接落盘（带时间戳，不覆盖上次）
        None => match report_dir() {
            Some(d) => {
                let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let _ = std::fs::create_dir_all(&d);
                d.join(format!("TS流质量检测报告_{ts}.md"))
                    .to_string_lossy()
                    .to_string()
            }
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
        },
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
