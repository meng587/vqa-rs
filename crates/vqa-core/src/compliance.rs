//! 合规性检测 —— 依据《广电办发〔2024〕253 号》节目技术质量要求。
//!
//! 从 Python 版 `tools/player/ui/pages/CompliancePage.py`（v0.6.x）移植，
//! 只保留"能自动判"的部分：
//!
//! | 类别 | 项 | 数据来源 |
//! |------|----|----------|
//! | 节目技术质量 | 分辨率/帧率/编码/Profile&Level/量化精度/色域/转换曲线/视频码率/音频格式/采样率/音频码率 | ffprobe |
//! | 网络传输质量（IPTV） | RTP 抖动/MDI-DF/MDI-MLR/RTP 丢包 | 收流统计（`mdi`） |
//! | 网络传输质量（有线） | QAM 电平/MER/BER | 需硬件，软件不判（unknown） |
//!
//! 判定字段三态：`pass` / `fail` / `unknown`（未标注、取不到值），
//! 与 Python 版 `_add_row` 的 ✓ / ✗ / — 一致。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 检测依据：原文《广播电视和网络视听节目技术质量监督检测工作方案》三、检测依据。
pub const BASIS: [(&str, &str); 15] = [
    ("GB/T 41808—2022", "高动态范围电视节目制作和交换图像参数值"),
    ("GB/T 41809—2022", "超高清晰度电视系统节目制作和交换参数值"),
    ("GB/T 42758—2023", "用于节目制作的先进声音系统"),
    ("GY/T 257.1—2012", "广播电视先进音视频编解码 第1部分：视频"),
    ("GY/T 299.1—2016", "高效音视频编码 第1部分：视频"),
    ("GY/T 353—2021", "网络视听节目视频格式命名及参数规范"),
    ("GY/T 375—2023", "有线数字电视音视频技术质量要求和测量方法"),
    ("GY/T 376—2023", "IPTV 音视频技术质量要求和测量方法"),
    ("报批稿", "网络视听节目视频技术质量要求和主观评价方法"),
    ("征求意见稿", "超高清视频图像质量客观评价方法"),
    ("总局令第 67 号", "有线广播电视运营服务管理暂行规定"),
    ("广电办发〔2023〕187号", "4K 超高清电视技术应用实施指南（2023 版）"),
    ("广电发〔2022〕37号", "关于进一步加强推进高清超高清电视发展的意见"),
    ("新广电发〔2017〕230号", "关于规范和促进 4K 超高清电视发展的通知"),
    ("新广电发〔2014〕75号", "广播电视先进视频编解码（AVS+）技术应用实施指南"),
];

/// 说明页表格：节目技术质量（表 1 / 表 2 序号 1~14）。
pub const DOC_PROG: [(&str, &str, &str, &str); 11] = [
    ("1", "分辨率", "1920×1080 / 3840×2160", "ffprobe 流信息"),
    ("2", "帧率", "50i / 50p（高清 50i 或 25p，超高清 50p）", "ffprobe 流信息"),
    ("3", "色域", "BT.709 / BT.2020", "码流 color_space"),
    ("4", "量化精度", "8bit / 10bit", "bits_per_raw_sample、pix_fmt"),
    ("5", "转换曲线", "电视伽马 / HLG 或 PQ", "码流 color_transfer"),
    (
        "6",
        "视频编码方式",
        "高清：AVS+、H.264、AVS2、H.265；超高清：AVS2、H.265",
        "codec_name",
    ),
    (
        "7~10",
        "视频编码类和级",
        "高清：AVS+ 广播类 6.0 级及以上；H.264 High Profile L4.1 及以上；AVS2 基准 8 位类 6.0.30 级及以上；H.265 Main Profile L4.0 及以上。超高清：AVS2 基准 10 位类 8.0.60 级及以上；H.265 Main10 Profile L5.1 及以上",
        "profile / level",
    ),
    (
        "11",
        "视频编码码率",
        "高清：电视台输出信号检测点 ≥11Mbps；有线前端输出点 / IPTV 网络传输信号检测点 AVS+、H.264 ≥8Mbps，AVS2、H.265 ≥6Mbps。超高清：≥36Mbps",
        "流级 bit_rate，缺则取容器码率",
    ),
    (
        "12",
        "音频格式",
        "MPEG-1 Layer2、AAC、AC-3（立体声或 5.1 环绕声；超高清可含三维声）",
        "codec_name + 声道数",
    ),
    ("13", "音频采样率", "48kHz", "sample_rate"),
    (
        "14",
        "音频编码码率",
        "立体声 ≥256kbps、5.1 环绕声 ≥384kbps、三维声 ≥384kbps",
        "音频流 bit_rate",
    ),
];

/// 说明页表格：IPTV 网络传输质量（表 2 序号 15~18）。
pub const DOC_NET_IPTV: [(&str, &str, &str, &str); 4] = [
    ("15", "RTP 抖动", "不大于 50ms", "收流，2s 窗口"),
    ("16", "RTP 丢包率", "不高于 10E-6（即 1×10⁻⁶）", "RTP 序号跳变统计"),
    (
        "17",
        "MDI-DF 时延",
        "以 5s 为计算周期，MDI-DF ≥100ms 的时间占比不大于 0.05%",
        "收流，5s 窗口",
    ),
    (
        "18",
        "MDI-MLR 媒体丢包率",
        "以 10s 为计算周期，MDI-MLR=0 的时间占比不小于 99.99%",
        "收流，10s 窗口",
    ),
];

/// 说明页表格：有线 RF 与 IP 信号（表 1 序号 16~17）。
pub const DOC_NET_CATV: [(&str, &str, &str, &str); 3] = [
    (
        "16",
        "QAM 调制信号",
        "电平 50dBuV～75dBuV、MER 不低于 24dB、BER 纠错前不高于 10E-4 且纠错后不高于 10E-11，超门限持续小于 2s",
        "需电平表 / 码流分析仪，软件不判",
    ),
    (
        "17",
        "MDI-DF（IP 信号）",
        "单节目码流 DF 不高于 80ms，多节目复用码流不高于 40ms，超门限持续小于 2s",
        "收流实测（平均包间隔偏差近似）",
    ),
    (
        "17",
        "MDI-MLR（IP 信号）",
        "每秒丢包不超过 1 个 IP 包或 7 个 TS 包；每 15min 不超过 3 个 IP 包或 21 个 TS 包",
        "收流实测（RTP 序号丢包 / 秒）",
    ),
];

/// 说明页表格：音视频质量（事件类）。
pub const DOC_EVENTS: [(&str, &str, &str, &str); 5] = [
    (
        "15（表 1）",
        "视频质量要求",
        "指定 PID 视频流丢包或无有效净荷 ≤2s；画面不应出现黑场、静帧、马赛克，异常持续 ≤2s",
        "本版本按 TS PID 层判丢包/无净荷；画面异常需逐帧解码",
    ),
    (
        "19（表 2）",
        "音视频质量（一）视频连续无丢帧",
        "无指定视频 PID 或无有效净荷 ≤2s",
        "可判：离线按 TS PID 连续性实测",
    ),
    (
        "20（表 2）",
        "音视频质量（二）解码正确",
        "有有效包但解码器无法还原帧 <2s",
        "本版本未实现（需解码器）",
    ),
    (
        "21（表 2）",
        "音视频质量（三）声音连续无中断",
        "无指定音频 PID 或无有效净荷 <2s",
        "可判：离线按 TS PID 连续性实测",
    ),
    (
        "22（表 2）",
        "音视频质量（四）画面主观异常",
        "马赛克、静帧、黑屏、可察觉音画不同步 <2s",
        "本版本未实现",
    ),
];

/// 说明页的判定口径与已知局限。
pub const DOC_LIMITS: [&str; 7] = [
    "事件类：表 2 序号 19 / 21 已按 TS 包头实测（连续性计数错误 = 丢包，仅适配字段 = 无有效净荷）；序号 20 解码正确、22 画面主观异常，以及表 1 序号 15 的画面部分，仍需逐帧解码与参考比对，标未标注。",
    "表 1 序号 16 的电平 / MER / BER 属 QAM 射频层，需电平表与码流分析仪，软件不判。",
    "视频码率优先用流级 bit_rate；TS 未给时取容器码率，含音频与节目 SI，读数会略高于纯视频码率。",
    "AVS、AVS2 的 Profile / Level ffprobe 取不到（level = -99），该项显示未标注，需专业设备核对。",
    "多数 TS 不写 color_space / color_transfer，色域与转换曲线常常只能显示未标注。",
    "MDI-MLR 的「每 15min ≤3 个 IP 包」口径未单列，页面按每秒丢包数判定；样本时长不足 15min 时仅供参考。",
    "表 3（互联网电视）、表 4（互联网视听平台）的主观评分与格式命名不在本页范围内。",
];

fn doc_rows(rows: &[(&str, &str, &str, &str)]) -> Vec<Value> {
    rows.iter().map(|r| json!(r)).collect()
}

/// 合规说明页的全部静态数据：前端只取这一份，避免 TS 与 Rust 两套文案漂移。
pub fn doc_json() -> Value {
    let basis: Vec<Value> = BASIS
        .iter()
        .map(|(code, name)| json!({ "code": code, "name": name }))
        .collect();
    json!({
        "basis": basis,
        "prog": doc_rows(&DOC_PROG),
        "net_iptv": doc_rows(&DOC_NET_IPTV),
        "net_catv": doc_rows(&DOC_NET_CATV),
        "events": doc_rows(&DOC_EVENTS),
        "limits": DOC_LIMITS,
    })
}

#[cfg(test)]
mod doc_tests {
    use super::*;

    #[test]
    fn doc_json_has_all_sections() {
        let v = doc_json();
        assert_eq!(v["basis"].as_array().unwrap().len(), 15);
        assert_eq!(v["prog"].as_array().unwrap().len(), DOC_PROG.len());
        assert_eq!(v["net_iptv"].as_array().unwrap().len(), DOC_NET_IPTV.len());
        assert_eq!(v["net_catv"].as_array().unwrap().len(), DOC_NET_CATV.len());
        assert_eq!(v["events"].as_array().unwrap().len(), DOC_EVENTS.len());
        assert_eq!(v["limits"].as_array().unwrap().len(), DOC_LIMITS.len());
    }
}

/// 检测场景：有线电视（表 1） / IPTV（表 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Catv,
    Iptv,
}

impl Kind {
    pub fn from_str(s: &str) -> Self {
        let t = s.trim().to_ascii_uppercase();
        if t.contains("IPTV") {
            Kind::Iptv
        } else {
            Kind::Catv
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Catv => "有线电视（表 1）",
            Kind::Iptv => "IPTV（表 2）",
        }
    }
}

/// 判定结果：合格 / 不合格 / 未标注。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Unknown,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "✓ 合格",
            Verdict::Fail => "✗ 不合格",
            Verdict::Unknown => "— 未标注",
        }
    }
}

/// 一条检测项。
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// 稳定键名（导出报告、前端着色都用它）
    pub key: &'static str,
    pub name: &'static str,
    /// 标准要求（人读）
    pub standard: String,
    /// 实测值（人读）
    pub actual: String,
    pub verdict: Verdict,
    /// 补充说明（取不到值的原因、口径等）
    pub note: String,
}

/// 汇总。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub unknown: usize,
    /// 总体结论（有 fail 即不合规）
    pub ok: bool,
}

impl Summary {
    pub fn of(items: &[Item]) -> Self {
        let pass = items.iter().filter(|i| i.verdict == Verdict::Pass).count();
        let fail = items.iter().filter(|i| i.verdict == Verdict::Fail).count();
        let unknown = items.iter().filter(|i| i.verdict == Verdict::Unknown).count();
        Summary { total: items.len(), pass, fail, unknown, ok: fail == 0 }
    }
}

fn v(b: bool) -> Verdict {
    if b {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

/// 原文序号顺序（表 1 序号 1~17 / 表 2 序号 1~22），页面与报告都按它排。
const ORDER: [&str; 24] = [
    "res", "fps", "colorspace", "bits", "transfer", "codec", "profile", "vbitrate", "acodec",
    "asr", "abr", // 1~14 节目技术质量
    "jitter", "loss", "df", "mlr", // 表 2 序号 15~18 网络
    "ev_pid", "ev_decode", "ev_audio", "ev_video", // 表 1 序号 15 / 表 2 序号 19~22
    "qam", "mer", "ber", "catv_df", "catv_mlr", // 表 1 序号 16~17 有线 RF / IP 信号
];

fn rank(key: &str) -> usize {
    ORDER.iter().position(|k| *k == key).unwrap_or(ORDER.len())
}

/// 按原文序号排序（不改变判定，只影响展示顺序）。
pub fn sort_items(items: &mut [Item]) {
    items.sort_by_key(|i| rank(i.key));
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|x| x.parse::<f64>().ok())
        .filter(|f| f.is_finite())
}

fn txt(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// 帧率解析 `"50/1"` → 50.0；取不到返回 0。
fn fps_of(stream: &Value) -> f64 {
    let raw = txt(stream, "avg_frame_rate");
    let parts: Vec<&str> = raw.split('/').collect();
    if parts.len() == 2 {
        let n: f64 = parts[0].parse().unwrap_or(0.0);
        let d: f64 = parts[1].parse().unwrap_or(0.0);
        if d > 0.0 {
            return (n / d * 100.0).round() / 100.0;
        }
    }
    let alt = txt(stream, "r_frame_rate");
    let parts: Vec<&str> = alt.split('/').collect();
    if parts.len() == 2 {
        let n: f64 = parts[0].parse().unwrap_or(0.0);
        let d: f64 = parts[1].parse().unwrap_or(0.0);
        if d > 0.0 {
            return (n / d * 100.0).round() / 100.0;
        }
    }
    0.0
}

fn codec_label(codec: &str) -> String {
    match codec {
        "h264" => s("H.264"),
        "hevc" => s("H.265"),
        "avs" => s("AVS+"),
        "avs2" => s("AVS2"),
        "avs3" => s("AVS3"),
        "mpeg2video" => s("MPEG2"),
        "" => s("未知"),
        other => other.to_string(),
    }
}

/// 节目技术质量检测：吃一份 ffprobe `-show_format -show_streams` 的 JSON。
/// `kind` 只影响少数文案（如视频码率的检测点口径）。
pub fn evaluate_probe(raw: &Value, kind: Kind) -> Vec<Item> {
    let streams = raw
        .get("streams")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let fmt = raw.get("format").cloned().unwrap_or(Value::Null);
    let video = streams
        .iter()
        .find(|st| txt(st, "codec_type") == "video")
        .cloned()
        .unwrap_or(Value::Null);
    let audio = streams
        .iter()
        .find(|st| txt(st, "codec_type") == "audio")
        .cloned()
        .unwrap_or(Value::Null);

    let mut items: Vec<Item> = Vec::new();
    if video.is_null() {
        items.push(Item {
            key: "video",
            name: "视频流",
            standard: s("应存在视频流"),
            actual: s("未检测到视频流"),
            verdict: Verdict::Fail,
            note: s("ffprobe 未返回 codec_type=video 的流"),
        });
        return items;
    }

    let w = video.get("width").and_then(|x| x.as_u64()).unwrap_or(0);
    let h = video.get("height").and_then(|x| x.as_u64()).unwrap_or(0);
    let is_uhd = w >= 3840;

    // ---- 分辨率 ----
    let res_std = if is_uhd { "3840×2160" } else { "1920×1080" };
    let res_fit = if is_uhd {
        w == 3840 && h == 2160
    } else {
        w == 1920 && h == 1080
    };
    items.push(Item {
        key: "res",
        name: "分辨率",
        standard: s(res_std),
        actual: if w > 0 && h > 0 {
            format!("{w}×{h}")
        } else {
            s("未标注")
        },
        // 直播样本探测可能拿不到宽高，这时不能判不合格
        verdict: if w > 0 && h > 0 { v(res_fit) } else { Verdict::Unknown },
        note: if w == 0 || h == 0 {
            s("ffprobe 未给出分辨率，多为直播样本不足，需重新收流或换离线文件")
        } else if is_uhd {
            s("超高清档")
        } else {
            s("高清档")
        },
    });

    // ---- 帧率 ----
    let fps = fps_of(&video);
    let fps_fit = if is_uhd {
        (fps - 50.0).abs() < 1.0
    } else {
        (fps - 50.0).abs() < 1.0 || (fps - 25.0).abs() < 1.0
    };
    items.push(Item {
        key: "fps",
        name: "帧率",
        standard: s("50p（超高清）/ 50i 或 25p（高清）"),
        actual: format!("{fps:.2} fps"),
        verdict: if fps > 0.0 { v(fps_fit) } else { Verdict::Unknown },
        note: if fps > 0.0 { s("") } else { s("avg_frame_rate 取不到") },
    });

    // ---- 编码方式 ----
    let codec = txt(&video, "codec_name");
    let codec_fit = if is_uhd {
        matches!(codec.as_str(), "avs2" | "hevc")
    } else {
        matches!(codec.as_str(), "h264" | "avs" | "avs2" | "hevc")
    };
    items.push(Item {
        key: "codec",
        name: "视频编码方式",
        standard: if is_uhd {
            s("AVS2 / H.265")
        } else {
            s("AVS+ / H.264 / AVS2 / H.265")
        },
        actual: codec_label(&codec),
        verdict: v(codec_fit),
        note: if codec.is_empty() {
            s("ffprobe 未给出 codec_name")
        } else if codec_fit {
            s("")
        } else if codec == "avs3" {
            s("AVS3 不在表 1/2 的编码方式范围内（超高清只列 AVS2、H.265）")
        } else {
            s("表 1/2 未列该编码方式")
        },
    });

    // ---- Profile / Level（表 1/2 序号 7~10 视频编码类和级）----
    let prof = txt(&video, "profile");
    let lev = video.get("level").and_then(|x| x.as_i64()).unwrap_or(0);
    // ffprobe 对 AVS 常给 level=-99（未知），别把它显示成 L-9.9
    let lev_known = lev > 0;
    let lev_str = if !lev_known {
        "—".to_string()
    } else if codec == "hevc" {
        format!("L{:.1}", lev as f64 / 30.0)
    } else {
        format!("L{:.1}", lev as f64 / 10.0)
    };
    let lvl_verdict = if !lev_known {
        Verdict::Unknown
    } else {
        match codec.as_str() {
            "h264" => v(prof.to_ascii_lowercase().contains("high") && lev >= 41),
            "hevc" if is_uhd => {
                let p = prof.to_ascii_lowercase().replace(' ', "");
                v(p.contains("main10") && lev >= 153)
            }
            "hevc" => v(prof.to_ascii_lowercase().contains("main") && lev >= 120),
            _ => Verdict::Unknown,
        }
    };
    let lvl_std = if is_uhd {
        "AVS2 基准 10 位类 8.0.60 级及以上；H.265 Main10 Profile L5.1 及以上"
    } else {
        "AVS+ 广播类 6.0 级及以上；H.264 High L4.1 及以上；AVS2 基准 8 位类 6.0.30 级及以上；H.265 Main L4.0 及以上"
    };
    items.push(Item {
        key: "profile",
        name: "视频编码类和级",
        standard: s(lvl_std),
        actual: format!("{} / {}", if prof.is_empty() { "—" } else { &prof }, lev_str),
        verdict: lvl_verdict,
        note: if lvl_verdict == Verdict::Unknown {
            if matches!(codec.as_str(), "avs" | "avs2" | "avs3") {
                s("AVS 系码流的类/级 ffprobe 取不到（level=-99），需专业设备核对；该项属表内判定项")
            } else {
                s("ffprobe 未给出可判的 Profile/Level，需人工核对")
            }
        } else {
            s("")
        },
    });

    // ---- 量化精度 ----
    let bits_raw = txt(&video, "bits_per_raw_sample");
    let pix = txt(&video, "pix_fmt");
    let bits: Option<i64> = if !bits_raw.is_empty() && bits_raw != "0" {
        bits_raw.parse::<i64>().ok().filter(|b| *b > 0)
    } else if pix.contains("10") || pix.contains("12") {
        Some(if pix.contains("12") { 12 } else { 10 })
    } else if !pix.is_empty() {
        Some(8)
    } else {
        None // AVS2 复用器可能什么都不给
    };
    let bits_fit = match bits {
        Some(b) => v(if is_uhd { b == 10 } else { b == 8 }),
        None => Verdict::Unknown,
    };
    items.push(Item {
        key: "bits",
        name: "量化精度",
        standard: s("10bit（超高清）/ 8bit（高清）"),
        actual: match bits {
            Some(b) => format!("{b}bit"),
            None => s("未标注"),
        },
        verdict: bits_fit,
        note: if bits.is_some() {
            format!("pix_fmt={}", if pix.is_empty() { "—" } else { &pix })
        } else {
            s("ffprobe 未给出 bits_per_raw_sample / pix_fmt，需人工核对")
        },
    });

    // ---- 色域 ----
    let csp = txt(&video, "color_space");
    let csp_verdict = if csp.is_empty() {
        Verdict::Unknown
    } else if is_uhd {
        v(csp.starts_with("bt2020"))
    } else {
        v(csp == "bt709")
    };
    items.push(Item {
        key: "colorspace",
        name: "色域",
        standard: s("BT.2020（超高清）/ BT.709（高清）"),
        actual: if csp.is_empty() { s("未标注") } else { csp.clone() },
        verdict: csp_verdict,
        note: if csp.is_empty() { s("码流未写 color_space，无法判定") } else { s("") },
    });

    // ---- 转换曲线 ----
    let trc = txt(&video, "color_transfer");
    let trc_verdict = if trc.is_empty() {
        Verdict::Unknown
    } else if is_uhd {
        v(matches!(trc.as_str(), "arib-std-b67" | "smpte2084"))
    } else {
        v(matches!(trc.as_str(), "bt709" | "bt1886" | "gamma28"))
    };
    items.push(Item {
        key: "transfer",
        name: "转换曲线",
        standard: s("HLG / PQ（超高清）/ 电视伽马（高清）"),
        actual: if trc.is_empty() { s("未标注") } else { trc.clone() },
        verdict: trc_verdict,
        note: if trc.is_empty() { s("码流未写 color_transfer，无法判定") } else { s("") },
    });

    // ---- 视频码率（表 1 序号 11 / 表 2 序号 11）----
    let vbr_stream = num(&video, "bit_rate");
    let vbr = vbr_stream.or_else(|| num(&fmt, "bit_rate")).unwrap_or(0.0);
    let vbr_mbps = vbr / 1_000_000.0;
    // 超高清统一 ≥36Mbps；高清分两个检测点：电视台输出点 ≥11Mbps，
    // 前端/IPTV 传输点按编码方式 ≥8Mbps（AVS+/H.264）或 ≥6Mbps（AVS2/H.265）。
    let need_mbps = if is_uhd {
        36.0
    } else if matches!(codec.as_str(), "hevc" | "avs2" | "avs3") {
        6.0
    } else {
        8.0
    };
    let vbr_std = if is_uhd {
        s("不低于 36Mbps")
    } else {
        let point = if kind == Kind::Iptv {
            "IPTV 网络传输信号检测点"
        } else {
            "有线电视前端输出信号检测点"
        };
        format!(
            "电视台输出信号检测点 ≥11Mbps；{point} AVS+/H.264 ≥8Mbps，AVS2/H.265 ≥6Mbps",
        )
    };
    // 判定值与显示值同源（都按 2 位小数），避免"显示 8.00 却判不合格"这种误读
    let vbr_show = (vbr_mbps * 100.0).round() / 100.0;
    items.push(Item {
        key: "vbitrate",
        name: "视频编码码率",
        standard: vbr_std,
        actual: if vbr_show > 0.0 {
            format!("{vbr_show:.2} Mbps")
        } else {
            s("—")
        },
        verdict: if vbr_show > 0.0 {
            v(vbr_show >= need_mbps)
        } else {
            Verdict::Unknown
        },
        note: if vbr_mbps <= 0.0 {
            s("TS 未给流级 bit_rate，改用容器码率也取不到")
        } else if vbr_stream.is_none() {
            format!("本机按{}判定；流级未给 bit_rate，取容器码率（含音频与 SI）",
                if is_uhd { s("超高清 36Mbps") } else { format!("高清 {need_mbps:.0}Mbps") })
        } else {
            s("")
        },
    });

    // ---- 音频 ----
    if audio.is_null() {
        for (key, name, stdx) in [
            ("acodec", "音频格式", "MPEG-1 Layer2、AAC、AC-3（立体声或 5.1 环绕声；超高清可含三维声）"),
            ("asr", "音频采样率", "48kHz"),
            ("abr", "音频编码码率", "立体声 ≥256kbps、5.1 环绕声 ≥384kbps、三维声 ≥384kbps"),
        ] {
            items.push(Item {
                key,
                name,
                standard: s(stdx),
                actual: s("未检测到音频流"),
                verdict: Verdict::Fail,
                note: s(""),
            });
        }
    } else {
        let acodec = txt(&audio, "codec_name");
        let a_fit = matches!(acodec.as_str(), "mp2" | "aac" | "ac3" | "eac3");
        items.push(Item {
            key: "acodec",
            name: "音频格式",
            standard: s("MPEG-1 Layer2、AAC、AC-3（立体声或 5.1 环绕声；超高清可含三维声）"),
            actual: if acodec.is_empty() { s("未知") } else { codec_label_audio(&acodec) },
            verdict: if acodec.is_empty() { Verdict::Unknown } else { v(a_fit) },
            note: s(""),
        });

        let sr = audio
            .get("sample_rate")
            .and_then(|x| x.as_str())
            .and_then(|x| x.parse::<i64>().ok())
            .unwrap_or(0);
        items.push(Item {
            key: "asr",
            name: "音频采样率",
            standard: s("48kHz"),
            actual: if sr > 0 {
                format!("{:.1} kHz", sr as f64 / 1000.0)
            } else {
                s("—")
            },
            verdict: if sr > 0 { v(sr == 48000) } else { Verdict::Unknown },
            note: s(""),
        });

        let ach = audio.get("channels").and_then(|x| x.as_u64()).unwrap_or(0);
        let abr = audio
            .get("bit_rate")
            .and_then(|x| x.as_str())
            .and_then(|x| x.parse::<f64>().ok())
            .unwrap_or(0.0);
        let need_k = if ach > 2 { 384.0 } else { 256.0 };
        let ch_name = match ach {
            0 => s("—"),
            1 => s("单声道"),
            2 => s("立体声"),
            6 => s("5.1环绕声"),
            8 => s("三维声"),
            n => format!("{n}声道"),
        };
        // 同视频码率：判定与显示都用取整后的 kbps 值
        let abr_show = (abr / 1000.0).round();
        items.push(Item {
            key: "abr",
            name: "音频编码码率",
            standard: s("立体声 ≥256kbps、5.1 环绕声 ≥384kbps、三维声 ≥384kbps"),
            actual: if abr_show > 0.0 {
                format!("{ch_name} / {abr_show:.0} kbps")
            } else {
                format!("{ch_name} / —")
            },
            verdict: if abr_show > 0.0 {
                v(abr_show >= need_k)
            } else {
                Verdict::Unknown
            },
            note: if abr <= 0.0 {
                s("TS 未给音频流 bit_rate")
            } else if ach == 1 {
                s("单声道不在表列音频格式（立体声 / 5.1 环绕声 / 三维声）内，按 ≥256kbps 计")
            } else {
                s("")
            },
        });
    }

    items
}

fn codec_label_audio(codec: &str) -> String {
    match codec {
        "mp2" => s("MPEG-1 Layer2"),
        "mp3" => s("MP3"),
        "aac" => s("AAC"),
        "ac3" => s("AC-3"),
        "eac3" => s("EC-3"),
        other => other.to_string(),
    }
}

/// 网络传输质量（IPTV 表 2）——吃一路收流统计。
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetStats {
    /// 码率（Mbps，2s 窗口滚动）
    pub bitrate_mbps: f64,
    /// RTP 抖动（ms）
    pub jitter_ms: f64,
    /// q. 丢包率（累计丢包 / 累计应收）
    pub loss_ratio: f64,
    /// MDI-DF：5s 窗口里抖动 ≥100ms 的包占比（%），取最近窗口
    pub df_bad_pct: f64,
    /// MDI-MLR：10s 窗口丢包为 0 的窗口占比（%）
    pub mlr_good_pct: f64,
    /// 是否确认 RTP 封装（非 RTP 无法包级判丢包）
    pub rtp: bool,
    /// 收流累计包数
    pub packets: u64,
    /// 收流累计丢包数（RTP 序号跳变计）
    pub lost: u64,
}

/// IPTV 表 2 的四项网络指标（原文序号 15~18）。
pub fn evaluate_net(st: &NetStats) -> Vec<Item> {
    // 一个包都没收到：四项都不能判（否则 0ms 抖动会假绿）
    if st.packets == 0 {
        return [
            ("jitter", "RTP 抖动", "不大于 50ms"),
            ("loss", "RTP 丢包率", "不高于 10E-6"),
            (
                "df",
                "MDI-DF 时延",
                "以 5s 为计算周期，MDI-DF ≥100ms 的时间占比不大于 0.05%",
            ),
            (
                "mlr",
                "MDI-MLR 媒体丢包率",
                "以 10s 为计算周期，MDI-MLR=0 的时间占比不小于 99.99%",
            ),
        ]
        .iter()
        .map(|(k, n, stdx)| Item {
            key: k,
            name: n,
            standard: s(stdx),
            actual: s("未收到数据"),
            verdict: Verdict::Unknown,
            note: s("检查组播地址/网卡是否选对，或该地址当前无流"),
        })
        .collect();
    }

    let mut items = Vec::new();

    items.push(Item {
        key: "jitter",
        name: "RTP 抖动",
        standard: s("不大于 50ms"),
        actual: format!("{:.1} ms", st.jitter_ms),
        verdict: v(st.jitter_ms <= 50.0),
        note: s("2s 窗口，抖动=|实际包间隔-40ms| 的绝对值均值"),
    });

    let loss_txt = if st.rtp {
        if st.loss_ratio <= 0.0 {
            format!("0（收 {} 包）", st.packets)
        } else {
            format!("{:.2e}（收 {} 包）", st.loss_ratio, st.packets)
        }
    } else {
        s("非 RTP 流，无法按序号判丢包")
    };
    items.push(Item {
        key: "loss",
        name: "RTP 丢包率",
        standard: s("不高于 10E-6（即 1×10⁻⁶）"),
        actual: loss_txt,
        verdict: if st.rtp {
            v(st.loss_ratio <= 1e-6)
        } else {
            Verdict::Unknown
        },
        note: if st.rtp { s("按 RTP 序号跳变统计") } else { s("UDP 裸封装无序号") },
    });

    items.push(Item {
        key: "df",
        name: "MDI-DF 时延",
        standard: s("以 5s 为计算周期，MDI-DF ≥100ms 的时间占比不大于 0.05%"),
        actual: format!("{:.3}%", st.df_bad_pct),
        verdict: v(st.df_bad_pct <= 0.05),
        note: s("最近一个 5s 窗口的占比"),
    });

    items.push(Item {
        key: "mlr",
        name: "MDI-MLR 媒体丢包率",
        standard: s("以 10s 为计算周期，MDI-MLR=0 的时间占比不小于 99.99%"),
        actual: if st.rtp {
            format!("{:.2}%", st.mlr_good_pct)
        } else {
            s("—")
        },
        verdict: if st.rtp {
            v(st.mlr_good_pct >= 99.99)
        } else {
            Verdict::Unknown
        },
        note: s("累计窗口口径"),
    });

    items
}

/// 音视频质量（事件类）：有线为表 1 序号 15，IPTV 为表 2 序号 19~22。
///
/// 能判的：序号 19 / 21（视频、音频 PID 丢包或无有效净荷）——按 TS 包头连续性实测。
/// 判不了的：序号 20 解码正确、22 画面主观异常（马赛克/静帧/黑屏/音画不同步），
/// 需要逐帧解码 + 参考比对，如实标未标注。
pub fn evaluate_events(kind: Kind, ev: Option<&crate::ts_events::EvReport>) -> Vec<Item> {
    const IPTV: [(&str, &str, &str); 4] = [
        (
            "ev_pid",
            "音视频质量（一）视频连续无丢帧",
            "未收到指定视频 PID 数据包，或收到但无有效净荷，持续时间不超过 2s",
        ),
        (
            "ev_decode",
            "音视频质量（二）解码正确",
            "含有效音视频数据包但解码器无法解码还原到帧的持续时间小于 2s",
        ),
        (
            "ev_audio",
            "音视频质量（三）声音连续无中断",
            "未收到指定音频 PID 数据包，或收到但无有效净荷，持续时间小于 2s",
        ),
        (
            "ev_video",
            "音视频质量（四）画面主观异常",
            "出现马赛克、静帧、黑屏、可察觉音画不同步等异常，持续时间小于 2s",
        ),
    ];
    const CATV: [(&str, &str, &str); 1] = [(
        "ev_pid",
        "视频质量要求",
        "指定 PID 视频流的数据包丢失或数据包中无有效净荷，持续时间不应超过 2s；画面应正常显示，不应出现黑场、静帧、马赛克，异常持续不超过 2s",
    )];

    let rows: &[(&str, &str, &str)] = if kind == Kind::Iptv {
        &IPTV
    } else {
        &CATV
    };

    rows.iter()
        .map(|(k, n, stdx)| {
            let mut it = Item {
                key: k,
                name: n,
                standard: s(stdx),
                actual: s("未检测"),
                verdict: Verdict::Unknown,
                note: s("需逐帧解码与参考比对，本版本未实现；建议用音视频质量分析仪复核"),
            };
            if let Some(e) = ev {
                match *k {
                    "ev_pid" => {
                        let (cc, np, pk) = e.video_errors();
                        let pids = pid_list(&e.video);
                        if pk == 0 {
                            it.actual = s("未收到视频 PID 数据包");
                            it.note = s("文件/样本里没有 PMT 声明的视频流，无法判定");
                        } else {
                            it.actual = format!(
                                "视频 PID {}：连续性错误 {} 次 · 无有效净荷 {} 包（共 {} 包 / {:.1}s）",
                                pids, cc, np, pk, e.duration_s
                            );
                            it.verdict = if cc == 0 && np == 0 {
                                Verdict::Pass
                            } else {
                                Verdict::Fail
                            };
                            it.note = if kind == Kind::Catv {
                                s("PID 层实测（丢包/无净荷）；黑场、静帧、马赛克需逐帧解码，未判")
                            } else {
                                s("PID 层实测：出现连续性错误即按不合格计（单次持续时间无法定位）")
                            };
                            if kind == Kind::Catv {
                                // 表 1 序号 15 还包含画面异常，不能给「合格」
                                it.verdict = Verdict::Unknown;
                                it.actual = format!(
                                    "{} ｜ PID 层{}",
                                    it.actual,
                                    if cc == 0 && np == 0 { "无丢包" } else { "有丢包" }
                                );
                            }
                        }
                    }
                    "ev_audio" => {
                        let (cc, np, pk) = e.audio_errors();
                        let pids = pid_list(&e.audio);
                        if pk == 0 {
                            it.actual = s("未收到音频 PID 数据包");
                            it.note = s("文件/样本里没有 PMT 声明的音频流，无法判定");
                        } else {
                            it.actual = format!(
                                "音频 PID {}：连续性错误 {} 次 · 无有效净荷 {} 包（共 {} 包 / {:.1}s）",
                                pids, cc, np, pk, e.duration_s
                            );
                            it.verdict = if cc == 0 && np == 0 {
                                Verdict::Pass
                            } else {
                                Verdict::Fail
                            };
                            it.note = s("PID 层实测：出现连续性错误即按不合格计（单次持续时间无法定位）");
                        }
                    }
                    "ev_decode" => {
                        it.actual = s("未检测");
                        it.note = s("需解码器逐帧还原核对，本版本未实现");
                    }
                    "ev_video" => {
                        it.actual = s("未检测");
                        it.note = s("马赛克/静帧/黑屏/音画不同步需逐帧比照，本版本未实现");
                    }
                    _ => {}
                }
            } else {
                match *k {
                    "ev_pid" | "ev_audio" => {
                        it.note = s("收流模式的事件类统计未实现，请用离线文件检测");
                    }
                    _ => {}
                }
            }
            it
        })
        .collect()
}

fn pid_list(v: &[crate::ts_events::PidEv]) -> String {
    v.iter()
        .map(|p| format!("0x{:04X}", p.pid))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 有线电视表 1 序号 16~17。
/// 16 项（电平/MER/BER）属 RF 层，软件不判；
/// 17 项（MDI-DF / MDI-MLR）在 IP 侧，收流时有实测就按原文判，没有则标未标注。
pub fn evaluate_catv_net(st: &NetStats, secs: u64) -> Vec<Item> {
    const ROWS: [(&str, &str, &str); 5] = [
        (
            "qam",
            "终端信号电平（QAM 调制信号）",
            "50dBuV～75dBuV，信号电平超出范围的持续时间应小于 2s",
        ),
        ("mer", "MER（调制误差率）", "不低于 24dB，低于门限的持续时间应小于 2s"),
        (
            "ber",
            "BER（误码率）",
            "纠错前 BER 不高于 10E-4，纠错后不高于 10E-11，超出门限的持续时间应小于 2s",
        ),
        (
            "catv_df",
            "MDI-DF（IP 信号）",
            "单节目码流 DF 值不高于 80ms，多节目复用码流不高于 40ms，超出门限的持续时间应小于 2s",
        ),
        (
            "catv_mlr",
            "MDI-MLR（IP 信号）",
            "每秒丢包不超过 1 个 IP 包或 7 个 TS 包；每 15min 丢包不超过 3 个 IP 包或 21 个 TS 包",
        ),
    ];
    let measured = st.packets > 0;
    ROWS.iter()
        .map(|(k, n, stdx)| {
            let mut it = Item {
                key: k,
                name: n,
                standard: s(stdx),
                actual: s("需硬件测量"),
                verdict: Verdict::Unknown,
                note: s("有线 RF 层指标需电平表/码流分析仪，本工作站只做 IP 侧"),
            };
            // 17 项是 IP 侧指标，没收到流时不能写"需硬件"，如实说等收流
            if !measured && matches!(*k, "catv_df" | "catv_mlr") {
                it.actual = s("需实时收流");
                it.note = s("离线文件/未收到数据时无传输层统计，切到收流模式再测");
            }
            match *k {
                "catv_df" if measured => {
                    it.actual = format!("{:.1} ms（平均间隔偏差）", st.jitter_ms);
                    it.verdict = v(st.jitter_ms <= 80.0);
                    it.note = s("按单节目码流 80ms 门限判；若本流为多节目复用，应收紧到 40ms");
                }
                "catv_mlr" if measured => {
                    if !st.rtp {
                        // 裸 UDP/TS 没有序号，丢包无从统计，别显示成 0 个/秒的假合格
                        it.actual = s("非 RTP 收流");
                        it.note = s("裸 UDP/TS 流没有 RTP 序号，无法统计丢包；如需判该项请走 RTP 收流");
                    } else {
                        let per_sec = if secs > 0 {
                            st.lost as f64 / secs as f64
                        } else {
                            st.lost as f64
                        };
                        it.actual = format!("{:.2} 个/秒（累计丢 {} / 收 {} 包）", per_sec, st.lost, st.packets);
                        it.verdict = v(per_sec <= 1.0);
                        it.note = s("按每秒 ≤1 个 IP 包判；每 15min ≤3 个包的口径未单列（样本时长不足 15min）");
                    }
                }
                _ => {}
            }
            it
        })
        .collect()
}
