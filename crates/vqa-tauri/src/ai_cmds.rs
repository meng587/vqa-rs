//! AI 编排层：把自然语言目标转成可执行计划，并管理模型清单。
//!
//! 这一层做三件事：
//! 1. 生成结构化计划（规则或本机/云端 OpenAI 兼容模型），**不直接操作页面**；
//! 2. 管理 GGUF 模型文件，支持内置清单下载、离线导入与升级提示；
//! 3. 本机模型由内置 llama-server 直接加载，不要求用户另装 Ollama。
//!
//! 真正的模块动作由前端执行器调用现有 Tauri 命令完成，报告也由前端汇总。
//! 这样 AI 与页面共用同一套 API，不会出现“AI 点页面”的旁路。


use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

/// 内置模型清单：随安装包编译，不依赖联网即可展示可下载模型。
const BUILTIN_MODELS_INDEX: &str = include_str!("../update/models.json");

// ═══════════════════════════ 配置 ═══════════════════════════

fn ai_dir() -> PathBuf {
    vqa_core::config::base_dir().join("ai")
}

pub(crate) fn models_dir() -> PathBuf {
    ai_dir().join("models")
}

fn config_path() -> PathBuf {
    ai_dir().join("config.json")
}

fn installed_path() -> PathBuf {
    ai_dir().join("installed.json")
}

fn default_timeout() -> u64 {
    60
}

fn default_mode() -> String {
    "rule".into()
}

fn default_true() -> bool {
    true
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderCfg {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl Default for AiProviderCfg {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            timeout_secs: default_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub local: AiProviderCfg,
    #[serde(default)]
    pub cloud: AiProviderCfg,
    /// 自定义模型清单地址；留空时使用随安装包内置的清单。
    #[serde(default)]
    pub models_index_url: String,
    #[serde(default = "default_true")]
    pub auto_check: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            local: AiProviderCfg {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen3:4b".into(),
                timeout_secs: default_timeout(),
            },
            cloud: AiProviderCfg::default(),
            models_index_url: String::new(),
            auto_check: default_true(),
        }
    }
}

fn load_config() -> AiConfig {
    let p = config_path();
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &AiConfig) -> Result<(), String> {
    std::fs::create_dir_all(ai_dir()).map_err(|e| format!("建 AI 目录失败：{e}"))?;
    let raw = serde_json::to_string_pretty(cfg).map_err(|e| format!("序列化 AI 配置失败：{e}"))?;
    let tmp = config_path().with_extension("json.tmp");
    std::fs::write(&tmp, raw).map_err(|e| format!("写 AI 配置失败：{e}"))?;
    let _ = std::fs::remove_file(config_path());
    std::fs::rename(&tmp, config_path()).map_err(|e| format!("保存 AI 配置失败：{e}"))
}

fn validate_cfg(cfg: &AiConfig) -> Result<(), String> {
    match cfg.mode.as_str() {
        "rule" => Ok(()),
        "local" => {
            if cfg.local.base_url.trim().is_empty() {
                return Err("本机模型接口地址不能为空".into());
            }
            if cfg.local.model.trim().is_empty() {
                return Err("本机模型名不能为空".into());
            }
            Ok(())
        }
        "cloud" => {
            if cfg.cloud.base_url.trim().is_empty() {
                return Err("云端模型接口地址不能为空".into());
            }
            if cfg.cloud.model.trim().is_empty() {
                return Err("云端模型名不能为空".into());
            }
            Ok(())
        }
        other => Err(format!("未知 AI 模式：{other}")),
    }
}

#[tauri::command]
pub fn ai_config_get() -> AiConfig {
    load_config()
}

#[tauri::command]
pub fn ai_config_set(cfg: AiConfig) -> Result<AiConfig, String> {
    validate_cfg(&cfg)?;
    save_config(&cfg)?;
    Ok(cfg)
}

// ═══════════════════════════ 计划结构 ═══════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiStep {
    pub id: String,
    pub tool: String,
    pub title: String,
    #[serde(default = "empty_object")]
    pub args: Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiPlan {
    pub goal: String,
    pub summary: String,
    pub source: String,
    pub nic_ip: String,
    /// rule / local:<model> / cloud:<model>
    pub provider: String,
    pub steps: Vec<AiStep>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

fn contains_any(s: &str, words: &[&str]) -> bool {
    words.iter().any(|w| s.contains(w))
}

/// 从目标里找“30秒 / 1分钟 / 2min”这类时长；找不到返回 None。
fn parse_duration_secs(goal: &str) -> Option<u64> {
    let re = regex::Regex::new(r"(\d+)\s*(秒|s|sec|秒钟|分钟|分|min|m)").ok()?;
    let cap = re.captures(goal)?;
    let n: u64 = cap.get(1)?.as_str().parse().ok()?;
    let unit = cap.get(2)?.as_str();
    Some(match unit {
        "分钟" | "分" | "min" | "m" => n.saturating_mul(60),
        _ => n,
    })
}

fn source_is_live(source: &str) -> bool {
    let s = source.trim().to_ascii_lowercase();
    s.starts_with("udp://") || s.starts_with("rtp://")
}

fn source_is_file(source: &str) -> bool {
    let raw = source.trim().strip_prefix("file://").unwrap_or(source.trim());
    Path::new(raw).is_file()
}

fn clamp_duration(v: u64) -> u64 {
    v.clamp(5, 600)
}

fn step(id: &str, tool: &str, title: &str, args: Value, depends_on: Vec<&str>) -> AiStep {
    AiStep {
        id: id.into(),
        tool: tool.into(),
        title: title.into(),
        args,
        depends_on: depends_on.into_iter().map(str::to_string).collect(),
    }
}

fn rule_plan(goal: &str, source: &str, nic_ip: &str) -> Result<AiPlan, String> {
    let src = source.trim();
    if src.is_empty() {
        return Err("请先填写来源地址或文件路径".into());
    }
    let live = source_is_live(src);
    let file = source_is_file(src);
    if !live && !file {
        return Err(format!(
            "来源不是可识别的 UDP/RTP 地址或本地文件；抓包页只接受实时流，视频分析只接受 188/204 包长的 TS 文件：{src}"
        ));
    }

    let g = goal.to_lowercase();
    let health = contains_any(&g, &["体检", "全面", "一键", "巡检", "检测一下", "检查一下"]);
    let all = contains_any(&g, &["全面", "全项", "全部", "全都", "完整"]);
    let mut want_capture = contains_any(&g, &["抓包", "pcap", "抓取", "抓包分析"]);
    let mut want_analysis = contains_any(&g, &["ts", "码流", "视频分析", "tr101", "tr 101", "pcr", "pid", "节目结构"]);
    let mut want_compliance = contains_any(&g, &["合规", "253", "政策", "依据"]);
    let mut want_burst = contains_any(&g, &["微突发", "突发"]);
    let mut want_avsync = contains_any(&g, &["音画", "同步"]);
    let want_player = contains_any(&g, &["播放", "看画面", "预览"]);
    let want_report = contains_any(&g, &["报告", "出报告", "生成报告"]) || health;

    if health {
        want_capture = true;
        want_analysis = true;
        if all {
            want_compliance = true;
            want_burst = true;
            want_avsync = true;
        }
    }
    if want_capture || want_burst || want_avsync || want_compliance {
        // 这些关键词本身就是检测意图，即使用户没写“分析”也保留。
        want_analysis |= want_capture;
    }

    let mut warnings = Vec::new();
    if !live {
        if want_capture {
            want_capture = false;
            warnings.push("抓包只支持实时 UDP/RTP，本地文件已跳过抓包步骤".into());
        }
        if want_burst {
            want_burst = false;
            warnings.push("微突发需要实时收流，本地文件已跳过微突发步骤".into());
        }
    }
    if live && want_player && !want_capture && !want_analysis && !want_burst && !want_avsync && !want_compliance {
        // 纯“播放”指令只起播放器，不强行加检测。
    }

    if !want_capture && !want_analysis && !want_compliance && !want_burst && !want_avsync && !want_player {
        if want_report {
            want_analysis = true;
        } else if live {
            want_capture = true;
            want_analysis = true;
        } else {
            want_analysis = true;
        }
    }

    let duration = clamp_duration(parse_duration_secs(goal).unwrap_or(30));
    let mut steps = Vec::new();
    let mut module_ids: Vec<String> = Vec::new();

    if want_capture {
        steps.push(step(
            "capture",
            "capture.run",
            "抓包并取 TS 样本",
            json!({ "url": src, "nic_ip": nic_ip, "duration_sec": duration }),
            vec![],
        ));
        module_ids.push("capture".into());
    }
    if want_burst {
        steps.push(step(
            "burst",
            "burst.run",
            "微突发检测",
            json!({ "url": src, "nic_ip": nic_ip, "duration_sec": duration, "ratio": 2.0, "window_ms": 1 }),
            vec![],
        ));
        module_ids.push("burst".into());
    }
    if want_compliance {
        steps.push(step(
            "compliance",
            "compliance.run",
            if live { "合规检测（实时）" } else { "合规检测（文件）" },
            json!({ "source": src, "nic_ip": nic_ip, "duration_sec": duration, "live": live, "kind": "IPTV" }),
            vec![],
        ));
        module_ids.push("compliance".into());
    }
    if want_avsync {
        steps.push(step(
            "avsync",
            "avsync.run",
            "音画同步检测",
            json!({ "source": src, "nic_ip": nic_ip, "secs": duration }),
            vec![],
        ));
        module_ids.push("avsync".into());
    }
    if want_analysis {
        let (analysis_source, analysis_live, deps) = if want_capture {
            ("$capture.ts_path", false, vec!["capture"])
        } else {
            (src, live, vec![])
        };
        steps.push(step(
            "analysis",
            "analysis.run",
            "TS 码流结构分析",
            json!({ "source": analysis_source, "nic_ip": nic_ip, "duration_sec": duration, "live": analysis_live }),
            deps,
        ));
        module_ids.push("analysis".into());
    }
    if want_player {
        steps.push(step(
            "player",
            "player.play",
            "播放器画面抽检",
            json!({ "source": src, "nic_ip": nic_ip }),
            vec![],
        ));
        module_ids.push("player".into());
    }

    if want_report && !steps.is_empty() {
        let mut formats = vec!["md"];
        if contains_any(&g, &["html", "网页", "浏览器"]) {
            formats.push("html");
        }
        if contains_any(&g, &["word", "docx", "文档"]) {
            warnings.push("AI 汇总报告暂先导出 Markdown/HTML；Word 版可在后续版本接入".into());
        }
        steps.push(step(
            "report",
            "report.build",
            "汇总检测报告",
            json!({ "formats": formats }),
            module_ids.iter().map(String::as_str).collect(),
        ));
    } else if want_report && steps.is_empty() {
        warnings.push("没有可汇总的检测步骤，未生成报告".into());
    }

    let summary = if steps.is_empty() {
        "没有匹配到可执行步骤".to_string()
    } else {
        format!(
            "按目标编排 {} 个步骤：{}。",
            steps.len(),
            steps.iter().map(|s| s.title.as_str()).collect::<Vec<_>>().join("、")
        )
    };

    Ok(AiPlan {
        goal: goal.trim().to_string(),
        summary,
        source: src.to_string(),
        nic_ip: nic_ip.trim().to_string(),
        provider: "rule".into(),
        steps,
        warnings,
    })
}

// ═══════════════════════════ 模型计划 ═══════════════════════════

/// 模型只负责“编排意图”，不需要知道真实组播地址、URL 或本地路径。
/// 发送前把这些片段遮掉，避免云端/本机日志里留下完整来源。
fn redact_for_model(goal: &str) -> String {
    let re = Regex::new(
        r"(?i)(?:udp|rtp|http|https)://[^\s，。；、]+|(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?|[a-z]:[\\/][^\s]+|\\\\[^\s]+",
    )
    .expect("来源遮蔽正则");
    re.replace_all(goal, "〔地址已隐藏〕").to_string()
}

fn tool_catalog_text() -> &'static str {
    r#"可用工具（只能使用这些 tool 名）：
- capture.run：实时 UDP/RTP 抓包，产出 pcap 和 TS 样本。args: {"url":"来源","nic_ip":"网卡IP","duration_sec":30}
- analysis.run：TS 码流结构/TR290 分析。args: {"source":"来源或 $capture.ts_path","nic_ip":"","duration_sec":30,"live":true/false}
- compliance.run：合规检测。args: {"source":"来源","nic_ip":"","duration_sec":30,"live":true/false,"kind":"IPTV"}
- burst.run：微突发检测，仅实时流。args: {"url":"来源","nic_ip":"","duration_sec":30,"ratio":2.0,"window_ms":1}
- avsync.run：音画同步检测。args: {"source":"来源","nic_ip":"","secs":30}
- player.play：启动播放器并截一张画面。args: {"source":"来源","nic_ip":""}
- report.build：汇总报告。args: {"formats":["md"]}，必须依赖所有检测步骤。
步骤字段：id（唯一短英文）、tool、title（中文短标题）、args（对象）、depends_on（依赖的步骤 id 数组）。"#
}

fn chat_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else if b.ends_with("/v1") {
        format!("{b}/chat/completions")
    } else {
        format!("{b}/v1/chat/completions")
    }
}

fn call_chat(
    base_url: &str,
    model: &str,
    api_key: &str,
    timeout_secs: u64,
    system: &str,
    user: &str,
    max_tokens: u32,
    json_mode: bool,
) -> Result<String, String> {
    let url = chat_url(base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.clamp(5, 300)))
        .build()
        .map_err(|e| format!("创建模型客户端失败：{e}"))?;
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.1,
        "max_tokens": max_tokens,
        "stream": false
    });
    if json_mode {
        // llama-server / 多数 OpenAI 兼容服务支持 json_object；用于计划生成，
        // 避免小模型输出解释文字导致解析失败。
        body["response_format"] = json!({ "type": "json_object" });
    }
    let mut req = client.post(&url).json(&body);
    if !api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", api_key.trim()));
    }
    let resp = req.send().map_err(|e| format!("请求模型接口失败：{e}"))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("读取模型响应失败：{e}"))?;
    if !status.is_success() {
        let brief: String = text.chars().take(300).collect();
        return Err(format!("模型接口返回 HTTP {status}：{brief}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("模型响应不是 JSON：{e}"))?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .or_else(|| v["message"]["content"].as_str())
        .or_else(|| v["response"].as_str())
        .ok_or_else(|| "模型响应里没有 content".to_string())?;
    Ok(content.to_string())
}

fn strip_json_fence(raw: &str) -> String {
    let t = raw.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    let start = t.find('{');
    let end = t.rfind('}');
    match (start, end) {
        (Some(a), Some(b)) if b >= a => t[a..=b].to_string(),
        _ => t.trim().to_string(),
    }
}

fn model_plan(
    cfg: &AiConfig,
    goal: &str,
    source_kind: &str,
    source: &str,
    nic_ip: &str,
    api_key: &str,
) -> Result<AiPlan, String> {
    let (provider_name, pcfg) = match cfg.mode.as_str() {
        "local" => ("local", &cfg.local),
        "cloud" => ("cloud", &cfg.cloud),
        _ => return Err("规则模式不调用模型".into()),
    };
    if pcfg.base_url.trim().is_empty() || pcfg.model.trim().is_empty() {
        return Err(format!("{provider_name} 模型接口地址或模型名为空"));
    }
    let system = "你是视听质量检测工作站的编排助手。只输出一个 JSON 对象，不要 Markdown，不要解释。你必须从给定工具清单中选择步骤，不能编造工具名，不能输出任何 shell 命令。计划要最小化：只包含用户明确要求的检测项，以及生成报告所必需的步骤；用户没有说“全面/体检/全部”时，不要自作主张增加微突发、音画同步、合规或播放器。";
    let user = format!(
        "用户目标：{}\n来源类型：{}\n请输出计划 JSON：{{\"summary\":\"一句话说明\",\"steps\":[...]}}。\n{}",
        redact_for_model(goal.trim()),
        source_kind,
        tool_catalog_text()
    );
    let raw = call_chat(
        &pcfg.base_url,
        &pcfg.model,
        api_key,
        pcfg.timeout_secs,
        system,
        &user,
        1600,
        true,
    )?;
    let cleaned = strip_json_fence(&raw);
    let v: Value = match serde_json::from_str(&cleaned) {
        Ok(v) => v,
        Err(e) => {
            // 留一份最近一次失败输出，便于现场排查是截断、乱码还是模型跑题。
            let _ = std::fs::write(
                ai_dir().join("plan-error.txt"),
                format!("{raw}\n\n--- cleaned ---\n{cleaned}"),
            );
            return Err(format!("模型计划不是合法 JSON：{e}"));
        }
    };
    let arr = v["steps"]
        .as_array()
        .ok_or_else(|| "模型计划缺少 steps 数组".to_string())?;
    if arr.is_empty() {
        return Err("模型计划 steps 为空".into());
    }
    let allowed = [
        "capture.run",
        "analysis.run",
        "compliance.run",
        "burst.run",
        "avsync.run",
        "player.play",
        "report.build",
    ];
    let mut steps: Vec<AiStep> = Vec::new();
    for (idx, item) in arr.iter().take(10).enumerate() {
        let tool = item["tool"].as_str().unwrap_or("").trim();
        if !allowed.contains(&tool) {
            return Err(format!("模型返回了不允许的工具：{tool}"));
        }
        let id = item["id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("step{}", idx + 1));
        let title = item["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(tool)
            .to_string();
        let args = if item["args"].is_object() {
            item["args"].clone()
        } else {
            json!({})
        };
        let depends_on = item["depends_on"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        steps.push(AiStep {
            id,
            tool: tool.into(),
            title,
            args,
            depends_on,
        });
    }

    let source_live = source_is_live(source);
    let mut warnings = Vec::new();
    // 用规则计划限定模型可编排的步骤集合：模型仍可决定顺序和标题，但不能
    // 自行增加用户没要求的检测项；漏掉的必需步骤由规则计划补回。
    let baseline = rule_plan(goal, source, nic_ip)?;
    let allowed_tools: HashSet<String> = baseline.steps.iter().map(|s| s.tool.clone()).collect();
    let before_filter = steps.len();
    steps.retain(|s| allowed_tools.contains(&s.tool));
    let trimmed = before_filter - steps.len();
    if trimmed > 0 {
        warnings.push(format!(
            "模型计划包含 {trimmed} 个用户未要求的步骤，已按目标裁剪"
        ));
    }
    let mut seen_tool = HashSet::new();
    steps.retain(|s| seen_tool.insert(s.tool.clone()));
    let mut missing = 0;
    for b in &baseline.steps {
        if steps.iter().any(|s| s.tool == b.tool) {
            continue;
        }
        let mut s = b.clone();
        let mut n = 1;
        while steps.iter().any(|x| x.id == s.id) {
            s.id = format!("{}-{}", b.id, n);
            n += 1;
        }
        steps.push(s);
        missing += 1;
    }
    if missing > 0 {
        warnings.push(format!(
            "模型漏掉 {missing} 个必需步骤，已按规则计划补回"
        ));
    }
    // 模型给的依赖可能指向被裁掉的步骤；由后面的能力校验统一重建。
    for s in &mut steps {
        s.depends_on.clear();
    }
    if steps.is_empty() {
        return Err("模型没有给出适用于当前来源的步骤".into());
    }

    if contains_any(&goal.to_lowercase(), &["word", "docx", "文档"]) {
        warnings.push("AI 汇总报告暂先导出 Markdown/HTML；Word 版可在后续版本接入".into());
    }
    let mut seen_ids = HashSet::new();
    let mut seen_tools = HashSet::new();
    for s in &steps {
        if !seen_ids.insert(s.id.clone()) {
            return Err(format!("模型计划步骤 id 重复：{}", s.id));
        }
        if !seen_tools.insert(s.tool.clone()) {
            return Err(format!("模型计划重复使用工具：{}", s.tool));
        }
    }
    // 模型可能不知道本地文件不能抓包/做微突发；这里做最后一道能力校验。
    if !source_live {
        let before = steps.len();
        steps.retain(|s| s.tool != "capture.run" && s.tool != "burst.run");
        if steps.len() != before {
            warnings.push("本地文件不支持抓包/微突发，已从模型中移除对应步骤".into());
        }
    }
    if steps.is_empty() {
        return Err("模型没有给出适用于当前来源的步骤".into());
    }

    let has_capture = steps.iter().any(|s| s.tool == "capture.run");
    let capture_id = steps
        .iter()
        .find(|s| s.tool == "capture.run")
        .map(|s| s.id.clone());
    for s in &mut steps {
        if !s.args.is_object() {
            s.args = json!({});
        }
        let obj = s.args.as_object_mut().unwrap();
        match s.tool.as_str() {
            "capture.run" => {
                // 来源与网卡只能由程序注入，不能采用模型编造的地址。
                obj.insert("url".into(), json!(source));
                obj.insert("nic_ip".into(), json!(nic_ip));
                obj.entry("duration_sec").or_insert(json!(30));
            }
            "analysis.run" => {
                // 有抓包步骤时，分析对象必须是抓包产出的 TS，不能把原始组播地址当文件。
                if has_capture {
                    obj.insert("source".into(), json!("$capture.ts_path"));
                } else {
                    obj.insert("source".into(), json!(source));
                }
                obj.insert("nic_ip".into(), json!(nic_ip));
                obj.entry("duration_sec").or_insert(json!(30));
                obj.insert("live".into(), json!(!has_capture && source_live));
                if let Some(cid) = &capture_id {
                    if !s.depends_on.contains(cid) {
                        s.depends_on.push(cid.clone());
                    }
                }
            }
            "compliance.run" => {
                obj.insert("source".into(), json!(source));
                obj.insert("nic_ip".into(), json!(nic_ip));
                obj.entry("duration_sec").or_insert(json!(30));
                obj.insert("live".into(), json!(source_live));
                obj.entry("kind").or_insert(json!("IPTV"));
            }
            "burst.run" => {
                obj.insert("url".into(), json!(source));
                obj.insert("nic_ip".into(), json!(nic_ip));
                obj.entry("duration_sec").or_insert(json!(30));
                obj.entry("ratio").or_insert(json!(2.0));
                obj.entry("window_ms").or_insert(json!(1));
            }
            "avsync.run" => {
                obj.insert("source".into(), json!(source));
                obj.insert("nic_ip".into(), json!(nic_ip));
                obj.entry("secs").or_insert(json!(30));
            }
            "player.play" => {
                obj.insert("source".into(), json!(source));
                obj.insert("nic_ip".into(), json!(nic_ip));
            }
            "report.build" => {
                // 模型只能决定“要不要报告”，格式由程序限制在 Markdown/HTML。
                let formats: Vec<String> = obj
                    .get("formats")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_lowercase)
                            .filter(|x| x == "md" || x == "html")
                            .collect()
                    })
                    .unwrap_or_default();
                if formats.is_empty() {
                    obj.insert("formats".into(), json!(["md"]));
                } else {
                    obj.insert("formats".into(), json!(formats));
                }
            }
            _ => {}
        }
    }

    // 如果用户明确要报告，而模型漏掉了，补一个依赖所有检测步骤的报告步骤。
    let wants_report = contains_any(&goal.to_lowercase(), &["报告", "出报告", "生成报告"]);
    if wants_report && !steps.iter().any(|s| s.tool == "report.build") {
        let ids: Vec<String> = steps.iter().map(|s| s.id.clone()).collect();
        steps.push(AiStep {
            id: "report".into(),
            tool: "report.build".into(),
            title: "汇总检测报告".into(),
            args: json!({ "formats": ["md"] }),
            depends_on: ids,
        });
    }

    // 报告必须等所有检测步骤结束；模型给的依赖不全时以执行器为准补齐。
    let detection_ids: Vec<String> = steps
        .iter()
        .filter(|s| s.tool != "report.build")
        .map(|s| s.id.clone())
        .collect();
    for s in &mut steps {
        if s.tool == "report.build" {
            s.depends_on = detection_ids.clone();
        }
    }

    // 清理无效依赖，避免前端执行器空等。
    let ids: Vec<String> = steps.iter().map(|s| s.id.clone()).collect();
    for s in &mut steps {
        let sid = s.id.clone();
        s.depends_on.retain(|d| ids.contains(d) && d != &sid);
    }

    let summary = if trimmed > 0 || missing > 0 {
        format!(
            "按目标编排 {} 个步骤：{}。",
            steps.len(),
            steps.iter().map(|s| s.title.as_str()).collect::<Vec<_>>().join("、")
        )
    } else {
        v["summary"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("模型编排了 {} 个步骤。", steps.len()))
    };

    Ok(AiPlan {
        goal: goal.trim().to_string(),
        summary,
        source: source.to_string(),
        nic_ip: nic_ip.trim().to_string(),
        provider: format!("{}:{}", provider_name, pcfg.model),
        steps,
        warnings,
    })
}

#[tauri::command]
pub async fn ai_plan(
    goal: String,
    source: String,
    nic_ip: String,
    api_key: Option<String>,
) -> Result<AiPlan, String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err("请先输入要做什么".into());
    }
    let raw = source.trim();
    let src = raw.strip_prefix("file://").unwrap_or(raw);
    if src.is_empty() {
        return Err("请先选择来源".into());
    }
    let cfg = load_config();
    if cfg.mode == "rule" {
        return rule_plan(goal, src, &nic_ip);
    }
    let kind = if source_is_live(src) {
        "实时 UDP/RTP 流"
    } else if source_is_file(src) {
        "本地媒体文件"
    } else {
        return Err(format!("来源不存在或不支持：{src}"));
    };
    let key = api_key
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("VQA_AI_API_KEY").ok())
        .unwrap_or_default();
    let cfg2 = cfg.clone();
    let goal2 = goal.to_string();
    let src2 = src.to_string();
    let nic2 = nic_ip.clone();
    let planned = tauri::async_runtime::spawn_blocking(move || {
        model_plan(&cfg2, &goal2, kind, &src2, &nic2, &key)
    })
    .await
    .map_err(|e| format!("模型编排线程失败：{e}"))?;

    match planned {
        Ok(p) => Ok(p),
        Err(e) => {
            let mut fallback = rule_plan(goal, src, &nic_ip)?;
            fallback.warnings.push(format!("模型编排未生效，已改用规则计划：{e}"));
            Ok(fallback)
        }
    }
}

#[tauri::command]
pub async fn ai_test(
    mode: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<Value, String> {
    let mode = mode.trim();
    if mode == "rule" {
        return Ok(json!({ "ok": true, "provider": "rule", "message": "规则模式无需模型" }));
    }
    if mode != "local" && mode != "cloud" {
        return Err(format!("未知模式：{mode}"));
    }
    if base_url.trim().is_empty() || model.trim().is_empty() {
        return Err("接口地址和模型名都要填".into());
    }
    let key = api_key
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("VQA_AI_API_KEY").ok())
        .unwrap_or_default();
    let base = base_url.trim().to_string();
    let model = model.trim().to_string();
    let call_model = model.clone();
    let t0 = Instant::now();
    let r = tauri::async_runtime::spawn_blocking(move || {
        call_chat(
            &base,
            &call_model,
            &key,
            20,
            "你是一个连通性测试端点，只回复 OK。",
            "只回复 OK",
            8,
            false,
        )
    })
    .await
    .map_err(|e| format!("测试线程失败：{e}"))?;
    match r {
        Ok(content) => Ok(json!({
            "ok": true,
            "provider": mode,
            "model": model,
            "latency_ms": t0.elapsed().as_millis() as u64,
            "reply": content.chars().take(40).collect::<String>(),
        })),
        Err(e) => Ok(json!({
            "ok": false,
            "provider": mode,
            "model": model,
            "latency_ms": t0.elapsed().as_millis() as u64,
            "error": e,
        })),
    }
}

#[tauri::command]
pub fn ai_tools() -> Value {
    json!([
        {"name":"capture.run","title":"抓包并取 TS 样本","description":"单路 UDP/RTP 收流，写 pcap 并抽 188 字节 TS 样本；只支持实时流。","read_only":false},
        {"name":"analysis.run","title":"TS 码流结构分析","description":"分析 TS 包长、同步、TR290、节目/PID、码率等；支持实时流和本地 TS 文件。","read_only":false},
        {"name":"compliance.run","title":"合规检测","description":"按 IPTV/CATV 口径检查网络项与节目技术项；实时流或离线文件。","read_only":false},
        {"name":"burst.run","title":"微突发检测","description":"按 1/10/100/1000ms 窗口统计到达字节，识别微突发；只支持实时流。","read_only":false},
        {"name":"avsync.run","title":"音画同步检测","description":"分析音视频相对偏移；支持实时流或本地文件。","read_only":false},
        {"name":"player.play","title":"播放器画面抽检","description":"启动播放器并截一张画面，用于人工复核。","read_only":false},
        {"name":"report.build","title":"汇总检测报告","description":"把各步骤结果汇总为 Markdown/HTML；Word 版暂未接入。","read_only":false}
    ])
}

// ═══════════════════════════ 模型文件 ═══════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstalledModel {
    pub id: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub imported_at: String,
    #[serde(default)]
    pub source: String,
}

pub(crate) fn load_installed() -> Vec<InstalledModel> {
    std::fs::read_to_string(installed_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save_installed(list: &[InstalledModel]) -> Result<(), String> {
    std::fs::create_dir_all(ai_dir()).map_err(|e| format!("建 AI 目录失败：{e}"))?;
    let raw = serde_json::to_string_pretty(list).map_err(|e| format!("序列化模型清单失败：{e}"))?;
    std::fs::write(installed_path(), raw).map_err(|e| format!("写模型清单失败：{e}"))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("打开模型文件失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("读取模型文件失败：{e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn model_info_from_meta(m: &InstalledModel) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "path": m.path,
        "bytes": m.bytes,
        "sha256": m.sha256,
        "version": m.version,
        "imported_at": m.imported_at,
        "source": m.source,
        "present": Path::new(&m.path).is_file(),
    })
}

#[tauri::command]
pub fn ai_model_list() -> Value {
    let meta = load_installed();
    let mut out: Vec<Value> = meta.iter().map(model_info_from_meta).collect();
    // 扫描目录里尚未登记进 installed.json 的 .gguf，避免用户手工放文件后看不到。
    if let Ok(rd) = std::fs::read_dir(models_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("gguf") {
                continue;
            }
            let name = p.file_name().and_then(|x| x.to_str()).unwrap_or("model.gguf");
            let pstr = p.to_string_lossy().to_string();
            if out
                .iter()
                .any(|v| v["path"].as_str() == Some(pstr.as_str()) || v["name"].as_str() == Some(name))
            {
                continue;
            }
            out.push(json!({
                "id": p.file_stem().and_then(|x| x.to_str()).unwrap_or(name),
                "name": name,
                "path": p.to_string_lossy(),
                "bytes": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                "sha256": "",
                "version": "",
                "imported_at": "",
                "source": "scan",
                "present": true,
            }));
        }
    }
    json!(out)
}

fn import_model_file(src: PathBuf) -> Result<Value, String> {
    if !src.is_file() {
        return Err(format!("模型文件不存在：{}", src.display()));
    }
    std::fs::create_dir_all(models_dir()).map_err(|e| format!("建模型目录失败：{e}"))?;
    let name = src
        .file_name()
        .and_then(|x| x.to_str())
        .ok_or_else(|| "模型文件名不可用".to_string())?
        .to_string();
    let dest = models_dir().join(&name);
    if dest.exists() {
        return Err(format!("同名模型已存在：{}；请先移除旧文件再导入", dest.display()));
    }
    std::fs::copy(&src, &dest).map_err(|e| format!("复制模型失败：{e}"))?;
    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    let sha = sha256_file(&dest)?;
    let id = dest
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(&name)
        .to_string();
    let mut list = load_installed();
    list.retain(|m| m.id != id);
    let item = InstalledModel {
        id,
        name,
        path: dest.to_string_lossy().to_string(),
        bytes,
        sha256: sha,
        version: String::new(),
        imported_at: chrono::Local::now().to_rfc3339(),
        source: "import".into(),
    };
    list.push(item.clone());
    save_installed(&list)?;
    Ok(model_info_from_meta(&item))
}

#[tauri::command]
pub async fn ai_model_import(app: AppHandle) -> Result<Value, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("选择 GGUF 模型文件")
        .add_filter("GGUF 模型", &["gguf"])
        .blocking_pick_file();
    let src = picked
        .map(|p| p.to_string())
        .ok_or_else(|| "未选择模型文件".to_string())?;
    let src = PathBuf::from(src);
    // 2.5 GB 级模型复制与 SHA256 放到后台线程，避免卡住窗口。
    tauri::async_runtime::spawn_blocking(move || import_model_file(src))
        .await
        .map_err(|e| format!("导入任务失败：{e}"))?
}

#[tauri::command]
pub async fn ai_model_remove(id: String) -> Result<(), String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("模型 id 为空".into());
    }
    let mut list = load_installed();
    let path = if let Some(pos) = list.iter().position(|m| m.id == id) {
        let m = list.remove(pos);
        PathBuf::from(m.path)
    } else {
        // 目录里手工放入、尚未登记的 .gguf 也允许按文件主名删除。
        let mut found = None;
        if let Ok(rd) = std::fs::read_dir(models_dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("gguf")
                    && p.file_stem().and_then(|x| x.to_str()) == Some(id.as_str())
                {
                    found = Some(p);
                    break;
                }
            }
        }
        found.ok_or_else(|| format!("没有登记这个模型：{id}"))?
    };
    let root = std::fs::canonicalize(models_dir()).unwrap_or_else(|_| models_dir());
    let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if !target.starts_with(&root) {
        return Err(format!("模型路径不在模型目录，拒绝删除：{}", path.display()));
    }
    tauri::async_runtime::spawn_blocking(move || {
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| format!("删除模型文件失败：{e}"))?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("删除任务失败：{e}"))??;
    save_installed(&list)?;
    Ok(())
}

#[tauri::command]
pub fn ai_report_export(name: String, content: String, ext: String) -> Result<String, String> {
    let ext = match ext.trim().to_lowercase().as_str() {
        "md" => "md",
        "html" => "html",
        other => return Err(format!("AI 报告只支持 md/html，收到：{other}")),
    };
    let stem = Path::new(name.trim())
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("AI检测报告")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let stem = if stem.trim().is_empty() {
        "AI检测报告".to_string()
    } else {
        stem.trim().to_string()
    };
    let dir = ai_dir().join("reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建报告目录失败：{e}"))?;
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    std::fs::write(&path, content).map_err(|e| format!("写 AI 报告失败：{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

// ═══════════════════════════ 模型清单与下载 ═══════════════════════════

#[derive(Debug, Clone, Deserialize)]
struct CatalogModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    notes: String,
}

fn parse_catalog(raw: &str) -> Result<Vec<CatalogModel>, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("模型清单不是 JSON：{e}"))?;
    let arr = v["models"]
        .as_array()
        .cloned()
        .or_else(|| v.as_array().cloned())
        .ok_or_else(|| "模型清单缺少 models 数组".to_string())?;
    let models: Vec<CatalogModel> =
        serde_json::from_value(Value::Array(arr)).map_err(|e| format!("模型清单字段无效：{e}"))?;
    Ok(models)
}

/// 读取内置清单或自定义清单。返回（来源说明, 模型列表, 是否内置）。
fn load_catalog(index_url: Option<String>) -> Result<(String, Vec<CatalogModel>, bool), String> {
    let cfg = load_config();
    let url = index_url
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(cfg.models_index_url)
        .trim()
        .to_string();
    if url.is_empty() {
        let models = parse_catalog(BUILTIN_MODELS_INDEX)?;
        return Ok(("内置模型清单".to_string(), models, true));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent(format!("vqa-workbench/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("创建清单客户端失败：{e}"))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("请求模型清单失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("模型清单返回 HTTP {}", resp.status()));
    }
    let text = resp.text().map_err(|e| format!("读取模型清单失败：{e}"))?;
    let models = parse_catalog(&text)?;
    Ok((url, models, false))
}

/// 后端按 id 找已安装模型，供内置 llama-server 启动使用。
pub(crate) fn resolve_model(id: &str) -> Result<(PathBuf, String), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("模型 id 为空".into());
    }
    if let Some(m) = load_installed().into_iter().find(|m| m.id == id) {
        let path = PathBuf::from(&m.path);
        if path.is_file() {
            return Ok((path, m.name));
        }
    }
    if let Ok(rd) = std::fs::read_dir(models_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("gguf") {
                continue;
            }
            if p.file_stem().and_then(|x| x.to_str()) == Some(id) {
                let name = p
                    .file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("model.gguf")
                    .to_string();
                return Ok((p, name));
            }
        }
    }
    Err(format!("模型不存在或文件已丢失：{id}；请重新下载或导入"))
}

/// 内置引擎启动成功后写回本地 OpenAI 兼容端点。
pub(crate) fn set_local_endpoint(base_url: &str, model: &str) -> Result<AiConfig, String> {
    let mut cfg = load_config();
    cfg.mode = "local".into();
    cfg.local.base_url = base_url.trim().to_string();
    cfg.local.model = model.trim().to_string();
    save_config(&cfg)?;
    Ok(cfg)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DownloadStatus {
    pub active: bool,
    pub cancelling: bool,
    pub done: bool,
    pub id: String,
    pub name: String,
    pub downloaded: u64,
    pub total: u64,
    pub percent: f64,
    pub speed_bps: f64,
    pub eta_secs: u64,
    pub error: String,
    pub path: String,
    pub sha256: String,
}

struct DownloadRuntime {
    status: DownloadStatus,
    cancel: Option<Arc<AtomicBool>>,
    generation: u64,
}

impl Default for DownloadRuntime {
    fn default() -> Self {
        Self {
            status: DownloadStatus::default(),
            cancel: None,
            generation: 0,
        }
    }
}

fn download_state() -> &'static Mutex<DownloadRuntime> {
    static STATE: OnceLock<Mutex<DownloadRuntime>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(DownloadRuntime::default()))
}

fn set_download_status(gen: u64, f: impl FnOnce(&mut DownloadStatus)) {
    if let Ok(mut g) = download_state().lock() {
        if g.generation == gen {
            f(&mut g.status);
        }
    }
}

fn safe_model_file_name(entry: &CatalogModel) -> String {
    let candidate = if !entry.file_name.trim().is_empty() {
        entry.file_name.trim().to_string()
    } else if !entry.url.trim().is_empty() {
        entry
            .url
            .split('?')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string()
    } else {
        format!("{}.gguf", entry.id)
    };
    let mut name: String = candidate
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while name.starts_with('.') {
        name.remove(0);
    }
    if name.is_empty() {
        name = format!("{}.gguf", entry.id);
    }
    if !name.to_lowercase().ends_with(".gguf") {
        name.push_str(".gguf");
    }
    name
}

fn register_downloaded_model(
    entry: &CatalogModel,
    path: &Path,
    bytes: u64,
    sha: &str,
) -> Result<(), String> {
    let mut list = load_installed();
    let new_path = path.to_string_lossy().to_string();
    if let Some(pos) = list.iter().position(|m| m.id == entry.id) {
        let old_path = list[pos].path.clone();
        if old_path != new_path {
            let old = PathBuf::from(&old_path);
            let root = std::fs::canonicalize(models_dir()).unwrap_or_else(|_| models_dir());
            let target = std::fs::canonicalize(&old).unwrap_or_else(|_| old.clone());
            if target.starts_with(&root) {
                let _ = std::fs::remove_file(&old);
            }
        }
        list[pos] = InstalledModel {
            id: entry.id.clone(),
            name: entry.name.clone(),
            path: new_path,
            bytes,
            sha256: sha.to_string(),
            version: entry.version.clone(),
            imported_at: chrono::Local::now().to_rfc3339(),
            source: "download".into(),
        };
    } else {
        list.push(InstalledModel {
            id: entry.id.clone(),
            name: entry.name.clone(),
            path: new_path,
            bytes,
            sha256: sha.to_string(),
            version: entry.version.clone(),
            imported_at: chrono::Local::now().to_rfc3339(),
            source: "download".into(),
        });
    }
    save_installed(&list)
}

const DOWNLOAD_CANCELLED: &str = "__download_cancelled__";

fn download_model_inner(
    gen: u64,
    entry: &CatalogModel,
    dest: &Path,
    part: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<(u64, String, String), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // blocking 版没有独立的 read_timeout；用整体上限兜底网络卡死（15 分钟）。
        // 正常下载会持续有数据返回，取消也能在下一次 read 返回后生效。
        .timeout(Duration::from_secs(900))
        .user_agent(format!("vqa-workbench/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("创建下载客户端失败：{e}"))?;
    let mut resp = client
        .get(&entry.url)
        .send()
        .map_err(|e| format!("请求模型下载地址失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("模型下载返回 HTTP {}", resp.status()));
    }
    let header_len = resp.content_length().unwrap_or(0);
    let total = if header_len > 0 { header_len } else { entry.size };
    if total > 0 {
        set_download_status(gen, |s| s.total = total);
    }

    let mut file = std::fs::File::create(part).map_err(|e| format!("创建临时模型文件失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut last_report = Instant::now();
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            return Err(DOWNLOAD_CANCELLED.to_string());
        }
        let n = resp.read(&mut buf).map_err(|e| format!("读取模型下载流失败：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("写入模型文件失败：{e}"))?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;
        if last_report.elapsed() >= Duration::from_millis(240) {
            let secs = started.elapsed().as_secs_f64().max(0.1);
            let speed = downloaded as f64 / secs;
            let eta = if total > downloaded && speed > 0.0 {
                ((total - downloaded) as f64 / speed) as u64
            } else {
                0
            };
            let percent = if total > 0 {
                (downloaded as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            set_download_status(gen, |s| {
                s.downloaded = downloaded;
                s.total = total;
                s.percent = percent;
                s.speed_bps = speed;
                s.eta_secs = eta;
            });
            last_report = Instant::now();
        }
    }
    file.flush().map_err(|e| format!("刷新模型文件失败：{e}"))?;
    drop(file);

    if entry.size > 0 && downloaded != entry.size {
        return Err(format!(
            "模型下载长度不符：清单 {} 字节，实际 {} 字节",
            entry.size, downloaded
        ));
    }
    let actual = hex::encode(hasher.finalize());
    let expected_sha = entry.sha256.trim().to_lowercase();
    if !expected_sha.is_empty() && actual != expected_sha {
        return Err(format!(
            "模型 SHA256 校验失败：期望 {expected_sha}，实际 {actual}"
        ));
    }
    if dest.exists() {
        std::fs::remove_file(dest).map_err(|e| format!("替换旧模型失败：{e}"))?;
    }
    std::fs::rename(part, dest).map_err(|e| format!("保存模型文件失败：{e}"))?;
    let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(downloaded);
    register_downloaded_model(entry, dest, bytes, &actual)?;
    Ok((bytes, actual, dest.to_string_lossy().to_string()))
}

fn download_worker(
    gen: u64,
    entry: CatalogModel,
    dest: PathBuf,
    part: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let result = download_model_inner(gen, &entry, &dest, &part, &cancel);
    match result {
        Ok((bytes, sha, path)) => {
            set_download_status(gen, |s| {
                s.active = false;
                s.cancelling = false;
                s.done = true;
                s.downloaded = bytes;
                s.total = bytes;
                s.percent = 100.0;
                s.speed_bps = 0.0;
                s.eta_secs = 0;
                s.sha256 = sha.clone();
                s.path = path.clone();
            });
        }
        Err(e) if e == DOWNLOAD_CANCELLED => {
            let _ = std::fs::remove_file(&part);
            set_download_status(gen, |s| {
                s.active = false;
                s.cancelling = false;
                s.error = "已取消".into();
            });
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            set_download_status(gen, |s| {
                s.active = false;
                s.cancelling = false;
                s.error = e;
            });
        }
    }
}

#[tauri::command]
pub async fn ai_model_download(
    id: String,
    index_url: Option<String>,
) -> Result<DownloadStatus, String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("模型 id 为空".into());
    }
    if let Ok(g) = download_state().lock() {
        if g.status.active {
            return Err(if g.status.cancelling {
                "正在取消上一个模型下载，请稍候".into()
            } else {
                format!("已有模型正在下载：{}", g.status.name)
            });
        }
    }
    let id2 = id.clone();
    let (_, models, _) =
        tauri::async_runtime::spawn_blocking(move || load_catalog(index_url))
            .await
            .map_err(|e| format!("模型清单线程失败：{e}"))??;
    let entry = models
        .into_iter()
        .find(|m| m.id == id2)
        .ok_or_else(|| format!("模型清单中没有这个模型：{id2}"))?;
    if entry.url.trim().is_empty() {
        return Err("该模型清单项没有下载地址".into());
    }
    let name = if entry.name.trim().is_empty() {
        entry.id.clone()
    } else {
        entry.name.clone()
    };

    // 已安装且文件、SHA 与清单一致时直接视为完成。
    if let Some(local) = load_installed().into_iter().find(|m| m.id == id) {
        if Path::new(&local.path).is_file()
            && !entry.sha256.trim().is_empty()
            && local.sha256.eq_ignore_ascii_case(entry.sha256.trim())
        {
            return Ok(DownloadStatus {
                done: true,
                id,
                name,
                downloaded: local.bytes,
                total: local.bytes.max(entry.size),
                percent: 100.0,
                path: local.path,
                ..DownloadStatus::default()
            });
        }
    }

    std::fs::create_dir_all(models_dir()).map_err(|e| format!("建模型目录失败：{e}"))?;
    // 上次异常退出可能留下 .part；新下载开始前清掉，避免磁盘越积越多。
    if let Ok(rd) = std::fs::read_dir(models_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
            if name.starts_with('.') && name.ends_with(".part") {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let file_name = safe_model_file_name(&entry);
    let dest = models_dir().join(&file_name);
    let generation = {
        let mut g = download_state()
            .lock()
            .map_err(|e| format!("下载状态锁失败：{e}"))?;
        // 前面读清单是异步的，期间可能有另一个下载已启动；落状态前再查一次。
        if g.status.active {
            return Err(if g.status.cancelling {
                "正在取消上一个模型下载，请稍候".into()
            } else {
                format!("已有模型正在下载：{}", g.status.name)
            });
        }
        g.generation = g.generation.wrapping_add(1);
        let gen = g.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        g.cancel = Some(cancel.clone());
        g.status = DownloadStatus {
            active: true,
            id: id.clone(),
            name: name.clone(),
            total: entry.size,
            ..DownloadStatus::default()
        };
        let part = models_dir().join(format!(".{file_name}.{gen}.part"));
        let entry2 = entry.clone();
        let dest2 = dest.clone();
        let cancel2 = cancel.clone();
        std::thread::Builder::new()
            .name("vqa-model-download".into())
            .spawn(move || download_worker(gen, entry2, dest2, part, cancel2))
            .map_err(|e| format!("启动模型下载线程失败：{e}"))?;
        gen
    };
    let _ = generation;
    Ok(download_state()
        .lock()
        .map(|g| g.status.clone())
        .unwrap_or_default())
}

#[tauri::command]
pub fn ai_model_download_status() -> DownloadStatus {
    download_state()
        .lock()
        .map(|g| g.status.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub fn ai_model_download_cancel() -> DownloadStatus {
    if let Ok(mut g) = download_state().lock() {
        if g.status.active {
            g.status.cancelling = true;
            if let Some(c) = &g.cancel {
                c.store(true, Ordering::Relaxed);
            }
        }
        return g.status.clone();
    }
    DownloadStatus::default()
}

#[tauri::command]
pub async fn ai_model_check(index_url: Option<String>) -> Result<Value, String> {
    let installed = load_installed();
    let checked = tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let (source, models, builtin) = load_catalog(index_url)?;
        let mut out = Vec::new();
        for m in models {
            let id = m.id.trim().to_string();
            if id.is_empty() {
                continue;
            }
            let latest = m.version.clone();
            let sha = m.sha256.clone();
            let local = installed.iter().find(|x| x.id == id);
            let installed_version = local.map(|x| x.version.clone()).unwrap_or_default();
            let installed_sha = local.map(|x| x.sha256.clone()).unwrap_or_default();
            let update = match local {
                None => false,
                Some(_) => {
                    (!latest.is_empty() && latest != installed_version)
                        || (!sha.is_empty() && !installed_sha.is_empty() && sha != installed_sha)
                }
            };
            out.push(json!({
                "id": id,
                "name": if m.name.trim().is_empty() { m.id.clone() } else { m.name.clone() },
                "version": latest,
                "size": m.size,
                "sha256": sha,
                "url": m.url,
                "file_name": m.file_name,
                "notes": m.notes,
                "installed": local.is_some(),
                "installed_version": installed_version,
                "update_available": update,
            }));
        }
        Ok(json!({
            "index_url": source,
            "builtin": builtin,
            "checked_at": chrono::Local::now().to_rfc3339(),
            "models": out,
        }))
    })
    .await
    .map_err(|e| format!("模型检查线程失败：{e}"))?;
    checked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_goal_hides_source_addresses() {
        let g = redact_for_model("对 udp://@239.1.1.1:1234 抓包 30 秒，并分析 D:\\tmp\\a.ts");
        assert!(!g.contains("239.1.1.1"));
        assert!(!g.contains("udp://"));
        assert!(!g.contains("D:\\tmp"));
        assert!(g.contains("抓包 30 秒"));
        assert!(!redact_for_model("分析 D:/tmp/a.ts").contains("D:/tmp"));
    }

    #[test]
    fn live_chain_orders_analysis_after_capture() {
        let p = rule_plan("抓包 30 秒，分析 TS 并生成报告", "udp://@239.1.1.1:1234", "").unwrap();
        assert!(p.steps.iter().any(|s| s.tool == "capture.run"));
        let a = p.steps.iter().find(|s| s.tool == "analysis.run").unwrap();
        assert_eq!(a.args["source"], "$capture.ts_path");
        assert!(a.depends_on.contains(&"capture".to_string()));
        let r = p.steps.iter().find(|s| s.tool == "report.build").unwrap();
        assert!(r.depends_on.contains(&"capture".to_string()));
        assert!(r.depends_on.contains(&"analysis".to_string()));
    }

    #[test]
    fn local_file_skips_live_only_modules() {
        let path = std::env::temp_dir().join(format!("vqa_ai_test_{}.ts", std::process::id()));
        std::fs::write(&path, b"not a real ts").unwrap();
        let p = rule_plan("全面体检，出报告", &path.to_string_lossy(), "").unwrap();
        assert!(!p.steps.iter().any(|s| s.tool == "capture.run"));
        assert!(!p.steps.iter().any(|s| s.tool == "burst.run"));
        assert!(p.steps.iter().any(|s| s.tool == "analysis.run"));
        assert!(p.steps.iter().any(|s| s.tool == "report.build"));
        assert!(p.warnings.iter().any(|w| w.contains("抓包")));
        let _ = std::fs::remove_file(path);
    }
}
