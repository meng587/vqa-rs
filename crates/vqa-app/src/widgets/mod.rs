//! 公共小组件 —— SourceBar / NicSelector / 状态灯 / 设计系统控件。

pub mod nic_selector;
pub mod source_bar;
pub mod status;

use egui::{Color32, Rect, RichText, Sense, Ui, Vec2};

use crate::theme;

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

/// 一行「字段名 + 值」的信息条目。
pub fn kv(ui: &mut Ui, k: &str, v: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).size(12.0).color(theme::MUT));
        ui.label(RichText::new(v.into()).size(12.0));
    });
}

/// 统计卡片：左缘强调条 + 标签 + 大数字 + 说明（方案 A 卡片）。
#[allow(dead_code)]
pub fn stat_card(ui: &mut Ui, label: &str, value: impl Into<String>, color: Color32) {
    stat_card_sub(ui, label, value, "", color)
}

/// 带说明行的统计卡片。
/// 行高预算（66px）：标签 6..19 · 值 21..43（18px 字）· 说明 47..60——三段互不重叠。
pub fn stat_card_sub(ui: &mut Ui, label: &str, value: impl Into<String>, sub: &str, color: Color32) {
    let (rect, _) = ui.allocate_at_least(Vec2::new(112.0, 66.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, theme::PANEL);
    p.rect_stroke(rect, 8.0, egui::Stroke::new(1.0_f32, theme::BORDER));
    // 左缘强调条
    p.rect_filled(
        Rect::from_min_size(
            rect.left_top() + Vec2::new(0.0, 8.0),
            Vec2::new(3.0, rect.height() - 16.0),
        ),
        1.5,
        color,
    );
    let pad = 12.0;
    p.text(
        rect.left_top() + Vec2::new(pad, 6.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::proportional(10.5),
        theme::FAINT,
    );
    p.text(
        rect.left_top() + Vec2::new(pad, 21.0),
        egui::Align2::LEFT_TOP,
        value.into(),
        egui::FontId::proportional(18.0),
        color,
    );
    if !sub.is_empty() {
        p.text(
            rect.left_bottom() + Vec2::new(pad, -5.0),
            egui::Align2::LEFT_BOTTOM,
            sub,
            egui::FontId::proportional(10.5),
            theme::MUT,
        );
    }
}

/// 信息 chip：圆角描边小标签。
pub fn chip(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    let t = text.into();
    let font = egui::FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(t.clone(), font.clone(), color);
    let size = galley.size() + Vec2::new(20.0, 9.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 99.0, theme::PANEL);
    p.rect_stroke(rect, 99.0, egui::Stroke::new(1.0_f32, theme::BORDER));
    p.text(rect.center(), egui::Align2::CENTER_CENTER, t, font, color);
}

/// 级别徽章 P1/P2/P3（TR290 表）。
pub fn lv_badge(ui: &mut Ui, level: u8) {
    let (bg, fg, t) = match level {
        1 => (
            Color32::from_rgba_premultiplied(64, 22, 22, 255),
            theme::BAD,
            "P1",
        ),
        2 => (
            Color32::from_rgba_premultiplied(64, 51, 12, 255),
            theme::WARN,
            "P2",
        ),
        _ => (
            Color32::from_rgba_premultiplied(38, 44, 52, 255),
            theme::FAINT,
            "P3",
        ),
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(28.0, 18.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 5.0, bg);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        t,
        egui::FontId::monospace(10.0),
        fg,
    );
}

/// 状态 pill（「199 次」「未出现」等）。
pub fn pill(ui: &mut Ui, text: &str, color: Color32) {
    let bg = Color32::from_rgba_premultiplied(
        (color.r() as f32 * 0.16) as u8,
        (color.g() as f32 * 0.16) as u8,
        (color.b() as f32 * 0.16) as u8,
        255,
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(58.0, 20.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 99.0, bg);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(10.5),
        color,
    );
}

/// 信息 chip：图标 + 圆角描边小标签（设计稿 chips 带 lucide 小图标）。
pub fn chip_icon(ui: &mut Ui, icon: Option<Icon>, text: impl Into<String>, color: Color32) {
    let t = text.into();
    let font = egui::FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(t.clone(), font.clone(), color);
    let ic_w = if icon.is_some() { 15.0 } else { 0.0 };
    let size = galley.size() + Vec2::new(20.0 + ic_w, 9.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 99.0, theme::PANEL);
    p.rect_stroke(rect, 99.0, egui::Stroke::new(1.0, theme::BORDER));
    if let Some(ic) = icon {
        ic.draw(&p, rect.left_center() + Vec2::new(8.0 + ic_w * 0.5, 0.0), color);
    }
    p.text(
        rect.right_center() - Vec2::new(10.0 + 0.0, 0.0) - Vec2::new(0.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        t,
        font,
        color,
    );
}

// ---------------------------------------------------------------- 图标按钮

/// 简单矢量图标（painter 直绘，不依赖图标字体）。
#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Record,
    Camera,
    Volume,
    Folder,
    Scan,
    // 侧栏导航图标（对应设计稿 lucide：monitor-play / activity / shield-check / radio-tower / info）
    NavPlayer,
    NavActivity,
    NavShield,
    NavTower,
    NavInfo,
    // chips 小图标（gauge / wifi / check / audio-lines）
    Gauge,
    Wifi,
    Check,
    Audio,
}

impl Icon {
    /// 放大绘制（大播放键等场景），scale=1 与 icon_button 等大。
    pub fn draw_big(&self, p: &egui::Painter, c: egui::Pos2, color: Color32, scale: f32) {
        let s = 5.0 * scale;
        self.draw_impl(p, c, color, s);
    }

    fn draw(&self, p: &egui::Painter, c: egui::Pos2, color: Color32) {
        self.draw_impl(p, c, color, 5.0);
    }

    /// 供外部（如导航行）以自定义倍率绘制（scale=1 与 icon_button 等大）。
    pub fn draw_impl_pub(&self, p: &egui::Painter, c: egui::Pos2, color: Color32, scale: f32) {
        self.draw_impl(p, c, color, 5.0 * scale);
    }

    fn draw_impl(&self, p: &egui::Painter, c: egui::Pos2, color: Color32, s: f32) {
        match self {
            // 侧栏小图标（描边风格，lucide 近似）
            Icon::NavPlayer => {
                let r = Rect::from_center_size(c, Vec2::new(2.0 * s, 1.5 * s));
                p.rect_stroke(r, 2.0, egui::Stroke::new(1.3, color));
                p.add(egui::Shape::convex_polygon(
                    vec![
                        c + Vec2::new(-0.3 * s, -0.4 * s),
                        c + Vec2::new(-0.3 * s, 0.4 * s),
                        c + Vec2::new(0.5 * s, 0.0),
                    ],
                    color,
                    egui::Stroke::NONE,
                ));
            }
            Icon::NavActivity => {
                let pts = [
                    c + Vec2::new(-s, 0.2 * s),
                    c + Vec2::new(-0.55 * s, 0.2 * s),
                    c + Vec2::new(-0.3 * s, -0.7 * s),
                    c + Vec2::new(0.0, 0.8 * s),
                    c + Vec2::new(0.3 * s, -0.4 * s),
                    c + Vec2::new(0.55 * s, 0.2 * s),
                    c + Vec2::new(s, 0.2 * s),
                ];
                let stroke = egui::Stroke::new(1.4, color);
                for w in pts.windows(2) {
                    p.line_segment([w[0], w[1]], stroke);
                }
            }
            Icon::NavShield => {
                p.add(egui::Shape::convex_polygon(
                    vec![
                        c + Vec2::new(-0.8 * s, -0.8 * s),
                        c + Vec2::new(0.8 * s, -0.8 * s),
                        c + Vec2::new(0.8 * s, 0.2 * s),
                        c + Vec2::new(0.0, 0.95 * s),
                        c + Vec2::new(-0.8 * s, 0.2 * s),
                    ],
                    Color32::TRANSPARENT,
                    egui::Stroke::new(1.3, color),
                ));
                p.line_segment(
                    [c + Vec2::new(-0.3 * s, 0.0), c + Vec2::new(-0.05 * s, 0.3 * s)],
                    egui::Stroke::new(1.3, color),
                );
                p.line_segment(
                    [c + Vec2::new(-0.05 * s, 0.3 * s), c + Vec2::new(0.4 * s, -0.3 * s)],
                    egui::Stroke::new(1.3, color),
                );
            }
            Icon::NavTower => {
                p.line_segment(
                    [
                        c + Vec2::new(0.0, -0.9 * s),
                        c + Vec2::new(-0.55 * s, 0.95 * s),
                    ],
                    egui::Stroke::new(1.3, color),
                );
                p.line_segment(
                    [
                        c + Vec2::new(0.0, -0.9 * s),
                        c + Vec2::new(0.55 * s, 0.95 * s),
                    ],
                    egui::Stroke::new(1.3, color),
                );
                p.line_segment(
                    [
                        c + Vec2::new(-0.32 * s, 0.2 * s),
                        c + Vec2::new(0.32 * s, 0.2 * s),
                    ],
                    egui::Stroke::new(1.3, color),
                );
                p.circle_filled(c + Vec2::new(0.0, -0.9 * s), 0.18 * s, color);
            }
            Icon::NavInfo => {
                p.circle_stroke(c, 0.9 * s, egui::Stroke::new(1.3, color));
                p.circle_filled(c + Vec2::new(0.0, -0.4 * s), 0.12 * s, color);
                p.line_segment(
                    [c + Vec2::new(0.0, 0.0), c + Vec2::new(0.0, 0.45 * s)],
                    egui::Stroke::new(1.4, color),
                );
            }
            Icon::Play => {
                let s = s * 0.62; // 三角形略收，视觉居中
                p.add(egui::Shape::convex_polygon(
                    vec![
                        c + Vec2::new(-s * 0.9, -s),
                        c + Vec2::new(-s * 0.9, s),
                        c + Vec2::new(s, 0.0),
                    ],
                    color,
                    egui::Stroke::NONE,
                ));
            }
            Icon::Pause => {
                for dx in [-2.4f32, 2.4] {
                    p.rect_filled(
                        Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(2.2, 2.0 * s)),
                        1.0,
                        color,
                    );
                }
            }
            Icon::Stop => {
                p.rect_filled(
                    Rect::from_center_size(c, Vec2::new(1.4 * s, 1.4 * s)),
                    2.0,
                    color,
                );
            }
            Icon::Record => {
                p.circle_filled(c, 3.6, color);
            }
            Icon::Camera => {
                p.rect_filled(
                    Rect::from_center_size(c, Vec2::new(1.9 * s, 1.35 * s)),
                    2.0,
                    color,
                );
                p.rect_filled(
                    Rect::from_center_size(
                        c + Vec2::new(0.0, -1.15 * s),
                        Vec2::new(0.6 * s, 0.5 * s),
                    ),
                    1.0,
                    color,
                );
                p.circle_stroke(c, 0.45 * s, egui::Stroke::new(1.4_f32, theme::PANEL));
            }
            Icon::Volume => {
                p.add(egui::Shape::convex_polygon(
                    vec![
                        c + Vec2::new(-s, -0.45 * s),
                        c + Vec2::new(-s, 0.45 * s),
                        c + Vec2::new(-0.2 * s, 0.45 * s),
                        c + Vec2::new(0.45 * s, s * 0.9),
                        c + Vec2::new(0.45 * s, -s * 0.9),
                        c + Vec2::new(-0.2 * s, -0.45 * s),
                    ],
                    color,
                    egui::Stroke::NONE,
                ));
                p.circle_stroke(
                    c + Vec2::new(0.6 * s, 0.0),
                    0.85 * s,
                    egui::Stroke::new(1.3_f32, color),
                );
            }
            Icon::Folder => {
                p.rect_stroke(
                    Rect::from_center_size(
                        c + Vec2::new(0.0, 0.15 * s),
                        Vec2::new(1.9 * s, 1.2 * s),
                    ),
                    1.5,
                    egui::Stroke::new(1.4_f32, color),
                );
                p.rect_filled(
                    Rect::from_center_size(
                        c + Vec2::new(-0.55 * s, -0.55 * s),
                        Vec2::new(0.9 * s, 0.4 * s),
                    ),
                    1.0,
                    color,
                );
            }
            Icon::Scan => {
                p.circle_stroke(c, 0.85 * s, egui::Stroke::new(1.4, color));
                p.line_segment(
                    [c + Vec2::new(-1.3 * s, 0.0), c + Vec2::new(-0.5 * s, 0.0)],
                    egui::Stroke::new(1.4, color),
                );
                p.line_segment(
                    [c + Vec2::new(0.5 * s, 0.0), c + Vec2::new(1.3 * s, 0.0)],
                    egui::Stroke::new(1.4, color),
                );
                p.line_segment(
                    [c + Vec2::new(0.0, -1.3 * s), c + Vec2::new(0.0, -0.5 * s)],
                    egui::Stroke::new(1.4, color),
                );
                p.line_segment(
                    [c + Vec2::new(0.0, 0.5 * s), c + Vec2::new(0.0, 1.3 * s)],
                    egui::Stroke::new(1.4, color),
                );
            }
            // chips 小图标
            Icon::Gauge => {
                p.circle_stroke(c, 0.85 * s, egui::Stroke::new(1.3, color));
                p.line_segment(
                    [c + Vec2::new(0.0, 0.1 * s), c + Vec2::new(0.55 * s, -0.45 * s)],
                    egui::Stroke::new(1.3, color),
                );
            }
            Icon::Wifi => {
                // 两道扇形弧近似 + 顶点
                p.line_segment(
                    [c + Vec2::new(-0.8 * s, -0.25 * s), c + Vec2::new(0.0, 0.35 * s)],
                    egui::Stroke::new(1.3, color),
                );
                p.line_segment(
                    [c + Vec2::new(0.8 * s, -0.25 * s), c + Vec2::new(0.0, 0.35 * s)],
                    egui::Stroke::new(1.3, color),
                );
                p.line_segment(
                    [c + Vec2::new(-0.4 * s, 0.15 * s), c + Vec2::new(0.0, 0.45 * s)],
                    egui::Stroke::new(1.3, color),
                );
                p.line_segment(
                    [c + Vec2::new(0.4 * s, 0.15 * s), c + Vec2::new(0.0, 0.45 * s)],
                    egui::Stroke::new(1.3, color),
                );
                p.circle_filled(c + Vec2::new(0.0, 0.7 * s), 0.16 * s, color);
            }
            Icon::Check => {
                p.line_segment(
                    [c + Vec2::new(-0.7 * s, 0.05 * s), c + Vec2::new(-0.15 * s, 0.6 * s)],
                    egui::Stroke::new(1.5, color),
                );
                p.line_segment(
                    [c + Vec2::new(-0.15 * s, 0.6 * s), c + Vec2::new(0.75 * s, -0.55 * s)],
                    egui::Stroke::new(1.5, color),
                );
            }
            Icon::Audio => {
                for (dx, h) in [(-0.65, 0.45), (0.0, 0.85), (0.65, 0.35)] {
                    p.line_segment(
                        [c + Vec2::new(dx * s, -h * s), c + Vec2::new(dx * s, h * s)],
                        egui::Stroke::new(1.4, color),
                    );
                }
            }
        }
    }
}

/// 28×28 图标按钮。
pub fn icon_button(ui: &mut Ui, icon: Icon) -> egui::Response {
    icon_button_tinted(ui, icon, theme::MUT)
}

pub fn icon_button_tinted(ui: &mut Ui, icon: Icon, tint: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
    let p = ui.painter_at(rect);
    if resp.hovered() {
        p.rect_filled(rect, 7.0, Color32::from_rgba_premultiplied(34, 42, 52, 255));
    }
    let fg = if resp.hovered() { theme::TEXT } else { tint };
    icon.draw(&p, rect.center(), fg);
    resp
}

/// 自绘进度/拖动条（占满剩余宽度；直播流不可拖时 disabled）。
/// 返回 Some(new_frac) 表示用户拖动到新位置。
#[allow(dead_code)]
pub fn seek_bar(ui: &mut Ui, frac: f32, enabled: bool) -> Option<f32> {
    let w = ui.available_width();
    seek_bar_at(ui, w, frac, enabled)
}

/// 定宽版本（控制条里给右侧控件簇让位，防溢出压行）。
pub fn seek_bar_at(ui: &mut Ui, width: f32, frac: f32, enabled: bool) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(width.max(40.0), 14.0),
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let p = ui.painter_at(rect);
    let track = Rect::from_min_size(
        rect.left_top() + Vec2::new(0.0, 4.5),
        Vec2::new(rect.width(), 5.0),
    );
    p.rect_filled(track, 99.0, Color32::from_rgba_premultiplied(46, 56, 68, 255));
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.005 {
        p.rect_filled(
            Rect::from_min_size(track.left_top(), Vec2::new(track.width() * frac, 5.0)),
            99.0,
            theme::ACC,
        );
    }
    if !enabled {
        return None;
    }
    let x = match resp.interact_pointer_pos() {
        Some(pos) => ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        None => return None,
    };
    if resp.dragged() || resp.clicked() {
        Some(x)
    } else {
        None
    }
}
