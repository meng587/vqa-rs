//! 诊断日志 —— 收集/导出/上传（Gitee Issue）。
//!
//! 命名：`log_{MAC前8位}_{yyyymmdd_HHMMSS}.txt`（MAC 区分设备）。
//! 内容力求一份可定位：环境元信息 + 播放器/分析页状态 + 应用日志尾部
//! + 崩溃文件全文。上传走 Gitee Issue API（token 由用户粘贴一次，
//! 存本地 shared config，不进代码库）。

use serde_json::json;

/// MAC（去分隔符 12 位十六进制，取第一条物理网卡）。
fn device_mac() -> String {
    for row in vqa_core::netif::interface_rows() {
        if !row.mac.is_empty() && row.mac != "00-00-00-00-00-00" {
            return row.mac.replace(['-', ':'], "").to_uppercase();
        }
    }
    "UNKNOWN".into()
}

fn os_version() -> String {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        let ctor: fn(std::ffi::OsString) -> Command = Command::new;
        let mut cmd = ctor("cmd".into());
        cmd.arg("/c").arg("ver");
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd.output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "Windows".into())
    }
    #[cfg(not(windows))]
    {
        std::env::consts::OS.into()
    }
}

/// 组装诊断日志 → (文件名, 内容)。
pub fn collect(
    player: &serde_json::Value,
    analysis_mode: u8,
    analysis_err: Option<String>,
    analysis_summary: &str,
) -> (String, String) {
    let mac = device_mac();
    let ts = chrono::Local::now();
    let name = format!("log_{}_{}.txt", &mac[..mac.len().min(8)], ts.format("%Y%m%d_%H%M%S"));

    // ---- 应用日志尾部 200 行 ----
    let app_log = {
        let dir = vqa_core::config::log_dir();
        let f = dir.join("vqa.log");
        std::fs::read_to_string(&f)
            .map(|s| {
                let lines: Vec<&str> = s.lines().collect();
                let start = lines.len().saturating_sub(200);
                lines[start..].join("\n")
            })
            .unwrap_or_else(|_| "（无 vqa.log）".into())
    };

    // ---- 崩溃文件 ----
    let crashes = {
        let dir = vqa_core::config::crash_dir();
        let mut out = String::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "log").unwrap_or(false))
                .collect();
            files.sort();
            for p in files.iter().rev().take(5) {
                out.push_str(&format!("--- {} ---\n", p.file_name().unwrap_or_default().to_string_lossy()));
                out.push_str(&std::fs::read_to_string(p).unwrap_or_else(|_| "（读取失败）".into()));
                out.push('\n');
            }
        }
        if out.is_empty() {
            "（无崩溃记录）".to_string()
        } else {
            out
        }
    };

    let uptime = APP_START
        .get()
        .map(|t| format!("{:.0} 分钟", t.elapsed().as_secs_f64() / 60.0))
        .unwrap_or_else(|| "?".into());

    // ---- mpv 自带日志尾部 50 行（--log-file 落在 scratch）----
    let mpv_tail = {
        let f = vqa_core::config::scratch_dir().join("mpv.log");
        match std::fs::read_to_string(&f) {
            Ok(s) => {
                let lines: Vec<&str> = s.lines().collect();
                let start = lines.len().saturating_sub(50);
                lines[start..].join("\n")
            }
            Err(_) => "（无 mpv.log）".into(),
        }
    };

    let content = format!(
        "===== VQA 诊断日志 =====\n\
         生成时间   : {}\n\
         应用版本   : {} (Tauri/Web UI)\n\
         核心库     : vqa-core {}\n\
         操作系统   : {}\n\
         主机名     : {} ({})\n\
         设备MAC    : {}\n\
         运行时长   : {}\n\
         \n===== 播放器状态 =====\n{}\n\
         \n===== 分析页状态 =====\n模式: {}  错误: {}\n\
         \n===== 分析结果摘要 =====\n{}\n\
         \n===== 事件时间线（最近 50 条）=====\n{}\n\
         \n===== 播放采样（每 5s 一条，最近 60 条）=====\n{}\n\
         \n===== mpv 日志（尾部 50 行）=====\n{}\n\
         \n===== 应用日志（尾部 200 行）=====\n{}\n\
         \n===== 崩溃记录 =====\n{}\n\
         ===== 结束 =====\n",
        ts.format("%Y-%m-%d %H:%M:%S"),
        env!("CARGO_PKG_VERSION"),
        "TS 解析 / TR101 290 / PCR 双环 PLL",
        os_version(),
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "?".into()),
        std::env::consts::ARCH,
        mac,
        uptime,
        serde_json::to_string_pretty(player).unwrap_or_default(),
        analysis_mode,
        analysis_err.as_deref().unwrap_or("无"),
        analysis_summary,
        crate::events::events_dump(),
        crate::events::samples_dump(),
        mpv_tail,
        app_log,
        crashes,
    );
    (name, content)
}

pub static APP_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// 随安装包下发的上传凭据（`bin/diag_secret.json`，内容 `{"token":"..."}`）。
///
/// 打包时由 `tools/release.ps1` 从本机 `%APPDATA%\vqa\shared\settings.json` 的
/// `diag_token` 生成，文件名匹配 `.gitignore` 的 `secret*.json`，不进代码库。
/// 目的是让「装完即能上传」：新机器没有本机保存的凭据时用它兜底。
pub fn bundled_token() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for p in [
        dir.join("bin").join("diag_secret.json"),
        dir.join("diag_secret.json"),
    ] {
        if let Ok(txt) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                if let Some(t) = v.get("token").and_then(|t| t.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 实际可用的上传凭据：本机已保存 → 环境变量 `VQA_DIAG_TOKEN` → 安装包自带。
pub fn resolve_token() -> Option<String> {
    let saved: String = vqa_core::config::shared_get_or("diag_token", String::new());
    let saved = saved.trim().to_string();
    if !saved.is_empty() {
        return Some(saved);
    }
    if let Ok(t) = std::env::var("VQA_DIAG_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    bundled_token()
}

/// 上传到仓库 `logs/` 目录（Gitee Contents API；token 存 shared config `diag_token`）。
/// 日志文件直接进仓库，维护者 git pull 即可阅读分析。
pub fn upload_issue(token: &str, name: &str, content: &str) -> Result<String, String> {
    let url = format!(
        "https://gitee.com/api/v5/repos/meng587/vqa-rs/contents/logs/{name}"
    );
    // 手写 base64（标准字母表），避免引入新依赖
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = content.as_bytes();
    let mut b64 = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        b64.push(TBL[(n >> 18) as usize & 63] as char);
        b64.push(TBL[(n >> 12) as usize & 63] as char);
        b64.push(if chunk.len() > 1 { TBL[(n >> 6) as usize & 63] as char } else { '=' });
        b64.push(if chunk.len() > 2 { TBL[n as usize & 63] as char } else { '=' });
    }
    let payload = json!({
        "access_token": token,
        "content": b64,
        "message": format!("诊断日志 {name}（应用内上传）"),
        "branch": "master",
    });
    let ctor: fn(std::ffi::OsString) -> std::process::Command = std::process::Command::new;
    // 不引新依赖：Gitee Contents API 用 JSON POST，走系统自带 curl（Windows 10+ 自带）。
    // payload 走 stdin（@file 方式在 System32 curl 下读 C:\ 反斜杠路径会报错 26，实测）
    let body = payload.to_string();
    let mut curl_cmd = ctor("curl".into());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        curl_cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    curl_cmd
        .args(["-sS", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-"])
        .arg(&url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = curl_cmd
        .spawn()
        .map_err(|e| format!("启动 curl 失败：{e}"))?;
    use std::io::Write as _;
    child
        .stdin
        .take()
        .ok_or("curl stdin 不可用")?
        .write_all(body.as_bytes())
        .map_err(|e| format!("写入 curl stdin 失败：{e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("等待 curl 失败：{e}"))?;
    eprintln!(
        "[diag] curl exit={:?} url={url} stdout_len={} stderr={}",
        out.status.code(),
        out.stdout.len(),
        String::from_utf8_lossy(&out.stderr)
    );
    let resp = String::from_utf8_lossy(&out.stdout).to_string();
    if resp.contains("html_url") {
        let v: serde_json::Value = serde_json::from_str(&resp).map_err(|e| format!("响应解析失败：{e}"))?;
        Ok(v["content"]["html_url"].as_str().unwrap_or("(已上传)").to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "上传失败：{}（curl 退出码 {}，payload {} 字节）",
            &resp[..resp.len().min(300)],
            out.status.code().unwrap_or(-1),
            payload.to_string().len()
        ))
        .map_err(|e| {
            if stderr.trim().is_empty() { e } else { format!("{e}；curl stderr: {}", stderr.trim()) }
        })
    }
}
