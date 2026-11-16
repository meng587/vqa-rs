//! 视听质量检测工作站 —— Tauri/Web UI 版主入口。
//!
//! 纪律与 egui 版（crates/vqa-app/src/main.rs）一致：
//! panic 钩子留物证、单实例拉前台、日志 3 天/3MB 清理。
//! 视频子窗在 setup 阶段创建（WebView2 已就位，同父兄弟后建者在上）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod diag;
mod mpv_host;
mod state;

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
        .setup(|app| {
            // 视频子窗：父 = 主窗口 HWND（WebView2 的同父兄弟，后建者 z 序在上）
            if let Some(win) = app.get_webview_window("main") {
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
            commands::thresholds_get,
            commands::thresholds_set,
            // 诊断日志
            commands::diag_collect,
            commands::diag_export,
            commands::diag_upload,
            commands::diag_has_token,
            // 播放器
            commands::place_video,
            commands::player_play,
            commands::player_stop,
            commands::player_pause_toggle,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_set_mute,
            commands::player_set_speed,
            commands::player_set_loop,
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
}
