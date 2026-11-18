//! 直播转码：把一路输入（文件 / udp / rtp / http）按指定编码参数转出到
//! 文件或组播地址，并把 ffmpeg 的实时统计（帧数、fps、输出码率、速度、丢帧）
//! 读出来给页面轮询。
//!
//! 与播放器里的「桥转码」分开：桥只为了解决播放兼容性问题，这里是运维侧的
//! 常态转码（CBR 码率、可选分辨率/帧率、音频直通或转 AAC）。

use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct XcodeCfg {
    pub input: String,
    pub output: String,
    /// auto / libopenh264 / h264_nvenc / h264_qsv / h264_amf / hevc_nvenc / libx264
    pub encoder: String,
    /// 目标视频码率 kbps
    pub vbr_kbps: u32,
    /// copy / aac / none
    pub audio: String,
    /// 本地文件是否按实时速率读（模拟直播）；网络流忽略
    pub realtime: bool,
    /// 可选缩放高度（-2:height）
    pub height: Option<u32>,
    /// 可选输出帧率
    pub fps: Option<f64>,
    /// 组播输入使用的本机网卡地址（空 = 自动路由）
    pub nic_ip: Option<String>,
    /// 输出 GOP（帧），0/空 = 25
    pub gop: Option<u32>,
    /// AAC 音频码率 kbps；copy/none 时忽略
    pub audio_kbps: Option<u32>,
    /// 输出封装：auto / mpegts / mp4；auto 按输出后缀或协议推导
    pub container: Option<String>,
    /// 组播输出包大小；默认 1316（MPEG-TS over UDP 的标准值）
    pub pkt_size: Option<u32>,
    /// 组播输出 TTL；0/空 = ffmpeg 默认
    pub ttl: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct XcodeStats {
    pub running: bool,
    /// 实际阶段：待机 / 启动中 / 运行中 / 已完成 / 已停止 / 失败
    pub stage: String,
    /// 实际使用的编码器（auto 解析后）
    pub encoder: String,
    pub frame: u64,
    pub fps: f64,
    pub kbps: f64,
    pub speed: f64,
    pub drop: u64,
    pub out_time: String,
    pub secs: f64,
    pub cmd: String,
    pub err: Option<String>,
    pub log: Vec<String>,
}

fn is_live_url(u: &str) -> bool {
    let l = u.to_ascii_lowercase();
    l.starts_with("udp://")
        || l.starts_with("rtp://")
        || l.starts_with("http://")
        || l.starts_with("https://")
        || l.starts_with("rtsp://")
        || l.starts_with("srt://")
}

fn is_udp_url(u: &str) -> bool {
    let l = u.to_ascii_lowercase();
    l.starts_with("udp://") || l.starts_with("rtp://")
}

/// 组输出参数（纯函数，便于单测）。
pub fn build_args(cfg: &XcodeCfg, enc: &str) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "info".into(),
        "-stats".into(),
        "-stats_period".into(),
        "1".into(),
    ];
    if cfg.realtime && !is_live_url(&cfg.input) {
        a.push("-re".into());
    }
    if let Some(ip) = cfg
        .nic_ip
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if is_udp_url(&cfg.input) {
            a.extend(["-localaddr".into(), ip.to_string()]);
        }
    }
    a.extend(["-i".into(), cfg.input.clone()]);
    a.extend(["-map".into(), "0:v:0".into()]);
    a.extend(["-c:v".into(), enc.to_string()]);
    let kbps = cfg.vbr_kbps.max(200);
    a.extend([
        "-b:v".into(),
        format!("{kbps}k"),
        "-maxrate".into(),
        format!("{kbps}k"),
        "-bufsize".into(),
        format!("{}k", kbps * 2),
    ]);
    let gop = cfg.gop.unwrap_or(25).clamp(1, 300);
    // 各编码器只给自身支持的实时参数；不能把 nvenc/qsv 的 preset 套给
    // Media Foundation / AMF / openh264，否则会直接报 Unknown option。
    match enc {
        "libopenh264" | "h264_mf" => a.extend(["-g".into(), gop.to_string()]),
        "h264_amf" => a.extend([
            "-quality".into(),
            "speed".into(),
            "-g".into(),
            gop.to_string(),
        ]),
        "h264_nvenc" | "hevc_nvenc" => a.extend([
            "-preset".into(),
            "p1".into(),
            "-tune".into(),
            "ll".into(),
            "-rc".into(),
            "cbr".into(),
            "-g".into(),
            gop.to_string(),
        ]),
        "h264_qsv" => a.extend([
            "-preset".into(),
            "veryfast".into(),
            "-g".into(),
            gop.to_string(),
        ]),
        _ => a.extend([
            "-preset".into(),
            "veryfast".into(),
            "-tune".into(),
            "zerolatency".into(),
            "-g".into(),
            gop.to_string(),
        ]),
    }
    if let Some(h) = cfg.height {
        a.extend(["-vf".into(), format!("scale=-2:{}", h.max(120))]);
    }
    if let Some(f) = cfg.fps {
        if f > 0.0 {
            a.extend(["-r".into(), format!("{f}")]);
        }
    }
    if !enc.starts_with("hevc") {
        a.extend(["-pix_fmt".into(), "yuv420p".into()]);
    }
    match cfg.audio.as_str() {
        "copy" => a.extend(["-map".into(), "0:a:0?".into(), "-c:a".into(), "copy".into()]),
        "aac" => {
            let audio_kbps = cfg.audio_kbps.unwrap_or(128).clamp(16, 512);
            a.extend([
                "-map".into(),
                "0:a:0?".into(),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{audio_kbps}k"),
            ]);
        }
        _ => a.push("-an".into()),
    }

    // 输出参数：组播补 pkt_size / ttl，封装按页面配置或按地址推导。
    let out = cfg.output.trim();
    let container = cfg
        .container
        .as_deref()
        .unwrap_or("auto")
        .to_ascii_lowercase();
    let pkt_size = cfg.pkt_size.unwrap_or(1316).clamp(1, 65535);
    let ttl = cfg.ttl.unwrap_or(0).min(255);
    fn with_udp_param(url: &str, key: &str, val: &str) -> String {
        if url.contains(&format!("{key}=")) {
            return url.to_string();
        }
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}{key}={val}")
    }
    if out.starts_with("udp://") || out.starts_with("rtp://") {
        let mut url = with_udp_param(out, "pkt_size", &pkt_size.to_string());
        if ttl > 0 {
            url = with_udp_param(&url, "ttl", &ttl.to_string());
        }
        let fmt = if container == "mp4" { "mp4" } else { "mpegts" };
        a.extend(["-f".into(), fmt.into(), url]);
    } else {
        let fmt = match container.as_str() {
            "mp4" => Some("mp4"),
            "mpegts" | "ts" => Some("mpegts"),
            _ if out.to_ascii_lowercase().ends_with(".ts") => Some("mpegts"),
            _ => None,
        };
        if let Some(fmt) = fmt {
            a.extend(["-f".into(), fmt.into(), out.to_string()]);
        } else {
            a.push(out.to_string());
        }
    }
    a.push("-y".into());
    a
}

/// 从一行 ffmpeg 统计里抽字段。返回是否识别为统计行。
fn parse_stats_line(line: &str, st: &mut XcodeStats) -> bool {
    if !line.contains("frame=") && !line.contains("size=") {
        return false;
    }
    let val = |key: &str| -> Option<&str> {
        line.find(key).map(|i| {
            let s = &line[i + key.len()..];
            s.split_whitespace().next().unwrap_or("")
        })
    };
    if let Some(v) = val("frame=") {
        if let Ok(n) = v.trim().parse::<u64>() {
            st.frame = n;
        }
    }
    if let Some(v) = val("fps=") {
        if let Ok(n) = v.trim().parse::<f64>() {
            st.fps = n;
        }
    }
    if let Some(v) = val("bitrate=") {
        let t = v
            .trim()
            .trim_end_matches("kbits/s")
            .trim_end_matches("bits/s");
        if let Ok(n) = t.parse::<f64>() {
            // 非 kbits/s 时按 bits/s 处理
            st.kbps = if v.contains("kbits") { n } else { n / 1000.0 };
        }
    }
    if let Some(v) = val("speed=") {
        let t = v.trim().trim_end_matches('x');
        if let Ok(n) = t.parse::<f64>() {
            st.speed = n;
        }
    }
    if let Some(v) = val("drop=") {
        if let Ok(n) = v.trim().parse::<u64>() {
            st.drop = n;
        }
    }
    if let Some(v) = val("time=") {
        st.out_time = v.trim().to_string();
    }
    true
}

pub struct XcodeSession {
    child: Arc<Mutex<Option<Child>>>,
    stats: Arc<Mutex<XcodeStats>>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

fn validate_cfg(cfg: &XcodeCfg) -> anyhow::Result<()> {
    if cfg.input.trim().is_empty() {
        return Err(anyhow::anyhow!("来源为空"));
    }
    if cfg.output.trim().is_empty() {
        return Err(anyhow::anyhow!("输出为空"));
    }
    let out = cfg.output.trim().to_ascii_lowercase();
    let container = cfg
        .container
        .as_deref()
        .unwrap_or("auto")
        .to_ascii_lowercase();
    if (out.starts_with("udp://") || out.starts_with("rtp://")) && container == "mp4" {
        return Err(anyhow::anyhow!("组播输出不能使用 mp4 封装，请选 MPEG-TS"));
    }
    Ok(())
}

fn encoder_id(cfg: &XcodeCfg, ffmpeg: &std::path::Path) -> String {
    if cfg.encoder == "auto" {
        crate::bridge::pick_h264_encoder(ffmpeg)
    } else {
        cfg.encoder.clone()
    }
}

/// 只生成实际会执行的命令与编码器，不启动进程；供设置页“预览命令”使用。
pub fn preview(cfg: &XcodeCfg) -> anyhow::Result<(String, String)> {
    validate_cfg(cfg)?;
    let ffmpeg = crate::tools::find_ffmpeg().ok_or_else(|| anyhow::anyhow!("未找到 ffmpeg"))?;
    let enc = encoder_id(cfg, &ffmpeg);
    let args = build_args(cfg, &enc);
    Ok((format!("ffmpeg {}", args.join(" ")), enc))
}

impl XcodeSession {
    pub fn start(cfg: &XcodeCfg) -> anyhow::Result<Self> {
        validate_cfg(cfg)?;
        let ffmpeg = crate::tools::find_ffmpeg().ok_or_else(|| anyhow::anyhow!("未找到 ffmpeg"))?;
        let enc = encoder_id(cfg, &ffmpeg);
        let args = build_args(cfg, &enc);
        let mut cmd = crate::ffmpeg::hidden(&ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("ffmpeg 启动失败：{e}"))?;
        let stderr = child.stderr.take();
        let stats = Arc::new(Mutex::new(XcodeStats {
            running: true,
            stage: "运行中".into(),
            encoder: enc,
            cmd: format!("ffmpeg {}", args.join(" ")),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));

        if let Some(mut se) = stderr {
            let st = Arc::clone(&stats);
            let sp = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("vqa-xcode-log".into())
                .spawn(move || {
                    let mut raw = [0u8; 8192];
                    let mut pending: Vec<u8> = Vec::new();
                    loop {
                        let n = match se.read(&mut raw) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        pending.extend_from_slice(&raw[..n]);
                        // 按 \r 与 \n 切段：ffmpeg 的统计用 \r 原地刷，报错用 \n
                        let mut cuts: Vec<usize> = Vec::new();
                        for (i, b) in pending.iter().enumerate() {
                            if *b == b'\r' || *b == b'\n' {
                                cuts.push(i);
                            }
                        }
                        let mut consumed = 0usize;
                        for i in cuts {
                            let seg = String::from_utf8_lossy(&pending[consumed..i])
                                .trim()
                                .to_string();
                            consumed = i + 1;
                            if seg.is_empty() {
                                continue;
                            }
                            let mut g = match st.lock() {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                            if !parse_stats_line(&seg, &mut g) {
                                if seg.len() < 400 && !seg.contains("Press [q]") {
                                    g.log.push(seg);
                                    if g.log.len() > 60 {
                                        g.log.remove(0);
                                    }
                                }
                            }
                        }
                        if consumed > 0 {
                            pending.drain(..consumed);
                        }
                        if pending.len() > 16 * 1024 {
                            pending.clear();
                        }
                        if sp.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    if let Ok(mut g) = st.lock() {
                        g.running = false;
                        if g.stage == "运行中" {
                            g.stage = "已结束".into();
                        }
                    }
                })
                .ok();
        }

        Ok(Self {
            child: Arc::new(Mutex::new(Some(child))),
            stats,
            stop,
            started: Instant::now(),
        })
    }

    pub fn snapshot(&self) -> XcodeStats {
        // 进程退出要反映到 running 上（转码正常结束或报错退出）
        if let Ok(mut g) = self.child.lock() {
            if let Some(ch) = g.as_mut() {
                if let Ok(Some(code)) = ch.try_wait() {
                    drop(g);
                    if let Ok(mut s) = self.stats.lock() {
                        s.running = false;
                        s.secs = self.started.elapsed().as_secs_f64();
                        if code.success() {
                            s.stage = "已完成".into();
                            s.err = None;
                        } else {
                            s.stage = "失败".into();
                            if s.err.is_none() {
                                s.err = Some(format!("ffmpeg 退出码 {code}"));
                            }
                        }
                    }
                }
            }
        }
        let mut s = self.stats.lock().map(|g| g.clone()).unwrap_or_default();
        s.secs = self.started.elapsed().as_secs_f64();
        s
    }

    pub fn alive(&self) -> bool {
        self.stats.lock().map(|s| s.running).unwrap_or(false)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.child.lock() {
            if let Some(ch) = g.as_mut() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
            *g = None;
        }
        if let Ok(mut s) = self.stats.lock() {
            s.running = false;
            s.stage = "已停止".into();
        }
    }
}

impl Drop for XcodeSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> XcodeCfg {
        XcodeCfg {
            input: "E:\\x.ts".into(),
            output: "udp://239.1.1.1:5000".into(),
            encoder: "auto".into(),
            vbr_kbps: 8000,
            audio: "copy".into(),
            realtime: true,
            height: Some(1080),
            fps: Some(50.0),
            nic_ip: None,
            gop: None,
            audio_kbps: None,
            container: None,
            pkt_size: None,
            ttl: None,
        }
    }

    #[test]
    fn live_output_gets_pkt_size_and_mpegts() {
        let a = build_args(&cfg(), "libopenh264");
        assert!(a.iter().any(|x| x.contains("pkt_size=1316")));
        let i = a.iter().position(|x| x == "-f").unwrap();
        assert_eq!(a[i + 1], "mpegts");
        assert!(a.iter().any(|x| x == "-b:v") && a.iter().any(|x| x == "8000k"));
        assert!(a.iter().any(|x| x == "-vf") && a.iter().any(|x| x == "scale=-2:1080"));
        assert!(a.iter().any(|x| x == "-re"));
    }

    #[test]
    fn output_and_transcode_options_are_applied() {
        let mut c = cfg();
        c.input = "udp://@239.1.1.1:5000".into();
        c.nic_ip = Some("192.168.10.20".into());
        c.gop = Some(50);
        c.audio = "aac".into();
        c.audio_kbps = Some(192);
        c.pkt_size = Some(4096);
        c.ttl = Some(8);
        let a = build_args(&c, "libopenh264");
        let i = a.iter().position(|x| x == "-localaddr").unwrap();
        assert_eq!(a[i + 1], "192.168.10.20");
        assert!(a.windows(2).any(|w| w[0] == "-g" && w[1] == "50"));
        assert!(a.windows(2).any(|w| w[0] == "-b:a" && w[1] == "192k"));
        let url = a.iter().rev().find(|x| x.starts_with("udp://239")).unwrap();
        assert!(url.contains("pkt_size=4096"));
        assert!(url.contains("ttl=8"));
    }

    #[test]
    fn hardware_encoders_only_get_supported_options() {
        let a = build_args(&cfg(), "h264_mf");
        assert!(!a.iter().any(|x| x == "-preset"));
        assert!(!a.iter().any(|x| x == "-tune"));
        assert!(a.windows(2).any(|w| w[0] == "-g" && w[1] == "25"));

        let a = build_args(&cfg(), "h264_amf");
        assert!(!a.iter().any(|x| x == "-preset"));
        assert!(a.windows(2).any(|w| w[0] == "-quality" && w[1] == "speed"));
    }

    #[test]
    fn stats_line_parsed() {
        let mut st = XcodeStats::default();
        let ok = parse_stats_line(
            "frame=  123 fps= 50 q=28.0 size=  2048kB time=00:00:05.00 bitrate=8192.0kbits/s speed=1.00x drop=3",
            &mut st,
        );
        assert!(ok);
        assert_eq!(st.frame, 123);
        assert!((st.kbps - 8192.0).abs() < 0.01);
        assert!((st.speed - 1.0).abs() < 0.01);
        assert_eq!(st.drop, 3);
        assert_eq!(st.out_time, "00:00:05.00");
    }
}
