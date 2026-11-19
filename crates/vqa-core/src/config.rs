//! 数据目录 / 配置 / 日志 —— `vkitcore/config.py` 的移植。
//!
//! 目录解析顺序（与 Python 版一致）：
//! 1. 程序目录旁的指针文件 `data_dir.txt`（首行=目标目录）
//! 2. 环境变量 `VQA_HOME`（Python 版叫 VKIT_HOME）
//! 3. `%APPDATA%\vqa`
//!
//! 约定：`base_dir()` 只在首次调用时读指针文件，此后进程内缓存；
//! `reset()` 供测试使用。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 程序自身所在目录（exe 旁；开发期=当前 exe 所在）。
pub fn program_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn pointer_file() -> PathBuf {
    program_dir().join("data_dir.txt")
}

/// 读取指针文件内容（首行）。没有则 None。
pub fn read_pointer() -> Option<PathBuf> {
    let txt = fs::read_to_string(pointer_file()).ok()?;
    let line = txt.lines().next()?.trim().to_string();
    if line.is_empty() {
        return None;
    }
    Some(PathBuf::from(line))
}

/// 写指针文件（原子：tmp + rename）。返回 (是否成功, 消息)。
pub fn write_pointer(path: &Path) -> (bool, String) {
    let pf = pointer_file();
    let tmp = pf.with_extension("txt.tmp");
    let res = fs::write(&tmp, path.to_string_lossy().as_bytes())
        .and_then(|_| fs::rename(&tmp, &pf));
    match res {
        Ok(_) => (true, format!("已写入 {}", pf.display())),
        Err(e) => (false, format!("写入失败: {e}")),
    }
}

/// 解析数据根目录（带缓存）。顺序：指针文件 → `VQA_HOME` → `%APPDATA%\vqa`。
pub fn base_dir() -> PathBuf {
    let cached = ROOT.get_or_init(|| {
        if let Some(p) = read_pointer() {
            return Some(p);
        }
        if let Ok(p) = std::env::var("VQA_HOME") {
            if !p.trim().is_empty() {
                return Some(PathBuf::from(p.trim()));
            }
        }
        #[allow(deprecated)]
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        appdata.map(|d| d.join("vqa"))
    });
    cached.clone().unwrap_or_else(|| PathBuf::from("."))
}

/// 工具级子目录，如 `base/tools/player`。
pub fn tool_dir(tool: &str) -> PathBuf {
    base_dir().join("tools").join(tool)
}

/// 共享配置目录（跨页面公共项）。
pub fn shared_dir() -> PathBuf {
    base_dir().join("shared")
}

/// 日志目录。
pub fn log_dir() -> PathBuf {
    base_dir().join("logs")
}

/// 崩溃转储目录。
pub fn crash_dir() -> PathBuf {
    base_dir().join("crash")
}

/// 转码临时目录（分片 / 中间件 / 未完成的桥输出）。
///
/// 落数据目录下的 `scratch`，跟着数据目录走（`data_dir.txt` / `VQA_HOME` 迁到
/// 大容量盘时一并迁移），不再默认吃系统盘。建不出来才退回系统临时目录——
/// 后果是回到历史行为（占 C 盘），但不会让转码直接失败。
pub fn scratch_dir() -> PathBuf {
    let p = base_dir().join("scratch");
    if fs::create_dir_all(&p).is_ok() {
        p
    } else {
        std::env::temp_dir()
    }
}

/// 工具配置文件路径 `base/tools/<tool>/config.json`。
pub fn config_path(tool: &str) -> PathBuf {
    tool_dir(tool).join("config.json")
}

/// 读取工具配置；文件不存在/损坏时返回 `defaults`（损坏会记一条日志）。
pub fn load_config(tool: &str, defaults: serde_json::Value) -> serde_json::Value {
    let p = config_path(tool);
    match fs::read_to_string(&p) {
        Ok(txt) => match serde_json::from_str(&txt) {
            Ok(v) => v,
            Err(e) => {
                log_line("config", &format!("配置解析失败 {}: {e}", p.display()));
                defaults
            }
        },
        Err(_) => defaults,
    }
}

/// 保存工具配置（原子：tmp + rename）。
pub fn save_config(tool: &str, data: &serde_json::Value) -> Result<()> {
    let p = config_path(tool);
    fs::create_dir_all(p.parent().context("config path has no parent")?)?;
    let tmp = p.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(data)?.as_bytes())?;
    }
    fs::rename(&tmp, &p)?;
    Ok(())
}

fn shared_file() -> PathBuf {
    shared_dir().join("settings.json")
}

/// 读共享配置键。缺失返回 None。
pub fn shared_get(key: &str) -> Option<serde_json::Value> {
    let txt = fs::read_to_string(shared_file()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    v.get(key).cloned()
}

/// 读共享配置键，缺失/类型不符返回默认值。
pub fn shared_get_or<T: serde::de::DeserializeOwned>(key: &str, default: T) -> T {
    shared_get(key)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or(default)
}

/// 写共享配置键（读-改-写）。多个页面/命令可能同时写，串行化避免互相覆盖掉键。
pub fn shared_set(key: &str, value: serde_json::Value) -> Result<()> {
    static SHARED_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SHARED_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let p = shared_file();
    fs::create_dir_all(p.parent().context("shared path has no parent")?)?;
    let mut root: serde_json::Value = match fs::read_to_string(&p) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    root[key] = value;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&root)?)?;
    fs::rename(&tmp, &p)?;
    Ok(())
}

/// 清理日志：删除超过 `max_age_days` 天或整体超过 `max_size_mb` 的最旧文件。
/// 返回删除数。与 Python `cleanup_logs` 同口径：先按时间删，再按总量删最旧。
pub fn cleanup_logs(logs: &Path, max_age_days: u32, max_size_mb: u64) -> usize {
    let mut removed = 0;
    let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = match fs::read_dir(logs) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                Some((e.path(), md.modified().ok()?, md.len()))
            })
            .collect(),
        Err(_) => return 0,
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            max_age_days as u64 * 86_400,
        ))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    files.retain(|(p, t, _)| {
        if *t < cutoff {
            if fs::remove_file(p).is_ok() {
                removed += 1;
            }
            return false;
        }
        true
    });
    let cap = max_size_mb.saturating_mul(1024 * 1024);
    let mut total: u64 = files.iter().map(|(_, _, s)| s).sum();
    files.sort_by_key(|(_, t, _)| *t);
    for (p, _, s) in files {
        if total <= cap {
            break;
        }
        if fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(s);
            removed += 1;
        }
    }
    removed
}

/// 追加一行到 `logs/vqa.log`（带时间戳；失败静默——日志不能挡业务）。
pub fn log_line(module: &str, msg: &str) {
    let d = log_dir();
    if fs::create_dir_all(&d).is_err() {
        return;
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(d.join("vqa.log"))
    {
        let _ = writeln!(f, "[{ts}] [{module}] {msg}");
    }
}

/// 供测试重置目录缓存。
pub fn reset_for_tests() {
    // OnceLock 无法清空；测试里用进程内一次性调用约定。
    // 保留占位以维持 API 对称性（Python 版 reset() 同样仅测试用）。
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_config_roundtrip() {
        // 用临时 VQA_HOME，避免污染真实 %APPDATA%。
        let tmp = std::env::temp_dir().join(format!("vqa-test-{}", std::process::id()));
        std::env::set_var("VQA_HOME", &tmp);
        let _ = ROOT.get_or_init(|| Some(tmp.clone())); // 保险：已被初始化也无妨

        shared_set("k", serde_json::json!("v1")).unwrap();
        assert_eq!(shared_get("k"), Some(serde_json::json!("v1")));
        shared_set("k", serde_json::json!(42)).unwrap();
        assert_eq!(shared_get("k"), Some(serde_json::json!(42)));
        assert_eq!(shared_get_or::<i64>("missing", -1), -1);
        // 类型不符（数字 → String）时取默认值
        assert_eq!(shared_get_or::<String>("k", "d".into()), "d");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_save_load() {
        let tmp = std::env::temp_dir().join(format!("vqa-test-cfg-{}", std::process::id()));
        std::env::set_var("VQA_HOME", &tmp);
        save_config(
            "player",
            &serde_json::json!({"volume": 80}),
        )
        .unwrap();
        let v = load_config("player", serde_json::json!({"volume": 100}));
        assert_eq!(v["volume"], 80);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
