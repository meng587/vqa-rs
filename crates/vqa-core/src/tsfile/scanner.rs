//! 流式扫描状态机 —— `tsfile.py` `_Scanner` 的逐语义移植。
//!
//! `feed(pkt, off, arrival)` 逐包吃；文件扫描与实时收流共用同一个状态机。

use std::collections::{HashMap, HashSet};

use serde_json::json;

use super::pcr::compute_pcr_jitter;
use super::sections::{bcd_dur, desc_loop, dvb_str, fmt_ts, mjd_time, pes_ts, SecAsm};
use super::tr290::{tr290_level, Tr290Thresholds};
use super::{
    crc32_mpeg, is_si_pid, PidStat, Program, RatePoint, SiValue, StreamInfo,
    Tr290Item, _MAX_PCR_RAW, _MAX_PES_SAMPLES, _MAX_RATE_POINTS, _MAX_SI_ITEMS,
};

const NULL_PID: u16 = 0x1FFF;

/// SI 表名 → (三级错误名, 阈值键)。
fn si_map(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "NIT" => Some(("NIT_interval_error", "nit")),
        "SDT_ACT" => Some(("SDT_act_interval_error", "sdt_act")),
        "SDT_OTH" => Some(("SDT_oth_interval_error", "sdt_oth")),
        "EIT_PF" => Some(("EIT_act_interval_error", "eit_act")),
        "EIT_OTH" => Some(("EIT_oth_interval_error", "eit_oth")),
        "SI_REP" => Some(("SI_repetition_error", "sirep")),
        "TDT" => Some(("TDT_error", "tdt")),
        _ => None,
    }
}

/// SI 表 section table_id → 三级参数名（BAT 无专属参数，归 SI 重复率）。
fn tr290_tbl(tid: u8) -> Option<&'static str> {
    match tid {
        0x40 => Some("NIT"),
        0x41 => Some("NIT_OTH"),
        0x42 => Some("SDT_ACT"),
        0x46 => Some("SDT_OTH"),
        0x4A => Some("SI_REP"),
        0x4E => Some("EIT_PF"),
        0x4F => Some("EIT_OTH"),
        0x70 | 0x73 => Some("TDT"),
        _ => None,
    }
}

/// 无 CRC 的表（TDT/RST），CRC_error 跳过。
fn no_crc_tid(tid: u8) -> bool {
    matches!(tid, 0x70 | 0x72)
}

/// 流式扫描状态机。
pub struct Scanner {
    pub res: super::ScanResult,
    /// 报警阈值（覆盖后的）
    pub alm: Tr290Thresholds,
    pub garbage: u64, // 连续垃圾字节数（同步丢失判据）

    pcr_pid: Option<u16>,               // 码率锁定的 PCR PID
    valid_pids: Option<HashSet<u16>>,   // None = 裸流全算；set = 节目内有效
    prev_pcr: Option<f64>,
    acc: u64, // 当前区间累计的有效字节
    seen_pmt: HashSet<(u16, u8)>,
    asm: HashMap<u16, SecAsm>,
    last_pcr: Option<f64>, // 最近一次 PCR（配 PES 采样用）
    si_seen: HashSet<(String, String)>,

    // ---- TR 101 290 状态（§31.56.13）----
    cc: HashMap<u16, u8>,      // pid -> 上个 continuity_counter
    allowed: HashSet<u16>,     // PMT 声明过的 PID（PID_error 白名单）
    pid_bad: HashMap<u16, f64>, // 未声明 pid -> 首见时刻
    clk: [Option<f64>; 4],     // (o1,p1,o2,p2) PCR 插值基准
    pat_t: Option<f64>,        // 上次见到 PAT 的时钟时刻
    pmt_t: HashMap<u16, f64>,  // pmt_pid -> 上次见到的时刻
    pcr_t: Option<f64>,        // 上次 PCR 的时刻
    pcr_arr: Option<f64>,      // 上次 PCR 的网卡到达时间（实时停发检测）
    saw_cat: bool,             // CAT_error 用
    tbl_seen: HashSet<&'static str>, // 见到过的 SI 表（三级间隔检查）
    tbl_t: HashMap<&'static str, f64>, // SI 表名 -> 上次时刻
    last_pts: HashMap<u16, f64>, // 视频 pid -> 上个 PTS 秒
    scrambled_pkts: u64,       // TS包加扰错（加扰控制段非 0 的包数）
}

impl Scanner {
    pub fn new(res: super::ScanResult, overrides: &super::TrOverrides) -> Self {
        Self {
            res,
            alm: Tr290Thresholds::with_overrides(overrides),
            garbage: 0,
            pcr_pid: None,
            valid_pids: None,
            prev_pcr: None,
            acc: 0,
            seen_pmt: HashSet::new(),
            asm: HashMap::new(),
            last_pcr: None,
            si_seen: HashSet::new(),
            cc: HashMap::new(),
            allowed: HashSet::new(),
            pid_bad: HashMap::new(),
            clk: [None; 4],
            pat_t: None,
            pmt_t: HashMap::new(),
            pcr_t: None,
            pcr_arr: None,
            saw_cat: false,
            tbl_seen: HashSet::new(),
            tbl_t: HashMap::new(),
            last_pts: HashMap::new(),
            scrambled_pkts: 0,
        }
    }

    pub fn into_result(self) -> super::ScanResult {
        self.res
    }

    // ---- TR 101 290 工具 ----

    /// 上报一条 TR290 结果；同名条目合并计数。
    pub fn tr_report(&mut self, name: &str, t: Option<f64>, detail: &str) {
        if let Some(item) = self.res.tr290.iter_mut().find(|i| i.name == name) {
            item.count += 1;
            return;
        }
        self.res.tr290.push(Tr290Item {
            level: tr290_level(name),
            name: name.into(),
            count: 1,
            first: fmt_ts(t),
            detail: detail.into(),
        });
    }

    fn clk_set(&mut self, off: f64, pcr: f64) {
        if self.clk[1].is_none() {
            self.clk[0] = Some(off);
            self.clk[1] = Some(pcr);
        } else if self.clk[3].is_none() {
            self.clk[2] = Some(off);
            self.clk[3] = Some(pcr);
        } else {
            self.clk[0] = self.clk[2];
            self.clk[1] = self.clk[3];
            self.clk[2] = Some(off);
            self.clk[3] = Some(pcr);
        }
    }

    /// 把字节偏移映射到 PCR 时钟秒（两点插值 + 末段外推）。无基准 → None。
    pub fn time(&self, off: f64) -> Option<f64> {
        let (o1, p1) = (self.clk[0]?, self.clk[1]?);
        let (o2, p2) = (self.clk[2]?, self.clk[3]?);
        if o2 <= o1 {
            return None;
        }
        let rate = (p2 - p1) / (o2 - o1);
        if rate <= 0.0 {
            return None;
        }
        Some(p2 + (off - o2) * rate)
    }

    fn tr290_timers(
        &mut self,
        pid: u16,
        pusi: bool,
        afc: u8,
        off: f64,
        arrival: Option<f64>,
    ) {
        let _ = pusi;
        // PCR 停发检测：仅实时流用网卡独立到达时间（VBR 可靠）；
        // 文件场景的 PCR 间隔错误在 on_pcr 基于 PCR 时间戳检测。
        if self.pcr_pid.is_some() {
            if let Some(arr) = arrival {
                match self.pcr_arr {
                    None => self.pcr_arr = Some(arr),
                    Some(prev) => {
                        if arr - prev > self.alm.pcr + 0.005 {
                            let p = self.pcr_pid.unwrap();
                            let th_pcr = self.alm.pcr;
                            self.tr_report(
                                "PCR_error",
                                Some(prev),
                                &format!(
                                    "PCR PID 0x{p:04X} 超过 {:.0} ms 没有 PCR",
                                    th_pcr * 1000.0
                                ),
                            );
                            self.pcr_arr = Some(arr);
                        }
                    }
                }
            }
        }
        let Some(t) = self.time(off) else { return };
        // PID_error：未声明 PID 携带载荷持续出现
        let reserved = matches!(pid, 0x00 | 0x01 | 0x10 | 0x11 | 0x12 | 0x14 | 0x1FFF);
        if self.res.has_pmt
            && afc & 1 != 0
            && !self.allowed.contains(&pid)
            && !reserved
        {
            let first = *self.pid_bad.entry(pid).or_insert(t);
            if t - first > self.alm.pid {
                self.tr_report(
                    "PID_error",
                    Some(first),
                    &format!("0x{pid:04X} 携带载荷但不在任何 PMT 声明中"),
                );
                self.pid_bad.insert(pid, t);
            }
        }
        if self.res.encrypted() && !self.saw_cat {
            self.tr_report("CAT_error", Some(t), "有加扰包但没见到 CAT");
        }
    }

    fn tr290_packet(&mut self, pkt: &[u8], pid: u16, afc: u8, off: f64) {
        // Transport_error（二级）：TEI 置位
        if pkt[1] & 0x80 != 0 {
            let t = self.time(off);
            self.tr_report(
                "Transport_error",
                t,
                &format!("0x{pid:04X} 的 TEI 置位（上游已标错）"),
            );
        }
        // Continuity_count_error（一级）：只对带载荷的包查，DI 置位时重同步
        if afc & 1 != 0 {
            let cc = pkt[3] & 0x0F;
            let di = afc & 2 != 0
                && pkt.len() > 5
                && pkt[4] > 0
                && pkt[5] & 0x80 != 0;
            let last = self.cc.get(&pid).copied();
            if let Some(last) = last {
                if !di && cc != (last + 1) & 0x0F {
                    let t = self.time(off);
                    self.tr_report(
                        "Continuity_count_error",
                        t,
                        &format!("0x{pid:04X} 的 CC 从 {last} 跳到 {cc}"),
                    );
                }
            }
            self.cc.insert(pid, cc);
        }
    }

    /// 扫描收尾的一次性判据：SI 表从未出现（ID 错）+ 加扰包统计 + PCR 抖动。
    pub fn finalize(&mut self) {
        // PCR 抖动 OJ/AC（文件一次性计算；实时流由 UI 用 pcr_raw 现算）
        if self.res.pcr_raw.len() >= 2 {
            let raw = self.res.pcr_raw.clone();
            let (samples, cbr) = compute_pcr_jitter(&raw, 8, false);
            self.res.pcr_samples = samples;
            self.res.pcr_cbr = cbr;
        }
        // PCR 精度错误（二级 P2.5）：慢环 AC 相位偏差超 ±500ns（仅 CBR 可靠）
        if !self.res.pcr_samples.is_empty() && self.res.pcr_cbr {
            let lim = self.alm.pcr_accuracy;
            let bad = self
                .res
                .pcr_samples
                .iter()
                .filter(|s| s.ac_ms.abs() > lim * 1000.0)
                .count();
            if bad > 0 {
                let peak = self
                    .res
                    .pcr_samples
                    .iter()
                    .map(|s| s.ac_ms.abs())
                    .fold(0.0f64, f64::max)
                    * 1e6;
                self.tr_report(
                    "PCR_accuracy_error",
                    None,
                    &format!(
                        "{bad} 个 PCR 精度偏差超 ±{:.0} ns（峰值 {peak:.0} ns）",
                        lim * 1e9
                    ),
                );
            }
        }
        if self.scrambled_pkts > 0 {
            let n = self.scrambled_pkts;
            self.tr_report(
                "TS_pack_scrambled",
                None,
                &format!("{n} 个包的加扰控制段非 0"),
            );
        }
        let dur = self.res.duration_pcr();
        if dur <= 0.0 {
            return;
        }
        if !self.tbl_seen.contains("NIT") && dur > self.alm.nit {
            self.tr_report(
                "NIT_id_error",
                None,
                &format!("{dur:.0} 秒的流里从未出现 NIT"),
            );
        }
        if !self.tbl_seen.contains("SDT_ACT")
            && !self.tbl_seen.contains("SDT_OTH")
            && dur > self.alm.sdt_act
        {
            self.tr_report(
                "SDT_id_error",
                None,
                &format!("{dur:.0} 秒的流里从未出现 SDT"),
            );
        }
        if !self.tbl_seen.contains("EIT_PF")
            && !self.tbl_seen.contains("EIT_OTH")
            && dur > self.alm.eit_act
        {
            self.tr_report(
                "EIT_id_error",
                None,
                &format!("{dur:.0} 秒的流里从未出现 EIT"),
            );
        }
    }

    // ---- 主入口 ----

    /// 吃一个 188/204 字节的 TS 包。`off`=字节偏移（文件），`arrival`=网卡
    /// 独立到达时间秒（实时流，可选）。
    pub fn feed(&mut self, pkt: &[u8], off: f64, arrival: Option<f64>) {
        if pkt.len() < 4 {
            return;
        }
        let ps = pkt.len();
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        let pusi = pkt[1] >> 6 & 1 == 1;
        let tsc = pkt[3] >> 6;
        let afc = (pkt[3] >> 4) & 3;

        // TR 101 290 包级检查（TEI / CC）
        self.tr290_packet(pkt, pid, afc, off);
        // PAT/PMT 被加扰也属 PAT/PMT 错误（文档 P1.3/P1.5 判据）
        if tsc != 0 {
            if pid == 0x00 {
                let t = self.time(off);
                self.tr_report("PAT_error", t, "PID 0x0000 的包被加扰");
            } else if self.res.programs.iter().any(|p| p.pmt_pid == pid) {
                let t = self.time(off);
                self.tr_report(
                    "PMT_scrambled_error",
                    t,
                    &format!("PMT 0x{pid:04X} 的包被加扰"),
                );
            }
        }

        let kind = if pid == 0x00 || pid == 0x01 {
            "psi"
        } else if is_si_pid(pid) {
            "si"
        } else {
            "unknown"
        };
        let stat = self
            .res
            .pids
            .entry(pid)
            .or_insert_with(|| PidStat::new(pid, kind));
        stat.add(ps, 0, tsc != 0);
        if tsc != 0 {
            self.scrambled_pkts += 1;
            self.res.has_scrambled = true;
        }
        self.res.packets += 1;

        // 自适应字段：长度 + PCR
        let mut payload_at = 4usize;
        let mut pcr: Option<f64> = None;
        if afc & 2 != 0 && pkt.len() > 4 {
            let afl = pkt[4] as usize;
            payload_at = 5 + afl;
            if afl >= 7 && pkt.len() >= 12 && pkt[5] & 0x10 != 0 {
                let base = ((pkt[6] as u64) << 25)
                    | ((pkt[7] as u64) << 17)
                    | ((pkt[8] as u64) << 9)
                    | ((pkt[9] as u64) << 1)
                    | ((pkt[10] as u64) >> 7);
                let ext = (((pkt[10] & 1) as u64) << 8) | pkt[11] as u64;
                pcr = Some(base as f64 / 90_000.0 + ext as f64 / 27_000_000.0);
            }
        }
        let payload: &[u8] = if afc & 1 != 0 && payload_at < ps {
            &pkt[payload_at..]
        } else {
            &[]
        };

        if let Some(pcr) = pcr {
            if self.res.first_pcr.is_none() {
                self.res.first_pcr = Some(pcr);
            }
            self.res.last_pcr = Some(pcr);
            self.on_pcr(pid, pcr, ps, off, arrival);
        } else if self.pcr_pid.is_some()
            && (self.valid_pids.is_none()
                || self
                    .valid_pids
                    .as_ref()
                    .is_some_and(|v| v.contains(&pid)))
        {
            // 非 PCR 包：落在码率区间里就累计（PCR 包自己在 on_pcr 里计）
            self.acc += ps as u64;
        }

        // PSI/SI：跨包 section 重组后按 table_id 分发
        let is_pmt = self.res.programs.iter().any(|p| p.pmt_pid == pid);
        if pid == 0x00 || pid == 0x01 || is_si_pid(pid) || is_pmt {
            if pusi && !self.asm.contains_key(&pid) {
                self.asm.insert(pid, SecAsm::new());
            }
            let feedable = self
                .asm
                .get(&pid)
                .map(|a| pusi || a.pending())
                .unwrap_or(false);
            if feedable {
                let secs = self.asm.get_mut(&pid).unwrap().feed(payload, pusi);
                for sec in secs {
                    self.dispatch_sec(pid, &sec, off);
                }
            }
            self.tr290_timers(pid, pusi, afc, off, arrival);
            return;
        }

        self.tr290_timers(pid, pusi, afc, off, arrival);

        // PES 头：给视频 PID 采 (DTS, PTS, 当时的 PCR) —— 时间戳/抖动图的数据
        if pusi && payload.len() >= 9 && payload[0..3] == [0x00, 0x00, 0x01] {
            let sid = payload[3];
            if (0xE0..=0xEF).contains(&sid) {
                if let Some(st) = self.res.pids.get_mut(&pid) {
                    if st.kind == "unknown" {
                        st.kind = "video".into();
                    }
                }
                self.pes_sample(pid, payload);
            } else if (0xC0..=0xDF).contains(&sid) {
                if let Some(st) = self.res.pids.get_mut(&pid) {
                    if st.kind == "unknown" {
                        st.kind = "audio".into();
                    }
                }
            }
        }
        // 统计有效负载字节（包头/自适应字段之外）
        if let Some(st) = self.res.pids.get_mut(&pid) {
            st.payload_bytes += payload.len() as u64;
        }
    }

    // ---- 码率区间 ----

    fn on_pcr(&mut self, pid: u16, pcr: f64, ps: usize, off: f64, arrival: Option<f64>) {
        // PCR 不连续（P1.9）：字节插值偏离 >100ms，且 PCR 时间戳本身也异常跳变。
        // 双重条件排除 VBR 文件字节波动导致的插值误报（VBR 不影响 PCR 时间戳）。
        let t = self.time(off);
        if let (Some(prev), Some(tv)) = (self.prev_pcr, t) {
            if (pcr - tv).abs() > 0.1 {
                let dp = pcr - prev;
                if dp > 0.1 || dp < -0.1 {
                    self.tr_report(
                        "PCR_discontinuity_error",
                        Some(tv),
                        &format!("PCR={pcr:.3}s 跳变 {:.0} ms", dp * 1000.0),
                    );
                }
            }
        }
        self.clk_set(off, pcr);
        if self.pcr_pid == Some(pid) {
            self.pcr_t = Some(pcr);
        }
        if self.pcr_pid.is_none() {
            // 锁定时机：PMT 已到 → 用节目声明的 PCR PID；否则裸流兜底
            // （第一个出现 PCR 的 PID），两 PCR 之间所有数据视为有效。
            let prog = self
                .res
                .programs
                .iter()
                .find(|p| p.pcr_pid == pid)
                .cloned();
            if let Some(prog) = prog {
                self.pcr_pid = Some(prog.pcr_pid);
                let mut v: HashSet<u16> =
                    [0x00, prog.pmt_pid, prog.pcr_pid].into_iter().collect();
                v.extend(prog.streams.iter().map(|s| s.pid));
                self.valid_pids = Some(v);
                self.res.pcr_pid = Some(prog.pcr_pid);
            } else if !self.res.has_pmt {
                self.pcr_pid = Some(pid);
                self.valid_pids = None;
                self.res.pcr_pid = Some(pid);
            } else {
                return;
            }
        }
        if self.pcr_pid != Some(pid) {
            return;
        }
        // PCR 包本身按有效计并减 11 字节（EasyICE 口径）
        self.acc += ps.saturating_sub(11) as u64;
        // 参考轴：实时流用网卡独立到达时间(arrival)，文件用字节偏移(off)
        let x_ref = arrival.unwrap_or(off);
        self.res.pcr_raw.push((x_ref, pcr));
        if self.res.pcr_raw.len() > _MAX_PCR_RAW {
            self.res.pcr_raw.remove(0);
        }
        if let Some(prev) = self.prev_pcr {
            let dp = pcr - prev;
            // PCR 重复错误：相邻 PCR 时间戳间隔超阈值（基于 PCR 值，VBR 可靠）
            if dp > self.alm.pcr + 0.005 {
                self.tr_report(
                    "PCR_error",
                    Some(prev),
                    &format!(
                        "PCR 间隔 {:.0} ms 超过 {:.0} ms",
                        dp * 1000.0,
                        self.alm.pcr * 1000.0
                    ),
                );
            }
            if pcr > prev {
                self.res.rate.push(RatePoint {
                    t0: prev,
                    t1: pcr,
                    payload: self.acc,
                });
                if self.res.rate.len() > _MAX_RATE_POINTS {
                    self.res.rate = self.res.rate.iter().step_by(2).copied().collect();
                }
            }
        }
        self.prev_pcr = Some(pcr);
        if arrival.is_some() {
            self.pcr_arr = arrival;
        }
        self.acc = 0;
        self.last_pcr = Some(pcr);
    }

    // ---- PSI/SI 分发 ----

    fn dispatch_sec(&mut self, pid: u16, sec: &[u8], off: f64) {
        if sec.len() < 3 {
            return;
        }
        if let Some(st) = self.res.pids.get_mut(&pid) {
            if st.kind == "unknown" {
                st.kind = "psi".into();
            }
        }
        let tid = sec[0];
        if let Some(k) = tr290_tbl(tid) {
            self.tbl_seen.insert(k);
            self.si_interval_check(k, off);
        }
        if (0x50..=0x5F).contains(&tid) {
            self.tbl_seen.insert("EIT_PF"); // EIT 实际 schedule 归当前视图
            self.si_interval_check("EIT_PF", off);
        }
        if (0x60..=0x6F).contains(&tid) {
            self.tbl_seen.insert("EIT_OTH");
            self.si_interval_check("EIT_OTH", off);
        }
        if !no_crc_tid(tid) && sec.len() >= 8 {
            let body = &sec[..sec.len() - 4];
            let stored = u32::from_be_bytes([
                sec[sec.len() - 4],
                sec[sec.len() - 3],
                sec[sec.len() - 2],
                sec[sec.len() - 1],
            ]);
            if crc32_mpeg(body) != stored {
                let t = self.time(off);
                self.tr_report(
                    "CRC_error",
                    t,
                    &format!("PID 0x{pid:04X} 的 table_id 0x{tid:02X} 校验错"),
                );
            }
        }
        let is_pmt = self.res.programs.iter().any(|p| p.pmt_pid == pid);
        match tid {
            0x00 => {
                self.pat_interval_check(off);
                self.parse_pat_sec(sec);
            }
            0x02 | 0x42 if is_pmt => {
                self.pmt_interval_check(pid, off);
                self.parse_pmt_sec(pid, sec);
            }
            0x01 => self.parse_cat(sec),
            0x40 | 0x41 => self.parse_nit(tid, sec),
            0x42 | 0x46 => self.parse_sdt(tid, sec),
            0x4A => self.parse_bat(sec),
            0x4E..=0x6F => self.parse_eit(tid, sec),
            0x70 | 0x73 => self.parse_tdt(sec),
            _ => {}
        }
    }

    // ---- PES 采样 ----

    fn pes_sample(&mut self, pid: u16, payload: &[u8]) {
        if payload.len() < 9 {
            return;
        }
        let flags = payload[7];
        let hlen = payload[8] as usize;
        if flags & 0x80 == 0 {
            return; // 没有 PTS
        }
        if payload.len() < 9 + hlen || hlen < 5 {
            return;
        }
        let pts = pes_ts(&payload[9..14]);
        let dts = if flags & 0xC0 == 0xC0 && payload.len() >= 19 {
            pes_ts(&payload[14..19])
        } else {
            pts
        };
        // PTS 错误（二级）：PTS 重复间隔超过 700ms（文档口径 P2.6）
        let pts_s = pts as f64 / 90_000.0;
        let last = self.last_pts.get(&pid).copied();
        if let Some(l) = last {
            if pts_s > l && pts_s - l > self.alm.pts {
                let t = self.time(0.0).or(self.last_pcr);
                self.tr_report(
                    "PTS_error",
                    t,
                    &format!(
                        "0x{pid:04X} 的 PTS 间隔 {:.0} ms 超过 {:.0} ms",
                        (pts_s - l) * 1000.0,
                        self.alm.pts * 1000.0
                    ),
                );
            }
        }
        self.last_pts
            .insert(pid, match last {
                None => pts_s,
                Some(l) => l.max(pts_s),
            });
        let pcr_val = self.last_pcr.unwrap_or(0.0);
        let lst = self.res.pes.entry(pid).or_default();
        lst.push((dts as f64 / 90_000.0, pts_s, pcr_val));
        if lst.len() > _MAX_PES_SAMPLES {
            let decimated: Vec<(f64, f64, f64)> =
                lst.iter().step_by(2).copied().collect();
            *lst = decimated;
        }
    }

    // ---- 表重复间隔（标准口径：表出现时测量）----

    fn pat_interval_check(&mut self, off: f64) {
        let Some(t) = self.time(off) else { return };
        let th = self.alm.pat;
        match self.pat_t {
            None => self.pat_t = Some(t),
            Some(cur) => {
                if t - cur > th {
                    self.tr_report(
                        "PAT_error",
                        Some(cur),
                        &format!("超过 {th:.1} 秒没有 PAT"),
                    );
                }
                self.pat_t = Some(t); // 每次 PAT 出现都刷新基准（重复间隔语义）
            }
        }
    }

    fn pmt_interval_check(&mut self, pid: u16, off: f64) {
        let Some(t) = self.time(off) else { return };
        let th = self.alm.pmt;
        match self.pmt_t.get(&pid).copied() {
            None => {
                self.pmt_t.insert(pid, t);
            }
            Some(cur) => {
                if t - cur > th {
                    let prog_num = self
                        .res
                        .programs
                        .iter()
                        .find(|p| p.pmt_pid == pid)
                        .map(|p| p.number)
                        .unwrap_or(0);
                    self.tr_report(
                        "PMT_error",
                        Some(cur),
                        &format!("节目 {prog_num} 的 PMT 0x{pid:04X} 超过 {th:.1} 秒没出现"),
                    );
                }
                self.pmt_t.insert(pid, t); // 每次 PMT 出现都刷新基准
            }
        }
    }

    fn si_interval_check(&mut self, name: &'static str, off: f64) {
        let Some(t) = self.time(off) else { return };
        let Some((ename, key)) = si_map(name) else { return };
        let th = match key {
            "nit" => self.alm.nit,
            "sdt_act" => self.alm.sdt_act,
            "sdt_oth" => self.alm.sdt_oth,
            "eit_act" => self.alm.eit_act,
            "eit_oth" => self.alm.eit_oth,
            "sirep" => self.alm.sirep,
            "tdt" => self.alm.tdt,
            _ => 10.0,
        };
        match self.tbl_t.get(name).copied() {
            None => {
                self.tbl_t.insert(name, t);
            }
            Some(cur) => {
                if t - cur > th {
                    self.tr_report(
                        ename,
                        Some(cur),
                        &format!("{ename} 超过 {th:.0} 秒没有重复"),
                    );
                }
                self.tbl_t.insert(name, t); // 每次表出现都刷新基准
            }
        }
    }

    // ---- SI 存储与表解析 ----

    /// 带去重与上限的 SI 存储。返回新条目在列表中的下标；
    /// 重复键 / 已达上限 → None（调用方不再改写）。
    fn si_put(&mut self, table: &str, key: Option<String>, item: super::SiItem) -> Option<usize> {
        if let Some(k) = &key {
            if self.si_seen.contains(&(table.to_string(), k.clone())) {
                return None;
            }
        }
        if !matches!(self.res.si.get(table), Some(SiValue::List(_))) {
            self.res.si.insert(table.to_string(), SiValue::List(vec![]));
        }
        let Some(SiValue::List(list)) = self.res.si.get_mut(table) else {
            return None;
        };
        if list.len() >= _MAX_SI_ITEMS {
            self.res.si_truncated = true;
            return None;
        }
        list.push(item);
        let idx = list.len() - 1;
        if let Some(k) = key {
            self.si_seen.insert((table.to_string(), k));
        }
        Some(idx)
    }

    fn parse_cat(&mut self, sec: &[u8]) {
        self.saw_cat = true;
        // CAT 表头只有 6 字节（ver+节号，无 tsid），描述符从 sec[6] 起
        if sec.len() < 10 {
            return;
        }
        for (tag, data) in desc_loop(&sec[6..sec.len() - 4]) {
            if tag == 0x09 && data.len() >= 4 {
                let sys_id = ((data[0] as u16) << 8) | data[1] as u16;
                let ca_pid = (((data[2] & 0x1F) as u16) << 8) | data[3] as u16;
                let item = serde_json::Map::from_iter([
                    ("ca_system_id".to_string(), json!(format!("0x{sys_id:04X}"))),
                    ("pid".to_string(), json!(format!("0x{ca_pid:04X}"))),
                ]);
                self.si_put("CAT", Some(format!("{sys_id}")), item);
            }
        }
    }

    fn parse_nit(&mut self, tid: u8, sec: &[u8]) {
        if sec.len() < 12 {
            return;
        }
        let body = &sec[8..sec.len() - 4];
        if body.len() < 2 {
            return;
        }
        let ndlen = (((body[0] & 0x0F) as usize) << 8) | body[1] as usize;
        let descs = &body[2..(2 + ndlen).min(body.len())];
        let mut name = String::new();
        for (tag, data) in desc_loop(descs) {
            if tag == 0x40 {
                name = dvb_str(data);
            }
        }
        let mut item = serde_json::Map::new();
        item.insert(
            "name".into(),
            json!(if name.is_empty() { "（未给网络名）".to_string() } else { name }),
        );
        item.insert("ts_count".into(), json!(0));
        let idx = self.si_put("NIT", Some(tid.to_string()), item);
        let i = 2 + ndlen;
        if let Some(idx) = idx {
            if i + 2 <= body.len() {
                let tslen = (((body[i] & 0x0F) as usize) << 8) | body[i + 1] as usize;
                if let Some(SiValue::List(l)) = self.res.si.get_mut("NIT") {
                    if let Some(it) = l.get_mut(idx) {
                        it.insert("ts_count".into(), json!(tslen / 6));
                    }
                }
            }
        }
    }

    fn parse_sdt(&mut self, tid: u8, sec: &[u8]) {
        if sec.len() < 12 {
            return;
        }
        let body = &sec[8..sec.len() - 4];
        if body.len() < 3 {
            return;
        }
        let onid = ((body[0] as u16) << 8) | body[1] as u16;
        let mut i = 3usize;
        while i + 5 <= body.len() {
            let sid = ((body[i] as u16) << 8) | body[i + 1] as u16;
            let l = (((body[i + 3] & 0x0F) as usize) << 8) | body[i + 4] as usize;
            let descs = &body[i + 5..(i + 5 + l).min(body.len())];
            let mut name = String::new();
            for (tag, data) in desc_loop(descs) {
                // 0x48 业务描述符：type(1) provider_len(1) provider name_len(1) name
                if tag == 0x48 && data.len() >= 2 {
                    let sl = data[1] as usize; // 提供商名长度
                    if 2 + sl < data.len() {
                        let nl = data[2 + sl] as usize; // 业务名长度
                        let end = (3 + sl + nl).min(data.len());
                        name = dvb_str(&data[3 + sl..end]);
                    }
                }
            }
            let item = serde_json::Map::from_iter([
                ("sid".to_string(), json!(format!("0x{sid:04X}"))),
                ("onid".to_string(), json!(format!("0x{onid:04X}"))),
                (
                    "name".to_string(),
                    json!(if name.is_empty() { "（未命名业务）".to_string() } else { name }),
                ),
                (
                    "table".to_string(),
                    json!(if tid == 0x42 { "SDT" } else { "SDT(other)" }),
                ),
            ]);
            self.si_put("SDT", Some(format!("{sid}")), item);
            i += 5 + l;
        }
    }

    fn parse_bat(&mut self, sec: &[u8]) {
        if sec.len() < 12 {
            return;
        }
        let body = &sec[8..sec.len() - 4];
        if body.len() < 2 {
            return;
        }
        let blen = (((body[0] & 0x0F) as usize) << 8) | body[1] as usize;
        let mut name = String::new();
        for (tag, data) in desc_loop(&body[2..(2 + blen).min(body.len())]) {
            if tag == 0x47 {
                name = dvb_str(data);
            }
        }
        let item = serde_json::Map::from_iter([
            (
                "bouquet".to_string(),
                json!(if name.is_empty() { "（未命名 bouquet）".to_string() } else { name }),
            ),
            ("count".to_string(), json!(1)),
        ]);
        self.si_put("BAT", Some(sec[0].to_string()), item);
    }

    fn parse_eit(&mut self, tid: u8, sec: &[u8]) {
        if sec.len() < 12 {
            return;
        }
        let body = &sec[8..sec.len() - 4];
        if body.len() < 7 {
            return;
        }
        let sid = ((body[0] as u16) << 8) | body[1] as u16;
        let mut i = 6usize; // sec[8:] = tsid(2)+onid(2)+段末节号(1)+末表号(1)，事件从这起
        while i + 12 <= body.len() {
            let event_id = ((body[i] as u16) << 8) | body[i + 1] as u16;
            let start = mjd_time(&body[i + 2..i + 7]);
            let dur = bcd_dur(&body[i + 7..i + 10]);
            let l = (((body[i + 10] & 0x0F) as usize) << 8) | body[i + 11] as usize;
            let descs = &body[i + 12..(i + 12 + l).min(body.len())];
            let mut name = String::new();
            for (tag, data) in desc_loop(descs) {
                if tag == 0x4D && data.len() >= 4 {
                    let nl = data[3] as usize;
                    let end = (4 + nl).min(data.len());
                    name = dvb_str(&data[4..end]);
                }
            }
            let item = serde_json::Map::from_iter([
                ("sid".to_string(), json!(format!("0x{sid:04X}"))),
                ("event_id".to_string(), json!(format!("0x{event_id:04X}"))),
                ("start".to_string(), json!(if start.is_empty() { "—".to_string() } else { start })),
                ("dur".to_string(), json!(if dur.is_empty() { "—".to_string() } else { dur })),
                (
                    "name".to_string(),
                    json!(if name.is_empty() { "（未命名事件）".to_string() } else { name }),
                ),
            ]);
            self.si_put("EIT", Some(format!("{sid}|{event_id}")), item);
            i += 12 + l;
        }
        let _ = tid;
    }

    fn parse_tdt(&mut self, sec: &[u8]) {
        if sec.len() < 8 {
            return;
        }
        let t = mjd_time(&sec[3..8]); // TDT/TOT 的 UTC 时间都在 body 开头
        if !t.is_empty() {
            self.res.si.insert("TDT".into(), SiValue::Text(t));
        }
    }

    // ---- PSI ----

    fn parse_pat_sec(&mut self, sec: &[u8]) {
        if sec[0] != 0x00 {
            return;
        }
        self.res.has_pat = true;
        if sec.len() < 4 {
            return;
        }
        let body = &sec[3..sec.len() - 4]; // 去 CRC
        // 表头 5 字节：transport_stream_id(2)+version(1)+节号(2)
        let mut i = 5usize;
        while i + 4 <= body.len() {
            let prog_num = ((body[i] as u16) << 8) | body[i + 1] as u16;
            let pid = (((body[i + 2] & 0x1F) as u16) << 8) | body[i + 3] as u16;
            if prog_num == 0 || pid == NULL_PID {
                i += 4;
                continue; // 网络 PID / 保留，不管
            }
            if !self.res.programs.iter().any(|p| p.number == prog_num) {
                self.res.programs.push(Program::new(prog_num, pid));
            }
            i += 4;
        }
    }

    fn parse_pmt_sec(&mut self, pid: u16, sec: &[u8]) {
        if sec[0] != 0x02 && sec[0] != 0x42 {
            return;
        }
        if sec.len() < 16 {
            return; // 防御：Python 版此处越界即崩，这里安全返回
        }
        let version = (sec[5] >> 1) & 0x1F;
        if self.seen_pmt.contains(&(pid, version)) {
            return;
        }
        let prog_num = ((sec[3] as u16) << 8) | sec[4] as u16;
        if !self.res.programs.iter().any(|p| p.number == prog_num) {
            self.res.programs.push(Program::new(prog_num, pid));
        }
        let pcr_pid = (((sec[8] & 0x1F) as u16) << 8) | sec[9] as u16;
        let pilen = (((sec[10] & 0x0F) as usize) << 8) | sec[11] as usize;
        let mut i = 12 + pilen;
        let mut streams: Vec<StreamInfo> = Vec::new();
        while i + 5 <= sec.len() - 4 {
            let st = sec[i];
            let epid = (((sec[i + 1] & 0x1F) as u16) << 8) | sec[i + 2] as u16;
            let eslen = (((sec[i + 3] & 0x0F) as usize) << 8) | sec[i + 4] as usize;
            if epid != NULL_PID {
                streams.push(StreamInfo::new(epid, st));
            }
            i += 5 + eslen;
        }
        self.res.has_pmt = true;
        self.seen_pmt.insert((pid, version));
        let mut stream_pids: Vec<u16> = Vec::new();
        if let Some(prog) = self
            .res
            .programs
            .iter_mut()
            .find(|p| p.number == prog_num)
        {
            prog.pcr_pid = pcr_pid;
            prog.streams = streams;
            stream_pids = prog.streams.iter().map(|s| s.pid).collect();
            self.allowed.insert(prog.pmt_pid);
        }
        if pcr_pid != NULL_PID {
            self.allowed.insert(pcr_pid);
        }
        for p in stream_pids {
            self.allowed.insert(p);
        }
        // PMT 到手后如果码率还没锁定（或锁在兜底 PID 上），
        // 用节目声明值重锁 —— 裸流兜底期间累计的残段直接作废。
        if pcr_pid != NULL_PID {
            self.pcr_pid = Some(pcr_pid);
            let mut v: HashSet<u16> = [0x00, pid, pcr_pid].into_iter().collect();
            if let Some(prog) = self
                .res
                .programs
                .iter()
                .find(|p| p.number == prog_num)
            {
                v.extend(prog.streams.iter().map(|s| s.pid));
            }
            self.valid_pids = Some(v);
            self.prev_pcr = None;
            self.acc = 0;
            self.res.pcr_pid = Some(pcr_pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clk_time_interpolation() {
        let mut sc = Scanner::new(super::super::ScanResult::new("t"), &Default::default());
        sc.clk_set(100.0, 1.0);
        assert!(sc.time(0.0).is_none()); // 只有一个基准点
        sc.clk_set(200.0, 2.0);
        let t = sc.time(300.0).unwrap();
        assert!((t - 3.0).abs() < 1e-9);
    }
}
