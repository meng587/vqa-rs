//! 事件类检测的数据源：按 PID 扫 TS 包，统计连续性计数错误、无有效净荷、PCR 跨度。
//!
//! 对应 253 号文里软件能判的那部分：
//! - 表 2 序号 19 / 21：视频、音频 PID 的数据包丢失（CC 跳变）或无有效净荷
//! - 表 1 序号 15：同上（画面类异常仍需逐帧解码，本模块不管）
//!
//! 只读包头，不解码、不解 PAYLOAD，速度只受磁盘带宽限制。

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// 一个 PID 的事件统计。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PidEv {
    pub pid: u16,
    /// video / audio / other
    pub kind: String,
    pub stream_type: u8,
    pub packets: u64,
    /// 连续性计数错误次数（≈ 丢包次数）
    pub cc_err: u64,
    /// 有包头但无有效净荷的包（adaptation only / 空包）
    pub no_payload: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EvReport {
    /// 文件时长（PCR 跨度，秒）；无 PCR 时为 0
    pub duration_s: f64,
    pub total_packets: u64,
    pub pmt_ok: bool,
    pub pcr_ok: bool,
    pub video: Vec<PidEv>,
    pub audio: Vec<PidEv>,
    pub other: Vec<PidEv>,
}

impl EvReport {
    pub fn video_errors(&self) -> (u64, u64, u64) {
        sum(&self.video)
    }
    pub fn audio_errors(&self) -> (u64, u64, u64) {
        sum(&self.audio)
    }
}

fn sum(v: &[PidEv]) -> (u64, u64, u64) {
    v.iter().fold((0, 0, 0), |(e, n, p), x| {
        (e + x.cc_err, n + x.no_payload, p + x.packets)
    })
}

struct Acc {
    packets: u64,
    cc_err: u64,
    no_payload: u64,
    last_cc: Option<u8>,
}

impl Acc {
    fn new() -> Self {
        Self {
            packets: 0,
            cc_err: 0,
            no_payload: 0,
            last_cc: None,
        }
    }
}

/// 扫一遍 TS 文件，返回各 PID 的连续性统计。
pub fn scan(path: &Path) -> std::io::Result<EvReport> {
    let mut rep = EvReport::default();
    let mut f = std::fs::File::open(path)?;

    // 找同步与包长
    let mut head = vec![0u8; 188 * 32];
    let n = f.read(&mut head)?;
    head.truncate(n);
    let (anchor, ps) = crate::tsfile::find_sync(&head);
    if ps == 0 {
        return Ok(rep);
    }
    // 读头之后一律回到锚点（anchor 常为 0，也必须回绕，否则会把已读的头当成数据漏掉）
    {
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(anchor as u64))?;
    }

    let mut acc: HashMap<u16, Acc> = HashMap::new();
    let mut pmt_pid: Option<u16> = None;
    let mut pcr_pid: Option<u16> = None;
    let mut streams: Vec<(u16, u8)> = Vec::new();
    let mut pat_asm = crate::tsfile::sections::SecAsm::new();
    let mut pmt_asm = crate::tsfile::sections::SecAsm::new();
    // PCR 取全体 PCR 包的最小/最大值（多节目也能给出跨度）
    let mut pcr_min: Option<u64> = None;
    let mut pcr_max: Option<u64> = None;

    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut data = std::mem::take(&mut tail);
        data.extend_from_slice(&buf[..n]);
        let whole = data.len() / ps * ps;
        tail = data.split_off(whole);

        let mut pos = 0usize;
        while pos + ps <= data.len() {
            let p = &data[pos..pos + ps];
            pos += ps;
            if p[0] != 0x47 {
                continue;
            }
            rep.total_packets += 1;
            let pid = ((p[1] as u16 & 0x1f) << 8) | p[2] as u16;
            let afc = (p[3] >> 4) & 0x3;
            let has_payload = afc == 1 || afc == 3;
            let has_af = afc == 2 || afc == 3;

            // 适配字段：PCR 与不连续标志
            let mut discontinuity = false;
            let mut payload_off = 4usize;
            if has_af && p.len() > 5 {
                let af_len = p[4] as usize;
                if af_len >= 1 && p.len() > 5 {
                    let flags = p[5];
                    discontinuity = flags & 0x80 != 0;
                    if flags & 0x10 != 0 && p.len() >= 12 {
                        let base = ((p[6] as u64) << 25)
                            | ((p[7] as u64) << 17)
                            | ((p[8] as u64) << 9)
                            | ((p[9] as u64) << 1)
                            | ((p[10] as u64) >> 7);
                        pcr_min = Some(pcr_min.map_or(base, |m| m.min(base)));
                        pcr_max = Some(pcr_max.map_or(base, |m| m.max(base)));
                    }
                }
                payload_off = 5 + af_len;
            }

            // 连续性计数只统计带净荷的包（适配only的包 CC 不递增）
            let e = acc.entry(pid).or_insert_with(Acc::new);
            e.packets += 1;
            if !has_payload {
                e.no_payload += 1;
            }
            if has_payload {
                let cc = p[3] & 0x0f;
                if discontinuity {
                    e.last_cc = Some(cc);
                } else if let Some(last) = e.last_cc {
                    let expect = (last + 1) & 0x0f;
                    if cc == last {
                        // 重复包，不算错
                    } else if cc != expect {
                        e.cc_err += 1;
                    }
                }
                e.last_cc = Some(cc);
            }

            // PSI：PAT / PMT
            if has_payload && (pid == 0 || Some(pid) == pmt_pid) {
                if payload_off < p.len() {
                    let pusi = p[1] & 0x40 != 0;
                    let payload = &p[payload_off..];
                    let secs = if pid == 0 {
                        pat_asm.feed(payload, pusi)
                    } else {
                        pmt_asm.feed(payload, pusi)
                    };
                    for sec in secs {
                        if pid == 0 && sec.first() == Some(&0x00) && sec.len() >= 12 {
                            let end = 3 + ((sec[1] as usize) << 8 | sec[2] as usize) - 4;
                            let mut i = 8;
                            while i + 4 <= sec.len().min(end) {
                                let prog = (sec[i] as u16) << 8 | sec[i + 1] as u16;
                                let pmt = (sec[i + 2] as u16 & 0x1f) << 8 | sec[i + 3] as u16;
                                if prog != 0 {
                                    pmt_pid = Some(pmt);
                                    break;
                                }
                                i += 4;
                            }
                        } else if sec.first() == Some(&0x02) && sec.len() >= 16 {
                            rep.pmt_ok = true;
                            let pi_len = ((sec[10] as usize) << 8 | sec[11] as usize) & 0x0fff;
                            if pcr_pid.is_none() {
                                pcr_pid = Some(((sec[8] as u16 & 0x1f) << 8) | sec[9] as u16);
                            }
                            let end = 3 + ((sec[1] as usize) << 8 | sec[2] as usize) - 4;
                            let mut i = 12 + pi_len;
                            while i + 5 <= sec.len().min(end) {
                                let st = sec[i];
                                let epid = ((sec[i + 1] as u16 & 0x1f) << 8) | sec[i + 2] as u16;
                                let es_len = ((sec[i + 3] as usize) << 8 | sec[i + 4] as usize) & 0x0fff;
                                if !streams.iter().any(|(p2, _)| *p2 == epid) {
                                    streams.push((epid, st));
                                }
                                i += 5 + es_len;
                            }
                        }
                    }
                }
            }
        }
    }

    if let (Some(a), Some(b)) = (pcr_min, pcr_max) {
        rep.pcr_ok = true;
        rep.duration_s = (b.saturating_sub(a)) as f64 / 90_000.0;
    }
    let _ = pcr_pid;

    // 按 PMT 把 PID 分到 视频 / 音频 / 其它
    for (pid, st) in streams {
        if let Some(e) = acc.remove(&pid) {
            let item = PidEv {
                pid,
                kind: crate::tsfile::kind_of_stream_type(st).to_string(),
                stream_type: st,
                packets: e.packets,
                cc_err: e.cc_err,
                no_payload: e.no_payload,
            };
            match item.kind.as_str() {
                "video" => rep.video.push(item),
                "audio" => rep.audio.push(item),
                _ => rep.other.push(item),
            }
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(pid: u16, cc: u8, payload: bool) -> Vec<u8> {
        let mut p = vec![0u8; 188];
        p[0] = 0x47;
        p[1] = ((pid >> 8) as u8 & 0x1f) | 0x40;
        p[2] = pid as u8;
        p[3] = if payload { 0x10 } else { 0x20 }; // afc=1 有净荷 / afc=2 仅适配
        p[3] |= cc & 0x0f;
        p
    }

    /// PUSI 包带一段 PSI section（pointer_field=0，CRC 填 0，解析不校验 CRC）
    fn psi(pid: u16, cc: u8, sec: &[u8]) -> Vec<u8> {
        let mut p = pkt(pid, cc, true);
        for (i, b) in sec.iter().enumerate() {
            p[4 + 1 + i] = *b;
        }
        p
    }

    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut v = vec![0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00];
        v.extend([0x00, 0x01]);
        v.extend([0xE0 | ((pmt_pid >> 8) as u8 & 0x1f), pmt_pid as u8]);
        v.extend([0, 0, 0, 0]);
        v
    }

    fn pmt(video_pid: u16) -> Vec<u8> {
        let mut v = vec![0x02, 0xB0, 0x12, 0x00, 0x01, 0xC1, 0x00, 0x00];
        v.extend([0xE0 | ((video_pid >> 8) as u8 & 0x1f), video_pid as u8]); // PCR_PID
        v.extend([0xF0, 0x00]); // program_info_length = 0
        v.extend([0x1B, 0xE0 | ((video_pid >> 8) as u8 & 0x1f), video_pid as u8, 0xF0, 0x00]);
        v.extend([0, 0, 0, 0]);
        v
    }

    fn write_and_scan(name: &str, body: Vec<u8>) -> EvReport {
        let dir = std::env::temp_dir().join(name);
        std::fs::write(&dir, &body).unwrap();
        let rep = scan(&dir).unwrap();
        let _ = std::fs::remove_file(&dir);
        rep
    }

    #[test]
    fn cc_jump_classified_as_video_loss() {
        let mut data = Vec::new();
        data.extend(psi(0, 0, &pat(0x0100)));
        data.extend(psi(0x0100, 0, &pmt(0x0101)));
        // 视频 PID 0x101：0,1,3（缺 2）→ 1 次 CC 错误
        for (cc, pl) in [(0u8, true), (1, true), (3, true)] {
            data.extend(pkt(0x0101, cc, pl));
        }
        let rep = write_and_scan("vqa_ev_test.ts", data);
        assert!(rep.pmt_ok);
        assert_eq!(rep.total_packets, 5);
        assert_eq!(rep.video.len(), 1);
        assert_eq!(rep.video[0].pid, 0x0101);
        assert_eq!(rep.video[0].cc_err, 1);
    }

    #[test]
    fn no_payload_counted() {
        let mut data = Vec::new();
        data.extend(psi(0, 0, &pat(0x0100)));
        data.extend(psi(0x0100, 0, &pmt(0x0101)));
        data.extend(pkt(0x0101, 0, true));
        data.extend(pkt(0x0101, 0, false)); // 仅适配字段，无净荷
        data.extend(pkt(0x0101, 1, true));
        let rep = write_and_scan("vqa_ev_test2.ts", data);
        assert_eq!(rep.total_packets, 5);
        assert_eq!(rep.video[0].no_payload, 1);
        assert_eq!(rep.video[0].cc_err, 0);
    }
}
