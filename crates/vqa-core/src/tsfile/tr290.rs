//! TR 101 290 阈值与级别 —— tsfile.py 顶部的 TR290 / TR290_LEVEL 常量。
//!
//! 阈值（秒）：None = 默认；调用方传入的合法正值覆盖（自定义阈值对话框）。

use std::collections::HashMap;

/// 默认阈值。`sync_pkts` 是包数不是秒。
#[derive(Debug, Clone)]
pub struct Tr290Thresholds {
    pub pat: f64,
    pub pmt: f64,
    pub pid: f64,
    pub sync_pkts: u32,
    pub pcr: f64,
    pub pcr_jump: f64,
    pub pts: f64,
    pub pcr_accuracy: f64,
    pub nit: f64,
    pub sirep: f64,
    pub eit_act: f64,
    pub eit_oth: f64,
    pub sdt_act: f64,
    pub sdt_oth: f64,
    pub tdt: f64,
}

impl Default for Tr290Thresholds {
    fn default() -> Self {
        Self {
            pat: 0.5,               // PAT 重复间隔上限（含 PAT 被加扰判据）
            pmt: 0.5,               // PMT 重复间隔上限（含 PMT 被加扰判据）
            pid: 0.5,               // 未声明 PID 出现判定时长
            sync_pkts: 2,           // 连续 ≥2 个包起始同步错判同步丢失（文档口径）
            pcr: 0.04,              // PCR 间隔上限 40ms（PCR 间隔错误）
            pcr_jump: 0.1,          // PCR 偏离插值 >100ms（非连续标志类）
            pts: 0.7,               // PTS 重复间隔上限 700ms（标准口径，P2.6）
            pcr_accuracy: 500e-9,   // PCR 精度偏差上限 ±500ns（P2.5）
            nit: 10.0,
            sirep: 10.0,
            eit_act: 2.0,
            eit_oth: 10.0,
            sdt_act: 2.0,
            sdt_oth: 10.0,
            tdt: 30.0,
        }
    }
}

/// 自定义覆盖：键名同 Python 版（pat/pmt/pid/pcr/...），仅收正值。
#[derive(Debug, Clone, Default)]
pub struct TrOverrides {
    pub map: HashMap<String, f64>,
}

impl TrOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: &str, value: f64) {
        if value.is_finite() && value > 0.0 {
            self.map.insert(key.into(), value);
        }
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        self.map.get(key).copied()
    }
}

impl Tr290Thresholds {
    /// 应用覆盖生成扫描用阈值。
    pub fn with_overrides(o: &TrOverrides) -> Self {
        let mut t = Self::default();
        let f = |cur: &mut f64, key: &str| {
            if let Some(v) = o.get(key) {
                *cur = v;
            }
        };
        f(&mut t.pat, "pat");
        f(&mut t.pmt, "pmt");
        f(&mut t.pid, "pid");
        if let Some(v) = o.get("sync_pkts") {
            t.sync_pkts = v as u32;
        }
        f(&mut t.pcr, "pcr");
        f(&mut t.pcr_jump, "pcr_jump");
        f(&mut t.pts, "pts");
        f(&mut t.pcr_accuracy, "pcr_accuracy");
        f(&mut t.nit, "nit");
        f(&mut t.sirep, "sirep");
        f(&mut t.eit_act, "eit_act");
        f(&mut t.eit_oth, "eit_oth");
        f(&mut t.sdt_act, "sdt_act");
        f(&mut t.sdt_oth, "sdt_oth");
        f(&mut t.tdt, "tdt");
        t
    }
}

pub fn tr290_defaults() -> Tr290Thresholds {
    Tr290Thresholds::default()
}

/// 参数 → 级别（1/2/3），按《ETSI TR101 290 监测的三种级别错误》文档口径：
/// 一级 6 种、二级 6 种、三级 10 种。
pub fn tr290_level(name: &str) -> u8 {
    match name {
        // 一级
        "TS_sync_loss" | "Sync_byte_error" | "PAT_error"
        | "Continuity_count_error" | "PMT_interval_error"
        | "PMT_scrambled_error" | "PID_error" => 1,
        // 二级
        "Transport_error" | "CRC_error" | "PCR_error"
        | "PCR_discontinuity_error" | "PCR_accuracy_error" | "PTS_error"
        | "TS_pack_scrambled" | "CAT_error" => 2,
        // 三级
        "NIT_id_error" | "NIT_interval_error" | "NIT_other_error"
        | "SI_repetition_error" | "Buffer_error" | "Unreferenced_PID_error"
        | "SDT_id_error" | "SDT_act_interval_error" | "SDT_oth_interval_error"
        | "EIT_id_error" | "EIT_act_interval_error" | "EIT_oth_interval_error"
        | "EIT_pf_error" | "RST_error" | "TDT_error" | "Empty_buffer_error"
        | "Data_delay_error" => 3,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_match_python() {
        assert_eq!(tr290_level("TS_sync_loss"), 1);
        assert_eq!(tr290_level("PAT_error"), 1);
        assert_eq!(tr290_level("CRC_error"), 2);
        assert_eq!(tr290_level("PCR_accuracy_error"), 2);
        assert_eq!(tr290_level("NIT_id_error"), 3);
        assert_eq!(tr290_level("TDT_error"), 3);
    }

    #[test]
    fn overrides_only_positive() {
        let mut o = TrOverrides::new();
        o.set("pcr", 0.08);
        o.set("pat", -1.0); // 非法值忽略
        let t = Tr290Thresholds::with_overrides(&o);
        assert_eq!(t.pcr, 0.08);
        assert_eq!(t.pat, 0.5);
        assert_eq!(t.sync_pkts, 2);
    }
}
