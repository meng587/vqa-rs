//! 播放器页 —— Python `PlayerPage` + Rust `vkitplayer` 的融合移植。
//!
//! 架构：
//! * mpv.exe 独立进程（崩溃隔离）+ `--wid` 嵌入 VideoChild 子窗；
//! * 起播前 `probe_quick` 判 AVS 桥（8MB/4s 窗口；4K/10-bit 走 ffmpeg 桥）；
//! * 组播源按所选网卡带 `localaddr`；
//! * 控制条：播放/暂停/停止/进度/音量/倍速/快照/录制/强制桥接。

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, RichText, Ui, Vec2};
use vqa_core::bridge::RealtimeBridge;
use vqa_core::player::{Mpv, Snapshot};
use vqa_core::probe;

use crate::theme;
use crate::widgets::lucide::Lucide;
use crate::video_child::VideoChild;
use crate::widgets::nic_selector::NicSelector;
use crate::widgets::source_bar::{SourceAction, SourceBar};
use crate::widgets::status::PageState;

/// 起播探测的结果（探测线程 → UI）。
enum Launch {
    Direct {
        url: String, // 已带 localaddr 选项包装
    },
    Bridged {
        bridge: RealtimeBridge,
        url: String,
        reason: String,
    },
    Failed(String),
}

enum ProbeMsg {
    Outcome(Launch),
    ProbeInfo(probe::QuickProbe),
}

const SPEEDS: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

pub struct PlayerPage {
    mpv: Option<Arc<Mpv>>,
    child: Option<VideoChild>,
    main_hwnd: isize,
    /// 本次播放的原始地址
    playing_url: String,
    bridge: Option<RealtimeBridge>,
    /// 录制输出文件（stream-record 直拷）
    recording: Option<PathBuf>,
    /// 起播探测通道
    rx: Receiver<ProbeMsg>,
    probe_url: Option<String>,
    probe_info: Option<probe::QuickProbe>,
    /// ffmpeg 桥存在时的说明
    bridge_note: String,
    err: Option<String>,
    hint: String,
    hint_until: Option<Instant>,

    bar: SourceBar,
    nic: NicSelector,
    force_bridge: bool,

    seek_val: f32,
    dragging: bool,
    speed_idx: usize,
    volume: f64,
    muted: bool,

    snapshot_dir: PathBuf,
    record_dir: PathBuf,
    last_poll: Instant,
}

impl PlayerPage {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx; // 每次起播重建
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            mpv: None,
            child: None,
            main_hwnd: 0,
            playing_url: String::new(),
            bridge: None,
            recording: None,
            rx,
            probe_url: None,
            probe_info: None,
            bridge_note: String::new(),
            err: None,
            hint: String::new(),
            hint_until: None,
            bar: SourceBar::new("player", "本地文件 / udp://@239.x.x.x:port / rtsp://…"),
            nic: NicSelector::new(false),
            force_bridge: false,
            seek_val: 0.0,
            dragging: false,
            speed_idx: 2,
            volume: 100.0,
            muted: false,
            snapshot_dir: home.join("Pictures").join("vqa_snapshots"),
            record_dir: home.join("Videos").join("vqa_recordings"),
            last_poll: Instant::now(),
        }
    }

    pub fn nav_state(&self) -> PageState {
        if self.err.is_some() {
            PageState::Error
        } else if self.playing_url.is_empty() {
            PageState::Idle
        } else {
            PageState::Running
        }
    }

    pub fn ensure_child(&mut self, hwnd: isize) {
        self.main_hwnd = hwnd;
        if self.child.is_none() {
            self.child = VideoChild::create(hwnd);
        }
    }

    pub fn set_child_visible(&mut self, visible: bool) {
        if let Some(c) = self.child.as_mut() {
            // 页面切走时按 (0,0,1,1) 隐藏；回来由 ui() 的 place 恢复
            if !visible {
                c.place(egui::Rect::from_min_size(egui::Pos2::ZERO, [1.0, 1.0].into()), 1.0, false);
            }
        }
    }

    fn show_hint(&mut self, msg: impl Into<String>) {
        self.hint = msg.into();
        self.hint_until = Some(Instant::now() + std::time::Duration::from_secs(3));
    }

    // ------------------------------------------------------------ 起播

    fn play(&mut self, url: String, nic_ip: String) {
        self.stop_bridge();
        self.err = None;
        self.playing_url = url.clone();
        self.probe_info = None;
        self.bridge_note.clear();
        if self.mpv.is_none() {
            let Some(exe) = vqa_core::tools::find_mpv() else {
                self.err = Some("未找到 mpv.exe（bin/mpv/mpv.exe）".into());
                return;
            };
            match Mpv::launch(&exe, Some(self.child.as_ref().map_or(0, |c| c.hwnd()))) {
                Ok(m) => self.mpv = Some(m),
                Err(e) => {
                    self.err = Some(format!("启动 mpv 失败：{e}"));
                    return;
                }
            }
        }
        // 探测线程：probe_quick → 桥判定 → （需要时）起桥
        let (tx, rx) = std::sync::mpsc::channel::<ProbeMsg>();
        self.rx = rx;
        self.probe_url = Some(url.clone());
        let is_local = vqa_core::bridge::is_local(&url);
        std::thread::Builder::new()
            .name("vqa-probe".into())
            .spawn(move || {
                let q = if is_local {
                    probe::probe_quick(&url, 5.0, "")
                } else {
                    probe::probe_quick(&url, 6.0, &nic_ip)
                };
                if let Some(q) = &q {
                    let _ = tx.send(ProbeMsg::ProbeInfo(q.clone()));
                }
                let need = vqa_core::bridge::needs_bridge(&q, false);
                if !need {
                    let _ = tx.send(ProbeMsg::Outcome(Launch::Direct { url }));
                    return;
                }
                // 起桥（阻塞 ~1.2s 存活检查，线程内无妨）
                let Some(ffmpeg) = vqa_core::tools::find_ffmpeg() else {
                    let _ = tx.send(ProbeMsg::Outcome(Launch::Failed(
                        "需要转码桥但未找到 ffmpeg".into(),
                    )));
                    return;
                };
                let ten_bit = q.as_ref().map(|q| q.video.pix_fmt.contains("10")).unwrap_or(false);
                let davs2 = vqa_core::tools::find_davs2();
                if ten_bit && davs2.is_none() {
                    let _ = tx.send(ProbeMsg::Outcome(Launch::Failed(
                        "10-bit AVS2 需要 vkit_davs2.exe（bin/davs2-10bit/）".into(),
                    )));
                    return;
                }
                let chain10 = ten_bit;
                let davs2_path = davs2.unwrap_or_else(|| PathBuf::from("davs2"));
                match RealtimeBridge::start(&ffmpeg, &davs2_path, &url, chain10, 26, &nic_ip) {
                    Ok(b) => {
                        let mode = b.mode.clone();
                        let port = b.port;
                        let _ = tx.send(ProbeMsg::Outcome(Launch::Bridged {
                            bridge: b,
                            url: format!("udp://@127.0.0.1:{port}"),
                            reason: format!("AVS 源自动转码桥（{mode}）"),
                        }));
                    }
                    Err(e) => {
                        // 桥起不来 → 回退直放并报因（口径同 Python 版）
                        let _ = tx.send(ProbeMsg::Outcome(Launch::Failed(format!(
                            "转码桥失败：{e}（回退直放）"
                        ))));
                    }
                }
            })
            .ok();
    }

    fn consume_launch(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                ProbeMsg::ProbeInfo(q) => self.probe_info = Some(q),
                ProbeMsg::Outcome(l) => {
                    self.probe_url = None;
                    match l {
                        Launch::Direct { url } => {
                            if let Some(m) = &self.mpv {
                                if let Err(e) = m.loadfile(&url) {
                                    self.err = Some(format!("加载失败：{e}"));
                                } else {
                                    self.show_hint("已加载");
                                }
                            }
                        }
                        Launch::Bridged { bridge, url, reason } => {
                            self.bridge_note = reason;
                            let port = bridge.port;
                            let mode = bridge.mode.clone();
                            self.bridge = Some(bridge);
                            if let Some(m) = &self.mpv {
                                let _ = m.loadfile(&url);
                                let _ = m.show_text(&format!("转码桥 {mode} :{port}"), 4000);
                            }
                            ctx.request_repaint();
                        }
                        Launch::Failed(e) => {
                            // 直放兜底
                            if let Some(m) = &self.mpv {
                                let _ = m.loadfile(&self.playing_url);
                            }
                            self.err = Some(e);
                        }
                    }
                }
            }
        }
    }

    fn stop_bridge(&mut self) {
        if let Some(b) = self.bridge.take() {
            b.stop();
        }
        self.bridge_note.clear();
    }

    pub fn shutdown(&mut self) {
        if let Some(m) = &self.mpv {
            let _ = m.stop();
        }
        self.mpv = None; // Drop → quit + kill
        self.stop_bridge();
        if let Some(c) = self.child.take() {
            c.destroy();
        }
    }

    // ------------------------------------------------------------ UI

    pub fn ui(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        // mpv 保活排空 + 事件（每帧）
        if let Some(m) = &self.mpv {
            m.poll();
            if m.is_dead() {
                self.mpv = None;
                self.stop_bridge();
                self.err = Some("mpv 已退出（崩溃或被关闭）".into());
            }
        }
        self.consume_launch(ctx);
        if let Some(t) = self.hint_until {
            if Instant::now() > t {
                self.hint.clear();
                self.hint_until = None;
            }
        }

        // 来源栏
        let running = !self.playing_url.is_empty() || self.probe_url.is_some();
        let action = self.bar.show(ui, &mut self.nic, running);
        match action {
            SourceAction::Start { url, nic_ip } => self.play(url, nic_ip),
            SourceAction::Stop => {
                self.playing_url.clear();
                if let Some(m) = &self.mpv {
                    let _ = m.stop();
                }
                self.stop_bridge();
            }
            SourceAction::None => {}
        }

        // 错误/提示行
        if let Some(e) = &self.err {
            ui.colored_label(theme::BAD, RichText::new(format!("✗ {e}")).size(12.0));
        }
        if !self.hint.is_empty() {
            ui.label(RichText::new(&self.hint).size(12.0).color(theme::MUT));
        }

        // 视频卡片（设计稿：控制条叠层在卡片内部底部）。
        // mpv 原生子窗只占卡片**上半**（画面子矩形）；底部 44px 控制条带
        // 仍是 egui 地盘——播放时按钮不被视频盖住，观感与叠层一致。
        let snap = self.mpv.as_ref().map(|m| m.snapshot()).unwrap_or_default();
        let has_media = !self.playing_url.is_empty() && !snap.idle;
        let avail = ui.available_size();
        // 卡片下方：chips 28 + 强制桥接 24 + 行距 ~12
        let card_size = Vec2::new(avail.x, (avail.y - 66.0).max(220.0));
        let (card_rect, _) = ui.allocate_exact_size(card_size, egui::Sense::hover());
        let p = ui.painter_at(card_rect);
        p.rect_filled(card_rect, 10.0, theme::DARK);
        p.rect_stroke(card_rect, 10.0, egui::Stroke::new(1.0, theme::BORDER2));

        let ctrl_h = 44.0;
        // mpv 画面子矩形（卡片内、控制条上方；留 1px 露出圆角底色）
        let mpv_rect = egui::Rect::from_min_max(
            card_rect.left_top() + Vec2::new(1.0, 1.0),
            card_rect.right_bottom() + Vec2::new(-1.0, -ctrl_h),
        );
        // 辉光：三层同心圆（画在画面子矩形；播放时被视频盖住，待机可见）
        if mpv_rect.width() > 120.0 && mpv_rect.height() > 90.0 {
            let c = mpv_rect.center();
            let base = (mpv_rect.height() * 0.44).min(210.0);
            p.circle_filled(c, base, Color32::from_rgba_premultiplied(10, 22, 34, 255));
            p.circle_filled(c, base * 0.66, Color32::from_rgba_premultiplied(13, 29, 44, 255));
            p.circle_filled(c, base * 0.38, Color32::from_rgba_premultiplied(17, 38, 56, 255));
        }
        // 角标：左上编码信息 / 右上转码桥警示（待机/加载时可见；播放中被画面覆盖，
        // 信息由下方 chips 行承载——mpv 原生子窗物理上盖住 egui 绘制）
        if snap.width > 0 {
            let mut tag = format!("{}×{}", snap.width, snap.height);
            if !snap.video_codec.is_empty() {
                tag += &format!(" · {}", short_codec(&snap.video_codec));
            }
            if !snap.hwdec.is_empty() && snap.hwdec != "no" {
                tag += &format!(" · 硬解 {}", snap.hwdec);
            }
            draw_tag(ui, card_rect, tag, egui::Align2::LEFT_TOP, theme::MUT);
        }
        if !self.bridge_note.is_empty() {
            draw_tag(
                ui,
                card_rect,
                format!("⚠ {}", self.bridge_note),
                egui::Align2::RIGHT_TOP,
                theme::WARN,
            );
        }
        // 中央：大播放键（设计稿待机即显示；空源点击给提示）
        if !has_media {
            let c = mpv_rect.center() + Vec2::new(0.0, -6.0);
            p.circle_filled(c, 29.0, Color32::from_rgba_unmultiplied(255, 255, 255, 26));
            p.text(
                c + Vec2::new(1.5, 0.0),
                egui::Align2::CENTER_CENTER,
                Lucide::Play.glyph(),
                egui::FontId::new(
                    26.0,
                    egui::FontFamily::Name(crate::widgets::lucide::LUCIDE_FAMILY.into()),
                ),
                Color32::WHITE,
            );
            let hit = egui::Rect::from_center_size(c, Vec2::new(70.0, 70.0));
            let resp = ui.allocate_rect(hit, egui::Sense::click());
            if resp.clicked() {
                if self.playing_url.is_empty() {
                    self.show_hint("请先在上方填写来源");
                } else if let Some(m) = &self.mpv {
                    let _ = m.loadfile(&self.playing_url);
                }
            }
            p.text(
                mpv_rect.center() + Vec2::new(0.0, 46.0),
                egui::Align2::CENTER_CENTER,
                "输入来源后点「开始」播放",
                egui::FontId::proportional(13.0),
                theme::MUT,
            );
        }
        if let Some(c) = self.child.as_mut() {
            c.place(mpv_rect, ctx.pixels_per_point(), has_media);
        }

        // 控制条：卡片内底部黑带（叠层观感；mpv 子矩形不含此区域，永不被盖）
        let dur = snap.duration_ms / 1000.0;
        let pos = snap.time_ms / 1000.0;
        let seekable = snap.seekable && dur > 0.0;
        let ctrl_rect = egui::Rect::from_min_max(
            egui::Pos2::new(card_rect.left(), card_rect.bottom() - ctrl_h),
            egui::Pos2::new(card_rect.right(), card_rect.bottom()),
        );
        p.rect_filled(ctrl_rect, 0.0, Color32::from_rgba_premultiplied(6, 8, 12, 215));
        ui.allocate_ui_at_rect(ctrl_rect.shrink2(Vec2::new(10.0, 6.0)), |ctrl| {
            ctrl.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                self.controls(ui, &snap);
            });
        });

        // 信息 chips（带小图标）
        self.chips(ui, &snap);
        // 强制桥接开关
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.force_bridge, "强制走转码桥")
                .on_hover_text("下次起播强制经 ffmpeg 转码（排查直放崩溃时用）");
        });

        // 进度轮询刷新（~10Hz 足够；mpv 管道排空已在帧头做过）
        let now = Instant::now();
        if (now - self.last_poll).as_millis() > 100 {
            self.last_poll = now;
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// 控制条（叠层内）：左簇 + seek + 右簇。
    fn controls(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let paused = snap.paused || snap.idle;
        if crate::widgets::icon_button(ui, if paused { Lucide::Play } else { Lucide::Pause }).clicked() {
            if let Some(m) = &self.mpv {
                if snap.idle && !self.playing_url.is_empty() {
                    let _ = m.loadfile(&self.playing_url);
                } else {
                    let _ = m.set_pause(!paused);
                }
            }
        }
        if crate::widgets::icon_button(ui, Lucide::Square).clicked() {
            self.playing_url.clear();
            self.stop_bridge();
            if let Some(m) = &self.mpv {
                let _ = m.stop();
            }
        }
        // seek：右侧控件簇实测预留，防溢出
        let dur = snap.duration_ms / 1000.0;
        let pos = snap.time_ms / 1000.0;
        let seekable = snap.seekable && dur > 0.0;
        let right_cluster = 430.0_f32;
        let frac = if seekable {
            if self.dragging {
                (self.seek_val / 1000.0).clamp(0.0, 1.0)
            } else {
                ((pos / dur) as f32).clamp(0.0, 1.0)
            }
        } else if dur > 0.0 {
            ((pos / dur) as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if let Some(x) = crate::widgets::seek_bar_at(
            ui,
            (ui.available_width() - right_cluster).max(60.0),
            frac,
            seekable,
        ) {
            self.dragging = true;
            self.seek_val = x * 1000.0;
            if seekable {
                if let Some(m) = &self.mpv {
                    let _ = m.seek_percent((x * 100.0) as f64);
                }
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.dragging = false;
        }
        self.controls_right(ui, snap);
    }

    /// 控制条右侧簇：时间 + 音量 + 快照 + 录制 + 倍速。
    fn controls_right(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let dur = snap.duration_ms / 1000.0;
        let pos = snap.time_ms / 1000.0;
        // 时间
        ui.label(
            RichText::new(format!(
                "{} / {}",
                fmt_hms(pos),
                if dur > 0.0 { fmt_hms(dur) } else { "--:--".to_string() }
            ))
            .monospace()
            .size(11.0)
            .color(Color32::from_rgb(0xcf, 0xd8, 0xe3)),
        );
        // 音量（图标点击 = 静音切换）
        let vol_resp = crate::widgets::icon_button_tinted(
            ui,
            Lucide::Volume2,
            if self.muted { theme::BAD } else { theme::MUT },
        );
        if vol_resp.clicked() {
            self.muted = !self.muted;
            if let Some(m) = &self.mpv {
                let _ = m.set_mute(self.muted);
            }
        }
        let vol = ui.add_sized(
            [64.0, 14.0],
            egui::Slider::new(&mut self.volume, 0.0..=130.0)
                .show_value(false)
                .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.35 }),
        );
        if vol.changed() {
            if let Some(m) = &self.mpv {
                let _ = m.set_volume(self.volume);
            }
        }
        // 快照
        if crate::widgets::icon_button(ui, Lucide::Camera).clicked() {
            if let Some(m) = &self.mpv {
                let _ = std::fs::create_dir_all(&self.snapshot_dir);
                let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let p = self.snapshot_dir.join(format!("snap_{ts}.png"));
                match m.screenshot(&p) {
                    Ok(_) => self.show_hint(format!("已存 {}", p.display())),
                    Err(e) => self.show_hint(format!("快照失败：{e}")),
                }
            }
        }
        // 录制
        if crate::widgets::icon_button_tinted(
            ui,
            Lucide::CircleDot,
            if self.recording.is_some() { theme::BAD } else { theme::MUT },
        )
        .clicked()
        {
            if let Some(m) = &self.mpv {
                if let Some(rec) = self.recording.take() {
                    let _ = m.stop_record();
                    self.show_hint(format!("已录制 {}", rec.display()));
                } else {
                    let _ = std::fs::create_dir_all(&self.record_dir);
                    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                    let p = self.record_dir.join(format!("rec_{ts}.ts"));
                    match m.record(&p) {
                        Ok(_) => {
                            self.recording = Some(p);
                            self.show_hint("录制中（stream-record 直拷）");
                        }
                        Err(e) => self.show_hint(format!("录制失败：{e}")),
                    }
                }
            }
        }
        // 倍速
        egui::ComboBox::from_id_source("speed")
            .width(62.0)
            .selected_text(format!("{}x", SPEEDS[self.speed_idx]))
            .show_ui(ui, |ui| {
                for (i, s) in SPEEDS.iter().enumerate() {
                    if ui.selectable_label(self.speed_idx == i, format!("{s}x")).clicked() {
                        self.speed_idx = i;
                        if let Some(m) = &self.mpv {
                            let _ = m.set_speed(*s);
                        }
                    }
                }
            });
    }

    /// 信息 chips 行（图标 + 文本，设计稿口径）。
    /// 待机无数据时整行不画（避免孤立 chip）。
    fn chips(&self, ui: &mut Ui, snap: &Snapshot) {
        let has_any = snap.width > 0
            || !self.bridge_note.is_empty()
            || snap.vf_fps > 0.0
            || !snap.audio_codec.is_empty();
        if !has_any {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if snap.width > 0 {
                let mut t = format!("{}×{}", snap.width, snap.height);
                if !snap.video_codec.is_empty() {
                    t += &format!(" · {}", short_codec(&snap.video_codec));
                }
                if !snap.hwdec.is_empty() && snap.hwdec != "no" {
                    t += &format!(" · 硬解 {}", snap.hwdec);
                }
                crate::widgets::chip_icon(ui, None, t, theme::MUT);
            }
            if !self.bridge_note.is_empty() {
                crate::widgets::chip_icon(ui, Some(Lucide::ScanLine), format!("⚠ {}", self.bridge_note), theme::WARN);
            }
            if snap.vf_fps > 0.0 {
                crate::widgets::chip_icon(ui, Some(Lucide::Gauge), format!("{:.1} fps", snap.vf_fps), theme::MUT);
            }
            if snap.v_bitrate > 0.0 {
                crate::widgets::chip_icon(
                    ui,
                    Some(Lucide::Wifi),
                    format!("{:.0} kbps", snap.v_bitrate / 1000.0),
                    theme::MUT,
                );
            }
            if snap.width > 0 {
                let drops = snap.decoder_drop + snap.render_drop;
                crate::widgets::chip_icon(
                    ui,
                    Some(Lucide::Check),
                    format!("丢帧 {}/{}", snap.decoder_drop, snap.render_drop),
                    if drops > 0 { theme::WARN } else { theme::OK },
                );
            }
            if !snap.audio_codec.is_empty() {
                crate::widgets::chip_icon(ui, Some(Lucide::AudioLines), snap.audio_codec.clone(), theme::MUT);
            }
            if snap.avsync.abs() > 0.001 {
                crate::widgets::chip_icon(
                    ui,
                    None,
                    format!("avsync {:+.3}s", snap.avsync),
                    if snap.avsync.abs() > 0.1 { theme::WARN } else { theme::MUT },
                );
            }
        });
    }
}

fn fmt_hms(s: f64) -> String {
    let s = s.max(0.0);
    format!("{:02}:{:02}:{:02}", s as u64 / 3600, (s as u64 / 60) % 60, s as u64 % 60)
}

/// 长 codec 名取首段（"H.264 / AVC / …" → "H.264"）。
fn short_codec(c: &str) -> String {
    c.split('/').next().unwrap_or(c).trim().to_string()
}

/// 角标（半透明黑底小标签）。
fn draw_tag(
    ui: &mut Ui,
    area: egui::Rect,
    text: impl Into<String>,
    anchor: egui::Align2,
    color: Color32,
) {
    let font = egui::FontId::proportional(10.5);
    let text = text.into();
    let g = ui.painter().layout_no_wrap(text.clone(), font.clone(), color);
    let size = g.size() + Vec2::new(16.0, 7.0);
    let pos = match anchor {
        egui::Align2::LEFT_TOP => area.left_top() + Vec2::new(10.0, 10.0),
        _ => area.right_top() + Vec2::new(-10.0 - size.x, 10.0),
    };
    let rect = egui::Rect::from_min_size(pos, size);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, Color32::from_rgba_premultiplied(0, 0, 0, 115));
    p.text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
}
