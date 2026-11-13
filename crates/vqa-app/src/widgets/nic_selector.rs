//! 网卡选择下拉框 —— `NicSelector.py` 的移植。
//! 「自动」= 空串（交给系统路由）；列出在用网卡 IPv4。

use egui::Ui;

#[derive(Default)]
pub struct NicSelector {
    pub selected: String, // "" = 自动
    pub persist: bool,
}

impl NicSelector {
    pub fn new(persist: bool) -> Self {
        let mut s = Self { selected: String::new(), persist };
        if persist {
            s.selected = vqa_core::config::shared_get_or("net_iface", String::new());
        }
        s
    }

    /// 选中 IP（"" = 自动）。
    pub fn selected_ip(&self) -> &str {
        &self.selected
    }

    pub fn show(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("网卡");
            let choices = vqa_core::netif::choices();
            let cur = if self.selected.is_empty() {
                vqa_core::netif::AUTO.to_string()
            } else {
                self.selected.clone()
            };
            let mut changed: Option<String> = None;
            egui::ComboBox::from_id_source("nic_sel")
                .width(150.0)
                .selected_text(cur)
                .show_ui(ui, |ui| {
                    for c in &choices {
                        let val = if c == vqa_core::netif::AUTO { "" } else { c.as_str() };
                        ui.selectable_value(&mut changed, Some(val.to_string()), c.clone());
                    }
                });
            if let Some(v) = changed {
                self.selected = v;
                if self.persist {
                    let _ = vqa_core::config::shared_set(
                        "net_iface",
                        serde_json::json!(self.selected),
                    );
                }
            }
            if ui.button("⟳").clicked() {
                // 重新枚举即生效（choices 每次现算）
            }
        });
    }
}
