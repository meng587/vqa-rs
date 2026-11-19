//! 合规检测页 —— 依据《广电办发〔2024〕253 号》表 1 / 表 2。
//!
//! 数据来源：
//! - 文件：`compliance_check`（ffprobe → 节目技术质量 11 项）
//! - udp/rtp：`compliance_live_start` 收流 + `compliance_live_snapshot` 轮询
//!   （网络 4 项实测；节目技术质量由后端后台探测补齐）
//!
//! 判定三态与后端一致：pass(✓ 合格) / fail(✗ 不合格) / unknown(— 未标注)。
//! 未标注不等于合格 —— 页面汇总单独计一栏，避免"看起来全绿"。

import {
  api,
  type ComplianceLive,
  type ComplianceResult,
  type NicRow,
} from "./api";
import { setNavLamp, nicLabel } from "./main";
import { renderComplianceDoc } from "./compliance-doc";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

/** 检测项归组（key → 类别名），顺序即表格里的固定分组 */
const GROUP: Record<string, string> = {
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
const NO_IPTV: Record<string, string> = {
  res: "1", fps: "2", colorspace: "3", bits: "4", transfer: "5", codec: "6",
  profile: "7~10", vbitrate: "11", acodec: "12", asr: "13", abr: "14",
  jitter: "15", loss: "16", df: "17", mlr: "18",
  ev_pid: "19", ev_decode: "20", ev_audio: "21", ev_video: "22",
};
const NO_CATV: Record<string, string> = {
  res: "1", fps: "2", colorspace: "3", bits: "4", transfer: "5", codec: "6",
  profile: "7~10", vbitrate: "11", acodec: "12", asr: "13", abr: "14",
  ev_pid: "15", qam: "16", mer: "16", ber: "16", catv_df: "17", catv_mlr: "17",
};

const VERDICT_TXT: Record<string, string> = {
  pass: "✓ 合格",
  fail: "✗ 不合格",
  unknown: "— 未标注",
};

let last: ComplianceResult | null = null;
let pollTimer: number | null = null;
let liveMode = false;

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function setStatus(text: string, state: "idle" | "run" | "err") {
  $("k-status-text").textContent = text;
  const dot = $("k-dot");
  dot.className = "dot" + (state === "run" ? " run" : state === "err" ? " err" : "");
  setNavLamp("compliance", state === "run" ? "running" : state === "err" ? "error" : "idle");
}

function renderChips(sum: ComplianceResult["summary"]) {
  const okTag = sum.ok && sum.fail === 0 && sum.unknown === 0 ? "全部合格" : sum.ok ? "无不合格项" : "存在不合格项";
  $("k-sum").innerHTML = [
    `<span class="chip-m ${sum.ok ? "ok" : "warn"}">${okTag}</span>`,
    `<span class="chip-m ok">✓ ${sum.pass}</span>`,
    `<span class="chip-m ${sum.fail > 0 ? "bad" : ""}">✗ ${sum.fail}</span>`,
    `<span class="chip-m">— ${sum.unknown}</span>`,
  ].join("");
}

function render(r: ComplianceResult) {
  last = r;
  $("k-empty").hidden = true;
  $("k-tbl").hidden = false;
  $("k-foot").hidden = false;

  // 分组渲染：类别用小标题行，不再每行重复
  const NO = r.kind.indexOf("有线") >= 0 ? NO_CATV : NO_IPTV;
  let curGrp = "";
  let rows = "";
  for (const it of r.items || []) {
    const grp = GROUP[it.key] || "其它";
    if (grp !== curGrp) {
      curGrp = grp;
      rows += `<div class="ksec">${esc(grp)}</div>`;
    }
    const no = NO[it.key] ? `<i class="kno">${NO[it.key]}</i>` : "";
    rows += `<div class="krow">
        <span class="kname">${no}${esc(it.name)}</span>
        <span class="kstd">${esc(it.standard)}</span>
        <span class="kact">${esc(it.actual)}</span>
        <span class="kvd"><span class="vd vd-${it.verdict}">${VERDICT_TXT[it.verdict] || it.verdict}</span></span>
        <span class="knote">${esc(it.note)}</span>
      </div>`;
  }
  $("k-rows").innerHTML = rows;
  renderChips(r.summary);

  const footText =
    r.kind.indexOf("有线") >= 0
      ? "表 1 的 QAM 电平 / MER / BER 与画面异常需电平表、码流仪或逐帧解码，软件不判；序号 15 已按 TS PID 层实测丢包与无有效净荷。"
      : "表 2 序号 19 / 21 已按 TS PID 层实测（丢包与无有效净荷）；20 / 22 需逐帧解码与参考比对，标「未标注」。";
  const basisTxt = (r.basis || []).length
    ? `<span class="kfoot-basis">检测依据：《广电办发〔2024〕253 号》，共 ${(r.basis || []).length} 项标准与文件 · <a class="kfoot-link" data-view="doc">查看检测依据与判定说明</a></span>`
    : "";
  const src = r.source ? `<span class="kfoot-src">来源：${esc(r.source)}</span>` : "";
  $("k-foot").innerHTML =
    `<span class="kfoot-note">${footText}</span>${basisTxt}${src}`;
}

function stopPolling() {
  if (pollTimer != null) {
    window.clearInterval(pollTimer);
    pollTimer = null;
  }
}

function liveEnded(msg: string, state: "idle" | "err") {
  stopPolling();
  liveMode = false;
  $("k-stop").hidden = true;
  ($("k-act") as HTMLButtonElement).disabled = false;
  setStatus(msg, state);
}

async function tick() {
  const kind = ($("k-kind") as HTMLSelectElement).value;
  try {
    const s = await api.complianceLiveSnapshot(kind);
    if (s.err) {
      liveEnded(`收流失败：${s.err}`, "err");
      return;
    }
    if (!s.alive) {
      liveEnded("收流已断开", "err");
    }
    const mm = String(Math.floor(s.secs / 60)).padStart(2, "0");
    const ss = String(s.secs % 60).padStart(2, "0");
    const probeTag = s.probe_ready
      ? ""
      : s.probe_state.startsWith("err")
        ? " · 节目项探测失败（直播流无法取编码参数）"
        : " · 节目项探测中";
    setStatus(
      s.stats.packets > 0
        ? `收流中 ${mm}:${ss} · ${s.stats.bitrate_mbps.toFixed(2)} Mbps · ${s.stats.packets} 包${probeTag}`
        : `收流中 ${mm}:${ss} · 尚未收到数据（检查网卡/组播地址）`,
      "run",
    );
    // 节目项还没探到时先只画网络项，避免出现"半张空表"
    render({
      mode: s.mode,
      source: s.source,
      kind: s.kind,
      items: s.items,
      summary: s.summary,
      basis: s.basis,
    });
  } catch (e) {
    liveEnded(`快照失败：${e}`, "err");
  }
}

export function initCompliance() {
  const url = $("k-url") as HTMLInputElement;
  const nic = $("k-nic") as HTMLSelectElement | null;
  const act = $("k-act") as HTMLButtonElement;
  const stop = $("k-stop") as HTMLButtonElement;
  const kind = $("k-kind") as HTMLSelectElement;
  const histBtn = $("k-hist") as HTMLButtonElement;
  const histPop = $("k-histpop") as HTMLElement;

  // 合规说明是页内视图：顶栏书本图标或页脚链接打开，说明页里点「返回检测」关掉
  const docView = $("k-doc");
  const docBtn = $("k-kdoc") as HTMLButtonElement;
  const showDoc = (on: boolean) => {
    if (on) renderComplianceDoc(docView);
    docView.hidden = !on;
    $("k-status").hidden = on;
    $("k-wrap").hidden = on;
    docBtn.classList.toggle("is-on", on);
    docBtn.title = on ? "返回检测结果" : "合规说明（检测依据与判定口径）";
  };
  docBtn.addEventListener("click", () => showDoc(docView.hidden));
  docView.addEventListener("click", (ev) => {
    if ((ev.target as HTMLElement).closest('[data-view="check"]')) showDoc(false);
  });
  $("k-foot").addEventListener("click", (ev) => {
    if ((ev.target as HTMLElement).closest('[data-view="doc"], .kfoot-link')) showDoc(true);
  });

  // 网卡：值 = IP，显示「IP · 网卡名」；位置在「合规检测设置」抽屉里
  api.listNics().then(async (list: NicRow[]) => {
    if (!nic) return;
    nic.innerHTML = [`<option value="">自动</option>`]
      .concat(list.map((n) => `<option value="${n.ip}">${nicLabel(n)}</option>`))
      .join("");
    try {
      const saved = await api.uiGet("compliance.nic");
      if (typeof saved === "string" && list.some((n) => n.ip === saved)) nic.value = saved;
    } catch {
      /* 没保存过就保持自动 */
    }
  });

  // 历史来源
  const refreshHist = () => {
    api.sourceHistory("compliance").then((list) => {
      if (!list.length) {
        histPop.hidden = true;
        return;
      }
      histPop.innerHTML = list
        .map((h) => `<button data-url="${esc(h)}">${esc(h)}</button>`)
        .join("");
      histPop.hidden = false;
    });
  };
  histBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (histPop.hidden) refreshHist();
    else histPop.hidden = true;
  });
  histPop.addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest("button") as HTMLButtonElement | null;
    if (!b) return;
    url.value = b.getAttribute("data-url") || "";
    histPop.hidden = true;
  });
  document.addEventListener("click", () => {
    histPop.hidden = true;
  });

  $("k-browse").addEventListener("click", async () => {
    const p = await api.browse();
    if (p) {
      url.value = p;
      api.pushSourceHistory("compliance", p);
    }
  });

  stop.addEventListener("click", async () => {
    await api.complianceLiveStop();
    liveEnded("已停止收流", "idle");
  });

  act.addEventListener("click", async () => {
    showDoc(false); // 开始检测就回检测视图，别让结果藏在说明后面
    const src = url.value.trim();
    if (!src) {
      setStatus("请先填来源（TS 文件或 udp://@组播地址）", "err");
      return;
    }
    const isLive = /^udp:|^rtp:/i.test(src);
    api.pushSourceHistory("compliance", src);
    act.disabled = true;
    setStatus(isLive ? "正在起收流…" : "正在探测…", "run");
    try {
      if (isLive) {
        liveMode = true;
        await api.complianceLiveStart(src, nic?.value || "", kind.value);
        stop.hidden = false;
        $("k-empty").hidden = true;
        stopPolling();
        pollTimer = window.setInterval(tick, 1000);
        await tick();
      } else {
        const r = await api.complianceCheck(src, kind.value);
        render(r);
        setStatus(`检测完成 · ${r.kind} · 合格 ${r.summary.pass} / 不合格 ${r.summary.fail} / 未标注 ${r.summary.unknown}`, r.summary.ok ? "idle" : "err");
        act.disabled = false;
      }
    } catch (e) {
      act.disabled = false;
      setStatus(`检测失败：${e}`, "err");
    }
  });

  // 切换依据（表 1 / 表 2）时清掉旧结果，避免用上一张表的标准看旧数据
  kind.addEventListener("change", () => {
    if (liveMode) return;
    last = null;
    $("k-tbl").hidden = true;
    $("k-foot").hidden = true;
    $("k-empty").hidden = false;
    $("k-sum").innerHTML = "";
    setStatus("已切换依据，请重新开始检测", "idle");
  });

  // 回车 = 开始（与其它页一致）
  url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") act.click();
  });

  $("k-export").addEventListener("click", async () => {
    if (!last) {
      setStatus("还没有检测结果可导出", "err");
      return;
    }
    const md = toMarkdown(last);
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
    try {
      const p = await api.complianceExport(`合规检测_${stamp}.md`, md);
      if (p) setStatus(`报告已导出：${p}`, "idle");
    } catch (e) {
      setStatus(`导出失败：${e}`, "err");
    }
  });

  // 从别的页切回来时把收流状态补上
  window.addEventListener("page-changed", () => {
    if (liveMode && pollTimer == null) {
      pollTimer = window.setInterval(tick, 1000);
    }
  });
}

/** 导出用的 Markdown（与页面上表同序，含判定符号）。 */
function toMarkdown(r: ComplianceResult): string {
  const lines: string[] = [];
  lines.push(`# 合规性检测报告`);
  lines.push("");
  lines.push(`- 依据：《广电办发〔2024〕253 号》《广播电视和网络视听节目技术质量监督检测工作方案》${r.kind}`);
  lines.push(`- 来源：${r.source}`);
  lines.push(`- 时间：${new Date().toLocaleString("zh-CN")}`);
  lines.push(
    `- 结论：${r.summary.ok ? "无不合格项" : "存在不合格项"}（合格 ${r.summary.pass} / 不合格 ${r.summary.fail} / 未标注 ${r.summary.unknown}）`,
  );
  if ((r.basis || []).length) {
    lines.push("");
    lines.push("## 检测依据");
    lines.push("");
    for (const b of r.basis) lines.push(`- ${b.code}《${b.name}》`);
  }
  lines.push("");
  lines.push("| 序号 | 类别 | 检测项 | 标准要求 | 实测值 | 判定 | 说明 |");
  lines.push("|------|------|--------|----------|--------|------|------|");
  const NO = r.kind.indexOf("有线") >= 0 ? NO_CATV : NO_IPTV;
  for (const it of r.items) {
    const grp = GROUP[it.key] || "其它";
    lines.push(
      `| ${NO[it.key] || "—"} | ${grp} | ${it.name} | ${it.standard.replace(/\|/g, "／")} | ${it.actual.replace(/\|/g, "／")} | ${VERDICT_TXT[it.verdict]} | ${it.note.replace(/\|/g, "／")} |`,
    );
  }
  lines.push("");
  lines.push(
    r.kind.indexOf("有线") >= 0
      ? "> 有线表 1 的 QAM 电平 / MER / BER 与画面异常需专用仪表或逐帧解码，本程序不判定；序号 15 已按 TS PID 层实测丢包与无有效净荷。"
      : "> 表 2 序号 19 / 21 已按 TS PID 层实测（丢包与无有效净荷）；序号 20 / 22 需逐帧解码与参考比对，本程序标「未标注」。",
  );
  return lines.join("\n");
}

/** 供状态栏 / 其它页取用：当前一次合规结论（无结果时 null）。 */
export function complianceSummary(): ComplianceLive | ComplianceResult | null {
  return last;
}
