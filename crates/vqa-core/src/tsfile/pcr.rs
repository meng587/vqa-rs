//! PCR 抖动 —— 二阶数字锁相环（双环）重建参考时钟。
//!
//! 参考 DVB TR101290 / EasyICE：
//! * 快环重建相位 Tf（较高带宽）→ PCR_OJ 总抖动（瞬时，含传输突发）；
//! * 慢环重建相位 Ts（低带宽）  → PCR_AC 准确度（慢变频率漂移）；
//! * PCR Interval = 相邻 PCR 时间戳差。
//!
//! 参考轴 x：实时流=网卡独立到达时间，文件=字节偏移。PLL 低带宽抑制用户态
//! 收包的随机调度噪声，同时保留真实的传输突发/频率漂移。
//! 文件按参考轴均匀性判 CBR（VBR 文件 OJ/AC 不可靠，由 UI 留空）。
//! 返回 (samples, is_cbr)。

/// 一个 PCR 采样点的抖动产出。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct PcrSample {
    pub off: f64,
    pub t: f64,
    pub interval_ms: f64,
    pub oj_ms: f64,
    pub ac_ms: f64,
}

/// 由 PCR 原始序列 `[(参考x, PCR秒)]` 计算每个 PCR 的间隔与抖动。
/// `window` 参数保留（Python 版同名声明的兼容位，当前算法未使用）。
pub fn compute_pcr_jitter(
    raw: &[(f64, f64)],
    _window: usize,
    arrival_based: bool,
) -> (Vec<PcrSample>, bool) {
    let n = raw.len();
    if n < 2 {
        return (vec![], false);
    }
    let offs: Vec<f64> = raw.iter().map(|r| r.0).collect();
    let pcrs: Vec<f64> = raw.iter().map(|r| r.1).collect();
    let span = pcrs[n - 1] - pcrs[0];

    // 参考轴均匀性：实时(独立到达时间)恒可测；文件按相邻参考差变异系数判 CBR
    let is_cbr = if arrival_based {
        true
    } else {
        let dx: Vec<f64> = (0..n - 1).map(|i| offs[i + 1] - offs[i]).collect();
        let mref = dx.iter().sum::<f64>() / dx.len() as f64;
        if mref > 0.0 {
            let vr = dx.iter().map(|d| (d - mref) * (d - mref)).sum::<f64>()
                / dx.len() as f64;
            vr.sqrt() / mref < 0.10
        } else {
            false
        }
    };

    // 标称斜率 b0（秒/参考单位）与标称 PCR 间隔（秒），用于频率环归一。
    // 实时流标称 1 秒/秒（用整体估计会吸收频率漂移，导致 AC 测不出）；
    // 文件无独立标称：用起始段（前 min(16,n-1) 个间隔）拟合真实斜率，
    // 避免整体斜率把全程频率漂移一并吸收、慢环 AC 恒为 0。
    let (b0, nom_iv) = if arrival_based {
        let nom = if n > 1 { (offs[n - 1] - offs[0]) / (n - 1) as f64 } else { 1.0 };
        (1.0, nom)
    } else {
        let w = (n - 1).min(16);
        let mut sxx = 0.0;
        let mut sxy = 0.0;
        for k in 0..w {
            let dx = offs[k + 1] - offs[k];
            let dy = pcrs[k + 1] - pcrs[k];
            sxx += dx * dx;
            sxy += dx * dy;
        }
        let b0 = if sxx != 0.0 {
            sxy / sxx
        } else if offs[n - 1] != offs[0] {
            span / (offs[n - 1] - offs[0])
        } else {
            1.0
        };
        let nom = if n > 1 { span / (n - 1) as f64 } else { 1.0 };
        (b0, nom)
    };

    // 快环（OJ）：二阶 PLL，相位 Tf、频率比 rf，闭环跟踪平均频率/相位；
    // 慢环（AC）：极低带宽，仅估计慢频率比 rs，随机相位噪声经积分平均。
    const KP_F: f64 = 0.10;
    const KI_F: f64 = 0.004;
    const KP_S: f64 = 0.02;
    const KI_S: f64 = 0.001;

    let mut tf = pcrs[0];
    let mut ts = pcrs[0];
    let mut rf = 1.0;
    let mut rs = 1.0;
    let mut a_prev = offs[0];
    let mut ac_phase = 0.0; // 慢频率偏差积分出的慢变相位（AC，干净）
    let mut out = Vec::with_capacity(n);
    let mut prev: Option<f64> = None;

    for i in 0..n {
        let a = offs[i];
        let pcr = pcrs[i];
        let dt = a - a_prev;
        // 自由运行（按各自估计频率推进重建相位）
        tf += b0 * rf * dt;
        ts += b0 * rs * dt;
        let ef = pcr - tf;
        let es = pcr - ts;
        // AC 相位：慢频率偏差 (rs-1) 的积分（不含当点随机噪声）
        ac_phase += (rs - 1.0) * b0 * dt;
        let interval = match prev {
            None => 0.0,
            Some(p) => (pcr - p) * 1000.0,
        };
        out.push(PcrSample {
            off: a,
            t: pcr,
            interval_ms: interval,
            oj_ms: ef * 1000.0,
            ac_ms: ac_phase * 1000.0,
        });
        // 闭环更新（频率修正按误差占一个标称周期的比例，文件/实时通用）
        tf += KP_F * ef;
        if nom_iv != 0.0 {
            rf += KI_F * (ef / nom_iv);
        }
        ts += KP_S * es;
        if nom_iv != 0.0 {
            rs += KI_S * (es / nom_iv);
        }
        prev = Some(pcr);
        a_prev = a;
    }
    (out, is_cbr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 27MHz 时钟完美 CBR：每 40ms 一个 PCR，字节间隔恒定 → 抖动应为 0 量级。
    #[test]
    fn perfect_cbr_zero_jitter() {
        let mut raw = vec![];
        for i in 0..100 {
            let pcr = i as f64 * 0.04;
            let off = i as f64 * 100_000.0;
            raw.push((off, pcr));
        }
        let (samples, cbr) = compute_pcr_jitter(&raw, 8, false);
        assert!(cbr);
        assert_eq!(samples.len(), 100);
        let max_oj = samples.iter().map(|s| s.oj_ms.abs()).fold(0.0, f64::max);
        assert!(max_oj < 1.0, "oj too big: {max_oj}");
        let max_ac = samples.iter().map(|s| s.ac_ms.abs()).fold(0.0, f64::max);
        assert!(max_ac < 1.0, "ac too big: {max_ac}");
        // 间隔恒 40ms
        assert!((samples[50].interval_ms - 40.0).abs() < 1e-9);
    }

    #[test]
    fn too_short_is_empty() {
        assert_eq!(compute_pcr_jitter(&[], 8, false).0.len(), 0);
        assert_eq!(compute_pcr_jitter(&[(1.0, 2.0)], 8, false).0.len(), 0);
    }

    /// 注入一次 5ms 的突发 → OJ 应出现同量级尖峰。
    #[test]
    fn burst_shows_in_oj() {
        let mut raw = vec![];
        for i in 0..100 {
            let pcr = i as f64 * 0.04;
            let off = i as f64 * 100_000.0;
            raw.push((off, pcr));
        }
        // 在第 50 点注入：到达时间推后 5ms 等效（字节轴不均匀）
        raw[50].0 += 25_000.0; // 0.25 个包间隔的字节量，近似 5ms@恒定码率
        let (samples, cbr) = compute_pcr_jitter(&raw, 8, false);
        let _ = cbr;
        let peak = samples.iter().map(|s| s.oj_ms.abs()).fold(0.0, f64::max);
        assert!(peak > 1.0, "expected visible OJ peak, got {peak}");
    }
}
