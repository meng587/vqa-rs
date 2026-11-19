//! mpv IPC 冒烟测试（无 GUI）—— 起 mpv idle、连命名管道、发命令、收属性、退出。
//!
//! 用法：`cargo run -p vqa-core --example mpv_smoke -- [媒体文件]`

fn main() -> anyhow::Result<()> {
    let Some(mpv) = vqa_core::tools::find_mpv() else {
        anyhow::bail!("未找到 mpv.exe");
    };
    println!("mpv: {}", mpv.display());

    // 1) 启动（不带 wid，纯 idle）
    let m = vqa_core::player::Mpv::launch(&mpv, None)
        .map_err(|e| anyhow::anyhow!("启动失败: {e}"))?;
    println!("[1] mpv 已启动并连上 IPC");

    // 2) 命令往返
    let v = m
        .command(serde_json::json!(["get_property", "mpv-version"]))
        .map_err(|e| anyhow::anyhow!("get_property 失败: {e}"))?;
    let ver = v
        .get("data")
        .and_then(|d| d.as_str())
        .unwrap_or("?")
        .to_string();
    println!("[2] mpv-version = {ver}");

    // 3) 观察属性轮询（idle-active 应为 true）
    std::thread::sleep(std::time::Duration::from_millis(300));
    for _ in 0..10 {
        m.poll();
        let s = m.snapshot();
        if s.idle {
            println!("[3] idle-active = true ✓");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // 4) 可选：真播一个文件 2 秒
    let arg = std::env::args().nth(1);
    if let Some(path) = arg {
        println!("[4] 加载 {path}");
        m.loadfile(&path).map_err(|e| anyhow::anyhow!("loadfile: {e}"))?;
        let t0 = std::time::Instant::now();
        let mut played = false;
        while t0.elapsed() < std::time::Duration::from_secs(10) {
            m.poll();
            let s = m.snapshot();
            if !s.idle && s.time_ms > 500.0 {
                played = true;
                break;
            }
            if m.is_dead() {
                anyhow::bail!("mpv 中途退出");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let s = m.snapshot();
        println!(
            "[4] 播放 {} codec={} {}x{} time={:.1}s",
            if played { "✓" } else { "？（超时）" },
            if s.video_codec.is_empty() { "?" } else { &s.video_codec },
            s.width,
            s.height,
            s.time_ms / 1000.0
        );
        m.stop().ok();
    } else {
        println!("[4] 未给媒体文件，跳过真播（传文件路径可测）");
    }

    // 5) 退出
    drop(m);
    println!("[5] 已优雅退出");
    Ok(())
}
