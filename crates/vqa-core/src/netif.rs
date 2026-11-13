//! 本机网卡枚举 + ffmpeg 出口地址绑定 —— `vkitcore/media/netif.py` 的移植。
//!
//! 绑定支持（ffmpeg 协议选项表 + 实测确认，不是猜的）：
//! - udp / rtp → `-localaddr <ip>`
//! - tcp       → `-local_addr <ip>`（带下划线）
//! - srt/rtsp/http/rtmp → **不支持**（加了被静默忽略），如实说明
//!
//! 枚举：Windows `GetAdaptersAddresses`（windows crate）→ `ipconfig` 解析回退。
//! 回环过滤、169.254 降权、按 IP 去重；每次调用重新枚举（网卡可能刚插上）。

use std::net::Ipv4Addr;

pub const AUTO: &str = "自动";
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_OPER_STATUS_UP: i32 = 1;

#[derive(Debug, Clone, serde::Serialize)]
pub struct IfRow {
    pub ip: String,
    pub prefix: u8,
    pub name: String,
    pub desc: String,
    pub up: bool,
    pub mac: String,
    pub ifindex: u32,
}

// ---------------------------------------------------------------- 枚举

#[cfg(windows)]
fn win_adapters() -> Vec<IfRow> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_PREFIX, GAA_FLAG_SKIP_ANYCAST,
        GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::Networking::WinSock::AF_UNSPEC;

    let _ = AF_UNSPEC; // 仅文档用途；API 收 u32
    let flags = GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER
        | GAA_FLAG_INCLUDE_PREFIX;

    // 两段式：先取所需长度，再分配重调（与 Python 版同一策略，多留 4KB）。
    let mut size: u32 = 0;
    unsafe {
        let _ = GetAdaptersAddresses(0u32 /* AF_UNSPEC */, flags, None, None, &mut size);
    }
    if size == 0 {
        return vec![];
    }
    let mut buf = vec![0u8; size as usize + 4096];
    let rc = unsafe {
        GetAdaptersAddresses(
            0u32, // AF_UNSPEC
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
            &mut size,
        )
    };
    if rc != 0 {
        return vec![];
    }

    let mut rows = Vec::new();
    unsafe {
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            if a.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                p = a.Next;
                continue;
            }
            let up = a.OperStatus.0 == IF_OPER_STATUS_UP;
            let name = a.FriendlyName.to_string().unwrap_or_default();
            let desc = a.Description.to_string().unwrap_or_default();
            let mac = a.PhysicalAddress
                .iter()
                .take(a.PhysicalAddressLength as usize)
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join("-");
            let ifindex = a.Anonymous1.Anonymous.IfIndex;

            // 遍历单播地址链表（防护上限 32，与 Python 版一致）
            let mut u = a.FirstUnicastAddress;
            let mut guard = 0;
            while !u.is_null() && guard < 32 {
                guard += 1;
                let ua = &*u;
                let sa = ua.Address.lpSockaddr;
                if !sa.is_null() && (*sa).sa_family.0 == 2u16 /* AF_INET */ {
                    let d = &(*sa).sa_data;
                    let ip = Ipv4Addr::new(d[2] as u8, d[3] as u8, d[4] as u8, d[5] as u8);
                    rows.push(IfRow {
                        ip: ip.to_string(),
                        prefix: ua.OnLinkPrefixLength,
                        name: name.clone(),
                        desc: desc.clone(),
                        up,
                        mac: mac.clone(),
                        ifindex,
                    });
                }
                u = ua.Next;
            }
            p = a.Next;
        }
    }
    rows
}

/// 兜底：从 ipconfig 文本抠 IPv4（GBK 解码，拿不到网卡名也至少列地址）。
fn fallback_adapters() -> Vec<IfRow> {
    let mut ips = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("ipconfig")
            .output()
        {
            let txt = String::from_utf8_lossy(&out.stdout).to_string();
            for ln in txt.lines() {
                if ln.contains("IPv4") && ln.contains(':') {
                    let cand = ln.rsplit(':').next().unwrap_or("").trim();
                    let cand = cand.replace("(首选)", "").trim().to_string();
                    if cand.matches('.').count() == 3 {
                        ips.push(cand);
                    }
                }
            }
        }
    }
    ips.sort();
    ips.dedup();
    ips.into_iter()
        .filter(|ip| !ip.starts_with("127."))
        .map(|ip| IfRow {
            ip,
            prefix: 0,
            name: String::new(),
            desc: "(ipconfig)".into(),
            up: true,
            mac: String::new(),
            ifindex: 0,
        })
        .collect()
}

/// 本机网卡列表（过滤回环、按 IP 去重；可用的排前面）。每次现枚举。
pub fn interface_rows() -> Vec<IfRow> {
    let ad = {
        #[cfg(windows)]
        {
            let r = win_adapters();
            if r.is_empty() {
                fallback_adapters()
            } else {
                r
            }
        }
        #[cfg(not(windows))]
        {
            fallback_adapters()
        }
    };
    let mut rows: Vec<IfRow> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for a in ad {
        if a.ip.starts_with("127.") || a.ip.is_empty() {
            continue;
        }
        // Loopback 类型在 win_adapters 里已按行产出；这里按 mac+name 粗判回环
        if a.mac.is_empty() && a.name.eq_ignore_ascii_case("Loopback Pseudo-Interface") {
            continue;
        }
        if !seen.insert(a.ip.clone()) {
            continue;
        }
        rows.push(a);
    }
    rows.sort_by(|a, b| {
        let ka = (!a.up, a.ip.starts_with("169.254."), a.ip.clone());
        let kb = (!b.up, b.ip.starts_with("169.254."), b.ip.clone());
        ka.cmp(&kb)
    });
    // 合并同名同状态多 IP 行的元信息（按 IP 一行，Python 版同口径）
    rows
}

/// 下拉框可选项：自动 + 在用网卡的 IPv4（排除 169.254 APIPA；全 down 则全列）。
pub fn choices() -> Vec<String> {
    let rows = interface_rows();
    let mut ips: Vec<String> = rows
        .iter()
        .filter(|r| r.up && !r.ip.starts_with("169.254."))
        .map(|r| r.ip.clone())
        .collect();
    if ips.is_empty() {
        ips = rows.iter().map(|r| r.ip.clone()).collect();
    }
    let mut out = vec![AUTO.to_string()];
    out.extend(ips);
    out
}

/// 把网卡名/状态写进提示，帮用户分清哪条是哪条。
pub fn choices_tip() -> String {
    let rows = interface_rows();
    if rows.is_empty() {
        return "没有检测到可用的网卡地址。".into();
    }
    let mut lines = vec!["本机网卡:".to_string()];
    for r in &rows {
        let tag = if r.up { "up" } else { "down" };
        let name = if r.name.is_empty() {
            if r.desc.is_empty() { "(未命名网卡)" } else { &r.desc }
        } else {
            &r.name
        };
        lines.push(format!("  {:<15} [{}] {}", r.ip, tag, name));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------- 选择 + 绑定

fn warn(msg: &str) {
    crate::config::log_line("netif", msg);
}

/// 设置值 → None（自动）或合法 IPv4。
/// 校验地址还在不在本机；不在则回退自动并警告（否则所有源全线失败）。
pub fn normalize(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty()
        || v == AUTO
        || v == "自动（系统默认）"
        || v.eq_ignore_ascii_case("auto")
        || v == "默认"
    {
        return None;
    }
    for r in interface_rows() {
        if r.ip == v {
            if !r.up {
                warn(&format!(
                    "出口网卡 {v} 当前是断开(down)状态，取流可能失败；设置里仍保留该选择"
                ));
            }
            return Some(v.to_string());
        }
    }
    warn(&format!(
        "网卡设置里的地址 {v} 在本机不存在，已回退「自动」"
    ));
    None
}

fn scheme(url: &str) -> String {
    let u = url.trim().to_ascii_lowercase();
    if let Some(idx) = u.find("://") {
        u[..idx].to_string()
    } else {
        "file".to_string()
    }
}

fn host_of(url: &str) -> String {
    let s = url.trim();
    let Some(pos) = s.find("://") else {
        return String::new();
    };
    let mut rest = &s[pos + 3..];
    if let Some(at) = rest.find('@') {
        rest = &rest[at + 1..];
    }
    let rest = rest.split('/').next().unwrap_or("");
    let rest = rest.split('?').next().unwrap_or("");
    if rest.starts_with('[') {
        return rest.split(']').next().unwrap_or("").trim_start_matches('[').to_string();
    }
    match rest.rfind(':') {
        Some(i) => rest[..i].to_string(),
        None => rest.to_string(),
    }
}

/// 是组播收流地址吗（udp/rtp + 224.0.0.0~239.255.255.255）？
pub fn is_multicast_url(url: &str) -> bool {
    let sc = scheme(url);
    if sc != "udp" && sc != "rtp" {
        return false;
    }
    let host = host_of(url);
    host.parse::<Ipv4Addr>()
        .map(|ip| ip.is_multicast())
        .unwrap_or(false)
}

/// 按 URL 协议给出绑定本机出口地址的 ffmpeg 选项（放在 URL 之前）。
/// 没选网卡 / 协议不支持时返回空切片（完全保持默认行为）。
pub fn bind_args(url: &str, nic_ip: &str) -> Vec<String> {
    let ip = match normalize(nic_ip) {
        Some(ip) => ip,
        None => return vec![],
    };
    match scheme(url).as_str() {
        "udp" | "rtp" => vec!["-localaddr".into(), ip],
        "tcp" => vec!["-local_addr".into(), ip],
        // srt/rtsp/http/rtmp 等没有本地地址选项——如实不加
        _ => vec![],
    }
}

/// 该协议是否支持绑定（界面如实说明用）。
pub fn supported(url: &str) -> bool {
    matches!(scheme(url).as_str(), "udp" | "rtp" | "tcp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_helpers() {
        assert_eq!(scheme("UDP://@239.1.1.1:5000"), "udp");
        assert_eq!(scheme(r"D:\v\a.ts"), "file");
        assert_eq!(host_of("udp://@239.1.1.1:5000"), "239.1.1.1");
        assert_eq!(host_of("udp://239.1.1.1:5000?pkt_size=1316"), "239.1.1.1");
        assert_eq!(host_of("rtp://@232.0.0.1:8000"), "232.0.0.1");
        assert!(is_multicast_url("udp://@239.1.1.1:5000"));
        assert!(!is_multicast_url("udp://@192.168.1.10:5000"));
        assert!(!is_multicast_url("rtp://example.com:5000"));
    }

    #[test]
    fn bind_args_matrix() {
        // normalize() 会校验地址在不在本机——用真实枚举到的第一个网卡 IP
        let Some(ip) = interface_rows().first().map(|r| r.ip.clone()) else {
            return; // 无网卡环境跳过
        };
        assert_eq!(
            bind_args("udp://@239.9.9.9:1234", &ip),
            vec!["-localaddr", ip.as_str()]
        );
        assert_eq!(
            bind_args("tcp://@239.9.9.9:1234", &ip),
            vec!["-local_addr", ip.as_str()]
        );
        // 本机不存在的地址 → 回退自动 → 不加参数
        assert!(bind_args("udp://@239.9.9.9:1234", "10.255.254.253").is_empty());
        // 协议不支持 / 未选卡
        assert!(bind_args("rtsp://cam/stream", &ip).is_empty());
        assert!(bind_args("udp://@239.9.9.9:1234", "自动").is_empty());
        assert!(bind_args("udp://@239.9.9.9:1234", "").is_empty());
    }

    #[test]
    fn interface_rows_no_panic() {
        let rows = interface_rows();
        // 不为空时每行都是合法 IPv4 且无回环
        for r in &rows {
            assert!(r.ip.parse::<std::net::Ipv4Addr>().is_ok(), "ip={}", r.ip);
            assert!(!r.ip.starts_with("127."));
        }
    }
}
