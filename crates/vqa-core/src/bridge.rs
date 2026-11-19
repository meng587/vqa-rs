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

pub fn free_udp_port() -> std::io::Result<u16> {
    let s = UdpSocket::bind((LOOPBACK, 0))?;
    Ok(s.local_addr()?.port())
}

// ------------------------------------------------------------ H.264 编码器
static ENC_CACHE: Mutex<Option<String>> = Mutex::new(None);

fn enc_ok(ffmpeg: &Path, name: &str) -> bool {
    let mut cmd = crate::ffmpeg::hidden(ffmpeg);
    cmd.args([
        "-hide_banner", "-v", "error", "-f", "lavfi",
        "-i", "testsrc2=size=1280x720:rate=5", "-t", "0.3",
        "-c:v", name, "-f", "null", "-",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// 优先硬编（快得多），退回软编；结果缓存（探测要起进程）。
/// `h264_mf` 是 Windows Media Foundation 编码器：没有 NVENC/QSV/AMF 的机器
/// 往往还能用它走显卡/系统硬编，比直接退 libopenh264 快（本机 4K 实测 3.99×）。
/// 随包 FFmpeg 没有 libx264，因此最终兜底必须是 libopenh264。
pub fn pick_h264_encoder(ffmpeg: &Path) -> String {
    if let Ok(cached) = ENC_CACHE.lock() {
        if let Some(c) = cached.as_ref() {
            return c.clone();
        }
    }
    let enc = ["h264_nvenc", "h264_qsv", "h264_amf", "h264_mf"]
        .into_iter()
        .find(|n| enc_ok(ffmpeg, n))
        .or_else(|| {
            // 软编兜底：GPL 版有 libx264，随包的 LGPL 版只有 libopenh264。
            ["libx264", "libopenh264"]
                .into_iter()
                .find(|n| enc_ok(ffmpeg, n))
        })
        .unwrap_or("libopenh264")
        .to_string();
    if let Ok(mut c) = ENC_CACHE.lock() {
        *c = Some(enc.clone());
    }
    enc
}

pub fn encoder_args(enc: &str, crf: i32) -> Vec<String> {
    let cq = crf.to_string();
    let base: &[&str] = match enc {
        "h264_nvenc" => &[
            "-preset", "p1", "-tune", "ll", "-rc", "vbr", "-g", "25",
            "-pix_fmt", "yuv420p",
        ],
        "h264_qsv" => &["-preset", "veryfast", "-g", "25", "-pix_fmt", "yuv420p"],
        "h264_amf" => &["-quality", "speed", "-g", "25", "-pix_fmt", "yuv420p"],
        // h264_mf（Media Foundation）不认 -preset/-tune，也不支持 CRF；
        // 只给 GOP/像素格式，码率用 -b:v 近似（与 libopenh264 同档）。
        "h264_mf" => &["-g", "25", "-pix_fmt", "yuv420p"],
        // libopenh264 不认 -preset/-tune，只给 GOP 与像素格式
        "libopenh264" => &["-g", "25", "-pix_fmt", "yuv420p"],
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
        // libopenh264 / h264_mf 没有 crf，用码率近似：crf 越小给得越高
        "libopenh264" | "h264_mf" => {
            let kbps = match crf {
                c if c >= 28 => "4000k",
                c if c >= 24 => "8000k",
                _ => "12000k",
            };
            vec![
                "-c:v".into(),
                enc.into(),
                "-b:v".into(),
                kbps.into(),
            ]
        }
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

/// 8K 直播 UDP 的默认接收缓冲只有 28672 个 188 字节包（约 5.4 MB）。
/// 120 Mbps 下不到 0.4 秒就会填满，ffmpeg 直接报 "Circular buffer
/// overrun" 并中止；这不是解码器崩溃，而是输入没来得及读走。
/// 把 fifo 提高到约 49 MB，并申请更大的系统 socket 缓冲；仍溢出时
/// 允许丢包继续跑，避免 watchdog 反复杀桥。
pub(crate) fn udp_input_url(url: &str) -> String {
    if !url.to_ascii_lowercase().starts_with("udp://") {
        return url.to_string();
    }
    let mut out = url.to_string();
    let sep = if out.contains('?') { '&' } else { '?' };
    let mut opts: Vec<&str> = Vec::new();
    if !out.contains("fifo_size=") {
        opts.push("fifo_size=262144");
    }
    if !out.contains("overrun_nonfatal=") {
        opts.push("overrun_nonfatal=1");
    }
    if !out.contains("rcvbuf=") {
        opts.push("rcvbuf=33554432");
    }
    if !opts.is_empty() {
        out.push(sep);
        out.push_str(&opts.join("&"));
    }
    out
}

// ------------------------------------------------------------ 实时转码桥
/// "解 AVS → 转 H.264 → 本地回环 UDP" 的实时管道（直播语义，不可拖动）。
/// mpv 读 `udp://@127.0.0.1:<port>`。
pub struct RealtimeBridge {
    procs: std::sync::Arc<Mutex<Vec<Child>>>,
    pub port: u16,
    pub mode: String,
    /// chain10 本地文件模式抽出的 AVS2 裸流临时文件（Drop 时清理）
    tmp_es: Option<std::path::PathBuf>,
    /// 桥各段的 stderr 尾部（davs2/nvenc 报错留证；诊断日志读取）
    pub stderr_buf: std::sync::Arc<Mutex<String>>,
    /// chain10 本地文件模式全片转码完成后的 H.264 TS 文件（mpv 播放它）
    out_file: Option<std::path::PathBuf>,
}

impl RealtimeBridge {
    /// 起桥。`chain10` 为 true 走三段链（10-bit AVS2）。
    /// `localaddr`：组播源的出口网卡（空 = 自动）。
    /// `audio_lead`：源里音频比画面提前的秒数（>0 时裁掉音频头，否则全程恒差错位）。
    pub fn start(
        ffmpeg: &Path,
        davs2: &Path,
        url: &str,
        chain10: bool,
        crf: i32,
        localaddr: &str,
        audio_lead: f64,
    ) -> Result<RealtimeBridge, String> {
        Self::start_scaled(
            ffmpeg, davs2, url, chain10, crf, localaddr, audio_lead, 0, None, false,
        )
    }

    /// 同 [`RealtimeBridge::start`]，但可指定输出宽度（0 = 保持源分辨率）。
    /// 8K 软解跟不上时，播放器按丢帧阶梯调用：3840 → 1920 → 1280。
    /// `decoder` 可强制输入解码器（AVS3 用 `libuavs3d`，避免 ffmpeg
    /// 把 AVS3 误判成 AVS2 后找不到解码器）。
    /// `drop_audio`：8K AVS3 自动降级时不带源音频。部分信源 MP2 头损坏，
    /// 音频解码错误会持续刷 stderr；监看场景宁可静音，也不让桥被拖垮。
    pub fn start_scaled(
        ffmpeg: &Path,
        davs2: &Path,
        url: &str,
        chain10: bool,
        crf: i32,
        localaddr: &str,
        audio_lead: f64,
        scale_w: u32,
        decoder: Option<&str>,
        drop_audio: bool,
    ) -> Result<RealtimeBridge, String> {
        let port = free_udp_port().map_err(|e| format!("挑 UDP 端口失败：{e}"))?;
        let local = is_local(url);
        // ★ 传输用 TCP（cmd3 监听，mpv 连接）而非 UDP 推送：环回 UDP 无流控，
        //   mpv 消费端任何短暂停顿都会让接收缓冲溢出丢 TS 包 → 流残缺 →
        //   mpv 重新同步 → 周期性"卡一下"（实测 3s 播/3s 卡交替）。TCP 的
        //   流控会把 mpv 的消费速度逐级回压到 davs2 解码，零丢包连续播放。
        let dst = format!("tcp://127.0.0.1:{port}?listen=1");
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
            if !local {
                // 8K 120 Mbps 源把 ffmpeg 输入线程队列也放大，降低突发丢包。
                cmd.args(["-thread_queue_size", "4096"]);
            }
            // ★ AVS3 必须强制 libuavs3d：ffmpeg 的裸流探测会把 AVS3 当 AVS2，
            //   默认解码器直接报 “Decoder (codec avs2) not found”。
            if let Some(dec) = decoder {
                cmd.args(["-c:v", dec]);
            }
            cmd.arg("-i")
                .arg(udp_input_url(url))
                .args(["-map", "0:v:0"]);
            // ★ 8K 性能不足自动降级：先缩放再编码，输出 4K/1080p，mpv
            //   只解降级后的画面；这是"能播放"与"画质"之间的折中。
            //   8K 50fps 源即使 4K 输出也容易压垮 NVENC 的实时编码，
            //   同时把桥输出固定为 25fps：监看足够，且能避免"播几秒停几秒"。
            if scale_w > 0 {
                cmd.args(["-vf", &format!("scale={scale_w}:-2,fps=25")]);
            }
            cmd.args(&enc_args);
            if drop_audio {
                // 8K AVS3 降级监看：源里 MP2 头损坏时，带音频只会不断刷
                // "Header missing" 并让看门狗误判桥无推进；直接 `-an`。
                cmd.arg("-an");
            } else {
                cmd.args(["-map", "0:a:0?", "-c:a", "aac", "-b:a", "96k"]);
            }
            cmd.args(["-f", "mpegts", &dst])
                .stderr(Stdio::piped());
            // ★ 这里必须等 encoder_args / 音频参数都追加后再打印：
            //   之前提前打印的 cmd 看不到实际编码器，诊断包无法判断用了
            //   h264_nvenc 还是 libopenh264，也没法定位 4K 编码失败。
            crate::config::log_line(
                "bridge",
                &format!(
                    "start_scaled scale_w={scale_w} decoder={} enc={enc} drop_audio={drop_audio} cmd={cmd:?}",
                    decoder.unwrap_or("default")
                ),
            );
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
                tmp_es: None,
                out_file: None,
                stderr_buf: err_buf,
            });
        }

        // 三段链：抽裸流 | vkit_davs2 解码 → Y4M | ffmpeg 编码 → 回环
        //
        // ★ 本地文件源：先全速抽 AVS2 裸流到临时文件，davs2 直读文件——
        //   vkit_davs2 的 stdin 管道输入实测仅 ~0.05fps（管道读缺陷），
        //   文件输入 43fps（远超实时）。mpv 在抽流完成后才 loadfile（Bridged
        //   Outcome 在 start 返回后才收割），无超时窗口问题。
        //   直播源仍走管道（实时流不能落盘等齐）。
        //
        //   （ADTS 双输出抽取方案已回退：`-c:a copy -f adts` 对 CCTV TS 的
        //   AAC 包装会失败导致整链报错；直连修复后 cmd3 用 analyzeduration 0
        //   直读原始 TS 已验证可行，无需预抽音频。）
        let tmp_es = if local {
            let es = std::env::temp_dir().join(format!(
                "vqa_avs2_{}_{}.es",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_millis())
                    .unwrap_or(0)
            ));
            let mut cmd0 = crate::ffmpeg::hidden(ffmpeg);
            cmd0.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
            push_localaddr(&mut cmd0, url, localaddr);
            cmd0.args(["-thread_queue_size", "4096"]);
            cmd0.arg("-i")
                .arg(udp_input_url(url))
                .args(["-map", "0:v:0", "-c", "copy", "-f", "avs2"])
                .arg(&es)
                .stderr(Stdio::null());
            let st0 = cmd0
                .status()
                .map_err(|e| format!("抽流段启动失败：{e}"))?;
            if !st0.success() {
                return Err("抽流段失败（AVS2 裸流抽取未完成）".into());
            }
            Some(es)
        } else {
            None
        };

        let mut cmd1 = crate::ffmpeg::hidden(ffmpeg);
        cmd1.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        // 直播源：实时流按原速进入（源天然实时）；本地文件源已落盘为
        // 裸流文件，davs2 直读（见上），不走 cmd1。
        if !local {
            push_localaddr(&mut cmd1, url, localaddr);
            cmd1.args(["-thread_queue_size", "4096"]);
            cmd1.arg("-i")
                .arg(udp_input_url(url))
                .args(["-map", "0:v:0", "-c", "copy", "-f", "avs2", "-"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
        }
        let mut p1 = if local {
            None
        } else {
            Some(cmd1.spawn().map_err(|e| format!("抽流段失败：{e}"))?)
        };

        let mut cmd2 = crate::ffmpeg::hidden(davs2);
        // ★ 多线程解码：davs2 默认单线程，4K 10-bit 单线程 5-15fps（弱机更慢），
        //   多线程把首帧压进秒级。vkit_davs2 的 -t N 即解码线程数（--help 实查）。
        let davs2_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(16).to_string())
            .unwrap_or_else(|_| "16".into());
        if let Some(f) = &tmp_es {
            // 本地文件：davs2 直读裸流文件（绕开 stdin 管道 0.05fps 缺陷）
            cmd2.arg("-i").arg(f.as_os_str()).stdin(Stdio::null());
        } else {
            cmd2.arg("-i").arg("-").stdin(p1.as_mut().unwrap().stdout.take().unwrap());
        }
        cmd2.args(["-o", "stdout", "-t", &davs2_threads])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut p2 = match cmd2.spawn() {
            Ok(p2) => p2,
            Err(e) => {
                if let Some(mut p) = p1 {
                    let _ = p.kill(); // 不把抽流段留在半空
                }
                return Err(format!("10bit 解码段失败：{e}"));
            }
        };

        let mut cmd3 = crate::ffmpeg::hidden(ffmpeg);
        cmd3.args([
            "-hide_banner", "-loglevel", "error", "-nostdin",
            "-f", "yuv4mpegpipe", "-i", "-",
        ]);
        if local {
            // 音频"再开一次源"。★ analyzeduration 0：跳过 AVS2 视频流的
            // find_stream_info（内置 8-bit davs2 解 10-bit 会拖死整链），
            // 直读原始 TS 只取它的 AAC 音频（直连修复后已实测可行）。
            cmd3.args(["-analyzeduration", "0", "-probesize", "1000000", "-i", url])
                .stdin(Stdio::null());
        }
        // ★ -map/-c 是输出选项，必须放在所有 -i 之后、输出文件之前——
        //   夹在两个 -i 之间会被该 ffmpeg 当作后一个输入的选项报错
        //   （"Option map cannot be applied to input url ..."，实测），
        //   这正是 AVS 桥从未出过画面的直接原因。
        cmd3.args(["-map", "0:v:0"]);
        if local {
            cmd3.args(["-map", "1:a:0?"]);
            // ★ 音画同步：源里音频常比画面早到（抓流/录制起点所致）。
            //   视频走 y4m 管道，时间戳从 0 重排；音频按源时间轴走——
            //   不裁头就是全程恒定"声音超前画面 audio_lead 秒"（实测 avs1.ts
            //   音频 1.400s 起、视频 3.256s 起，错位 1.856s）。
            if audio_lead > 0.05 {
                cmd3.args([
                    "-af",
                    &format!(
                        "atrim=start={:.3},asetpts=PTS-STARTPTS",
                        audio_lead
                    ),
                ]);
            }
        }
        // ★ 10-bit → 8-bit：davs2 输出 yuv420p10le（y4m C420p10），而
        //   h264_nvenc 对 10-bit H.264 报 "No capable devices found"
        //   （实测本机 GPU）——显式降 8-bit 再喂编码器。
        // ★ fps=25：davs2 软解 4K 10-bit 实测 ~41fps，低于源帧率（50fps）时
        //   播放端会"播几秒、停几秒"交替（供给 < 需求，缓冲周期性耗尽）。
        //   桥输出降为 25fps 后解码富余 64%，播放连续不断（检测观看场景
        //   25fps 足够）。25fps 也让 nvenc 码率减半。
        cmd3.args(["-vf", "format=yuv420p,fps=25"]);
        cmd3.args(&enc_args);
        if local {
            cmd3.args(["-c:a", "aac", "-b:a", "96k"]);
        }
        // ★ 输出目标按分支给（见下方 local/直播分支）——不能在这里统一加：
        //   local 分支若残留 tcp listen 输出，ffmpeg 启动时初始化 muxer 会
        //   阻塞在 accept 等客户端连接，不读 y4m 管道 → davs2 管道写满 →
        //   整链互等死锁（两个进程 CPU 归零，实测）。
        cmd3.stderr(Stdio::piped());

        // ★ 时序（工作站 §31.46.3 实测）：读端必须先绑好 socket。先起前两段，
        //   把端口暴露给调用方去起 mpv，1.2 秒后再起发送段（第 3 段）——
        //   否则早期 PAT/PMT/PPS 包永久丢失，mpv 卡在 "non-existing PPS"。
        // 前两段存活检查：秒退（参数/解码器问题）就立刻回退直放并报因，
        // 别让 mpv 对着永远没有数据的 UDP 干等
        std::thread::sleep(Duration::from_millis(400));
        let p1_dead = p1
            .as_mut()
            .map(|p| p.try_wait().map(|s| s.is_some()).unwrap_or(false))
            .unwrap_or(false);
        let p2_dead = p2.try_wait().map(|s| s.is_some()).unwrap_or(false);
        if p1_dead || p2_dead {
            if let Some(mut p) = p1 {
                let _ = p.kill();
                let _ = p.wait();
            }
            let _ = p2.kill();
            let _ = p2.wait();
            return Err("10-bit 链前两段秒退（源不是 AVS2 或解码器不可用）".into());
        }
        let procs = std::sync::Arc::new(Mutex::new(Vec::<Child>::new()));
        if let Some(p) = p1 {
            procs.lock().unwrap().push(p);
        }
        // stderr 收集：davs2/编码段的报错必须被读走（否则 4KB 管道缓冲
        // 写满会把进程堵死——工作站实测纪律），失败时留证可查
        // （写入共享 stderr_buf 供诊断日志读取 + eprintln 供开发日志）
        let stderr_buf = std::sync::Arc::new(Mutex::new(String::new()));
        let davs2_stderr = p2.stderr.take();
        // ★ cmd2.stdout → cmd3.stdin 直连（davs2 的 y4m 输出喂编码段）。
        //   若两边都各自 Stdio::piped()，是两个互不相通的管道：davs2 写 A
        //   塞满阻塞、cmd3 读 B 永远空——三段链整体死锁（实测 0.05fps 假象
        //   与"已播完"皆源于此）。
        let davs2_y4m = p2.stdout.take().unwrap();
        procs.lock().unwrap().push(p2);
        if let Some(mut se) = davs2_stderr {
            let buf2 = std::sync::Arc::clone(&stderr_buf);
            std::thread::spawn(move || {
                use std::io::Read as _;
                let mut buf = String::new();
                let _ = se.read_to_string(&mut buf);
                if !buf.trim().is_empty() {
                    *buf2.lock().unwrap() = format!("[davs2] {}", buf.trim());
                    eprintln!("[bridge/davs2] ====== {}", buf.trim());
                }
            });
        }
        let procs3 = std::sync::Arc::clone(&procs);
        let stderr_buf3 = std::sync::Arc::clone(&stderr_buf);
        if local {
            // ★ 本地文件模式：cmd3 立即启动，stdin 直连 davs2 的 y4m 输出。
            //   延迟版（直播口径）叠加"两管道未直连"的缺陷会整体死锁；
            //   Python 同构复刻（立即启动 + 真直连）一切正常，实测定位。
            //   mpv 端 UDP 周期性 PAT/PMT 重发（ffmpeg 默认 ~100ms），
            //   晚 bind 也不会丢表。
            //
            //   ★ 输出走 TCP 流（与直播同机制）：本地文件抽完裸流后 davs2
            //     文件直读 76fps，3 倍于桥输出 25fps 的播放消耗，流式也零
            //     卡顿（fps=25 后 40s 连续播放实测）。mpv 读"增长中的本地
            //     文件"会在写入点提前 EOF（实测播 1 秒就"已播完"），所以
            //     不用文件模式。非 seek——用户优先"直接可以看到"。
            //   cmd3 立即启动（listen 随 spawn 就绪），留 1.2s 让读端就绪。
            cmd3.args(["-f", "mpegts", &dst]);
            cmd3.stdin(davs2_y4m);
            let mut p3 = match cmd3.spawn() {
                Ok(p) => p,
                Err(e) => {
                    // 编码段起不来：收割 davs2（在 procs 里），不留孤儿
                    if let Ok(mut v) = procs.lock() {
                        for p in v.iter_mut() {
                            let _ = p.kill();
                            let _ = p.wait();
                        }
                        v.clear();
                    }
                    return Err(format!("编码段启动失败：{e}"));
                }
            };
            if let Some(mut se) = p3.stderr.take() {
                let buf3 = std::sync::Arc::clone(&stderr_buf);
                std::thread::spawn(move || {
                    use std::io::Read as _;
                    let mut buf = String::new();
                    let _ = se.read_to_string(&mut buf);
                    if !buf.trim().is_empty() {
                        *buf3.lock().unwrap() = format!("[nvenc] {}", buf.trim());
                        eprintln!("[bridge/nvenc] ====== {}", buf.trim());
                    }
                });
            }
            // p3 交给 procs：正常结束自己退出；用户中途停止时 stop() kill
            procs.lock().unwrap().push(p3);
            // 读端就绪窗口：TCP listen 随 spawn 就绪，mpv 在 Outcome 收割后才连接，
            // 这里只需给首包留一点余量——原 1200ms 是白等（起播慢的直接原因之一）。
            std::thread::sleep(Duration::from_millis(400));
            // 立即返回：mpv 连接 tcp 流，抽流+解码在前台继续
            return Ok(RealtimeBridge {
                procs,
                port,
                mode: "chain10".into(),
                tmp_es,
                stderr_buf,
                out_file: None,
            });
        } else {
            // 直播源：维持"读端先绑 socket"时序（§31.46.3），延迟 1.2s 启动。
            // 输出走 tcp listen（mpv 作为客户端连接，TCP 流控回压解码段）
            cmd3.args(["-f", "mpegts", &dst]);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(CHAIN_SEND_DELAY_MS));
                if let Ok(mut v) = procs3.lock() {
                    if let Ok(mut p3) = cmd3.spawn() {
                        if let Some(mut se) = p3.stderr.take() {
                            std::thread::spawn(move || {
                                use std::io::Read as _;
                                let mut buf = String::new();
                                let _ = se.read_to_string(&mut buf);
                                if !buf.trim().is_empty() {
                                    *stderr_buf3.lock().unwrap() = format!("[nvenc] {}", buf.trim());
                                    eprintln!("[bridge/nvenc] ====== {}", buf.trim());
                                }
                            });
                        }
                        v.push(p3);
                    }
                }
            });
        }
        Ok(RealtimeBridge {
            procs,
            port,
            mode: "chain10".into(),
            tmp_es,
            stderr_buf,
            out_file: None,
        })
    }

    /// 桥各段 stderr 尾部（诊断日志用）。
    pub fn stderr_tail(&self) -> String {
        self.stderr_buf.lock().unwrap().clone()
    }

    /// 桥各段进程的存活数（诊断用；链完整 = 3）。
    pub fn alive(&self) -> usize {
        let mut n = 0;
        for p in self.procs.lock().unwrap().iter_mut() {
            if p.try_wait().map(|s| s.is_none()).unwrap_or(true) {
                n += 1;
            }
        }
        n
    }

    /// mpv 要播放的地址。
    /// ★ chain10 本地文件模式：全片转码完成后的 TS 文件路径（loadfile 文件，
    ///   可拖动、零丢包零卡顿——UDP/TCP 推送模式实测均有周期性停顿）。
    ///   直播源仍是 tcp 流地址（mpv 作为客户端连接 cmd3 的 listen）。
    pub fn url(&self) -> String {
        if let Some(f) = &self.out_file {
            return f.to_string_lossy().to_string();
        }
        // ★ 与 cmd3 的 tcp listen 配对：mpv 作为客户端连接，TCP 流控保证
        //   消费速度逐级回压到解码段，零丢包（UDP 版实测周期性卡顿）。
        format!("tcp://127.0.0.1:{}", self.port)
    }

    /// 本地文件桥（chain10 同步转码模式）——EOF 播完是正常结束，不是桥故障。
    pub fn is_file(&self) -> bool {
        self.out_file.is_some()
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
        // 本地文件模式抽出的 AVS2 裸流 + ADTS 音频临时文件：桥结束后清理
        if let Some(f) = self.tmp_es.take() {
            let _ = std::fs::remove_file(f);
        }
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

    #[test]
    fn udp_gets_buffer_options() {
        let u = udp_input_url("udp://238.123.43.96:2000");
        assert!(u.contains("fifo_size=262144"), "{u}");
        assert!(u.contains("overrun_nonfatal=1"), "{u}");
        assert!(u.contains("rcvbuf=33554432"), "{u}");
    }

    #[test]
    fn udp_query_uses_ampersand_and_keeps_existing() {
        let u = udp_input_url("udp://@239.1.1.1:5000?localaddr=192.168.1.2");
        assert!(u.contains("?localaddr=192.168.1.2&"), "{u}");
        assert!(u.contains("fifo_size="), "{u}");
        let u2 = udp_input_url("udp://239.1.1.1:5000?fifo_size=65536");
        assert_eq!(u2.matches("fifo_size=").count(), 1, "{u2}");
    }

    #[test]
    fn non_udp_unchanged() {
        assert_eq!(udp_input_url("http://x/y.ts"), "http://x/y.ts");
        assert_eq!(udp_input_url(r"D:\a.ts"), r"D:\a.ts");
    }
}
