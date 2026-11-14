//! 视频分析页 —— 扫描/实时轮询 + 7 页签渲染。
//! 数据源：analysis_progress（200ms）/ analysis_result（1s 实时）/ packet_rows。

import { api, type Res } from "./api";
import { icon } from "./icons";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

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
  const nic = $("a-nic") as HTMLSelectElement;
  const act = $("a-act") as HTMLButtonElement;
  const histBtn = $("a-hist") as HTMLButtonElement;
  const histPop = $("a-histpop") as HTMLElement;

  api.listNics().then((list) => {
    nic.innerHTML = [`<option>自动</option>`, ...list.map((n) => `<option>${esc(n)}</option>`)].join("");
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
    if (running) {
      await api.analysisStop();
      running = false;
      setActUi(false);
      return;
    }
    const u = url.value.trim();
    if (!u) return;
    await api.pushSourceHistory("analysis", u);
    try {
      await api.analysisStart(u, nic.value === "自动" ? "" : nic.value);
      running = true;
      setActUi(true);
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }
  function setActUi(on: boolean) {
    act.classList.toggle("stop", on);
    act.innerHTML = on ? `<span>${icon("square", 13)}</span>停止` : `<span>${icon("search", 13)}</span>开始扫描`;
  }
  setActUi(false);

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

  // 报告导出
  document.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).closest("#a-export")) {
      api.exportReport().then((p) => {
        if (p) pushLog(`报告已导出：${p}`);
      });
    }
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
      // 收割完成态
      if (lastMode !== 0 && p.mode === 0) {
        running = false;
        setActUi(false);
        if (!p.err) hasResult = true;
        refreshResult();
      }
      lastMode = p.mode;
      renderStatus(p);
    } catch {
      /* 静默 */
    }
  }, 200);
  setInterval(async () => {
    if (lastMode === 2) refreshResult();
  }, 1000);
  setInterval(async () => {
    if (lastMode !== 0) pushLogList(await api.analysisLog());
  }, 600);

  window.addEventListener("page-changed", (e) => {
    page = (e as CustomEvent).detail;
    if (page === "analysis" && hasResult) refreshResult();
  });
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
  const el = document.querySelector<HTMLElement>('[data-panel="logs"] .logwrap');
  if (el) {
    el.scrollTop = el.scrollHeight;
    el.innerHTML = logLines.map(esc).join("<br>");
  }
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
function renderBitrate(res: Res | null) {
  const el = document.querySelector<HTMLElement>('[data-panel="bitrate"]')!;
  el.hidden = curTab !== "bitrate";
  if (!res) return;
  const rates = res.rate.map((r) => r[2]);
  if (rates.length === 0) {
    el.innerHTML = `<div class="empty" style="flex:1">无码率区间（流中没有 PCR）</div>`;
    return;
  }
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
  el.innerHTML = cards + `<div class="panel" style="flex:1;padding:8px"><div class="chartbox" id="rate-chart"></div></div>`;

  import("uplot").then(async (m) => {
    const uPlot = m.default;
    if (!uplotCss) {
      // @ts-ignore CSS 副作用导入
      await import("uplot/dist/uPlot.min.css");
      uplotCss = true;
    }
    const host = document.getElementById("rate-chart")!;
    if (chart) {
      chart.destroy();
      chart = null;
    }
    const w = host.clientWidth || 600;
    const hgt = host.clientHeight || 240;
    chart = new uPlot(
      {
        width: w,
        height: hgt,
        cursor: { show: false },
        legend: { show: false },
        axes: [
          {
            stroke: "#71717a",
            grid: { stroke: "rgba(63,63,70,.35)", width: 1 },
            ticks: { stroke: "#3f3f46" },
            values: (_, ticks) => ticks.map((t: number) => String(Math.round(t))),
          },
          {
            stroke: "#71717a",
            grid: { stroke: "rgba(63,63,70,.35)", width: 1 },
            size: 62,
            values: (_, ticks) => ticks.map((t: number) => `${(t / 1e6).toFixed(2)}`),
          },
        ],
        series: [
          {},
          {
            stroke: "#818cf8",
            width: 1.4,
            points: { show: false },
          },
        ],
      },
      [rates.map((_, i) => i), rates],
      host,
    );
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

void TAB_LABELS;
