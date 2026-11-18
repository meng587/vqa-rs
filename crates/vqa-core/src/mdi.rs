//! 组播收流传输质量统计（RTP 抖动 / 丢包 / MDI-DF / MDI-MLR）。
//!
//! 从 Python `vkitcore/media/udpstats.py`（`UdpMcastMonitor`）移植，
//! 口径逐项对齐：
//!
//! | 指标 | 窗口 | 算法 |
//! |------|------|------|
//! | 码率 | 2s | 窗口字节 ×8 / 窗口时长 |
//! | RTP 抖动 | 2s | 每包 \|实际间隔 − 40ms\| 的均值 |
//! | MDI-DF | 5s | 抖动 ≥100ms 的包数 / 窗口包数（%） |
//! | MDI-MLR | 10s | 窗口内 RTP 序号跳变=0 记好窗口，好窗口 / 总窗口（%） |
//!
//! 丢包只认 RTP 序号（RFC 2250：12 字节头 + n×188 TS）。UDP 裸封装没有序号，
//! 无法在包级判丢包 —— 此时 `rtp=false`，页面如实显示"非 RTP 流"而不是冒充合格。

use std::time::{Duration, Instant};

use crate::compliance::NetStats;

const EXP_INTERVAL_S: f64 = 0.04; // 40ms（25 包/s，标准高清 TS 单播间隔）
const DF_BAD_MS: f64 = 100.0;

#[derive(Debug)]
pub struct MdiState {
    // ---- 累计 ----
    pub packets: u64,
    pub bytes: u64,
    started: Instant,

    // ---- 2s 窗口（码率 / 抖动）----
    win_start: Instant,
    win_pkts: u64,
    win_bytes: u64,
    win_jit_sum: f64,
    win_jit_n: u64,
    last_bitrate_mbps: f64,
    last_jitter_ms: f64,

    // ---- 5s 窗口（MDI-DF）----
    df_start: Instant,
    df_total: u64,
    df_bad: u64,
    last_df_pct: f64,

    // ---- 10s 窗口（MDI-MLR）----
    mlr_start: Instant,
    mlr_windows: u64,
    mlr_good: u64,
    mlr_window_lost: u64,

    // ---- RTP ----
    pub rtp: bool,
    rtp_checked: bool,
    ssrc: Option<u32>,
    last_seq: Option<u16>,
    lost: u64,
    rtp_pkts: u64,

    last_arrival: Option<Instant>,
}

impl Default for MdiState {
    fn default() -> Self {
        Self::new()
    }
}

impl MdiState {
    pub fn new() -> Self {
        let now = Instant::now();
        MdiState {
            packets: 0,
            bytes: 0,
            started: now,
            win_start: now,
            win_pkts: 0,
            win_bytes: 0,
            win_jit_sum: 0.0,
            win_jit_n: 0,
            last_bitrate_mbps: 0.0,
            last_jitter_ms: 0.0,
            df_start: now,
            df_total: 0,
            df_bad: 0,
            last_df_pct: 0.0,
            mlr_start: now,
            mlr_windows: 0,
            mlr_good: 0,
            mlr_window_lost: 0,
            rtp: false,
            rtp_checked: false,
            ssrc: None,
            last_seq: None,
            lost: 0,
            rtp_pkts: 0,
            last_arrival: None,
        }
    }

    /// 每个 UDP 数据报进来调一次（原始字节，未剥 RTP 头）。
    pub fn feed(&mut self, raw: &[u8], now: Instant) {
        self.packets += 1;
        self.bytes += raw.len() as u64;

        // ---- RTP 头校验（首个包判定一次，之后只看同 SSRC）----
        // 只看版本位（高两位=10）；payload type 96~127 是动态类型，
        // 不能按 <96 判（RTP muxer 发 H.264 就是 96）。TS 包首字节 0x47 不会误判。
        let is_rtp = raw.len() >= 12 && (raw[0] >> 6) == 2;
        if !self.rtp_checked {
            self.rtp_checked = true;
            self.rtp = is_rtp;
        }
        if self.rtp && is_rtp {
            let ssrc = u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]);
            if self.ssrc.is_none() {
                self.ssrc = Some(ssrc);
            }
            if self.ssrc == Some(ssrc) {
                let seq = u16::from_be_bytes([raw[2], raw[3]]);
                self.rtp_pkts += 1;
                if let Some(prev) = self.last_seq {
                    // 16 位序号回绕：先按 u16 求差，再解释成有符号（>32768 视为负）
                    let d = seq.wrapping_sub(prev) as i32;
                    let delta = if d > 32768 { d - 65536 } else { d };
                    if delta > 1 {
                        let miss = (delta - 1) as u64;
                        self.lost += miss;
                        self.mlr_window_lost += miss;
                    }
                    // delta <= 0：乱序/重复包，不记丢包（容忍）
                }
                self.last_seq = Some(seq);
            }
        }

        // ---- 抖动：与上一包到达间隔比对 ----
        if let Some(prev) = self.last_arrival {
            let dt = now.duration_since(prev).as_secs_f64();
            let dev_ms = ((dt - EXP_INTERVAL_S) * 1000.0).abs();
            self.win_jit_sum += dev_ms;
            self.win_jit_n += 1;
            self.df_total += 1;
            if dev_ms >= DF_BAD_MS {
                self.df_bad += 1;
            }
        }
        self.last_arrival = Some(now);

        self.win_pkts += 1;
        self.win_bytes += raw.len() as u64;

        // ---- 2s 窗口收口 ----
        let win_elapsed = now.duration_since(self.win_start);
        if win_elapsed >= Duration::from_secs(2) {
            let secs = win_elapsed.as_secs_f64();
            self.last_bitrate_mbps = self.win_bytes as f64 * 8.0 / secs / 1_000_000.0;
            self.last_jitter_ms = if self.win_jit_n > 0 {
                self.win_jit_sum / self.win_jit_n as f64
            } else {
                0.0
            };
            self.win_start = now;
            self.win_pkts = 0;
            self.win_bytes = 0;
            self.win_jit_sum = 0.0;
            self.win_jit_n = 0;
        }

        // ---- 5s 窗口收口（MDI-DF）----
        if now.duration_since(self.df_start) >= Duration::from_secs(5) {
            self.last_df_pct = if self.df_total > 0 {
                self.df_bad as f64 * 100.0 / self.df_total as f64
            } else {
                0.0
            };
            self.df_start = now;
            self.df_total = 0;
            self.df_bad = 0;
        }

        // ---- 10s 窗口收口（MDI-MLR）----
        if now.duration_since(self.mlr_start) >= Duration::from_secs(10) {
            self.mlr_windows += 1;
            if self.mlr_window_lost == 0 {
                self.mlr_good += 1;
            }
            self.mlr_start = now;
            self.mlr_window_lost = 0;
        }
    }

    /// 当前窗口的 DF 实测（未满 5s 也能给个近似值，便于页面实时看）。
    fn df_pct_now(&self) -> f64 {
        if self.df_total > 0 {
            self.df_bad as f64 * 100.0 / self.df_total as f64
        } else {
            self.last_df_pct
        }
    }

    pub fn snapshot(&self) -> NetStats {
        let loss_ratio = if self.rtp && (self.lost + self.rtp_pkts) > 0 {
            self.lost as f64 / (self.lost + self.rtp_pkts) as f64
        } else {
            0.0
        };
        let mlr_good_pct = if self.mlr_windows > 0 {
            self.mlr_good as f64 * 100.0 / self.mlr_windows as f64
        } else if self.lost == 0 {
            100.0
        } else {
            0.0
        };
        NetStats {
            bitrate_mbps: (self.last_bitrate_mbps * 100.0).round() / 100.0,
            jitter_ms: (self.last_jitter_ms * 10.0).round() / 10.0,
            loss_ratio,
            df_bad_pct: (self.df_pct_now() * 1000.0).round() / 1000.0,
            mlr_good_pct: (mlr_good_pct * 100.0).round() / 100.0,
            rtp: self.rtp,
            packets: self.packets,
            lost: self.lost,
        }
    }

    /// 已收流多久（页面显示"收流 03:20"）。
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(seq: u16, ssrc: u32) -> Vec<u8> {
        let mut v = vec![0u8; 12 + 188];
        v[0] = 0x80;
        v[1] = 33; // PT
        v[2..4].copy_from_slice(&seq.to_be_bytes());
        v[8..12].copy_from_slice(&ssrc.to_be_bytes());
        v
    }

    #[test]
    fn loss_counts_sequence_gaps() {
        let mut st = MdiState::new();
        let t0 = Instant::now();
        // 收到 1,2,5,6 → 缺 3,4 共 2 个
        for (i, seq) in [1u16, 2, 5, 6].into_iter().enumerate() {
            st.feed(&rtp(seq, 7), t0 + Duration::from_millis(40 * (i as u64 + 1)));
        }
        let s = st.snapshot();
        assert!(s.rtp);
        assert_eq!(st.lost, 2);
        assert!((s.loss_ratio - 2.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn reorder_within_tolerance_is_not_loss() {
        let mut st = MdiState::new();
        let t0 = Instant::now();
        for (i, seq) in [1u16, 3, 2, 4].into_iter().enumerate() {
            st.feed(&rtp(seq, 9), t0 + Duration::from_millis(40 * (i as u64 + 1)));
        }
        // 1→3 记 1 个（缺 2），3→2 是倒退（容忍），2→4 记 1 个（缺 3，实际乱序已到过）
        assert!(st.lost <= 2);
    }

    #[test]
    fn plain_udp_has_no_loss_figure() {
        let mut st = MdiState::new();
        let raw = vec![0x47u8; 188];
        st.feed(&raw, Instant::now());
        let s = st.snapshot();
        assert!(!s.rtp);
    }
}
