//! 跨包 section 重组 + 描述符/DVB 文本辅助。

/// 跨包 section 重组器（每 PID 一个）。
///
/// TS 的 section 可以横跨多个包：PUSI 包的 payload[0] 是 pointer_field，
/// 之后是新 section 的开始；后续包的 payload 顺序续接。按
/// section_length 攒够即产出（一个 payload 里也可能连着多个 section）。
#[derive(Default)]
pub struct SecAsm {
    buf: Vec<u8>,
}

impl SecAsm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> bool {
        !self.buf.is_empty()
    }

    /// 喂入一个包的 payload；返回攒齐的完整 section（含 4 字节 CRC）。
    pub fn feed(&mut self, payload: &[u8], pusi: bool) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if pusi {
            self.buf.clear();
            if payload.is_empty() {
                return out;
            }
            let ptr = payload[0] as usize;
            if 1 + ptr > payload.len() {
                return out; // pointer_field 越界（脏数据），丢弃
            }
            let data = &payload[1 + ptr..];
            self.buf.extend_from_slice(data);
        } else {
            if self.buf.is_empty() || payload.is_empty() {
                return out;
            }
            self.buf.extend_from_slice(payload);
        }
        while self.buf.len() >= 3 {
            let slen = (((self.buf[1] & 0x0F) as usize) << 8) | self.buf[2] as usize;
            if slen == 0 {
                self.buf.clear();
                break;
            }
            let total = 3 + slen;
            if self.buf.len() < total {
                break;
            }
            out.push(self.buf[..total].to_vec());
            self.buf.drain(..total);
        }
        out
    }
}

/// 扫描时钟秒 → 'HH:MM:SS.mmm'；无基准 → '—'。
pub fn fmt_ts(t: Option<f64>) -> String {
    let Some(t) = t else { return "—".into() };
    let t = t.max(0.0);
    let ms = ((t % 60.0) * 1000.0).round() as u64;
    let s = t as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ms % 1000
    )
}

/// PES 里 5 字节的 PTS/DTS → 90kHz 整数。
pub fn pes_ts(b: &[u8]) -> u64 {
    (((b[0] >> 1) & 0x07) as u64) << 30
        | (b[1] as u64) << 22
        | (((b[2] >> 1) as u64) << 15)
        | (b[3] as u64) << 7
        | (b[4] >> 1) as u64
}

/// 描述符循环 → [(tag, data), …]。喂进去的字节在循环范围内部。
pub fn desc_loop(body: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 2 <= body.len() {
        let tag = body[i];
        let l = body[i + 1] as usize;
        let end = (i + 2 + l).min(body.len());
        out.push((tag, &body[i + 2..end]));
        i += 2 + l;
    }
    out
}

/// DVB 描述符文本。首字节是编码表标识（0x01 latin-1 / 0x05·0x11 utf-8 /
/// 0x15 GB2312 常被用作 utf-8…实际广播里常见乱标）；无前缀时先试 utf-8
/// 严格解码，不行再退 GBK（国内业务名多走这两条）。
pub fn dvb_str(b: &[u8]) -> String {
    if b.is_empty() {
        return String::new();
    }
    let head = b[0];
    let enc: Option<&'static str> = match head {
        0x01 => Some("windows-1252"), // 近似 latin-1
        0x05 | 0x11 | 0x15 => Some("utf-8"),
        0x13 => Some("utf-16be"),
        _ => None,
    };
    let raw = if enc.is_some() { &b[1..] } else { b };
    if let Some(e) = enc {
        if e == "windows-1252" {
            return String::from_utf8_lossy(raw).trim().to_string();
        }
        if e == "utf-16be" {
            let (s, _, _) = encoding_rs::UTF_16BE.decode(raw);
            return s.trim().to_string();
        }
        match std::str::from_utf8(raw) {
            Ok(s) => return s.trim().to_string(),
            Err(_) => return String::new(),
        }
    }
    for e in [
        encoding_rs::UTF_8,
        encoding_rs::GBK,
        encoding_rs::WINDOWS_1252,
    ] {
        let (s, _, had_errors) = e.decode(raw);
        if !had_errors {
            return s.trim().to_string();
        }
    }
    String::new()
}

/// MJD + BCD 时间（5 字节）→ 'YYYY-MM-DD HH:MM:SS'；未定义 → ''。
pub fn mjd_time(b5: &[u8]) -> String {
    if b5.len() < 5 {
        return String::new();
    }
    let mjd = ((b5[0] as u32) << 8) | b5[1] as u32;
    if mjd == 0xFFFF || mjd == 0 {
        return String::new();
    }
    let yp = ((mjd as f64 - 15078.2) / 365.25) as i64;
    let mp = ((mjd as f64 - 14956.1 - (yp as f64 * 365.25).trunc()) / 30.6001) as i64;
    let day = mjd as i64
        - 14956
        - (yp as f64 * 365.25).trunc() as i64
        - (mp as f64 * 30.6001).trunc() as i64;
    let k = i64::from(matches!(mp, 14 | 15));
    let year = yp + k + 1900;
    let month = mp - 1 - k * 12;
    // BCD：按十六进制位读（Python 版 "%02x" 口径）
    format!(
        "{year:04}-{month:02}-{day:02} {h:02x}:{m:02x}:{s:02x}",
        h = b5[2],
        m = b5[3],
        s = b5[4]
    )
}

/// BCD 时长（3 字节）→ 'HH:MM:SS'。
pub fn bcd_dur(b3: &[u8]) -> String {
    if b3.len() < 3 || b3 == [0xFF, 0xFF, 0xFF] {
        return String::new();
    }
    format!("{:02}:{:02}:{:02}", b3[0], b3[1], b3[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_asm_single_and_split() {
        // 构造一个 section：3 字节头 + slen 字节体 = 3+slen 总长，跨两包
        let slen = 10u16;
        let mut sec = vec![0x42u8];
        sec.push(((slen >> 8) & 0x0F) as u8);
        sec.push((slen & 0xFF) as u8);
        sec.extend(std::iter::repeat_n(0xABu8, slen as usize));
        assert_eq!(sec.len(), 3 + slen as usize);
        let mut asm = SecAsm::new();
        // 第一包：pointer=0，前 6 字节
        let mut p1 = vec![0u8];
        p1.extend_from_slice(&sec[..6]);
        let got1 = asm.feed(&p1, true);
        assert!(got1.is_empty());
        assert!(asm.pending());
        // 第二包：续 7 字节
        let p2 = sec[6..].to_vec();
        let got2 = asm.feed(&p2, false);
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0], sec);
    }

    #[test]
    fn sec_asm_two_sections_in_one_payload() {
        let sec = vec![0x00u8, 0x00, 0x05, 1, 2, 3, 4, 5]; // slen=5 → total 8
        let mut asm = SecAsm::new();
        let mut payload = vec![0u8]; // pointer 0
        payload.extend_from_slice(&sec);
        payload.extend_from_slice(&sec);
        let got = asm.feed(&payload, true);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], sec);
    }

    #[test]
    fn fmt_ts_matches_python() {
        assert_eq!(fmt_ts(None), "—");
        assert_eq!(fmt_ts(Some(3661.5)), "01:01:01.500");
        assert_eq!(fmt_ts(Some(0.0)), "00:00:00.000");
    }

    #[test]
    fn mjd_known_date() {
        // MJD 51544 = 2000-01-01（DVB 标准 MJD 公式）
        let b = [0xC9u8, 0x58, 0x12, 0x30, 0x00];
        assert_eq!(mjd_time(&b), "2000-01-01 12:30:00");
        // 0xFFFF/0 = 未定义
        assert_eq!(mjd_time(&[0xFF, 0xFF, 0, 0, 0]), "");
    }
}
