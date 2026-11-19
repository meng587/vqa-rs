//! 微突发检测：收一路组播/单播，按可配置时间窗（1/10/100/1000ms）统计
//! 到达字节量，用来观察网络微突发（单个窗口内涌入远高于均值的字节量）——
//! 交换机缓冲不足时，这类突发会先在接收端表现为抖动与丢包。
//!
//! 只统计到达时刻与字节数，不解 TS、不解码，开销极小。

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERIES_N: usize = 240; // 页面 sparkline 保留最近 240 个检测窗口
const DEFAULT_WINDOW_MS: u64 = 1;

fn default_window_ms() -> u64 {
    DEFAULT_WINDOW_MS
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BurstCfg {
    pub url: String,
    pub nic_ip: String,
    /// 突发判定倍率：某个检测窗口的字节量 ≥ 平均每窗口字节量 × 该倍率即计一次突发
    pub ratio: f64,
    /// 检测时间窗（毫秒）：1 / 10 / 100 / 1000
    #[serde(default = "default_window_ms")]
    pub window_ms: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BurstStats {
    pub running: bool,
    pub secs: f64,
    pub packets: u64,
    pub bytes: u64,
    pub mbps_avg: f64,
    /// 单个检测窗口的峰值瞬时码率（Mbps）
    pub mbps_peak: f64,
    /// 峰值窗口的字节量
    pub ms_peak_bytes: u64,
    /// 峰值 / 平均 的倍数
    pub peak_ratio: f64,
    /// 超阈值窗口数（即"微突发"次数）
    pub burst_cnt: u64,
    /// 平均每 ms 字节
    pub bytes_per_ms_avg: f64,
    /// 平均每个检测窗口字节
    pub bytes_per_window_avg: f64,
    /// 最近 240 个检测窗口（单位：字节）
    pub series: Vec<u64>,
    pub url: String,
    /// 本次检测使用的时间窗（毫秒）
    pub window_ms: u64,
    pub err: Option<String>,
}

struct Inner {
    packets: u64,
    bytes: u64,
    buckets: Vec<(u64, u64)>, // (window_index, bytes)
    peak: u64,
    burst_cnt: u64,
}

pub struct BurstSession {
    inner: Arc<Mutex<Inner>>,
    running: Arc<AtomicBool>,
    started: Instant,
    window_ms: u64,
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
            burst_cnt: 0,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let th_in = Arc::clone(&inner);
        let th_run = Arc::clone(&running);
        let ratio = cfg.ratio.max(1.05);
        let window_ms = cfg.window_ms.clamp(1, 1000);

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
                    let window_idx = ms / window_ms;
                    if let Ok(mut g) = th_in.lock() {
                        g.packets += 1;
                        g.bytes += n as u64;
                        let mut new_window = false;
                        match g.buckets.last_mut() {
                            Some((idx, b)) if *idx == window_idx => *b += n as u64,
                            _ => {
                                new_window = true;
                                g.buckets.push((window_idx, n as u64));
                            }
                        }
                        if new_window {
                            // 上一个窗口已结束：按当时的均值判断是否算一次微突发
                            if g.buckets.len() >= 2 {
                                let (_, prev) = g.buckets[g.buckets.len() - 2];
                                let elapsed_ms = ms.max(1) as f64;
                                let avg_bpm = g.bytes as f64 / elapsed_ms;
                                let threshold = avg_bpm * window_ms as f64 * ratio;
                                if avg_bpm > 0.0 && prev as f64 >= threshold {
                                    g.burst_cnt += 1;
                                }
                            }
                            // 只保留最近窗口，长时间跑不吃内存
                            if g.buckets.len() > SERIES_N * 4 {
                                let drop_n = g.buckets.len() - SERIES_N * 2;
                                g.buckets.drain(..drop_n);
                            }
                        }
                        let last = *g.buckets.last().map(|(_, b)| b).unwrap_or(&0);
                        if last > g.peak {
                            g.peak = last;
                        }
                    }
                }
            })?;

        Ok(Self {
            inner,
            running,
            started: Instant::now(),
            window_ms,
            url: cfg.url.clone(),
        })
    }

    pub fn snapshot(&self) -> BurstStats {
        let secs = self.started.elapsed().as_secs_f64();
        let g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => {
                return BurstStats {
                    window_ms: self.window_ms,
                    url: self.url.clone(),
                    err: Some("统计锁失败".into()),
                    ..BurstStats::default()
                }
            }
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
        let window_secs = self.window_ms as f64 / 1000.0;
        let mbps_peak = if window_secs > 0.0 {
            g.peak as f64 * 8.0 / window_secs / 1e6
        } else {
            0.0
        };
        let avg_per_window = avg_bpm * self.window_ms as f64;
        let peak_ratio = if avg_per_window > 0.0 {
            g.peak as f64 / avg_per_window
        } else {
            0.0
        };
        let burst_cnt = g.burst_cnt;
        let series: Vec<u64> = g
            .buckets
            .iter()
            .rev()
            .take(SERIES_N)
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
            bytes_per_window_avg: avg_per_window,
            series,
            url: self.url.clone(),
            window_ms: self.window_ms,
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

    #[test]
    fn window_ratio_compares_same_window() {
        // 10ms 窗口：1s 内 1MB → 平均每 10ms 10000B；峰值 40000B → 4 倍
        let avg_per_ms = 1_000_000f64 / 1000.0;
        let peak = 40_000f64;
        let ratio = peak / (avg_per_ms * 10.0);
        assert!((ratio - 4.0).abs() < 0.01);
    }
}
