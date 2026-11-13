//! 主题 —— `ui/Style.py` 的语义色板与字号常量。
//!
//! 「唯一色源」纪律同 Python 版：页面禁止硬编码色值，一律引这里。

use egui::{Color32, RichText};

pub const PRIMARY: Color32 = Color32::from_rgb(0x25, 0x63, 0xeb); // #2563eb
pub const DANGER: Color32 = Color32::from_rgb(0xdc, 0x26, 0x26); // #dc2626
pub const OK: Color32 = Color32::from_rgb(0x16, 0xa3, 0x4a); // #16a34a
pub const WARN: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b); // #f59e0b
pub const BODY: Color32 = Color32::from_rgb(0x1f, 0x29, 0x37);
pub const SUB: Color32 = Color32::from_rgb(0x6b, 0x72, 0x80);
pub const BORDER: Color32 = Color32::from_rgb(0xe5, 0xe7, 0xeb);
pub const CARD_BG: Color32 = Color32::from_rgb(0xf8, 0xfa, 0xfc);
pub const NAV_BG: Color32 = Color32::from_rgb(0xf3, 0xf4, 0xf6);
pub const DARK: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15); // 播放器沉浸区

/// 结论色：✓ 绿 / ？橙 / ✗ 红 / — 灰
#[allow(dead_code)]
pub fn verdict_color(v: &str) -> Color32 {
    match v {
        "✓" => OK,
        "？" | "?" => WARN,
        "✗" => DANGER,
        _ => SUB,
    }
}

#[allow(dead_code)]
pub fn verdict(text: &str, color: Color32) -> RichText {
    RichText::new(text).strong().color(color)
}

/// 应用全局样式（字体大小、控件圆角、间距）。
pub fn install(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let vs = &mut style.visuals;
    vs.window_rounding = egui::Rounding::same(6.0);
    vs.widgets.noninteractive.rounding = egui::Rounding::same(4.0);
    vs.widgets.inactive.rounding = egui::Rounding::same(4.0);
    vs.widgets.hovered.rounding = egui::Rounding::same(4.0);
    vs.widgets.active.rounding = egui::Rounding::same(4.0);
    vs.widgets.inactive.bg_fill = Color32::WHITE;
    vs.widgets.inactive.weak_bg_fill = Color32::WHITE;
    vs.widgets.inactive.fg_stroke.color = BODY;
    vs.selection.stroke.color = PRIMARY;
    vs.selection.bg_fill = PRIMARY.gamma_multiply(0.25);
    ctx.set_style(style);
}
