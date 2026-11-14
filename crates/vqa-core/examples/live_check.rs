//! 实时分析链路 headless 验证：ffmpeg 推 TS 到回环 UDP → LiveSession 收流 →
//! 断言字节/包数/TR290 产出。
//!
//! 用法：cargo run -p vqa-core --example live_check -- <ts文件>

use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let Some(ts) = std::env::args().nth(1) else {
        anyhow::bail!("用法: live_check <ts文件>");
    };
    let Some(ffmpeg) = vqa_core::tools::find_ffmpeg() else {
        anyhow::bail!("未找到 ffmpeg");
    };
    let url = "udp://127.0.0.1:5007";

    // 1) 后台推流（单遍，约 20s 内容）
    let mut cmd = vqa_core::ffmpeg::hidden(&ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-re", "-i", &ts,
              "-c", "copy", "-f", "mpegts", &format!("{url}?pkt_size=1316")]);
    let mut pusher = cmd.spawn()?;
    println!("[1] ffmpeg 推流已启动 → {url}");

    // 2) 实时会话收流 5 秒
    let session = vqa_core::live::LiveSession::start(url, "", &Default::default())?;
    println!("[2] LiveSession 已建立");
    let t0 = Instant::now();
    let mut last_bytes = 0u64;
    let mut got_packets = false;
    while t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(500));
        let Some(bytes) = session.with_core(|c| {
            c.bytes
        }) else { break };
        if bytes > last_bytes {
            last_bytes = bytes;
        }
        if bytes > 188 * 100 {
            got_packets = true;
        }
    }
    let summary = session.with_core(|c| {
        let res = &c.scanner.res;
        (
            c.bytes,
            res.packets,
            res.programs.len(),
            res.has_pat,
            res.has_pmt,
            res.tr290.iter().filter(|i| i.count > 0).count(),
        )
    });
    let (bytes, packets, progs, pat, pmt, errs) = summary.unwrap_or_default();
    println!(
        "[3] 5s 收流: bytes={bytes} packets={packets} programs={progs} PAT={pat} PMT={pmt} tr290_active={errs}"
    );
    session.stop();
    let _ = pusher.kill();

    // 3) 断言
    anyhow::ensure!(got_packets, "未收到足够 TS 包（组播/socket 链路故障）");
    anyhow::ensure!(packets > 500, "包数异常: {packets}");
    anyhow::ensure!(pat && pmt, "PAT/PMT 未解出");
    println!("[✓] 实时分析链路验证通过");
    Ok(())
}
