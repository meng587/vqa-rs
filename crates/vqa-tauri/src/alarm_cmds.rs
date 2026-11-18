//! 告警处理页的后端：告警记录、证据录像、检测日志与存储设置。
//!
//! 记录落 `data/alarm/alarms.json`；日志和录像的目录由用户在告警页设置，
//! 默认优先 D 盘（D:\vqa-data），没有 D 盘时退到应用数据目录，避免新机器上
//! 因为盘符不存在而无法保存。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static IO_LOCK: Mutex<()> = Mutex::new(());

fn default_root() -> PathBuf {
    if Path::new("D:\\").exists() {
        PathBuf::from(r"D:\vqa-data")
    } else {
        vqa_core::config::base_dir().join("alarm")
    }
}

fn setting_dir(key: &str, sub: &str) -> PathBuf {
    let s: String = vqa_core::config::shared_get_or(key, String::new());
    let t = s.trim();
    if t.is_empty() {
        default_root().join(sub)
    } else {
        PathBuf::from(t)
    }
}

pub fn log_dir() -> PathBuf {
    setting_dir("alarm.log_dir", "logs")
}

pub fn clip_dir() -> PathBuf {
    setting_dir("alarm.clip_dir", "clips")
}

fn alarms_file() -> PathBuf {
    vqa_core::config::base_dir().join("alarm").join("alarms.json")
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AlarmHistory {
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub who: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AlarmSeg {
    #[serde(default)]
    pub from_s: f64,
    #[serde(default)]
    pub to_s: f64,
    #[serde(default)]
    pub sim: f64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AlarmRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub pair: String,
    #[serde(default)]
    pub group_id: String,
    /// 不一致 / 不同步 / 无信号
    #[serde(default)]
    pub kind: String,
    /// high / mid / low
    #[serde(default)]
    pub severity: String,
    /// pending / doing / done / false
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub similarity: f64,
    #[serde(default)]
    pub min_sim: f64,
    #[serde(default)]
    pub offset_ms: i32,
    #[serde(default)]
    pub segment_count: usize,
    #[serde(default)]
    pub first_time: String,
    #[serde(default)]
    pub last_time: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub detail: String,
    /// 比对时的采样帧率与逐帧相似度，供告警详情还原曲线
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub series_sim: Vec<f64>,
    #[serde(default)]
    pub bad_segments: Vec<AlarmSeg>,
    #[serde(default)]
    pub clip_path: String,
    #[serde(default)]
    pub clip_secs: u32,
    #[serde(default)]
    pub clip_size: u64,
    /// none / recording / ready / failed
    #[serde(default)]
    pub clip_state: String,
    #[serde(default)]
    pub assignee: String,
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub history: Vec<AlarmHistory>,
}

#[derive(Deserialize)]
pub struct AlarmInput {
    pub channel: String,
    pub pair: String,
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub severity: String,
    pub similarity: f64,
    pub min_sim: f64,
    pub offset_ms: i32,
    pub segment_count: usize,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub series_sim: Vec<f64>,
    #[serde(default)]
    pub bad_segments: Vec<AlarmSeg>,
}

fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn read_alarms() -> Vec<AlarmRecord> {
    fs::read_to_string(alarms_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<AlarmRecord>>(&s).ok())
        .unwrap_or_default()
}

fn write_alarms(list: &[AlarmRecord]) -> Result<(), String> {
    let p = alarms_file();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("建告警目录失败：{e}"))?;
    }
    let tmp = p.with_extension("json.tmp");
    let txt = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    fs::write(&tmp, txt).map_err(|e| format!("写告警记录失败：{e}"))?;
    fs::rename(&tmp, &p).map_err(|e| format!("落盘告警记录失败：{e}"))?;
    Ok(())
}

fn next_id(list: &[AlarmRecord]) -> String {
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let prefix = format!("A-{day}-");
    let n = list
        .iter()
        .filter(|a| a.id.starts_with(&prefix))
        .filter_map(|a| a.id.rsplit('-').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{:03}", n + 1)
}

/// 写一条检测日志（JSON）。路径按 日期/频道 归档。
fn write_detect_log(rec: &AlarmRecord) -> Option<String> {
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let safe = sanitize(&rec.channel);
    let stamp = chrono::Local::now().format("%H%M%S").to_string();
    let dir = log_dir().join(&day);
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let path = dir.join(format!("{safe}_{stamp}.json"));
    let body = serde_json::json!({
        "time": now_text(),
        "result": "abnormal",
        "alarm_id": rec.id,
        "channel": rec.channel,
        "pair": rec.pair,
        "similarity": rec.similarity,
        "min_sim": rec.min_sim,
        "offset_ms": rec.offset_ms,
        "segment_count": rec.segment_count,
        "detail": rec.detail,
        "reference": rec.reference,
        "source": rec.source
    });
    let txt = serde_json::to_string_pretty(&body).ok()?;
    fs::write(&path, txt).ok()?;
    Some(path.to_string_lossy().to_string())
}

/// 每次比对（无论通过还是异常）都写一条检测日志，便于按日期回溯。
#[tauri::command]
pub fn alarm_detect_log(
    channel: String,
    pair: String,
    similarity: f64,
    min_sim: f64,
    offset_ms: i32,
    segment_count: usize,
    verdict: String,
    source: String,
    reference: String,
    fps: f64,
    bad: bool,
) -> Result<(), String> {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let stamp = chrono::Local::now().format("%H%M%S%.3f").to_string().replace('.', "");
    let dir = log_dir().join(&day);
    fs::create_dir_all(&dir).map_err(|e| format!("建检测日志目录失败：{e}"))?;
    let path = dir.join(format!("{}_{}.json", sanitize(&channel), stamp));
    let body = serde_json::json!({
        "time": now_text(),
        "result": if bad { "abnormal" } else { "pass" },
        "channel": channel,
        "pair": pair,
        "similarity": similarity,
        "min_sim": min_sim,
        "offset_ms": offset_ms,
        "segment_count": segment_count,
        "verdict": verdict,
        "fps": fps,
        "reference": reference,
        "source": source
    });
    let txt = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    fs::write(&path, txt).map_err(|e| format!("写检测日志失败：{e}"))?;
    Ok(())
}

#[tauri::command]
pub fn alarm_list() -> Vec<AlarmRecord> {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = read_alarms();
    list.sort_by(|a, b| b.last_time.cmp(&a.last_time));
    list
}

/// 收到一次异常比对后落告警。10 分钟内同频道同来源对的活动告警合并，避免刷屏。
#[tauri::command]
pub fn alarm_record(input: AlarmInput) -> Result<AlarmRecord, String> {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = read_alarms();
    let now = now_text();
    let kind = if input.kind.trim().is_empty() {
        if input.segment_count > 0 { "不一致".to_string() } else { "不同步".to_string() }
    } else {
        input.kind.trim().to_string()
    };
    let severity = if input.severity.trim().is_empty() {
        if input.similarity < 0.6 || input.min_sim < 0.5 { "high" } else { "mid" }.to_string()
    } else {
        input.severity.trim().to_string()
    };

    let existing = list.iter().position(|a| {
        a.status != "done"
            && a.status != "false"
            && a.channel == input.channel
            && a.pair == input.pair
            && !a.last_time.is_empty()
            && chrono::NaiveDateTime::parse_from_str(&a.last_time, "%Y-%m-%d %H:%M:%S")
                .map(|t| (chrono::Local::now().naive_local() - t).num_seconds() < 600)
                .unwrap_or(false)
    });

    let idx = if let Some(i) = existing {
        let r = &mut list[i];
        r.last_time = now.clone();
        r.similarity = input.similarity;
        r.min_sim = input.min_sim;
        r.offset_ms = input.offset_ms;
        r.segment_count = input.segment_count;
        r.detail = input.detail.clone();
        r.fps = input.fps;
        r.series_sim = input.series_sim.clone();
        r.bad_segments = input.bad_segments.clone();
        r.history.push(AlarmHistory {
            time: now.clone(),
            action: "检测再次命中，合并到本条告警".into(),
            who: "系统".into(),
        });
        i
    } else {
        let r = AlarmRecord {
            id: next_id(&list),
            channel: input.channel.clone(),
            pair: input.pair.clone(),
            group_id: input.group_id.clone(),
            kind,
            severity,
            status: "pending".into(),
            similarity: input.similarity,
            min_sim: input.min_sim,
            offset_ms: input.offset_ms,
            segment_count: input.segment_count,
            first_time: now.clone(),
            last_time: now.clone(),
            source: input.source.clone(),
            reference: input.reference.clone(),
            detail: input.detail.clone(),
            fps: input.fps,
            series_sim: input.series_sim.clone(),
            bad_segments: input.bad_segments.clone(),
            clip_state: "none".into(),
            history: vec![AlarmHistory {
                time: now.clone(),
                action: "自动产生".into(),
                who: "系统".into(),
            }],
            ..Default::default()
        };
        list.push(r);
        list.len() - 1
    };

    if let Some(log_path) = write_detect_log(&list[idx]) {
        list[idx].history.push(AlarmHistory {
            time: now.clone(),
            action: format!("日志：{log_path}"),
            who: "系统".into(),
        });
    }
    let rec = list[idx].clone();
    write_alarms(&list)?;
    Ok(rec)
}

#[tauri::command]
pub fn alarm_update(
    id: String,
    status: String,
    conclusion: String,
    assignee: String,
    note: String,
) -> Result<AlarmRecord, String> {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = read_alarms();
    let Some(r) = list.iter_mut().find(|a| a.id == id) else {
        return Err(format!("告警 {id} 不存在"));
    };
    if !status.trim().is_empty() {
        r.status = status.trim().to_string();
    }
    if !conclusion.trim().is_empty() {
        r.conclusion = conclusion.trim().to_string();
    }
    if !assignee.trim().is_empty() {
        r.assignee = assignee.trim().to_string();
    }
    if !note.trim().is_empty() {
        r.note = note.trim().to_string();
    }
    r.history.push(AlarmHistory {
        time: now_text(),
        action: format!(
            "人工处理：{} / {}",
            if r.conclusion.is_empty() { "未填结论" } else { &r.conclusion },
            if r.status.is_empty() { "状态未变" } else { &r.status }
        ),
        who: if r.assignee.is_empty() { "本机用户".into() } else { r.assignee.clone() },
    });
    let out = r.clone();
    write_alarms(&list)?;
    Ok(out)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_whitespace() => '_',
            _ => c,
        })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(40)
        .collect()
}

/// 告警触发后录一段证据。文件为 TS（对 H.264/AVC/AVS 等都兼容），
/// 录完回填路径、时长和大小；录制本身在后台线程，不挡前端。
#[tauri::command]
pub fn alarm_capture(
    record_id: String,
    source: String,
    seconds: u32,
    start_s: Option<f64>,
) -> Result<(), String> {
    let src = source.trim().to_string();
    if src.is_empty() {
        return Err("证据录像来源为空".into());
    }
    let secs = seconds.clamp(4, 120);
    {
        let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut list = read_alarms();
        let Some(r) = list.iter_mut().find(|a| a.id == record_id) else {
            return Err(format!("告警 {record_id} 不存在"));
        };
        r.clip_state = "recording".into();
        write_alarms(&list)?;
    }

    std::thread::spawn(move || {
        let res = capture_worker(&record_id, &src, secs, start_s);
        let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut list = read_alarms();
        if let Some(r) = list.iter_mut().find(|a| a.id == record_id) {
            match res {
                Ok((path, size)) => {
                    r.clip_path = path;
                    r.clip_secs = secs;
                    r.clip_size = size;
                    r.clip_state = "ready".into();
                    r.history.push(AlarmHistory {
                        time: now_text(),
                        action: "证据录像已保存".into(),
                        who: "系统".into(),
                    });
                }
                Err(e) => {
                    r.clip_state = "failed".into();
                    r.history.push(AlarmHistory {
                        time: now_text(),
                        action: format!("证据录像失败：{e}"),
                        who: "系统".into(),
                    });
                }
            }
            let _ = write_alarms(&list);
        }
    });
    Ok(())
}

fn capture_worker(
    record_id: &str,
    source: &str,
    seconds: u32,
    start_s: Option<f64>,
) -> Result<(String, u64), String> {
    let ff = vqa_core::tools::find_ffmpeg().ok_or_else(|| "未找到 ffmpeg".to_string())?;
    let channel = {
        let list = read_alarms();
        list.iter()
            .find(|a| a.id == record_id)
            .map(|a| a.channel.clone())
            .unwrap_or_else(|| "告警".into())
    };
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let dir = clip_dir().join(&day);
    fs::create_dir_all(&dir).map_err(|e| format!("建录像目录失败：{e}"))?;
    let stamp = chrono::Local::now().format("%H%M%S").to_string();
    let out = dir.join(format!("{}_{}.ts", sanitize(&channel), stamp));

    let mut cmd = vqa_core::ffmpeg::hidden(&ff);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    let is_file = Path::new(source).exists();
    if is_file {
        let ss = start_s.unwrap_or(0.0).max(0.0);
        if ss > 0.0 {
            cmd.args(["-ss", &format!("{ss:.2}")]);
        }
    }
    cmd.args(["-i", source, "-t", &seconds.to_string(), "-c", "copy", "-y"])
        .arg(&out);
    let st = cmd.status().map_err(|e| format!("启动 ffmpeg 失败：{e}"))?;
    if !st.success() {
        let _ = fs::remove_file(&out);
        return Err(format!("ffmpeg 退出码 {}", st.code().unwrap_or(-1)));
    }
    let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        let _ = fs::remove_file(&out);
        return Err("录像为空".into());
    }
    Ok((out.to_string_lossy().to_string(), size))
}

#[tauri::command]
pub fn alarm_open(path: String) -> Result<(), String> {
    let p = PathBuf::from(path.trim());
    if !p.exists() {
        return Err("文件不存在".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", &p.to_string_lossy()]);
        c.creation_flags(0x0800_0000);
        c.spawn().map_err(|e| format!("打开失败：{e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err("当前平台暂不支持直接打开".into())
    }
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn clean_dir(p: &Path, days: u32, max_bytes: u64) -> u32 {
    let mut removed = 0u32;
    let cutoff = chrono::Local::now()
        .checked_sub_signed(chrono::Duration::days(days as i64))
        .map(|t| t.timestamp())
        .unwrap_or(0);
    let mut files: Vec<(PathBuf, i64, u64)> = Vec::new();
    fn walk(p: &Path, out: &mut Vec<(PathBuf, i64, u64)>) {
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(md) = e.metadata() {
                    let ts = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    out.push((path, ts, md.len()));
                }
            }
        }
    }
    walk(p, &mut files);
    files.retain(|(path, ts, _)| {
        if days > 0 && *ts < cutoff {
            let _ = fs::remove_file(path);
            removed += 1;
            false
        } else {
            true
        }
    });
    let mut total: u64 = files.iter().map(|(_, _, s)| *s).sum();
    files.sort_by_key(|(_, ts, _)| *ts);
    for (path, _, size) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
            removed += 1;
        }
    }
    removed
}

/// 导出告警记录（JSON），弹保存对话框。
#[tauri::command]
pub fn alarm_export(app: tauri::AppHandle, content: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let name = format!(
        "告警记录_{}.json",
        chrono::Local::now().format("%Y%m%d_%H%M")
    );
    let sel = app
        .dialog()
        .file()
        .add_filter("JSON", &["json"])
        .set_file_name(&name)
        .blocking_save_file()?;
    let path = sel.into_path().ok()?;
    fs::write(&path, content).ok()?;
    Some(path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn alarm_settings_get() -> serde_json::Value {
    let days: u32 = vqa_core::config::shared_get_or("alarm.retain_days", 90);
    let max_gb: f64 = vqa_core::config::shared_get_or("alarm.max_gb", 50.0);
    let auto: bool = vqa_core::config::shared_get_or("alarm.auto_clean", true);
    let logs = log_dir();
    let clips = clip_dir();
    let log_size = dir_size(&logs);
    let clip_size = dir_size(&clips);
    serde_json::json!({
        "log_dir": logs.to_string_lossy(),
        "clip_dir": clips.to_string_lossy(),
        "retain_days": days,
        "max_gb": max_gb,
        "auto_clean": auto,
        "log_size": log_size,
        "clip_size": clip_size,
        "default_log_dir": default_root().join("logs").to_string_lossy(),
        "default_clip_dir": default_root().join("clips").to_string_lossy()
    })
}

fn check_dir(label: &str, raw: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(raw.trim());
    if !p.is_absolute() {
        return Err(format!("{label}要填绝对路径：{raw}"));
    }
    fs::create_dir_all(&p).map_err(|e| format!("{label}建不出来 {}：{e}", p.display()))?;
    let probe = p.join(".vqa_write_test");
    fs::write(&probe, b"ok").map_err(|e| format!("{label}不可写 {}：{e}", p.display()))?;
    let _ = fs::remove_file(&probe);
    Ok(p)
}

#[tauri::command]
pub fn alarm_settings_set(
    log_dir: String,
    clip_dir: String,
    retain_days: u32,
    max_gb: f64,
    auto_clean: bool,
) -> Result<serde_json::Value, String> {
    let logs = check_dir("检测日志目录", &log_dir)?;
    let clips = check_dir("故障视频目录", &clip_dir)?;
    vqa_core::config::shared_set("alarm.log_dir", serde_json::json!(logs.to_string_lossy()))
        .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set("alarm.clip_dir", serde_json::json!(clips.to_string_lossy()))
        .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set("alarm.retain_days", serde_json::json!(retain_days.max(1)))
        .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set("alarm.max_gb", serde_json::json!(max_gb.max(1.0)))
        .map_err(|e| format!("写配置失败：{e}"))?;
    vqa_core::config::shared_set("alarm.auto_clean", serde_json::json!(auto_clean))
        .map_err(|e| format!("写配置失败：{e}"))?;
    if auto_clean {
        clean_dir(&clips, retain_days.max(1), (max_gb.max(1.0) * 1024.0 * 1024.0 * 1024.0) as u64);
        clean_dir(&logs, retain_days.max(1), (max_gb.max(1.0) * 1024.0 * 1024.0 * 1024.0) as u64);
    }
    Ok(alarm_settings_get())
}




