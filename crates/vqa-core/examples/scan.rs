//! headless TS 扫描 CLI —— 对拍验证用。
//!
//! 用法：`cargo run -p vqa-core --example scan -- <ts文件> [--json]`
//!
//! 输出摘要：包长/包数/时长/平均码率/节目表/一级二级三级错误计数，
//! `--json` 时输出完整 `ScanResult::to_json()`。

use std::io::Write;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        anyhow::bail!("用法: scan <ts文件> [--json]");
    };
    let json_mode = args.iter().any(|a| a == "--json");

    let start = std::time::Instant::now();
    let mut last_pct = 0u64;
    let res = vqa_core::tsfile::scan_file(
        std::path::Path::new(path),
        Some(&mut |done, total| {
            let pct = if total > 0 { done * 100 / total } else { 0 };
            if pct / 10 > last_pct {
                last_pct = pct / 10;
                print!("{}% ", pct);
                let _ = std::io::stdout().flush();
            }
        }),
        None,
        &Default::default(),
    );
    println!("\n扫描耗时 {:.1}s", start.elapsed().as_secs_f64());

    if res.not_ts {
        anyhow::bail!("不是 188/204 包长的 TS 流: {path}");
    }

    if json_mode {
        println!("{}", serde_json::to_string_pretty(&res.to_json())?);
        return Ok(());
    }

    println!("文件            : {}", res.path);
    println!("大小            : {} bytes", res.file_size);
    println!("包长            : {}", res.packet_size);
    println!("包数            : {}", res.packets);
    println!("同步错误        : {} bytes", res.sync_errors);
    println!(
        "PCR 时长        : {:.3} s ({:.3} ~ {:.3})",
        res.duration_pcr(),
        res.first_pcr.unwrap_or(0.0),
        res.last_pcr.unwrap_or(0.0)
    );
    println!("平均码率        : {:.0} bps ({:.3} Mbps)", res.bitrate_avg(), res.bitrate_avg() / 1e6);
    println!("码率点          : {}", res.rate.len());
    println!("PCR PID         : {:?}", res.pcr_pid);
    println!("加密            : {}", if res.encrypted() { "是" } else { "否" });
    println!("节目数          : {}", res.programs.len());
    for p in &res.programs {
        println!(
            "  节目 {:>5}  PMT=0x{:04X} PCR=0x{:04X}  {} 路流",
            p.number,
            p.pmt_pid,
            p.pcr_pid,
            p.streams.len()
        );
        for s in &p.streams {
            println!("    0x{:04X}  {} ({})", s.pid, s.type_name(), s.kind);
        }
    }
    println!("SI 表           : {}", {
        let names: Vec<String> = res.si.keys().cloned().collect();
        names.join(", ")
    });
    // TR 101 290 按级汇总
    let (mut n1, mut n2, mut n3) = (0u64, 0u64, 0u64);
    for it in &res.tr290 {
        match it.level {
            1 => n1 += it.count,
            2 => n2 += it.count,
            _ => n3 += it.count,
        }
    }
    println!("TR290 错误      : 一级 {n1} / 二级 {n2} / 三级 {n3}");
    for it in &res.tr290 {
        println!(
            "  [P{}] {:<28} x{:<6} {}  {}",
            it.level, it.name, it.count, it.first, it.detail
        );
    }
    if !res.pcr_samples.is_empty() {
        let max_oj = res
            .pcr_samples
            .iter()
            .map(|s| s.oj_ms.abs())
            .fold(0.0f64, f64::max);
        let max_iv = res
            .pcr_samples
            .iter()
            .map(|s| s.interval_ms)
            .fold(0.0f64, f64::max);
        println!(
            "PCR 抖动        : max|OJ|={max_oj:.3} ms, max interval={max_iv:.3} ms, CBR={}",
            res.pcr_cbr
        );
    }
    Ok(())
}
