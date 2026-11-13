//! 状态灯 —— `StatusIndicator.py` 的移植（导航行与页面共用）。

use egui::{Color32, Ui};

pub const COLOR_IDLE: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
pub const COLOR_RUNNING: Color32 = Color32::from_rgb(0x16, 0xa3, 0x4a);
pub const COLOR_WARN: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
pub const COLOR_ERROR: Color32 = Color32::from_rgb(0xdc, 0x26, 0x26);

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // Warn 供后续页（合规/突发）使用
pub enum PageState {
    Idle,
    Running,
    Warn,
    Error,
}

impl PageState {
    pub fn color(self) -> Color32 {
        match self {
            PageState::Idle => COLOR_IDLE,
            PageState::Running => COLOR_RUNNING,
            PageState::Warn => COLOR_WARN,
            PageState::Error => COLOR_ERROR,
        }
    }
}

/// 画一个状态圆点。（导航栏当前使用内联绘制；此函数供后续页面复用）
#[allow(dead_code)]
pub fn dot(ui: &mut Ui, state: PageState) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    let blink = ui.input(|i| i.time) % 1.0 < 0.5;
    let color = match state {
        PageState::Running if blink => state.color().gamma_multiply(0.55),
        _ => state.color(),
    };
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// 圆点 + 文本。
#[allow(dead_code)]
pub fn indicator(ui: &mut Ui, state: PageState, label: &str) {
    ui.horizontal(|ui| {
        dot(ui, state);
        ui.label(label);
    });
}
