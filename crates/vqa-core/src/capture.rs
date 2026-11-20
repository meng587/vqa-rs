//! UDP/RTP 业务流抓包 —— 无需 Npcap 的轻量抓包引擎。
//!
//! 只绑定一路 `udp://` / `rtp://` 业务流，记录原始 UDP 数据报为 pcap，
//! 同时按 RTP/TS 规则抽取一段裸 TS 样本供播放器/分析页使用。
//! 不做全协议解析，也不创建网卡驱动。

use std::collections::VecDeque;
use std::fs::{create_dir_all, File};
use std::io::{BufWriter, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::compliance::NetStats;
use crate::live::{looks_like_rtp, open_socket, parse_udp_url, strip_rtp};
use crate::mdi::MdiState;

const PACKET_RING: usize = 200;
const TS_SAMPLE_MAX: u64 = 64 * 1024 * 1024;
const PCAP_SNAPLEN: u32 = 262_144;
const PCAP_LINKTYPE_ETHERNET: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct CapturePacket {
    /// 从抓包开始算的毫秒偏移。
    pub ts_ms: u64,
    pub src: String,
    pub dst: String,
    pub len: u32,
    pub kind: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureSnapshot {
    pub running: bool,
    pub url: String,
    pub nic_ip: String,
    pub out_dir: String,
    pub pcap_path: String,
    pub ts_path: String,
    pub pcap_bytes: u64,
    pub ts_bytes: u64,
    pub started_secs: u64,
    pub net: NetStats,
    pub packets: Vec<CapturePacket>,
    pub error: Option<String>,
}

struct CaptureShared {
    mdi: MdiState,
    pcap_bytes: u64,
    ts_bytes: u64,
    tail: Vec<u8>,
    rows: VecDeque<CapturePacket>,
    error: Option<String>,
}

pub struct CaptureSession {
    shared: Arc<Mutex<CaptureShared>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    url: String,
    nic_ip: String,
    out_dir: PathBuf,
    pcap_path: PathBuf,
    ts_path: PathBuf,
    started: Instant,
}

impl CaptureSession {
    pub fn start(url: &str, nic_ip: &str, out_dir: &Path) -> anyhow::Result<Self> {
        let target = url.split('?').next().unwrap_or(url).trim();
        let (host, port, scheme) = parse_udp_url(target)?;
        let sock = open_socket(&host, port, nic_ip)
            .map_err(|e| anyhow::anyhow!("抓包 Socket 绑定失败：{e}"))?;

        create_dir_all(out_dir)?;
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let host_tag = if host.is_empty() {
            "any".to_string()
        } else {
            safe_name(&host)
        };
        let base = format!("capture_{stamp}_{host_tag}_{port}");
        let pcap_path = out_dir.join(format!("{base}.pcap"));
        let ts_path = out_dir.join(format!("{base}.ts"));
        let pcap = PcapWriter::new(&pcap_path)?;
        let ts_file = File::create(&ts_path)?;
        let pcap_bytes = pcap.bytes();

        let shared = Arc::new(Mutex::new(CaptureShared {
            mdi: MdiState::new(),
            pcap_bytes,
            ts_bytes: 0,
            tail: Vec::new(),
            rows: VecDeque::with_capacity(PACKET_RING + 8),
            error: None,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let th_shared = Arc::clone(&shared);
        let th_run = Arc::clone(&running);
        let forced_rtp = scheme == "rtp";
        let dst_ip = host.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::UNSPECIFIED);
        let dst_tag = if host.is_empty() {
            "*".to_string()
        } else {
            host.clone()
        };
        let t0 = Instant::now();

        let thread = std::thread::Builder::new()
            .name("vqa-capture".into())
            .spawn(move || {
                let mut pcap = pcap;
                let mut ts = BufWriter::new(ts_file);
                let mut pkt = [0u8; 65535];
                let mut rtp_detected = if forced_rtp { Some(true) } else { None };
                while th_run.load(Ordering::Relaxed) {
                    let (n, src) = match sock.recv_from(&mut pkt) {
                        Ok(v) => v,
                        Err(_) => continue, // 1s 超时/中断：回到循环检查停止标志
                    };
                    if n == 0 {
                        continue;
                    }
                    let data = &pkt[..n];
                    let now = Instant::now();
                    let Ok(mut c) = th_shared.lock() else {
                        break;
                    };

                    // 传输层统计先吃原始包（RTP 头还在，序号才能算丢包）
                    c.mdi.feed(data, now);

                    if let Err(e) = pcap.write_udp(SystemTime::now(), src, dst_ip, port, data) {
                        c.error = Some(format!("pcap 写入失败：{e}"));
                        th_run.store(false, Ordering::Relaxed);
                        break;
                    }
                    c.pcap_bytes = pcap.bytes();

                    let is_rtp = match rtp_detected {
                        Some(v) => v,
                        None => {
                            // 首个数据报可能是 RTCP 或残包：识别不到时继续观察，
                            // 一旦某个包符合 RTP 特征就固定为 RTP，避免整段误判为裸 UDP。
                            let v = looks_like_rtp(data);
                            if v {
                                rtp_detected = Some(true);
                            }
                            v
                        }
                    };
                    let payload = if is_rtp { strip_rtp(data) } else { data };

                    // 与实时分析一致的 188 对齐；只保留前 64MB 作为 TS 样本
                    if !payload.is_empty() {
                        c.tail.extend_from_slice(payload);
                        let n_ts = c.tail.len() / 188;
                        if n_ts > 0 {
                            let usable = n_ts * 188;
                            let take: Vec<u8> = c.tail.drain(..usable).collect();
                            if c.ts_bytes < TS_SAMPLE_MAX {
                                let cap =
                                    (TS_SAMPLE_MAX - c.ts_bytes).min(take.len() as u64) as usize;
                                if let Err(e) = ts.write_all(&take[..cap]) {
                                    c.error = Some(format!("TS 样本写入失败：{e}"));
                                    th_run.store(false, Ordering::Relaxed);
                                    break;
                                }
                                c.ts_bytes += cap as u64;
                            }
                        }
                    }

                    let kind = if is_rtp {
                        "RTP"
                    } else if payload.len() >= 188
                        && payload.len() % 188 == 0
                        && payload.first() == Some(&0x47)
                    {
                        "TS"
                    } else {
                        "UDP"
                    };
                    let summary = if is_rtp {
                        let seq = if data.len() >= 4 {
                            u16::from_be_bytes([data[2], data[3]])
                        } else {
                            0
                        };
                        let pt = data.get(1).copied().unwrap_or(0) & 0x7f;
                        format!("RTP seq {seq} · PT {pt}")
                    } else if kind == "TS" {
                        format!("TS ×{}", payload.len() / 188)
                    } else {
                        format!("UDP payload {} B", data.len())
                    };
                    c.rows.push_back(CapturePacket {
                        ts_ms: t0.elapsed().as_millis() as u64,
                        src: format!("{}:{}", src.ip(), src.port()),
                        dst: format!("{dst_tag}:{port}"),
                        len: data.len() as u32,
                        kind: kind.to_string(),
                        summary,
                    });
                    while c.rows.len() > PACKET_RING {
                        c.rows.pop_front();
                    }
                }
                let _ = ts.flush();
                th_run.store(false, Ordering::Relaxed);
            })?;

        Ok(Self {
            shared,
            running,
            thread: Some(thread),
            url: url.to_string(),
            nic_ip: nic_ip.to_string(),
            out_dir: out_dir.to_path_buf(),
            pcap_path,
            ts_path,
            started: Instant::now(),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }

    pub fn snapshot(&self) -> CaptureSnapshot {
        let (net, rows, pcap_bytes, ts_bytes, error) = {
            let c = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            (
                c.mdi.snapshot(),
                c.rows.iter().cloned().collect::<Vec<_>>(),
                c.pcap_bytes,
                c.ts_bytes,
                c.error.clone(),
            )
        };
        CaptureSnapshot {
            running: self.is_running(),
            url: self.url.clone(),
            nic_ip: self.nic_ip.clone(),
            out_dir: self.out_dir.to_string_lossy().to_string(),
            pcap_path: self.pcap_path.to_string_lossy().to_string(),
            ts_path: self.ts_path.to_string_lossy().to_string(),
            pcap_bytes,
            ts_bytes,
            started_secs: self.started.elapsed().as_secs(),
            net,
            packets: rows,
            error,
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

struct PcapWriter {
    w: BufWriter<File>,
    bytes: u64,
    ip_id: u16,
}

impl PcapWriter {
    fn new(path: &Path) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        w.write_all(&pcap_global_header())?;
        Ok(Self {
            w,
            bytes: 24,
            ip_id: 1,
        })
    }

    fn bytes(&self) -> u64 {
        self.bytes
    }

    fn write_udp(
        &mut self,
        ts: SystemTime,
        src: SocketAddr,
        dst_ip: Ipv4Addr,
        dst_port: u16,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let frame = build_udp_frame(src, dst_ip, dst_port, payload, self.ip_id);
        self.ip_id = self.ip_id.wrapping_add(1);
        let d = ts.duration_since(UNIX_EPOCH).unwrap_or_default();
        let sec = d.as_secs() as u32;
        let usec = d.subsec_micros();
        let len = frame.len() as u32;
        let mut hdr = [0u8; 16];
        hdr[0..4].copy_from_slice(&sec.to_le_bytes());
        hdr[4..8].copy_from_slice(&usec.to_le_bytes());
        hdr[8..12].copy_from_slice(&len.to_le_bytes());
        hdr[12..16].copy_from_slice(&len.to_le_bytes());
        self.w.write_all(&hdr)?;
        self.w.write_all(&frame)?;
        self.bytes += 16 + frame.len() as u64;
        Ok(())
    }
}

fn pcap_global_header() -> [u8; 24] {
    let mut h = [0u8; 24];
    h[0..4].copy_from_slice(&0xa1b2c3d4u32.to_le_bytes());
    h[4..6].copy_from_slice(&2u16.to_le_bytes());
    h[6..8].copy_from_slice(&4u16.to_le_bytes());
    h[16..20].copy_from_slice(&PCAP_SNAPLEN.to_le_bytes());
    h[20..24].copy_from_slice(&PCAP_LINKTYPE_ETHERNET.to_le_bytes());
    h
}

fn build_udp_frame(
    src: SocketAddr,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    payload: &[u8],
    ip_id: u16,
) -> Vec<u8> {
    // IPv4 总长上限 65535，UDP 负载最多 65507；正常 IPTV 包远小于此。
    let payload = &payload[..payload.len().min(65507)];
    let udp_len = 8 + payload.len();
    let ip_len = 20 + udp_len;
    let mut f = Vec::with_capacity(14 + ip_len);

    // Ethernet：组播按 MAC 映射，单播用全零目的 MAC（Wireshark 可正常解析）
    let dst_mac = if dst_ip.is_multicast() {
        let n = u32::from(dst_ip);
        [
            0x01,
            0x00,
            0x5e,
            ((n >> 16) & 0x7f) as u8,
            ((n >> 8) & 0xff) as u8,
            (n & 0xff) as u8,
        ]
    } else {
        [0, 0, 0, 0, 0, 0]
    };
    f.extend_from_slice(&dst_mac);
    f.extend_from_slice(&[0x02, 0, 0, 0, 0, 0x01]);
    f.extend_from_slice(&0x0800u16.to_be_bytes());

    let (src_ip, src_port) = match src {
        SocketAddr::V4(v4) => (v4.ip().octets(), v4.port()),
        _ => ([0u8; 4], 0),
    };
    let mut ip = [0u8; 20];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&(ip_len as u16).to_be_bytes());
    ip[4..6].copy_from_slice(&ip_id.to_be_bytes());
    ip[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // DF
    ip[8] = 64;
    ip[9] = 17; // UDP
    ip[12..16].copy_from_slice(&src_ip);
    ip[16..20].copy_from_slice(&dst_ip.octets());
    let sum = checksum(&ip);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    f.extend_from_slice(&ip);

    f.extend_from_slice(&src_port.to_be_bytes());
    f.extend_from_slice(&dst_port.to_be_bytes());
    f.extend_from_slice(&(udp_len as u16).to_be_bytes());
    f.extend_from_slice(&[0u8, 0u8]); // IPv4 UDP checksum 可省略
    f.extend_from_slice(payload);
    f
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        sum += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += (last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_frame_has_ip_and_udp_headers() {
        let frame = build_udp_frame(
            SocketAddr::from(([192, 168, 1, 2], 5000)),
            Ipv4Addr::new(239, 1, 1, 1),
            5000,
            &[0x47; 188],
            7,
        );
        assert_eq!(frame.len(), 14 + 20 + 8 + 188);
        assert_eq!(&frame[0..6], &[0x01, 0x00, 0x5e, 0x01, 0x01, 0x01]);
        assert_eq!(&frame[12..14], &[0x08, 0x00]);
        assert_eq!(frame[14], 0x45);
        assert_eq!(
            u16::from_be_bytes([frame[16], frame[17]]) as usize,
            20 + 8 + 188
        );
        assert_eq!(frame[14 + 9], 17);
        assert_eq!(u16::from_be_bytes([frame[34], frame[35]]), 5000);
        // 有效 IPv4 头连同校验和再算一次，结果应为 0。
        assert_eq!(checksum(&frame[14..34]), 0);
    }

    #[test]
    fn pcap_header_is_little_endian_ethernet() {
        let h = pcap_global_header();
        assert_eq!(u32::from_le_bytes([h[0], h[1], h[2], h[3]]), 0xa1b2c3d4);
        assert_eq!(u16::from_le_bytes([h[4], h[5]]), 2);
        assert_eq!(u16::from_le_bytes([h[6], h[7]]), 4);
        assert_eq!(u32::from_le_bytes([h[20], h[21], h[22], h[23]]), 1);
    }

    #[test]
    fn captures_udp_and_writes_pcap_and_ts_sample() {
        use std::net::UdpSocket;
        use std::time::Duration;

        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let dir = std::env::temp_dir().join(format!(
            "vqa_capture_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = CaptureSession::start(&format!("udp://127.0.0.1:{port}"), "", &dir).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut ts_pkt = [0u8; 188];
        ts_pkt[0] = 0x47;
        ts_pkt[1] = 0x40;
        ts_pkt[2] = 0x00;
        ts_pkt[3] = 0x10;
        for _ in 0..10 {
            tx.send_to(&ts_pkt, ("127.0.0.1", port)).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(250));
        let alive = s.snapshot();
        s.stop();
        let snap = s.snapshot();

        assert!(alive.running);
        assert_eq!(alive.net.packets, 10);
        assert_eq!(alive.packets.len(), 10);
        assert!(snap.pcap_bytes > 24, "pcap bytes = {}", snap.pcap_bytes);
        assert!(snap.ts_bytes >= 188, "ts bytes = {}", snap.ts_bytes);
        let pcap = std::fs::read(&snap.pcap_path).unwrap();
        assert_eq!(&pcap[0..4], &0xa1b2c3d4u32.to_le_bytes());
        assert_eq!(pcap.len() as u64, snap.pcap_bytes);
        let rec_len = u32::from_le_bytes([pcap[32], pcap[33], pcap[34], pcap[35]]) as usize;
        assert_eq!(rec_len, 14 + 20 + 8 + 188);
        assert_eq!(pcap.len(), 24 + 10 * (16 + rec_len));
        let ts = std::fs::read(&snap.ts_path).unwrap();
        assert_eq!(ts.len(), 188 * 10);
        assert_eq!(ts[0], 0x47);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
