//! 视频子窗口 —— mpv `--wid` 嵌入的 WS_CHILD 黑底窗口（移植自 vkitplayer）。
//!
//! ★ SetWindowPos 对**子窗口**用的是父窗口客户区相对坐标——egui 的
//!   screen_rect 原点恰好也是客户区左上角，直接乘 ppp 即可；
//!   千万不要 ClientToScreen（那是给顶层窗口的），会双重偏移
//!   （实测：黑块整个跑到窗口中右部，盖住控制条和播放列表）。

use egui::Rect;

unsafe extern "system" fn video_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wp, lp) }
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
                lpszClassName: windows::core::w!("vqa_video"),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc); // 重复注册失败没关系
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("vqa_video"),
                windows::core::w!(""),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
                0, 0, 100, 100,
                HWND(parent as *mut core::ffi::c_void),
                None, None, None,
            )
            .ok()?;
            Some(VideoChild { hwnd: hwnd.0 as isize, last: None })
        }
    }

    /// 把子窗口对齐到视频区（egui 逻辑点 → 物理像素）。
    /// `visible=false`（待机没播东西）时隐藏子窗口，让 egui 的提示文字露出。
    pub fn place(&mut self, rect: Rect, ppp: f32, visible: bool) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, ShowWindow, SWP_NOZORDER, SW_HIDE, SW_SHOW,
        };
        unsafe {
            let x = (rect.min.x * ppp) as i32;
            let y = (rect.min.y * ppp) as i32;
            let w = (rect.width() * ppp) as i32;
            let h = (rect.height() * ppp) as i32;
            let cur = [x, y, w, h, visible as i32];
            if self.last != Some(cur) {
                let hwnd = HWND(self.hwnd as *mut core::ffi::c_void);
                let _ = SetWindowPos(hwnd, HWND::default(), x, y, w.max(1), h.max(1), SWP_NOZORDER);
                let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
                self.last = Some(cur);
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
