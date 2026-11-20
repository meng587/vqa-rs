//! 视听质量检测工作站 —— Tauri/Web UI 版主入口。
//!
//! 纪律与 egui 版（crates/vqa-app/src/main.rs）一致：
//! panic 钩子留物证、单实例拉前台、日志 3 天/3MB 清理。
//! 视频子窗在 setup 阶段创建（WebView2 已就位，同父兄弟后建者在上）。

// 始终按 GUI 子系统编译：不允许出现"打开程序附带一个 cmd 黑窗"，
// 调试版的排障信息走 crash_dir 的 panic 文件与 config::log_line 落盘的日志。
#![windows_subsystem = "windows"]

mod commands;
mod ai_cmds;
mod alarm_cmds;
mod burst_cmds;
mod capture_cmds;
mod compliance_cmds;
mod diag;
mod easytier_cmds;
mod events;
mod mediainfo;
mod mpv_host;
mod state;
mod sync_cmds;
mod preview;
mod xcode_cmds;

use tauri::Manager;

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = vqa_core::config::crash_dir();
        let _ = std::fs::create_dir_all(&dir);
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let path = dir.join(format!("panic_{ts}.log"));
        let _ = std::fs::write(&path, format!("{info}\n"));
        let _ = vqa_core::config::log_line("panic", &format!("{info}"));
        default(info);
    }));
}

/// 串流地址清理（egui 版 widgets::clean_stream_url 的移植）。
pub fn clean_stream_url(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || std::path::Path::new(s).exists() {
        return s.to_string();
    }
    s.chars()
        .map(|c| match c {
            '：' => ':',
            '＠' => '@',
            '，' => ',',
            '。' => '.',
            '？' => '?',
            '＝' => '=',
            '＆' => '&',
            '／' => '/',
            '＋' => '+',
            '％' => '%',
            '　' => ' ',
            _ => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect()
}

/// 把主窗口拉回显示器工作区。
///
/// 起因：窗口按 1360×860 逻辑尺寸开，在 1080p + 任务栏的机器上 outer 高 899，
/// 若被系统放到屏幕底部，最后 32px（贯通整宽的状态栏）会被任务栏盖住（实测）。
/// 这里只在越界时收：先按工作区缩，再平移进来，不改变正常机器上的尺寸。
fn clamp_into_work_area(win: &tauri::WebviewWindow) {
    let Ok(Some(mon)) = win.current_monitor() else {
        return;
    };
    let wa = mon.work_area();
    let Ok(size) = win.outer_size() else {
        return;
    };
    let Ok(pos) = win.outer_position() else {
        return;
    };
    let wa_x = wa.position.x;
    let wa_y = wa.position.y;
    let wa_r = wa_x + wa.size.width as i32;
    let wa_b = wa_y + wa.size.height as i32;

    let mut w = size.width as i32;
    let mut h = size.height as i32;
    if w > wa.size.width as i32 {
        w = wa.size.width as i32;
    }
    if h > wa.size.height as i32 {
        h = wa.size.height as i32;
    }
    if w != size.width as i32 || h != size.height as i32 {
        let _ = win.set_size(tauri::PhysicalSize::new(w as u32, h as u32));
    }

    let mut x = pos.x;
    let mut y = pos.y;
    if x + w > wa_r {
        x = wa_r - w;
    }
    if y + h > wa_b {
        y = wa_b - h;
    }
    if x < wa_x {
        x = wa_x;
    }
    if y < wa_y {
        y = wa_y;
    }
    if x != pos.x || y != pos.y {
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

fn main() {
    let _ = diag::APP_START.set(std::time::Instant::now());
    install_panic_hook();
    vqa_core::config::cleanup_logs(&vqa_core::config::log_dir(), 3, 3);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            use tauri::Manager as _;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state::PlayerState::new())
        .manage(state::AnalysisState::new())
        .manage(state::ComplianceState::new())
        .manage(state::XcodeState::new())
        .manage(state::BurstState::new())
            .manage(state::AvState::new())
            .manage(state::ConsistState::new())
            .manage(capture_cmds::CaptureState::new())
            .manage(easytier_cmds::EasytierState::new())
            .manage(preview::Preview::new())
        .setup(|app| {
            // 视频子窗：父 = 主窗口 HWND（WebView2 的同父兄弟，后建者 z 序在上）
            if let Some(win) = app.get_webview_window("main") {
                clamp_into_work_area(&win);
                // 视频子窗拖放要回传前端，先存一份 AppHandle
                mpv_host::set_app_handle(app.handle().clone());
                if let Ok(hwnd) = win.hwnd() {
                    let child = mpv_host::VideoChild::create(hwnd.0 as isize);
                    *app.state::<state::PlayerState>()
                        .child
                        .lock()
                        .unwrap() = child;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 通用
            commands::app_version,
            commands::list_nics,
            commands::source_history,
            commands::push_source_history,
            commands::browse_media_file,
            commands::browse_folder,
            commands::player_settings_get,
            commands::player_settings_set,
            commands::analysis_settings_get,
            commands::analysis_settings_set,
            commands::tool_paths,
            commands::app_paths,
            commands::reveal_path,
            commands::thresholds_get,
            commands::thresholds_set,
            // 诊断日志
            commands::diag_collect,
            commands::diag_export,
            commands::media_info,
            commands::diag_upload,
            commands::diag_has_token,
            // 播放器
            commands::place_video,
            commands::player_prewarm,
            commands::player_play,
            commands::player_adaptive_scale,
            commands::player_stop,
            commands::player_pause_toggle,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_set_mute,
            commands::player_set_speed,
            commands::player_set_loop,
            commands::player_osd,
            commands::player_programs,
            commands::player_set_program,
            commands::report_export,
            commands::report_export_b64,
            commands::player_snapshot,
            commands::player_snapshot_save,
            commands::player_record_toggle,
            // 分析
            commands::analysis_start,
            commands::analysis_stop,
            commands::analysis_progress,
            commands::analysis_log,
            commands::analysis_result,
            commands::packet_rows,
            commands::export_report_md,
            // 合规检测
            compliance_cmds::compliance_check,
            compliance_cmds::compliance_live_start,
            compliance_cmds::compliance_live_snapshot,
            compliance_cmds::compliance_live_stop,
            compliance_cmds::compliance_export_md,
            // 直播转码
            xcode_cmds::xcode_encoders,
            xcode_cmds::xcode_preview,
            xcode_cmds::xcode_start,
            xcode_cmds::xcode_snapshot,
            xcode_cmds::xcode_stop,
            // 微突发检测
            burst_cmds::burst_start,
            burst_cmds::burst_snapshot,
            burst_cmds::burst_stop,
            sync_cmds::avsync_start,
            sync_cmds::avsync_progress,
            sync_cmds::avsync_stop,
            sync_cmds::consist_start,
            sync_cmds::consist_progress,
            sync_cmds::consist_stop,
            // 抓包分析
            capture_cmds::capture_start,
            capture_cmds::capture_stop,
            capture_cmds::capture_status,
            // AI 编排与模型管理
            ai_cmds::ai_config_get,
            ai_cmds::ai_config_set,
            ai_cmds::ai_plan,
            ai_cmds::ai_test,
            ai_cmds::ai_tools,
            ai_cmds::ai_model_list,
            ai_cmds::ai_model_check,
            ai_cmds::ai_model_import,
            ai_cmds::ai_model_remove,
            ai_cmds::ai_report_export,
            // EasyTier 组网
            easytier_cmds::easytier_config_get,
            easytier_cmds::easytier_config_save,
            easytier_cmds::easytier_start,
            easytier_cmds::easytier_stop,
            easytier_cmds::easytier_status,
            easytier_cmds::easytier_restart_elevated,
            sync_cmds::channels_get,
            sync_cmds::channels_save,
            sync_cmds::ui_pref_get,
            sync_cmds::ui_pref_set,
            sync_cmds::preview_start,
            sync_cmds::preview_stop,
            sync_cmds::preview_frame,
            // 告警处理 / 证据录像 / 存储设置
            alarm_cmds::alarm_list,
            alarm_cmds::alarm_record,
            alarm_cmds::alarm_update,
            alarm_cmds::alarm_capture,
            alarm_cmds::alarm_open,
            alarm_cmds::alarm_settings_get,
            alarm_cmds::alarm_settings_set,
            alarm_cmds::alarm_detect_log,
            alarm_cmds::alarm_export,
        ])
        .build(tauri::generate_context!())
        .expect("tauri 应用构建失败")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                shutdown(app);
            }
        });
}

/// 退出清理（对齐 egui 版 on_exit）：mpv quit+kill、桥 Drop kill ffmpeg、
/// 停扫描线程/实时会话、销毁视频子窗。
fn shutdown(app: &tauri::AppHandle) {
    use tauri::Manager;
    let pst = app.state::<state::PlayerState>();
    *pst.playing_url.lock().unwrap() = String::new();
    if let Some(m) = pst.mpv.lock().unwrap().take() {
        let _ = m.stop();
        drop(m); // Drop: quit → kill
    }
    *pst.bridge.lock().unwrap() = None; // Drop: kill ffmpeg
    if let Some(c) = pst.child.lock().unwrap().take() {
        c.destroy();
    }
    let ast = app.state::<state::AnalysisState>();
    if let Some(f) = ast.stop_flag.lock().unwrap().as_ref() {
        f.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let live = ast.live.lock().unwrap().take();
    if let Some(l) = live {
        l.stop();
    }
    drop(ast);
    // 抓包线程退出前先把会话停掉，保证 pcap/TS 都 flush 到磁盘。
    let cst = app.state::<capture_cmds::CaptureState>();
    if let Some(mut s) = cst.session.lock().unwrap().take() {
        s.stop();
    }
    drop(cst);
    // EasyTier 内核随工作站退出而停止，避免留下孤儿进程持续占用 TUN/RPC。
    let est = app.state::<easytier_cmds::EasytierState>();
    if let Some(mut c) = est.child.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    };
}
