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
            if self.show_file_browse && ui.add_sized([64.0, 26.0], egui::Button::new("浏览…")).clicked() {
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
                        .add(egui::Button::new("停止").fill(crate::theme::BAD))
                        .clicked()
                    {
                        action = SourceAction::Stop;
                    }
                } else if ui
                    .add(
                        egui::Button::new(egui::RichText::new("开始").color(egui::Color32::WHITE))
                            .fill(crate::theme::ACC),
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
