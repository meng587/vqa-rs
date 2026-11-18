//! 微突发巡检：收一路组播/单播，按 1ms 时间桶统计到达字节量，
//! 用来观察网络微突发（单个 1ms 内涌入远高于均值的字节量）——
//! 交换机缓冲不足时，这类突发会先在接收端表现为抖动与丢包。
//!
//! 只统计到达时刻与字节数，不解 TS、不解码，开销极小。

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERIES_MS: usize = 240; // 页面 sparkline 保留最近 240 个 1ms 桶

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BurstCfg {
    pub url: String,
    pub nic_ip: String,
    /// 突发判定倍率：某个 1ms 桶字节量 ≥ 平均每 ms 字节量 × 该倍率即计一次突发
    pub ratio: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BurstStats {
    pub running: bool,
    pub secs: f64,
    pub packets: u64,
    pub bytes: u64,
    pub mbps_avg: f64,
    /// 单个 1ms 桶的峰值瞬时码率（Mbps）
    pub mbps_peak: f64,
    /// 峰值桶的字节量
    pub ms_peak_bytes: u64,
    /// 峰值 / 平均 的倍数
    pub peak_ratio: f64,
    /// 超阈值桶数（即"微突发"次数）
    pub burst_cnt: u64,
    /// 平均每 ms 字节
    pub bytes_per_ms_avg: f64,
    /// 最近 240 个 1ms 桶（单位：字节）
    pub series: Vec<u64>,
    pub url: String,
    pub err: Option<String>,
}

struct Inner {
    packets: u64,
    bytes: u64,
    buckets: Vec<(u64, u64)>, // (ms, bytes)
    peak: u64,
    peak_ms: u64,
    burst_cnt: u64,
}

pub struct BurstSession {
    inner: Arc<Mutex<Inner>>,
    running: Arc<AtomicBool>,
    started: Instant,
    ratio: f64,
    pub url: String,
}

impl BurstSession {
    pub fn start(cfg: &BurstCfg) -> anyhow::Result<Self> {
        let (host, port, _scheme) = crate::live::parse_udp_url(&cfg.url)?;
        let sock = crate::live::open_socket(&host, port, &cfg.nic_ip)
            .map_err(|e| anyhow::anyhow!("Socket 绑定失败：{e}"))?;
        let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));

        let inner = Arc::new(Mutex::new(Inner {
            packets: 0,
            bytes: 0,
            buckets: Vec::new(),
            peak: 0,
            peak_ms: 0,
            burst_cnt: 0,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let th_in = Arc::clone(&inner);
        let th_run = Arc::clone(&running);
        let ratio = cfg.ratio.max(1.05);

        std::thread::Builder::new()
            .name("vqa-burst-recv".into())
            .spawn(move || {
                let mut pkt = [0u8; 65535];
                let t0 = Instant::now();
                while th_run.load(Ordering::Relaxed) {
                    let n = match sock.recv_from(&mut pkt) {
                        Ok((n, _)) => n,
                        Err(_) => continue, // 超时：回到循环检查停止标志
                    };
                    if n == 0 {
                        continue;
                    }
                    let ms = t0.elapsed().as_millis() as u64;
                    if let Ok(mut g) = th_in.lock() {
                        g.packets += 1;
                        g.bytes += n as u64;
                        let mut new_bucket = false;
                        match g.buckets.last_mut() {
                            Some((m, b)) if *m == ms => *b += n as u64,
                            _ => {
                                new_bucket = true;
                                g.buckets.push((ms, n as u64));
                            }
                        }
                        if new_bucket {
                            // 上一个桶已结束：按当时的均值判断是否算一次微突发
                            if g.buckets.len() >= 2 {
                                let (_, prev) = g.buckets[g.buckets.len() - 2];
                                let avg = g.bytes as f64 / (ms.max(1) as f64);
                                if avg > 0.0 && prev as f64 >= avg * ratio {
                                    g.burst_cnt += 1;
                                }
                            }
                            // 只保留最近窗口，长时间跑不吃内存
                            if g.buckets.len() > SERIES_MS * 4 {
                                let drop_n = g.buckets.len() - SERIES_MS * 2;
                                g.buckets.drain(..drop_n);
                            }
                        }
                        let last = *g.buckets.last().map(|(_, b)| b).unwrap_or(&0);
                        if last > g.peak {
                            g.peak = last;
                            g.peak_ms = ms;
                        }
                    }
                }
            })?;

        Ok(Self {
            inner,
            running,
            started: Instant::now(),
            ratio,
            url: cfg.url.clone(),
        })
    }

    pub fn snapshot(&self) -> BurstStats {
        let secs = self.started.elapsed().as_secs_f64();
        let g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return BurstStats::default(),
        };
        let avg_bpm = if secs > 0.0 {
            g.bytes as f64 / (secs * 1000.0)
        } else {
            0.0
        };
        let mbps_avg = if secs > 0.0 {
            g.bytes as f64 * 8.0 / secs / 1e6
        } else {
            0.0
        };
        let mbps_peak = g.peak as f64 * 8.0 / 0.001 / 1e6;
        let peak_ratio = if avg_bpm > 0.0 {
            g.peak as f64 / avg_bpm
        } else {
            0.0
        };
        let burst_cnt = g.burst_cnt;
        let series: Vec<u64> = g
            .buckets
            .iter()
            .rev()
            .take(SERIES_MS)
            .rev()
            .map(|(_, b)| *b)
            .collect();
        BurstStats {
            running: self.running.load(Ordering::Relaxed),
            secs,
            packets: g.packets,
            bytes: g.bytes,
            mbps_avg,
            mbps_peak,
            ms_peak_bytes: g.peak,
            peak_ratio,
            burst_cnt,
            bytes_per_ms_avg: avg_bpm,
            series,
            url: self.url.clone(),
            err: None,
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for BurstSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 起一个本机 UDP 收端做自检（单测里用真 socket 不方便，这里只测统计口径）。
pub fn ratio_of(peak: u64, bytes: u64, secs: f64) -> f64 {
    let avg = if secs > 0.0 { bytes as f64 / (secs * 1000.0) } else { 0.0 };
    if avg > 0.0 {
        peak as f64 / avg
    } else {
        0.0
    }
}

#[allow(dead_code)]
fn _assert_socket(_: UdpSocket) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_math() {
        // 1s 内 1MB → 平均 1000B/ms；峰值桶 4000B → 4 倍
        assert!((ratio_of(4000, 1_000_000, 1.0) - 4.0).abs() < 0.01);
    }
}
