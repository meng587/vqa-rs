//! 视频分析页 —— `AnalysisPage` 的移植：文件扫描 / 实时 UDP·RTP +
//! 七个页签（概览 / 流结构 / 错误与抖动 / 码率 / TS包视图 / TR101290 / 日志）。
//!
//! 架构：壳持有扫描线程与 LiveSession，页签只消费 `&ScanResult`。
//! 文件模式结果走 channel（Arc<ScanResult>）；直播模式每秒锁内核刷新。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use egui::{RichText, Ui};
use vqa_core::live::LiveSession;
use vqa_core::tsfile::{
    self, PacketFile, ScanResult, Tr290Item, TrOverrides,
};

use crate::theme;
use crate::widgets::nic_selector::NicSelector;
use crate::widgets::source_bar::{SourceAction, SourceBar};
use crate::widgets::status::PageState;
use crate::widgets::{kv, stat_card_sub};

const TABS: [&str; 7] = [
    "概览", "流结构", "错误与抖动", "码率", "TS包视图", "TR101290", "日志",
];

/// TR101290 固定条目（GY/T 299.1 口径，名称→key 映射与影响说明照搬 Python 版）。
const TR_LEVEL1: [(&str, &str, &str); 4] = [
    ("TS同步丢失", "TS_sync_loss", "完全黑屏，无任何画面输出"),
    ("PAT错误", "PAT_error", "无法搜索节目，找不到任何频道"),
    ("TS传输错误", "Transport_error", "对应位置数据损坏，大面积花屏"),
    ("连续性计数错误", "Continuity_count_error", "丢包导致局部马赛克、卡顿"),
];
const TR_LEVEL2: [(&str, &str, &str); 6] = [
    ("CRC校验错误", "CRC_error", "对应表无法解析，信息丢失"),
    ("PCR间隔错误", "PCR_error", "STC时钟异常，音画不同步"),
    ("PCR不连续", "PCR_discontinuity_error", "PCR跳变导致画面卡顿、马赛克"),
    ("PCR精度错误", "PCR_accuracy_error", "PCR时间戳精度超限，STC时钟受损"),
    ("CAT错误", "CAT_error", "加密频道CA信息无法识别"),
    ("PID错误", "PID_error", "未声明PID出现，流结构异常"),
];
const TR_LEVEL3: [(&str, &str, &str); 7] = [
    ("NIT错误", "NIT_id_error", "网络信息、频率参数异常"),
    ("SDT错误", "SDT_id_error", "频道名称、提供商信息异常"),
    ("EIT错误", "EIT_id_error", "EPG节目单、节目预告异常"),
    ("SI重复率错误", "SI_repetition_error", "SI表重复间隔不符合规范"),
    ("SDT当前间隔错误", "SDT_act_interval_error", "SDT当前业务表超时"),
    ("EIT当前间隔错误", "EIT_act_interval_error", "EIT当前事件表超时"),
    ("TDT错误", "TDT_error", "时间日期表异常"),
];

enum Mode {
    Idle,
    File,
    Live,
}

pub struct AnalysisPage {
    bar: SourceBar,
    nic: NicSelector,
    mode: Mode,
    running_flag: Arc<AtomicBool>,
    progress_done: Arc<AtomicU64>,
    progress_total: Arc<AtomicU64>,
    scan_rx: Option<std::sync::mpsc::Receiver<Result<Arc<ScanResult>, String>>>,
    result: Option<Arc<ScanResult>>,
    live: Option<LiveSession>,
    live_err: Option<String>,
    overrides: Arc<Mutex<TrOverrides>>,
    tab: usize,
    log: VecDeque<String>,
    pkt_file: Option<PacketFile>,
    pkt_page: usize,
}

impl AnalysisPage {
    pub fn new() -> Self {
        Self {
            bar: SourceBar::new("analysis", "TS 文件 / udp://@239.x.x.x:port / rtp://:port"),
            nic: NicSelector::new(false),
            mode: Mode::Idle,
            running_flag: Arc::new(AtomicBool::new(false)),
            progress_done: Arc::new(AtomicU64::new(0)),
            progress_total: Arc::new(AtomicU64::new(0)),
            scan_rx: None,
            result: None,
            live: None,
            live_err: None,
            overrides: Arc::new(Mutex::new(TrOverrides::new())),
            tab: 0,
            log: VecDeque::new(),
            pkt_file: None,
            pkt_page: 0,
        }
    }

    pub fn nav_state(&self) -> PageState {
        if self.live_err.is_some() {
            PageState::Error
        } else if matches!(self.mode, Mode::Idle) {
            PageState::Idle
        } else {
            PageState::Running
        }
    }

    pub fn shutdown(&mut self) {
        self.running_flag.store(true, Ordering::Relaxed);
        if let Some(l) = self.live.take() {
            l.stop();
        }
    }

    fn log_push(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > 500 {
            self.log.pop_front();
        }
    }

    fn start(&mut self, url: String, nic_ip: String) {
        // 运行中忽略重复启动（Enter+按钮双触发会互相覆盖 scan_rx）
        if !matches!(self.mode, Mode::Idle) {
            return;
        }
        self.live_err = None;
        let cleaned = crate::widgets::clean_stream_url(&url);
        // 本地文件 → 文件扫描线程；udp/rtp → 实时会话
        let is_live = cleaned.starts_with("udp://") || cleaned.starts_with("rtp://");
        if is_live {
            let overrides = self.overrides.lock().unwrap().clone();
            match LiveSession::start(&cleaned, &nic_ip, &overrides) {
                Ok(s) => {
                    self.live = Some(s);
                    self.mode = Mode::Live;
                    self.result = None;
                    self.log_push(format!("实时分析已启动：{cleaned}"));
                }
                Err(e) => {
                    self.live_err = Some(format!("{e:#}"));
                }
            }
        } else if std::path::Path::new(&cleaned).is_file() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.scan_rx = Some(rx);
            self.mode = Mode::File;
            self.result = None;
            self.running_flag.store(false, Ordering::Relaxed);
            let stop = Arc::new(AtomicBool::new(false));
            self.running_flag = Arc::clone(&stop);
            let done = Arc::clone(&self.progress_done);
            let total = Arc::clone(&self.progress_total);
            let ov = self.overrides.lock().unwrap().clone();
            let path = cleaned.clone();
            self.progress_total
                .store(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
            std::thread::Builder::new()
                .name("vqa-scan".into())
                .spawn(move || {
                    let mut last_prog = 0u64;
                    let mut res = tsfile::scan_file(
                        std::path::Path::new(&path),
                        Some(&mut |d, t| {
                            if d / (8 * 1024 * 1024) > last_prog {
                                last_prog = d / (8 * 1024 * 1024);
                            }
                            done.store(d, Ordering::Relaxed);
                            total.store(t, Ordering::Relaxed);
                        }),
                        Some(&|| stop.load(Ordering::Relaxed)),
                        &ov,
                    );
                    if res.not_ts {
                        let _ = tx.send(Err("不是 188/204 包长的 TS 流".into()));
                        return;
                    }
                    // GOP（ffprobe，前 60s）
                    if let Some(ffprobe) = vqa_core::tools::find_ffprobe() {
                        let (gops, truncated) =
                            tsfile::gop_list(&ffprobe, &path, 60.0);
                        res.gop = gops;
                        res.gop_note = if truncated {
                            "前 60 秒".into()
                        } else {
                            String::new()
                        };
                    }
                    let _ = tx.send(Ok(Arc::new(res)));
                })
                .ok();
            self.log_push(format!("开始扫描：{cleaned}"));
        } else {
            self.live_err = Some(format!("来源不存在或不支持：{cleaned}"));
        }
    }

    fn stop(&mut self) {
        match self.mode {
            Mode::Live => {
                if let Some(l) = self.live.take() {
                    l.stop();
                }
                self.log_push("实时分析已停止");
            }
            _ => {
                self.running_flag.store(true, Ordering::Relaxed);
                self.log_push("扫描中断");
            }
        }
        self.mode = Mode::Idle;
    }

    pub fn ui(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let running = !matches!(self.mode, Mode::Idle);
        let action = self.bar.show(ui, &mut self.nic, running);
        match action {
            SourceAction::Start { url, nic_ip } => self.start(url, nic_ip),
            SourceAction::Stop => self.stop(),
            SourceAction::None => {}
        }

        // 扫描/实时期间主动请求重绘——egui 无输入事件时不会自发刷帧，
        // 否则进度与「扫描完成」状态会一直停在旧帧不上屏
        if !matches!(self.mode, Mode::Idle) {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        // 文件扫描收割
        if let Some(rx) = &self.scan_rx {
            if let Ok(msg) = rx.try_recv() {
                self.scan_rx = None;
                match msg {
                    Ok(res) => {
                        self.log_push(format!(
                            "扫描完成：{} 包 / {:.3}s / {:.0} kbps",
                            res.packets,
                            res.duration_pcr(),
                            res.bitrate_avg() / 1000.0
                        ));
                        self.pkt_file =
                            PacketFile::open(std::path::Path::new(&res.path)).ok();
                        self.pkt_page = 0;
                        self.result = Some(res);
                        self.mode = Mode::Idle; // 完成后回待机态（结果保留显示）
                    }
                    Err(e) => {
                        self.live_err = Some(e);
                        self.mode = Mode::Idle;
                    }
                }
            }
        }

        // 状态行
        ui.horizontal(|ui| {
            match self.mode {
                Mode::Idle => {
                    ui.label(RichText::new("● 待机").color(theme::FAINT).size(12.0));
                }
                Mode::File => {
                    let d = self.progress_done.load(Ordering::Relaxed);
                    let t = self.progress_total.load(Ordering::Relaxed);
                    ui.label(
                        RichText::new(format!("● 扫描中 {}%", if t > 0 { d * 100 / t } else { 0 }))
                            .color(theme::OK)
                            .size(12.0),
                    );
                }
                Mode::Live => {
                    ui.label(
                        RichText::new("● 实时分析中").color(theme::OK).size(12.0),
                    );
                    if let Some(l) = &self.live {
                        if let Some(bytes) = l.with_core(|c| c.bytes) {
                            ui.label(
                                RichText::new(format!("已收 {bytes} 字节"))
                                    .color(theme::FAINT)
                                    .size(11.0),
                            );
                        }
                    }
                }
            }
            if let Some(e) = &self.live_err {
                ui.label(RichText::new(format!("✗ {e}")).color(theme::BAD).size(12.0));
            }
        });
        ui.add_space(2.0);

        // 页签（下划线式：激活 ACC + 底线，禁用 FAINT）
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            for (i, t) in TABS.iter().enumerate() {
                let on = self.tab == i;
                // 设计稿口径：文字式页签，选中 = 主色文字 + 2px 底线（无背景填充）
                let txt = RichText::new(*t).size(12.5).color(if on {
                    theme::ACC
                } else {
                    theme::MUT
                });
                let resp = ui.add(
                    egui::Button::new(txt)
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE)
                        .rounding(0.0),
                );
                if on {
                    let rect = resp.rect;
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            rect.left_bottom() + egui::vec2(3.0, -1.0),
                            egui::vec2(rect.width() - 6.0, 2.0),
                        ),
                        1.0,
                        theme::ACC,
                    );
                }
                if resp.clicked() {
                    self.tab = i;
                }
            }
        });
        ui.separator();

        // 数据源：文件结果 / 直播锁内核
        let is_live = matches!(self.mode, Mode::Live);
        if is_live {
            // 先把会话移出 self，避免闭包与 &mut self.pkt_page 的借用冲突
            let live = self.live.take();
            if let Some(l) = &live {
                let tab = self.tab;
                let log = std::mem::take(&mut self.log);
                let mut bytes: Option<u64> = None;
                let core_ok = l.with_core(|c| {
                    bytes = Some(c.bytes);
                    if tab == 4 {
                        // 包视图：直播用环形缓冲
                        self_render_live_packets(ui, c);
                    } else {
                        render_tabs(ui, &c.scanner.res, tab, &log, &self.pkt_file, &mut self.pkt_page, is_live);
                    }
                });
                self.log = log;
                if core_ok.is_none() {
                    bytes = None;
                }
                if bytes.is_none() {
                    ui.label("直播会话已结束");
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(1000));
            }
            self.live = live;
        } else if let Some(res) = &self.result {
            egui::ScrollArea::vertical().show(ui, |ui| {
                render_tabs(ui, res, self.tab, &self.log, &self.pkt_file, &mut self.pkt_page, is_live);
            });
        } else if matches!(self.mode, Mode::File) {
            ui.centered_and_justified(|ui| {
                ui.label("正在扫描…");
            });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("输入 TS 文件或 UDP/RTP 组播地址后点「开始」")
                        .color(theme::FAINT),
                );
            });
        }
    }
}

// ---------------------------------------------------------------- 渲染

fn render_tabs(
    ui: &mut Ui,
    res: &ScanResult,
    tab: usize,
    log: &VecDeque<String>,
    pkt_file: &Option<PacketFile>,
    pkt_page: &mut usize,
    _is_live: bool,
) {
    match tab {
        0 => overview(ui, res),
        1 => structure(ui, res),
        2 => errors_jitter(ui, res),
        3 => bitrate_tab(ui, res),
        4 => packets_tab(ui, res, pkt_file, pkt_page),
        5 => tr290_tab(ui, &res.tr290),
        _ => logs_tab(ui, log, res),
    }
}

fn overview(ui: &mut Ui, res: &ScanResult) {
    // 设计稿口径：统计卡 + 两栏面板（节目结构 | PCR 抖动）
    let (mut n1, mut n2, mut n3) = (0u64, 0u64, 0u64);
    for it in &res.tr290 {
        match it.level {
            1 => n1 += it.count,
            2 => n2 += it.count,
            _ => n3 += it.count,
        }
    }
    let l2_top = res
        .tr290
        .iter()
        .find(|i| i.level == 2 && i.count > 0)
        .map(|i| i.name.clone());
    let l3_top = res
        .tr290
        .iter()
        .find(|i| i.level == 3 && i.count > 0)
        .map(|i| i.name.clone());
    ui.horizontal_wrapped(|ui| {
        stat_card_sub(ui, "总码率", format!("{:.2} Mbps", res.bitrate_avg() / 1e6), "PCR 区间口径", theme::ACC);
        stat_card_sub(ui, "同步状态", if res.sync_errors == 0 { "正常" } else { "异常" },
            &format!("{} 字节失步", res.sync_errors),
            if res.sync_errors == 0 { theme::OK } else { theme::BAD });
        stat_card_sub(ui, "一级错误", n1.to_string(), if n1 > 0 { "秒级致断风险" } else { "无" },
            if n1 > 0 { theme::BAD } else { theme::OK });
        stat_card_sub(ui, "二级错误", n2.to_string(),
            &l2_top.unwrap_or_else(|| "无".into()),
            if n2 > 0 { theme::WARN } else { theme::OK });
        stat_card_sub(ui, "三级错误", n3.to_string(),
            &l3_top.unwrap_or_else(|| "无".into()),
            if n3 > 0 { theme::WARN } else { theme::OK });
        stat_card_sub(ui, "TS 包长", res.packet_size.to_string(), &format!("{} 包", res.packets), theme::MUT);
    });
    ui.add_space(6.0);
    // 两栏：节目结构 | PCR 抖动（设计稿 grid2）
    ui.columns(2, |cols| {
        cols[0].add_space(2.0);
        theme::panel(&mut cols[0], "节目结构", |ui| {
            if res.programs.is_empty() {
                ui.label(RichText::new("—").color(theme::FAINT));
            }
            for p in &res.programs {
                ui.horizontal(|ui| {
                    crate::widgets::pill(ui, &format!("节目 {}", p.number), theme::ACC);
                    ui.label(
                        RichText::new(format!("PMT 0x{:04X} · PCR 0x{:04X}", p.pmt_pid, p.pcr_pid))
                            .monospace()
                            .size(11.0)
                            .color(theme::MUT),
                    );
                });
                egui::Grid::new(format!("prog{}", p.number))
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        for s in &p.streams {
                            ui.label(RichText::new(format!("0x{:04X}", s.pid)).monospace());
                            ui.label(s.type_name());
                            ui.label(RichText::new(s.kind.clone()).size(10.5).color(theme::FAINT));
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
            }
        });
        cols[1].add_space(2.0);
        theme::panel(&mut cols[1], "PCR 抖动", |ui| {
            if res.pcr_samples.is_empty() {
                ui.label(RichText::new("无 PCR 采样").color(theme::FAINT));
            } else {
                let avg_iv = res.pcr_samples.iter().map(|s| s.interval_ms).sum::<f64>()
                    / res.pcr_samples.len() as f64;
                let max_oj = res
                    .pcr_samples
                    .iter()
                    .map(|s| s.oj_ms.abs())
                    .fold(0.0f64, f64::max);
                for (k, v, c) in [
                    ("平均间隔", format!("{avg_iv:.2} ms"), theme::TEXT),
                    ("最大 |OJ|", format!("{max_oj:.3} ms"), if max_oj > 1.0 { theme::WARN } else { theme::TEXT }),
                    ("CBR 判定", if res.pcr_cbr { "CBR" } else { "VBR" }.to_string(), theme::TEXT),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(k).size(12.0).color(theme::MUT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(v).monospace().size(12.0).color(c));
                        });
                    });
                    ui.separator();
                }
                let last = res.pcr_samples.last().copied().unwrap();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("最近间隔").size(12.0).color(theme::MUT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{:.2} ms", last.interval_ms)).monospace().size(12.0));
                    });
                });
            }
        });
    });
}

fn structure(ui: &mut Ui, res: &ScanResult) {
    egui::ScrollArea::vertical().id_source("struct").show(ui, |ui| {
        // 容器信息（概览页移入；设计稿流结构含完整容器数据）
        theme::panel(ui, "容器信息", |ui| {
            egui::Grid::new("container_grid").num_columns(2).striped(true).show(ui, |ui| {
                kv(ui, "文件", res.path.clone());
                ui.end_row();
                kv(ui, "大小", format!("{} bytes", res.file_size));
                ui.end_row();
                kv(ui, "包数", res.packets.to_string());
                ui.end_row();
                kv(ui, "同步错误字节", res.sync_errors.to_string());
                ui.end_row();
                kv(ui, "PCR 时长", format!("{:.3} s", res.duration_pcr()));
                ui.end_row();
                kv(ui, "PCR PID", res.pcr_pid.map_or("—".into(), |p| format!("0x{p:04X}")));
                ui.end_row();
                kv(ui, "加密", if res.encrypted() { "是" } else { "否" }.to_string());
                ui.end_row();
                kv(ui, "码率点", res.rate.len().to_string());
                ui.end_row();
                kv(ui, "SI 表", {
                    let names: Vec<String> = res.si.keys().cloned().collect();
                    if names.is_empty() { "—".into() } else { names.join(", ") }
                });
                ui.end_row();
            });
        });
        ui.add_space(8.0);
        theme::panel(ui, "节目与流", |ui| {
            for p in &res.programs {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    crate::widgets::pill(
                        ui,
                        &format!("节目 {}", p.number),
                        theme::ACC,
                    );
                    ui.label(
                        RichText::new(format!(
                            "PMT 0x{:04X} · PCR 0x{:04X}",
                            p.pmt_pid, p.pcr_pid
                        ))
                        .monospace()
                        .size(11.5)
                        .color(theme::MUT),
                    );
                });
                egui::Grid::new(format!("prog{}", p.number))
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        for s in &p.streams {
                            ui.label(RichText::new(format!("0x{:04X}", s.pid)).monospace());
                            ui.label(s.type_name());
                            ui.label(RichText::new(s.kind.clone()).size(10.5).color(theme::FAINT));
                            ui.end_row();
                        }
                    });
            }
        });
        ui.add_space(8.0);
        theme::panel(ui, "PID 统计", |ui| {
            egui::Grid::new("pid_grid").num_columns(6).striped(true).show(ui, |ui| {
                for h in ["PID", "类型", "包数", "字节", "有效载荷", "加扰"] {
                    ui.label(RichText::new(h).size(10.5).color(theme::FAINT));
                }
                ui.end_row();
                for s in res.top_pids(64) {
                    ui.label(RichText::new(format!("0x{:04X}", s.pid)).monospace());
                    ui.label(s.kind.clone());
                    ui.label(s.packets.to_string());
                    ui.label(s.bytes.to_string());
                    ui.label(s.payload_bytes.to_string());
                    ui.label(if s.scrambled > 0 { s.scrambled.to_string() } else { "—".into() });
                    ui.end_row();
                }
            });
        });
        ui.add_space(8.0);
        // SI 表内容
        for (name, val) in &res.si {
            theme::panel(ui, name, |ui| match val {
                tsfile::SiValue::Text(t) => {
                    ui.label(t.clone());
                }
                tsfile::SiValue::List(items) => {
                    egui::Grid::new(format!("si_{name}")).striped(true).show(ui, |ui| {
                        for it in items.iter().take(50) {
                            for (k, v) in it {
                                ui.label(RichText::new(k.clone()).size(11.0).color(theme::FAINT));
                                ui.label(v.to_string().trim_matches('"').to_string());
                            }
                            ui.end_row();
                        }
                    });
                }
            });
            ui.add_space(8.0);
        }
        if !res.gop.is_empty() {
            theme::panel(ui, &format!("GOP（{}）", res.gop_note), |ui| {
                egui::Grid::new("gop").num_columns(3).striped(true).show(ui, |ui| {
                    for h in ["start(s)", "dur(s)", "frames"] {
                        ui.label(RichText::new(h).size(10.5).color(theme::FAINT));
                    }
                    ui.end_row();
                    for g in res.gop.iter().take(50) {
                        ui.label(format!("{:.3}", g.start));
                        ui.label(format!("{:.3}", g.dur));
                        ui.label(g.frames.to_string());
                        ui.end_row();
                    }
                });
            });
        }
    });
}

fn errors_jitter(ui: &mut Ui, res: &ScanResult) {
    if res.pcr_samples.is_empty() {
        ui.label(RichText::new("无 PCR 采样（流中没有 PCR 或样本不足）").color(theme::MUT));
        return;
    }
    let max_oj = res.pcr_samples.iter().map(|s| s.oj_ms.abs()).fold(0.0f64, f64::max);
    let max_iv = res.pcr_samples.iter().map(|s| s.interval_ms).fold(0.0f64, f64::max);
    let avg_iv = res.pcr_samples.iter().map(|s| s.interval_ms).sum::<f64>()
        / res.pcr_samples.len() as f64;
    ui.horizontal_wrapped(|ui| {
        stat_card_sub(ui, "平均间隔", format!("{avg_iv:.2} ms"), "", theme::MUT);
        stat_card_sub(ui, "最大间隔", format!("{max_iv:.2} ms"), "阈值 40 ms",
            if max_iv > 40.0 { theme::BAD } else { theme::OK });
        stat_card_sub(ui, "最大抖动 |OJ|", format!("{max_oj:.3} ms"), "", if max_oj > 1.0 { theme::WARN } else { theme::OK });
        stat_card_sub(ui, "CBR 判定", if res.pcr_cbr { "CBR" } else { "VBR" }.to_string(), "", theme::MUT);
    });
    ui.add_space(6.0);
    theme::panel(ui, "PCR 详情（最近 100 条）", |ui| {
        egui::ScrollArea::vertical().id_source("pcr").show(ui, |ui| {
            egui::Grid::new("pcr_g").num_columns(5).striped(true).show(ui, |ui| {
                for h in ["到达", "PCR(s)", "间隔(ms)", "OJ(ms)", "AC(ms)"] {
                    ui.label(RichText::new(h).size(10.5).color(theme::FAINT));
                }
                ui.end_row();
                for s in res.pcr_samples.iter().rev().take(100) {
                    ui.label(RichText::new(format!("{:.0}", s.off)).monospace());
                    ui.label(RichText::new(format!("{:.3}", s.t)).monospace());
                    ui.label(RichText::new(format!("{:.2}", s.interval_ms)).monospace());
                    ui.label(RichText::new(format!("{:.3}", s.oj_ms)).monospace());
                    ui.label(RichText::new(format!("{:.3}", s.ac_ms)).monospace());
                    ui.end_row();
                }
            });
        });
    });
}

fn bitrate_tab(ui: &mut Ui, res: &ScanResult) {
    if res.rate.is_empty() {
        ui.label(RichText::new("无码率区间（流中没有 PCR）").color(theme::MUT));
        return;
    }
    let bps: Vec<f64> = res.rate.iter().map(|p| p.bps()).collect();
    let cur = bps.last().copied().unwrap_or(0.0);
    let avg = bps.iter().sum::<f64>() / bps.len() as f64;
    let min = bps.iter().cloned().fold(f64::MAX, f64::min);
    let max = bps.iter().cloned().fold(0.0f64, f64::max);
    let par = if min > 0.0 { max / min } else { 0.0 };
    ui.horizontal_wrapped(|ui| {
        stat_card_sub(ui, "当前", format!("{:.0} kbps", cur / 1000.0), "", theme::MUT);
        stat_card_sub(ui, "平均", format!("{:.0} kbps", avg / 1000.0), "", theme::ACC);
        stat_card_sub(ui, "最小", format!("{:.0} kbps", min / 1000.0), "", theme::MUT);
        stat_card_sub(ui, "最大", format!("{:.0} kbps", max / 1000.0), "", theme::MUT);
        stat_card_sub(ui, "峰均比", format!("{par:.2}"), "", if par > 2.0 { theme::WARN } else { theme::OK });
        stat_card_sub(ui, "采样模式", "PCR 区间".to_string(), "", theme::MUT);
    });
    ui.add_space(6.0);
    // 抽稀到 ≤1200 点
    let step = (bps.len() + 1199) / 1200;
    let pts: egui_plot::PlotPoints = bps
        .iter()
        .enumerate()
        .step_by(step.max(1))
        .map(|(i, b)| [i as f64 * step as f64, *b / 1e6])
        .collect();
    egui_plot::Plot::new("bitrate")
        .height(ui.available_height() - 20.0)
        .y_axis_label("Mbps")
        .x_axis_label("PCR 区间序号")
        .show(ui, |p| {
            p.line(egui_plot::Line::new(pts).color(theme::ACC).width(1.4_f32));
        });
}

fn packets_tab(ui: &mut Ui, res: &ScanResult, pkt_file: &Option<PacketFile>, page: &mut usize) {
    // 文件模式：按页随机读；直播模式：环形缓冲（外层已处理）
    let Some(pf) = pkt_file else {
        ui.label("（文件扫描完成后可在此翻看原始 TS 包；直播模式显示最近 300 包）");
        if !res.not_ts && res.packet_size > 0 && std::path::Path::new(&res.path).is_file() {
            if ui.button("打开包视图").clicked() {
                // PacketFile::open 在外部处理（见下方 packets_tab_file）
            }
        }
        return;
    };
    let per_page = 100usize;
    let total_pages = (pf.count as usize + per_page - 1) / per_page;
    *page = (*page).min(total_pages.saturating_sub(1));
    ui.horizontal(|ui| {
        if ui.button("◀ 上一页").clicked() && *page > 0 {
            *page -= 1;
        }
        ui.label(format!("第 {}/{} 页（共 {} 包）", *page + 1, total_pages, pf.count));
        if ui.button("下一页 ▶").clicked() && *page + 1 < total_pages {
            *page += 1;
        }
    });
    egui::ScrollArea::vertical().id_source("pkts").show(ui, |ui| {
        egui::Grid::new("pkt_g").num_columns(6).striped(true).show(ui, |ui| {
            for h in ["#", "PID", "PUSI", "AFC", "CC", "前 16 字节"] {
                ui.label(RichText::new(h).strong().size(11.0));
            }
            ui.end_row();
            for (i, pkt) in pf.iter_from((*page * per_page) as u64, per_page as u64) {
                let info = tsfile::packet_info(&pkt);
                ui.label(i.to_string());
                ui.label(format!("0x{:04X}", info.pid));
                ui.label(if info.pusi { "●" } else { "" });
                ui.label(info.afc.to_string());
                ui.label(info.cc.to_string());
                ui.label(
                    RichText::new(hex16(&pkt[..16.min(pkt.len())]))
                        .monospace()
                        .size(10.0),
                );
                ui.end_row();
            }
        });
    });
}

/// 直播模式的包视图（环形缓冲最近 300 包）。
fn self_render_live_packets(ui: &mut Ui, core: &mut vqa_core::live::LiveCore) {
    ui.label(
        RichText::new(format!("最近 {} 包（直播）", core.ring.len()))
            .size(12.0)
            .strong(),
    );
    egui::ScrollArea::vertical().id_source("live_pkts").show(ui, |ui| {
        egui::Grid::new("lpkt_g").num_columns(6).striped(true).show(ui, |ui| {
            for h in ["#", "PID", "PUSI", "AFC", "CC", "前 16 字节"] {
                ui.label(RichText::new(h).strong().size(11.0));
            }
            ui.end_row();
            let start = core.ring.len().saturating_sub(100);
            for (idx, pkt) in core.ring.iter().skip(start).enumerate() {
                let info = tsfile::packet_info(pkt);
                ui.label((start + idx).to_string());
                ui.label(format!("0x{:04X}", info.pid));
                ui.label(if info.pusi { "●" } else { "" });
                ui.label(info.afc.to_string());
                ui.label(info.cc.to_string());
                ui.label(
                    RichText::new(hex16(&pkt[..16.min(pkt.len())]))
                        .monospace()
                        .size(10.0),
                );
                ui.end_row();
            }
        });
    });
}

fn tr290_tab(ui: &mut Ui, items: &[Tr290Item]) {
    use egui_extras::{Column, TableBuilder};
    let counts: std::collections::HashMap<&str, u64> =
        items.iter().map(|i| (i.name.as_str(), i.count)).collect();
    let firsts: std::collections::HashMap<&str, &str> =
        items.iter().map(|i| (i.name.as_str(), i.first.as_str())).collect();
    ui.label(
        RichText::new("依据 GY/T 299.1——三级 17 项")
            .size(11.0)
            .color(theme::FAINT),
    );
    egui::ScrollArea::vertical().id_source("tr290").show(ui, |ui| {
        for (level, title, rows) in [
            (1u8, "一级错误（基本信息丢失，秒级致断）", &TR_LEVEL1[..]),
            (2, "二级错误（单个错误，影响传输质量）", &TR_LEVEL2[..]),
            (3, "三级错误（轻微/辅助错误）", &TR_LEVEL3[..]),
        ] {
            ui.add_space(4.0);
            // push_id：三张 TableBuilder 共享同一父 ui.id()，需独立命名空间
            ui.push_id(level, |ui| {
                theme::panel(ui, title, |ui| {
                    // TableBuilder：末列 remainder 吃满剩余宽度 → 表格通栏（设计稿口径）
                    TableBuilder::new(ui)
                        .striped(true)
                        .vscroll(false)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .column(Column::auto())
                        .column(Column::auto())
                        .column(Column::auto())
                        .column(Column::auto().at_least(48.0))
                        .column(Column::remainder())
                        .header(22.0, |mut h| {
                            for t in ["级别", "检测项", "状态", "次数", "最近发生 / 影响"] {
                                h.col(|ui| {
                                    ui.label(RichText::new(t).size(10.5).color(theme::FAINT));
                                });
                            }
                        })
                        .body(|mut body| {
                            for (disp, key, impact) in rows {
                                let cnt = counts.get(key).copied().unwrap_or(0);
                                body.row(26.0, |mut r| {
                                    r.col(|ui| crate::widgets::lv_badge(ui, level));
                                    r.col(|ui| {
                                        ui.label(*disp);
                                    });
                                    r.col(|ui| {
                                        if cnt == 0 {
                                            ui.label(
                                                RichText::new("正常").size(12.0).color(theme::OK),
                                            );
                                        } else {
                                            crate::widgets::pill(
                                                ui,
                                                &format!("{} 次", cnt),
                                                theme::WARN,
                                            );
                                        }
                                    });
                                    r.col(|ui| {
                                        ui.label(
                                            RichText::new(cnt.to_string())
                                                .monospace()
                                                .color(if cnt == 0 { theme::MUT } else { theme::WARN }),
                                        );
                                    });
                                    r.col(|ui| {
                                        if cnt == 0 {
                                            ui.label(
                                                RichText::new(*impact).size(11.5).color(theme::FAINT),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} · {}",
                                                    firsts.get(key).copied().unwrap_or("—"),
                                                    impact
                                                ))
                                                .size(11.5)
                                                .color(theme::MUT),
                                            );
                                        }
                                    });
                                });
                            }
                        });
                });
            });
            ui.add_space(8.0);
        }
    });
}

fn logs_tab(ui: &mut Ui, log: &VecDeque<String>, res: &ScanResult) {
    ui.horizontal(|ui| {
        if ui.button("导出 Markdown 报告").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .set_file_name("vqa_report.md")
                .save_file()
            {
                let _ = std::fs::write(&p, build_markdown(res));
            }
        }
    });
    ui.separator();
    egui::ScrollArea::vertical().id_source("logs").show(ui, |ui| {
        for l in log {
            ui.label(RichText::new(l.as_str()).monospace().size(11.0));
        }
    });
}

/// Markdown 报告（Python 版口径的精简移植）。
fn build_markdown(res: &ScanResult) -> String {
    let (mut n1, mut n2, mut n3) = (0u64, 0u64, 0u64);
    for it in &res.tr290 {
        match it.level {
            1 => n1 += it.count,
            2 => n2 += it.count,
            _ => n3 += it.count,
        }
    }
    let mut m = String::new();
    m.push_str("# TS 流分析报告\n\n");
    m.push_str(&format!("- 文件：{}\n", res.path));
    m.push_str(&format!("- 大小：{} bytes\n", res.file_size));
    m.push_str(&format!("- 包长：{} / 包数：{}\n", res.packet_size, res.packets));
    m.push_str(&format!(
        "- PCR 时长：{:.3} s · 平均码率：{:.0} bps\n",
        res.duration_pcr(),
        res.bitrate_avg()
    ));
    m.push_str(&format!(
        "- TR 101 290：一级 {n1} / 二级 {n2} / 三级 {n3}\n\n"
    ));
    m.push_str("## 节目结构\n\n");
    for p in &res.programs {
        m.push_str(&format!(
            "- 节目 {}（PMT 0x{:04X}，PCR 0x{:04X}）\n",
            p.number, p.pmt_pid, p.pcr_pid
        ));
        for s in &p.streams {
            m.push_str(&format!("  - 0x{:04X} {}（{}）\n", s.pid, s.type_name(), s.kind));
        }
    }
    m.push_str("\n## TR 101 290 明细\n\n");
    m.push_str("| 级别 | 项目 | 次数 | 最近发生 | 说明 |\n|---|---|---|---|---|\n");
    for it in &res.tr290 {
        m.push_str(&format!(
            "| P{} | {} | {} | {} | {} |\n",
            it.level, it.name, it.count, it.first, it.detail
        ));
    }
    m
}

fn hex16(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}
