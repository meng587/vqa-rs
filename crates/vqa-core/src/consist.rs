//! 一致性比对：参考样本 vs 待测样本，按帧指纹（dHash）比对，自动搜索时间偏移。
//!
//! 用途：同一节目在不同链路/不同码率/不同时间点上的一致性核查（回看与直播、
//! 备份流与主路、转码前后），以及台标一致性（可限定台标区域）。
//!
//! 指纹方案：`fps=F,scale=9:8,format=gray` 逐帧取 72 字节 → 行内梯度比较得 64bit
//! dHash。dHash 对缩放/压缩/轻微色差稳定，逐帧汉明距离即相似度（1 - d/64）。

use anyhow::Result;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const HW: usize = 8;
const HH: usize = 8;
const HSZ: usize = HW * HH;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct ConsistCfg {
    pub reference: String,
    pub target: String,
    /// 采样帧率（1 = 每秒 1 帧）
    pub fps: f64,
    /// 比对时长（秒），0 表示整段（按参考样本时长）
    pub secs: f64,
    /// 允许的时间偏移搜索范围（秒，双向）
    pub max_lag_s: f64,
    /// 相似度低于该值记为一个差异段
    pub thr: f64,
    /// 台标区域（x, y, w, h 为百分比 0–1），None = 整帧
    pub logo: Option<[f64; 4]>,
}

impl Default for ConsistCfg {
    fn default() -> Self {
        Self {
            reference: String::new(),
            target: String::new(),
            fps: 1.0,
            secs: 0.0,
            max_lag_s: 3.0,
            thr: 0.75,
            logo: None,
        }
    }
}

#[derive(Clone, serde::Serialize)]
pub struct BadSeg {
    pub from_s: f64,
    pub to_s: f64,
    pub sim: f64,
}

#[derive(Clone, serde::Serialize)]
pub struct ConsistResult {
    pub ok: bool,
    pub reference: String,
    pub target: String,
    pub fps: f64,
    pub frames_ref: usize,
    pub frames_tgt: usize,
    /// 参考相对待测的偏移（正 = 待测样本内容更晚）
    pub best_offset_ms: i32,
    /// 对齐后的平均相似度（0–1）
    pub mean_sim: f64,
    pub min_sim: f64,
    pub bad_count: usize,
    pub bad_segments: Vec<BadSeg>,
    /// 对齐区间内逐帧相似度（抽稀 ≤ 600）
    pub series_sim: Vec<f32>,
    pub verdict: String,
    pub note: String,
    pub err: Option<String>,
}

impl ConsistResult {
    pub fn fail(reference: &str, target: &str, msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            reference: reference.to_string(),
            target: target.to_string(),
            fps: 1.0,
            frames_ref: 0,
            frames_tgt: 0,
            best_offset_ms: 0,
            mean_sim: 0.0,
            min_sim: 0.0,
            bad_count: 0,
            bad_segments: Vec::new(),
            series_sim: Vec::new(),
            verdict: String::new(),
            note: String::new(),
            err: Some(msg.into()),
        }
    }
}

fn ffmpeg_exe() -> Result<PathBuf> {
    crate::tools::find_ffmpeg().ok_or_else(|| anyhow::anyhow!("未找到 ffmpeg（请检查 bin 目录或设置 ffmpeg_path）"))
}

fn spawn_hash(ff: &PathBuf, src: &str, secs: f64, fps: f64, logo: Option<[f64; 4]>) -> Result<Child> {
    let base = format!("fps={fps}");
    let vf = match logo {
        Some([x, y, w, h]) => {
            let (x, y, w, h) = (
                x.clamp(0.0, 0.95),
                y.clamp(0.0, 0.95),
                w.clamp(0.02, 1.0),
                h.clamp(0.02, 1.0),
            );
            format!(
                "{base},crop=iw*{w:.3}:ih*{h:.3}:iw*{x:.3}:ih*{y:.3},scale={HW}:{HH},format=gray"
            )
        }
        None => format!("{base},scale={HW}:{HH},format=gray"),
    };
    let mut c: Command = crate::ffmpeg::hidden(ff);
    c.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    if secs > 0.5 {
        c.args(["-t", &format!("{secs:.2}")]);
    }
    c.args(["-i", src, "-an", "-vf", &vf])
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(c.spawn()?)
}

/// 读整段帧指纹（64bit dHash），读完返回。
fn read_hashes(ch: &mut Child, cancel: &Arc<AtomicBool>, max: usize, progress: &mut dyn FnMut(usize)) -> Vec<u64> {
    let mut out = Vec::new();
    if let Some(mut so) = ch.stdout.take() {
        let mut buf = vec![0u8; HSZ];
        loop {
            if cancel.load(Ordering::Relaxed) || out.len() >= max {
                break;
            }
            match so.read_exact(&mut buf) {
                Ok(()) => {
                    // aHash：8×8 均值网格，逐点与整帧均值比较——对亮度/码率差异鲁棒，
                    // 又比 9×8 横向梯度更能区分不同画面
                    let mean: u32 = buf.iter().map(|v| *v as u32).sum::<u32>() / HSZ as u32;
                    let mut h: u64 = 0;
                    for i in 0..HSZ {
                        if buf[i] as u32 > mean {
                            h |= 1u64 << i;
                        }
                    }
                    out.push(h);
                    if out.len() % 16 == 0 {
                        progress(out.len());
                    }
                }
                Err(_) => break,
            }
        }
    }
    // 读满上限或要取消时 ffmpeg 还在往管道写，必须先杀掉再 wait，
    // 否则管道塞满后子进程阻塞、wait 永不返回
    let _ = ch.kill();
    let _ = ch.wait();
    out
}
fn dist(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

pub fn compare(
    cfg: &ConsistCfg,
    progress: &mut dyn FnMut(u8),
    cancel: Arc<AtomicBool>,
) -> ConsistResult {
    let r = cfg.reference.trim().to_string();
    let t = cfg.target.trim().to_string();
    if r.is_empty() || t.is_empty() {
        return ConsistResult::fail(&r, &t, "参考源与待测源都要填");
    }
    let fps = if cfg.fps > 0.2 { cfg.fps } else { 1.0 };
    let clip = cfg.secs.clamp(0.0, 3600.0);
    let Ok(ff) = ffmpeg_exe() else {
        return ConsistResult::fail(&r, &t, "未找到 ffmpeg");
    };

    let (mut rc, mut tc) = match (
        spawn_hash(&ff, &r, clip, fps, cfg.logo),
        spawn_hash(&ff, &t, clip, fps, cfg.logo),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            let _ = a.map(|mut c| {
                let _ = c.kill();
            });
            let _ = b.map(|mut c| {
                let _ = c.kill();
            });
            return ConsistResult::fail(&r, &t, "ffmpeg 启动失败（来源不可读或参数不支持）");
        }
    };

    let cap = ((clip.max(1.0) * fps) as usize).max(32);
    let cancel_t = cancel.clone();
    let t_th = std::thread::spawn(move || read_hashes(&mut tc, &cancel_t, cap * 2, &mut |_| {}));

    let rh = read_hashes(&mut rc, &cancel, cap * 2, &mut |n| {
        let pct = ((n as f32 / cap as f32) * 45.0).min(45.0) as u8;
        progress(pct);
    });
    let th = t_th.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return ConsistResult::fail(&r, &t, "已取消");
    }
    if rh.len() < 8 || th.len() < 8 {
        return ConsistResult::fail(
            &r,
            &t,
            format!("样本太短（参考 {} 帧 / 待测 {} 帧），请加长比对时长或提高采样帧率", rh.len(), th.len()),
        );
    }
    progress(60);

    // 粗对齐：对整个重叠区做偏移搜索，代价 = 偏移数 × 帧数；帧数大时先按 1/4 抽样定粗偏移
    let max_lag = ((cfg.max_lag_s.max(0.2) * fps).round() as i32).clamp(2, 600);
    let step = ((rh.len().min(th.len()) / 400).max(1));
    // 打分：(平均汉明距离, 负重叠帧数, |lag|) 字典序取最小——
    // 静态画面会在多个 lag 上并列满分，此时优先更大重叠、再优先更小偏移
    let mut best: Option<(i32, f64, usize)> = None;
    for lag in -max_lag..=max_lag {
        if (lag & 7) == 0 && cancel.load(Ordering::Relaxed) {
            return ConsistResult::fail(&r, &t, "已取消");
        }
        let (rs, ts, len) = if lag >= 0 {
            (0usize, lag as usize, th.len().saturating_sub(lag as usize))
        } else {
            ((-lag) as usize, 0usize, rh.len().saturating_sub((-lag) as usize))
        };
        // 重叠长度必须同时受两路帧数约束：负 lag 时若只按参考长度算，
        // 下面的 th[ts + i] 会越界 panic（线程卡在 running=true、进度停在 60%）
        let len = len
            .min(rh.len().saturating_sub(rs))
            .min(th.len().saturating_sub(ts));
        if len < 8 {
            continue;
        }
        let mut acc = 0f64;
        let mut n = 0f64;
        let mut i = 0usize;
        while i < len {
            acc += dist(rh[rs + i], th[ts + i]) as f64;
            n += 1.0;
            i += step;
        }
        if n < 4.0 {
            continue;
        }
        let mean = acc / n;
        let cand = (lag, mean, len);
        let take = match best {
            None => true,
            Some((bl, bm, blen)) => {
                if mean < bm - 1e-9 {
                    true
                } else if (mean - bm).abs() <= 1e-9 {
                    if len > blen {
                        true
                    } else {
                        len == blen && lag.abs() < bl.abs()
                    }
                } else {
                    false
                }
            }
        };
        if take {
            best = Some(cand);
        }
    }
    let lag = best.map(|b| b.0).unwrap_or(0);
    progress(78);

    // 精细：在整段上逐帧算相似度（不做二次搜索，粗对齐已足够）
    let (rs, ts, len) = if lag >= 0 {
        (0usize, lag as usize, th.len().saturating_sub(lag as usize))
    } else {
        ((-lag) as usize, 0usize, rh.len().saturating_sub((-lag) as usize))
    };
    let len = len
        .min(rh.len().saturating_sub(rs))
        .min(th.len().saturating_sub(ts));
    if len < 8 {
        return ConsistResult::fail(&r, &t, "有效重叠区不足，请减小最大偏移或加长样本");
    }

    let mut sims: Vec<f32> = Vec::with_capacity(len);
    let mut sum = 0f64;
    let mut minv = 1.0f64;
    for i in 0..len {
        let s = 1.0 - dist(rh[rs + i], th[ts + i]) as f64 / 64.0;
        sum += s;
        if s < minv {
            minv = s;
        }
        sims.push(s as f32);
    }
    let mean_sim = sum / len as f64;
    progress(92);

    // 差异段：连续低于阈值 ≥2 帧
    let mut segs: Vec<BadSeg> = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..len {
        let bad = (sims[i] as f64) < cfg.thr;
        match (bad, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                if i - s >= 2 {
                    let worst = sims[s..i].iter().cloned().fold(1.0f32, f32::min) as f64;
                    segs.push(BadSeg {
                        from_s: s as f64 / fps,
                        to_s: i as f64 / fps,
                        sim: worst,
                    });
                }
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = run_start {
        if len - s >= 2 {
            let worst = sims[s..len].iter().cloned().fold(1.0f32, f32::min) as f64;
            segs.push(BadSeg { from_s: s as f64 / fps, to_s: len as f64 / fps, sim: worst });
        }
    }
    // 最多保留 40 段，超出的合并成一条提示
    if segs.len() > 40 {
        let tail = segs.split_off(39);
        let worst = tail.iter().map(|x| x.sim).fold(1.0f64, f64::min);
        segs.push(BadSeg {
            from_s: tail.first().map(|x| x.from_s).unwrap_or(0.0),
            to_s: tail.last().map(|x| x.to_s).unwrap_or(0.0),
            sim: worst,
        });
    }

    // lag 的定义是「th[lag+i] 对齐 rh[i]」；待测内容更晚时 lag 为负，故取反给用户
    let best_offset_ms = (-(lag as f64) * 1000.0 / fps).round() as i32;
    let verdict = if mean_sim >= 0.92 && segs.is_empty() {
        format!("一致（平均相似度 {:.1}%，偏移 {} ms）", mean_sim * 100.0, best_offset_ms)
    } else if mean_sim >= 0.80 {
        format!(
            "基本一致（平均 {:.1}%，{} 段差异，最差 {:.1}%）",
            mean_sim * 100.0,
            segs.len(),
            minv * 100.0
        )
    } else {
        format!("差异明显（平均 {:.1}%，最差 {:.1}%，{} 段）", mean_sim * 100.0, minv * 100.0, segs.len())
    };
    let note = if cfg.logo.is_some() {
        "按台标区域比对：只比较指定区域内画面".to_string()
    } else {
        "整帧比对：换台/插播/黑场都会表现为差异段".to_string()
    };

    // 相似度曲线抽稀 ≤ 600
    let series_sim = if sims.len() <= 600 {
        sims.clone()
    } else {
        let st = sims.len() / 600;
        (0..600).map(|i| sims[i * st]).collect()
    };

    progress(100);
    ConsistResult {
        ok: true,
        reference: r,
        target: t,
        fps,
        frames_ref: rh.len(),
        frames_tgt: th.len(),
        best_offset_ms,
        mean_sim,
        min_sim: minv,
        bad_count: segs.len(),
        bad_segments: segs,
        series_sim,
        verdict,
        note,
        err: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_similarity() {
        assert_eq!(dist(0, 0), 0);
        assert_eq!(dist(0, 1), 1);
        assert_eq!(dist(u64::MAX, 0), 64);
    }

    #[test]
    fn lag_search_offsets_shifted_series() {
        // 由 compare 内部逻辑抽出的等价断言：同一序列整体后移 3 帧，应当找到 lag=3
        let rh: Vec<u64> = (0..40).map(|i| (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)).collect();
        let th: Vec<u64> = (3..43).map(|i| (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)).collect();
        let mut best = (0i32, f64::MAX);
        for lag in -8..=8i32 {
            let (rs, ts, len) = if lag >= 0 {
                (0usize, lag as usize, th.len().saturating_sub(lag as usize))
            } else {
                ((-lag) as usize, 0usize, rh.len().saturating_sub((-lag) as usize))
            };
            let len = len.min(rh.len().saturating_sub(rs));
            if len < 4 {
                continue;
            }
            let mut acc = 0f64;
            for i in 0..len {
                acc += dist(rh[rs + i], th[ts + i]) as f64;
            }
            let mean = acc / len as f64;
            if mean < best.1 {
                best = (lag, mean);
            }
        }
        // th[i] = rh[i+3]（待测内容晚 3 帧），按 th[lag+i]==rh[i] 的定义应得到 lag=-3
        assert_eq!(best.0, -3);
    }
}
