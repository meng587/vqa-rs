//! 视频分析报告 —— Markdown 版本。
//! 与 HTML / Word 报告共用同一份 Res，字段口径保持一致。

import type { Res } from "./api";

const cell = (v: unknown) => String(v ?? "").replace(/\|/g, "\\|").replace(/\r?\n/g, " ");
const mb = (n: number) => `${(n / 1048576).toFixed(2)} MB`;
const mbps = (n: number) => `${(n / 1e6).toFixed(2)} Mbps`;

export function buildMarkdownReport(r: Res): string {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const tr = r.tr290 || [];
  const bad = tr.filter((t) => t.level >= 2);
  const pids = (r.pids || []).slice(0, 400);
  const progs = r.programs || [];
  const rateMax = (r.rate || []).reduce((m, x) => Math.max(m, x[1] || 0), 0);

  const lines: string[] = [];
  lines.push("# TS 流质量检测报告", "");
  lines.push(`生成时间：${now} · 视听质量检测工作站`, "");

  lines.push("## 摘要", "", "| 项目 | 值 |", "|---|---|");
  lines.push(`| 码流平均码率 | ${mbps(r.bitrate_avg)} |`);
  lines.push(`| 文件大小 | ${mb(r.file_size)} |`);
  lines.push(`| 包数 | ${r.packets} |`);
  lines.push(`| 同步错误 | ${r.sync_errors} |`);
  lines.push(`| PCR 时长 | ${(r.duration_pcr || 0).toFixed(2)} s |`);
  lines.push(`| TR290 异常项 | ${bad.length} |`);
  lines.push(`| 节目数 / PID 数 | ${progs.length} / ${(r.pids || []).length} |`);
  lines.push(`| 加密 | ${r.encrypted || r.has_scrambled ? "有（加扰）" : "无"} |`, "");

  lines.push("## 基本信息", "", "| 项目 | 值 |", "|---|---|");
  lines.push(`| 来源 | ${cell(r.path)} |`);
  lines.push(`| 包长 | ${r.packet_size} 字节 |`);
  lines.push(`| PCR PID | ${r.pcr_pid == null ? "无" : String(r.pcr_pid)} |`);
  lines.push(`| PCR 恒定码率 | ${r.pcr_cbr ? "是" : "否"} |`);
  lines.push(`| SI 截断 | ${r.si_truncated ? "是（表未取全）" : "否"} |`);
  lines.push(`| GOP | ${cell(r.gop_note || "—")} |`);
  lines.push(`| 峰值码率（采样） | ${rateMax ? mbps(rateMax) : "—"} |`, "");

  lines.push("## TR 290 优先级 1/2/3", "");
  if (tr.length) {
    lines.push("| 级别 | 项目 | 次数 | 首次 | 说明 |", "|---|---|---:|---|---|");
    for (const t of tr) {
      lines.push(`| L${t.level} | ${cell(t.name)} | ${t.count} | ${cell(t.first || "")} | ${cell(t.detail || "")} |`);
    }
  } else {
    lines.push("未采到 TR290 指标。");
  }
  lines.push("");

  lines.push("## 节目结构", "");
  if (progs.length) {
    for (const p of progs) {
      lines.push(`### 节目 ${p.number}`, "");
      lines.push(`PMT PID：${p.pmt_pid} · PCR PID：${p.pcr_pid}`, "");
      lines.push("| PID | 类型 | 种类 |", "|---|---|---|");
      for (const s of p.streams || []) {
        lines.push(`| ${s.pid} | ${cell(s.type_name)} | ${cell(s.kind)} |`);
      }
      lines.push("");
    }
  } else {
    lines.push("未读到节目表（PAT/PMT 缺失或非 TS 封装）。", "");
  }

  lines.push(`## PID 占用（${pids.length} / ${(r.pids || []).length}，按字节降序）`, "");
  if (pids.length) {
    lines.push("| PID | 种类 | 包数 | 字节 | 净荷 | 加扰 |", "|---|---|---:|---:|---:|---:|");
    for (const p of pids) {
      lines.push(`| ${p.pid} | ${cell(p.kind)} | ${p.packets} | ${mb(p.bytes)} | ${mb(p.payload_bytes)} | ${p.scrambled || 0} |`);
    }
  } else {
    lines.push("无 PID 数据。");
  }
  lines.push("");
  lines.push("本报告由「视听质量检测工作站」自动生成，数值取自对源的一次完整扫描；如需复核请保留原始码流与同版本程序。");
  return lines.join("\n");
}
