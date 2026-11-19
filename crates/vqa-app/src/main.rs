//! 视听质量检测工作站（VQA Workbench）—— Rust 版主入口。
//!
//! 启动顺序（对齐 Python 版 app_qt_new.py 的纪律）：
//! 1. panic 钩子 → crash/ 目录留物证（原生崩溃/逻辑 panic 都有迹可查）
//! 2. 单实例命名互斥体：已在运行则把已有窗口拉到前台后退出
//! 3. eframe 启动，首帧创建视频子窗口（mpv --wid 只能给一次）

#![cfg(windows)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod pages;
mod theme;
mod video_child;
mod widgets;

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

/// 单实例：命名互斥体。已在运行 → 找已有窗口拉前台 → 返回 false。
fn acquire_single_instance() -> bool {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;
    unsafe {
        let r = CreateMutexW(None, false, w!("vqa-workbench-single-instance"));
        if r.is_err() {
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            bring_existing_to_front();
            return false;
        }
        // 互斥体句柄故意不关——进程存活期间持有
        std::mem::forget(r);
    }
    true
}

fn bring_existing_to_front() {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    unsafe {
        let title = windows::core::w!("视听质量检测工作站");
        if let Ok(hwnd) = FindWindowW(None, title) {
            if hwnd != HWND::default() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
                return;
            }
        }
        // 兜底：按窗口标题枚举
        unsafe extern "system" fn cb(hwnd: HWND, _: LPARAM) -> BOOL {
            let mut buf = [0u16; 64];
            let n = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                hwnd, &mut buf,
            );
            let text = String::from_utf16_lossy(&buf[..n as usize]);
            if text.starts_with("视听质量检测工作站") {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
                return BOOL(0);
            }
            BOOL(1)
        }
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }
}

fn main() -> eframe::Result<()> {
    install_panic_hook();
    if !acquire_single_instance() {
        return Ok(());
    }
    vqa_core::config::cleanup_logs(
        &vqa_core::config::log_dir(),
        3,
        3,
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_title("视听质量检测工作站"),
        ..Default::default()
    };
    eframe::run_native(
        "vqa-workbench",
        options,
        Box::new(|cc| Ok(Box::new(app::VqaApp::new(cc)))),
    )
}
