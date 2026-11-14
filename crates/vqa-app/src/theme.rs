//! 主题 —— 方案 A「播控室暗色」设计系统（设计稿 vqa-ui-redesign.html 落地）。
//!
//! 「唯一色源」纪律：页面禁止硬编码色值，一律引这里。

use egui::{Color32, RichText, Vec2};

// ---- 方案 A · 播控室暗色 ----
pub const BG: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x13); // 外围
pub const WIN: Color32 = Color32::from_rgb(0x10, 0x16, 0x1e); // 中央面板
pub const PANEL: Color32 = Color32::from_rgb(0x13, 0x1a, 0x23); // 卡片/面板
pub const PANEL2: Color32 = Color32::from_rgb(0x0d, 0x13, 0x19); // 侧栏/输入
pub const BORDER: Color32 = Color32::from_rgb(0x1f, 0x2a, 0x36);
pub const BORDER2: Color32 = Color32::from_rgb(0x28, 0x37, 0x4a);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xee, 0xf5);
pub const MUT: Color32 = Color32::from_rgb(0x8a, 0x99, 0xab);
pub const FAINT: Color32 = Color32::from_rgb(0x5b, 0x6b, 0x7d);
pub const ACC: Color32 = Color32::from_rgb(0x38, 0xbd, 0xf8); // 青蓝主色
/// 主色 14% 底（预乘：56/189/248 × 36/255 ≈ 8/27/35）
pub const ACC_SOFT: Color32 = Color32::from_rgba_premultiplied(8, 27, 35, 36);
pub const OK: Color32 = Color32::from_rgb(0x34, 0xd3, 0x99);
pub const WARN: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const BAD: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub const DARK: Color32 = Color32::from_rgb(0x05, 0x08, 0x0c); // 视频区深处

#[allow(dead_code)]
pub const MONO: &str = "Consolas";

/// 结论色：✓ 绿 / ？橙 / ✗ 红 / — 灰
#[allow(dead_code)]
pub fn verdict_color(v: &str) -> Color32 {
    match v {
        "✓" => OK,
        "？" | "?" => WARN,
        "✗" => BAD,
        _ => MUT,
    }
}

#[allow(dead_code)]
pub fn verdict(text: &str, color: Color32) -> RichText {
    RichText::new(text).strong().color(color)
}

#[allow(dead_code)]
pub fn small(text: impl Into<String>) -> RichText {
    RichText::new(&text.into()[..]).size(10.5).color(FAINT)
}

#[allow(dead_code)]
pub fn mut_text(text: impl Into<String>) -> RichText {
    RichText::new(&text.into()[..]).size(12.0).color(MUT)
}

/// 应用全局样式（暗色控件体系 + 字号阶 + 间距）。
pub fn install(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    let vs = &mut style.visuals;
    vs.override_text_color = Some(TEXT);
    vs.panel_fill = WIN;
    vs.window_fill = PANEL;
    vs.extreme_bg_color = PANEL2;
    vs.faint_bg_color = ACC_SOFT;
    vs.window_rounding = egui::Rounding::same(10.0);
    vs.window_stroke = egui::Stroke::new(1.0_f32, BORDER2);
    let r = egui::Rounding::same(8.0);
    vs.widgets.noninteractive.rounding = r;
    vs.widgets.noninteractive.bg_fill = PANEL;
    vs.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    vs.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    vs.widgets.inactive.rounding = r;
    vs.widgets.inactive.bg_fill = PANEL;
    vs.widgets.inactive.weak_bg_fill = PANEL;
    vs.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, MUT);
    vs.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER2);
    vs.widgets.hovered.rounding = r;
    vs.widgets.hovered.weak_bg_fill = ACC_SOFT;
    vs.widgets.hovered.bg_fill = ACC_SOFT;
    vs.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    vs.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACC.gamma_multiply(0.5));
    vs.widgets.active.rounding = r;
    vs.widgets.active.weak_bg_fill = ACC.gamma_multiply(0.85);
    vs.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
    vs.selection.bg_fill = ACC.gamma_multiply(0.35);
    vs.selection.stroke = egui::Stroke::new(1.0_f32, ACC);
    vs.text_cursor.stroke = egui::Stroke::new(1.6_f32, ACC);

    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 5.0);
    style.spacing.menu_margin = egui::Margin::symmetric(6.0, 6.0);

    style.text_styles = [
        (egui::TextStyle::Body, egui::FontId::proportional(13.5)),
        (egui::TextStyle::Button, egui::FontId::proportional(12.5)),
        (egui::TextStyle::Small, egui::FontId::proportional(10.5)),
        (egui::TextStyle::Heading, egui::FontId::proportional(16.0)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(12.0)),
    ]
    .into_iter()
    .collect();
    ctx.set_style(style);
}

/// 向白插值提亮（hover 态；egui gamma_multiply 只接受 ≤1.0 的压暗系数）。
pub fn lighten(c: Color32, f: f32) -> Color32 {
    let f = f.clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 + (255.0 - c.r() as f32) * f) as u8,
        (c.g() as f32 + (255.0 - c.g() as f32) * f) as u8,
        (c.b() as f32 + (255.0 - c.b() as f32) * f) as u8,
    )
}

/// 面板框（卡片/分组的统一容器）。
pub fn panel_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .rounding(10.0)
        .inner_margin(egui::Margin::same(12.0))
}

/// 带标题的面板。
pub fn panel(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    panel_frame().show(ui, |ui| {
        ui.label(RichText::new(title).size(12.0).strong().color(MUT));
        ui.add_space(6.0);
        add(ui);
    });
}
