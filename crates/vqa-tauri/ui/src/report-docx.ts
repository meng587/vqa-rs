//! 视频分析报告 —— Word (.docx) 版本。
//! 用 docx 生成真正的 OOXML 文档，表格可直接在 Word 里编辑。

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

const FONT = "Microsoft YaHei";
const B = { style: BorderStyle.SINGLE, size: 4, color: "D9D9D9" };
const BORDERS = { top: B, bottom: B, left: B, right: B, insideHorizontal: B, insideVertical: B };
const CELL_MARGINS = { top: 60, bottom: 60, left: 100, right: 100 };

const mb = (n: number) => `${(n / 1048576).toFixed(2)} MB`;
const mbps = (n: number) => `${(n / 1e6).toFixed(2)} Mbps`;

type Align = (typeof AlignmentType)[keyof typeof AlignmentType];

function run(text: string, opts: { bold?: boolean; size?: number; color?: string } = {}) {
  return new TextRun({
    text: String(text ?? ""),
    font: FONT,
    size: opts.size ?? 20,
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

function sectionTitle(text: string) {
  return para(text, { bold: true, size: 28, color: "334155", before: 240, after: 100 });
}

function cell(
  text: string,
  opts: { bold?: boolean; align?: Align; fill?: string; width?: number } = {},
) {
  return new TableCell({
    width: opts.width ? { size: opts.width, type: WidthType.PERCENTAGE } : undefined,
    shading: opts.fill ? { fill: opts.fill } : undefined,
    margins: CELL_MARGINS,
    children: [
      new Paragraph({
        alignment: opts.align,
        spacing: { before: 0, after: 0 },
        children: [run(text, { bold: opts.bold, size: 19 })],
      }),
    ],
  });
}

function table(rows: TableRow[]) {
  return new Table({
    width: { size: 100, type: WidthType.PERCENTAGE },
    borders: BORDERS,
    rows,
  });
}

function headerRow(labels: string[], widths?: number[]) {
  return new TableRow({
    tableHeader: true,
    children: labels.map((t, i) => cell(t, { bold: true, fill: "F1F3F6", width: widths?.[i] })),
  });
}

function kvTable(rows: [string, string][]) {
  return table(
    rows.map(([k, v]) =>
      new TableRow({
        children: [
          cell(k, { bold: true, fill: "F8FAFC", width: 28 }),
          cell(v, { width: 72 }),
        ],
      }),
    ),
  );
}

export async function buildDocxReport(r: Res): Promise<string> {
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const tr = r.tr290 || [];
  const bad = tr.filter((t) => t.level >= 2);
  const pids = (r.pids || []).slice(0, 400);
  const progs = r.programs || [];
  const rateMax = (r.rate || []).reduce((m, x) => Math.max(m, x[1] || 0), 0);

  const children: (Paragraph | Table)[] = [
    para("TS 流质量检测报告", { bold: true, size: 40, align: AlignmentType.CENTER, after: 60 }),
    para(`生成时间 ${now} · 视听质量检测工作站`, {
      size: 18,
      color: "6B7280",
      align: AlignmentType.CENTER,
      after: 180,
    }),
  ];

  children.push(sectionTitle("摘要"));
  children.push(
    kvTable([
      ["码流平均码率", mbps(r.bitrate_avg)],
      ["文件大小", mb(r.file_size)],
      ["包数", String(r.packets)],
      ["同步错误", String(r.sync_errors)],
      ["PCR 时长", `${(r.duration_pcr || 0).toFixed(2)} s`],
      ["TR290 异常项", String(bad.length)],
      ["节目数 / PID 数", `${progs.length} / ${(r.pids || []).length}`],
      ["加密", r.encrypted || r.has_scrambled ? "有（加扰）" : "无"],
    ]),
  );

  children.push(sectionTitle("基本信息"));
  children.push(
    kvTable([
      ["来源", r.path],
      ["包长", `${r.packet_size} 字节`],
      ["PCR PID", r.pcr_pid == null ? "无" : String(r.pcr_pid)],
      ["PCR 恒定码率", r.pcr_cbr ? "是" : "否"],
      ["SI 截断", r.si_truncated ? "是（表未取全）" : "否"],
      ["GOP", r.gop_note || "—"],
      ["峰值码率（采样）", rateMax ? mbps(rateMax) : "—"],
    ]),
  );

  children.push(sectionTitle("TR 290 优先级 1/2/3"));
  if (tr.length) {
    children.push(
      table(
        [
          headerRow(["级别", "项目", "次数", "首次", "说明"], [8, 22, 10, 24, 56]),
          ...tr.map(
            (t) =>
              new TableRow({
                children: [
                  cell(`L${t.level}`, { width: 8, align: AlignmentType.CENTER }),
                  cell(t.name, { width: 22 }),
                  cell(String(t.count), { width: 10, align: AlignmentType.RIGHT }),
                  cell(t.first || "", { width: 24 }),
                  cell(t.detail || "", { width: 56 }),
                ],
              }),
          ),
        ],
      ),
    );
  } else {
    children.push(para("未采到 TR290 指标。", { color: "6B7280" }));
  }

  children.push(sectionTitle("节目结构"));
  if (progs.length) {
    for (const p of progs) {
      children.push(para(`节目 ${p.number}`, { bold: true, size: 22, before: 120, after: 60 }));
      children.push(
        kvTable([
          ["PMT PID", String(p.pmt_pid)],
          ["PCR PID", String(p.pcr_pid)],
        ]),
      );
      if ((p.streams || []).length) {
        children.push(
          table(
            [
              headerRow(["PID", "类型", "种类"], [22, 42, 36]),
              ...p.streams.map(
                (s) =>
                  new TableRow({
                    children: [
                      cell(String(s.pid), { width: 22, align: AlignmentType.RIGHT }),
                      cell(s.type_name, { width: 42 }),
                      cell(s.kind, { width: 36 }),
                    ],
                  }),
              ),
            ],
          ),
        );
      }
    }
  } else {
    children.push(para("未读到节目表（PAT/PMT 缺失或非 TS 封装）。", { color: "6B7280" }));
  }

  children.push(sectionTitle(`PID 占用（${pids.length} / ${(r.pids || []).length}，按字节降序）`));
  if (pids.length) {
    children.push(
      table(
        [
          headerRow(["PID", "种类", "包数", "字节", "净荷", "加扰"], [10, 22, 12, 18, 18, 18]),
          ...pids.map(
            (p) =>
              new TableRow({
                children: [
                  cell(String(p.pid), { width: 10, align: AlignmentType.RIGHT }),
                  cell(p.kind, { width: 22 }),
                  cell(String(p.packets), { width: 12, align: AlignmentType.RIGHT }),
                  cell(mb(p.bytes), { width: 18, align: AlignmentType.RIGHT }),
                  cell(mb(p.payload_bytes), { width: 18, align: AlignmentType.RIGHT }),
                  cell(String(p.scrambled || 0), { width: 18, align: AlignmentType.RIGHT }),
                ],
              }),
          ),
        ],
      ),
    );
  } else {
    children.push(para("无 PID 数据。", { color: "6B7280" }));
  }

  children.push(
    para(
      "本报告由「视听质量检测工作站」自动生成，数值取自对源的一次完整扫描；如需复核请保留原始码流与同版本程序。",
      { size: 17, color: "6B7280", before: 240, after: 0 },
    ),
  );

  const doc = new Document({
    creator: "视听质量检测工作站",
    title: "TS 流质量检测报告",
    description: "视听质量检测工作站自动生成的 TS 流质量检测报告",
    sections: [
      {
        properties: { page: { margin: { top: 720, right: 720, bottom: 720, left: 720 } } },
        children,
      },
    ],
  });
  return Packer.toBase64String(doc);
}
