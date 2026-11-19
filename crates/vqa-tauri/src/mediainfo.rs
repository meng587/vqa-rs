//! 媒体信息 —— ffprobe 全量探测（容器 + 每条流的编码/分辨率/帧率/码率/色彩等）。
//!
//! 与 `probe::probe_quick` 的区别：那个只取起播决策要用的 5 个字段（毫秒级）；
//! 这里是"媒体信息"面板要的完整报告，一次性 `-show_format -show_streams`，
//! 原样 JSON 也一并回给前端（详情可展开看）。

use serde_json::{json, Value};

fn no_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = &cmd;
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|f| f.is_finite())
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty() && s != "unknown")
}

/// 探测一个来源（本地文件或 HTTP/UDP 流），返回给前端直接渲染的摘要 + 原始 JSON。
pub fn probe(path: &str) -> Result<Value, String> {
    let ff = vqa_core::tools::find_ffprobe()
        .ok_or("未找到 ffprobe.exe（检查 tools 查找链）")?;
    let ctor: fn(std::ffi::OsString) -> std::process::Command = std::process::Command::new;
    let mut cmd = ctor(ff.as_os_str().to_os_string());
    cmd.args([
        "-v",
        "error",
        "-show_format",
        "-show_streams",
        "-print_format",
        "json",
        path,
    ]);
    no_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("启动 ffprobe 失败：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail = err.trim().lines().last().unwrap_or("未知错误").to_string();
        return Err(format!("ffprobe 无法解析该来源：{tail}"));
    }
    let raw: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("解析 ffprobe 输出失败：{e}"))?;
    Ok(summarize(path, &raw))
}

fn summarize(path: &str, raw: &Value) -> Value {
    let fmt = raw.get("format").cloned().unwrap_or(json!({}));
    let streams = raw
        .get("streams")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    // 时长：容器优先，缺失时取各流最大值（裸流常见）
    let duration = num(&fmt, "duration").or_else(|| {
        streams.iter().filter_map(|s| num(s, "duration")).fold(None, |a: Option<f64>, b| {
            Some(a.map(|x| x.max(b)).unwrap_or(b))
        })
    });

    let format_info = json!({
        "container": text(&fmt, "format_name"),
        "format_long": text(&fmt, "format_long_name"),
        "duration_s": duration,
        "size_bytes": num(&fmt, "size"),
        "bit_rate": num(&fmt, "bit_rate"),
        "nb_streams": streams.len(),
        "probe_score": num(&fmt, "probe_score"),
    });

    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let mut others = Vec::new();

    for s in &streams {
        let kind = text(s, "codec_type").unwrap_or_default();
        let idx = num(s, "index").unwrap_or(0.0) as u32;
        match kind.as_str() {
            "video" => videos.push(json!({
                "index": idx,
                "codec": text(s, "codec_name"),
                "codec_long": text(s, "codec_long_name"),
                "profile": text(s, "profile"),
                "width": num(s, "width"),
                "height": num(s, "height"),
                "pix_fmt": text(s, "pix_fmt"),
                "avg_frame_rate": text(s, "avg_frame_rate"),
                "r_frame_rate": text(s, "r_frame_rate"),
                "bit_rate": num(s, "bit_rate"),
                "color_space": text(s, "color_space"),
                "color_transfer": text(s, "color_transfer"),
                "color_primaries": text(s, "color_primaries"),
                "field_order": text(s, "field_order"),
                "sample_aspect_ratio": text(s, "sample_aspect_ratio"),
                "display_aspect_ratio": text(s, "display_aspect_ratio"),
                "level": num(s, "level"),
                "nb_frames": num(s, "nb_frames"),
                "duration_s": num(s, "duration"),
                "language": s.get("tags").and_then(|t| text(t, "language")),
            })),
            "audio" => audios.push(json!({
                "index": idx,
                "codec": text(s, "codec_name"),
                "codec_long": text(s, "codec_long_name"),
                "profile": text(s, "profile"),
                "sample_rate": num(s, "sample_rate"),
                "channels": num(s, "channels"),
                "channel_layout": text(s, "channel_layout"),
                "bit_rate": num(s, "bit_rate"),
                "duration_s": num(s, "duration"),
                "language": s.get("tags").and_then(|t| text(t, "language")),
            })),
            _ => others.push(json!({
                "index": idx,
                "type": kind,
                "codec": text(s, "codec_name"),
                "language": s.get("tags").and_then(|t| text(t, "language")),
            })),
        }
    }

    json!({
        "path": path,
        "format": format_info,
        "video": videos,
        "audio": audios,
        "others": others,
        "raw": raw,
    })
}
