//! **并行分片转码桥** —— vkitcore/media/bridge_par.py 的 Rust 移植（§31.50）。
//!
//! 为什么存在：单条转码链整链吞吐只有 **0.63×实时**（解码 76fps 是纯解码段，
//! 含管道/编码/写盘的整链远低于此），"实时硬撑"必然周期性饿死——
//! 用户看到的就是"放一会儿卡一下"。多链并行可叠到 ~1.85×，让转码跑在播放前面。
//!
//! 架构（照搬 Python 版，含其全部实测教训）：
//! 1. **切块**：ffmpeg segment 复用器一遍 `-c copy` 顺序读，按**关键帧**切开
//!    （不重不漏；每片数帧定时长——容器时长会因时间戳跳变虚报）。
//! 2. **并行转**：滑窗内最多 4 段同时跑 mini 链（抽裸流→davs2→NVENC），
//!    stderr 一律写**文件**（12 个进程的 PIPE 没人排空会写满卡死）。
//! 3. **按序拼**：协调线程按顺序把分片追加进增长文件；每段
//!    `-output_ts_offset <段起点>` 平移时间戳；`-frames:v N` 按帧数精确切
//!    （`-t 秒` 会切出半个 PES，每个接缝报 Packet corrupt）。
//! 4. **边转边播**：发送线程按 ~1.05× 推回环 UDP，盯"领先量"：
//!    落后全速追（1.20×），领先超 1.2s 就等（UDP 缓冲灌了就丢包）。
//! 5. **缓存**：转完的整片留 `shared/bridge_cache/`（.done 标记才算完整），
//!    同文件第二次打开直接本地播（0 等待、可拖动）；超上限按最久未用删。

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::bridge::{encoder_args, pick_h264_encoder};
pub use crate::bridge::is_local;

// ------------------------------------------------------------ 参数（Python 版实测值）
const CHUNK_S: f64 = 3.0; // 分片小：首片转完越快起播越早
const WORKERS: usize = 4; // 4 段 ≈ 1.85×、6 段 ≈ 2.06×，再加收益很小
const THREADS_MULTI: u32 = 4; // 4 段 × 4 线程 = 16，不打架
const SEND_RATE: f64 = 1.05; // 只比实时快一点点
const SEND_CATCHUP: f64 = 1.20; // 落后时追赶
const LEAD_MIN: f64 = 0.5; // 领先量下限（内容秒）
const LEAD_MAX: f64 = 1.2; // ★ 领先量不能大：6s 领先会让 UDP 接收缓冲每 5~6s 溢出
const READY_S: f64 = 3.0; // 起播门槛（内容秒）
const SEND_DELAY_MS: u64 = 1200; // 读端（mpv）先绑 socket
const SEND_TICK: f64 = 0.01; // 发送节流轮长（越小越平滑）
const SEND_BURST_TICKS: u64 = 3; // 单跳最多几跳的量
const CACHE_CAP_MAX: u64 = 2 * 1024 * 1024 * 1024; // 缓存上限基线（原 6 GB，改为按盘余量自适应）
const CACHE_RESERVE: u64 = 2 * 1024 * 1024 * 1024; // 给系统留的余量：盘再大也不吃满
const PKT: usize = 1316; // 与桥一样的 TS 包粒度
const LOOPBACK: &str = "127.0.0.1";

// ------------------------------------------------------------ 共享状态
#[derive(Default)]
struct Shared {
    state: String, // starting | running | done | error
    error: String,
    mode: String, // par | cache
    lead: f64,
    done_chunks: usize,
    total_chunks: usize,
    plan_s: f64,
    /// (全局起点, 内容时长, 字节数) —— 字节↔内容秒换算表
    frags: Vec<(f64, f64, u64)>,
    /// (已拼字节, 正在写、还没拼的分片路径) —— **原子快照**，发送线程一轮读一次
    bind: (u64, Option<PathBuf>),
    stop: bool,
}

/// 在飞的子进程（协调线程写入，stop()/alive() 消费）。
type Inflight = Arc<Mutex<HashMap<usize, Vec<Child>>>>;

/// 本地文件的并行转码桥。
pub struct ParBridge {
    sh: Arc<Mutex<Shared>>,
    inflight: Inflight,
    out: PathBuf,
    mark: PathBuf,
    port: u16,
    segdir: Mutex<Option<PathBuf>>,
    fragdir: Mutex<Option<PathBuf>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    stopped: Arc<AtomicBool>,
}

impl ParBridge {
    /// 起桥。`url` 必须是本地文件。起不来返回 Err（原因在消息里），调用方回退实时桥。
    pub fn start(ffmpeg: &Path, davs2: &Path, url: &str, crf: i32) -> Result<ParBridge, String> {
        let ffmpeg = ffmpeg.to_path_buf();
        let davs2 = davs2.to_path_buf();
        if !is_local(url) {
            return Err("并行桥只接本地文件".into());
        }
        let sh = Arc::new(Mutex::new(Shared {
            state: "starting".into(),
            mode: "par".into(),
            ..Default::default()
        }));
        let inflight: Inflight = Arc::new(Mutex::new(HashMap::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        // 兜底：清掉上次异常退出留下的分片目录（24 小时未动的才动，见函数注释）
        let _ = cleanup_stale_scratch();

        // ---- 命中缓存：直接当本地文件播（0 等待、可拖动）
        let (out, mark) = cache_paths(url, crf);
        if let (Some(f), Some(m)) = (&out, &mark) {
            let complete =
                f.is_file() && m.is_file() && f.metadata().map(|x| x.len() > 0).unwrap_or(false);
            if complete {
                *sh.lock().unwrap() = Shared {
                    state: "running".into(),
                    mode: "cache".into(),
                    ..Default::default()
                };
                return Ok(ParBridge {
                    sh,
                    inflight,
                    out: f.clone(),
                    mark: m.clone(),
                    port: 0,
                    segdir: Mutex::new(None),
                    fragdir: Mutex::new(None),
                    threads: Mutex::new(Vec::new()),
                    stopped,
                });
            }
        }

        // ---- 切分片：一遍 -c copy 顺序读，按关键帧切开（不重不漏）
        // 分片落数据目录的 scratch，不再默认吃系统盘（C 盘常年只剩几 GB）
        let segdir = crate::config::scratch_dir()
            .join(format!("vqa_seg_{}_{}", std::process::id(), epoch_ms()));
        let segs = split_segments(&ffmpeg, Path::new(url), &segdir, CHUNK_S);
        if segs.is_empty() {
            return Err("切不出分片（segment 复用器失败）".into());
        }
        let mut chunks: Vec<(PathBuf, f64, f64, u64)> = Vec::new(); // (路径, 全局起点, 时长, 帧数)
        let mut acc = 0.0f64;
        for (p, dur, nfr) in segs {
            chunks.push((p, acc, dur, nfr));
            acc += dur;
        }
        // 备好输出（＝缓存文件本体；转完才写 .done）
        let (out, mark) = match (out, mark) {
            (Some(f), Some(m)) => (f, m),
            _ => {
                let tmp = crate::config::scratch_dir().join("vqa_par");
                std::fs::create_dir_all(&tmp).map_err(|e| format!("没有可写的临时目录：{e}"))?;
                let key = cache_key_hex(url, crf);
                (tmp.join(key.clone() + ".ts"), tmp.join(key + ".done"))
            }
        };
        let cap_dir = out.parent().map(|p| p.to_path_buf()).unwrap_or_else(crate::config::scratch_dir);
        prune_cache(out.parent(), cache_cap(&cap_dir), Some(&out));
        for p in [&out, &mark] {
            let _ = std::fs::remove_file(p); // 上次留下的半截/旧标记先清掉
        }
        let fragdir = crate::config::scratch_dir().join(format!(
            "vqa_frag_{}_{}",
            std::process::id(),
            out.file_stem().and_then(|s| s.to_str()).unwrap_or("x")
        ));
        std::fs::create_dir_all(&fragdir).map_err(|e| format!("建不下分片目录：{e}"))?;

        // ---- 音频一次性规范化（音画同步的关键）：
        //   源 TS 的音轨普遍残缺（PTS 迟开、内部断口、结尾溢出）——逐片 -ss
        //   从残缺 TS 抽音频每片都有偏差，累积起来就是音画不同步。
        //   先整轨重编码一遍：async=1 填断口 + first_pts=0 对齐零点，
        //   得到与视频等长、时间连续的 M4A——M4A 的 -ss 定位是帧精确的。
        let audio_norm_path = fragdir.join("audio_norm.m4a");
        let url_s = url.to_string();
        let mut an = crate::ffmpeg::hidden(&ffmpeg);
        an.args([
            "-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i", url_s.as_str(),
        ]);
        an.args(["-map", "0:a:0?"]);
        // ★ -ac 2 下混立体声：源音轨可能是 5.1，且 ipod 容器会写入未命名的
        //   "6 channels" 布局，分片的 aac 编码器直接拒开（实测 5.1 源）。
        //   广播质检监听立体声足够。
        an.args(["-ac", "2"]);
        an.args(["-af", "aresample=async=1:first_pts=0", "-c:a", "aac", "-b:a", "96k"]);
        an.arg("-f").arg("ipod").arg(&audio_norm_path);
        an.stdin(Stdio::null());
        let audio_norm: Option<PathBuf> = match an.status() {
            Ok(st)
                if st.success()
                    && audio_norm_path.is_file()
                    && audio_norm_path.metadata().map(|m| m.len() > 0).unwrap_or(false) =>
            {
                Some(audio_norm_path)
            }
            _ => None, // 无音轨或抽不出来：分片退回从源直取（旧行为）
        };

        {
            let mut s = sh.lock().unwrap();
            s.total_chunks = chunks.len();
            s.plan_s = acc.max(1.0);
        }
        let port = crate::bridge::free_udp_port()
            .map_err(|e| format!("挑 UDP 端口失败：{e}"))?;
        // 克隆好线程要用的 Arc（bridge 构造会 move 掉 stopped）
        let stop_c = stopped.clone();
        let stop_s = stopped.clone();

        let bridge = ParBridge {
            sh: sh.clone(),
            inflight: inflight.clone(),
            out,
            mark,
            port,
            segdir: Mutex::new(Some(segdir)),
            fragdir: Mutex::new(Some(fragdir.clone())),
            threads: Mutex::new(Vec::new()),
            stopped,
        };

        // 两条线程：协调（转 + 按序拼）、发送（边转边播）
        let sh_c = sh.clone();
        let if_c = inflight.clone();
        let ffmpeg_c = ffmpeg.clone();
        let davs2_c = davs2.clone();
        let src = url.to_string();
        let out_c = bridge.out.clone();
        let mark_c = bridge.mark.clone();
        let fd = fragdir.clone();
        let an_c = audio_norm.clone();
        let t_coord = std::thread::Builder::new()
            .name("vqa-par-coord".into())
            .spawn(move || {
                coord_loop(
                    &sh_c, &if_c, &stop_c, &ffmpeg_c, &davs2_c, &src, crf, an_c.as_deref(),
                    chunks, &out_c, &mark_c, &fd,
                )
            })
            .map_err(|e| format!("协调线程起不来：{e}"))?;
        let sh_s = sh.clone();
        let out_s = bridge.out.clone();
        let t_send = std::thread::Builder::new()
            .name("vqa-par-send".into())
            .spawn(move || send_loop(&sh_s, &stop_s, &out_s, port))
            .map_err(|e| format!("发送线程起不来：{e}"))?;
        *bridge.threads.lock().unwrap() = vec![t_coord, t_send];
        Ok(bridge)
    }

    /// 给播放器读的地址：命中缓存是文件路径，否则回环 UDP。
    pub fn url(&self) -> String {
        let s = self.sh.lock().unwrap();
        if s.state == "error" {
            return String::new();
        }
        if s.mode == "cache" {
            return self.out.to_string_lossy().to_string();
        }
        if self.port > 0 {
            format!("udp://@{LOOPBACK}:{}", self.port)
        } else {
            String::new()
        }
    }

    pub fn mode(&self) -> String {
        self.sh.lock().unwrap().mode.clone()
    }

    pub fn is_file(&self) -> bool {
        self.sh.lock().unwrap().mode == "cache"
    }

    /// 给人看的一行进度（「桥」chip 用）。
    pub fn progress(&self) -> String {
        let s = self.sh.lock().unwrap();
        if s.mode == "cache" {
            return "缓存".into();
        }
        if s.state == "done" {
            return "转完".into();
        }
        if s.total_chunks > 0 {
            return format!("{}/{}", s.done_chunks, s.total_chunks);
        }
        String::new()
    }

    pub fn error(&self) -> String {
        self.sh.lock().unwrap().error.clone()
    }

    pub fn stderr_tail(&self) -> String {
        self.error()
    }

    pub fn alive(&self) -> usize {
        let mut n = 0;
        for ps in self.inflight.lock().unwrap().values_mut() {
            for p in ps.iter_mut() {
                if p.try_wait().map(|s| s.is_none()).unwrap_or(true) {
                    n += 1;
                }
            }
        }
        n
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.sh.lock().unwrap().stop = true;
        if let Ok(mut map) = self.inflight.lock() {
            for ps in map.values_mut() {
                for p in ps.iter_mut() {
                    let _ = p.kill();
                    let _ = p.wait();
                }
            }
            map.clear();
        }
        let st = self.sh.lock().unwrap().state.clone();
        if self.sh.lock().unwrap().mode == "par" && st != "done" && st != "error" {
            // 没转完的输出直接删（免得当下次缓存）
            let _ = std::fs::remove_file(&self.out);
            let _ = std::fs::remove_file(&self.mark);
        }
        if let Some(d) = self.segdir.lock().unwrap().take() {
            let _ = std::fs::remove_dir_all(d);
        }
        if let Some(d) = self.fragdir.lock().unwrap().take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

impl Drop for ParBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ------------------------------------------------------------ 切分
/// ffmpeg segment 复用器：同一遍读取里按关键帧切（不重不漏，每片以关键帧开头）。
/// ★ 不做 `-ss` 定位——对 TS 定位不准（实测偏 0.5~2.3 秒，接缝处内容空洞）。
fn split_segments(
    ffmpeg: &Path,
    src: &Path,
    outdir: &Path,
    chunk_s: f64,
) -> Vec<(PathBuf, f64, u64)> {
    let _ = std::fs::create_dir_all(outdir);
    let seg_time = format!("{:.3}", chunk_s);
    let pat = outdir.join("%04d.ts");
    let src_s = src.to_string_lossy().to_string();
    let mut cmd = crate::ffmpeg::hidden(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i", src_s.as_str()])
        .args(["-map", "0:v:0", "-c", "copy", "-f", "segment"])
        .args(["-segment_format", "mpegts"])
        .args(["-segment_time"])
        .arg(seg_time)
        .args(["-reset_timestamps", "1"])
        .arg(pat)
        .stdin(Stdio::null());
    if cmd.status().map(|s| !s.success()).unwrap_or(true) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut names: Vec<PathBuf> = std::fs::read_dir(outdir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "ts").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    for p in names {
        match segment_frames(ffmpeg, &p) {
            Some((nfr, fps)) if nfr > 0 && fps > 0.0 => out.push((p, nfr as f64 / fps, nfr)),
            _ => {
                let d = file_duration(ffmpeg, &p);
                if d > 0.0 {
                    out.push((p, d, 0));
                }
            }
        }
    }
    out
}

/// 一个分片的 (帧数, 帧率)。数不出返回 None。
/// 帧数用途：编码时 `-frames:v N` 精确切整帧（`-t 秒` 会切出半个 PES）。
fn segment_frames(ffmpeg: &Path, path: &Path) -> Option<(u64, f64)> {
    let fp = ffprobe_of(ffmpeg)?;
    let mut cmd = crate::ffmpeg::hidden(&fp);
    cmd.args(["-v", "error", "-select_streams", "v:0", "-count_packets"])
        .args(["-show_entries", "stream=r_frame_rate,nb_read_packets", "-of", "csv=p=0"])
        .arg(path);
    let out = cmd.output().ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    for line in txt.lines() {
        let line = line.trim();
        // 形如 "50/1,150"
        let mut it = line.split(',');
        let rate = it.next()?;
        let n = it.next()?;
        let nfr: u64 = n.trim().parse().ok()?;
        let mut ri = rate.split('/');
        let a: f64 = ri.next()?.trim().parse().ok()?;
        let b: f64 = ri.next().map(|x| x.trim().parse().unwrap_or(1.0)).unwrap_or(1.0);
        if a > 0.0 {
            return Some((nfr, a / b.max(1.0)));
        }
    }
    None
}

fn ffprobe_of(ffmpeg: &Path) -> Option<PathBuf> {
    let fp = ffmpeg.parent()?.join("ffprobe.exe");
    if fp.is_file() {
        Some(fp)
    } else {
        None
    }
}

fn file_duration(ffmpeg: &Path, path: &Path) -> f64 {
    if let Some(fp) = ffprobe_of(ffmpeg) {
        let mut cmd = crate::ffmpeg::hidden(&fp);
        cmd.args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(path);
        if let Ok(out) = cmd.output() {
            if let Ok(v) = String::from_utf8_lossy(&out.stdout).trim().parse::<f64>() {
                if v > 0.0 {
                    return v;
                }
            }
        }
    }
    0.0
}

// ------------------------------------------------------------ 单段命令
/// 一段的迷你链三条命令 `(抽裸流, 解码, 编码)`。
///
/// ★ 关键点（每一条都是 Python 版踩过的坑）：
///   1. 视频从**分片文件**读（不对源 -ss 定位）；
///   2. `-output_ts_offset <段起点>` 平移回全局时间轴（否则时间戳倒流）；
///   3. 输出侧 `-frames:v N` 按帧数切（秒切会留半个 PES，接缝报 corrupt）；
///   4. **不加** `-muxdelay 0`——会抹掉 TS 开头的 PAT/PMT，播放端直接不认格式。
fn fragment_cmds(
    ffmpeg: &Path,
    davs2: &Path,
    src: &str,
    audio_norm: Option<&Path>,
    chunk_file: &Path,
    offset: f64,
    dur: f64,
    out: &Path,
    crf: i32,
    threads: u32,
    frames: u64,
) -> (Command, Command, Command) {
    let offset_s = format!("{:.3}", offset);
    let dur_s = format!("{:.3}", dur);
    let threads_s = threads.to_string();
    let frames_s = frames.to_string();

    let chunk_s = chunk_file.to_string_lossy().to_string();
    let mut ex = crate::ffmpeg::hidden(ffmpeg);
    ex.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i", chunk_s.as_str()]);
    ex.args(["-map", "0:v:0", "-c", "copy", "-f", "avs2", "-"]);

    let mut dec = crate::ffmpeg::hidden(davs2);
    dec.args(["-i", "-", "-o", "stdout"]);
    dec.arg("-t").arg(threads_s);

    let enc_str = pick_h264_encoder(ffmpeg);
    let mut enc = crate::ffmpeg::hidden(ffmpeg);
    enc.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    enc.args(["-f", "yuv4mpegpipe", "-i", "-"]);
    enc.arg("-ss").arg(offset_s.as_str()); // 音频从规范化 M4A 取（按段窗口，定位帧精确）
    enc.arg("-t").arg(dur_s.as_str());
    match audio_norm {
        Some(a) => {
            enc.args(["-i", a.to_string_lossy().as_ref()]);
        }
        None => {
            enc.args(["-i", src]);
        }
    }
    enc.args(["-map", "0:v:0", "-map", "1:a:0?"]);
    enc.arg("-vf").arg("format=yuv420p"); // 10-bit y4m → nvenc 要 8-bit
    for a in encoder_args(&enc_str, crf) {
        enc.arg(a);
    }
    // aresample=async=1：音频时间戳断口填静音（否则播放端 resync 丢帧）
    enc.args(["-af", "aresample=async=1", "-c:a", "aac", "-b:a", "96k"]);
    if frames > 0 {
        enc.arg("-frames:v").arg(frames_s);
    } else {
        enc.arg("-t").arg(dur_s.as_str());
    }
    enc.arg("-output_ts_offset").arg(offset_s);
    enc.arg("-f").arg("mpegts").arg(out);
    (ex, dec, enc)
}

fn stdio_file(f: Option<std::fs::File>) -> Stdio {
    match f {
        Some(f) => Stdio::from(f),
        None => Stdio::null(),
    }
}

fn frag_path(dir: &Path, idx: usize) -> PathBuf {
    dir.join(format!("{:05}.ts", idx))
}

fn err_tail(p: Option<&PathBuf>) -> Option<String> {
    let txt = std::fs::read_to_string(p?).ok()?;
    let last = txt.lines().rev().find(|l| !l.trim().is_empty())?;
    Some(last.trim().chars().take(200).collect())
}

// ------------------------------------------------------------ 协调线程
#[allow(clippy::too_many_arguments)]
fn coord_loop(
    sh: &Arc<Mutex<Shared>>,
    inflight: &Inflight,
    stop: &Arc<AtomicBool>,
    ffmpeg: &Path,
    davs2: &Path,
    src: &str,
    crf: i32,
    audio_norm: Option<&Path>,
    chunks: Vec<(PathBuf, f64, f64, u64)>,
    out: &Path,
    mark: &Path,
    fragdir: &Path,
) {
    let total = chunks.len();
    let mut fout = match std::fs::File::create(out) {
        Ok(f) => f,
        Err(e) => {
            let mut s = sh.lock().unwrap();
            s.error = format!("输出文件打不开：{e}");
            s.state = "error".into();
            return;
        }
    };
    let threads_per_dec = if WORKERS <= 1 { 8 } else { THREADS_MULTI };
    let mut nxt_start = 0usize;
    let mut nxt_append = 0usize;
    let mut appended: u64 = 0;
    let mut errs: HashMap<usize, PathBuf> = Default::default();

    while nxt_append < total && !stop.load(Ordering::SeqCst) {
        // 滑窗：保持 workers 段在飞
        while nxt_start < total
            && !stop.load(Ordering::SeqCst)
            && nxt_start < nxt_append + WORKERS
        {
            let idx = nxt_start;
            let c = &chunks[idx];
            let frag = frag_path(fragdir, idx);
            let (mut ex, mut dec, mut enc) = fragment_cmds(
                ffmpeg, davs2, src, audio_norm, &c.0, c.1, c.2, &frag, crf,
                threads_per_dec, c.3,
            );
            let errp = frag.with_extension("err");
            errs.insert(idx, errp.clone());
            let e1 = std::fs::File::create(&errp).ok();
            // 同一句柄的副本：三个进程共享一个文件指针，追加写不打架
            let e1b = e1.as_ref().and_then(|f| f.try_clone().ok());
            let e1c = e1.as_ref().and_then(|f| f.try_clone().ok());
            // stderr 一律写文件：PIPE 没人排空写满 64KB 会把子进程卡死
            ex.stdout(Stdio::piped())
                .stderr(stdio_file(e1))
                .stdin(Stdio::null());
            let spawned = (|| -> Result<Vec<Child>, String> {
                let mut p1 = ex.spawn().map_err(|e| format!("抽流段：{e}"))?;
                let p1_out = p1.stdout.take().unwrap();
                dec.stdin(p1_out)
                    .stdout(Stdio::piped())
                    .stderr(stdio_file(e1b));
                let mut p2 = dec.spawn().map_err(|e| format!("解码段：{e}"))?;
                let p2_out = p2.stdout.take().unwrap();
                enc.stdin(p2_out).stderr(stdio_file(e1c));
                let p3 = enc.spawn().map_err(|e| format!("编码段：{e}"))?;
                Ok(vec![p1, p2, p3])
            })();
            match spawned {
                Ok(ps) => {
                    inflight.lock().unwrap().insert(idx, ps);
                    nxt_start += 1;
                }
                Err(e) => {
                    let mut s = sh.lock().unwrap();
                    s.error = e;
                    s.state = "error".into();
                    let _ = fout.flush();
                    return;
                }
            }
            // 标记"正在写的段"（发送线程 tail 它）
            {
                let mut s = sh.lock().unwrap();
                s.bind = (appended, Some(frag.clone()));
            }
        }
        // 等当前该拼的这段收工（编码段退出码 0 = 成功）
        {
            let mut map = inflight.lock().unwrap();
            let ps = match map.get_mut(&nxt_append) {
                Some(ps) => ps,
                None => break,
            };
            let enc_pid = ps.last().map(|x| x.id()).unwrap_or(0);
            for p in ps {
                match p.wait() {
                    Ok(st) => {
                        if !st.success() && p.id() == enc_pid {
                            let mut s = sh.lock().unwrap();
                            s.error = err_tail(errs.get(&nxt_append))
                                .unwrap_or_else(|| "转码段失败".into());
                            s.state = "error".into();
                            return;
                        }
                    }
                    Err(_) => {
                        let mut s = sh.lock().unwrap();
                        s.error = "等待转码段失败".into();
                        s.state = "error".into();
                        return;
                    }
                }
            }
        }
        inflight.lock().unwrap().remove(&nxt_append);
        let frag = frag_path(fragdir, nxt_append);
        let mut n: u64 = 0;
        if let Ok(mut f) = std::fs::File::open(&frag) {
            let mut buf = vec![0u8; 1 << 20];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(k) => {
                        if fout.write_all(&buf[..k]).is_err() {
                            let mut s = sh.lock().unwrap();
                            s.error = "写输出文件失败".into();
                            s.state = "error".into();
                            return;
                        }
                        n += k as u64;
                    }
                    Err(_) => break,
                }
            }
            let _ = fout.flush();
        }
        {
            let mut s = sh.lock().unwrap();
            s.frags.push((chunks[nxt_append].1, chunks[nxt_append].2, n));
            appended += n;
            s.bind = (appended, None);
            s.done_chunks = nxt_append + 1;
        }
        let _ = std::fs::remove_file(&frag);
        nxt_append += 1;
    }
    drop(fout);
    if stop.load(Ordering::SeqCst) {
        return;
    }
    // 全部转完 → 写标记（下次打开直接当本地文件播）
    if let Ok(md) = std::fs::metadata(out) {
        let _ = std::fs::write(mark, format!("{}\n", md.len()));
    }
    sh.lock().unwrap().state = "done".into();
}

// ------------------------------------------------------------ 发送线程
fn send_loop(sh: &Arc<Mutex<Shared>>, stop: &Arc<AtomicBool>, out: &Path, port: u16) {
    std::thread::sleep(Duration::from_millis(SEND_DELAY_MS)); // 读端先绑 socket
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            let mut s = sh.lock().unwrap();
            s.error = format!("回环 socket 建不起来：{e}");
            s.state = "error".into();
            return;
        }
    };
    let dest = format!("{LOOPBACK}:{port}");
    let mut sent: u64 = 0;
    let mut t0: Option<Instant> = None;
    let mut h_out: Option<(PathBuf, std::fs::File)> = None;
    let mut h_tail: Option<(PathBuf, std::fs::File)> = None;

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if t0.is_none() {
            let (ready, err) = {
                let s = sh.lock().unwrap();
                (
                    s.done_chunks >= 1 && buffered_s(&s) >= ready_s(&s),
                    s.state.clone(),
                )            };
            if !ready {
                if err == "error" {
                    return;
                }
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            t0 = Some(Instant::now());
            sh.lock().unwrap().state = "running".into();
        }
        let (appended, tail) = {
            let s = sh.lock().unwrap();
            s.bind.clone()
        };
        let mut avail = appended;
        if let Some(t) = &tail {
            avail += std::fs::metadata(t).map(|m| m.len()).unwrap_or(0);
        }
        if sent >= avail {
            let st = sh.lock().unwrap().state.clone();
            if st == "done" || st == "error" {
                return; // 源发完了
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let elapsed = t0.unwrap().elapsed().as_secs_f64();
        let lead = content_s(&sh.lock().unwrap(), sent) - elapsed;
        sh.lock().unwrap().lead = lead;
        if lead > LEAD_MAX {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let speed = if lead > LEAD_MIN { SEND_RATE } else { SEND_CATCHUP };
        let bps = avg_bps(sh);
        let mut n = (bps * speed * SEND_TICK) as u64;
        let cap = (bps * SEND_RATE * SEND_TICK * SEND_BURST_TICKS as f64) as u64;
        n = n.max(1024).min(cap);
        n = n.min(avail - sent);
        // 读逻辑流（跨 输出文件/在写分片 两个区）—— 句柄缓存，别每轮重开
        let mut data: Vec<u8> = Vec::with_capacity(n as usize);
        if sent < appended {
            let want = (appended - sent).min(n) as usize;
            if let Some(f) = cached_handle(&mut h_out, out) {
                if f.seek(std::io::SeekFrom::Start(sent)).is_ok() {
                    let mut tmp = vec![0u8; want];
                    if let Ok(k) = f.read(&mut tmp) {
                        data.extend_from_slice(&tmp[..k]);
                    }
                }
            }
        }
        if (data.len() as u64) < n {
            if let Some(t) = &tail {
                if let Some(f) = cached_handle(&mut h_tail, t) {
                    let off = sent + data.len() as u64 - appended;
                    if f.seek(std::io::SeekFrom::Start(off)).is_ok() {
                        let rest = n as usize - data.len();
                        let mut tmp = vec![0u8; rest];
                        if let Ok(k) = f.read(&mut tmp) {
                            data.extend_from_slice(&tmp[..k]);
                        }
                    }
                }
            }
        }
        if data.is_empty() {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        for i in (0..data.len()).step_by(PKT) {
            let _ = sock.send_to(&data[i..(i + PKT).min(data.len())], &dest);
        }
        sent += data.len() as u64;
        std::thread::sleep(Duration::from_secs_f64(SEND_TICK));
    }
}

/// 只读句柄缓存（路径一变就重开）——第一版每轮重 open 花 ~40ms，
/// 发送吞吐被循环开销卡死在 1.16MB/s（这条流要 3.5MB/s）。
fn cached_handle<'a>(
    holder: &'a mut Option<(PathBuf, std::fs::File)>,
    path: &Path,
) -> Option<&'a mut std::fs::File> {
    let need_reopen = match holder {
        Some((p, _)) => p != path,
        None => true,
    };
    if need_reopen {
        let f = std::fs::File::open(path).ok()?;
        *holder = Some((path.to_path_buf(), f));
    }
    holder.as_mut().map(|(_, f)| f)
}

fn ready_s(s: &Shared) -> f64 {
    READY_S.min((s.plan_s * 0.6).max(0.5))
}

fn buffered_s(s: &Shared) -> f64 {
    let mut base: f64 = s.frags.iter().map(|(_, d, _)| d).sum();
    if let Some(t) = s.bind.1.as_ref() {
        let sz = std::fs::metadata(t).map(|m| m.len()).unwrap_or(0);
        base += sz as f64 / avg_bps_locked(s);
    }
    base
}

fn content_s(s: &Shared, nbytes: u64) -> f64 {
    let mut left = nbytes;
    let mut acc = 0.0;
    for (_st, d, n) in &s.frags {
        if *n == 0 {
            continue;
        }
        if left <= *n {
            return acc + d * (left as f64 / *n as f64);
        }
        left -= *n;
        acc += d;
    }
    if left > 0 {
        acc + left as f64 / avg_bps_locked(s)
    } else {
        acc
    }
}

fn avg_bps(sh: &Arc<Mutex<Shared>>) -> f64 {
    avg_bps_locked(&sh.lock().unwrap())
}

fn avg_bps_locked(s: &Shared) -> f64 {
    let tb: u64 = s.frags.iter().map(|(_, _, n)| n).sum();
    let ts: f64 = s.frags.iter().map(|(_, d, _)| d).sum();
    if tb <= 0 || ts <= 0.0 {
        // 第一段没转完没有实测码率：用 4K50 的保守估计（4 MB/s）。
        // 偏大只会让领先量被高估 → 发送更保守，不会灌爆播放端。
        return 4.0 * 1024.0 * 1024.0;
    }
    tb as f64 / ts
}

// ------------------------------------------------------------ 缓存
fn cache_key_hex(src: &str, crf: i32) -> String {
    let raw = match std::fs::metadata(src) {
        Ok(md) => format!(
            "{}|{}|{}|{}",
            src,
            md.len(),
            md.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            crf
        ),
        Err(_) => format!("{src}|?|?|{crf}"),
    };
    // FNV-1a 64（缓存键用）
    let mut h: u64 = 0xcbf29ce484222325;
    for b in raw.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn cache_paths(src: &str, crf: i32) -> (Option<PathBuf>, Option<PathBuf>) {
    let folder = crate::config::shared_dir().join("bridge_cache");
    if std::fs::create_dir_all(&folder).is_err() {
        return (None, None);
    }
    let key = cache_key_hex(src, crf);
    (
        Some(folder.join(key.clone() + ".ts")),
        Some(folder.join(key + ".done")),
    )
}

/// 目标目录所在卷的可用字节数（Win32 `GetDiskFreeSpaceExW`；取不到返回 None）。
#[cfg(windows)]
fn free_space(dir: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = HSTRING::from(dir.to_string_lossy().as_ref());
    let mut avail: u64 = 0;
    unsafe {
        GetDiskFreeSpaceExW(&w, Some(&mut avail), None, None)
            .ok()
            .map(|_| avail)
    }
}

#[cfg(not(windows))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
}

/// 自适应缓存上限：`min(CACHE_CAP_MAX, 可用 - CACHE_RESERVE)`，保底 256 MB。
/// 开发机与发布机的盘位、余量都不同，写死一个值要么浪费要么爆盘。
fn cache_cap(dir: &Path) -> u64 {
    const FLOOR: u64 = 256 * 1024 * 1024;
    match free_space(dir) {
        Some(avail) => avail
            .saturating_sub(CACHE_RESERVE)
            .min(CACHE_CAP_MAX)
            .max(FLOOR),
        None => CACHE_CAP_MAX,
    }
}

/// 清掉异常退出留下的分片目录，返回删除个数。
///
/// 正常停止时 `stop()` 会自己删 seg/frag，只有进程被强杀或崩溃才留下残渣；
/// 这里只动「24 小时内没有改动过」的 `vqa_seg_*` / `vqa_frag_*` / `vqa_par`，
/// 正在播放的实例（哪怕同时开着 `cargo run` 和安装版）不会被误删。
pub fn cleanup_stale_scratch() -> usize {
    const STALE_S: u64 = 24 * 3600;
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(STALE_S))
        .unwrap_or(std::time::UNIX_EPOCH);
    let mut removed = 0usize;
    // 新位置（数据目录\scratch）与旧位置（系统临时目录，历史版本留下的）都扫
    for root in [crate::config::scratch_dir(), std::env::temp_dir()] {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = match p.file_name().and_then(|s| s.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if !(name.starts_with("vqa_seg_")
                || name.starts_with("vqa_frag_")
                || name == "vqa_par")
            {
                continue;
            }
            // 目录内最新一次写入时间；空目录退回目录自身的 mtime
            let newest = std::fs::read_dir(&p)
                .ok()
                .and_then(|ds| {
                    ds.filter_map(|x| x.ok())
                        .filter_map(|x| x.metadata().ok())
                        .filter_map(|m| m.modified().ok())
                        .max()
                })
                .or_else(|| p.metadata().ok().and_then(|m| m.modified().ok()));
            if newest.map(|t| t < cutoff).unwrap_or(false) && std::fs::remove_dir_all(&p).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

fn prune_cache(folder: Option<&Path>, cap: u64, keep: Option<&Path>) {
    let folder = match folder {
        Some(f) if f.is_dir() => f,
        _ => return,
    };
    let mut items: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    if let Ok(rd) = std::fs::read_dir(folder) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if let Ok(md) = p.metadata() {
                if md.is_file() {
                    total += md.len();
                    items.push((
                        md.modified().unwrap_or(std::time::UNIX_EPOCH),
                        md.len(),
                        p,
                    ));
                }
            }
        }
    }
    if total <= cap {
        return;
    }
    items.sort();
    let mut freed: u64 = 0;
    for (_mt, size, p) in items {
        if total - freed <= cap {
            break;
        }
        if let Some(k) = keep {
            if p == k {
                continue;
            }
        }
        if std::fs::remove_file(&p).is_ok() {
            freed += size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自适应上限必须落在 [256 MB, 2 GB]：写死或算溢出都会让缓存要么爆盘要么频繁重转码。
    #[test]
    fn cache_cap_stays_in_bounds() {
        let cap = cache_cap(Path::new("."));
        assert!(cap >= 256 * 1024 * 1024, "低于保底 256 MB：{cap}");
        assert!(cap <= CACHE_CAP_MAX, "超过基线 2 GB：{cap}");
    }
}
