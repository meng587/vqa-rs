//! 合规检测报告 —— Markdown / HTML / Word 三种格式。
//! 三种格式共用同一套行序、序号和判定文案，避免导出结果互相不一致。

import {
  AlignmentType,
  BorderStyle,
  Document,
  PageOrientation,
  Packer,
  Paragraph,
  Table,
  TableCell,
  TableLayoutType,
  TableRow,
  TextRun,
  WidthType,
} from "docx";
import type { ComplianceResult } from "./api";
import { VERDICT_TXT, reportFootnote, reportRows } from "./compliance-meta";

const FONT = "Microsoft YaHei";
const BAND = { style: BorderStyle.SINGLE, size: 4, color: "D9D9D9" };
const BORDERS = { top: BAND, bottom: BAND, left: BAND, right: BAND, insideHorizontal: BAND, insideVertical: BAND };
const CELL_MARGINS = { top: 60, bottom: 60, left: 80, right: 80 };

const nowText = () => new Date().toLocaleString("zh-CN", { hour12: false });

function conclusion(r: ComplianceResult): string {
  const s = r.summary;
  if (s.ok && s.fail === 0 && s.unknown === 0) return "全部合格";
  return s.ok ? "无不合格项" : "存在不合格项";
}

function statsText(r: ComplianceResult): string {
  return `合格 ${r.summary.pass} / 不合格 ${r.summary.fail} / 未标注 ${r.summary.unknown}`;
}

/** Markdown 表格单元格：竖线换成全角，换行压成空格，避免破表 */
function mdCell(s: string): string {
  return String(s ?? "").replace(/\|/g, "／").replace(/\r?\n/g, " ");
}

function escHtml(s: string): string {
  return String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function basisLine(r: ComplianceResult): string {
  return `《广电办发〔2024〕253 号》《广播电视和网络视听节目技术质量监督检测工作方案》${r.kind}`;
}

export function buildComplianceMarkdown(r: ComplianceResult): string {
  const lines: string[] = [];
  lines.push("# 合规性检测报告", "");
  lines.push(`- 依据：${basisLine(r)}`);
  lines.push(`- 来源：${r.source}`);
  lines.push(`- 时间：${nowText()}`);
  lines.push(`- 结论：${conclusion(r)}（${statsText(r)}）`);

  if ((r.basis || []).length) {
    lines.push("", "## 检测依据", "");
    for (const b of r.basis) lines.push(`- ${b.code}《${b.name}》`);
  }

  lines.push("", "## 检测结果", "");
  lines.push("| 序号 | 类别 | 检测项 | 标准要求 | 实测值 | 判定 | 说明 |");
  lines.push("|------|------|--------|----------|--------|------|------|");
  for (const row of reportRows(r)) {
    const it = row.item;
    lines.push(
      `| ${row.no} | ${mdCell(row.group)} | ${mdCell(it.name)} | ${mdCell(it.standard)} | ${mdCell(it.actual)} | ${VERDICT_TXT[it.verdict]} | ${mdCell(it.note)} |`,
    );
  }
  lines.push("", `> ${reportFootnote(r.kind)}`);
  return lines.join("\n");
}

export function buildComplianceHtml(r: ComplianceResult): string {
  const rows = reportRows(r)
    .map((row, i) => {
      const it = row.item;
      return `<tr class="${i % 2 ? "alt" : ""}">
  <td class="no">${escHtml(row.no)}</td>
  <td class="cat">${escHtml(row.group)}</td>
  <td class="name">${escHtml(it.name)}</td>
  <td class="std">${escHtml(it.standard)}</td>
  <td class="act">${escHtml(it.actual)}</td>
  <td class="vd"><span class="v ${escHtml(it.verdict)}">${escHtml(VERDICT_TXT[it.verdict] || it.verdict)}</span></td>
  <td class="note">${escHtml(it.note)}</td>
</tr>`;
    })
    .join("\n");

  const basis = (r.basis || []).length
    ? `<section class="basis"><h2>检测依据</h2><ul>${r.basis
        .map((b) => `<li><b>${escHtml(b.code)}</b>《${escHtml(b.name)}》</li>`)
        .join("")}</ul></section>`
    : "";

  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>合规性检测报告</title>
<style>
:root { color-scheme: light; }
* { box-sizing: border-box; }
body { margin: 0; background: #f3f5f8; color: #1f2937; font-family: "Microsoft YaHei", "PingFang SC", "Noto Sans SC", sans-serif; font-size: 13px; line-height: 1.6; }
.sheet { max-width: 1180px; margin: 28px auto 48px; background: #fff; padding: 34px 40px 28px; box-shadow: 0 1px 3px rgba(15, 23, 42, .12); }
.head { border-bottom: 1px solid #e5e7eb; padding-bottom: 16px; margin-bottom: 20px; }
h1 { margin: 0 0 6px; font-size: 24px; letter-spacing: .02em; color: #0f172a; }
.sub { margin: 0; color: #64748b; font-size: 12px; }
.cards { display: flex; flex-wrap: wrap; gap: 10px; margin: 18px 0 20px; }
.card { flex: 1 1 150px; border: 1px solid #e5e7eb; border-radius: 8px; padding: 11px 14px; background: #fafbfc; }
.card .k { color: #64748b; font-size: 12px; }
.card .v { margin-top: 3px; font-size: 17px; font-weight: 700; }
.card.ok .v { color: #047857; }
.card.bad .v { color: #b91c1c; }
.card.warn .v { color: #b45309; }
.meta { width: 100%; border-collapse: collapse; margin: 4px 0 20px; }
.meta th { width: 120px; text-align: left; color: #475569; font-weight: 600; background: #f8fafc; }
.meta th, .meta td { border: 1px solid #e5e7eb; padding: 7px 10px; vertical-align: top; }
.meta td { word-break: break-all; }
h2 { margin: 24px 0 9px; font-size: 15px; color: #0f172a; }
.basis ul { margin: 0; padding-left: 20px; color: #475569; }
.basis li { margin: 2px 0; }
table.result { width: 100%; border-collapse: collapse; table-layout: fixed; font-size: 12px; }
table.result thead th { background: #334155; color: #fff; font-weight: 600; padding: 8px 6px; border: 1px solid #334155; text-align: left; }
table.result td { padding: 7px 6px; border: 1px solid #e2e8f0; vertical-align: top; word-break: break-word; }
table.result tr.alt td { background: #fbfcfe; }
table.result .no { width: 48px; text-align: center; color: #64748b; }
table.result .cat { width: 86px; color: #475569; }
table.result .name { width: 132px; font-weight: 600; }
table.result .std { width: 250px; }
table.result .act { width: 220px; }
table.result .vd { width: 88px; text-align: center; }
table.result .note { width: 150px; color: #475569; }
.v { display: inline-block; min-width: 64px; padding: 2px 7px; border-radius: 999px; font-size: 11px; font-weight: 600; }
.v.pass { color: #047857; background: #ecfdf5; }
.v.fail { color: #b91c1c; background: #fef2f2; }
.v.unknown { color: #6b7280; background: #f3f4f6; }
.foot { margin-top: 22px; padding-top: 12px; border-top: 1px solid #e5e7eb; color: #64748b; font-size: 11px; }
.foot .note { margin-bottom: 6px; }
@media print { body { background: #fff; } .sheet { max-width: none; margin: 0; padding: 0; box-shadow: none; } @page { size: A4 landscape; margin: 12mm; } }
</style>
</head>
<body>
<div class="sheet">
  <header class="head">
    <h1>合规性检测报告</h1>
    <p class="sub">${escHtml(basisLine(r))} · 生成时间 ${escHtml(nowText())}</p>
  </header>

  <div class="cards">
    <div class="card ${r.summary.ok ? "ok" : "bad"}"><div class="k">检测结论</div><div class="v">${escHtml(conclusion(r))}</div></div>
    <div class="card ok"><div class="k">合格</div><div class="v">${r.summary.pass}</div></div>
    <div class="card ${r.summary.fail ? "bad" : ""}"><div class="k">不合格</div><div class="v">${r.summary.fail}</div></div>
    <div class="card ${r.summary.unknown ? "warn" : ""}"><div class="k">未标注</div><div class="v">${r.summary.unknown}</div></div>
  </div>

  <table class="meta">
    <tr><th>检测来源</th><td>${escHtml(r.source)}</td></tr>
    <tr><th>检测依据</th><td>${escHtml(r.kind)}</td></tr>
    <tr><th>统计</th><td>${escHtml(statsText(r))}</td></tr>
  </table>

  ${basis}

  <section class="result-sec">
    <h2>检测结果</h2>
    <table class="result">
      <thead><tr><th>序号</th><th>类别</th><th>检测项</th><th>标准要求</th><th>实测值</th><th>判定</th><th>说明</th></tr></thead>
      <tbody>
${rows}
      </tbody>
    </table>
  </section>

  <footer class="foot">
    <div class="note">${escHtml(reportFootnote(r.kind))}</div>
    <div>本报告由「视听质量检测工作站」自动生成；未标注项不等于合格，请按 253 号文要求补测。</div>
  </footer>
</div>
</body>
</html>`;
}

// ── Word ──

type Align = (typeof AlignmentType)[keyof typeof AlignmentType];

function run(text: string, opts: { bold?: boolean; size?: number; color?: string } = {}) {
  return new TextRun({
    text: String(text ?? ""),
    font: FONT,
    size: opts.size ?? 19,
    bold: opts.bold,
    color: opts.color,
  });
}

function para(
  text: string,
  opts: { bold?: boolean; size?: number; color?: string; align?: Align; before?: number; after?: number } = {},
) {
  return new Paragraph({
    children: [run(text, opts)],
    alignment: opts.align,
    spacing: { before: opts.before ?? 0, after: opts.after ?? 80 },
  });
}

function cell(
  text: string,
  opts: {
    bold?: boolean;
    align?: Align;
    fill?: string;
    width?: number;
    color?: string;
    size?: number;
  } = {},
) {
  return new TableCell({
    width: opts.width ? { size: opts.width, type: WidthType.PERCENTAGE } : undefined,
    shading: opts.fill ? { fill: opts.fill } : undefined,
    margins: CELL_MARGINS,
    children: [
      new Paragraph({
        alignment: opts.align,
        spacing: { before: 0, after: 0 },
        children: [run(text, { bold: opts.bold, color: opts.color, size: opts.size ?? 18 })],
      }),
    ],
  });
}

function table(rows: TableRow[]) {
  return new Table({
    width: { size: 100, type: WidthType.PERCENTAGE },
    layout: TableLayoutType.FIXED,
    borders: BORDERS,
    rows,
  });
}

function kvTable(rows: [string, string][]) {
  return table(
    rows.map(([k, v]) =>
      new TableRow({
        children: [
          cell(k, { bold: true, fill: "F8FAFC", width: 20, color: "475569" }),
          cell(v, { width: 80 }),
        ],
      }),
    ),
  );
}

export async function buildComplianceDocx(r: ComplianceResult): Promise<string> {
  const rows = reportRows(r);
  const verdictFill: Record<string, string> = {
    pass: "ECFDF5",
    fail: "FEF2F2",
    unknown: "F3F4F6",
  };
  const verdictColor: Record<string, string> = {
    pass: "047857",
    fail: "B91C1C",
    unknown: "6B7280",
  };

  const children: (Paragraph | Table)[] = [
    para("合规性检测报告", { bold: true, size: 40, align: AlignmentType.CENTER, after: 60 }),
    para(`生成时间 ${nowText()} · 视听质量检测工作站`, {
      size: 17,
      color: "6B7280",
      align: AlignmentType.CENTER,
      after: 180,
    }),
    kvTable([
      ["检测依据", basisLine(r)],
      ["检测来源", r.source],
      ["检测结论", `${conclusion(r)}（${statsText(r)}）`],
    ]),
  ];

  if ((r.basis || []).length) {
    children.push(para("检测依据清单", { bold: true, size: 26, color: "334155", before: 240, after: 100 }));
    for (const b of r.basis) children.push(para(`• ${b.code}《${b.name}》`, { size: 18, after: 40 }));
  }

  children.push(para("检测结果", { bold: true, size: 26, color: "334155", before: 240, after: 100 }));
  children.push(
    table([
      new TableRow({
        tableHeader: true,
        children: [
          cell("序号", { bold: true, fill: "334155", color: "FFFFFF", width: 6, align: AlignmentType.CENTER }),
          cell("类别", { bold: true, fill: "334155", color: "FFFFFF", width: 10 }),
          cell("检测项", { bold: true, fill: "334155", color: "FFFFFF", width: 17 }),
          cell("标准要求", { bold: true, fill: "334155", color: "FFFFFF", width: 24 }),
          cell("实测值", { bold: true, fill: "334155", color: "FFFFFF", width: 22 }),
          cell("判定", { bold: true, fill: "334155", color: "FFFFFF", width: 10, align: AlignmentType.CENTER }),
          cell("说明", { bold: true, fill: "334155", color: "FFFFFF", width: 11 }),
        ],
      }),
      ...rows.map(
        (row, i) =>
          new TableRow({
            children: [
              cell(row.no, { width: 6, align: AlignmentType.CENTER, color: "6B7280", fill: i % 2 ? "FBFCFE" : undefined }),
              cell(row.group, { width: 10, color: "475569", fill: i % 2 ? "FBFCFE" : undefined }),
              cell(row.item.name, { width: 17, bold: true, fill: i % 2 ? "FBFCFE" : undefined }),
              cell(row.item.standard, { width: 24, fill: i % 2 ? "FBFCFE" : undefined }),
              cell(row.item.actual, { width: 22, fill: i % 2 ? "FBFCFE" : undefined }),
              cell(VERDICT_TXT[row.item.verdict] || row.item.verdict, {
                width: 10,
                align: AlignmentType.CENTER,
                bold: true,
                color: verdictColor[row.item.verdict] || "6B7280",
                fill: verdictFill[row.item.verdict],
              }),
              cell(row.item.note, { width: 11, color: "475569", fill: i % 2 ? "FBFCFE" : undefined }),
            ],
          }),
      ),
    ]),
  );

  children.push(
    para(reportFootnote(r.kind), { size: 16, color: "6B7280", before: 220, after: 60 }),
    para("本报告由「视听质量检测工作站」自动生成；未标注项不等于合格，请按 253 号文要求补测。", {
      size: 16,
      color: "6B7280",
      after: 0,
    }),
  );

  const doc = new Document({
    creator: "视听质量检测工作站",
    title: "合规性检测报告",
    description: "视听质量检测工作站自动生成的合规性检测报告",
    sections: [
      {
        properties: {
          page: {
            size: { width: 11906, height: 16838, orientation: PageOrientation.LANDSCAPE },
            margin: { top: 720, right: 720, bottom: 720, left: 720 },
          },
        },
        children,
      },
    ],
  });
  return Packer.toBase64String(doc);
}
