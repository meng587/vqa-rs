//! 公共小组件 —— SourceBar / NicSelector / 状态灯（`ui/components/` 的移植）。

pub mod nic_selector;
pub mod source_bar;
pub mod status;

use egui::{RichText, Ui};

/// 清理串流地址 —— `url_utils.py` 的移植：全角中文标点 → ASCII、去空白；
/// 已存在的本地路径原样保留。
pub fn clean_stream_url(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || std::path::Path::new(s).exists() {
        return s.to_string();
    }
    s.chars()
        .map(|c| match c {
            '：' => ':',
            '＠' => '@',
            '，' => ',',
            '。' => '.',
            '？' => '?',
            '＝' => '=',
            '＆' => '&',
            '／' => '/',
            '＋' => '+',
            '％' => '%',
            '　' => ' ',
            _ => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect()
}

/// 一行「字段名 + 值」的信息条目（概览卡片用）。
pub fn kv(ui: &mut Ui, k: &str, v: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).color(crate::theme::SUB).size(12.0));
        ui.label(RichText::new(v.into()).size(12.0));
    });
}

/// 统计卡片：大数字 + 标签（`AnalysisPage_shared.make_card` 的移植）。
pub fn stat_card(ui: &mut Ui, label: &str, value: impl Into<String>, color: egui::Color32) {
    egui::Frame::default()
        .fill(crate::theme::CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, crate::theme::BORDER))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(10.0, 6.0))
        .show(ui, |ui| {
            ui.set_min_width(96.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(10.0).color(crate::theme::SUB));
                ui.label(RichText::new(value.into()).size(17.0).strong().color(color));
            });
        });
}
