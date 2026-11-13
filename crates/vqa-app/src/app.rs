//! 主窗口 —— 侧边导航 + 页面切换（`app_qt_new.py` MainWindow 的移植）。
//!
//! 约定：
//! * 导航行兼作各页状态灯（`set_nav_state`）；更新徽标挂在「关于」。
//! * 页面切换是轻操作：不销毁后台线程/子进程（对齐 Python 版 cleanup()
//!   ≠ shutdown() 的分层）。播放器页切走时隐藏视频子窗，回来即恢复。

use eframe::Frame;
use egui::Context;

use crate::pages::{analysis::AnalysisPage, player::PlayerPage};

pub const NAV_WIDTH: f32 = 150.0;
const NAV_ITEMS: [&str; 3] = ["播放器", "视频分析", "关于"];

#[derive(Clone, Copy, PartialEq)]
pub enum PageId {
    Player,
    Analysis,
    About,
}

pub struct VqaApp {
    page: PageId,
    pub player: PlayerPage,
    pub analysis: AnalysisPage,
    nav_states: [crate::widgets::status::PageState; 3],
    main_hwnd: Option<isize>,
}

impl VqaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::theme::install(&cc.egui_ctx);
        // 中文字体：接入系统微软雅黑（egui 默认字体缺 CJK）
        let mut fonts = egui::FontDefinitions::default();
        for fam in ["Microsoft YaHei UI", "Microsoft YaHei", "SimSun"] {
            if let Some(p) = font_path(fam) {
                if let Ok(data) = std::fs::read(&p) {
                    fonts.font_data.insert(
                        format!("cjk-{fam}"),
                        egui::FontData::from_owned(data).into(),
                    );
                    for slot in [
                        egui::FontFamily::Proportional,
                        egui::FontFamily::Monospace,
                    ] {
                        fonts
                            .families
                            .entry(slot)
                            .or_default()
                            .push(format!("cjk-{fam}"));
                    }
                    break;
                }
            }
        }
        cc.egui_ctx.set_fonts(fonts);

        Self {
            page: PageId::Player,
            player: PlayerPage::new(),
            analysis: AnalysisPage::new(),
            nav_states: [crate::widgets::status::PageState::Idle; 3],
            main_hwnd: None,
        }
    }

    pub fn set_nav_state(&mut self, page: PageId, st: crate::widgets::status::PageState) {
        self.nav_states[page as usize] = st;
    }
}

fn font_path(family: &str) -> Option<std::path::PathBuf> {
    // C:\Windows\Fonts\msyh.ttc / msyh.ttf / simsun.ttc
    let name = match family {
        "Microsoft YaHei UI" | "Microsoft YaHei" => "msyh.ttc",
        "SimSun" => "simsun.ttc",
        _ => return None,
    };
    let p = std::path::PathBuf::from(r"C:\Windows\Fonts").join(name);
    if p.is_file() {
        Some(p)
    } else {
        let alt = p.with_extension("ttf");
        alt.is_file().then_some(alt)
    }
}

impl eframe::App for VqaApp {
    fn update(&mut self, ctx: &Context, frame: &mut Frame) {
        // 首帧：拿主窗口 HWND 创建视频子窗
        if self.main_hwnd.is_none() {
            use raw_window_handle::HasWindowHandle as _;
            if let Ok(h) = frame.window_handle() {
                if let raw_window_handle::RawWindowHandle::Win32(w) = h.as_raw() {
                    self.main_hwnd = Some(w.hwnd.get() as isize);
                }
            }
        }
        if let Some(hwnd) = self.main_hwnd {
            self.player.ensure_child(hwnd);
        }

        // 状态灯同步（页面在 update 里上报）
        self.set_nav_state(PageId::Player, self.player.nav_state());
        self.set_nav_state(PageId::Analysis, self.analysis.nav_state());

        egui::SidePanel::left("nav")
            .exact_width(NAV_WIDTH)
            .frame(
                egui::Frame::none()
                    .fill(crate::theme::NAV_BG)
                    .inner_margin(egui::Margin::symmetric(0.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new("视听质量检测工作站")
                                .size(13.0)
                                .strong(),
                        );
                    });
                    ui.label(
                        egui::RichText::new("VQA Workbench · Rust")
                            .size(9.0)
                            .color(crate::theme::SUB),
                    );
                    ui.add_space(10.0);
                    for (i, name) in NAV_ITEMS.iter().enumerate() {
                        let id = match i {
                            0 => PageId::Player,
                            1 => PageId::Analysis,
                            _ => PageId::About,
                        };
                        let selected = self.page == id;
                        let state = self.nav_states[i];
                        use crate::theme::PRIMARY;
                        // 自绘导航行：状态灯 + 文本（DotWidget + NavRow 的移植）
                        let (rect, resp) = ui.allocate_exact_size(
                            [NAV_WIDTH - 12.0, 34.0].into(),
                            egui::Sense::click(),
                        );
                        if resp.hovered() || selected {
                            ui.painter().rect_filled(
                                rect,
                                4.0,
                                if selected {
                                    PRIMARY.gamma_multiply(0.14)
                                } else {
                                    egui::Color32::from_black_alpha(8)
                                },
                            );
                        }
                        if selected {
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    rect.left_top(),
                                    [3.0, rect.height()].into(),
                                ),
                                2.0,
                                PRIMARY,
                            );
                        }
                        let dot_center = egui::pos2(rect.left() + 16.0, rect.center().y);
                        let blink = ui.input(|i| i.time) % 1.0 < 0.5;
                        let color = match state {
                            crate::widgets::status::PageState::Running if blink => {
                                state.color().gamma_multiply(0.55)
                            }
                            _ => state.color(),
                        };
                        ui.painter()
                            .circle_filled(dot_center, 4.0, color);
                        ui.painter().text(
                            egui::pos2(rect.left() + 30.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            *name,
                            egui::FontId::proportional(13.5),
                            if selected { crate::theme::PRIMARY } else { crate::theme::BODY },
                        );
                        if resp.clicked() {
                            self.page = id;
                        }
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::WHITE).inner_margin(8.0))
            .show(ctx, |ui| match self.page {
                PageId::Player => self.player.ui(ctx, ui),
                PageId::Analysis => self.analysis.ui(ctx, ui),
                PageId::About => about_ui(ui),
            });

        // 播放器页不在前台时隐藏视频子窗（页面状态保留，回来即恢复）
        self.player
            .set_child_visible(self.page == PageId::Player);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.player.shutdown();
        self.analysis.shutdown();
    }
}

fn about_ui(ui: &mut egui::Ui) {
    ui.heading("关于");
    ui.add_space(8.0);
    ui.label(format!("版本 {} (Rust)", env!("CARGO_PKG_VERSION")));
    ui.add_space(6.0);
    for (k, v) in [
        ("ffmpeg", vqa_core::tools::find_ffmpeg()),
        ("ffprobe", vqa_core::tools::find_ffprobe()),
        ("mpv", vqa_core::tools::find_mpv()),
        ("davs2", vqa_core::tools::find_davs2()),
    ] {
        let v = v.map(|p| p.display().to_string())
            .unwrap_or_else(|| "未找到".into());
        crate::widgets::kv(ui, k, v);
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("首期交付：播放器 / 视频分析。其余功能页按计划迭代。")
            .color(crate::theme::SUB),
    );
}
