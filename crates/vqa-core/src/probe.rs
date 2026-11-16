//! ffprobe 结构化探测 —— `vkitcore/media/probe.py` 的移植。
//!
//! 两条路径（口径同 Python 版）：
//! - `probe`：完整探测（流参数/容器/码率），8s 超时，供媒体信息与 AVS 桥判定
//! - `probe_quick`：快速探测（仅 codec/尺寸/pix_fmt），8MB/4s 探测窗口，
//!   起播前的「4K 10-bit AVS」判定靠它，探不到**绝不拦起播**
//!
//! ★ `localaddr` 只允许加在**网络源**上（§31.47：加在本地文件上 ffprobe 直接
//!   报 Option not found 退出，探测全空 → 10-bit 判不出 → 不走桥 → mpv 崩）。
//!
//! 缓存：16 条 LRU，键 = `url|localaddr`（换网卡是收得到/收不到的区别）；
//! `peek()` 只查不探——对组播再起一次 ffprobe 等于额外加入组播组，
//! 可能把先到的接收者饿死。

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ffmpeg;

const NET_PREFIXES: [&str; 11] = [
    "udp://", "rtp://", "rtsp://", "rtmp://", "rtmps://", "http://", "https://",
    "srt://", "mms://", "ftp://", "tcp://",
];

pub fn looks_net(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    NET_PREFIXES.iter().any(|p| u.starts_with(p))
}

// ---------------------------------------------------------------- 结果结构

#[derive(Debug, Clone, Default, Serialize)]
pub struct VideoStream {
    pub codec: String,
    pub codec_long: String,
    pub profile: String,
    pub width: i64,
    pub height: i64,
    pub pix_fmt: String,
    pub color_primaries: String,
    pub color_transfer: String,
    pub color_space: String,
    pub color_range: String,
    pub field_order: String,
    pub level: i64,
    pub bit_rate: i64,
    pub framerate: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioStream {
    pub codec: String,
    pub codec_long: String,
    pub profile: String,
    pub sample_rate: i64,
    pub channels: i64,
    pub channel_layout: String,
    pub bit_rate: i64,
    pub sample_fmt: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeResult {
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
    pub container: String,
    pub container_long: String,
    pub duration: f64,
    pub bit_rate: i64,
}

impl ProbeResult {
    pub fn is_empty(&self) -> bool {
        self.video.is_none() && self.audio.is_none()
    }
}

fn num(v: Option<&serde_json::Value>) -> f64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn safe(v: Option<&serde_json::Value>) -> String {
    match v {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

// ---------------------------------------------------------------- 进程执行

/// 运行命令并限时收割输出。超时杀进程，返回 None。
pub fn run_limited(
    mut cmd: Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    // 两侧管道都排空，防 64KB 缓冲塞满后子进程写阻塞。
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let th_out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = out_pipe {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let th_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = err_pipe {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Some(std::process::Output {
                    status,
                    stdout: th_out.join().unwrap_or_default(),
                    stderr: th_err.join().unwrap_or_default(),
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------- 探测

fn ffprobe_path() -> anyhow::Result<std::path::PathBuf> {
    crate::tools::find_ffprobe()
        .ok_or_else(|| anyhow::anyhow!("ffprobe 未找到（放在 ffmpeg 同目录即可）"))
}

/// 完整探测。失败返回 `ProbeResult::default()`（不抛）。
/// `err_out`：可选，失败时 append stderr 尾巴（诊断链路）。
pub fn probe(
    url: &str,
    timeout: f64,
    localaddr: &str,
    mut err_out: Option<&mut Vec<String>>,
) -> ProbeResult {
    let empty = ProbeResult::default();
    if url.is_empty() {
        return empty;
    }
    let Ok(exe) = ffprobe_path() else {
        if let Some(e) = err_out.as_deref_mut() {
            e.push("找不到 ffprobe".into());
        }
        return empty;
    };
    let mut cmd = ffmpeg::hidden(&exe);
    cmd.args(["-hide_banner", "-v", "error"]);
    if !localaddr.is_empty() && looks_net(url) {
        cmd.args(["-localaddr", localaddr]);
    }
    cmd.args(["-i", url, "-show_streams", "-show_format", "-of", "json"]);
    let Some(out) = run_limited(cmd, Duration::from_secs_f64(timeout)) else {
        if let Some(e) = err_out.as_deref_mut() {
            e.push(format!("ffprobe 超时 {timeout}s（等不到流参数——长 GOP/无流？）"));
        }
        return empty;
    };
    if !out.status.success() {
        if let Some(e) = err_out.as_deref_mut() {
            let tail = stderr_tail(&out.stderr, 3);
            e.push(format!("ffprobe 退出码 {:?}：{}", out.status.code(), tail));
        }
        return empty;
    }
    let d: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            if let Some(e) = err_out.as_deref_mut() {
                e.push("ffprobe 输出不是合法 JSON（rc=0）".into());
            }
            return empty;
        }
    };
    let parsed = parse(&d);
    if let Some(e) = err_out.as_deref_mut() {
        if parsed.is_empty() {
            let tail = stderr_tail(&out.stderr, 3);
            if tail.is_empty() {
                e.push("ffprobe 无输出（组播一包未到？）".into());
            } else {
                e.push(tail);
            }
        }
    }
    parsed
}

/// 快速探测（仅 codec/尺寸/pix_fmt/bit_rate）。失败返回 None；**绝不拦起播**。
pub fn probe_quick(url: &str, timeout: f64, localaddr: &str) -> Option<QuickProbe> {
    if url.is_empty() {
        return None;
    }
    let exe = ffprobe_path().ok()?;
    let mut cmd = ffmpeg::hidden(&exe);
    cmd.args(["-hide_banner", "-v", "error"]);
    if !localaddr.is_empty() && looks_net(url) {
        cmd.args(["-localaddr", localaddr]);
    }
    // 512KB/0.52s 对 4K 10-bit 不够（只认出 codec、尺寸全空）→ 8MB/4s。
    cmd.args([
        "-probesize", "8388608",
        "-analyzeduration", "4000000",
        "-i", url,
        "-show_entries",
        "stream=codec_name,width,height,pix_fmt,bit_rate,avg_frame_rate:format=bit_rate",
        "-of", "json",
    ]);
    let out = run_limited(cmd, Duration::from_secs_f64(timeout))?;
    if !out.status.success() {
        return None;
    }
    let d: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let fmt = d.get("format").cloned().unwrap_or_default();
    let cont_bit = num(fmt.get("bit_rate")) as i64;
    for st in d.get("streams")?.as_array()? {
        let is_video = safe(st.get("codec_type")) == "video"
            || st.get("width").is_some();
        if is_video {
            return Some(QuickProbe {
                video: VideoStream {
                    codec: safe(st.get("codec_name")),
                    width: num(st.get("width")) as i64,
                    height: num(st.get("height")) as i64,
                    pix_fmt: safe(st.get("pix_fmt")),
                    bit_rate: num(st.get("bit_rate")) as i64,
                    // 源帧率（avg_frame_rate = "50/1"）——播放器页"桥转码"条要显示
                    // "什么格式 → 什么格式"，输出侧 fps 常被桥改造（chain10 固定 25），
                    // 没有源 fps 就说不清这一项。
                    framerate: safe(st.get("avg_frame_rate")),
                    ..Default::default()
                },
                bit_rate: if cont_bit > 0 { cont_bit } else { 0 },
            });
        }
    }
    Some(QuickProbe {
        video: VideoStream::default(),
        bit_rate: cont_bit,
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct QuickProbe {
    pub video: VideoStream,
    pub bit_rate: i64,
}

/// ffprobe JSON → ProbeResult。失败字段一律空，不抛。
fn parse(d: &serde_json::Value) -> ProbeResult {
    let mut out = ProbeResult::default();
    let fmt = d.get("format").cloned().unwrap_or_default();
    out.container = safe(fmt.get("format_name"));
    out.container_long = safe(fmt.get("format_long_name"));
    out.duration = num(fmt.get("duration"));
    out.bit_rate = num(fmt.get("bit_rate")) as i64;

    for s in d
        .get("streams")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let t = safe(s.get("codec_type"));
        if t == "video" && out.video.is_none() {
            out.video = Some(VideoStream {
                codec: safe(s.get("codec_name")),
                codec_long: safe(s.get("codec_long_name")),
                profile: safe(s.get("profile")),
                width: num(s.get("width")) as i64,
                height: num(s.get("height")) as i64,
                pix_fmt: safe(s.get("pix_fmt")),
                color_primaries: safe(s.get("color_primaries")),
                color_transfer: safe(s.get("color_transfer")),
                color_space: safe(s.get("color_space")),
                color_range: safe(s.get("color_range")),
                field_order: safe(s.get("field_order")),
                level: num(s.get("level")) as i64,
                bit_rate: num(s.get("bit_rate")) as i64,
                framerate: safe(if s.get("r_frame_rate").is_some_and(|v| !safe(Some(v)).is_empty()) {
                    s.get("r_frame_rate")
                } else {
                    s.get("avg_frame_rate")
                }),
            });
        } else if t == "audio" && out.audio.is_none() {
            out.audio = Some(AudioStream {
                codec: safe(s.get("codec_name")),
                codec_long: safe(s.get("codec_long_name")),
                profile: safe(s.get("profile")),
                sample_rate: num(s.get("sample_rate")) as i64,
                channels: num(s.get("channels")) as i64,
                channel_layout: safe(s.get("channel_layout")),
                bit_rate: num(s.get("bit_rate")) as i64,
                sample_fmt: safe(s.get("sample_fmt")),
            });
        }
    }
    out
}

fn stderr_tail(stderr: &[u8], n: usize) -> String {
    let txt = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = txt.trim().lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" / ")
}

// ---------------------------------------------------------------- 缓存

/// 同 URL 不重复 ffprobe。16 条 LRU；`peek` 只查不探（防组播饿死）。
pub struct ProbeCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    map: HashMap<String, ProbeResult>,
    order: Vec<String>,
}

const CACHE_MAX: usize = 16;

impl ProbeCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                map: HashMap::new(),
                order: Vec::new(),
            }),
        }
    }

    fn key(url: &str, localaddr: &str) -> String {
        if localaddr.is_empty() {
            url.to_string()
        } else {
            format!("{url}|{localaddr}")
        }
    }

    pub fn peek(&self, url: &str, localaddr: &str) -> Option<ProbeResult> {
        let g = self.inner.lock().ok()?;
        g.map.get(&Self::key(url, localaddr)).cloned()
    }

    /// 拿缓存；没有就探测一次塞进去。
    pub fn get(&self, url: &str, localaddr: &str) -> ProbeResult {
        let k = Self::key(url, localaddr);
        if let Ok(mut g) = self.inner.lock() {
            if let Some(v) = g.map.get(&k).cloned() {
                g.order.retain(|x| x != &k);
                g.order.push(k);
                return v;
            }
        }
        let result = probe(url, 8.0, localaddr, None);
        if let Ok(mut g) = self.inner.lock() {
            g.map.insert(k.clone(), result.clone());
            g.order.push(k);
            while g.order.len() > CACHE_MAX {
                let old = g.order.remove(0);
                g.map.remove(&old);
            }
        }
        result
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.map.clear();
            g.order.clear();
        }
    }
}

impl Default for ProbeCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 判断视频是否需要 AVS 转码桥：AVS 系 codec 且（10-bit 或 4K+）。
/// 口径来自 Python 版 `_avs_bridge_needed`：pix_fmt 是 8/10-bit 唯一判据。
pub fn needs_bridge(q: &Option<QuickProbe>) -> bool {
    let Some(q) = q else { return false };
    let c = q.video.codec.to_ascii_lowercase();
    let is_avs = c.starts_with("avs")
        || c == "davs2"
        || c == "uavs3d"
        || c.contains("avs2")
        || c.contains("avs3")
        || c.contains("cavs");
    if !is_avs {
        return false;
    }
    let ten_bit = q.video.pix_fmt.contains("10");
    let big = q.video.height >= 1440;
    ten_bit || big
}

#[allow(dead_code)]
fn _unused_path_helper(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_net_prefixes() {
        assert!(looks_net("udp://@239.1.1.1:5000"));
        assert!(looks_net("UDP://x"));
        assert!(!looks_net(r"D:\video\clip.ts"));
        assert!(!looks_net(""));
    }

    #[test]
    fn parse_handles_missing_fields() {
        let d = serde_json::json!({});
        let p = parse(&d);
        assert!(p.is_empty());
    }
}
