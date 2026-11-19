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
  type Channel,
  type ComplianceLive,
  type ComplianceResult,
  type NicRow,
} from "./api";
import { setNavLamp, nicLabel } from "./main";
import { renderComplianceDoc } from "./compliance-doc";
import {
  VERDICT_TXT,
  reportRows,
} from "./compliance-meta";
import {
  buildComplianceDocx,
  buildComplianceHtml,
  buildComplianceMarkdown,
} from "./compliance-report";
import { initChannelPicker } from "./channel-picker";
import type { ChannelBoard } from "./channels";
import { registerFlowTarget } from "./transfer";
import {
  buildComplianceBatchDocx,
  buildComplianceBatchHtml,
  buildComplianceBatchMarkdown,
  type ComplianceBatchItem,
} from "./compliance-batch-report";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

let last: ComplianceResult | null = null;
let pollTimer: number | null = null;
let liveMode = false;
let activeRow: string | null = null;
let rowSink: ((url: string, state: "idle" | "run" | "ok" | "warn" | "err", text: string) => void) | null = null;

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
  let curGrp = "";
  let rows = "";
  for (const row of reportRows(r)) {
    const grp = row.group;
    if (grp !== curGrp) {
      curGrp = grp;
      rows += `<div class="ksec">${esc(grp)}</div>`;
    }
    const no = row.no !== "—" ? `<i class="kno">${row.no}</i>` : "";
    rows += `<div class="krow">
        <span class="kname">${no}${esc(row.item.name)}</span>
        <span class="kstd">${esc(row.item.standard)}</span>
        <span class="kact">${esc(row.item.actual)}</span>
        <span class="kvd"><span class="vd vd-${row.item.verdict}">${VERDICT_TXT[row.item.verdict] || row.item.verdict}</span></span>
        <span class="knote">${esc(row.item.note)}</span>
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
  if (activeRow) {
    rowSink?.(activeRow, state === "err" ? "err" : "ok", msg);
    activeRow = null;
  }
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

  let channelBoard: ChannelBoard | null = null;
  let batchRunning = false;
  let batchStop = false;
  let pollSecs = 30;
  let batchItems: ComplianceBatchItem[] = [];

  channelBoard = initChannelPicker({
    page: "compliance",
    hostId: "k-chboard",
    bodyId: "k-body",
    railId: "k-ch-rail",
    inputId: "k-url",
    collapsedKey: "compliance.ch.collapsed",
    detectLabel: "检测选中",
    pollLabel: "轮询检测",
    onDetect: (ch) => {
      if (batchRunning) return;
      if (liveMode) {
        setStatus("收流中，先停止当前检测", "err");
        return;
      }
      url.value = ch.url;
      channelBoard?.selectByUrl(ch.url);
      activeRow = ch.url;
      channelBoard?.updateRowByUrl(ch.url, "run", "检测中…");
      batchItems = [];
      $("k-batch").hidden = true;
      act.click();
    },
    onPoll: (list) => void pollSelected(list),
    stopRun: () => stopBatch(),
  });
  rowSink = (u, state, text) => channelBoard?.updateRowByUrl(u, state, text);
  void loadPollSecs();

  // 合规说明是页内视图：顶栏书本图标或页脚链接打开，说明页里点「返回检测」关掉
  const docView = $("k-doc");
  const docBtn = $("k-kdoc") as HTMLButtonElement;
  const showDoc = (on: boolean) => {
    if (on) renderComplianceDoc(docView);
    docView.hidden = !on;
    $("k-status").hidden = on;
    $("k-wrap").hidden = on;
    const batchHost = $("k-batch");
    if (on) batchHost.hidden = true;
    else if (batchItems.length) batchHost.hidden = false;
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
    if (batchRunning) {
      stopBatch();
      return;
    }
    await api.complianceLiveStop();
    liveEnded("已停止收流", "idle");
  });

  act.addEventListener("click", async () => {
    if (batchRunning) {
      stopBatch();
      return;
    }
    showDoc(false); // 开始检测就回检测视图，别让结果藏在说明后面
    const src = url.value.trim();
    if (!src) {
      setStatus("请先填来源（TS 文件或 udp://@组播地址）", "err");
      return;
    }
    const isLive = /^udp:|^rtp:/i.test(src);
    api.pushSourceHistory("compliance", src);
    batchItems = [];
    $("k-batch").hidden = true;
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
        if (activeRow) {
          rowSink?.(activeRow, r.summary.fail > 0 ? "warn" : "ok", r.summary.fail > 0 ? "有不合格项" : "已完成");
          activeRow = null;
        }
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
    $("k-batch").hidden = true;
    setStatus("已切换依据，请重新开始检测", "idle");
  });

  async function loadPollSecs() {
    try {
      const v = await api.uiGet("compliance.poll_secs");
      if (typeof v === "number" && v >= 5) pollSecs = Math.min(600, Math.round(v));
    } catch {
      /* 默认 30 秒 */
    }
  }

  async function pollSelected(list: Channel[]) {
    if (batchRunning || liveMode || !list.length) return;
    await loadPollSecs();
    showDoc(false);
    batchRunning = true;
    batchStop = false;
    batchItems = [];
    activeRow = null;
    $("k-batch").hidden = true;
    $("k-empty").hidden = true;
    $("k-tbl").hidden = true;
    $("k-foot").hidden = true;
    $("k-sum").innerHTML = "";
    channelBoard?.setBusy(true);
    for (let k = 0; k < list.length; k++) {
      if (batchStop) break;
      const ch = list[k];
      if (!ch) continue;
      url.value = ch.url;
      channelBoard?.selectByUrl(ch.url);
      channelBoard?.updateRowByUrl(ch.url, "run", `检测 ${k + 1}/${list.length}`);
      setStatus(`轮询检测 ${k + 1}/${list.length} · ${ch.name}`, "run");
      try {
        const r = await runBatchChannel(ch);
        if (r) {
          batchItems.push({ name: ch.name, url: ch.url, result: r });
          channelBoard?.updateRowByUrl(ch.url, r.summary.fail > 0 ? "warn" : "ok", r.summary.fail > 0 ? "有不合格项" : "已完成");
        } else {
          batchItems.push({ name: ch.name, url: ch.url, result: null, err: "未取到结果" });
          channelBoard?.updateRowByUrl(ch.url, "err", "无结果");
        }
      } catch (e: any) {
        batchItems.push({ name: ch.name, url: ch.url, result: null, err: String(e).slice(0, 80) });
        channelBoard?.updateRowByUrl(ch.url, "err", String(e).slice(0, 60));
        act.disabled = false;
        stop.hidden = true;
        liveMode = false;
      }
    }
    batchRunning = false;
    batchStop = false;
    liveMode = false;
    channelBoard?.setBusy(false);
    act.disabled = false;
    stop.hidden = true;
    const ok = batchItems.filter((x) => x.result).length;
    renderBatchSummary();
    setStatus(`轮询检测完成：${ok}/${batchItems.length} 路有结果，已汇总为一份结果`, ok ? "idle" : "err");
  }

  async function runBatchChannel(ch: Channel): Promise<ComplianceResult | null> {
    await api.pushSourceHistory("compliance", ch.url);
    act.disabled = true;
    const isLive = /^udp:|^rtp:/i.test(ch.url);
    if (!isLive) {
      const r = await api.complianceCheck(ch.url, kind.value);
      act.disabled = false;
      return r;
    }
    liveMode = true;
    stop.hidden = false;
    let snap: ComplianceLive | null = null;
    try {
      await api.complianceLiveStart(ch.url, nic?.value || "", kind.value);
      const until = Date.now() + pollSecs * 1000;
      while (Date.now() < until && !batchStop) {
        try {
          snap = await api.complianceLiveSnapshot(kind.value);
        } catch {
          /* 单次快照失败继续等 */
        }
        if (snap?.err) break;
        if (snap && !snap.alive) break;
        await new Promise((r) => window.setTimeout(r, 1000));
      }
      try {
        snap = await api.complianceLiveSnapshot(kind.value);
      } catch {
        /* 保留上一帧 */
      }
    } finally {
      await api.complianceLiveStop().catch(() => undefined);
      liveMode = false;
      stop.hidden = true;
      act.disabled = false;
    }
    if (!snap) return null;
    return { mode: snap.mode, source: snap.source, kind: snap.kind, items: snap.items, summary: snap.summary, basis: snap.basis };
  }

  function stopBatch() {
    if (batchRunning) {
      batchStop = true;
      api.complianceLiveStop().catch(() => undefined);
      return;
    }
    if (liveMode) {
      api.complianceLiveStop().catch(() => undefined);
      liveEnded("已停止收流", "idle");
    }
  }

  function renderBatchSummary() {
    const host = $("k-batch");
    if (!batchItems.length) {
      host.hidden = true;
      $("k-empty").hidden = false;
      return;
    }
    $("k-empty").hidden = true;
    $("k-tbl").hidden = true;
    $("k-foot").hidden = true;
    const ok = batchItems.filter((x) => x.result).length;
    const rows = batchItems
      .map((it) => {
        if (!it.result) {
          return `<tr><td title="${esc(it.url)}">${esc(it.name)}</td><td colspan="4" class="bs-err">${esc(it.err || "未取到结果")}</td></tr>`;
        }
        const r = it.result;
        const result = r.summary.fail > 0 ? "存在不合格项" : !r.summary.ok ? "有未标注项" : "全部合格";
        return `<tr>
          <td title="${esc(it.url)}">${esc(it.name)}</td>
          <td>${esc(r.kind)}</td>
          <td>${r.summary.pass}</td>
          <td>${r.summary.fail}</td>
          <td>${r.summary.unknown}</td>
          <td class="${r.summary.fail ? "bs-warn" : "bs-ok"}">${result}</td>
        </tr>`;
      })
      .join("");
    host.innerHTML = `
      <div class="batchsum-h"><b>轮询汇总</b><span>共 ${batchItems.length} 路 · 有结果 ${ok} 路 · 失败 ${batchItems.length - ok} 路</span></div>
      <table class="batchsum-tbl">
        <thead><tr><th>频道</th><th>依据</th><th>合格</th><th>不合格</th><th>未标注</th><th>结论</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>`;
    host.hidden = false;
  }

  // 回车 = 开始（与其它页一致）
  url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") act.click();
  });

  // 报告导出：一个入口，点开选 Markdown / Word / HTML
  const exportBtn = $("k-export") as HTMLButtonElement;
  const exppop = $("k-exppop") as HTMLElement;
  let exporting = false;

  exportBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    exppop.hidden = !exppop.hidden;
  });
  document.addEventListener("click", (e) => {
    if (!(e.target as HTMLElement).closest(".expbox")) exppop.hidden = true;
  });

  async function doExport(fmt: "md" | "docx" | "html") {
    const batch = batchItems.length > 0 ? batchItems : null;
    const r = last;
    if (!batch && !r) {
      setStatus("还没有检测结果可导出", "err");
      return;
    }
    if (exporting) return;
    exporting = true;
    exportBtn.disabled = true;
    const stamp = new Date()
      .toLocaleString("sv-SE", { hour12: false })
      .replace(/[-: ]/g, (m) => (m === " " ? "_" : m))
      .slice(0, 17);
    const base = `${batch ? "合规检测轮询报告" : "合规检测"}_${stamp}`;
    try {
      if (fmt === "md") {
        const text = batch ? buildComplianceBatchMarkdown(batch) : buildComplianceMarkdown(r!);
        const p = await api.reportExport(`${base}.md`, text, "md");
        if (p) setStatus(`Markdown 报告已导出：${p}`, "idle");
      } else if (fmt === "docx") {
        const b64 = batch ? await buildComplianceBatchDocx(batch) : await buildComplianceDocx(r!);
        const p = await api.reportExportB64(`${base}.docx`, b64, "docx");
        if (p) setStatus(`Word 报告已导出：${p}`, "idle");
      } else {
        const html = batch ? buildComplianceBatchHtml(batch) : buildComplianceHtml(r!);
        const p = await api.reportExport(`${base}.html`, html, "html");
        if (p) setStatus(`HTML 报告已导出：${p}（可直接用浏览器打开/发邮件）`, "idle");
      }
    } catch (e) {
      const text = String(e);
      if (!text.includes("已取消")) setStatus(`导出失败：${text}`, "err");
    } finally {
      exporting = false;
      exportBtn.disabled = false;
    }
  }

  exppop.addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest("[data-fmt]") as HTMLButtonElement | null;
    if (!b) return;
    exppop.hidden = true;
    void doExport((b.dataset.fmt || "md") as "md" | "docx" | "html");
  });

  // 从别的页切回来时把收流状态补上
  window.addEventListener("page-changed", () => {
    if (liveMode && pollTimer == null) {
      pollTimer = window.setInterval(tick, 1000);
    }
  });

  registerFlowTarget(
    "compliance",
    async (src) => {
      const u = src.trim();
      if (!u) return { state: "failed", message: "来源为空" };
      const input = $("k-url") as HTMLInputElement;
      input.value = u;
      if (batchRunning || liveMode || act.disabled) {
        return { state: "filled", message: "合规检测正在工作，地址已填入" };
      }
      channelBoard?.selectByUrl(u);
      act.click();
      return { state: "started", message: "已提交检测" };
    },
    () => batchRunning || liveMode || act.disabled,
  );
}

/** 供状态栏 / 其它页取用：当前一次合规结论（无结果时 null）。 */
export function complianceSummary(): ComplianceLive | ComplianceResult | null {
  return last;
}
