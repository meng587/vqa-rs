//! 来源栏 —— `SourceBar.py` 的移植：URL 历史 + 网卡 + 浏览 + 开始/停止。
//!
//! 信号用返回值表达（egui 即时模式无需信号槽）：
//! [`SourceBar::show`] 返回动作枚举，页面据此启动/停止。

#[derive(Debug, Clone, PartialEq)]
pub enum SourceAction {
    None,
    Start { url: String, nic_ip: String },
    Stop,
}

pub struct SourceBar {
    pub key: String, // 历史键（shared config recent_sources.<key>）
    pub url: String,
    pub placeholder_hint: &'static str,
    pub show_file_browse: bool,
    pub file_filter: &'static [&'static str],
}

impl SourceBar {
    pub fn new(key: &str, hint: &'static str) -> Self {
        Self {
            key: key.into(),
            url: String::new(),
            placeholder_hint: hint,
            show_file_browse: true,
            file_filter: &["ts", "mpg", "mpeg", "mp4", "mkv", "avi", "flv", "m2t", "mts"],
        }
    }

    fn history(&self) -> Vec<String> {
        let k = format!("recent_sources.{}", self.key);
        vqa_core::config::shared_get_or(k.as_str(), Vec::<String>::new())
    }

    fn push_history(&self, url: &str) {
        if url.is_empty() {
            return;
        }
        let k = format!("recent_sources.{}", self.key);
        let mut h = self.history();
        h.retain(|x| x != url);
        h.insert(0, url.to_string());
        h.truncate(5);
        let _ = vqa_core::config::shared_set(k.as_str(), serde_json::json!(h));
    }

    /// 绘制并返回本帧动作。
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        nic: &mut crate::widgets::nic_selector::NicSelector,
        running: bool,
    ) -> SourceAction {
        let mut action = SourceAction::None;
        // wrapped：窄面板下自然换行，杜绝右侧按钮被窗口边缘裁切
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("来源").color(crate::theme::MUT));
            let hist = self.history();
            // 单一输入框 + ▾ 历史弹出（旧版 combo+TextEdit 双显示同址易挤爆一行）
            let hist_id = egui::Id::new(format!("src_hist_{}", self.key));
            let resp_url = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text(self.placeholder_hint)
                    .desired_width((ui.available_width() - 420.0).max(240.0)),
            );
            let hist_btn = ui.add(egui::Button::new("▾").rounding(6.0));
            if hist_btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(hist_id));
            }
            egui::popup_below_widget(ui, hist_id, &hist_btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(320.0);
                if ui.selectable_label(false, self.placeholder_hint).clicked() {
                    self.url.clear();
                    ui.memory_mut(|m| m.close_popup());
                }
                for h in &hist {
                    if ui.selectable_label(false, h).clicked() {
                        self.url = h.clone();
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            });
            let _ = resp_url;
            nic.show(ui);
            // 设计稿顺序：网卡 → 开始(主色+播放三角) → folder ghost
            let enter_start = resp_url.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = action_button(ui, running);
            if clicked {
                vqa_core::config::log_line(
                    "srcbar",
                    &format!("action clicked, running={running} url={:?} enter={enter_start}", self.url),
                );
            }
            if clicked || enter_start {
                if running {
                    action = SourceAction::Stop;
                } else {
                    let url = crate::widgets::clean_stream_url(&self.url);
                    self.url = url.clone();
                    if !url.is_empty() {
                        self.push_history(&url);
                        let nic_ip = nic.selected_ip().to_string();
                        action = SourceAction::Start { url, nic_ip };
                    }
                }
            }
            // folder ghost 图标钮（描边，设计稿末位）
            if self.show_file_browse {
                let (frect, fresp) = ui.allocate_exact_size(
                    egui::vec2(32.0, 32.0),
                    egui::Sense::click(),
                );
                let fp = ui.painter_at(frect);
                fp.rect_filled(frect, 8.0, if fresp.hovered() {
                    crate::theme::ACC_SOFT
                } else {
                    egui::Color32::TRANSPARENT
                });
                fp.rect_stroke(frect, 8.0, egui::Stroke::new(1.0, crate::theme::BORDER2));
                fp.text(
                    frect.center(),
                    egui::Align2::CENTER_CENTER,
                    crate::widgets::lucide::Lucide::Folder.glyph(),
                    egui::FontId::new(15.0, egui::FontFamily::Name(crate::widgets::lucide::LUCIDE_FAMILY.into())),
                    if fresp.hovered() { crate::theme::TEXT } else { crate::theme::MUT },
                );
                if fresp.clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("媒体文件", self.file_filter)
                        .pick_file()
                    {
                        self.url = p.to_string_lossy().to_string();
                    }
                }
                fresp.on_hover_text("浏览本地文件…");
            }
        });
        action
    }
}

/// 来源栏主行动钮（设计稿口径）：主色填充 + 白色图标 + 文字，76×32 圆角 8。
/// running=true 时为红底「停止」。
fn action_button(ui: &mut egui::Ui, running: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(76.0, 32.0), egui::Sense::click());
    let p = ui.painter_at(rect);
    let bg = if running { crate::theme::BAD } else { crate::theme::ACC };
    p.rect_filled(rect, 8.0, if resp.hovered() { crate::theme::darken(bg, 0.15) } else { bg });
    let icon = if running { crate::widgets::lucide::Lucide::Square } else { crate::widgets::lucide::Lucide::Play };
    p.text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon.glyph(),
        egui::FontId::new(14.0, egui::FontFamily::Name(crate::widgets::lucide::LUCIDE_FAMILY.into())),
        egui::Color32::WHITE,
    );
    p.text(
        egui::pos2(rect.left() + 32.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        if running { "停止" } else { "开始" },
        egui::FontId::proportional(12.5),
        egui::Color32::WHITE,
    );
    resp.clicked()
}
