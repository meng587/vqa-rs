//! Lucide 图标字体 —— 设计稿所用图标体系，字形与设计稿 1:1。
//!
//! 字体经 `include_bytes!` 内嵌（assets/lucide.ttf，lucide-static 0.462），
//! 码点来自官方 iconfont.css。用法：`Lucide::Play.text(14.0)` 生成字形文本。

use egui::RichText;

pub const LUCIDE_TTF: &[u8] = include_bytes!("../../assets/lucide.ttf");
pub const LUCIDE_FAMILY: &str = "lucide";

/// 把 lucide 字体注册进 egui（挂为独立族，避免与正文字体互相污染）。
pub fn install(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        "lucide".into(),
        egui::FontData::from_static(LUCIDE_TTF).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Name(LUCIDE_FAMILY.into()))
        .or_default()
        .push("lucide".into());
}

#[derive(Clone, Copy, PartialEq)]
pub enum Lucide {
    Play,
    Pause,
    Square,
    CircleDot,
    Camera,
    Volume2,
    Folder,
    ScanEye,
    MonitorPlay,
    Activity,
    ShieldCheck,
    RadioTower,
    Info,
    Gauge,
    Wifi,
    Check,
    AudioLines,
    X,
    TriangleAlert,
    ScanLine,
    ChevronDown,
}

impl Lucide {
    fn cp(self) -> char {
        let cp: u32 = match self {
            Lucide::Play => 0xe140,
            Lucide::Pause => 0xe132,
            Lucide::Square => 0xe16b,
            Lucide::CircleDot => 0xe349,
            Lucide::Camera => 0xe068,
            Lucide::Volume2 => 0xe1ab,
            Lucide::Folder => 0xe0dc,
            Lucide::ScanEye => 0xe53b,
            Lucide::MonitorPlay => 0xe48a,
            Lucide::Activity => 0xe038,
            Lucide::ShieldCheck => 0xe1ff,
            Lucide::RadioTower => 0xe409,
            Lucide::Info => 0xe0fe,
            Lucide::Gauge => 0xe1bf,
            Lucide::Wifi => 0xe1ae,
            Lucide::Check => 0xe070,
            Lucide::AudioLines => 0xe55f,
            Lucide::X => 0xe1b2,
            Lucide::TriangleAlert => 0xe193,
            Lucide::ScanLine => 0xe258,
            Lucide::ChevronDown => 0xe071,
        };
        char::from_u32(cp).unwrap_or('\u{fffd}')
    }

    /// 字形字符（私有区码点）。
    pub fn glyph(self) -> String {
        self.cp().to_string()
    }

    /// 指定字号的颜色字形文本。
    pub fn text(self, size: f32, color: egui::Color32) -> RichText {
        RichText::new(self.cp().to_string())
            .font(egui::FontId::new(size, egui::FontFamily::Name(LUCIDE_FAMILY.into())))
            .color(color)
    }
}
