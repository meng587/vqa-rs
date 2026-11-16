//! 视频子窗口 —— mpv `--wid` 嵌入的 WS_CHILD 黑底窗口。
//!
//! ★ SetWindowPos 对**子窗口**用的是父窗口客户区相对坐标。WebView2 充满
//!   主窗口客户区，所以前端 `getBoundingClientRect() × devicePixelRatio`
//!   得到的物理像素恰好就是这套坐标系；千万不要 ClientToScreen（那是给
//!   顶层窗口的，会双重偏移——egui 版实测黑块跑到窗口中右部）。
//!
//! z 序：本窗口在 WebView2 子窗**之后**创建（Tauri setup 时 WebView2 已
//! 存在），同父兄弟后创建者在上，天然盖住 WebView2 的视频区。
//!
//! 拖放：视频区被本子窗盖住时，WebView2 收不到文件拖放（Tauri 的拖放事件
//! 挂在 WebView2 上）。所以本窗自己 `DragAcceptFiles`，在 wndproc 里接
//! WM_DROPFILES，把路径 emit 成 `video://drop` 给前端；前端和 WebView2 的
//! 拖放走同一条处理路径。

use tauri::Emitter;

/// Windows 侧的拖放路径出口：wndproc 是静态函数，拿不到 AppHandle，用全局存一份。
static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP.set(app);
}

// shell32 / kernel32 的几个函数直接用 FFI 声明，省掉额外 feature 依赖
#[link(name = "shell32")]
extern "system" {
    fn DragAcceptFiles(hwnd: *mut core::ffi::c_void, accept: i32);
    fn DragFinish(hdrop: *mut core::ffi::c_void);
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut core::ffi::c_void;
    fn GlobalFree(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn GlobalLock(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn GlobalUnlock(h: *mut core::ffi::c_void) -> i32;
    fn GlobalSize(h: *mut core::ffi::c_void) -> usize;
}

/// 自己解析 HDROP：它就是一个全局内存块，头 20 字节是 DROPFILES。
/// （不调 DragQueryFileW：实测在这里拿到的是 0 条，直接读内存更可靠）
///
/// DROPFILES: pFiles(u32) + pt(8) + fNC(bool) + fWide(bool) = 20 字节，
/// 之后是若干路径字符串，宽字符以 \0\0 结尾，列表再以一个 \0 收尾。
unsafe fn parse_hdrop(hdrop: *mut core::ffi::c_void) -> Vec<String> {
    let mut out = Vec::new();
    if hdrop.is_null() {
        return out;
    }
    let base = GlobalLock(hdrop) as *const u8;
    if base.is_null() {
        return out;
    }
    let size = GlobalSize(hdrop);
    let read_u32 = |off: usize| -> u32 {
        if off + 4 <= size {
            std::ptr::read_unaligned(base.add(off) as *const u32)
        } else {
            0
        }
    };
    let pfiles = read_u32(0) as usize;
    let wide = read_u32(16) != 0;
    let mut off = if pfiles >= 20 && pfiles < size { pfiles } else { 20 };
    while off < size {
        if wide {
            let mut end = off;
            while end + 2 <= size {
                let ch = std::ptr::read_unaligned(base.add(end) as *const u16);
                if ch == 0 {
                    break;
                }
                end += 2;
            }
            if end == off {
                break; // 空串 = 列表结束
            }
            let units: Vec<u16> = (0..(end - off) / 2)
                .map(|i| std::ptr::read_unaligned(base.add(off + i * 2) as *const u16))
                .collect();
            if let Ok(s) = String::from_utf16(&units) {
                out.push(s);
            }
            off = end + 2;
        } else {
            let mut end = off;
            while end < size && *base.add(end) != 0 {
                end += 1;
            }
            if end == off {
                break;
            }
            let bytes: Vec<u8> = (off..end).map(|i| *base.add(i)).collect();
            out.push(String::from_utf8_lossy(&bytes).to_string());
            off = end + 1;
        }
    }
    let _ = GlobalUnlock(hdrop);
    out
}

pub struct VideoChild {
    hwnd: isize,
    last: Option<[i32; 5]>,
}

impl VideoChild {
    pub fn create(parent: isize) -> Option<VideoChild> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{GetStockObject, BLACK_BRUSH, HBRUSH};
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(video_wnd_proc),
                lpszClassName: windows::core::w!("vqa_video_tauri"),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc); // 重复注册失败没关系
            // WS_EX_TRANSPARENT：本窗不做命中测试，鼠标/拖放落到后面的 WebView2 兄弟窗上。
            // 起因：视频区被本子窗盖住时，文件拖放事件全被子窗吃掉，WebView2 收不到
            // （Tauri 的拖放事件挂在 WebView2 上）。视频区本身 pointer-events:none，
            // 让过去也不会误触；mpv 只负责渲染，不需要接收鼠标。
            // WS_EX_NOACTIVATE：点画面不会把键盘焦点从 WebView2 抢走，
            // 否则播放中按 Tab（媒体信息浮层）会被 mpv 窗口吃掉。
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0),
                windows::core::w!("vqa_video_tauri"),
                windows::core::w!(""),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN, // 保持"可见"——mpv vo 对
                // IsWindowVisible=false 的宿主窗可能拒绝渲染；隐藏用移出屏幕实现
                -20000, -20000, 100, 100,
                HWND(parent as *mut core::ffi::c_void),
                None, None, None,
            )
            .ok()?;
            // 让本窗（含 mpv 子窗）接收文件拖放，绕开"WebView2 被盖住收不到 drop"
            DragAcceptFiles(hwnd.0, 1);
            Some(VideoChild { hwnd: hwnd.0 as isize, last: None })
        }
    }

    /// 对齐到视频区（物理像素，父客户区相对）。
    /// `visible=false`（待机没播东西）时把子窗**移出屏幕**而不是 SW_HIDE——
    /// mpv vo 对 IsWindowVisible=false 的宿主窗可能拒绝渲染（实测黑屏）。
    /// ★ 显示时必须 HWND_TOP 提 z 序：WebView2 是 setup 之后异步初始化的，
    ///   比本子窗后创建、天然盖在上面，不提顶视频永远被压在下面（实测）。
    pub fn place(&mut self, x: i32, y: i32, w: i32, h: i32, visible: bool) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        };
        unsafe {
            let cur = [x, y, w, h, visible as i32];
            let hwnd = HWND(self.hwnd as *mut core::ffi::c_void);
            if self.last != Some(cur) {
                let (px, py) = if visible { (x, y) } else { (-20000, -20000) };
                let _ = SetWindowPos(hwnd, HWND_TOP, px, py, w.max(1), h.max(1), SWP_NOZORDER);
                self.last = Some(cur);
            } else if visible {
                // 对抗 WebView2 的 z 序竞争：每轮幂等提顶（廉价、不激活）
                let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    pub fn destroy(&self) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
        unsafe {
            let _ = DestroyWindow(HWND(self.hwnd as *mut core::ffi::c_void));
        }
    }
}

unsafe extern "system" fn video_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    const WM_DROPFILES: u32 = 0x0233;
    if msg == WM_DROPFILES {
        let hdrop = wp.0 as *mut core::ffi::c_void;
        let paths = unsafe { parse_hdrop(hdrop) };
        unsafe { DragFinish(hdrop) };
        if !paths.is_empty() {
            let emitted = match APP.get() {
                Some(app) => app.emit("video://drop", paths.clone()).is_ok(),
                None => false,
            };
            let _ = vqa_core::config::log_line(
                "drop",
                &format!("WM_DROPFILES paths={paths:?} emitted={emitted}"),
            );
        } else {
            let _ = vqa_core::config::log_line("drop", "WM_DROPFILES 没解析出路径");
        }
        return LRESULT(0);
    }
    unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wp, lp) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在本进程内造一块标准 HDROP（DROPFILES 头 + 宽字符路径 + 双 null），
    /// 验证 parse_hdrop 能读出路径。真实拖放里 Explorer 给的就是这块结构，
    /// 只是句柄由系统在目标进程里准备好，测试里只能自己造。
    #[test]
    fn parse_hdrop_reads_wide_paths() {
        unsafe {
            let path = "D:\\vqa-rs\\testdata\\sample_720p.ts";
            let units: Vec<u16> = path.encode_utf16().collect();
            let bytes = 20 + (units.len() + 2) * 2;
            let h = GlobalAlloc(0x0042, bytes); // GMEM_MOVEABLE | GMEM_ZEROINIT
            assert!(!h.is_null(), "GlobalAlloc 失败");
            let base = GlobalLock(h) as *mut u8;
            assert!(!base.is_null(), "GlobalLock 失败");
            std::ptr::write_unaligned(base as *mut u32, 20u32); // pFiles
            std::ptr::write_unaligned(base.add(16) as *mut u32, 1u32); // fWide
            for (i, u) in units.iter().enumerate() {
                std::ptr::write_unaligned(base.add(20 + i * 2) as *mut u16, *u);
            }
            let _ = GlobalUnlock(h);
            let got = parse_hdrop(h);
            let _ = GlobalFree(h);
            assert_eq!(got, vec![path.to_string()], "解析出的路径不对：{got:?}");
        }
    }
}
