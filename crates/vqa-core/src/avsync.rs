//! 音画同步检测：画面活动度 × 音频包络 互相关求偏移。
//!
//! 思路（无解码依赖，纯 ffmpeg 抽帧/抽 PCM）：
//! 1) 视频侧：`fps=F,scale=64:36,format=gray` 逐帧求「帧间差异」→ 画面活动度序列；
//! 2) 音频侧：`ac=1,ar=16k,s16le` 按 40ms 求 RMS → 音频包络，再按 F 帧率对齐；
//! 3) 两侧取「正向突变」（onset）并轻度平滑、去均值归一化；
//! 4) 在 ±max_lag 帧内滑动求 Pearson 相关，峰值处即音频相对视频的滞后量。
//!
//! 只关心相对偏移，与容器 start_time 差值天然吻合（容器里的音画起始差就是偏移本身）。

use anyhow::Result;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const VW: usize = 64;
const VH: usize = 36;
const VSZ: usize = VW * VH;
const AR: usize = 16000;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct AvSyncCfg {
    /// 本地文件或 udp/rtp/rtsp 地址
    pub source: String,
    /// 分析时长（秒）；实时流建议 20–60
    pub secs: f64,
    /// 最大搜索偏移（毫秒，双向）
    pub search_ms: i32,
    /// 视频侧采样帧率
    pub fps: f64,
    /// 组播输入绑定的本机网卡 IP；空 = 自动路由
    #[serde(default)]
    pub nic_ip: String,
}

impl Default for AvSyncCfg {
    fn default() -> Self {
        Self { source: String::new(), secs: 20.0, search_ms: 800, fps: 25.0, nic_ip: String::new() }
    }
}

#[derive(Clone, serde::Serialize)]
pub struct LagPeak {
    pub offset_ms: i32,
    pub corr: f64,
}

#[derive(Clone, serde::Serialize)]
pub struct AvSyncResult {
    pub ok: bool,
    pub source: String,
    pub secs: f64,
    pub fps: f64,
    /// 音频相对视频的滞后（正 = 音频晚）
    pub offset_ms: i32,
    /// 峰值相关系数（0–1）
    pub confidence: f64,
    /// 前几个相关峰，供判断是否有歧义
    pub peaks: Vec<LagPeak>,
    pub video_frames: usize,
    pub audio_windows: usize,
    /// 判定文案
    pub verdict: String,
    /// 下采样后的画面/音频活动度（画图用）
    pub series_video: Vec<f32>,
    pub series_audio: Vec<f32>,
    pub err: Option<String>,
}

impl AvSyncResult {
    pub fn fail(source: &str, msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            source: source.to_string(),
            secs: 0.0,
            fps: 25.0,
            offset_ms: 0,
            confidence: 0.0,
            peaks: Vec::new(),
            video_frames: 0,
            audio_windows: 0,
            verdict: String::new(),
            series_video: Vec::new(),
            series_audio: Vec::new(),
            err: Some(msg.into()),
        }
    }
}

fn ffmpeg_exe() -> Result<PathBuf> {
    crate::tools::find_ffmpeg().ok_or_else(|| anyhow::anyhow!("未找到 ffmpeg（请检查 bin 目录或设置 ffmpeg_path）"))
}

fn spawn_gray(ff: &PathBuf, src: &str, secs: f64, fps: f64, nic_ip: &str) -> Result<Child> {
    let vf = format!("fps={fps},scale={VW}:{VH},format=gray");
    let mut c: Command = crate::ffmpeg::hidden(ff);
    c.args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-t", &format!("{:.2}", secs.max(1.0))])
        .args(crate::netif::bind_args(src, nic_ip))
        .args(["-i", src, "-an", "-vf", &vf])
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(c.spawn()?)
}

fn spawn_pcm(ff: &PathBuf, src: &str, secs: f64, nic_ip: &str) -> Result<Child> {
    let mut c: Command = crate::ffmpeg::hidden(ff);
    c.args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-t", &format!("{:.2}", secs.max(1.0))])
        .args(crate::netif::bind_args(src, nic_ip))
        .args(["-i", src, "-vn", "-ac", "1", "-ar", &AR.to_string()])
        .args(["-f", "s16le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(c.spawn()?)
}

fn onset(series: &[f32]) -> Vec<f32> {
    // 正突变 + 轻度三角平滑，让尖锐事件在 ±1 帧内有响应，便于互相关
    let mut d = vec![0f32; series.len()];
    for i in 1..series.len() {
        let v = series[i] - series[i - 1];
        if v > 0.0 {
            d[i] = v;
        }
    }
    if d.len() >= 3 {
        let mut out = vec![0f32; d.len()];
        for i in 1..d.len() - 1 {
            out[i] = 0.25 * d[i - 1] + 0.5 * d[i] + 0.25 * d[i + 1];
        }
        out
    } else {
        d
    }
}

fn normalize(v: &mut [f32]) {
    let n = v.len() as f32;
    if n == 0.0 {
        return;
    }
    let mean: f32 = v.iter().sum::<f32>() / n;
    for x in v.iter_mut() {
        *x -= mean;
    }
    let var: f32 = v.iter().map(|x| x * x).sum::<f32>() / n;
    let sd = var.sqrt();
    if sd > 1e-6 {
        for x in v.iter_mut() {
            *x /= sd;
        }
    }
}

fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n < 4 {
        return 0.0;
    }
    let mut sxy = 0f64;
    let mut sxx = 0f64;
    let mut syy = 0f64;
    for i in 0..n {
        let x = a[i] as f64;
        let y = b[i] as f64;
        sxy += x * y;
        sxx += x * x;
        syy += y * y;
    }
    let den = (sxx * syy).sqrt();
    if den < 1e-9 {
        0.0
    } else {
        (sxy / den).clamp(-1.0, 1.0)
    }
}

/// 在 ±max_lag 帧内滑动求相关。返回 (lag, corr)；lag>0 表示音频事件更晚（音频滞后）。
fn search_lag(video: &[f32], audio: &[f32], max_lag: i32) -> Vec<(i32, f64)> {
    let n = video.len().min(audio.len());
    if n < 8 {
        return Vec::new();
    }
    let mut out: Vec<(i32, f64)> = Vec::new();
    for lag in -max_lag..=max_lag {
        // audio_shifted[i] = audio[i + lag]（lag>0 表示音频事件更晚，需要往后取）
        let (a_start, v_start, len) = if lag >= 0 {
            (lag as usize, 0usize, n.saturating_sub(lag as usize))
        } else {
            (0usize, (-lag) as usize, n.saturating_sub((-lag) as usize))
        };
        if len < 8 {
            continue;
        }
        let va = &video[v_start..v_start + len];
        let aa = &audio[a_start..a_start + len];
        out.push((lag, pearson(va, aa)));
    }
    // 取 3 个互不相邻的局部峰
    let mut peaks: Vec<(i32, f64)> = Vec::new();
    let mut used: Vec<i32> = Vec::new();
    for _ in 0..3 {
        let mut best: Option<(i32, f64)> = None;
        for (lag, corr) in out.iter() {
            if used.iter().any(|u| (u - lag).abs() < 4) {
                continue;
            }
            if best.map(|(_, c)| *corr > c).unwrap_or(true) {
                best = Some((*lag, *corr));
            }
        }
        match best {
            Some((lag, corr)) if corr > 0.05 => {
                used.push(lag);
                peaks.push((lag, corr));
            }
            _ => break,
        }
    }
    peaks

}

pub fn analyze(
    cfg: &AvSyncCfg,
    progress: &mut dyn FnMut(u8),
    cancel: Arc<AtomicBool>,
) -> AvSyncResult {
    let src = cfg.source.trim().to_string();
    if src.is_empty() {
        return AvSyncResult::fail(&src, "来源为空");
    }
    let fps = if cfg.fps > 1.0 { cfg.fps } else { 25.0 };
    let secs = cfg.secs.clamp(3.0, 600.0);
    let max_lag = (((cfg.search_ms.max(40) as f64) * fps / 1000.0).round() as i32).clamp(2, 200);

    let Ok(ff) = ffmpeg_exe() else {
        return AvSyncResult::fail(&src, "未找到 ffmpeg");
    };

    let (mut vch, mut ach) = match (
        spawn_gray(&ff, &src, secs, fps, &cfg.nic_ip),
        spawn_pcm(&ff, &src, secs, &cfg.nic_ip),
    ) {
        (Ok(v), Ok(a)) => (v, a),
        (v, a) => {
            let _ = v.map(|mut c| {
                let _ = c.kill();
            });
            let _ = a.map(|mut c| {
                let _ = c.kill();
            });
            return AvSyncResult::fail(&src, "ffmpeg 启动失败（来源不可读或参数不支持）");
        }
    };

    // 音频在独立线程读，避免两个管道互相阻塞
    let want_ms = ((secs * fps) as usize).max(1);
    let cancel_a = cancel.clone();
    let audio_th = std::thread::spawn(move || -> Vec<f32> {
        let mut env = Vec::new();
        if let Some(mut so) = ach.stdout.take() {
            let win = AR * 40 / 1000; // 40ms
            let mut buf = vec![0u8; win * 2];
            loop {
                if cancel_a.load(Ordering::Relaxed) {
                    break;
                }
                match so.read_exact(&mut buf) {
                    Ok(()) => {
                        let mut acc = 0f64;
                        for c in buf.chunks_exact(2) {
                            let s = i16::from_le_bytes([c[0], c[1]]) as f64;
                            acc += s * s;
                        }
                        let rms = (acc / win as f64).sqrt();
                        env.push(rms as f32);
                        if env.len() > 20000 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = ach.wait();
        env
    });

    let mut frames = 0usize;
    let mut prev = vec![0u8; VSZ];
    let mut first = true;
    let mut act: Vec<f32> = Vec::new();
    if let Some(mut so) = vch.stdout.take() {
        let mut buf = vec![0u8; VSZ];
        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            match so.read_exact(&mut buf) {
                Ok(()) => {
                    let mut diff = 0f64;
                    for i in 0..VSZ {
                        diff += (buf[i] as f64 - prev[i] as f64).abs();
                    }
                    let a = (diff / VSZ as f64) as f32;
                    act.push(if first { 0.0 } else { a });
                    first = false;
                    prev.copy_from_slice(&buf);
                    frames += 1;
                    if frames % 8 == 0 {
                        let pct = ((frames as f32 / want_ms as f32) * 50.0).min(50.0) as u8;
                        progress(pct);
                    }
                }
                Err(_) => break,
            }
        }
    }
    let _ = vch.wait();
    progress(55);

    let audio_env = audio_th.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return AvSyncResult::fail(&src, "已取消");
    }
    if frames < 10 || audio_env.len() < 10 {
        return AvSyncResult::fail(
            &src,
            format!("样本不足（视频 {frames} 帧 / 音频 {} 个窗口），请确认源同时含音视频且时长足够", audio_env.len()),
        );
    }
    progress(70);

    // 音频包络（100Hz）重采样到视频帧率网格：每帧取该帧时间窗内的 RMS 均值
    let audio_series = {
        let ratio = 25.0 / fps; // 每帧对应多少个 40ms 窗口（每秒 25 个窗口）
        let n_v = frames.min((audio_env.len() as f64 / ratio).floor() as usize + 1);
        let mut out = vec![0f32; n_v.max(1)];
        for i in 0..out.len() {
            let s = ((i as f64) * ratio).round() as usize;
            let e = (((i + 1) as f64) * ratio).round() as usize;
            let e = e.min(audio_env.len()).max(s + 1);
            let sl = &audio_env[s.min(audio_env.len() - 1)..e.min(audio_env.len())];
            let m = if sl.is_empty() { 0.0 } else { sl.iter().sum::<f32>() / sl.len() as f32 };
            out[i] = m;
        }
        out
    };

    let mut v_on = onset(&act);
    let mut a_on = onset(&audio_series);
    normalize(&mut v_on);
    normalize(&mut a_on);
    progress(85);

    let peaks: Vec<LagPeak> = search_lag(&v_on, &a_on, max_lag)
        .into_iter()
        .map(|(lag, corr)| LagPeak { offset_ms: (lag as f64 * 1000.0 / fps).round() as i32, corr })
        .collect();
    let (offset_ms, confidence) = peaks
        .first()
        .map(|p| (p.offset_ms, p.corr))
        .unwrap_or((0, 0.0));

    let verdict = if peaks.is_empty() {
        "无有效相关峰（音视频缺少同步的突发事件，可换更长的样本）".to_string()
    } else if offset_ms.abs() <= 40 {
        format!("同步（偏差 {offset_ms} ms，相关度 {:.2}）", confidence)
    } else if offset_ms.abs() <= 120 {
        format!("轻微偏移（音频{} {offset_ms} ms，相关度 {:.2}）", if offset_ms > 0 { "滞后" } else { "超前" }, confidence)
    } else {
        format!("不同步（音频{} {} ms，相关度 {:.2}）", if offset_ms > 0 { "滞后" } else { "超前" }, offset_ms.abs(), confidence)
    };

    // 画图抽稀到 ≤ 600 点
    let ds = |v: &[f32]| -> Vec<f32> {
        let n = v.len();
        if n <= 600 {
            return v.iter().map(|x| (*x * 100.0).round() / 100.0).collect();
        }
        let step = n / 600;
        (0..600).map(|i| (v[i * step] * 100.0).round() / 100.0).collect()
    };

    progress(100);
    AvSyncResult {
        ok: true,
        source: src,
        secs,
        fps,
        offset_ms,
        confidence,
        peaks,
        video_frames: frames,
        audio_windows: audio_env.len(),
        verdict,
        series_video: ds(&v_on),
        series_audio: ds(&a_on),
        err: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onset_and_lag_found() {
        // 构造视频事件在 t=100，音频事件在 t=110（滞后 10 帧 = 400ms@25fps）
        let mut v = vec![0f32; 300];
        let mut a = vec![0f32; 300];
        v[100] = 1.0;
        v[101] = 1.0;
        a[110] = 1.0;
        a[111] = 1.0;
        let v_on = onset(&v);
        let a_on = onset(&a);
        let peaks = search_lag(&v_on, &a_on, 60);
        assert!(!peaks.is_empty());
        let offset_ms = (peaks[0].0 as f64 * 1000.0 / 25.0) as i32;
        assert!((offset_ms - 400).abs() <= 80, "offset={offset_ms}");
    }

    #[test]
    fn pearson_identical_is_one() {
        let base = vec![1.0f32, 2.0, 3.0, 2.0, 1.0, 0.0];
        let mut a = base.clone();
        let mut b = base.clone();
        normalize(&mut a);
        normalize(&mut b);
        assert!((pearson(&a, &b) - 1.0).abs() < 1e-6);
    }
}
