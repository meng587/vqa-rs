//! 合规检测的 Tauri 命令层 —— 页面「合规检测」专用，不占播放器/分析状态。
//!
//! 两条路径：
//! - 离线：ffprobe 抓全量 JSON → `compliance::evaluate_probe` 11 项；
//! - 收流：`LiveSession` 收一路组播（`LiveCore.mdi` 逐包统计），快照时叠
//!   `compliance::evaluate_net` 的 4 项。节目技术质量项由后台线程另行 ffprobe
//!   探一次，探到后一并返回（组播 socket 带 SO_REUSEADDR，可与收流并存）。

use serde_json::json;
use tauri::State;
use vqa_core::compliance::{self, Item, Kind, NetStats, Summary};

use crate::state::ComplianceState;

/// 节目技术质量项（失败返回错误文本，页面照常显示网络项）。
fn probe_items(source: &str, kind: Kind) -> Result<Vec<Item>, String> {
    let info = crate::mediainfo::probe(source)?;
    let raw = info.get("raw").cloned().unwrap_or(json!({}));
    Ok(compliance::evaluate_probe(&raw, kind))
}

fn basis_list() -> Vec<serde_json::Value> {
    compliance::BASIS
        .iter()
        .map(|(code, name)| json!({ "code": code, "name": name }))
        .collect()
}

fn pack(items: &[Item], kind: Kind, source: &str, mode: &str) -> serde_json::Value {
    let summary: Summary = Summary::of(items);
    json!({
        "mode": mode,
        "source": source,
        "kind": kind.label(),
        "items": items,
        "summary": summary,
        "basis": basis_list(),
    })
}

fn report_dir() -> Option<std::path::PathBuf> {
    let dir = vqa_core::config::shared_get("analysis.report_dir")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    if dir.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(dir))
    }
}

/// 一次性检测：文件或 udp 地址。
#[tauri::command]
pub async fn compliance_check(
    path: String,
    kind: String,
) -> Result<serde_json::Value, String> {
    let cleaned = crate::clean_stream_url(&path);
    if cleaned.is_empty() {
        return Err("来源为空".into());
    }
    let k = Kind::from_str(&kind);
    let src = cleaned.clone();
    let (items, ev) = tauri::async_runtime::spawn_blocking(move || {
        let items = probe_items(&src, k)?;
        // 本地文件顺带做一遍 PID 层事件扫描（收流地址没有本地实体，跳过）
        let ev = if src.contains("://") {
            None
        } else {
            vqa_core::ts_events::scan(std::path::Path::new(&src)).ok()
        };
        Ok::<_, String>((items, ev))
    })
    .await
    .map_err(|e| format!("探测线程异常：{e}"))??;

    let mut all = items;
    if k == Kind::Iptv {
        // 离线快照拿不到收流统计：网络项如实标"未标注"，别冒充合格
        for mut it in compliance::evaluate_net(&NetStats::default()) {
            it.actual = "需实时收流".into();
            it.verdict = compliance::Verdict::Unknown;
            it.note = "离线文件无传输层数据，切到收流模式再测".into();
            all.push(it);
        }
    } else {
        all.extend(compliance::evaluate_catv_net(&NetStats::default(), 0));
    }
    all.extend(compliance::evaluate_events(k, ev.as_ref()));
    compliance::sort_items(&mut all);
    Ok(pack(&all, k, &cleaned, "offline"))
}

/// 起一路收流（表 2 的抖动/丢包/MDI 要连续收 ≥10s 才有意义）。
#[tauri::command]
pub async fn compliance_live_start(
    st: State<'_, ComplianceState>,
    url: String,
    nic_ip: String,
    kind: String,
) -> Result<(), String> {
    {
        let mut mode = st.mode.lock().unwrap();
        if *mode != 0 {
            return Ok(()); // 收流中忽略重复点击
        }
        *mode = 2;
    }
    let cleaned = crate::clean_stream_url(&url);
    let overrides = vqa_core::tsfile::TrOverrides::new();
    match vqa_core::live::LiveSession::start(&cleaned, &nic_ip, &overrides) {
        Ok(s) => {
            *st.live.lock().unwrap() = Some(s);
            *st.err.lock().unwrap() = None;
            *st.source.lock().unwrap() = (cleaned.clone(), kind.clone());
            *st.items.lock().unwrap() = None;
        }
        Err(e) => {
            *st.mode.lock().unwrap() = 0;
            let msg = format!("{e:#}");
            *st.err.lock().unwrap() = Some(msg.clone());
            return Err(msg);
        }
    }

    let src = cleaned.clone();
    let slot = st.items.clone();
    let pstate = st.probe_state.clone();
    let pk = Kind::from_str(&kind);
    if let Ok(mut g) = pstate.lock() {
        *g = "pending".into();
    }
    std::thread::Builder::new()
        .name("vqa-compliance-probe".into())
        .spawn(move || {
            let res = probe_items(&src, pk);
            if let Ok(mut g) = pstate.lock() {
                *g = match &res {
                    Ok(_) => "ok".to_string(),
                    Err(e) => format!("err:{e}"),
                };
            }
            if let Ok(items) = res {
                if let Ok(mut g) = slot.lock() {
                    *g = Some(items);
                }
            }
        })
        .ok();
    Ok(())
}

/// 收流快照：网络项实时 + 节目技术质量项（探到才给）。
#[tauri::command]
pub fn compliance_live_snapshot(
    st: State<'_, ComplianceState>,
    kind: String,
) -> serde_json::Value {
    let mode = *st.mode.lock().unwrap();
    let k = Kind::from_str(&kind);
    let (net, secs, alive) = {
        let live = st.live.lock().unwrap();
        match live.as_ref() {
            Some(s) => {
                let snap = s.with_core(|c| (c.mdi.snapshot(), c.mdi.elapsed().as_secs()));
                match snap {
                    Some((st2, t)) => (st2, t, !s.is_dead()),
                    None => (NetStats::default(), 0, false),
                }
            }
            None => (NetStats::default(), 0, false),
        }
    };

    let mut items: Vec<Item> = st.items.lock().unwrap().clone().unwrap_or_default();

    // 收流模式下直接探 URL 常常探不到（RTP 无 SDP），改用开头那段裸 TS 样本落盘再探
    if st.items.lock().unwrap().is_none() {
        let pstate0 = st.probe_state.lock().unwrap().clone();
        if pstate0 != "pending" && net.packets > 0 {
            let dump = {
                let live = st.live.lock().unwrap();
                live.as_ref().and_then(|s| s.take_dump(800_000))
            };
            if let Some(bytes) = dump {
                let path = std::env::temp_dir().join("vqa_probe_sample.ts");
                if std::fs::write(&path, &bytes).is_ok() {
                    if let Ok(mut g) = st.probe_state.lock() {
                        *g = "pending".into();
                    }
                    let slot = st.items.clone();
                    let pstate = st.probe_state.clone();
                    let ps = path.to_string_lossy().to_string();
                    std::thread::Builder::new()
                        .name("vqa-compliance-probe-dump".into())
                        .spawn(move || {
                            let res = probe_items(&ps, k);
                            if let Ok(mut g) = pstate.lock() {
                                *g = match &res {
                                    Ok(_) => "ok".to_string(),
                                    Err(e) => format!("err:{e}"),
                                };
                            }
                            if let Ok(it) = res {
                                if let Ok(mut g) = slot.lock() {
                                    *g = Some(it);
                                }
                            }
                            let _ = std::fs::remove_file(&ps);
                        })
                        .ok();
                }
            }
        }
    }
    if k == Kind::Iptv {
        items.extend(compliance::evaluate_net(&net));
    } else {
        items.extend(compliance::evaluate_catv_net(&net, secs));
    }
    // 收流模式的事件类统计还没接（离线文件走 PID 层实测，见 compliance_check）
    items.extend(compliance::evaluate_events(k, None));
    compliance::sort_items(&mut items);
    let summary = Summary::of(&items);
    let src = st.source.lock().unwrap().0.clone();
    let err = st.err.lock().unwrap().clone();
    let probe_ready = st.items.lock().unwrap().is_some();
    let probe_state = st.probe_state.lock().unwrap().clone();
    json!({
        "mode": if mode == 2 { "live" } else { "idle" },
        "source": src,
        "kind": k.label(),
        "secs": secs,
        "alive": alive,
        "stats": net,
        "probe_ready": probe_ready,
        "probe_state": probe_state,
        "items": items,
        "summary": summary,
        "err": err,
        "basis": basis_list(),
    })
}

#[tauri::command]
pub fn compliance_live_stop(st: State<'_, ComplianceState>) {
    if let Some(l) = st.live.lock().unwrap().take() {
        l.stop();
    }
    *st.mode.lock().unwrap() = 0;
}

/// 导出合规报告（Markdown），默认落到视频分析已配的报告目录。
#[tauri::command]
pub fn compliance_export_md(
    app: tauri::AppHandle,
    name: String,
    content: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dlg = app.dialog().file().set_file_name(&name);
    if let Some(dir) = report_dir() {
        dlg = dlg.set_directory(dir);
    }
    let Some(sel) = dlg.add_filter("Markdown", &["md"]).blocking_save_file() else {
        return Ok(None);
    };
    let path = sel.into_path().map_err(|e| format!("路径无效：{e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("写入失败：{e}"))?;
    Ok(Some(path.to_string_lossy().to_string()))
}
