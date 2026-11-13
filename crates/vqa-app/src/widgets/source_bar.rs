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
        ui.horizontal(|ui| {
            ui.label("来源");
            let hist = self.history();
            let mut url_changed = None;
            let combo = egui::ComboBox::from_id_source(format!("src_{}", self.key))
                .width(ui.available_width() - 330.0)
                .selected_text(if self.url.is_empty() {
                    self.placeholder_hint.to_string()
                } else {
                    self.url.clone()
                });
            combo.show_ui(ui, |ui| {
                if ui
                    .selectable_label(false, self.placeholder_hint)
                    .clicked()
                {
                    url_changed = Some(String::new());
                }
                for h in &hist {
                    if ui.selectable_label(false, h).clicked() {
                        url_changed = Some(h.clone());
                    }
                }
            });
            let resp_url = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text(self.placeholder_hint)
                    .desired_width(ui.available_width() - 320.0),
            );
            if resp_url.changed() {
                // 可编辑输入框为主，历史下拉只做快捷选择
            }
            if let Some(u) = url_changed {
                self.url = u;
            }
            nic.show(ui);
            if self.show_file_browse && ui.button("浏览…").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("媒体文件", self.file_filter)
                    .pick_file()
                {
                    self.url = p.to_string_lossy().to_string();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if ui
                        .add(egui::Button::new("停止").fill(crate::theme::DANGER))
                        .clicked()
                    {
                        action = SourceAction::Stop;
                    }
                } else if ui
                    .add(
                        egui::Button::new(egui::RichText::new("开始").color(egui::Color32::WHITE))
                            .fill(crate::theme::PRIMARY),
                    )
                    .clicked()
                    || resp_url.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    let url = crate::widgets::clean_stream_url(&self.url);
                    self.url = url.clone();
                    if !url.is_empty() {
                        self.push_history(&url);
                        let nic_ip = nic.selected_ip().to_string();
                        action = SourceAction::Start { url, nic_ip };
                    }
                }
            });
        });
        action
    }
}
