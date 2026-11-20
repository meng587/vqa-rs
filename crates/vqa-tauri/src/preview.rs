//! 检测时的画面预览：低码率 MJPEG 抓帧，前端按需轮询最新一帧。
//!
//! 每个页面（音画同步 / 一致性比对）各占一个槽位，互不影响；只在检测或手动预览
//! 期间起 ffmpeg，停止即杀进程，不会常驻。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 预览输出高度（宽度按源比例换算），够看清画面又不吃 CPU。
const PREV_H: u32 = 180;
/// 抓帧帧率：人眼可辨的运动，带宽占用可忽略。
const PREV_FPS: f64 = 4.0;

pub struct Slot {
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Slot {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            latest: Arc::new(Mutex::new(None)),
            child: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(mut c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.running.store(false, Ordering::Relaxed);
    }

    /// 重新起一路预览；同槽位已有任务时先停旧的。
    pub fn start(&self, src: &str, nic_ip: &str) -> Result<(), String> {
        let src = src.trim();
        if src.is_empty() {
            return Err("预览来源为空".into());
        }
        self.stop();
        // stop() 把 cancel 置 true，重开要复位
        self.cancel.store(false, Ordering::Relaxed);
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let ff: PathBuf =
            vqa_core::tools::find_ffmpeg().ok_or_else(|| "未找到 ffmpeg（请检查 bin 目录）".to_string())?;

        let mut cmd = vqa_core::ffmpeg::hidden(&ff);
        cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"])
            .args(vqa_core::netif::bind_args(src, nic_ip))
            .args(["-i", src])
            .args([
                "-an",
                "-vf",
                &format!("fps={PREV_FPS},scale=-2:{PREV_H}:flags=fast_bilinear"),
                "-f",
                "mjpeg",
                "-q:v",
                "7",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("预览启动失败: {e}"))?;
        let mut so = child.stdout.take().ok_or_else(|| "预览无输出".to_string())?;
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        self.running.store(true, Ordering::Relaxed);

        let running = self.running.clone();
        let cancel = self.cancel.clone();
        let latest = self.latest.clone();
        std::thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
            let mut chunk = vec![0u8; 8192];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                match so.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        // JPEG 边界：FFD8 开头、FFD9 结尾；一帧可能跨多个 chunk
                        loop {
                            let start = match buf.windows(2).position(|w| w == [0xFF, 0xD8]) {
                                Some(i) => i,
                                None => {
                                    if buf.len() > 64 {
                                        buf.drain(..buf.len() - 2);
                                    }
                                    break;
                                }
                            };
                            if start > 0 {
                                buf.drain(..start);
                            }
                            match buf.windows(2).position(|w| w == [0xFF, 0xD9]) {
                                Some(end) => {
                                    let frame = buf[..end + 2].to_vec();
                                    buf.drain(..end + 2);
                                    if let Ok(mut g) = latest.lock() {
                                        *g = Some(frame);
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            running.store(false, Ordering::Relaxed);
        });
        Ok(())
    }

    /// 取最新一帧的 data URL（没有帧时 None）。
    pub fn frame_data_url(&self) -> Option<String> {
        let g = self.latest.lock().ok()?;
        let bytes = g.as_ref()?;
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        Some(format!("data:image/jpeg;base64,{b64}"))
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self::new()
    }
}

/// 页面的预览槽位。一致性页要有基准/待比两路画面，各占一个槽。
pub struct Preview {
    av: Slot,
    consist_ref: Slot,
    consist_tgt: Slot,
}

impl Preview {
    pub fn new() -> Self {
        Self {
            av: Slot::new(),
            consist_ref: Slot::new(),
            consist_tgt: Slot::new(),
        }
    }

    pub fn slot(&self, owner: &str) -> Option<&Slot> {
        match owner {
            "av" => Some(&self.av),
            "consist-ref" => Some(&self.consist_ref),
            "consist" | "consist-tgt" => Some(&self.consist_tgt),
            _ => None,
        }
    }
}

impl Default for Preview {
    fn default() -> Self {
        Self::new()
    }
}

