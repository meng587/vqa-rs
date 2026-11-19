//! 视频分析「轮询一遍」的汇总报告：一份结果里包含所有频道。
//! Markdown 直接拼接单频道报告；HTML / Word 生成汇总表 + 每路关键指标。

import {
  AlignmentType,
  BorderStyle,
  Document,
  Packer,
  Paragraph,
  Table,
  TableCell,
  TableRow,
  TextRun,
  WidthType,
} from "docx";
import type { Res } from "./api";
import { buildMarkdownReport } from "./report-markdown";

export interface AnalysisBatchItem {
  name: string;
  url: string;
  res: Res | null;
  err?: string;
}

const FONT = "Microsoft YaHei";
const B = { style: BorderStyle.SINGLE, size: 4, color: "D9D9D9" };
const BORDERS = { top: B, bottom: B, left: B, right: B, insideHorizontal: B, insideVertical: B };
const CELL_MARGINS = { top: 60, bottom: 60, left: 100, right: 100 };

function esc(s: string): string {
  return String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c] || c,
  );
}

function badCount(r: Res): number {
  return (r.tr290 || []).filter((t) => t.level >= 2).length;
}

function summaryRows(items: AnalysisBatchItem[]): string {
  return items
    .map((it) => {
      if (!it.res) {
        return `<tr><td>${esc(it.name)}</td><td colspan="5" class="err">${esc(it.err || "未取到结果")}</td></tr>`;
      }
      return `<tr>
        <td>${esc(it.name)}</td>
        <td>${((it.res.bitrate_avg || 0) / 1e6).toFixed(2)} Mbps</td>
        <td>${it.res.sync_errors ?? "—"}</td>
        <td>${badCount(it.res)}</td>
        <td>${(it.res.programs || []).length} / ${(it.res.pids || []).length}</td>
        <td class="${badCount(it.res) ? "warn" : "ok"}">${badCount(it.res) ? "有异常" : "正常"}</td>
      </tr>`;
    })
    .join("");
}

export function buildAnalysisBatchMarkdown(items: AnalysisBatchItem[]): string {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const ok = items.filter((x) => x.res).length;
  const head = [
    "# TS 流质量检测轮询汇总",
    "",
    `生成时间：${now} · 视听质量检测工作站`,
    `共 ${items.length} 路，有结果 ${ok} 路，失败 ${items.length - ok} 路。`,
    "",
    "| 频道 | 来源 | 平均码率 | 同步错误 | TR290 L2+ | 节目 / PID | 结论 |",
    "| --- | --- | --- | --- | --- | --- | --- |",
    ...items.map((it) => {
      if (!it.res) return `| ${it.name} | ${it.url} | — | — | — | — | ${it.err || "未取到结果"} |`;
      return `| ${it.name} | ${it.res.path} | ${((it.res.bitrate_avg || 0) / 1e6).toFixed(2)} Mbps | ${it.res.sync_errors ?? "—"} | ${badCount(it.res)} | ${(it.res.programs || []).length} / ${(it.res.pids || []).length} | ${badCount(it.res) ? "有异常" : "正常"} |`;
    }),
    "",
  ].join("\n");
  const sections = items
    .filter((it) => it.res)
    .map((it, i) => `\n\n---\n\n## ${i + 1}. ${it.name}\n\n${buildMarkdownReport(it.res!)}`)
    .join("");
  const failed = items
    .filter((it) => !it.res)
    .map((it) => `- ${it.name}（${it.url}）：${it.err || "未取到结果"}`)
    .join("\n");
  return `${head}${failed ? `\n\n## 未取到结果的频道\n\n${failed}` : ""}${sections}\n`;
}

export function buildAnalysisBatchHtml(items: AnalysisBatchItem[]): string {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const ok = items.filter((x) => x.res).length;
  const sections = items
    .filter((it) => it.res)
    .map((it, i) => {
      const r = it.res!;
      const tr = (r.tr290 || []).filter((t) => t.level >= 2);
      const trRows = tr.length
        ? tr.map((t) => `<tr><td>L${t.level}</td><td>${esc(t.name)}</td><td>${t.count}</td><td>${esc(t.first || "")}</td><td>${esc(t.detail || "")}</td></tr>`).join("")
        : `<tr><td colspan="5" class="muted">未检出 L2/L3 异常</td></tr>`;
      return `<section class="item">
        <h2>${i + 1}. ${esc(it.name)}</h2>
        <div class="src">${esc(r.path)}</div>
        <table><tbody>
          <tr><th>平均码率</th><td>${((r.bitrate_avg || 0) / 1e6).toFixed(2)} Mbps</td><th>文件大小</th><td>${((r.file_size || 0) / 1048576).toFixed(2)} MB</td></tr>
          <tr><th>包数</th><td>${r.packets ?? "—"}</td><th>同步错误</th><td>${r.sync_errors ?? "—"}</td></tr>
          <tr><th>PCR 时长</th><td>${(r.duration_pcr || 0).toFixed(2)} s</td><th>节目 / PID</th><td>${(r.programs || []).length} / ${(r.pids || []).length}</td></tr>
        </tbody></table>
        <h3>TR290 L2/L3 异常</h3>
        <table><thead><tr><th>级别</th><th>项目</th><th>次数</th><th>首次</th><th>说明</th></tr></thead><tbody>${trRows}</tbody></table>
      </section>`;
    })
    .join("");
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>TS 流质量检测轮询汇总</title>
<style>
body{margin:0;background:#eef2f6;color:#1f2937;font:14px/1.65 "Microsoft YaHei",Arial,sans-serif}
.wrap{max-width:1080px;margin:0 auto;padding:28px 20px 48px}
h1{font-size:24px;margin:0 0 6px}h2{font-size:17px;margin:0 0 8px;color:#111827}h3{font-size:14px;margin:18px 0 8px;color:#374151}
.meta{color:#6b7280;font-size:12px;margin-bottom:18px}
.panel{background:#fff;border:1px solid #dbe2ea;border-radius:10px;padding:16px 18px;margin:14px 0;box-shadow:0 1px 2px rgba(15,23,42,.04)}
table{width:100%;border-collapse:collapse;font-size:12.5px}th,td{border:1px solid #dbe2ea;padding:6px 8px;text-align:left;vertical-align:top}
th{background:#f8fafc;font-weight:600}.ok{color:#047857;font-weight:600}.warn{color:#b45309;font-weight:600}.err,.muted{color:#9ca3af}
.src{color:#6b7280;font-size:12px;word-break:break-all;margin-bottom:12px}
.item{background:#fff;border:1px solid #dbe2ea;border-radius:10px;padding:16px 18px;margin:16px 0}
</style></head><body><div class="wrap">
<h1>TS 流质量检测轮询汇总</h1>
<div class="meta">生成时间 ${esc(now)} · 视听质量检测工作站 · 共 ${items.length} 路，有结果 ${ok} 路，失败 ${items.length - ok} 路</div>
<div class="panel"><table><thead><tr><th>频道</th><th>平均码率</th><th>同步错误</th><th>TR290 L2+</th><th>节目 / PID</th><th>结论</th></tr></thead><tbody>${summaryRows(items)}</tbody></table></div>
${sections}
</div></body></html>`;
}

function run(text: string, opts: { bold?: boolean; size?: number; color?: string } = {}) {
  return new TextRun({ text: String(text ?? ""), font: FONT, size: opts.size ?? 20, bold: opts.bold, color: opts.color });
}
function para(text: string, opts: { bold?: boolean; size?: number; color?: string; align?: any; before?: number; after?: number } = {}) {
  return new Paragraph({ children: [run(text, opts)], alignment: opts.align, spacing: { before: opts.before ?? 0, after: opts.after ?? 80 } });
}
function cell(text: string, opts: { bold?: boolean; width?: number; fill?: string; align?: any } = {}) {
  return new TableCell({
    width: opts.width ? { size: opts.width, type: WidthType.PERCENTAGE } : undefined,
    shading: opts.fill ? { fill: opts.fill } : undefined,
    margins: CELL_MARGINS,
    children: [para(text, { bold: opts.bold, align: opts.align, size: 18 })],
  });
}
function headerRow(labels: string[], widths?: number[]) {
  return new TableRow({
    tableHeader: true,
    children: labels.map((t, i) => cell(t, { bold: true, fill: "F1F3F6", width: widths?.[i] })),
  });
}
function table(rows: TableRow[]) {
  return new Table({ rows, width: { size: 100, type: WidthType.PERCENTAGE }, borders: BORDERS });
}

export async function buildAnalysisBatchDocx(items: AnalysisBatchItem[]): Promise<string> {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const ok = items.filter((x) => x.res).length;
  const children: (Paragraph | Table)[] = [
    para("TS 流质量检测轮询汇总", { bold: true, size: 40, align: AlignmentType.CENTER, after: 60 }),
    para(`生成时间 ${now} · 视听质量检测工作站 · 共 ${items.length} 路，有结果 ${ok} 路，失败 ${items.length - ok} 路`, {
      size: 18,
      color: "6B7280",
      align: AlignmentType.CENTER,
      after: 180,
    }),
    para("汇总", { bold: true, size: 24, before: 120, after: 80 }),
    table([
      headerRow(["频道", "平均码率", "同步错误", "TR290 L2+", "节目 / PID", "结论"], [26, 16, 14, 16, 16, 12]),
      ...items.map((it) => {
        if (!it.res) {
          return new TableRow({ children: [cell(it.name, { width: 26 }), cell(it.err || "未取到结果", { width: 74, color: "9CA3AF" } as any)] });
        }
        const r = it.res;
        const bad = badCount(r);
        return new TableRow({
          children: [
            cell(it.name, { width: 26 }),
            cell(`${((r.bitrate_avg || 0) / 1e6).toFixed(2)} Mbps`, { width: 16 }),
            cell(String(r.sync_errors ?? "—"), { width: 14 }),
            cell(String(bad), { width: 16 }),
            cell(`${(r.programs || []).length} / ${(r.pids || []).length}`, { width: 16 }),
            cell(bad ? "有异常" : "正常", { width: 12 }),
          ],
        });
      }),
    ]),
  ];

  items.forEach((it, i) => {
    children.push(para(`${i + 1}. ${it.name}`, { bold: true, size: 26, before: 260, after: 60 }));
    if (!it.res) {
      children.push(para(it.err || "未取到结果", { color: "9CA3AF" }));
      return;
    }
    const r = it.res;
    children.push(para(r.path, { size: 17, color: "6B7280", after: 100 }));
    children.push(
      table([
        new TableRow({ children: [cell("平均码率", { bold: true, width: 24, fill: "F8FAFC" }), cell(`${((r.bitrate_avg || 0) / 1e6).toFixed(2)} Mbps`, { width: 26 }), cell("同步错误", { bold: true, width: 24, fill: "F8FAFC" }), cell(String(r.sync_errors ?? "—"), { width: 26 })] }),
        new TableRow({ children: [cell("文件大小", { bold: true, width: 24, fill: "F8FAFC" }), cell(`${((r.file_size || 0) / 1048576).toFixed(2)} MB`, { width: 26 }), cell("包数", { bold: true, width: 24, fill: "F8FAFC" }), cell(String(r.packets ?? "—"), { width: 26 })] }),
        new TableRow({ children: [cell("PCR 时长", { bold: true, width: 24, fill: "F8FAFC" }), cell(`${(r.duration_pcr || 0).toFixed(2)} s`, { width: 26 }), cell("节目 / PID", { bold: true, width: 24, fill: "F8FAFC" }), cell(`${(r.programs || []).length} / ${(r.pids || []).length}`, { width: 26 })] }),
      ]),
    );
    const tr = (r.tr290 || []).filter((t) => t.level >= 2);
    children.push(para("TR290 L2/L3 异常", { bold: true, size: 20, before: 140, after: 60 }));
    if (tr.length) {
      children.push(
        table([
          headerRow(["级别", "项目", "次数", "首次", "说明"], [10, 24, 10, 22, 34]),
          ...tr.map((t) => new TableRow({ children: [cell(`L${t.level}`, { width: 10 }), cell(t.name, { width: 24 }), cell(String(t.count), { width: 10 }), cell(t.first || "", { width: 22 }), cell(t.detail || "", { width: 34 })] })),
        ]),
      );
    } else {
      children.push(para("未检出 L2/L3 异常。", { color: "6B7280" }));
    }
  });

  children.push(para("本报告由「视听质量检测工作站」按轮询结果自动汇总；各频道数值取自各自的一次扫描。", { size: 17, color: "6B7280", before: 240 }));
  const doc = new Document({
    creator: "视听质量检测工作站",
    title: "TS 流质量检测轮询汇总",
    description: "视频分析轮询汇总报告",
    sections: [{ properties: { page: { margin: { top: 720, right: 720, bottom: 720, left: 720 } } }, children }],
  });
  return Packer.toBase64String(doc);
}
