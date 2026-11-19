//! 合规检测的固定口径：检测项归组、253 号文原序号、判定文案。
//! 页面表格与三种格式的导出报告共用同一套映射，避免两处口径漂移。

import type { CItem } from "./api";

/** 检测项归组（key → 类别名），顺序即表格里的固定分组 */
export const GROUP: Record<string, string> = {
  res: "节目技术质量",
  fps: "节目技术质量",
  codec: "节目技术质量",
  profile: "节目技术质量",
  bits: "节目技术质量",
  colorspace: "节目技术质量",
  transfer: "节目技术质量",
  vbitrate: "节目技术质量",
  acodec: "节目技术质量",
  asr: "节目技术质量",
  abr: "节目技术质量",
  jitter: "网络传输质量",
  loss: "网络传输质量",
  df: "网络传输质量",
  mlr: "网络传输质量",
  qam: "有线 RF 指标",
  mer: "有线 RF 指标",
  ber: "有线 RF 指标",
  catv_df: "IP 信号指标",
  catv_mlr: "IP 信号指标",
  ev_pid: "音视频质量（事件类）",
  ev_decode: "音视频质量（事件类）",
  ev_audio: "音视频质量（事件类）",
  ev_video: "音视频质量（事件类）",
};

/** 原文序号（表 2 序号 1~22；表 1 用同一套号码，另加 16/17 的 RF 与 IP 信号项）。
 *  序号 7~10 是"视频编码类和级"的四档并列要求，本程序合成一行显示。 */
export const NO_IPTV: Record<string, string> = {
  res: "1", fps: "2", colorspace: "3", bits: "4", transfer: "5", codec: "6",
  profile: "7~10", vbitrate: "11", acodec: "12", asr: "13", abr: "14",
  jitter: "15", loss: "16", df: "17", mlr: "18",
  ev_pid: "19", ev_decode: "20", ev_audio: "21", ev_video: "22",
};
export const NO_CATV: Record<string, string> = {
  res: "1", fps: "2", colorspace: "3", bits: "4", transfer: "5", codec: "6",
  profile: "7~10", vbitrate: "11", acodec: "12", asr: "13", abr: "14",
  ev_pid: "15", qam: "16", mer: "16", ber: "16", catv_df: "17", catv_mlr: "17",
};

export const VERDICT_TXT: Record<string, string> = {
  pass: "✓ 合格",
  fail: "✗ 不合格",
  unknown: "— 未标注",
};

/** 当前依据表下某一项的原文序号 */
export function noOf(kind: string, key: string): string {
  const no = kind.indexOf("有线") >= 0 ? NO_CATV : NO_IPTV;
  return no[key] || "—";
}

export interface CReportRow {
  no: string;
  group: string;
  item: CItem;
}

/** 按页面同序展开报告行 */
export function reportRows(r: { kind: string; items: CItem[] }): CReportRow[] {
  return (r.items || []).map((item) => ({
    no: noOf(r.kind, item.key),
    group: GROUP[item.key] || "其它",
    item,
  }));
}

/** 导出报告尾注（按依据表区分可实测项） */
export function reportFootnote(kind: string): string {
  return kind.indexOf("有线") >= 0
    ? "有线表 1 的 QAM 电平 / MER / BER 与画面异常需专用仪表或逐帧解码，本程序不判定；序号 15 已按 TS PID 层实测丢包与无有效净荷。"
    : "表 2 序号 19 / 21 已按 TS PID 层实测（丢包与无有效净荷）；序号 20 / 22 需逐帧解码与参考比对，本程序标「未标注」。";
}
