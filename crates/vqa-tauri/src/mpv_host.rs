//! 视频子窗口 —— mpv `--wid` 嵌入的 WS_CHILD 黑底窗口。
//!
//! ★ SetWindowPos 对**子窗口**用的是父窗口客户区相对坐标。WebView2 充满
//!   主窗口客户区，所以前端 `getBoundingClientRect() × devicePixelRatio`
//!   得到的物理像素恰好就是这套坐标系；千万不要 ClientToScreen（那是给
//!   顶层窗口的，会双重偏移——egui 版实测黑块跑到窗口中右部）。
//!
//! z 序：本窗口在 WebView2 子窗**之后**创建（Tauri setup 时 WebView2 已
//! 存在），同父兄弟后创建者在上，天然盖住 WebView2 的视频区。

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
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("vqa_video_tauri"),
                windows::core::w!(""),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN, // 保持"可见"——mpv vo 对
                // IsWindowVisible=false 的宿主窗可能拒绝渲染；隐藏用移出屏幕实现
                -20000, -20000, 100, 100,
                HWND(parent as *mut core::ffi::c_void),
                None, None, None,
            )
            .ok()?;
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
    unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wp, lp) }
}
