//! AI 检测报告：只汇总各模块真实返回的数据，不做根因臆测。
//! 报告里的数字都来自 results；AI 编排的说明只放在“目标/编排方式”里。

import type { AiPlan } from "./api";

export type StepState = "pending" | "running" | "ok" | "error" | "skipped" | "cancelled";

export interface StepStatus {
  state: StepState;
  msg?: string;
}

export interface RunView {
  plan: AiPlan;
  results: Record<string, any>;
  statuses: Record<string, StepStatus>;
  startedAt: number;
  finishedAt?: number;
  cancelled?: boolean;
}

const STATUS_LABEL: Record<StepState, string> = {
  pending: "待执行",
  running: "执行中",
  ok: "完成",
  error: "失败",
  skipped: "跳过",
  cancelled: "已取消",
};

export function stepStateLabel(s: StepState): string {
  return STATUS_LABEL[s] ?? s;
}

function verdictText(v: any): string {
  return v === "pass" ? "通过" : v === "fail" ? "不通过" : v === "unknown" ? "无法判定" : String(v ?? "—");
}

function providerText(p: string): string {
  if (!p || p === "rule") return "规则模式";
  if (p.startsWith("local:")) return `本机模型 · ${p.slice(6)}`;
  if (p.startsWith("cloud:")) return `云端模型 · ${p.slice(6)}`;
  return p;
}

const esc = (s: any) => String(s ?? "").replace(/\|/g, "\\|").replace(/\n/g, " ");
const htmlEsc = (s: any) =>
  String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

const num = (v: any, digits = 2, fallback = "—") =>
  typeof v === "number" && Number.isFinite(v) ? v.toFixed(digits) : fallback;

const mbps = (v: any) => (typeof v === "number" && v > 0 ? `${(v / 1e6).toFixed(2)} Mbps` : "—");
const mb = (v: any) => (typeof v === "number" && v > 0 ? `${(v / 1048576).toFixed(2)} MB` : "—");
const pct = (v: any, digits = 3) =>
  typeof v === "number" && Number.isFinite(v) ? `${(v * 100).toFixed(digits)}%` : "—";
const pctRaw = (v: any, digits = 2) =>
  typeof v === "number" && Number.isFinite(v) ? `${v.toFixed(digits)}%` : "—";
const bool = (v: any) => (v ? "是" : "否");
const timeStr = (ms?: number) => {
  if (!ms) return "—";
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
};

function mdTable(headers: string[], rows: any[][]): string {
  const out = [`| ${headers.map(esc).join(" | ")} |`, `| ${headers.map(() => "---").join(" | ")} |`];
  for (const r of rows) out.push(`| ${r.map(esc).join(" | ")} |`);
  return out.join("\n");
}

function statusOf(run: RunView, id: string): StepState {
  return run.statuses[id]?.state ?? (run.results[id]?.error ? "error" : "pending");
}

/** 计划步骤 id 由规则/模型生成，不一定等于模块名；报告按 tool 找结果。 */
function findStep(run: RunView, tool: string) {
  return run.plan.steps.find((s) => s.tool === tool);
}

function pickResult(run: RunView, tool: string): any {
  const s = findStep(run, tool);
  return s ? run.results[s.id] : undefined;
}

function resultError(r: any): string {
  return typeof r?.error === "string" ? r.error : "";
}

function summaryRows(run: RunView): string[][] {
  return run.plan.steps.map((s) => [
    s.title,
    s.tool,
    stepStateLabel(statusOf(run, s.id)),
    run.statuses[s.id]?.msg || resultError(run.results[s.id]) || "—",
  ]);
}

export interface Verdict {
  level: "good" | "warn" | "bad" | "unknown";
  title: string;
  findings: string[];
}

export function overallVerdict(run: RunView): Verdict {
  const findings: string[] = [];
  const attention: string[] = [];
  let bad = 0;
  let warn = 0;
  let known = 0;
  const cap = pickResult(run, "capture.run");
  const ana = pickResult(run, "analysis.run");
  const comp = pickResult(run, "compliance.run");
  const burst = pickResult(run, "burst.run");
  const av = pickResult(run, "avsync.run");

  // 结论不只看“出的数据”，也要明确哪些检测没跑完，避免把失败当成没问题。
  for (const s of run.plan.steps) {
    if (s.tool === "report.build") continue;
    const st = statusOf(run, s.id);
    if (st === "error" || st === "skipped" || st === "cancelled") {
      const why = run.statuses[s.id]?.msg || resultError(run.results[s.id]) || "";
      attention.push(`${s.title}：${stepStateLabel(st)}${why ? `（${why}）` : ""}`);
    }
  }
  if (run.cancelled) attention.push("执行被手动停止，结果不完整");

  if (cap && !resultError(cap)) {
    known++;
    const loss = typeof cap.net?.loss_ratio === "number" ? cap.net.loss_ratio : null;
    const jitter = typeof cap.net?.jitter_ms === "number" ? cap.net.jitter_ms : null;
    if (loss != null && loss > 0.001) {
      warn++;
      findings.push(`抓包：丢包率 ${pct(loss, 3)}，建议复核链路与交换机缓冲`);
    }
    if (jitter != null && jitter > 5) {
      warn++;
      findings.push(`抓包：抖动 ${num(jitter, 2)} ms，网络时延稳定性需关注`);
    }
  }
  if (ana && !resultError(ana)) {
    known++;
    const sync = Number(ana.sync_errors || 0);
    const tr2 = (ana.tr290 || []).filter((x: any) => Number(x.level) >= 2 && Number(x.count) > 0);
    if (sync > 0) {
      warn++;
      findings.push(`TS：同步错误 ${sync} 次`);
    }
    if (tr2.length) {
      bad++;
      findings.push(`TS：TR290 优先级 2+ 异常 ${tr2.length} 项`);
    }
    if (ana.encrypted || ana.has_scrambled) {
      warn++;
      findings.push("TS：码流存在加扰，部分分析项可能受限");
    }
  }
  if (comp && !resultError(comp)) {
    known++;
    const fail = Number(comp.summary?.fail || 0);
    const unknown = Number(comp.summary?.unknown || 0);
    if (fail > 0) {
      bad++;
      findings.push(`合规：${fail} 项不通过`);
    }
    if (unknown > 0) {
      warn++;
      findings.push(`合规：${unknown} 项无法判定，需人工复核`);
    }
  }
  if (burst && !resultError(burst)) {
    known++;
    const cnt = Number(burst.burst_cnt || 0);
    const ratio = Number(burst.peak_ratio || 0);
    if (cnt > 0 || ratio >= 3) {
      warn++;
      findings.push(`微突发：峰值/均值 ${num(ratio, 2)} 倍，出现 ${cnt} 次`);
    }
  }
  if (av && !resultError(av)) {
    known++;
    const off = Number(av.offset_ms || 0);
    if (Math.abs(off) > 300) {
      warn++;
      findings.push(`音画：偏移 ${off > 0 ? "+" : ""}${num(off, 0)} ms，建议人工听看复核`);
    }
  }
  if (!known) {
    const failed = attention.length;
    return {
      level: "unknown",
      title: "无法判断",
      findings: failed ? [...attention.slice(0, 4), `共 ${failed} 个检测步骤未完成或失败`] : ["没有可用的检测结果"],
    };
  }
  if (bad) return { level: "bad", title: "异常", findings: [...findings, ...attention] };
  if (attention.length) {
    return {
      level: "warn",
      title: run.cancelled ? "已停止" : "检测不完整",
      findings: [...findings, ...attention.slice(0, 4)],
    };
  }
  if (warn) return { level: "warn", title: "有关注项", findings };
  return {
    level: "good",
    title: "未见明显异常",
    findings: ["本次已执行模块未发现超阈值项；结论以检测数据为准，建议结合人工复核。"],
  };
}

function mdCapture(r: any): string {
  if (!r || resultError(r)) return `- 失败：${esc(resultError(r) || "没有返回结果")}`;
  return mdTable(
    ["项目", "值"],
    [
      ["收流地址", r.url || "—"],
      ["抓包文件", r.pcap_path ? `${r.pcap_path}（${mb(r.pcap_bytes)}）` : "—"],
      ["TS 样本", r.ts_path ? `${r.ts_path}（${mb(r.ts_bytes)}）` : "—"],
      ["平均码率", `${num(r.net?.bitrate_mbps, 2)} Mbps`],
      ["抖动", `${num(r.net?.jitter_ms, 2)} ms`],
      ["丢包率", pct(r.net?.loss_ratio, 3)],
      ["MDI 差包占比", pctRaw(r.net?.df_bad_pct, 2)],
      ["MLR 好包占比", pctRaw(r.net?.mlr_good_pct, 2)],
      ["RTP", bool(r.net?.rtp)],
      ["包数", r.net?.packets ?? "—"],
    ],
  );
}

function mdAnalysis(r: any): string {
  if (!r || resultError(r)) return `- 失败：${esc(resultError(r) || "没有返回结果")}`;
  const tr = (r.tr290 || []).filter((x: any) => Number(x.count) > 0);
  const trBad = tr.filter((x: any) => Number(x.level) >= 2);
  const progs = r.programs || [];
  const lines = [
    mdTable(
      ["项目", "值"],
      [
        ["来源", r.path || "—"],
        ["包长", `${r.packet_size ?? "—"} 字节`],
        ["包数", r.packets ?? "—"],
        ["同步错误", r.sync_errors ?? "—"],
        ["PCR 时长", `${num(r.duration_pcr, 2)} s`],
        ["平均码率", mbps(r.bitrate_avg)],
        ["TR290 优先级 2+ 异常", trBad.length],
        ["节目数 / PID 数", `${progs.length} / ${(r.pids || []).length}`],
        ["加扰", r.encrypted || r.has_scrambled ? "有" : "无"],
        ["PCR PID", r.pcr_pid ?? "无"],
        ["GOP", r.gop_note || "—"],
      ],
    ),
  ];
  if (tr.length) {
    lines.push("", "**TR290 异常项**", "", mdTable(["级别", "项目", "次数", "说明"], tr.slice(0, 12).map((x: any) => [x.level, x.name, x.count, x.detail || x.first || ""])));
  }
  if (progs.length) {
    lines.push("", "**节目**", "", mdTable(["节目号", "PMT PID", "PCR PID"], progs.slice(0, 12).map((p: any) => [p.number, p.pmt_pid, p.pcr_pid])));
  }
  return lines.join("\n");
}

function mdCompliance(r: any): string {
  if (!r || resultError(r)) return `- 失败：${esc(resultError(r) || "没有返回结果")}`;
  const s = r.summary || {};
  const items = (r.items || []).slice(0, 30);
  const lines = [
    mdTable(
      ["通过", "不通过", "无法判定", "总计"],
      [[s.pass ?? "—", s.fail ?? "—", s.unknown ?? "—", s.total ?? "—"]],
    ),
  ];
  if (items.length) {
    lines.push("", "**判定明细（最多显示 30 项）**", "", mdTable(["项目", "标准", "实测", "结论"], items.map((x: any) => [x.name, x.standard, x.actual, verdictText(x.verdict)])));;
  }
  return lines.join("\n");
}

function mdBurst(r: any): string {
  if (!r || resultError(r)) return `- 失败：${esc(resultError(r) || "没有返回结果")}`;
  return mdTable(
    ["项目", "值"],
    [
      ["检测时长", `${num(r.secs, 1)} s`],
      ["平均码率", `${num(r.mbps_avg, 2)} Mbps`],
      ["峰值码率", `${num(r.mbps_peak, 2)} Mbps`],
      ["峰值/均值", `${num(r.peak_ratio, 2)} 倍`],
      ["突发次数", r.burst_cnt ?? "—"],
      ["时间窗", `${r.window_ms ?? "—"} ms`],
      ["包数 / 字节", `${r.packets ?? "—"} / ${mb(r.bytes)}`],
    ],
  );
}

function mdAvsync(r: any): string {
  if (!r || resultError(r)) return `- 失败：${esc(resultError(r) || "没有返回结果")}`;
  return mdTable(
    ["项目", "值"],
    [
      ["来源", r.source || "—"],
      ["偏移", `${num(r.offset_ms, 0)} ms`],
      ["置信度", num(r.confidence, 3)],
      ["视频帧 / 音频窗", `${r.video_frames ?? "—"} / ${r.audio_windows ?? "—"}`],
      ["结论", r.verdict || "—"],
    ],
  );
}

function mdPlayer(r: any): string {
  if (!r || resultError(r)) return `- 失败：${esc(resultError(r) || "没有返回结果")}`;
  const snap = r.snap || r;
  return mdTable(
    ["项目", "值"],
    [
      ["画面", `${snap.width ?? "—"}×${snap.height ?? "—"}`],
      ["视频编码", snap.video_codec || "—"],
      ["视频码率", `${num(snap.v_bitrate, 0)} kbps`],
      ["音画差", `${num(snap.avsync, 3)} s`],
      ["渲染丢帧", snap.render_drop ?? "—"],
      ["解码丢帧", snap.decoder_drop ?? "—"],
      ["截图", r.snapshot_path || "—"],
    ],
  );
}

export function buildAiMarkdown(run: RunView): string {
  const v = overallVerdict(run);
  const lines: string[] = [
    "# 视听质量 AI 检测报告",
    "",
    `- 生成时间：${timeStr(run.finishedAt || Date.now())}`,
    `- 检测目标：${esc(run.plan.goal)}`,
    `- 来源：${esc(run.plan.source)}`,
    `- 编排方式：${esc(providerText(run.plan.provider))}`,
    `- 结论：**${v.title}**`,
  ];
  if (v.findings.length) {
    lines.push("", "**主要发现**", "");
    v.findings.forEach((f) => lines.push(`- ${esc(f)}`));
  }
  lines.push("", "## 执行摘要", "", mdTable(["步骤", "工具", "状态", "说明"], summaryRows(run)));

  const sections: Array<[string, string, (r: any) => string]> = [
    ["抓包分析", "capture.run", mdCapture],
    ["TS 码流分析", "analysis.run", mdAnalysis],
    ["合规检测", "compliance.run", mdCompliance],
    ["微突发检测", "burst.run", mdBurst],
    ["音画同步", "avsync.run", mdAvsync],
    ["播放器抽检", "player.play", mdPlayer],
  ];
  for (const [title, tool, fn] of sections) {
    const step = findStep(run, tool);
    if (!step) continue;
    lines.push("", `## ${title}`, "");
    const r = run.results[step.id];
    const st = statusOf(run, step.id);
    if (st === "skipped" || st === "cancelled") {
      lines.push(`- 未执行（${stepStateLabel(st)}）`);
      continue;
    }
    lines.push(fn(r));
  }

  const evidence: string[] = [];
  const cap = pickResult(run, "capture.run");
  const player = pickResult(run, "player.play");
  if (cap?.pcap_path) evidence.push(`- pcap：${esc(cap.pcap_path)}`);
  if (cap?.ts_path) evidence.push(`- TS 样本：${esc(cap.ts_path)}`);
  if (player?.snapshot_path) evidence.push(`- 播放器截图：${esc(player.snapshot_path)}`);
  if (evidence.length) lines.push("", "## 证据文件", "", ...evidence);

  lines.push(
    "",
    "## 说明",
    "",
    "本报告由 AI 编排，各章节数字来自对应检测模块的 API 返回；AI 不直接读取或修改页面。",
    "报告只做数据汇总与阈值提示，不替代人工判断；涉及链路、片源、转码参数的问题建议结合现场复核。",
  );
  return lines.join("\n");
}

// ── HTML ──

function htmlTable(headers: string[], rows: any[][]): string {
  const th = headers.map((h) => `<th>${htmlEsc(h)}</th>`).join("");
  const tr = rows
    .map((r) => `<tr>${r.map((c) => `<td>${htmlEsc(c)}</td>`).join("")}</tr>`)
    .join("");
  return `<table><thead><tr>${th}</tr></thead><tbody>${tr}</tbody></table>`;
}

function htmlSection(title: string, body: string): string {
  return `<section><h2>${htmlEsc(title)}</h2>${body}</section>`;
}

function htmlError(r: any): string {
  return `<p class="err">失败：${htmlEsc(resultError(r) || "没有返回结果")}</p>`;
}

function htmlCapture(r: any): string {
  if (!r || resultError(r)) return htmlError(r);
  return htmlTable(
    ["项目", "值"],
    [
      ["收流地址", r.url || "—"],
      ["抓包文件", r.pcap_path ? `${r.pcap_path}（${mb(r.pcap_bytes)}）` : "—"],
      ["TS 样本", r.ts_path ? `${r.ts_path}（${mb(r.ts_bytes)}）` : "—"],
      ["平均码率", `${num(r.net?.bitrate_mbps, 2)} Mbps`],
      ["抖动", `${num(r.net?.jitter_ms, 2)} ms`],
      ["丢包率", pct(r.net?.loss_ratio, 3)],
      ["MDI 差包占比", pctRaw(r.net?.df_bad_pct, 2)],
      ["MLR 好包占比", pctRaw(r.net?.mlr_good_pct, 2)],
      ["RTP", bool(r.net?.rtp)],
      ["包数", r.net?.packets ?? "—"],
    ],
  );
}

function htmlAnalysis(r: any): string {
  if (!r || resultError(r)) return htmlError(r);
  const tr = (r.tr290 || []).filter((x: any) => Number(x.count) > 0);
  const bad = tr.filter((x: any) => Number(x.level) >= 2);
  let body = htmlTable(
    ["项目", "值"],
    [
      ["来源", r.path || "—"],
      ["包长", `${r.packet_size ?? "—"} 字节`],
      ["包数", r.packets ?? "—"],
      ["同步错误", r.sync_errors ?? "—"],
      ["PCR 时长", `${num(r.duration_pcr, 2)} s`],
      ["平均码率", mbps(r.bitrate_avg)],
      ["TR290 优先级 2+ 异常", bad.length],
      ["节目数 / PID 数", `${(r.programs || []).length} / ${(r.pids || []).length}`],
      ["加扰", r.encrypted || r.has_scrambled ? "有" : "无"],
    ],
  );
  if (tr.length) {
    body += `<h3>TR290 异常项</h3>` + htmlTable(["级别", "项目", "次数", "说明"], tr.slice(0, 12).map((x: any) => [x.level, x.name, x.count, x.detail || x.first || ""]));
  }
  return body;
}

function htmlCompliance(r: any): string {
  if (!r || resultError(r)) return htmlError(r);
  const s = r.summary || {};
  let body = htmlTable(["通过", "不通过", "无法判定", "总计"], [[s.pass ?? "—", s.fail ?? "—", s.unknown ?? "—", s.total ?? "—"]]);
  const items = (r.items || []).slice(0, 30);
  if (items.length) {
    body += `<h3>判定明细（最多 30 项）</h3>` + htmlTable(["项目", "标准", "实测", "结论"], items.map((x: any) => [x.name, x.standard, x.actual, verdictText(x.verdict)]));
  }
  return body;
}

function htmlBurst(r: any): string {
  if (!r || resultError(r)) return htmlError(r);
  return htmlTable(
    ["项目", "值"],
    [
      ["检测时长", `${num(r.secs, 1)} s`],
      ["平均码率", `${num(r.mbps_avg, 2)} Mbps`],
      ["峰值码率", `${num(r.mbps_peak, 2)} Mbps`],
      ["峰值/均值", `${num(r.peak_ratio, 2)} 倍`],
      ["突发次数", r.burst_cnt ?? "—"],
      ["时间窗", `${r.window_ms ?? "—"} ms`],
    ],
  );
}

function htmlAvsync(r: any): string {
  if (!r || resultError(r)) return htmlError(r);
  return htmlTable(
    ["项目", "值"],
    [
      ["来源", r.source || "—"],
      ["偏移", `${num(r.offset_ms, 0)} ms`],
      ["置信度", num(r.confidence, 3)],
      ["视频帧 / 音频窗", `${r.video_frames ?? "—"} / ${r.audio_windows ?? "—"}`],
      ["结论", r.verdict || "—"],
    ],
  );
}

function htmlPlayer(r: any): string {
  if (!r || resultError(r)) return htmlError(r);
  const snap = r.snap || r;
  return htmlTable(
    ["项目", "值"],
    [
      ["画面", `${snap.width ?? "—"}×${snap.height ?? "—"}`],
      ["视频编码", snap.video_codec || "—"],
      ["视频码率", `${num(snap.v_bitrate, 0)} kbps`],
      ["音画差", `${num(snap.avsync, 3)} s`],
      ["渲染丢帧", snap.render_drop ?? "—"],
      ["解码丢帧", snap.decoder_drop ?? "—"],
      ["截图", r.snapshot_path || "—"],
    ],
  );
}

export function buildAiHtml(run: RunView): string {
  const v = overallVerdict(run);
  const sections: Array<[string, string, (r: any) => string]> = [
    ["抓包分析", "capture.run", htmlCapture],
    ["TS 码流分析", "analysis.run", htmlAnalysis],
    ["合规检测", "compliance.run", htmlCompliance],
    ["微突发检测", "burst.run", htmlBurst],
    ["音画同步", "avsync.run", htmlAvsync],
    ["播放器抽检", "player.play", htmlPlayer],
  ];
  const sectionHtml = sections
    .map(([title, tool, fn]) => {
      const step = findStep(run, tool);
      if (!step) return "";
      const st = statusOf(run, step.id);
      if (st === "skipped" || st === "cancelled") {
        return htmlSection(title, `<p class="muted">未执行（${stepStateLabel(st)}）</p>`);
      }
      return htmlSection(title, fn(run.results[step.id]));
    })
    .join("");
  const summary = htmlTable(["步骤", "工具", "状态", "说明"], summaryRows(run));
  const findings = v.findings.map((f) => `<li>${htmlEsc(f)}</li>`).join("");
  const levelClass = v.level === "bad" ? "bad" : v.level === "warn" ? "warn" : v.level === "good" ? "good" : "unknown";
  const evidence: string[] = [];
  const cap = pickResult(run, "capture.run");
  const player = pickResult(run, "player.play");
  if (cap?.pcap_path) evidence.push(`<li>pcap：${htmlEsc(cap.pcap_path)}</li>`);
  if (cap?.ts_path) evidence.push(`<li>TS 样本：${htmlEsc(cap.ts_path)}</li>`);
  if (player?.snapshot_path) evidence.push(`<li>播放器截图：${htmlEsc(player.snapshot_path)}</li>`);
  return `<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>视听质量 AI 检测报告</title>
<style>
body{margin:0;background:#efe8db;color:#211c14;font:14px/1.6 "Microsoft YaHei",sans-serif}
.page{max-width:980px;margin:24px auto;background:#fffdf7;border:1px solid #dcd1be;padding:36px 42px;box-shadow:0 10px 30px rgba(33,28,20,.08)}
h1{font-size:30px;margin:0 0 8px;color:#211c14;border-bottom:2px solid #355f4d;padding-bottom:10px}
h2{font-size:19px;margin:28px 0 10px;color:#355f4d}
h3{font-size:15px;margin:18px 0 8px;color:#4e4536}
.meta{color:#8a7c66;font-size:12px;line-height:1.8}
.verdict{display:inline-block;padding:10px 16px;border-radius:6px;font-weight:700;margin:14px 0 6px}
.verdict.good{background:#e4efe8;color:#355f4d}.verdict.warn{background:#f6ecd2;color:#9a6e15}.verdict.bad{background:#f4dfda;color:#a83a2a}.verdict.unknown{background:#eee9df;color:#4e4536}
ul{margin:6px 0 10px 20px}li{margin:4px 0}
table{border-collapse:collapse;width:100%;margin:8px 0 14px;font-size:12.5px}
th,td{border:1px solid #dcd1be;padding:7px 9px;text-align:left;vertical-align:top}
th{background:#f5efe3;color:#4e4536;font-weight:600}
.err{color:#a83a2a}.muted{color:#8a7c66}
.footer{margin-top:30px;padding-top:12px;border-top:1px solid #dcd1be;color:#8a7c66;font-size:11.5px}
</style></head><body><div class="page">
<h1>视听质量 AI 检测报告</h1>
<div class="meta">生成时间：${htmlEsc(timeStr(run.finishedAt || Date.now()))}<br>检测目标：${htmlEsc(run.plan.goal)}<br>来源：${htmlEsc(run.plan.source)}<br>编排方式：${htmlEsc(providerText(run.plan.provider))}</div>
<div class="verdict ${levelClass}">${htmlEsc(v.title)}</div>
${findings ? `<ul>${findings}</ul>` : ""}
<h2>执行摘要</h2>${summary}
${sectionHtml}
${evidence.length ? `<h2>证据文件</h2><ul>${evidence.join("")}</ul>` : ""}
<div class="footer">本报告由 AI 编排，各章节数字来自对应检测模块的 API 返回；报告只做数据汇总与阈值提示，不替代人工判断。</div>
</div></body></html>`;
}

export function reportFileName(prefix = "AI检测报告"): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${prefix}_${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}_${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
}
