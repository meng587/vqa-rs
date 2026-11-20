//! 播放器「多路同看」命令层。
//!
//! 多路同看不复用单路播放器的 `PlayerState`：每一格独占一个 mpv 进程和一个
//! WS_CHILD 视频子窗。这样切路、静音、放大都不会影响单路播放链路，反之亦然。
//!
//! 音频策略：所有格启动时先静音，`multi_select` 只给选中格解除静音——保证
//! 同时最多只有一路出声。画面位置由前端按 `getBoundingClientRect × dpr`
//! 算出物理像素后调用 `multi_place` 对齐（坐标系与单路 `place_video` 一致）。

use crate::state::MultiState;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use vqa_core::player::Mpv;

#[derive(Debug, Clone, Serialize)]
pub struct MultiSlotView {
    pub index: usize,
    pub url: String,
    pub status: String,
    pub error: Option<String>,
    pub playing: bool,
    pub paused: bool,
    pub idle: bool,
    pub width: i64,
    pub height: i64,
    pub video_codec: String,
    pub audio_codec: String,
    pub volume: f64,
    pub mute: bool,
    pub avsync: f64,
    pub v_bitrate: f64,
    pub time_ms: f64,
    pub duration_ms: f64,
    /// 本轮回传前，该格收到过一次单击（前端据此切声）。
    pub click: bool,
    /// 本轮回传前，该格收到过一次双击（前端据此放大）。
    pub dblclick: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MultiRect {
    pub slot: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub visible: bool,
}

/// 启动多路同看。最多 4 路；某一格起不来时其余格继续起，错误回传给该格。
#[tauri::command]
pub fn multi_start(
    app: tauri::AppHandle,
    ms: State<'_, MultiState>,
    urls: Vec<String>,
    nic_ip: String,
) -> Result<Vec<MultiSlotView>, String> {
    let _ = nic_ip; // 多路直放暂未绑定网卡；保留参数与单路接口一致，后续接桥时可用
    let cleaned: Vec<String> = urls
        .iter()
        .map(|u| crate::clean_stream_url(u))
        .filter(|u| !u.is_empty())
        .take(4)
        .collect();
    if cleaned.is_empty() {
        return Err("没有可播放的来源".into());
    }
    let _ = crate::mpv_host::take_clicks(); // 丢掉上一轮残留的点击
    let mpv_exe = vqa_core::tools::find_mpv().ok_or("未找到 mpv.exe（bin/mpv/mpv.exe）")?;

    let mut slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    if slots.is_empty() {
        // 正常路径在 setup 已建好；万一 setup 拿 HWND 失败，这里兜底补建。
        let win = app
            .get_webview_window("main")
            .ok_or("找不到主窗口，无法创建多路子窗")?;
        let hwnd = win.hwnd().map_err(|e| format!("取主窗口句柄失败：{e}"))?;
        for i in 0..4 {
            if let Some(c) = crate::mpv_host::VideoChild::create_slot(hwnd.0 as isize, i + 1) {
                slots.push(crate::state::MultiSlot::new(c));
            }
        }
    }
    if slots.is_empty() {
        return Err("多路子窗尚未创建（请重启应用）".into());
    }

    let old_mpvs: Vec<std::sync::Arc<Mpv>> = slots.iter_mut().filter_map(|s| s.mpv.take()).collect();
    for s in slots.iter_mut() {
        s.url.clear();
        s.status = "待机".into();
        s.err = None;
    }
    drop(slots);
    retire(old_mpvs); // 锁外并行回收旧 mpv，必须等宿主窗释放后再启动新进程
    let mut slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    *ms.active.lock().unwrap_or_else(|e| e.into_inner()) = 0;

    for (i, url) in cleaned.iter().enumerate() {
        let slot = &mut slots[i];
        slot.url = url.clone();
        slot.status = "正在连接…".into();
        // 先静音再 loadfile：四路同时起播时不会先炸出一片混音。
        match Mpv::launch(&mpv_exe, Some(slot.child.hwnd())) {
            Ok(m) => {
                let _ = m.set_prop("mute", serde_json::json!(true));
                let _ = m.set_prop("volume", serde_json::json!(100));
                let _ = m.set_prop("pause", serde_json::json!(false));
                match m.loadfile(url) {
                    Ok(()) => {
                        slot.status = "等待画面…".into();
                        slot.mpv = Some(m);
                        crate::events::ev("multi", &format!("第 {} 路起播 {url}", i + 1));
                    }
                    Err(e) => {
                        slot.err = Some(format!("加载失败：{e}"));
                        slot.status = "加载失败".into();
                        // m 在这里 drop，mpv 进程一并回收
                    }
                }
            }
            Err(e) => {
                slot.err = Some(format!("启动失败：{e}"));
                slot.status = "启动失败".into();
            }
        }
    }

    // 只让第一格“真正启动成功”的路出声；若第 1 路失败，声音自动落到第 2 路。
    let first_ok = slots.iter().position(|s| s.mpv.is_some()).unwrap_or(0);
    for (i, s) in slots.iter().enumerate() {
        if let Some(m) = s.mpv.as_ref() {
            let _ = m.set_prop("mute", serde_json::json!(i != first_ok));
        }
    }
    *ms.active.lock().unwrap_or_else(|e| e.into_inner()) = first_ok;

    // 补齐空白格的状态，返回和格数一致的视图
    Ok(snapshot_locked(&slots))
}

/// 排空所有 mpv 管道并回传每格状态（前端 150ms 轮询）。
#[tauri::command]
pub fn multi_snapshot(ms: State<'_, MultiState>) -> Vec<MultiSlotView> {
    let slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    for s in slots.iter() {
        if let Some(m) = s.mpv.as_ref() {
            m.poll();
        }
    }
    let mut rows = snapshot_locked(&slots);
    // 原生子窗点击由 wndproc 排队，和快照合并返回；一次快照内同一格可能
    // 先单击后双击（双击的第一次 up 也会计一次），双击优先，前端只放大。
    for (slot, dbl) in crate::mpv_host::take_clicks() {
        if let Some(row) = rows.get_mut(slot) {
            if dbl {
                row.dblclick = true;
            } else {
                row.click = true;
            }
        }
    }
    rows
}

fn snapshot_locked(slots: &[crate::state::MultiSlot]) -> Vec<MultiSlotView> {
    let active = 0usize; // 仅用于兜底；实际 mute 由 mpv 自己汇报
    let _ = active;
    slots
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut row = MultiSlotView {
                index: i,
                url: s.url.clone(),
                status: s.status.clone(),
                error: s.err.clone(),
                playing: false,
                paused: false,
                idle: true,
                width: 0,
                height: 0,
                video_codec: String::new(),
                audio_codec: String::new(),
                volume: 0.0,
                mute: true,
                avsync: 0.0,
                v_bitrate: 0.0,
                time_ms: 0.0,
                duration_ms: 0.0,
                click: false,
                dblclick: false,
            };
            let Some(m) = s.mpv.as_ref() else {
                return row;
            };
            if m.is_dead() {
                row.status = "播放器已退出".into();
                row.error = Some("mpv 进程已退出（编码不支持或网络中断）".into());
                return row;
            }
            let snap = m.snapshot();
            row.playing = !snap.idle;
            row.paused = snap.paused;
            row.idle = snap.idle;
            row.width = snap.width;
            row.height = snap.height;
            row.video_codec = snap.video_codec.clone();
            row.audio_codec = snap.audio_codec.clone();
            row.volume = snap.volume;
            row.mute = snap.mute;
            row.avsync = snap.avsync;
            row.v_bitrate = snap.v_bitrate;
            row.time_ms = snap.time_ms;
            row.duration_ms = snap.duration_ms;
            if !s.url.is_empty() {
                row.status = if snap.idle {
                    s.status.clone()
                } else if snap.paused {
                    "已暂停".into()
                } else {
                    "播放中".into()
                };
            }
            row
        })
        .collect()
}

/// 选中第 `index` 格：仅该格解除静音，其余格全部静音。
#[tauri::command]
pub fn multi_select(ms: State<'_, MultiState>, index: usize) -> Result<(), String> {
    let slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    if slots.is_empty() {
        return Err("多路尚未启动".into());
    }
    let active = index.min(slots.len() - 1);
    for (i, s) in slots.iter().enumerate() {
        if let Some(m) = s.mpv.as_ref() {
            m.set_prop("mute", serde_json::json!(i != active))
                .map_err(|e| format!("第 {} 路静音切换失败：{e}", i + 1))?;
        }
    }
    *ms.active.lock().unwrap_or_else(|e| e.into_inner()) = active;
    Ok(())
}

/// 播放/暂停选中格。空闲格返回错误，避免把已结束的流误当暂停。
#[tauri::command]
pub fn multi_toggle_pause(ms: State<'_, MultiState>, index: usize) -> Result<bool, String> {
    let slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    let slot = slots.get(index).ok_or("多路格不存在")?;
    let m = slot.mpv.as_ref().ok_or("该路尚未启动")?;
    if m.snapshot().idle {
        return Err("该路没有在播放".into());
    }
    m.cycle_pause()?;
    Ok(m.get_bool("pause").unwrap_or(false))
}

/// 选中格音量（0–130，和单路控制条同一口径）。
#[tauri::command]
pub fn multi_set_volume(ms: State<'_, MultiState>, index: usize, volume: f64) -> Result<(), String> {
    let slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    let slot = slots.get(index).ok_or("多路格不存在")?;
    let m = slot.mpv.as_ref().ok_or("该路尚未启动")?;
    m.set_volume(volume.clamp(0.0, 130.0))
        .map_err(|e| format!("音量设置失败：{e}"))
}

/// 停止多路同看并回收全部 mpv / 桥句柄。子窗留在主窗口，由前端移出屏幕。
#[tauri::command]
pub fn multi_stop(ms: State<'_, MultiState>) {
    let old_mpvs: Vec<std::sync::Arc<Mpv>> = {
        let mut slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
        let mut olds = Vec::new();
        for s in slots.iter_mut() {
            if let Some(m) = s.mpv.take() {
                olds.push(m);
            }
            s.url.clear();
            s.status = "待机".into();
            s.err = None;
        }
        olds
    };
    retire(old_mpvs); // 锁外并行回收；最多约 1 秒，确保 mpv 全部退出再返回
    *ms.active.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    let _ = crate::mpv_host::take_clicks();
    crate::events::ev("multi", "多路同看已停止");
}

/// 旧 mpv 在独立线程里退出：先静音，再交给 `Mpv::drop` 等 quit/必要时强杀。
/// 这样停止/重开多路不必在主线程串行等待 4 × 1 秒，旧进程也不会继续出声。
fn retire(mpvs: Vec<std::sync::Arc<Mpv>>) {
    // 四路并行退出，每路最多等 1 秒；必须等旧 mpv 真正释放 --wid 宿主窗，
    // 否则紧接着启动的新 mpv 会和旧进程抢同一个子窗，出现黑屏/叠音。
    let handles: Vec<_> = mpvs
        .into_iter()
        .map(|m| {
            let _ = m.set_prop("mute", serde_json::json!(true));
            std::thread::spawn(move || drop(m))
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}

/// 对齐每一格的视频子窗。前端每帧/窗口尺寸变化时调用。
///
/// 先把所有子窗移出屏幕，再按本次传入的矩形放回可见格——这样前端切到放大页、
/// 退出同看、换宫格数量时不会留下上一次的残影窗口。
#[tauri::command]
pub fn multi_place(ms: State<'_, MultiState>, rects: Vec<MultiRect>) {
    let mut slots = ms.slots.lock().unwrap_or_else(|e| e.into_inner());
    for s in slots.iter_mut() {
        s.child.place(-20000, -20000, 1, 1, false);
    }
    for r in rects {
        if let Some(s) = slots.get_mut(r.slot) {
            s.child.place(r.x, r.y, r.w, r.h, r.visible);
        }
    }
}
