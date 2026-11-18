//! TS 文件/流的静态解析 —— `vkitcore/media/tsfile.py` 的逐语义移植。
//!
//! 只做解析、不带业务概念：包同步 188/204 → PSI(PAT/PMT) → PES 头 →
//! PCR 采样 → PID 统计 → 码率区间 + TR 101 290 监测 + PCR 抖动(PLL)。
//!
//! 约定（照 ISO/IEC 13818-1 / EasyICE 的口径，与 Python 版一致）：
//! * 包长自动判：188 优先；204 = 188 + 16 字节 RS 前向纠错。
//! * 码率区间 = 两个 PCR 包之间「有效数据」字节 ÷ PCR 差。有效数据 =
//!   所属节目的 PAT / PMT / PCR / 音视频 PID 包；PCR 包按有效计并
//!   **减 11 字节**（PCR 在自适应字段里占 8 字节 + 字段头 3 字节）。
//! * ES 级解析不做（视频序列层/GOP 走 ffprobe，见 [`gop_list`]）。
//! * stream_type 只到表级；AVS 系按常见厂商约定收录；未知原样显示，不猜。

mod crc;
mod pcr;
mod scanner;
pub mod sections;
mod tr290;

pub use pcr::{compute_pcr_jitter, PcrSample};
pub use scanner::Scanner;
pub use tr290::{tr290_defaults, TrOverrides};

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

pub use crc::crc32_mpeg;

pub const SYNC: u8 = 0x47;
pub const PKT_PLAIN: usize = 188;
pub const PKT_RS: usize = 204;
pub const NULL_PID: u16 = 0x1FFF;

const _MAX_RATE_POINTS: usize = 120_000; // 码率点上限，超出隔点抽稀
const _MAX_PES_SAMPLES: usize = 300_000; // 单视频 PID 时间戳采样上限
const _MAX_SI_ITEMS: usize = 400; // 单张 SI 表条目上限
const _MAX_PCR_RAW: usize = 900; // PCR 原始序列滑动窗口（约 30s）
const _CHUNK: usize = 4 * 1024 * 1024; // 流式读取块

/// stream_type → 名称。AVS 系条目为「厂商约定」值，未知显示 0x%02X。
pub fn stream_type_name(t: u8) -> String {
    match t {
        0x01 => "MPEG-1 视频".into(),
        0x02 => "MPEG-2 视频".into(),
        0x03 => "MPEG-1 音频".into(),
        0x04 => "MPEG-2 音频".into(),
        0x0F => "AAC (ADTS)".into(),
        0x10 => "MPEG-4 视频".into(),
        0x11 => "AAC (LATM)".into(),
        0x1B => "H.264/AVC".into(),
        0x24 => "H.265/HEVC".into(),
        0x80 => "H.264 (厂商约定)".into(),
        0x81 => "AC-3".into(),
        0x82 => "SCTE-27".into(),
        0x83 => "TrueHD (厂商约定)".into(),
        0x87 => "E-AC-3 (厂商约定)".into(),
        0x42 => "AVS 视频 (厂商约定)".into(),
        0xD2 => "AVS2 视频 (厂商约定)".into(),
        0xD3 => "AVS3 视频 (厂商约定)".into(),
        0xD1 => "AVS3 音频 (厂商约定)".into(),
        _ => format!("0x{t:02X}"),
    }
}

fn is_video_type(t: u8) -> bool {
    matches!(t, 0x01 | 0x02 | 0x10 | 0x1B | 0x24 | 0x42 | 0x80 | 0xD2 | 0xD3)
}

fn is_audio_type(t: u8) -> bool {
    matches!(t, 0x03 | 0x04 | 0x0F | 0x11 | 0x81 | 0x83 | 0x87 | 0xD1)
}

pub fn kind_of_stream_type(t: u8) -> &'static str {
    if is_video_type(t) {
        "video"
    } else if is_audio_type(t) {
        "audio"
    } else {
        "other"
    }
}

fn is_si_pid(pid: u16) -> bool {
    matches!(pid, 0x01 | 0x02 | 0x10 | 0x11 | 0x12 | 0x13 | 0x14 | 0x1E | 0x1F)
}

// ---------------------------------------------------------------- 数据

/// 一个 PID 的累计统计。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PidStat {
    pub pid: u16,
    pub kind: String, // video / audio / psi / si / pcr / unknown
    pub packets: u64,
    pub bytes: u64,
    pub payload_bytes: u64,
    pub scrambled: u64, // 加扰包字节数
}

impl PidStat {
    fn new(pid: u16, kind: &str) -> Self {
        Self {
            pid,
            kind: kind.into(),
            packets: 0,
            bytes: 0,
            payload_bytes: 0,
            scrambled: 0,
        }
    }

    fn add(&mut self, size: usize, payload: usize, scrambled: bool) {
        self.packets += 1;
        self.bytes += size as u64;
        self.payload_bytes += payload as u64;
        if scrambled {
            self.scrambled += size as u64;
        }
    }
}

/// PMT 里的一条 elementary stream。
#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamInfo {
    pub pid: u16,
    pub stream_type: u8,
    pub kind: String,
}

impl StreamInfo {
    pub fn new(pid: u16, stream_type: u8) -> Self {
        Self {
            pid,
            stream_type,
            kind: kind_of_stream_type(stream_type).into(),
        }
    }

    pub fn type_name(&self) -> String {
        stream_type_name(self.stream_type)
    }
}

/// PAT/PMT 解出的一个节目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Program {
    pub number: u16,
    pub pmt_pid: u16,
    pub pcr_pid: u16,
    pub streams: Vec<StreamInfo>,
}

impl Program {
    fn new(number: u16, pmt_pid: u16) -> Self {
        Self {
            number,
            pmt_pid,
            pcr_pid: 0x1FFF,
            streams: Vec::new(),
        }
    }

    pub fn by_kind(&self, kind: &str) -> Vec<&StreamInfo> {
        self.streams.iter().filter(|s| s.kind == kind).collect()
    }
}

/// 一个码率区间：t0→t1（秒，PCR 时钟）之间走了 payload 字节。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RatePoint {
    pub t0: f64,
    pub t1: f64,
    pub payload: u64,
}

impl RatePoint {
    pub fn seconds(&self) -> f64 {
        (self.t1 - self.t0).max(1e-9)
    }

    pub fn bps(&self) -> f64 {
        self.payload as f64 * 8.0 / self.seconds()
    }
}

/// SI 表条目（字段随表而异，UI 通用渲染键值）。
pub type SiItem = serde_json::Map<String, serde_json::Value>;

/// si 表的值：多数是条目列表，TDT 是单条时间文本。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum SiValue {
    List(Vec<SiItem>),
    Text(String),
}

/// TR 101 290 结果条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Tr290Item {
    pub level: u8,
    pub name: String,
    pub count: u64,
    pub first: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Gop {
    pub start: f64,
    pub dur: f64,
    pub frames: u64,
}

/// 一次扫描的全部产出（各子页从这里各取所需）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanResult {
    pub path: String,
    pub file_size: u64,
    pub packet_size: usize,
    pub not_ts: bool,  // 连 188/204 同步都对不上
    pub sync_errors: u64, // 伪同步/跳过的字节数
    pub packets: u64,
    pub pids: BTreeMap<u16, PidStat>,
    pub programs: Vec<Program>,
    pub has_pat: bool,
    pub has_pmt: bool,
    pub rate: Vec<RatePoint>, // 主节目码率区间
    pub pcr_pid: Option<u16>, // 码率区间用的 PCR PID
    pub first_pcr: Option<f64>,
    pub last_pcr: Option<f64>,
    // ---- 二期（§31.56.8）----
    pub si: BTreeMap<String, SiValue>,
    pub si_truncated: bool,
    /// 视频 pid → [(dts_s, pts_s, pcr_s)] 采样
    pub pes: BTreeMap<u16, Vec<(f64, f64, f64)>>,
    pub gop: Vec<Gop>,
    pub gop_note: String,
    pub tr290: Vec<Tr290Item>,
    /// 主 PCR PID 原始序列 [(参考x, PCR秒)]
    pub pcr_raw: Vec<(f64, f64)>,
    pub pcr_samples: Vec<PcrSample>,
    pub pcr_cbr: bool,
    /// 任一 PID 见到过加扰（encrypted 判据，单调）
    pub has_scrambled: bool,
}

impl ScanResult {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.into(),
            file_size: 0,
            packet_size: 0,
            not_ts: false,
            sync_errors: 0,
            packets: 0,
            pids: BTreeMap::new(),
            programs: Vec::new(),
            has_pat: false,
            has_pmt: false,
            rate: Vec::new(),
            pcr_pid: None,
            first_pcr: None,
            last_pcr: None,
            si: BTreeMap::new(),
            si_truncated: false,
            pes: BTreeMap::new(),
            gop: Vec::new(),
            gop_note: String::new(),
            tr290: Vec::new(),
            pcr_raw: Vec::new(),
            pcr_samples: Vec::new(),
            pcr_cbr: false,
            has_scrambled: false,
        }
    }

    // ---- 派生值 ----

    pub fn duration_pcr(&self) -> f64 {
        match (self.first_pcr, self.last_pcr) {
            (Some(a), Some(b)) => (b - a).max(0.0),
            _ => 0.0,
        }
    }

    pub fn encrypted(&self) -> bool {
        self.has_scrambled
    }

    /// 整段平均码率（bps）。优先 PCR 口径，否则文件大小÷PCR 时长。
    pub fn bitrate_avg(&self) -> f64 {
        if !self.rate.is_empty() {
            let total: u64 = self.rate.iter().map(|p| p.payload).sum();
            let span = self.rate.last().unwrap().t1 - self.rate.first().unwrap().t0;
            if span > 0.0 {
                return total as f64 * 8.0 / span;
            }
        }
        if self.duration_pcr() > 0.0 {
            return self.file_size as f64 * 8.0 / self.duration_pcr();
        }
        0.0
    }

    pub fn main_program(&self) -> Option<&Program> {
        self.programs
            .iter()
            .find(|p| !p.streams.is_empty())
            .or_else(|| self.programs.first())
    }

    pub fn top_pids(&self, n: usize) -> Vec<&PidStat> {
        let mut v: Vec<&PidStat> = self.pids.values().collect();
        v.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        v.truncate(n);
        v
    }

    /// 导出 JSON（总览页「导出 JSON」）。
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "file_size": self.file_size,
            "packet_size": self.packet_size,
            "not_ts": self.not_ts,
            "sync_errors": self.sync_errors,
            "packets": self.packets,
            "duration_pcr_s": (self.duration_pcr() * 1000.0).round() / 1000.0,
            "bitrate_avg_bps": self.bitrate_avg() as i64,
            "encrypted": self.encrypted(),
            "pcr_pid": self.pcr_pid,
            "programs": self.programs.iter().map(|p| serde_json::json!({
                "number": p.number, "pmt_pid": p.pmt_pid, "pcr_pid": p.pcr_pid,
                "streams": p.streams.iter().map(|s| serde_json::json!({
                    "pid": s.pid, "stream_type": s.stream_type,
                    "type_name": s.type_name(), "kind": s.kind,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "pids": self.pids.values().map(|s| serde_json::json!({
                "pid": s.pid, "kind": s.kind, "packets": s.packets,
                "bytes": s.bytes, "payload_bytes": s.payload_bytes,
                "scrambled": s.scrambled,
            })).collect::<Vec<_>>(),
            "rate_points": self.rate.len(),
            "si": self.si.iter().map(|(k, v)| (k.clone(), match v {
                SiValue::List(l) => serde_json::json!(l.len()),
                SiValue::Text(t) => serde_json::json!(t),
            })).collect::<serde_json::Map<_,_>>(),
            "si_truncated": self.si_truncated,
            "pes_samples": self.pes.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v.len())))
                .collect::<serde_json::Map<_,_>>(),
            "gop": self.gop.iter().take(50).collect::<Vec<_>>(),
            "gop_note": self.gop_note,
            "tr290": self.tr290,
        })
    }
}

// ---------------------------------------------------------------- 同步探测

/// 在头部里找第一个「以 0x47 起步、按 188/204 步进连续命中」的锚点。
/// 返回 (偏移, 包长)；找不到 = (0, 0)。容忍开头混进的少量垃圾字节。
pub fn find_sync(head: &[u8]) -> (usize, usize) {
    let lim = head.len();
    for start in 0..lim.min(8192) {
        if head[start] != SYNC {
            continue;
        }
        for ps in [PKT_PLAIN, PKT_RS] {
            let need = ((lim - start) / ps).min(24);
            if need >= 3
                && (0..need).all(|i| head[start + i * ps] == SYNC)
            {
                return (start, ps);
            }
        }
    }
    (0, 0)
}

/// 188 / 204 / 0（都不是）。要求连续多个包起点都是 0x47。
pub fn detect_packet_size(head: &[u8]) -> usize {
    find_sync(head).1
}

// ---------------------------------------------------------------- 扫描

/// 扫描进度回调（done, total 字节）。
pub type ProgressCb<'a> = &'a mut dyn FnMut(u64, u64);
/// 停止判据：返回 true 时中断，返回已扫部分。
pub type StopCb<'a> = &'a dyn Fn() -> bool;

/// 扫一个本地 TS 文件（或已录制的流样本）。同步阻塞，放后台线程跑。
pub fn scan_file(
    path: &std::path::Path,
    mut progress: Option<ProgressCb>,
    stop: Option<StopCb>,
    overrides: &TrOverrides,
) -> ScanResult {
    let mut res = ScanResult::new(&path.to_string_lossy());
    let Ok(meta) = std::fs::metadata(path) else {
        res.not_ts = true;
        return res;
    };
    res.file_size = meta.len();

    let Ok(mut f) = std::fs::File::open(path) else {
        res.not_ts = true;
        return res;
    };
    let mut head = vec![0u8; PKT_RS * 24];
    let Ok(n) = f.read(&mut head) else {
        res.not_ts = true;
        return res;
    };
    head.truncate(n);
    let (anchor, ps) = find_sync(&head);
    if ps == 0 {
        res.not_ts = true;
        return res;
    }
    res.packet_size = ps;
    res.sync_errors += anchor as u64;
    if f.seek(SeekFrom::Start(anchor as u64)).is_err() {
        res.not_ts = true;
        return res;
    }

    let mut sc = Scanner::new(res, overrides);
    sc.garbage = anchor as u64; // 锚点前的垃圾前缀按同步错误计

    let total = sc.res.file_size;
    let mut done: u64 = 0;
    let mut consumed: u64 = 0; // 已成包处理的字节（偏移基准）
    let mut tail: Vec<u8> = Vec::new();
    let mut next_report: u64 = 8 * 1024 * 1024;

    loop {
        let mut buf = vec![0u8; _CHUNK];
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        buf.truncate(n);
        let mut data = std::mem::take(&mut tail);
        data.extend_from_slice(&buf);
        let whole = data.len() / ps * ps;
        tail = data.split_off(whole);
        let body = data;

        let l = body.len();
        let mut pos = 0usize;
        while pos + ps <= l {
            let next_ok = pos + 2 * ps <= l && body[pos + ps] != SYNC;
            if body[pos] != SYNC || next_ok {
                match body[pos + 1..l].iter().position(|&b| b == SYNC) {
                    Some(rel) => {
                        let nxt = pos + 1 + rel;
                        sc.res.sync_errors += 1;
                        sc.garbage += (nxt - pos) as u64;
                        pos = nxt;
                        continue;
                    }
                    None => break,
                }
            }
            // 同步恢复：这一段垃圾按 TR 101 290 分类（P1.1 / P1.2）
            if sc.garbage > 0 {
                let th_bytes = (sc.alm.sync_pkts as u64) * ps as u64;
                let kind = if sc.garbage >= th_bytes {
                    "TS_sync_loss"
                } else {
                    "Sync_byte_error"
                };
                let t = sc.time((consumed + pos as u64) as f64);
                sc.tr_report(
                    kind,
                    t,
                    &format!("连续 {} 字节同步丢失", sc.garbage),
                );
                sc.garbage = 0;
            }
            sc.feed(&body[pos..pos + ps], (consumed + pos as u64) as f64, None);
            pos += ps;
        }
        consumed += l as u64;
        done += n as u64;
        if stop.is_some_and(|s| s()) {
            break;
        }
        if let Some(p) = progress.as_mut() {
            if done >= next_report {
                p(done.min(total), total);
                next_report += 8 * 1024 * 1024;
            }
        }
    }
    if sc.garbage > 0 {
        let th_bytes = (tr290_defaults().sync_pkts) * ps as u32;
        let kind = if sc.garbage >= th_bytes as u64 {
            "TS_sync_loss"
        } else {
            "Sync_byte_error"
        };
        sc.tr_report(
            kind,
            None,
            &format!("连续 {} 字节同步丢失（文件尾）", sc.garbage),
        );
        sc.garbage = 0;
    }
    sc.finalize();
    sc.into_result()
}

/// 按检测出的包长迭代 TS 包（跳过垃圾前缀/伪同步字节）。
/// 找不到同步返回 Err。
pub fn for_each_packet(
    reader: &mut dyn Read,
    mut f: impl FnMut(&[u8]),
) -> anyhow::Result<()> {
    let mut head = vec![0u8; PKT_RS * 24];
    let n = reader.read(&mut head)?;
    head.truncate(n);
    let (anchor, ps) = find_sync(&head);
    if ps == 0 {
        anyhow::bail!("不是 188/204 包长的 TS 流");
    }
    // 重新从 anchor 读：head 里 anchor 之后的部分先回放
    let mut tail: Vec<u8> = head[anchor..].to_vec();
    let mut buf = vec![0u8; _CHUNK];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&buf[..n]);
        let whole = tail.len() / ps * ps;
        let body: Vec<u8> = tail.drain(..whole).collect();
        let l = body.len();
        let mut pos = 0usize;
        while pos + ps <= l {
            let next_ok = pos + 2 * ps <= l && body[pos + ps] != SYNC;
            if body[pos] != SYNC || next_ok {
                match body[pos + 1..l].iter().position(|&b| b == SYNC) {
                    Some(rel) => {
                        pos = pos + 1 + rel;
                        continue;
                    }
                    None => break,
                }
            }
            f(&body[pos..pos + ps]);
            pos += ps;
        }
        if tail.len() > PKT_RS * 1024 {
            // 保护：垫尾不至于撑爆
            let keep = tail.split_off(tail.len() - ps);
            tail = keep;
        }
    }
    Ok(())
}

/// 只保留指定 PID 的包（抽取流）。返回写出的包数。
pub fn extract_pids(
    src: &std::path::Path,
    dst: &std::path::Path,
    keep_pids: &[u16],
) -> anyhow::Result<u64> {
    use std::io::Write;
    let keep: std::collections::HashSet<u16> = keep_pids.iter().copied().collect();
    if keep.is_empty() {
        anyhow::bail!("没有要保留的 PID");
    }
    let mut f = std::fs::File::open(src)?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(dst)?);
    let mut n: u64 = 0;
    for_each_packet(&mut f, |pkt| {
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        if keep.contains(&pid) {
            if out.write_all(pkt).is_ok() {
                n += 1;
            }
        }
    })?;
    out.flush()?;
    Ok(n)
}

/// 把 orig 的包改写成 dest（**只改包头 PID 字段**，见模块头「限制」）。
/// PSI 表内引用与 CRC 不动。返回改写的包数。
pub fn remap_pid(
    src: &std::path::Path,
    dst: &std::path::Path,
    orig: u16,
    dest: u16,
) -> anyhow::Result<u64> {
    use std::io::Write;
    if orig > 0x1FFF {
        anyhow::bail!("PID 超出 0~8191：{orig}");
    }
    if dest > 0x1FFF {
        anyhow::bail!("PID 超出 0~8191：{dest}");
    }
    if orig == dest {
        anyhow::bail!("源 PID 与目的 PID 相同");
    }
    let mut f = std::fs::File::open(src)?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(dst)?);
    let mut n: u64 = 0;
    for_each_packet(&mut f, |pkt| {
        let mut pkt = pkt.to_vec();
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        if pid == orig {
            pkt[1] = (pkt[1] & 0xE0) | ((dest >> 8) as u8 & 0x1F);
            pkt[2] = dest as u8;
            n += 1;
        }
        let _ = out.write_all(&pkt);
    })?;
    out.flush()?;
    Ok(n)
}

/// 204 → 188（剥掉每包 16 字节 RS）。返回写出的包数。
pub fn convert_to_188(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> anyhow::Result<u64> {
    use std::io::Write;
    let mut probe = std::fs::File::open(src)?;
    let mut head = vec![0u8; PKT_RS * 24];
    let n = probe.read(&mut head)?;
    head.truncate(n);
    match detect_packet_size(&head) {
        PKT_PLAIN => anyhow::bail!("源文件已经是 188 包长"),
        PKT_RS => {}
        _ => anyhow::bail!("不是 204 包长的 TS 流"),
    }
    let mut f = std::fs::File::open(src)?;
    // rewind 后从文件头重新迭代（for_each_packet 自己找锚点）
    f.seek(SeekFrom::Start(0))?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(dst)?);
    let mut cnt: u64 = 0;
    for_each_packet(&mut f, |pkt| {
        if out.write_all(&pkt[..PKT_PLAIN]).is_ok() {
            cnt += 1;
        }
    })?;
    out.flush()?;
    Ok(cnt)
}

// ---------------------------------------------------------------- GOP / 包视图

/// 用 ffprobe 取视频帧序 → GOP 列表。只看前 max_seconds 秒。
/// 返回 (gops, 截断标记)；ffprobe 失败返回 ([], false)。
pub fn gop_list(
    ffprobe: &std::path::Path,
    path: &str,
    max_seconds: f64,
) -> (Vec<Gop>, bool) {
    let mut cmd = crate::ffmpeg::hidden(ffprobe);
    cmd.args([
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "frame=pict_type,pts_time",
        "-of", "json",
    ]);
    if max_seconds > 0.0 {
        cmd.arg("-read_intervals")
            .arg(format!("%+{}", max_seconds as i64));
    }
    cmd.arg(path);
    let Some(out) = crate::probe::run_limited(cmd, std::time::Duration::from_secs(120))
    else {
        return (vec![], false);
    };
    if !out.status.success() {
        return (vec![], false);
    }
    let d: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return (vec![], false),
    };
    let frames = d
        .get("frames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let num = |v: Option<&serde_json::Value>| -> f64 {
        match v {
            Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0.0),
            _ => 0.0,
        }
    };
    let mut gops = Vec::new();
    let mut start: Option<f64> = None;
    let mut n: u64 = 0;
    for fr in &frames {
        let t = num(fr.get("pts_time"));
        if fr.get("pict_type").and_then(|v| v.as_str()) == Some("I") {
            if let Some(s) = start {
                gops.push(Gop {
                    start: (s * 1000.0).round() / 1000.0,
                    dur: ((t - s) * 1000.0).round() / 1000.0,
                    frames: n,
                });
            }
            start = Some(t);
            n = 1;
        } else if start.is_some() {
            n += 1;
        }
    }
    if let Some(s) = start {
        let last_t = frames
            .last()
            .map(|f| num(f.get("pts_time")))
            .filter(|t| *t != 0.0)
            .unwrap_or(s);
        gops.push(Gop {
            start: (s * 1000.0).round() / 1000.0,
            dur: ((last_t - s) * 1000.0).round() / 1000.0,
            frames: n,
        });
    }
    (gops, max_seconds > 0.0)
}

/// TS 包的随机读取器（包视图页用）—— 不吃内存，按索引 seek。
pub struct PacketFile {
    pub path: String,
    pub anchor: usize,
    pub ps: usize,
    pub size: u64,
    pub count: u64,
}

impl PacketFile {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut head = vec![0u8; PKT_RS * 24];
        let n = f.read(&mut head)?;
        head.truncate(n);
        let (anchor, ps) = find_sync(&head);
        if ps == 0 {
            anyhow::bail!("不是 188/204 包长的 TS 流");
        }
        let size = std::fs::metadata(path)?.len();
        let count = size.saturating_sub(anchor as u64) / ps as u64;
        Ok(Self {
            path: path.to_string_lossy().into(),
            anchor,
            ps,
            size,
            count,
        })
    }

    pub fn read_at(&self, index: u64) -> Vec<u8> {
        if index >= self.count {
            return vec![];
        }
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return vec![];
        };
        if f.seek(SeekFrom::Start(self.anchor as u64 + index * self.ps as u64))
            .is_err()
        {
            return vec![];
        }
        let mut buf = vec![0u8; self.ps];
        match f.read_exact(&mut buf) {
            Ok(_) => buf,
            Err(_) => vec![],
        }
    }

    /// 从 start 连续取 n 个包 → [(index, bytes)]。
    pub fn iter_from(&self, start: u64, n: u64) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::new();
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return out;
        };
        if f.seek(SeekFrom::Start(self.anchor as u64 + start * self.ps as u64))
            .is_err()
        {
            return out;
        }
        let end = (start + n).min(self.count);
        for i in start..end {
            let mut pkt = vec![0u8; self.ps];
            match f.read_exact(&mut pkt) {
                Ok(_) => {}
                Err(_) => break,
            }
            if pkt[0] != SYNC {
                continue;
            }
            out.push((i, pkt));
        }
        out
    }
}

/// 一个 TS 包的包头解读（包视图表格 + hex 页用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PacketInfo {
    pub pid: u16,
    pub pusi: bool,
    pub tsc: u8,
    pub afc: u8,
    pub cc: u8,
    pub pcr: Option<f64>,
    pub payload_len: usize,
    pub stream_id: String,
}

pub fn packet_info(pkt: &[u8]) -> PacketInfo {
    let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
    let pusi = pkt[1] & 0x40 != 0;
    let tsc = pkt[3] >> 6;
    let afc = (pkt[3] >> 4) & 3;
    let cc = pkt[3] & 0x0F;
    let mut payload_at = 4usize;
    let mut pcr = None;
    if afc & 2 != 0 && pkt.len() > 4 {
        let afl = pkt[4] as usize;
        payload_at = 5 + afl;
        if afl >= 7 && pkt.len() > 11 && pkt[5] & 0x10 != 0 {
            let base = ((pkt[6] as u64) << 25)
                | ((pkt[7] as u64) << 17)
                | ((pkt[8] as u64) << 9)
                | ((pkt[9] as u64) << 1)
                | ((pkt[10] as u64) >> 7);
            let ext = (((pkt[10] & 1) as u64) << 8) | pkt[11] as u64;
            pcr = Some(base as f64 / 90_000.0 + ext as f64 / 27_000_000.0);
        }
    }
    let payload: &[u8] = if afc & 1 != 0 && payload_at < pkt.len() {
        &pkt[payload_at..]
    } else {
        &[]
    };
    let mut sid = String::new();
    if pusi && payload.len() >= 4 && payload[0..3] == [0x00, 0x00, 0x01] {
        sid = format!("0x{:02X}", payload[3]);
    }
    PacketInfo {
        pid,
        pusi,
        tsc,
        afc,
        cc,
        pcr,
        payload_len: payload.len(),
        stream_id: sid,
    }
}
