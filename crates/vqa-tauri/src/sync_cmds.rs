//! 音画同步 / 一致性比对的命令层：起后台线程跑分析，前端按任务 id 轮询进度与结果。
//! 同一页可同时跑多路（并发度由前端控制），任务 id 由前端按并发槽位复用。

use crate::state::{AvState, ConsistState};
use std::sync::atomic::Ordering;
use tauri::State;
use vqa_core::avsync::{self, AvSyncCfg, AvSyncResult};
use vqa_core::consist::{self, ConsistCfg, ConsistResult};

/// 任务 id：前端不传时落到 main 槽；""/"all" 在停止命令里表示整页全部取消。
fn jid(job: Option<String>) -> String {
    job.filter(|s| !s.is_empty()).unwrap_or_else(|| "main".into())
}

const MAX_PARALLEL: usize = 8;

// ═════════ 音画同步 ═════════

#[tauri::command]
pub fn avsync_start(st: State<'_, AvState>, cfg: AvSyncCfg, job: Option<String>) -> Result<(), String> {
    if cfg.source.trim().is_empty() {
        return Err("来源为空".into());
    }
    let j = st.job(&jid(job));
    let (tx, rx) = std::sync::mpsc::channel::<AvSyncResult>();
    {
        // 占用检查与置位必须成对完成，否则并发起任务时会一起冲破并发上限
        let _gate = st.gate.lock().unwrap();
        if j.running.load(Ordering::Relaxed) {
            return Err("该任务已在运行".into());
        }
        if st.running_count() >= MAX_PARALLEL {
            return Err(format!("同时运行的检测任务最多 {MAX_PARALLEL} 路，请等部分完成"));
        }
        j.cancel.store(false, Ordering::Relaxed);
        j.pct.store(1, Ordering::Relaxed);
        j.running.store(true, Ordering::Relaxed);
        *j.rx.lock().unwrap() = Some(rx);
        *j.err.lock().unwrap() = None;
        *j.last.lock().unwrap() = None;
    }

    let pct = j.pct.clone();
    let cancel = j.cancel.clone();
    let running = j.running.clone();
    let errh = j.err.clone();
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut cb = |p: u8| pct.store(p, Ordering::Relaxed);
            avsync::analyze(&cfg, &mut cb, cancel)
        }));
        match res {
            Ok(r) => {
                let _ = tx.send(r);
            }
            Err(_) => {
                *errh.lock().unwrap() = Some("分析线程异常退出，请重试".into());
            }
        }
        running.store(false, Ordering::Relaxed);
    });
    Ok(())
}

#[derive(serde::Serialize)]
pub struct AvPoll {
    running: bool,
    pct: u8,
    result: Option<AvSyncResult>,
    err: Option<String>,
}

#[tauri::command]
pub fn avsync_progress(st: State<'_, AvState>, job: Option<String>) -> AvPoll {
    let j = st.jobs.lock().unwrap().get(&jid(job)).cloned();
    let Some(j) = j else {
        return AvPoll {
            running: false,
            pct: 0,
            result: None,
            err: None,
        };
    };
    if let Some(rx) = j.rx.lock().unwrap().take() {
        match rx.try_recv() {
            Ok(r) => {
                if !r.ok {
                    *j.err.lock().unwrap() = r.err.clone();
                }
                *j.last.lock().unwrap() = Some(r);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                *j.rx.lock().unwrap() = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }
    let result = j.last.lock().unwrap().clone();
    let err = j.err.lock().unwrap().clone();
    AvPoll {
        running: j.running.load(Ordering::Relaxed),
        pct: j.pct.load(Ordering::Relaxed),
        result,
        err,
    }
}

/// job 为空或 "all" 时取消该页全部任务。
#[tauri::command]
pub fn avsync_stop(st: State<'_, AvState>, job: Option<String>) {
    let all = job.as_deref().map(|s| s.is_empty() || s == "all").unwrap_or(true);
    let jobs = st.jobs.lock().unwrap();
    if all {
        for j in jobs.values() {
            j.cancel.store(true, Ordering::Relaxed);
        }
    } else if let Some(j) = job.as_deref().and_then(|id| jobs.get(id)) {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

// ═════════ 一致性比对 ═════════

#[tauri::command]
pub fn consist_start(st: State<'_, ConsistState>, cfg: ConsistCfg, job: Option<String>) -> Result<(), String> {
    if cfg.reference.trim().is_empty() || cfg.target.trim().is_empty() {
        return Err("参考源与待测源都要填".into());
    }
    let j = st.job(&jid(job));
    let (tx, rx) = std::sync::mpsc::channel::<ConsistResult>();
    {
        let _gate = st.gate.lock().unwrap();
        if j.running.load(Ordering::Relaxed) {
            return Err("该任务已在运行".into());
        }
        if st.running_count() >= MAX_PARALLEL {
            return Err(format!("同时运行的检测任务最多 {MAX_PARALLEL} 路，请等部分完成"));
        }
        j.cancel.store(false, Ordering::Relaxed);
        j.pct.store(1, Ordering::Relaxed);
        j.running.store(true, Ordering::Relaxed);
        *j.rx.lock().unwrap() = Some(rx);
        *j.err.lock().unwrap() = None;
        *j.last.lock().unwrap() = None;
    }

    let pct = j.pct.clone();
    let cancel = j.cancel.clone();
    let running = j.running.clone();
    let errh = j.err.clone();
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut cb = |p: u8| pct.store(p, Ordering::Relaxed);
            consist::compare(&cfg, &mut cb, cancel)
        }));
        match res {
            Ok(r) => {
                let _ = tx.send(r);
            }
            Err(_) => {
                *errh.lock().unwrap() = Some("比对线程异常退出，请重试".into());
            }
        }
        running.store(false, Ordering::Relaxed);
    });
    Ok(())
}

#[derive(serde::Serialize)]
pub struct ConsistPoll {
    running: bool,
    pct: u8,
    result: Option<ConsistResult>,
    err: Option<String>,
}

#[tauri::command]
pub fn consist_progress(st: State<'_, ConsistState>, job: Option<String>) -> ConsistPoll {
    let j = st.jobs.lock().unwrap().get(&jid(job)).cloned();
    let Some(j) = j else {
        return ConsistPoll {
            running: false,
            pct: 0,
            result: None,
            err: None,
        };
    };
    if let Some(rx) = j.rx.lock().unwrap().take() {
        match rx.try_recv() {
            Ok(r) => {
                if !r.ok {
                    *j.err.lock().unwrap() = r.err.clone();
                }
                *j.last.lock().unwrap() = Some(r);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                *j.rx.lock().unwrap() = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }
    let result = j.last.lock().unwrap().clone();
    let err = j.err.lock().unwrap().clone();
    ConsistPoll {
        running: j.running.load(Ordering::Relaxed),
        pct: j.pct.load(Ordering::Relaxed),
        result,
        err,
    }
}

#[tauri::command]
pub fn consist_stop(st: State<'_, ConsistState>, job: Option<String>) {
    let all = job.as_deref().map(|s| s.is_empty() || s == "all").unwrap_or(true);
    let jobs = st.jobs.lock().unwrap();
    if all {
        for j in jobs.values() {
            j.cancel.store(true, Ordering::Relaxed);
        }
    } else if let Some(j) = job.as_deref().and_then(|id| jobs.get(id)) {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

// ═════════ 频道列表（两个检测页共用一份，存 shared 配置） ═════════

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Channel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default = "ch_default_on")]
    pub enabled: bool,
}

fn ch_default_on() -> bool {
    true
}

#[tauri::command]
pub fn channels_get() -> Vec<Channel> {
    vqa_core::config::shared_get_or("channels", Vec::new())
}

#[tauri::command]
pub fn channels_save(list: Vec<Channel>) -> Result<(), String> {
    vqa_core::config::shared_set("channels", serde_json::json!(list)).map_err(|e| e.to_string())
}

// ═════════ 页面设置（通用键值；每个功能各存各的） ═════════

#[tauri::command]
pub fn ui_pref_get(key: String) -> serde_json::Value {
    vqa_core::config::shared_get(&key).unwrap_or(serde_json::Value::Null)
}

#[tauri::command]
pub fn ui_pref_set(key: String, value: serde_json::Value) -> Result<(), String> {
    vqa_core::config::shared_set(&key, value).map_err(|e| e.to_string())
}

// ═════════ 检测画面预览 ═════════

#[tauri::command]
pub fn preview_start(st: tauri::State<'_, crate::preview::Preview>, owner: String, src: String) -> Result<(), String> {
    let slot = st.slot(&owner).ok_or_else(|| format!("未知预览槽位 {owner}"))?;
    slot.start(&src)
}

#[tauri::command]
pub fn preview_stop(st: tauri::State<'_, crate::preview::Preview>, owner: String) {
    if let Some(slot) = st.slot(&owner) {
        slot.stop();
    }
}

#[tauri::command]
pub fn preview_frame(st: tauri::State<'_, crate::preview::Preview>, owner: String) -> Option<String> {
    st.slot(&owner).and_then(|s| s.frame_data_url())
}
