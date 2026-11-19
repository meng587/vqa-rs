//! 视频分析页 —— 扫描/实时轮询 + 7 页签渲染。
//! 数据源：analysis_progress（200ms）/ analysis_result（1s 实时）/ packet_rows。

import { api, type Channel, type NicRow, type Res } from "./api";
import { icon } from "./icons";
import { setNavLamp, nicLabel } from "./main";
import { buildDocxReport } from "./report-docx";
import { buildMarkdownReport } from "./report-markdown";
import { initChannelPicker } from "./channel-picker";
import type { ChannelBoard } from "./channels";
import { registerFlowTarget } from "./transfer";
import {
  buildAnalysisBatchDocx,
  buildAnalysisBatchHtml,
  buildAnalysisBatchMarkdown,
  type AnalysisBatchItem,
} from "./analysis-batch-report";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

function isLiveSource(u: string): boolean {
  return /^(udp|rtp|rtsp|rtmp|srt|http|https):/i.test(u.trim());
}

const TAB_LABELS: Record<string, string> = {
  overview: "概览",
  structure: "流结构",
  errors: "错误与抖动",
  bitrate: "码率",
  packets: "TS包视图",
  tr290: "TR101290",
  logs: "日志",
};

// TR290 固定行目录（级别/显示名/key/影响说明）——与 egui 版同一份口径（GY/T 299.1）
const TR290_ROWS: [number, string, string, string][] = [
  [1, "TS同步丢失", "TS_sync_loss", "完全黑屏，无任何画面输出"],
  [1, "PAT错误", "PAT_error", "无法搜索节目，找不到任何频道"],
  [1, "TS传输错误", "Transport_error", "对应位置数据损坏，大面积花屏"],
  [1, "连续性计数错误", "Continuity_count_error", "丢包导致局部马赛克、卡顿"],
  [2, "CRC校验错误", "CRC_error", "对应表无法解析，信息丢失"],
  [2, "PCR间隔错误", "PCR_error", "STC时钟异常，音画不同步"],
  [2, "PCR不连续", "PCR_discontinuity_error", "PCR跳变导致画面卡顿、马赛克"],
  [2, "PCR精度错误", "PCR_accuracy_error", "PCR时间戳精度超限，STC时钟受损"],
  [2, "CAT错误", "CAT_error", "加密频道CA信息无法识别"],
  [2, "PID错误", "PID_error", "未声明PID出现，流结构异常"],
  [3, "NIT错误", "NIT_id_error", "网络信息、频率参数异常"],
  [3, "SDT错误", "SDT_id_error", "频道名称、提供商信息异常"],
  [3, "EIT错误", "EIT_id_error", "EPG节目单、节目预告异常"],
  [3, "SI重复率错误", "SI_repetition_error", "SI表重复间隔不符合规范"],
  [3, "SDT当前间隔错误", "SDT_act_interval_error", "SDT当前业务表超时"],
  [3, "EIT当前间隔错误", "EIT_act_interval_error", "EIT当前事件表超时"],
  [3, "TDT错误", "TDT_error", "时间日期表异常"],
];

let page: "player" | "analysis" | "about" = "player";
let curTab = "overview";
let lastMode = 0;
let hasResult = false;
let pktPage = 0;
const PKT_PAGE_SIZE = 100;
let chart: any = null;
let resCache: Res | null = null;

export function initAnalysis() {
  const url = $("a-url") as HTMLInputElement;
  const nic = $("a-nic") as HTMLSelectElement | null;
  const act = $("a-act") as HTMLButtonElement;
  const histBtn = $("a-hist") as HTMLButtonElement;
  const histPop = $("a-histpop") as HTMLElement;

  let channelBoard: ChannelBoard | null = null;
  let batchRunning = false;
  let batchStop = false;
  let pollSecs = 30;
  let batchItems: AnalysisBatchItem[] = [];
  let activeScanUrl: string | null = null;

  channelBoard = initChannelPicker({
    page: "analysis",
    hostId: "a-chboard",
    bodyId: "a-body",
    railId: "a-ch-rail",
    inputId: "a-url",
    collapsedKey: "analysis.ch.collapsed",
    detectLabel: "扫描选中",
    pollLabel: "轮询扫描",
    onDetect: (ch) => {
      if (batchRunning) return;
      if (running) {
        showStatus("正在扫描，先停止当前任务", "err");
        return;
      }
      url.value = ch.url;
      channelBoard?.selectByUrl(ch.url);
      activeScanUrl = ch.url;
      channelBoard?.updateRowByUrl(ch.url, "run", "扫描中…");
      batchItems = [];
      $("a-batch").hidden = true;
      void go();
    },
    onPoll: (list) => void pollSelected(list),
    stopRun: () => stopBatch(),
  });
  void loadPollSecs();

  // 网卡下拉：值 = IP，显示「IP · 网卡名」；位置在「视频分析设置」抽屉里
  api.listNics().then(async (list: NicRow[]) => {
    if (!nic) return;
    nic.innerHTML = [
      `<option value="">自动</option>`,
      ...list.map((n) => `<option value="${esc(n.ip)}" title="${esc(n.desc || n.name)}">${esc(nicLabel(n))}</option>`),
    ].join("");
    try {
      const saved = await api.uiGet("analysis.nic");
      if (typeof saved === "string" && list.some((n) => n.ip === saved)) nic.value = saved;
    } catch {
      /* 没保存过就保持自动 */
    }
  });

  histBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (!histPop.hidden) {
      histPop.hidden = true;
      return;
    }
    api.sourceHistory("analysis").then((h) => {
      histPop.innerHTML =
        [`<button class="hi clear">清除（输入新地址）</button>`,
         ...h.map((x) => `<button class="hi" title="${esc(x)}">${esc(x)}</button>`)].join("");
      histPop.hidden = false;
    });
  });
  histPop.addEventListener("click", (e) => {
    const t = (e.target as HTMLElement).closest("button.hi") as HTMLButtonElement | null;
    if (!t) return;
    e.stopPropagation();
    if (t.classList.contains("clear")) url.value = "";
    else url.value = t.textContent || "";
    histPop.hidden = true;
  });

  $("a-browse").addEventListener("click", async () => {
    const p = await api.browse();
    if (p) url.value = p;
  });

  let running = false;
  act.addEventListener("click", () => go());
  url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") go();
  });
  async function go() {
    if (batchRunning) {
      stopBatch();
      return;
    }
    if (running) {
      await api.analysisStop();
      running = false;
      setActUi(false);
      return;
    }
    const u = url.value.trim();
    if (!u) return;
    if (batchItems.length) {
      batchItems = [];
      $("a-batch").hidden = true;
    }
    $("a-panels").hidden = false;
    $("a-panels").style.display = "flex";
    activeScanUrl = u;
    channelBoard?.selectByUrl(u);
    channelBoard?.updateRowByUrl(u, "run", "扫描中…");
    await api.pushSourceHistory("analysis", u);
    try {
      await api.analysisStart(u, nic?.value || "");
      running = true;
      setActUi(true);
    } catch (e: any) {
      activeScanUrl = null;
      channelBoard?.updateRowByUrl(u, "err", String(e).slice(0, 60));
      showStatus(String(e), "err");
    }
  }
  function setActUi(on: boolean) {
    act.classList.toggle("stop", on);
    act.innerHTML = on ? `<span>${icon("square", 13)}</span>停止` : `<span>${icon("search", 13)}</span>开始扫描`;
  }
  setActUi(false);

  async function loadPollSecs() {
    try {
      const v = await api.uiGet("analysis.poll_secs");
      if (typeof v === "number" && v >= 5) pollSecs = Math.min(600, Math.round(v));
    } catch {
      /* 默认 30 秒 */
    }
  }

  async function pollSelected(list: Channel[]) {
    if (batchRunning || running || !list.length) return;
    await loadPollSecs();
    batchRunning = true;
    batchStop = false;
    batchItems = [];
    $("a-batch").hidden = true;
    $("a-panels").hidden = true;
    $("a-panels").style.display = "none";
    $("a-stats").hidden = true;
    $("a-tabs").hidden = true;
    channelBoard?.setBusy(true);
    running = true;
    setActUi(true);
    for (let k = 0; k < list.length; k++) {
      if (batchStop) break;
      const ch = list[k];
      if (!ch) continue;
      url.value = ch.url;
      channelBoard?.selectByUrl(ch.url);
      channelBoard?.updateRowByUrl(ch.url, "run", `扫描 ${k + 1}/${list.length}`);
      try {
        const res = await runBatchChannel(ch);
        if (res) {
          batchItems.push({ name: ch.name, url: ch.url, res });
          channelBoard?.updateRowByUrl(ch.url, "ok", "已完成");
        } else {
          batchItems.push({ name: ch.name, url: ch.url, res: null, err: "未取到结果" });
          channelBoard?.updateRowByUrl(ch.url, "err", "无结果");
        }
      } catch (e: any) {
        batchItems.push({ name: ch.name, url: ch.url, res: null, err: String(e).slice(0, 80) });
        channelBoard?.updateRowByUrl(ch.url, "err", String(e).slice(0, 60));
      }
    }
    batchRunning = false;
    batchStop = false;
    running = false;
    setActUi(false);
    channelBoard?.setBusy(false);
    renderBatchSummary();
    const ok = batchItems.filter((x) => x.res).length;
    showStatus(`轮询扫描完成：${ok}/${batchItems.length} 路有结果`, ok ? "" : "err");
    pushLog(`轮询扫描完成：${ok}/${batchItems.length} 路有结果，已汇总为一份结果`);
  }

  async function runBatchChannel(ch: Channel): Promise<Res | null> {
    await api.pushSourceHistory("analysis", ch.url);
    await api.analysisStart(ch.url, nic?.value || "");
    running = true;
    setActUi(true);
    const live = isLiveSource(ch.url);
    if (live) {
      const until = Date.now() + pollSecs * 1000;
      while (Date.now() < until && !batchStop) {
        await new Promise((r) => window.setTimeout(r, 250));
      }
      if (!batchStop) await api.analysisStop().catch(() => undefined);
    }
    await waitScanIdle();
    const res = (await api.analysisResult()) as Res | null;
    return res;
  }

  async function waitScanIdle() {
    await new Promise((r) => window.setTimeout(r, 300));
    for (;;) {
      if (batchStop) return;
      try {
        const p = await api.analysisProgress();
        if (p.mode === 0) return;
      } catch {
        return;
      }
      await new Promise((r) => window.setTimeout(r, 250));
    }
  }

  function stopBatch() {
    if (batchRunning) {
      batchStop = true;
      api.analysisStop().catch(() => undefined);
      return;
    }
    if (running) {
      running = false;
      activeScanUrl = null;
      setActUi(false);
      api.analysisStop().catch(() => undefined);
    }
  }

  function renderBatchSummary() {
    const host = $("a-batch");
    if (!batchItems.length) {
      host.hidden = true;
      $("a-panels").hidden = false;
      $("a-panels").style.display = "flex";
      return;
    }
    $("a-panels").hidden = true;
    $("a-panels").style.display = "none";
    $("a-stats").hidden = true;
    $("a-tabs").hidden = true;
    const ok = batchItems.filter((x) => x.res).length;
    const rows = batchItems
      .map((it) => {
        if (!it.res) {
          return `<tr><td colspan="2" title="${esc(it.url)}">${esc(it.name)}</td><td colspan="4" class="bs-err">${esc(it.err || "失败")}</td><td>失败</td></tr>`;
        }
        const r = it.res;
        const bad = (r.tr290 || []).filter((t) => t.level >= 2).length;
        return `<tr>
          <td title="${esc(it.url)}">${esc(it.name)}</td>
          <td>${((r.bitrate_avg || 0) / 1e6).toFixed(2)} Mbps</td>
          <td>${r.sync_errors ?? "—"}</td>
          <td>${bad}</td>
          <td>${(r.programs || []).length} / ${(r.pids || []).length}</td>
          <td class="${bad ? "bs-warn" : "bs-ok"}">${bad ? "有异常" : "正常"}</td>
        </tr>`;
      })
      .join("");
    host.innerHTML = `
      <div class="batchsum-h"><b>轮询汇总</b><span>共 ${batchItems.length} 路 · 有结果 ${ok} 路 · 失败 ${batchItems.length - ok} 路</span></div>
      <table class="batchsum-tbl">
        <thead><tr><th>频道</th><th>平均码率</th><th>同步错误</th><th>TR290 L2+</th><th>节目 / PID</th><th>结论</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>`;
    host.hidden = false;
  }

  // 页签
  const tabs = document.querySelectorAll<HTMLElement>("#a-tabs .tab");
  tabs.forEach((t) =>
    t.addEventListener("click", () => {
      tabs.forEach((x) => x.classList.toggle("is-on", x === t));
      curTab = t.getAttribute("data-tab")!;
      document.querySelectorAll<HTMLElement>("#a-panels [data-panel]").forEach((p) => {
        p.hidden = p.getAttribute("data-panel") !== curTab;
      });
      if (curTab === "bitrate") renderBitrate(resCache);
      if (curTab === "packets") renderPackets();
    }),
  );

  // 报告导出：一个入口，点开选 Markdown / Word / HTML
  const exportBtn = $("a-export") as HTMLButtonElement;
  const exppop = $("a-exppop") as HTMLElement;
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
    if (!batch && !resCache) {
      pushLog("还没有扫描结果，先扫描再导出");
      return;
    }
    if (exporting) return;
    exporting = true;
    exportBtn.disabled = true;
    const stamp = new Date()
      .toLocaleString("sv-SE", { hour12: false })
      .replace(/[-: ]/g, (m) => (m === " " ? "_" : m))
      .slice(0, 17);
    const base = `${batch ? "TS流质量检测轮询报告" : "TS流质量检测报告"}_${stamp}`;
    try {
      if (fmt === "md") {
        const text = batch ? buildAnalysisBatchMarkdown(batch) : buildMarkdownReport(resCache!);
        const p = await api.reportExport(`${base}.md`, text, "md");
        if (p) pushLog(`Markdown 报告已导出：${p}`);
      } else if (fmt === "docx") {
        const b64 = batch ? await buildAnalysisBatchDocx(batch) : await buildDocxReport(resCache!);
        const p = await api.reportExportB64(`${base}.docx`, b64, "docx");
        if (p) pushLog(`Word 报告已导出：${p}`);
      } else {
        const html = batch ? buildAnalysisBatchHtml(batch) : buildHtmlReport(resCache!);
        const p = await api.reportExport(`${base}.html`, html, "html");
        if (p) pushLog(`HTML 报告已导出：${p}（可直接用浏览器打开/发邮件）`);
      }
    } catch (err) {
      const text = String(err);
      if (!text.includes("已取消")) pushLog(`导出失败：${text}`);
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

  document.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).closest("#a-pkt-prev")) {
      if (pktPage > 0) {
        pktPage--;
        renderPackets();
      }
    }
    if ((e.target as HTMLElement).closest("#a-pkt-next")) {
      pktPage++;
      renderPackets();
    }
  });

  // 轮询：progress 200ms；实时结果 1s
  setInterval(async () => {
    try {
      const p = await api.analysisProgress();
      // 收割完成态；批量轮询由 pollSelected 自己收结果
      if (!batchRunning && !batchItems.length && lastMode !== 0 && p.mode === 0) {
        running = false;
        setActUi(false);
        if (!p.err) hasResult = true;
        if (activeScanUrl) {
          channelBoard?.updateRowByUrl(activeScanUrl, p.err ? "err" : "ok", p.err || "已完成");
          activeScanUrl = null;
        }
        refreshResult();
      }
      lastMode = p.mode;
      renderStatus(p);
      setNavLamp(
        "analysis",
        p.err && p.mode === 0 ? "error" : p.mode !== 0 ? "running" : "idle",
      );
    } catch {
      /* 静默 */
    }
  }, 200);
  setInterval(async () => {
    if (!batchRunning && !batchItems.length && lastMode === 2) refreshResult();
  }, 1000);
  setInterval(async () => {
    if (lastMode !== 0) pushLogList(await api.analysisLog());
  }, 600);

  window.addEventListener("page-changed", (e) => {
    page = (e as CustomEvent).detail;
    if (page === "analysis" && hasResult && !batchItems.length) refreshResult();
  });

  registerFlowTarget(
    "analysis",
    async (src) => {
      const u = src.trim();
      if (!u) return { state: "failed", message: "来源为空" };
      const input = $("a-url") as HTMLInputElement;
      input.value = u;
      if (batchRunning || running) {
        return { state: "filled", message: "视频分析正在运行，地址已填入" };
      }
      await go();
      return running
        ? { state: "started", message: "已开始扫描" }
        : { state: "failed", message: "启动失败，请查看视频分析状态栏" };
    },
    () => batchRunning || running,
  );
}

// ═══════════════════════════ 状态行/日志 ═══════════════════════════

function showStatus(text: string, cls: "" | "err") {
  const dot = $("a-dot");
  const txt = $("a-status-text");
  dot.className = cls === "err" ? "dot err" : "dot";
  txt.textContent = text;
  $("a-status").className = `statusline${cls === "err" ? " err" : ""}`;
}

function renderStatus(p: any) {
  if (p.err && p.mode === 0) {
    showStatus(`✗ ${p.err}`, "err");
    return;
  }
  if (p.mode === 1) {
    const pct = p.total > 0 ? Math.min(100, Math.round((p.done * 100) / p.total)) : 0;
    showStatus(`扫描中 ${pct}%`, "");
    ($("a-dot") as HTMLElement).classList.add("run");
    $("a-status").className = "statusline";
  } else if (p.mode === 2) {
    showStatus(`实时分析中 · 已收 ${(p.live_bytes / 1048576).toFixed(1)} MB`, "");
  } else if (hasResult) {
    // 保持"扫描完成"文案由 refreshResult 写入
  } else {
    showStatus("待机", "");
  }
}

const logLines: string[] = [];
function pushLog(s: string) {
  logLines.push(s);
  renderLog();
}
function pushLogList(list: string[]) {
  if (list.length === logLines.length) return;
  logLines.length = 0;
  logLines.push(...list);
  renderLog();
}
function renderLog() {
  const panel = document.querySelector<HTMLElement>('[data-panel="logs"]');
  if (!panel) return;
  let el = panel.querySelector<HTMLElement>(".logwrap");
  // 首次写日志时面板还是空的：补一个容器，否则日志永远看不见
  if (!el) {
    el = document.createElement("div");
    el.className = "logwrap";
    panel.appendChild(el);
  }
  el.scrollTop = el.scrollHeight;
  el.innerHTML = logLines.map(esc).join("<br>");
}

// ═══════════════════════════ 结果渲染 ═══════════════════════════

async function refreshResult() {
  const res = await api.analysisResult();
  if (!res) return;
  resCache = res;
  if (!hasResult && lastMode !== 2) return;
  renderAll(res);
}

function renderAll(res: Res) {
  $("a-panels").hidden = false;
  $("a-panels").style.display = "flex";
  $("a-stats").hidden = false;
  $("a-tabs").hidden = false;
  renderStats(res);
  renderOverview(res);
  renderStructure(res);
  renderErrors(res);
  renderTr290(res);
  if (curTab === "bitrate") renderBitrate(res);
  if (curTab === "packets") renderPackets();
  if (res.packets > 0) {
    showStatus(
      `扫描完成 · ${res.packets.toLocaleString()} 包 · ${res.duration_pcr.toFixed(3)} s`,
      "",
    );
  }
}

function renderStats(res: Res) {
  const lv = (n: number) => res.tr290.filter((t) => t.level === n);
  const sum = (arr: { count: number }[]) => arr.reduce((a, b) => a + b.count, 0);
  const displayOf = new Map(TR290_ROWS.map(([, d, k]) => [k, d]));
  const first = (arr: { count: number; name: string }[]) => {
    const t = arr.find((t) => t.count > 0);
    return t ? displayOf.get(t.name) ?? t.name : "无";
  };
  const l1 = lv(1), l2 = lv(2), l3 = lv(3);
  const cards = [
    scard("总码率", (res.bitrate_avg / 1e6).toFixed(2), "Mbps", "PCR 区间口径"),
    scard(
      "同步状态",
      res.sync_errors === 0 ? "正常" : "异常",
      "",
      `${res.sync_errors} 字节失步`,
      res.sync_errors === 0 ? "var(--ok)" : "var(--bad)",
      res.sync_errors > 0,
    ),
    scard("一级错误", String(sum(l1)), "", first(l1), sum(l1) > 0 ? "var(--bad)" : "", sum(l1) > 0),
    scard("二级错误", String(sum(l2)), "", first(l2), sum(l2) > 0 ? "var(--warn)" : "", sum(l2) > 0),
    scard("三级错误", String(sum(l3)), "", first(l3)),
    scard("TS 包长", String(res.packet_size), "bytes", `${res.packets.toLocaleString()} 包`),
  ];
  ($("a-stats") as HTMLElement).innerHTML = cards.join("");
}

function renderOverview(res: Res) {
  const ov = document.querySelector<HTMLElement>('[data-panel="overview"]')!;
  ov.hidden = curTab !== "overview";
  ov.style.cssText = "flex:1;min-height:0;display:flex;flex-direction:column;gap:10px;overflow:auto;";
  const prog = res.programs
    .map(
      (p) => `
      <div class="prow"><span class="pillg">节目 ${p.number}</span><span class="hex" style="color:var(--mut)">PMT 0x${p.pmt_pid.toString(16)} · PCR 0x${p.pcr_pid.toString(16)}</span></div>
      ${p.streams
        .map(
          (s) =>
            `<div class="prow"><span class="hex">0x${s.pid.toString(16).padStart(4, "0")}</span> ${esc(s.type_name)} <em>${esc(s.kind)}</em></div>`,
        )
        .join("")}`,
    )
    .join("");

  // PCR 摘要（从 pcr_samples 统计）
  const samples = res.pcr_samples;
  let pcrPanel = `<div class="panel"><h4>PCR 抖动</h4><div class="krow"><span class="k">PCR PID</span><b>${res.pcr_pid != null ? "0x" + res.pcr_pid.toString(16) : "—"}</b></div></div>`;
  if (samples.length > 0) {
    const ivs = samples.map((s) => s[2]).filter((x) => x > 0);
    const ojs = samples.map((s) => Math.abs(s[3]));
    const avg = ivs.reduce((a, b) => a + b, 0) / Math.max(1, ivs.length);
    const maxOj = Math.max(0, ...ojs);
    pcrPanel = `<div class="panel"><h4>PCR 抖动</h4>
      <div class="krow"><span class="k">平均间隔</span><b>${avg.toFixed(2)} ms</b></div>
      <div class="krow"><span class="k">最大 |OJ|</span><b class="${maxOj > 1 ? "warn" : ""}">${maxOj.toFixed(3)} ms</b></div>
      <div class="krow"><span class="k">CBR 判定</span><b>${res.pcr_cbr ? "CBR" : "VBR"}</b></div>
    </div>`;
  }
  ov.innerHTML = `<div class="duo">
    <div class="panel"><h4>节目结构</h4>${prog}</div>
    ${pcrPanel}
  </div>
  <div class="panel" style="flex:1"><h4>TR 101 290 摘要</h4>${tr290Table(res, true)}</div>`;
}

function renderStructure(res: Res) {
  const el = document.querySelector<HTMLElement>('[data-panel="structure"]')!;
  el.hidden = curTab !== "structure";
  const kv = (k: string, v: string) => `<span class="k">${k}</span><span class="v">${esc(v)}</span>`;
  const container = `<div class="panel"><h4>容器信息</h4><div class="kvgrid">
    ${kv("文件", res.path)}
    ${kv("大小", `${(res.file_size / 1048576).toFixed(2)} MB`)}
    ${kv("包数", res.packets.toLocaleString())}
    ${kv("包长", `${res.packet_size} bytes`)}
    ${kv("同步错误", `${res.sync_errors} 字节`)}
    ${kv("PCR 时长", `${res.duration_pcr.toFixed(3)} s`)}
    ${kv("PCR PID", res.pcr_pid != null ? `0x${res.pcr_pid.toString(16)}` : "—")}
    ${kv("加密", res.encrypted ? "是" : "否")}
    ${kv("码率点", `${res.rate.length.toLocaleString()} 个`)}
    ${kv("SI 表", Object.keys(res.si).join("、") || "—")}
  </div></div>`;
  const prog = `<div class="panel"><h4>节目与流</h4>${res.programs
    .map(
      (p) => `
      <div class="prow"><span class="pillg">节目 ${p.number}</span><span class="hex" style="color:var(--mut)">PMT 0x${p.pmt_pid.toString(16)} · PCR 0x${p.pcr_pid.toString(16)}</span></div>
      ${p.streams
        .map(
          (s) =>
            `<div class="prow"><span class="hex">0x${s.pid.toString(16).padStart(4, "0")}</span> ${esc(s.type_name)} <em>${esc(s.kind)}</em></div>`,
        )
        .join("")}`,
    )
    .join("")}</div>`;
  const pids = `<div class="panel"><h4>PID 统计（前 ${Math.min(64, res.pids.length)}）</h4>
    <table class="dt"><thead><tr><th>PID</th><th>类型</th><th style="text-align:right">包数</th><th style="text-align:right">字节</th><th style="text-align:right">载荷</th><th style="text-align:right">加扰</th></tr></thead>
    <tbody>${res.pids
      .map(
        (p) =>
          `<tr><td class="mono">0x${p.pid.toString(16).padStart(4, "0")}</td><td>${esc(p.kind)}</td><td class="r">${p.packets.toLocaleString()}</td><td class="r">${p.bytes.toLocaleString()}</td><td class="r">${p.payload_bytes.toLocaleString()}</td><td class="r">${p.scrambled > 0 ? p.scrambled.toLocaleString() : "—"}</td></tr>`,
      )
      .join("")}</tbody></table></div>`;
  const si = Object.entries(res.si)
    .map(([name, v]) => {
      if (typeof v === "string") {
        return `<div class="panel"><h4>SI 表 · ${esc(name)}</h4><div class="si-text">${esc(v)}</div></div>`;
      }
      const rows = (v as any[])
        .map(
          (item: any) =>
            `<div class="prow"><span class="hex">${esc(String(item.key ?? ""))}</span> ${esc(String(item.value ?? ""))}</div>`,
        )
        .join("");
      return `<div class="panel"><h4>SI 表 · ${esc(name)}${res.si_truncated ? ' <span class="pillw">截断</span>' : ""}</h4>${rows || '<span style="color:var(--faint);font-size:12px">（空）</span>'}</div>`;
    })
    .join("");
  const gop =
    res.gop.length > 0
      ? `<div class="panel"><h4>GOP（${res.gop_note || "全部"}，前 ${res.gop.length}）</h4>
      <table class="dt"><thead><tr><th style="text-align:right">起始(s)</th><th style="text-align:right">时长(s)</th><th style="text-align:right">帧数</th></tr></thead>
      <tbody>${res.gop
        .map(
          (g) =>
            `<tr><td class="r">${g[0].toFixed(3)}</td><td class="r">${g[1].toFixed(3)}</td><td class="r">${g[2]}</td></tr>`,
        )
        .join("")}</tbody></table></div>`
      : "";
  el.innerHTML = [container, prog, pids, si, gop].filter(Boolean).join("");
}

function renderErrors(res: Res) {
  const el = document.querySelector<HTMLElement>('[data-panel="errors"]')!;
  el.hidden = curTab !== "errors";
  const samples = res.pcr_samples;
  let cards = "";
  let detail = `<div class="empty" style="flex:1">无 PCR 采样（流中没有 PCR 或未锁定）</div>`;
  if (samples.length > 0) {
    const ivs = samples.map((s) => s[2]).filter((x) => x > 0);
    const maxIv = Math.max(0, ...ivs);
    const avgIv = ivs.reduce((a, b) => a + b, 0) / Math.max(1, ivs.length);
    const maxOj = Math.max(0, ...samples.map((s) => Math.abs(s[3])));
    const maxAc = Math.max(0, ...samples.map((s) => Math.abs(s[4])));
    cards = `<div class="sgrid" style="grid-template-columns:repeat(4,1fr)">
      ${scard("平均间隔", avgIv.toFixed(2), "ms", "PCR 到达间隔")}
      ${scard("最大间隔", maxIv.toFixed(2), "ms", "阈值 40 ms", maxIv > 40 ? "var(--bad)" : "", maxIv > 40)}
      ${scard("最大抖动 |OJ|", maxOj.toFixed(3), "ms", "阈值 1 ms", maxOj > 1 ? "var(--warn)" : "", maxOj > 1)}
      ${scard("最大 |AC|", maxAc.toFixed(3), "ms", "到达时钟抖动")}
    </div>`;
    detail = `<div class="panel tblwrap" style="flex:1"><table class="dt">
      <thead><tr><th style="text-align:right">到达</th><th style="text-align:right">PCR (s)</th><th style="text-align:right">间隔 (ms)</th><th style="text-align:right">OJ (ms)</th><th style="text-align:right">AC (ms)</th></tr></thead>
      <tbody>${samples
        .map(
          (s) =>
            `<tr><td class="r">${s[0].toFixed(3)}</td><td class="r">${s[1].toFixed(6)}</td><td class="r">${s[2].toFixed(3)}</td><td class="r" style="color:${Math.abs(s[3]) > 1 ? "var(--warn)" : "var(--mut)"}">${s[3].toFixed(3)}</td><td class="r">${s[4].toFixed(3)}</td></tr>`,
        )
        .join("")}</tbody></table></div>`;
  }
  el.innerHTML = cards + detail;
}

let uplotCss = false;
let chartRO: ResizeObserver | null = null;
function renderBitrate(res: Res | null) {
  const el = document.querySelector<HTMLElement>('[data-panel="bitrate"]')!;
  el.hidden = curTab !== "bitrate";
  if (!res) return;
  const pts = res.rate;
  if (pts.length === 0) {
    el.innerHTML = `<div class="empty" style="flex:1">无码率区间（流中没有 PCR）</div>`;
    return;
  }
  const rates = pts.map((r) => r[2]);
  const avg = rates.reduce((a, b) => a + b, 0) / rates.length;
  const min = Math.min(...rates);
  const max = Math.max(...rates);
  const cards = `<div class="sgrid" style="grid-template-columns:repeat(5,1fr)">
    ${scard("当前码率", (rates[rates.length - 1] / 1e6).toFixed(3), "Mbps", "最近区间")}
    ${scard("平均码率", (avg / 1e6).toFixed(3), "Mbps", "全程")}
    ${scard("最小", (min / 1e6).toFixed(3), "Mbps", "")}
    ${scard("最大", (max / 1e6).toFixed(3), "Mbps", "峰值", max / avg > 2 ? "var(--warn)" : "", max / avg > 2)}
    ${scard("峰均比", (max / avg).toFixed(2), "x", "max/avg")}
  </div>`;
  el.innerHTML =
    cards +
    `<div class="panel chartpanel" style="flex:1;min-height:0;display:flex;flex-direction:column;padding:10px 12px">
       <div class="charthead"><span>码率曲线</span><span class="chartunit">纵轴 Mbps · 横轴 秒</span></div>
       <div class="chartbox" id="rate-chart"></div>
     </div>`;

  import("uplot").then(async (m) => {
    const uPlot = m.default;
    if (!uplotCss) {
      // @ts-ignore CSS 副作用导入
      await import("uplot/dist/uPlot.min.css");
      uplotCss = true;
    }
    if (chartRO) {
      chartRO.disconnect();
      chartRO = null;
    }
    if (chart) {
      chart.destroy();
      chart = null;
    }
    // 超长流：横轴用 mm:ss
    const span = Math.max(pts[pts.length - 1][1] - pts[0][0], 0);
    const clock = span >= 120;
    const fmtX = (t: number) => {
      const s = Math.max(0, Math.round(t));
      if (!clock) return String(s);
      return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
    };
    // 刻度小数位按量级统一（避免 36.7 与 110 混排）
    const yDecimals = max / 1e6 >= 100 ? 0 : max / 1e6 >= 20 ? 1 : 2;
    const fmtY = (v: number) => (v / 1e6).toFixed(yDecimals);

    const build = () => {
      const host = document.getElementById("rate-chart");
      if (!host) return;
      const w = Math.max(300, host.clientWidth);
      const h = Math.max(240, host.clientHeight);
      chart = new uPlot(
        {
          width: w,
          height: h,
          cursor: { show: false },
          legend: { show: false },
          padding: [8, 8, 0, 0],
          axes: [
            {
              stroke: "#4E4536",
              font: "11px system-ui, sans-serif",
              grid: { stroke: "rgba(203,189,164,.48)", width: 1 },
              ticks: { stroke: "#CBBDA4", size: 4 },
              size: 48,
              values: (_u, ticks) => ticks.map((t: number) => fmtX(t)),
            },
            {
              stroke: "#4E4536",
              font: "11px system-ui, sans-serif",
              grid: { stroke: "rgba(203,189,164,.48)", width: 1 },
              ticks: { stroke: "#CBBDA4", size: 4 },
              size: 58,
              // 固定 6 条刻度线（0 起、均分），高度变化时刻度数量不再塌缩
              splits: (_u: any, _ai: number, lo: number, hi: number) => {
                const n = 6;
                const out: number[] = [];
                for (let i = 0; i <= n; i++) out.push(lo + ((hi - lo) * i) / n);
                return hi > lo ? out : [lo];
              },
              values: (_u, ticks) => ticks.map((t: number) => fmtY(t)),
            },
          ],
          series: [
            {},
            {
              stroke: "#355F4D",
              width: 1.4,
              points: { show: false },
              fill: "rgba(53,95,77,.12)",
            },
          ],
        },
        [pts.map((r) => r[0]), rates],
        host,
      );
      chartRO = new ResizeObserver(() => {
        const box = document.getElementById("rate-chart");
        if (!box || !chart) return;
        chart.setSize({
          width: Math.max(300, box.clientWidth),
          height: Math.max(240, box.clientHeight),
        });
      });
      chartRO.observe(host);
    };
    // 等布局落定（innerHTML 刚换完时 clientHeight 只是 min-height）
    requestAnimationFrame(() => requestAnimationFrame(build));
  });
}

async function renderPackets() {
  const el = document.querySelector<HTMLElement>('[data-panel="packets"]')!;
  el.hidden = curTab !== "packets";
  const data = await api.packetRows(pktPage * PKT_PAGE_SIZE, PKT_PAGE_SIZE);
  if (data.total === 0) {
    el.innerHTML = `<div class="empty" style="flex:1">没有包数据（仅文件扫描可用）</div>`;
    return;
  }
  const pages = Math.ceil(data.total / PKT_PAGE_SIZE);
  el.innerHTML = `
    <div class="pager">
      <button id="a-pkt-prev" ${pktPage === 0 ? "disabled" : ""}>◀ 上一页</button>
      <span>第 ${pktPage + 1}/${pages} 页（共 ${data.total.toLocaleString()} 包）</span>
      <button id="a-pkt-next" ${pktPage + 1 >= pages ? "disabled" : ""}>下一页 ▶</button>
    </div>
    <div class="panel tblwrap" style="flex:1"><table class="dt">
      <thead><tr><th style="text-align:right">#</th><th>PID</th><th>PUSI</th><th>AFC</th><th>CC</th><th>前 16 字节</th></tr></thead>
      <tbody>${data.rows
        .map(
          (r) =>
            `<tr><td class="r">${r.idx}</td><td class="mono">0x${r.pid.toString(16).padStart(4, "0")}</td><td>${r.pusi ? "●" : ""}</td><td class="mono">${r.afc}</td><td class="mono">${r.cc}</td><td class="mono" style="font-size:11px;color:var(--mut)">${esc(r.hex)}</td></tr>`,
        )
        .join("")}</tbody></table></div>`;
}

function renderTr290(res: Res, compact = false) {
  if (!compact) {
    const el = document.querySelector<HTMLElement>('[data-panel="tr290"]')!;
    el.hidden = curTab !== "tr290";
    el.innerHTML = `<table class="dt"><thead><tr>
      <th style="width:60px">级别</th><th>检测项</th><th style="width:90px">状态</th>
      <th style="width:70px;text-align:right">次数</th><th>最近发生 / 影响</th>
    </tr></thead><tbody>${tr290Table(res)}</tbody></table>`;
  }
}

/** TR290 表体（compact=true 用于概览摘要） */
function tr290Table(res: Res, compact = false): string {
  const byKey = new Map(res.tr290.map((t) => [t.name, t]));
  const rows = TR290_ROWS.map(([lv, display, key, impact]) => {
    const hit = byKey.get(key);
    const count = hit?.count ?? 0;
    const first = hit?.first ?? "";
    const badge = `<span class="lv l${lv}">P${lv}</span>`;
    const status = count > 0 ? `<span class="pillw">${count} 次</span>` : `<span class="tok">正常</span>`;
    const numColor = count > 0 ? "var(--warn)" : "var(--mut)";
    const tail = count > 0 ? `${first || "—"} · ${impact}` : impact;
    return `<tr><td>${badge}</td><td>${esc(display)}</td><td>${status}</td>` +
      `<td class="r" style="color:${numColor}">${count}</td><td style="color:${count > 0 ? "var(--mut)" : "var(--faint)"}">${esc(tail)}</td></tr>`;
  });
  return `<table class="dt"><thead><tr>
      <th style="width:60px">级别</th><th>检测项</th><th style="width:90px">状态</th>
      <th style="width:70px;text-align:right">次数</th><th>最近发生 / 影响</th>
    </tr></thead><tbody>${rows.join("")}</tbody></table>`;
}

// ═══════════════════════════ 小工具 ═══════════════════════════

function scard(
  label: string,
  value: string,
  unit: string,
  sub: string,
  color = "",
  warn = false,
): string {
  const uc = unit ? `<i>${unit}</i>` : "";
  const vc = color ? ` style="color:${color}"` : "";
  return `<div class="scard${warn ? " w" : ""}"><small>${esc(label)}</small><b${vc}>${esc(value)}${uc}</b><span>${esc(sub)}</span></div>`;
}

export function esc(s: string): string {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** 自包含 HTML 报告：给巡检/外发用，双击就能看，不依赖本程序 */
function buildHtmlReport(r: Res): string {
  const mb = (n: number) => `${(n / 1048576).toFixed(2)} MB`;
  const mbps = (n: number) => `${(n / 1e6).toFixed(2)} Mbps`;
  const now = new Date().toLocaleString("zh-CN", { hour12: false });
  const tr = r.tr290 || [];
  const bad = tr.filter((t) => t.level >= 2);
  const pids = (r.pids || []).slice(0, 400);
  const progs = r.programs || [];
  const rateMax = (r.rate || []).reduce((m, x) => Math.max(m, x[2] || 0), 0);

  const kv = (k: string, v: string) => `<tr><th>${esc(k)}</th><td>${esc(v)}</td></tr>`;
  const trRow = (t: { level: number; name: string; count: number; first: string; detail: string }) =>
    `<tr class="${t.level >= 2 ? "bad" : t.level === 1 ? "mid" : ""}"><td>L${t.level}</td><td>${esc(
      t.name,
    )}</td><td class="num">${t.count}</td><td>${esc(t.first || "")}</td><td>${esc(t.detail || "")}</td></tr>`;
  const progBlock = progs
    .map(
      (p) => `<div class="pg"><h3>节目 ${p.number}</h3><table class="t">
      <tr><th>PMT PID</th><td class="num">${p.pmt_pid}</td><th>PCR PID</th><td class="num">${p.pcr_pid}</td></tr></table>
      <table class="t"><thead><tr><th>PID</th><th>类型</th><th>种类</th></tr></thead><tbody>${(p.streams || [])
        .map((s) => `<tr><td class="num">${s.pid}</td><td>${esc(s.type_name)}</td><td>${esc(s.kind)}</td></tr>`)
        .join("")}</tbody></table></div>`,
    )
    .join("");
  const pidRows = pids
    .map(
      (p) =>
        `<tr class="${p.scrambled ? "bad" : ""}"><td class="num">${p.pid}</td><td>${esc(p.kind)}</td>
        <td class="num">${p.packets}</td><td class="num">${mb(p.bytes)}</td>
        <td class="num">${mb(p.payload_bytes)}</td><td class="num">${p.scrambled || 0}</td></tr>`,
    )
    .join("");

  return `<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8">
<title>TS 流质量检测报告</title>
<style>
  :root{--ink:#111827;--mut:#6b7280;--line:#e5e7eb;--bad:#b42318;--mid:#b45309;--ok:#15803d;--acc:#6366f1}
  *{box-sizing:border-box}
  body{margin:0;padding:32px 28px 48px;font:14px/1.6 "Microsoft YaHei",system-ui,sans-serif;color:var(--ink);background:#f8fafc}
  .wrap{max-width:980px;margin:0 auto;background:#fff;border:1px solid var(--line);border-radius:14px;padding:28px 30px 34px}
  h1{margin:0 0 4px;font-size:22px;letter-spacing:.5px}
  .sub{color:var(--mut);font-size:12.5px;margin-bottom:20px}
  h2{margin:26px 0 10px;font-size:15px;border-left:3px solid var(--acc);padding-left:8px}
  .sum{display:flex;flex-wrap:wrap;gap:10px;margin:6px 0 4px}
  .kv{min-width:150px;border:1px solid var(--line);border-radius:10px;padding:8px 12px;background:#fbfbfe}
  .kv small{display:block;color:var(--mut);font-size:11.5px}
  .kv b{font-size:15px}
  table.t{width:100%;border-collapse:collapse;font-size:12.5px;margin:6px 0}
  table.t th,table.t td{border-bottom:1px solid var(--line);padding:6px 8px;text-align:left;vertical-align:top}
  table.t thead th{background:#f3f4f8;font-weight:600;color:#374151}
  table.t td.num{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
  table.t th[scope],table.t th{white-space:nowrap}
  tr.bad td{background:#fef3f2;color:var(--bad)}
  tr.mid td{background:#fffbeb}
  .pg{margin:10px 0 14px}
  .pg h3{margin:8px 0 4px;font-size:13px;color:#374151}
  .empty{color:var(--mut);font-size:12.5px;padding:6px 0}
  .foot{margin-top:26px;padding-top:12px;border-top:1px solid var(--line);color:var(--mut);font-size:11.5px}
</style></head><body><div class="wrap">
  <h1>TS 流质量检测报告</h1>
  <div class="sub">生成时间 ${esc(now)} · 视听质量检测工作站</div>

  <div class="sum">
    <div class="kv"><small>码流平均码率</small><b>${mbps(r.bitrate_avg)}</b></div>
    <div class="kv"><small>文件大小</small><b>${mb(r.file_size)}</b></div>
    <div class="kv"><small>包数</small><b>${r.packets}</b></div>
    <div class="kv"><small>同步错误</small><b style="color:${r.sync_errors ? "var(--bad)" : "var(--ok)"}">${r.sync_errors}</b></div>
    <div class="kv"><small>PCR 时长</small><b>${(r.duration_pcr || 0).toFixed(2)} s</b></div>
    <div class="kv"><small>TR290 异常项</small><b style="color:${bad.length ? "var(--bad)" : "var(--ok)"}">${bad.length}</b></div>
    <div class="kv"><small>节目数 / PID 数</small><b>${progs.length} / ${(r.pids || []).length}</b></div>
    <div class="kv"><small>加密</small><b>${r.encrypted || r.has_scrambled ? "有（加扰）" : "无"}</b></div>
  </div>

  <h2>基本信息</h2>
  <table class="t"><tbody>
    ${kv("来源", r.path)}
    ${kv("包长", `${r.packet_size} 字节`)}
    ${kv("PCR PID", r.pcr_pid == null ? "无" : String(r.pcr_pid))}
    ${kv("PCR 恒定码率", r.pcr_cbr ? "是" : "否")}
    ${kv("SI 截断", r.si_truncated ? "是（表未取全）" : "否")}
    ${kv("GOP", r.gop_note || "—")}
    ${kv("峰值码率（采样）", rateMax ? mbps(rateMax) : "—")}
  </tbody></table>

  <h2>TR 290 优先级 1/2/3</h2>
  ${tr.length ? `<table class="t"><thead><tr><th>级别</th><th>项目</th><th>次数</th><th>首次</th><th>说明</th></tr></thead><tbody>${tr.map(trRow).join("")}</tbody></table>` : `<div class="empty">未采到 TR290 指标。</div>`}

  <h2>节目结构</h2>
  ${progBlock || `<div class="empty">未读到节目表（PAT/PMT 缺失或非 TS 封装）。</div>`}

  <h2>PID 占用（${pids.length} / ${(r.pids || []).length}，按字节降序）</h2>
  ${pidRows ? `<table class="t"><thead><tr><th>PID</th><th>种类</th><th>包数</th><th>字节</th><th>净荷</th><th>加扰</th></tr></thead><tbody>${pidRows}</tbody></table>` : `<div class="empty">无 PID 数据。</div>`}

  <div class="foot">本报告由「视听质量检测工作站」自动生成，数值取自对源的一次完整扫描；如需复核请保留原始码流与同版本程序。</div>
</div></body></html>`;
}

void TAB_LABELS;
