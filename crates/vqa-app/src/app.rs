//! 主窗口 —— 侧边导航 + 页面切换（`app_qt_new.py` MainWindow 的移植，
//! 视觉方案 A「播控室暗色」）。
//!
//! 约定：
//! * 导航行兼作各页状态灯（`set_nav_state`）；后续版本页以「三期」徽标占位。
//! * 页面切换是轻操作：不销毁后台线程/子进程（对齐 Python 版 cleanup()
//!   ≠ shutdown() 的分层）。播放器页切走时隐藏视频子窗，回来即恢复。

use eframe::Frame;
use egui::{Color32, Context, FontId, RichText, Vec2};

use crate::pages::{analysis::AnalysisPage, player::PlayerPage};
use crate::theme;
use crate::widgets::status::PageState;

pub const NAV_WIDTH: f32 = 196.0;

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
    nav_states: [PageState; 3],
    main_hwnd: Option<isize>,
}

impl VqaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        // 中文字体：接入系统微软雅黑（egui 默认字体缺 CJK）
        let mut fonts = egui::FontDefinitions::default();
        for fam in ["Microsoft YaHei UI", "Microsoft YaHei", "SimSun"] {
            if let Some(p) = font_path(fam) {
                if let Ok(data) = std::fs::read(&p) {
                    fonts.font_data.insert(
                        format!("cjk-{fam}"),
                        egui::FontData::from_owned(data).into(),
                    );
                    for slot in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
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
        crate::widgets::lucide::install(&mut fonts);
        cc.egui_ctx.set_fonts(fonts);

        Self {
            page: PageId::Player,
            player: PlayerPage::new(),
            analysis: AnalysisPage::new(),
            nav_states: [PageState::Idle; 3],
            main_hwnd: None,
        }
    }

    pub fn set_nav_state(&mut self, page: PageId, st: PageState) {
        self.nav_states[page as usize] = st;
    }
}

fn font_path(family: &str) -> Option<std::path::PathBuf> {
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

        self.set_nav_state(PageId::Player, self.player.nav_state());
        self.set_nav_state(PageId::Analysis, self.analysis.nav_state());

        egui::SidePanel::left("nav")
            .exact_width(NAV_WIDTH)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL2)
                    .inner_margin(egui::Margin::same(10.0)),
            )
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.add_space(14.0);
                    // 品牌块
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        let logo = egui::Rect::from_min_size(
                            ui.cursor().left_top(),
                            Vec2::new(34.0, 34.0),
                        );
                        ui.allocate_rect(logo, egui::Sense::hover());
                        let p = ui.painter_at(logo);
                        // 品牌渐变方块（indigo→violet 对角，现代化签名元素）
                        let mut lg = egui::Mesh::default();
                        lg.reserve_triangles(2);
                        lg.colored_vertex(logo.left_top(), Color32::from_rgb(0x63, 0x66, 0xf1));
                        lg.colored_vertex(logo.right_top(), Color32::from_rgb(0x8b, 0x5c, 0xf6));
                        lg.colored_vertex(logo.left_bottom(), Color32::from_rgb(0x8b, 0x5c, 0xf6));
                        lg.colored_vertex(logo.right_bottom(), Color32::from_rgb(0xa8, 0x55, 0xf7));
                        lg.add_triangle(0, 1, 2);
                        lg.add_triangle(1, 3, 2);
                        p.add(lg);
                        p.text(
                            logo.center(),
                            egui::Align2::CENTER_CENTER,
                            "◉",
                            FontId::proportional(15.0),
                            Color32::WHITE,
                        );
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new("VQA").size(14.0).strong().color(theme::TEXT));
                            ui.label(
                                RichText::new("Workbench v0.2")
                                    .size(10.0)
                                    .color(theme::FAINT),
                            );
                        });
                    });
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // 功能导航（自绘行：lucide 图标 + 激活态左缘条 + 强调底色）
                    for (name, id, icon) in [
                        ("播放器", PageId::Player, crate::widgets::lucide::Lucide::MonitorPlay),
                        ("视频分析", PageId::Analysis, crate::widgets::lucide::Lucide::Activity),
                    ] {
                        let selected = self.page == id;
                        let state = self.nav_states[id as usize];
                        let (rect, resp) = ui.allocate_exact_size(
                            [NAV_WIDTH - 20.0, 34.0].into(),
                            egui::Sense::click(),
                        );
                        ui.add_space(3.0);
                        let p = ui.painter_at(rect);
                        if selected {
                            p.rect_filled(rect, 8.0, theme::ACC_SOFT);
                            p.rect_filled(
                                egui::Rect::from_min_size(
                                    rect.left_top() + Vec2::new(-10.0, 8.0),
                                    Vec2::new(3.0, rect.height() - 16.0),
                                ),
                                2.0,
                                theme::ACC,
                            );
                        } else if resp.hovered() {
                            p.rect_filled(rect, 8.0, theme::ACC_SOFT);
                        }
                        let ic_color = if selected { theme::ACC } else { theme::MUT };
                        p.text(
                            egui::pos2(rect.left() + 12.0, rect.center().y),
                            egui::Align2::CENTER_CENTER,
                            icon.glyph(),
                            egui::FontId::new(16.0, egui::FontFamily::Name(crate::widgets::lucide::LUCIDE_FAMILY.into())),
                            ic_color,
                        );
                        p.text(
                            egui::pos2(rect.left() + 30.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            name,
                            FontId::proportional(13.0),
                            if selected { theme::ACC } else { theme::MUT },
                        );
                        // 运行状态灯（仅非空闲时显示）
                        if state != PageState::Idle {
                            let blink = ui.input(|i| i.time) % 1.0 < 0.5;
                            let c = match state {
                                PageState::Running if blink => state.color().gamma_multiply(0.55),
                                _ => state.color(),
                            };
                            p.circle_filled(
                                egui::pos2(rect.right() - 14.0, rect.center().y),
                                4.0,
                                c,
                            );
                        }
                        if resp.clicked() {
                            self.page = id;
                        }
                    }
                    // 后续版本占位（灰色图标 + 三期徽标）
                    for (name, icon) in [
                        ("合规检测", crate::widgets::lucide::Lucide::ShieldCheck),
                        ("直播转码", crate::widgets::lucide::Lucide::RadioTower),
                        ("微突发巡检", crate::widgets::lucide::Lucide::Activity),
                    ] {
                        let (rect, _) = ui.allocate_exact_size(
                            [NAV_WIDTH - 20.0, 34.0].into(),
                            egui::Sense::hover(),
                        );
                        ui.add_space(3.0);
                        let p = ui.painter_at(rect);
                        p.text(
                            egui::pos2(rect.left() + 12.0, rect.center().y),
                            egui::Align2::CENTER_CENTER,
                            icon.glyph(),
                            egui::FontId::new(16.0, egui::FontFamily::Name(crate::widgets::lucide::LUCIDE_FAMILY.into())),
                            theme::FAINT.gamma_multiply(0.7),
                        );
                        p.text(
                            egui::pos2(rect.left() + 30.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            name,
                            FontId::proportional(13.0),
                            theme::FAINT.gamma_multiply(0.7),
                        );
                        let badge = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 24.0, rect.center().y),
                            Vec2::new(30.0, 16.0),
                        );
                        p.rect_stroke(badge, 99.0, egui::Stroke::new(1.0_f32, theme::BORDER2));
                        p.text(
                            badge.center(),
                            egui::Align2::CENTER_CENTER,
                            "三期",
                            FontId::proportional(9.0),
                            theme::FAINT,
                        );
                    }
                    ui.add_space(6.0);
                    ui.separator();

                    // 关于（常规导航项）
                    let (rect, resp) = ui.allocate_exact_size(
                        [NAV_WIDTH - 20.0, 34.0].into(),
                        egui::Sense::click(),
                    );
                    ui.add_space(3.0);
                    let selected = self.page == PageId::About;
                    let p = ui.painter_at(rect);
                    if selected {
                        p.rect_filled(rect, 8.0, theme::ACC_SOFT);
                        p.rect_filled(
                            egui::Rect::from_min_size(
                                rect.left_top() + Vec2::new(-10.0, 8.0),
                                Vec2::new(3.0, rect.height() - 16.0),
                            ),
                            2.0,
                            theme::ACC,
                        );
                    } else if resp.hovered() {
                        p.rect_filled(rect, 8.0, theme::ACC_SOFT);
                    }
                    let ic_color = if selected { theme::ACC } else { theme::MUT };
                    p.text(
                        egui::pos2(rect.left() + 12.0, rect.center().y),
                        egui::Align2::CENTER_CENTER,
                        crate::widgets::lucide::Lucide::Info.glyph(),
                        egui::FontId::new(16.0, egui::FontFamily::Name(crate::widgets::lucide::LUCIDE_FAMILY.into())),
                        ic_color,
                    );
                    p.text(
                        egui::pos2(rect.left() + 30.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "关于",
                        FontId::proportional(13.0),
                        if selected { theme::ACC } else { theme::MUT },
                    );
                    if resp.clicked() {
                        self.page = PageId::About;
                    }

                    // 侧栏底部：引擎状态
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            let blink = ui.input(|i| i.time) % 1.0 < 0.5;
                            ui.painter().circle_filled(
                                ui.cursor().left_top() + Vec2::new(4.0, 8.0),
                                4.0,
                                if blink { theme::OK } else { theme::OK.gamma_multiply(0.55) },
                            );
                            ui.add_space(12.0);
                            ui.label(RichText::new("引擎就绪").size(11.0).color(theme::MUT));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new("mpv 0.41").size(10.0).color(theme::FAINT),
                                    );
                                },
                            );
                        });
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG).inner_margin(14.0))
            .show(ctx, |ui| match self.page {
                PageId::Player => self.player.ui(ctx, ui),
                PageId::Analysis => self.analysis.ui(ctx, ui),
                PageId::About => about_ui(ui),
            });

        // 播放器页不在前台时隐藏视频子窗（页面状态保留，回来即恢复）
        self.player.set_child_visible(self.page == PageId::Player);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.player.shutdown();
        self.analysis.shutdown();
    }
}

fn about_ui(ui: &mut egui::Ui) {
    ui.label(RichText::new("关于").size(16.0).strong());
    ui.add_space(8.0);
    theme::panel(ui, "版本", |ui| {
        ui.label(format!(
            "视听质量检测工作站 v{}（Rust 版）",
            env!("CARGO_PKG_VERSION")
        ));
        ui.add_space(4.0);
        ui.label(RichText::new("首期交付：播放器 / 视频分析。其余功能页按路线迭代。")
            .size(12.0)
            .color(theme::MUT));
    });
    ui.add_space(8.0);
    theme::panel(ui, "工具链", |ui| {
        for (k, v) in [
            ("ffmpeg", vqa_core::tools::find_ffmpeg()),
            ("ffprobe", vqa_core::tools::find_ffprobe()),
            ("mpv", vqa_core::tools::find_mpv()),
            ("davs2", vqa_core::tools::find_davs2()),
        ] {
            let v = v
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "未找到".into());
            crate::widgets::kv(ui, k, v);
        }
    });
    let _ = Color32::WHITE;
}
