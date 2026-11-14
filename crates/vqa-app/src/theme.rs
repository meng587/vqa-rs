//! 主题 —— 方案 A「播控室暗色」· 高对比修订版（vqa-ui-redesign.html 方案 A）。
//!
//! 「唯一色源」纪律：页面禁止硬编码色值，一律引这里。
//! 例外：视频卡片内部（播放器本色）保持深色，硬编码于 player.rs。
//!
//! 对比度修订（相对初版 A）：
//! * 文字三阶全部提亮：TEXT #f2f7fc / MUT #a9b8ca / FAINT #8496ab
//! * 面板与底色层次拉开：卡片 #17202c 明显浮于底色 #0a0e13
//! * 边框整体加强一档：BORDER #2a3745 / BORDER2 #3a4a5f

use egui::{Color32, RichText, Vec2};

// ---- 方案 A · 播控室暗色（高对比修订版）----
pub const BG: Color32 = Color32::from_rgb(0x09, 0x0d, 0x12); // 外围
pub const WIN: Color32 = Color32::from_rgb(0x10, 0x18, 0x24); // 中央面板
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x20, 0x2c); // 卡片/面板
pub const PANEL2: Color32 = Color32::from_rgb(0x0d, 0x14, 0x1e); // 侧栏/输入
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x37, 0x45);
pub const BORDER2: Color32 = Color32::from_rgb(0x3a, 0x4a, 0x5f);
pub const TEXT: Color32 = Color32::from_rgb(0xf2, 0xf7, 0xfc);
pub const MUT: Color32 = Color32::from_rgb(0xa9, 0xb8, 0xca);
pub const FAINT: Color32 = Color32::from_rgb(0x84, 0x96, 0xab);
pub const ACC: Color32 = Color32::from_rgb(0x4c, 0xc2, 0xf7); // 青蓝主色（提亮一档）
/// 主色 15% 底（预乘：76/194/247 × 38/255 ≈ 11/29/37）
pub const ACC_SOFT: Color32 = Color32::from_rgba_premultiplied(11, 29, 37, 38);
pub const OK: Color32 = Color32::from_rgb(0x3b, 0xdc, 0x9d);
pub const WARN: Color32 = Color32::from_rgb(0xfc, 0xc5, 0x2f);
pub const BAD: Color32 = Color32::from_rgb(0xfb, 0x7b, 0x7b);
/// 视频卡内部（播放器本色，恒为深色）
pub const DARK: Color32 = Color32::from_rgb(0x06, 0x09, 0x0d);

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
    vs.faint_bg_color = Color32::from_rgb(0x1a, 0x24, 0x31); // 条纹
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
    vs.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACC.gamma_multiply(0.45));
    vs.widgets.active.rounding = r;
    vs.widgets.active.weak_bg_fill = ACC;
    vs.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
    vs.selection.bg_fill = ACC.gamma_multiply(0.25);
    vs.selection.stroke = egui::Stroke::new(1.0_f32, ACC);
    vs.text_cursor.stroke = egui::Stroke::new(1.6_f32, ACC);

    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    // 全局钮高规范：普通钮/下拉闭态 = 12.5px 字行 + 8×2 padding ≈ 32px
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
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

/// 向白插值提亮。
pub fn lighten(c: Color32, f: f32) -> Color32 {
    let f = f.clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 + (255.0 - c.r() as f32) * f) as u8,
        (c.g() as f32 + (255.0 - c.g() as f32) * f) as u8,
        (c.b() as f32 + (255.0 - c.b() as f32) * f) as u8,
    )
}

/// 向黑插值压暗（浅色主题的 hover 态）。
pub fn darken(c: Color32, f: f32) -> Color32 {
    let f = f.clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 * (1.0 - f)) as u8,
        (c.g() as f32 * (1.0 - f)) as u8,
        (c.b() as f32 * (1.0 - f)) as u8,
    )
}

/// 面板框（卡片/分组的统一容器；白卡 + 描边 + 轻阴影）。
pub fn panel_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .rounding(10.0)
        .inner_margin(egui::Margin::same(12.0))
        .shadow(egui::epaint::Shadow {
            offset: Vec2::new(0.0, 1.0),
            blur: 3.0,
            ..Default::default()
        })
}

/// 带标题的面板。
pub fn panel(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    panel_frame().show(ui, |ui| {
        ui.label(RichText::new(title).size(12.0).strong().color(MUT));
        ui.add_space(6.0);
        add(ui);
    });
}
