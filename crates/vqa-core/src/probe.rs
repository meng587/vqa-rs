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
    let probe_url = crate::bridge::udp_input_url(url);
    cmd.args(["-i", probe_url.as_str(), "-show_streams", "-show_format", "-of", "json"]);
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

/// AVS 编码族：ffprobe 对裸 AVS3 常误判成 avs2（两者起始码相近），
/// 需要用 libuavs3d 实际读一次流头来确认，否则 8K AVS3 会被错送进 davs2。
fn is_avs_family(codec: &str) -> bool {
    let c = codec.to_ascii_lowercase();
    c.starts_with("avs") || c == "davs2" || c == "uavs3d" || c.contains("cavs")
}

/// 强制 libuavs3d 读流头；成功返回 (宽, 高, 像素格式, 源帧率)。
/// 不输出、不消耗视频帧，ffmpeg 打印完流头即退出，8K 也能在数秒内返回。
fn confirm_avs3(url: &str, localaddr: &str) -> Option<(i64, i64, String, String)> {
    let ffmpeg = crate::tools::find_ffmpeg()?;
    let mut cmd = ffmpeg::hidden(&ffmpeg);
    cmd.args(["-hide_banner", "-v", "info"]);
    if !localaddr.is_empty() && looks_net(url) {
        cmd.args(["-localaddr", localaddr]);
    }
    let probe_url = crate::bridge::udp_input_url(url);
    cmd.args([
        "-probesize", "8388608",
        "-analyzeduration", "2000000",
        "-c:v", "libuavs3d",
        "-i", probe_url.as_str(),
        // 必须真解出一帧：AVS2 被强制交给 libuavs3d 时只打一行假流头，
        // frame=0 且立刻退出；AVS3 才能解出 >=1 帧。
        "-t", "0.5",
        "-frames:v", "1",
        "-f", "null", "-",
    ]);
    let out = run_limited(cmd, Duration::from_secs(6))?;
    let text = String::from_utf8_lossy(&out.stderr);
    let decoded = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("frame="))
        .filter_map(|v| v.split_whitespace().next())
        .filter_map(|v| v.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    if decoded == 0 {
        return None;
    }
    let line = text
        .lines()
        .find(|l| l.contains("Video: avs3") && l.contains("libuavs3d"))?;
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    let mut width = 0i64;
    let mut height = 0i64;
    let mut pix = String::new();
    for p in &parts {
        if let Some((a, b)) = p.split_once('x') {
            if let (Ok(w), Ok(h)) = (a.parse::<i64>(), b.split_whitespace().next().unwrap_or("").parse::<i64>()) {
                if w > 0 && h > 0 {
                    width = w;
                    height = h;
                }
            }
        } else if p.starts_with("yuv") || p.starts_with("nv12") || p.starts_with("p010") {
            pix = p.to_string();
        }
    }
    if width <= 0 || height <= 0 {
        return None;
    }
    let fps = parts
        .iter()
        .find_map(|p| p.strip_suffix(" fps"))
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| *v > 1.0)
        .map(|v| format!("{v}/1"))
        .unwrap_or_default();
    Some((width, height, pix, fps))
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
    // 512KB/0.52s 对 4K 10-bit 不够（只认出 codec、尺寸全空）→ 8MB。
    // analyzeduration 从 4s 压到 1s：8MB 在 120Mbps 下约 0.53s 就够，
    // 原来固定等 4s 会把起播拖到 4~5 秒；探测失败时用下面的短超时兜底。
    // start_time 一并取：音频比画面早到时，桥要按这段差值裁音频头（音画同步）。
    let probe_url = crate::bridge::udp_input_url(url);
    cmd.args([
        "-probesize", "8388608",
        "-analyzeduration", "1000000",
        "-i", probe_url.as_str(),
        "-show_entries",
        "stream=codec_name,width,height,pix_fmt,bit_rate,avg_frame_rate,start_time:format=bit_rate,start_time",
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
            let mut q = QuickProbe {
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
                video_start: num(st.get("start_time")),
                format_start: num(fmt.get("start_time")),
            };
            // ★ ffprobe 的 AVS3/AVS2 自动识别不可靠：裸 AVS3 会被报成
            //   avs2 + 2528x1159，随后被错送进 vkit_davs2。这里让 libuavs3d
            //   读一次流头，确认是 AVS3 就纠正 codec/宽高/帧率。
            if is_avs_family(&q.video.codec) && !q.video.codec.to_ascii_lowercase().contains("avs3") {
                if let Some((w, h, pix, fps)) = confirm_avs3(url, localaddr) {
                    q.video.codec = "avs3".into();
                    q.video.width = w;
                    q.video.height = h;
                    if !pix.is_empty() {
                        q.video.pix_fmt = pix;
                    }
                    if !fps.is_empty() {
                        q.video.framerate = fps;
                    }
                }
            }
            return Some(q);
        }
    }
    Some(QuickProbe {
        video: VideoStream::default(),
        bit_rate: cont_bit,
        video_start: 0.0,
        format_start: num(fmt.get("start_time")),
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct QuickProbe {
    pub video: VideoStream,
    pub bit_rate: i64,
    /// 视频首帧时间戳（秒，源时间轴）
    pub video_start: f64,
    /// 容器起始时间戳（秒）——通常等于最早流（多为音频）的首帧
    pub format_start: f64,
}

impl QuickProbe {
    /// 音轨相对视频"提前"了多少秒（>0 表示音频开头比画面早）。
    ///
    /// 抓流从半途开始、或录制起点落在音频先到时，TS 里普遍是
    /// `audio.start < video.start`。桥转码把视频时间轴从 0 重排（y4m 无时间戳），
    /// 音频却按源时间轴走，于是全程恒定错位——必须把这段音频头裁掉才能对齐。
    pub fn audio_lead(&self) -> f64 {
        if self.format_start > 0.0 && self.video_start > self.format_start {
            (self.video_start - self.format_start).max(0.0)
        } else {
            0.0
        }
    }
}

/// 多节目 TS 的一个节目（播放器「节目」下拉用）
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProgInfo {
    /// 节目号（节目表里 program_id，显示用）
    pub number: i64,
    /// 抽流用的节目序号（ffmpeg `-map 0:p:N` 的 N，从 1 开始，按 PAT 顺序）
    pub ord: i64,
    pub pmt_pid: i64,
    /// 「视频 h264」「音频 aac」这类简述，给下拉做副标题
    pub streams: Vec<String>,
}

/// 列节目：只读 PAT/PMT，不探流细节。单节目/非 TS 返回空。
pub fn probe_programs(url: &str, timeout: f64, localaddr: &str) -> Vec<ProgInfo> {
    if url.is_empty() {
        return Vec::new();
    }
    let Ok(exe) = ffprobe_path() else {
        return Vec::new();
    };
    let mut cmd = ffmpeg::hidden(&exe);
    cmd.args(["-hide_banner", "-v", "error"]);
    if !localaddr.is_empty() && looks_net(url) {
        cmd.args(["-localaddr", localaddr]);
    }
    let probe_url = crate::bridge::udp_input_url(url);
    cmd.args([
        "-probesize", "4194304",
        "-analyzeduration", "2000000",
        "-i", probe_url.as_str(),
        "-show_programs",
        "-show_entries", "program=program_id,program_num,pmt_pid:program_stream=codec_type,codec_name",
        "-of", "json",
    ]);
    let Some(out) = run_limited(cmd, Duration::from_secs_f64(timeout.max(1.0))) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(d) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (i, p) in d.get("programs").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().enumerate() {
        let number = {
            let id = num(p.get("program_id")) as i64;
            if id > 0 { id } else { num(p.get("program_num")) as i64 }
        };
        if number <= 0 {
            continue;
        }
        let streams = p
            .get("streams")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|s| {
                let kind = match safe(s.get("codec_type")).as_str() {
                    "video" => "视频",
                    "audio" => "音频",
                    "data" => "数据",
                    _ => "流",
                };
                let codec = safe(s.get("codec_name"));
                if codec.is_empty() {
                    kind.to_string()
                } else {
                    format!("{kind} {codec}")
                }
            })
            .collect::<Vec<_>>();
        rows.push(ProgInfo {
            number,
            ord: i as i64 + 1,
            pmt_pid: num(p.get("pmt_pid")) as i64,
            streams,
        });
    }
    rows
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
    let is_h264 = c.contains("h264") || c.contains("avc");
    let big_8k = q.video.width >= 7000 && q.video.height >= 4000;
    // 8K H.264 没有可用的硬解（NVDEC H.264 上限 4096 宽），直接交给 mpv
    // 会在 50fps 源上持续丢帧。必须在起播前转成 4K/25fps 桥流。
    if big_8k && (is_avs || is_h264) {
        return true;
    }
    if !is_avs {
        return false;
    }
    // ★ AVS2 4K 的快速探测经常只认出 codec、宽高为 0（avs1.ts 实测：
    //   codec=avs2 0x0 pix=""）。此时不能再按尺寸判直放——mpv 直接啃
    //   会退出。AVS 家族只要探不到尺寸，一律先走转码桥。
    if q.video.width <= 0 || q.video.height <= 0 {
        return true;
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

    #[test]
    fn bridge_required_for_8k_h264_but_not_4k_h264() {
        let mut q = QuickProbe::default();
        q.video.codec = "h264".into();
        q.video.width = 7680;
        q.video.height = 4320;
        q.video.pix_fmt = "yuv420p".into();
        assert!(needs_bridge(&Some(q)));

        let mut q4 = QuickProbe::default();
        q4.video.codec = "h264".into();
        q4.video.width = 3840;
        q4.video.height = 2160;
        q4.video.pix_fmt = "yuv420p".into();
        assert!(!needs_bridge(&Some(q4)));
    }

    #[test]
    fn bridge_not_required_for_8k_hevc() {
        let mut q = QuickProbe::default();
        q.video.codec = "hevc".into();
        q.video.width = 7680;
        q.video.height = 4320;
        q.video.pix_fmt = "yuv420p10le".into();
        assert!(!needs_bridge(&Some(q)));
    }

    #[test]
    fn bridge_required_for_avs2_with_unknown_dimensions() {
        let mut q = QuickProbe::default();
        q.video.codec = "avs2".into();
        q.video.width = 0;
        q.video.height = 0;
        q.video.pix_fmt = String::new();
        assert!(needs_bridge(&Some(q)));
    }
}
