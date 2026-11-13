//! AVS 探测与转码桥（移植自工作站 `vkitcore/media/bridge_ffmpeg.py`，
//! 实现参考 `vkitplayer/src/bridge.rs`）。
//!
//! 背景（工作站白皮书 §5.4 / §31.27 实测）：
//! * mpv 内置 libdavs2 解 4K 复杂 AVS2 画面会非法指令崩溃；
//! * 1080p 及以下直放没问题；AVS3 由 libuavs3d 解，无此问题；
//! * 解法：起播前用 ffprobe 快速探测，命中 4K/10-bit AVS2 就让 ffmpeg 在
//!   **自己的进程**里解（崩了带不走播放器），转成 H.264，走本地回环 UDP
//!   给 mpv 播。
//! * 10-bit 源：ffmpeg 的 libdavs2 只解 8bit，要走三段链
//!   （ffmpeg 抽裸流 → 自编译 10bit vkit_davs2 解码 → ffmpeg 编码回环）。

use std::net::UdpSocket;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::probe;

pub const AVS_CODECS: [&str; 3] = ["avs", "avs2", "cavs"];
const LOOPBACK: &str = "127.0.0.1";
/// 起桥存活判定窗口（对齐工作站 Bridge._ALIVE_S）
const ALIVE_MS: u64 = 1200;
/// 三段链发送端延迟（读端先绑 socket）
const CHAIN_SEND_DELAY_MS: u64 = 1200;

pub fn is_local(url: &str) -> bool {
    let low = url.to_ascii_lowercase();
    if low.starts_with("udp://")
        || low.starts_with("rtp://")
        || low.starts_with("rtsp://")
        || low.starts_with("rtmp://")
        || low.starts_with("http://")
        || low.starts_with("https://")
        || low.starts_with("srt://")
        || low.starts_with("mms://")
        || low.starts_with("file://")
    {
        return false;
    }
    Path::new(url).exists()
}

/// 起播判定：AVS 编码族且（10-bit 或 4K+）→ 要走桥。
/// 用 [`probe::probe_quick`]（8MB/4s 窗口；512KB 对 4K 10-bit 不够，
/// 只认出 codec、宽高全空 → 判不出 → 不走桥 → mpv 直接啃 → 崩）。
pub fn needs_bridge(q: &Option<probe::QuickProbe>, force: bool) -> bool {
    force || probe::needs_bridge(q)
}

fn free_udp_port() -> std::io::Result<u16> {
    let s = UdpSocket::bind((LOOPBACK, 0))?;
    Ok(s.local_addr()?.port())
}

// ------------------------------------------------------------ H.264 编码器
static ENC_CACHE: Mutex<Option<String>> = Mutex::new(None);

fn enc_ok(ffmpeg: &Path, name: &str) -> bool {
    let mut cmd = crate::ffmpeg::hidden(ffmpeg);
    cmd.args([
        "-hide_banner", "-v", "error", "-f", "lavfi",
        "-i", "testsrc2=size=320x240:rate=5", "-t", "0.3",
        "-c:v", name, "-f", "null", "-",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// 优先硬编（快得多），退回 libx264；结果缓存（探测要起进程）。
pub fn pick_h264_encoder(ffmpeg: &Path) -> String {
    if let Ok(cached) = ENC_CACHE.lock() {
        if let Some(c) = cached.as_ref() {
            return c.clone();
        }
    }
    let enc = ["h264_nvenc", "h264_qsv", "h264_amf"]
        .into_iter()
        .find(|n| enc_ok(ffmpeg, n))
        .unwrap_or("libx264")
        .to_string();
    if let Ok(mut c) = ENC_CACHE.lock() {
        *c = Some(enc.clone());
    }
    enc
}

fn encoder_args(enc: &str, crf: i32) -> Vec<String> {
    let cq = crf.to_string();
    let base: &[&str] = match enc {
        "h264_nvenc" => &[
            "-preset", "p1", "-tune", "ll", "-rc", "vbr", "-g", "25",
            "-pix_fmt", "yuv420p",
        ],
        "h264_qsv" => &["-preset", "veryfast", "-g", "25", "-pix_fmt", "yuv420p"],
        "h264_amf" => &["-quality", "speed", "-g", "25", "-pix_fmt", "yuv420p"],
        _ => &[
            "-preset", "ultrafast", "-tune", "zerolatency", "-g", "25",
            "-keyint_min", "25", "-sc_threshold", "0", "-pix_fmt", "yuv420p",
        ],
    };
    let mut v: Vec<String> = match enc {
        "h264_nvenc" => vec!["-c:v".into(), enc.into(), "-cq".into(), cq],
        "h264_qsv" => vec!["-c:v".into(), enc.into(), "-global_quality".into(), cq],
        "h264_amf" => vec![
            "-c:v".into(),
            enc.into(),
            "-rc".into(),
            "cqp".into(),
            "-qp_i".into(),
            cq.clone(),
            "-qp_p".into(),
            cq,
        ],
        _ => vec!["-c:v".into(), "libx264".into(), "-crf".into(), cq],
    };
    v.extend(base.iter().map(|s| s.to_string()));
    v
}

/// 给 ffmpeg 命令加网络源的出口网卡绑定（仅 udp/rtp/tcp 支持，netif 判定）。
fn push_localaddr(cmd: &mut Command, url: &str, localaddr: &str) {
    for a in crate::netif::bind_args(url, localaddr) {
        cmd.arg(a);
    }
}

// ------------------------------------------------------------ 实时转码桥
/// "解 AVS → 转 H.264 → 本地回环 UDP" 的实时管道（直播语义，不可拖动）。
/// mpv 读 `udp://@127.0.0.1:<port>`。
pub struct RealtimeBridge {
    procs: std::sync::Arc<Mutex<Vec<Child>>>,
    pub port: u16,
    pub mode: String,
}

impl RealtimeBridge {
    /// 起桥。`chain10` 为 true 走三段链（10-bit AVS2）。
    /// `localaddr`：组播源的出口网卡（空 = 自动）。
    pub fn start(
        ffmpeg: &Path,
        davs2: &Path,
        url: &str,
        chain10: bool,
        crf: i32,
        localaddr: &str,
    ) -> Result<RealtimeBridge, String> {
        let port = free_udp_port().map_err(|e| format!("挑 UDP 端口失败：{e}"))?;
        let local = is_local(url);
        let dst = format!("udp://{LOOPBACK}:{port}?pkt_size=1316");
        let enc = pick_h264_encoder(ffmpeg);
        let enc_args = encoder_args(&enc, crf);

        if !chain10 {
            // 单段：ffmpeg 直转（8-bit 源 / 非 AVS 的救场路径）
            let mut cmd = crate::ffmpeg::hidden(ffmpeg);
            cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
            if local {
                cmd.arg("-re"); // 本地文件按原速读，否则 mpv 端会快进
            }
            push_localaddr(&mut cmd, url, localaddr); // ★ 只对网络源生效（netif 判定）
            cmd.arg("-i")
                .arg(url)
                .args(["-map", "0:v:0"])
                .args(&enc_args)
                .args([
                    "-map", "0:a:0?", "-c:a", "aac", "-b:a", "96k",
                    "-f", "mpegts", &dst,
                ])
                .stderr(Stdio::piped());
            let mut p = cmd.spawn().map_err(|e| format!("起转码失败：{e}"))?;
            // 存活检查（对齐工作站 Bridge._ALIVE_S）：秒退 = 起桥失败，
            // 让调用方回退直放并报出原因，而不是让 mpv 对空 UDP 干等。
            // stderr 必须持续读走（工作站实测：不读会把 4KB 管道缓冲写满、
            // ffmpeg 被堵死）。后台线程收进共享缓冲，失败时取尾行报因。
            let err_buf = std::sync::Arc::new(Mutex::new(String::new()));
            let stderr = p.stderr.take();
            if let Some(se) = stderr {
                let sink = std::sync::Arc::clone(&err_buf);
                std::thread::spawn(move || {
                    let mut s = se;
                    let mut local = String::new();
                    if std::io::Read::read_to_string(&mut s, &mut local).is_ok() {
                        *sink.lock().unwrap() = local;
                    }
                });
            }
            let deadline = std::time::Instant::now() + Duration::from_millis(ALIVE_MS);
            while std::time::Instant::now() < deadline {
                if p.try_wait().map(|s| s.is_some()).unwrap_or(false) {
                    let code = p.try_wait().ok().and_then(|s| s).and_then(|s| s.code());
                    let tail = err_buf
                        .lock()
                        .unwrap()
                        .lines()
                        .last()
                        .unwrap_or("")
                        .to_string();
                    return Err(if tail.is_empty() {
                        format!("转码进程秒退（退出码 {code:?}）")
                    } else {
                        tail
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            return Ok(RealtimeBridge {
                procs: std::sync::Arc::new(Mutex::new(vec![p])),
                port,
                mode: "ffmpeg".into(),
            });
        }

        // 三段链：抽裸流 | vkit_davs2 解码 → Y4M | ffmpeg 编码 → 回环
        let mut cmd1 = crate::ffmpeg::hidden(ffmpeg);
        cmd1.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        if local {
            cmd1.arg("-re");
        }
        push_localaddr(&mut cmd1, url, localaddr);
        cmd1.arg("-i")
            .arg(url)
            .args(["-map", "0:v:0", "-c", "copy", "-f", "avs2", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut p1 = cmd1.spawn().map_err(|e| format!("抽流段失败：{e}"))?;

        let mut cmd2 = crate::ffmpeg::hidden(davs2);
        cmd2.args(["-i", "-", "-o", "stdout"])
            .stdin(p1.stdout.take().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut p2 = match cmd2.spawn() {
            Ok(p2) => p2,
            Err(e) => {
                let _ = p1.kill(); // 不把抽流段留在半空
                return Err(format!("10bit 解码段失败：{e}"));
            }
        };

        let mut cmd3 = crate::ffmpeg::hidden(ffmpeg);
        cmd3.args([
            "-hide_banner", "-loglevel", "error", "-nostdin",
            "-f", "yuv4mpegpipe", "-i", "-",
        ])
        .args(["-map", "0:v:0"]);
        if local {
            // 音频"再开一次源"：两路都 -re 才不会一阵一阵（工作站 §31.49）
            cmd3.args(["-re", "-i", url]).args(["-map", "1:a:0?"]);
        }
        cmd3.args(&enc_args);
        if local {
            cmd3.args(["-c:a", "aac", "-b:a", "96k"]);
        }
        cmd3.args(["-f", "mpegts", &dst]).stdin(Stdio::piped());

        // ★ 时序（工作站 §31.46.3 实测）：读端必须先绑好 socket。先起前两段，
        //   把端口暴露给调用方去起 mpv，1.2 秒后再起发送段（第 3 段）——
        //   否则早期 PAT/PMT/PPS 包永久丢失，mpv 卡在 "non-existing PPS"。
        // 前两段存活检查：秒退（参数/解码器问题）就立刻回退直放并报因，
        // 别让 mpv 对着永远没有数据的 UDP 干等
        std::thread::sleep(Duration::from_millis(400));
        let p1_dead = p1.try_wait().map(|s| s.is_some()).unwrap_or(false);
        let p2_dead = p2.try_wait().map(|s| s.is_some()).unwrap_or(false);
        if p1_dead || p2_dead {
            let _ = p1.kill();
            let _ = p2.kill();
            let _ = p1.wait();
            let _ = p2.wait();
            return Err("10-bit 链前两段秒退（源不是 AVS2 或解码器不可用）".into());
        }
        let procs = std::sync::Arc::new(Mutex::new(Vec::<Child>::new()));
        procs.lock().unwrap().push(p1);
        procs.lock().unwrap().push(p2);
        let procs2 = std::sync::Arc::clone(&procs);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(CHAIN_SEND_DELAY_MS));
            if let Ok(mut v) = procs2.lock() {
                if let Ok(p3) = cmd3.spawn() {
                    v.push(p3);
                }
            }
        });
        Ok(RealtimeBridge {
            procs,
            port,
            mode: "chain10".into(),
        })
    }

    /// mpv 要读的回环地址。
    pub fn url(&self) -> String {
        format!("udp://@{LOOPBACK}:{}", self.port)
    }

    pub fn stop(&self) {
        if let Ok(mut v) = self.procs.lock() {
            for p in v.iter_mut() {
                let _ = p.kill();
                let _ = p.wait();
            }
            v.clear();
        }
    }
}

impl Drop for RealtimeBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_args_shape() {
        let v = encoder_args("libx264", 26);
        assert!(v.contains(&"-crf".to_string()));
        assert!(v.contains(&"26".to_string()));
        let v = encoder_args("h264_nvenc", 20);
        assert!(v.contains(&"-cq".to_string()));
    }

    #[test]
    fn is_local_paths() {
        assert!(is_local(r"D:\nonexistent-check.ts") == Path::new(r"D:\nonexistent-check.ts").exists());
        assert!(!is_local("udp://@239.1.1.1:5000"));
        assert!(!is_local("http://x/y.ts"));
    }
}
