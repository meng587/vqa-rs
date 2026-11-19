//! 合规检测「轮询一遍」的汇总报告：一份结果里包含所有频道。

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
import type { ComplianceResult } from "./api";
import { reportRows } from "./compliance-meta";
import { buildComplianceHtml, buildComplianceMarkdown } from "./compliance-report";

export interface ComplianceBatchItem {
  name: string;
  url: string;
  result: ComplianceResult | null;
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

function conclusion(r: ComplianceResult): string {
  if (r.summary.fail > 0) return "存在不合格项";
  if (!r.summary.ok) return "有未标注项";
  if (r.summary.unknown > 0) return "全部合格（含未标注）";
  return "全部合格";
}

function summaryRows(items: ComplianceBatchItem[]): string {
  return items
    .map((it) => {
      if (!it.result) {
        return `<tr><td>${esc(it.name)}</td><td colspan="4" class="err">${esc(it.err || "未取到结果")}</td></tr>`;
      }
      const r = it.result;
      return `<tr>
        <td>${esc(it.name)}</td>
        <td>${esc(r.kind)}</td>
        <td>${r.summary.pass}</td>
        <td>${r.summary.fail}</td>
        <td>${r.summary.unknown}</td>
        <td class="${r.summary.fail ? "warn" : "ok"}">${conclusion(r)}</td>
      </tr>`;
    })
    .join("");
}

export function buildComplianceBatchMarkdown(items: ComplianceBatchItem[]): string {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const ok = items.filter((x) => x.result).length;
  const head = [
    "# 合规检测轮询汇总",
    "",
    `生成时间：${now} · 视听质量检测工作站`,
    `共 ${items.length} 路，有结果 ${ok} 路，失败 ${items.length - ok} 路。`,
    "",
    "| 频道 | 依据 | 合格 | 不合格 | 未标注 | 结论 |",
    "| --- | --- | --- | --- | --- | --- |",
    ...items.map((it) => {
      if (!it.result) return `| ${it.name} | — | — | — | — | ${it.err || "未取到结果"} |`;
      const r = it.result;
      return `| ${it.name} | ${r.kind} | ${r.summary.pass} | ${r.summary.fail} | ${r.summary.unknown} | ${conclusion(r)} |`;
    }),
    "",
  ].join("\n");
  const sections = items
    .filter((it) => it.result)
    .map((it, i) => `\n\n---\n\n## ${i + 1}. ${it.name}\n\n${buildComplianceMarkdown(it.result!)}`)
    .join("");
  const failed = items
    .filter((it) => !it.result)
    .map((it) => `- ${it.name}（${it.url}）：${it.err || "未取到结果"}`)
    .join("\n");
  return `${head}${failed ? `\n\n## 未取到结果的频道\n\n${failed}` : ""}${sections}\n`;
}

export function buildComplianceBatchHtml(items: ComplianceBatchItem[]): string {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const ok = items.filter((x) => x.result).length;
  const first = items.find((x) => x.result);
  const headPart = first
    ? buildComplianceHtml(first.result!).replace(/<body[\s\S]*$/i, "<body>")
    : `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>合规检测轮询汇总</title></head><body>`;
  const sections = items
    .filter((it) => it.result)
    .map((it, i) => {
      const html = buildComplianceHtml(it.result!);
      const body = html.replace(/^[\s\S]*<body[^>]*>/i, "").replace(/<\/body>[\s\S]*$/i, "");
      return `<section class="cb-item"><h2>${i + 1}. ${esc(it.name)}</h2>${body}</section>`;
    })
    .join("");
  const summary = `<div class="cb-sum"><h1>合规检测轮询汇总</h1>
    <div class="cb-meta">生成时间 ${esc(now)} · 视听质量检测工作站 · 共 ${items.length} 路，有结果 ${ok} 路，失败 ${items.length - ok} 路</div>
    <table class="cb-summary"><thead><tr><th>频道</th><th>依据</th><th>合格</th><th>不合格</th><th>未标注</th><th>结论</th></tr></thead><tbody>${summaryRows(items)}</tbody></table>
    </div>`;
  return `${headPart}${summary}${sections}</body></html>`;
}

function run(text: string, opts: { bold?: boolean; size?: number; color?: string } = {}) {
  return new TextRun({ text: String(text ?? ""), font: FONT, size: opts.size ?? 20, bold: opts.bold, color: opts.color });
}
function para(text: string, opts: { bold?: boolean; size?: number; color?: string; align?: any; before?: number; after?: number } = {}) {
  return new Paragraph({ children: [run(text, opts)], alignment: opts.align, spacing: { before: opts.before ?? 0, after: opts.after ?? 80 } });
}
function cell(text: string, opts: { bold?: boolean; width?: number; fill?: string } = {}) {
  return new TableCell({
    width: opts.width ? { size: opts.width, type: WidthType.PERCENTAGE } : undefined,
    shading: opts.fill ? { fill: opts.fill } : undefined,
    margins: CELL_MARGINS,
    children: [para(text, { bold: opts.bold, size: 17 })],
  });
}
function headerRow(labels: string[], widths?: number[]) {
  return new TableRow({ tableHeader: true, children: labels.map((t, i) => cell(t, { bold: true, fill: "F1F3F6", width: widths?.[i] })) });
}
function table(rows: TableRow[]) {
  return new Table({ rows, width: { size: 100, type: WidthType.PERCENTAGE }, borders: BORDERS });
}

export async function buildComplianceBatchDocx(items: ComplianceBatchItem[]): Promise<string> {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const ok = items.filter((x) => x.result).length;
  const children: (Paragraph | Table)[] = [
    para("合规检测轮询汇总", { bold: true, size: 40, align: AlignmentType.CENTER, after: 60 }),
    para(`生成时间 ${now} · 视听质量检测工作站 · 共 ${items.length} 路，有结果 ${ok} 路，失败 ${items.length - ok} 路`, {
      size: 18,
      color: "6B7280",
      align: AlignmentType.CENTER,
      after: 180,
    }),
    para("汇总", { bold: true, size: 24, before: 120, after: 80 }),
    table([
      headerRow(["频道", "依据", "合格", "不合格", "未标注", "结论"], [28, 20, 12, 14, 14, 12]),
      ...items.map((it) => {
        if (!it.result) {
          return new TableRow({ children: [cell(it.name, { width: 28 }), cell(it.err || "未取到结果", { width: 72 })] });
        }
        const r = it.result;
        return new TableRow({
          children: [
            cell(it.name, { width: 28 }),
            cell(r.kind, { width: 20 }),
            cell(String(r.summary.pass), { width: 12 }),
            cell(String(r.summary.fail), { width: 14 }),
            cell(String(r.summary.unknown), { width: 14 }),
            cell(conclusion(r), { width: 12 }),
          ],
        });
      }),
    ]),
  ];

  items.forEach((it, i) => {
    children.push(para(`${i + 1}. ${it.name}`, { bold: true, size: 26, before: 260, after: 60 }));
    if (!it.result) {
      children.push(para(it.err || "未取到结果", { color: "9CA3AF" }));
      return;
    }
    const r = it.result;
    children.push(para(r.source, { size: 17, color: "6B7280", after: 100 }));
    children.push(
      table([
        new TableRow({ children: [cell("检测依据", { bold: true, width: 24, fill: "F8FAFC" }), cell(r.kind, { width: 26 }), cell("结论", { bold: true, width: 24, fill: "F8FAFC" }), cell(conclusion(r), { width: 26 })] }),
        new TableRow({ children: [cell("合格", { bold: true, width: 24, fill: "F8FAFC" }), cell(String(r.summary.pass), { width: 26 }), cell("不合格 / 未标注", { bold: true, width: 24, fill: "F8FAFC" }), cell(`${r.summary.fail} / ${r.summary.unknown}`, { width: 26 })] }),
      ]),
    );
    const rows = reportRows(r);
    children.push(para("检测项", { bold: true, size: 20, before: 140, after: 60 }));
    children.push(
      table([
        headerRow(["项目", "标准", "实测", "判定"], [30, 28, 28, 14]),
        ...rows.map((row) =>
          new TableRow({
            children: [
              cell(`${row.no !== "—" ? row.no + " " : ""}${row.item.name}`, { width: 30 }),
              cell(row.item.standard, { width: 28 }),
              cell(row.item.actual, { width: 28 }),
              cell(row.item.verdict === "pass" ? "合格" : row.item.verdict === "fail" ? "不合格" : "未标注", { width: 14 }),
            ],
          }),
        ),
      ]),
    );
  });

  children.push(para("本报告由「视听质量检测工作站」按轮询结果自动汇总；各频道检测项均为各自一次检测的结果。", { size: 17, color: "6B7280", before: 240 }));
  const doc = new Document({
    creator: "视听质量检测工作站",
    title: "合规检测轮询汇总",
    description: "合规检测轮询汇总报告",
    sections: [{ properties: { page: { margin: { top: 720, right: 720, bottom: 720, left: 720 } } }, children }],
  });
  return Packer.toBase64String(doc);
}
