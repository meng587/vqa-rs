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
        // 单行确定性布局：输入框按显式预算收缩（wrapped 在宽度临界时会把
        // 末尾的开始/folder 换行到不可见位置，见 5514bb9 后的实测截图）
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("来源").color(crate::theme::MUT));
            let hist = self.history();
            // 单一输入框 + ▾ 历史弹出（旧版 combo+TextEdit 双显示同址易挤爆一行）
            let hist_id = egui::Id::new(format!("src_hist_{}", self.key));
            let resp_url = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text(self.placeholder_hint)
                    .min_size(egui::vec2(240.0, 32.0))
                    .desired_width((ui.available_width() - 456.0).max(200.0)),
            );
            // 32×32 规格钮 + lucide chevron-down 字形（与全行 32px 规范一致）
            let hist_btn = ui.add_sized(
                egui::vec2(32.0, 32.0),
                egui::Button::new(
                    crate::widgets::lucide::Lucide::ChevronDown.text(14.0, crate::theme::MUT),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER2))
                .rounding(8.0),
            );
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

/// 来源栏主行动钮（现代化口径）：靛紫渐变填充 + 白色图标 + 文字，76×32 圆角 8。
/// running=true 时为红底「停止」（纯色）。
fn action_button(ui: &mut egui::Ui, running: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(76.0, 32.0), egui::Sense::click());
    let p = ui.painter_at(rect);
    if running {
        let bg = if resp.hovered() {
            crate::theme::lighten(crate::theme::BAD, 0.12)
        } else {
            crate::theme::BAD
        };
        p.rect_filled(rect, 8.0, bg);
    } else {
        // 渐变底：indigo-500 → violet-500 横向（hover 提亮一档）
        let (cl, cr) = if resp.hovered() {
            (
                crate::theme::lighten(crate::theme::ACC_GRAD_L, 0.12),
                crate::theme::lighten(crate::theme::ACC_GRAD_R, 0.12),
            )
        } else {
            (crate::theme::ACC_GRAD_L, crate::theme::ACC_GRAD_R)
        };
        let mut mesh = egui::Mesh::default();
        mesh.reserve_triangles(2);
        mesh.colored_vertex(rect.left_top(), cl);
        mesh.colored_vertex(rect.right_top(), cr);
        mesh.colored_vertex(rect.left_bottom(), crate::theme::darken(cl, 0.12));
        mesh.colored_vertex(rect.right_bottom(), crate::theme::darken(cr, 0.12));
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        p.add(mesh);
    }
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
