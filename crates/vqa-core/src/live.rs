//! 实时 UDP/RTP 收流分析 —— `AnalysisPage._live_recv_loop` 的移植。
//!
//! 数据通路：UDP socket（组播 IGMP join，可选出口网卡）→ 去 RTP 头
//! （rtp:// 强制；udp:// 自动探测）→ 按 188 对齐切包 → 逐包喂
//! [`Scanner`]（偏移=累计字节，到达时间=单调时钟差）。
//!
//! 包环形缓冲保留最近 300 个原始包供「TS 包视图」。

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::tsfile::{Scanner, TrOverrides};

const RING_MAX: usize = 300;
/// 开头保留的裸 TS 样本上限（约 10 秒高清量级，够 ffprobe 出节目参数）
const DUMP_MAX: usize = 6 * 1024 * 1024;

/// 解析 `udp://@239.1.1.1:5000` / `udp://:5000` / `rtp://@host:port`。
pub fn parse_udp_url(url: &str) -> anyhow::Result<(String, u16, String)> {
    let Some((scheme, rest)) = url.split_once("://") else {
        anyhow::bail!("地址格式应为 udp://[host:]端口");
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "udp" && scheme != "rtp" {
        anyhow::bail!("实时分析仅支持 udp:// 或 rtp:// 地址");
    }
    let rest = rest.trim_start_matches('@').trim_end_matches('/');
    let Some((host_part, port_part)) = rest.rsplit_once(':') else {
        anyhow::bail!("缺少端口号");
    };
    let port: u16 = port_part
        .parse()
        .map_err(|_| anyhow::anyhow!("端口不合法：{port_part}"))?;
    Ok((host_part.to_string(), port, scheme))
}

/// 打开收流 socket：REUSEADDR → 绑定（网卡 IP 可空）→ 组播 join → 1s 超时。
pub fn open_socket(host: &str, port: u16, bind_ip: &str) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    let addr: std::net::SocketAddr = match bind_ip.parse::<std::net::Ipv4Addr>() {
        Ok(ip) => (ip, port).into(),
        Err(_) => (std::net::Ipv4Addr::UNSPECIFIED, port).into(),
    };
    sock.bind(&addr.into())?;
    // 组播加入（mreq = 组地址 + 出口接口地址；0.0.0.0 = 系统路由决定）
    if let Ok(group) = host.parse::<std::net::Ipv4Addr>() {
        if group.is_multicast() {
            let iface: std::net::Ipv4Addr = bind_ip
                .parse()
                .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
            let _ = sock.join_multicast_v4(&group, &iface);
        }
    }
    let s: UdpSocket = sock.into();
    s.set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(s)
}

/// 启发式判断 UDP 负载是否为 RTP 封装：版本=2，且去头后长度是 188 整数倍。
pub fn looks_like_rtp(data: &[u8]) -> bool {
    if data.len() < 12 + 188 {
        return false;
    }
    if data[0] >> 6 != 2 {
        return false;
    }
    let mut header = 12 + 4 * (data[0] & 0x0F) as usize;
    if data[0] & 0x10 != 0 {
        // extension
        if data.len() > header + 4 {
            let ext_len = u16::from_be_bytes([data[header + 2], data[header + 3]]) as usize;
            header += 4 + ext_len * 4;
        }
    }
    let payload = data.len().saturating_sub(header);
    payload > 0 && payload % 188 == 0
}

/// 剥离 RTP 头，返回 TS 负载；支持 CSRC 与扩展头。
pub fn strip_rtp(data: &[u8]) -> &[u8] {
    if data.len() < 12 || data[0] >> 6 != 2 {
        return data;
    }
    let cc = data[0] & 0x0F;
    let mut header = 12 + 4 * cc as usize;
    if data[0] & 0x10 != 0 {
        if data.len() < header + 4 {
            return &[];
        }
        let ext_len = u16::from_be_bytes([data[header + 2], data[header + 3]]) as usize;
        header += 4 + ext_len * 4;
    }
    if header > data.len() {
        return &[];
    }
    &data[header..]
}

/// 共享的实时分析核心（UI 线程按秒锁读）。
pub struct LiveCore {    pub scanner: Scanner,
    /// 传输层统计（抖动/丢包/MDI-DF/MLR），合规检测取这里
    pub mdi: crate::mdi::MdiState,
    /// 最近原始包环形缓冲（TS 包视图）
    pub ring: std::collections::VecDeque<Vec<u8>>,
    pub bytes: u64,
    /// 开头一段裸 TS 样本（给 ffprobe 探节目参数用，取满即停）
    pub dump: Vec<u8>,
    /// 未凑满 188 的尾巴
    tail: Vec<u8>,
}

/// 一路实时分析会话：socket + 收流线程。
pub struct LiveSession {
    core: Arc<Mutex<LiveCore>>,
    running: Arc<AtomicBool>,
    pub url: String,
    pub forced_rtp: bool,
    pub started: Instant,
    pub dead: Arc<AtomicBool>,
}

impl LiveSession {
    /// 起一路实时分析。失败多数是 socket 绑定/组播 join 问题。
    pub fn start(
        url: &str,
        nic_ip: &str,
        overrides: &TrOverrides,
    ) -> anyhow::Result<Self> {
        let (host, port, scheme) = parse_udp_url(url)?;
        let sock = open_socket(&host, port, nic_ip)
            .map_err(|e| anyhow::anyhow!("Socket绑定失败: {e}"))?;
        let forced_rtp = scheme == "rtp";

        let mut res = super::tsfile::ScanResult::new(url);
        res.packet_size = 188;
        let scanner = Scanner::new(res, overrides);
        let core = Arc::new(Mutex::new(LiveCore {
            scanner,
            mdi: crate::mdi::MdiState::new(),
            ring: std::collections::VecDeque::with_capacity(RING_MAX + 8),
            bytes: 0,
            dump: Vec::new(),
            tail: Vec::new(),
        }));

        let running = Arc::new(AtomicBool::new(true));
        let dead = Arc::new(AtomicBool::new(false));
        let th_core = Arc::clone(&core);
        let th_run = Arc::clone(&running);
        let th_dead = Arc::clone(&dead);
        let t0 = Instant::now();
        std::thread::Builder::new()
            .name("vqa-live-recv".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let mut pkt = [0u8; 65535];
                let mut rtp_detected: Option<bool> = None;
                while th_run.load(Ordering::Relaxed) {
                    let data = match sock.recv_from(&mut pkt) {
                        Ok((n, _)) => &pkt[..n],
                        Err(_) => continue, // 超时/中断：继续循环等停止标志
                    };
                    if data.is_empty() {
                        continue;
                    }
                    // 传输层统计先吃原始包（RTP 头还在，序号才能算丢包）
                    if let Ok(mut c) = th_core.lock() {
                        c.mdi.feed(data, Instant::now());
                    }
                    buf.clear();
                    // RTP 处理
                    if forced_rtp {
                        buf.extend_from_slice(strip_rtp(data));
                    } else {
                        if rtp_detected.is_none() {
                            rtp_detected = Some(looks_like_rtp(data));
                        }
                        if rtp_detected == Some(true) {
                            buf.extend_from_slice(strip_rtp(data));
                        } else {
                            buf.extend_from_slice(data);
                        }
                    }
                    if buf.is_empty() {
                        continue;
                    }
                    let Ok(mut c) = th_core.lock() else { return };
                    // 拼 188 对齐
                    c.tail.extend_from_slice(&buf);
                    let n_pkts = c.tail.len() / 188;
                    if n_pkts == 0 {
                        continue;
                    }
                    let usable = n_pkts * 188;
                    let take: Vec<u8> = c.tail.drain(..usable).collect();
                    if c.dump.len() < DUMP_MAX {
                        let cap = DUMP_MAX - c.dump.len();
                        let n = cap.min(take.len());
                        c.dump.extend_from_slice(&take[..n]);
                    }
                    let t_arr = t0.elapsed().as_secs_f64();
                    for chunk in take.chunks(188) {
                        let off = c.bytes as f64;
                        c.bytes += 188;
                        c.ring.push_back(chunk.to_vec());
                        if c.ring.len() > RING_MAX {
                            c.ring.pop_front();
                        }
                        c.scanner.feed(chunk, off, Some(t_arr));
                    }
                }
                th_dead.store(true, Ordering::Relaxed);
            })?;

        Ok(Self {
            core,
            running,
            url: url.to_string(),
            forced_rtp,
            started: Instant::now(),
            dead,
        })
    }

    /// 锁内访问分析核心（UI 帧内短临界区）。
    pub fn with_core<R>(&self, f: impl FnOnce(&mut LiveCore) -> R) -> Option<R> {
        self.core.lock().ok().map(|mut g| f(&mut g))
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// 判决用的裸 TS 样本：攒够 1.5MB 就整体交出（只交一次）
    pub fn take_dump(&self, min_bytes: usize) -> Option<Vec<u8>> {
        self.with_core(|c| {
            if c.dump.len() >= min_bytes {
                let d = std::mem::take(&mut c.dump);
                Some(d)
            } else {
                None
            }
        })
        .flatten()
    }

    pub fn stop(self) {
        self.running.store(false, Ordering::Relaxed);
        // 线程在读超时（≤1s）后退出；core Arc 随引用计数释放
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        assert_eq!(
            parse_udp_url("udp://@239.1.1.1:5000").unwrap(),
            ("239.1.1.1".to_string(), 5000, "udp".to_string())
        );
        assert_eq!(
            parse_udp_url("udp://:5000").unwrap(),
            ("".to_string(), 5000, "udp".to_string())
        );
        assert_eq!(
            parse_udp_url("rtp://@232.0.0.1:8000").unwrap(),
            ("232.0.0.1".to_string(), 8000, "rtp".to_string())
        );
        assert!(parse_udp_url("http://x").is_err());
        assert!(parse_udp_url("udp://noport").is_err());
    }

    #[test]
    fn rtp_heuristics() {
        // 裸 TS：0x47 起头 → 版本位 (0x47>>6)=1 ≠ 2 → 不像 RTP
        let ts = vec![0x47u8; 188 * 2];
        assert!(!looks_like_rtp(&ts));
        assert_eq!(strip_rtp(&ts), &ts[..]);

        // RTP 头 v=2，无 CSRC 无扩展 + 2 个 TS 包
        let mut rtp = vec![0x80u8, 96, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1];
        rtp.extend(std::iter::repeat_n(0x47u8, 188 * 2));
        assert!(looks_like_rtp(&rtp));
        assert_eq!(strip_rtp(&rtp).len(), 188 * 2);
    }
}
