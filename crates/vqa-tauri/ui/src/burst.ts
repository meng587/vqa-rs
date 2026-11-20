//! 微突发检测页：单路实时检测 / 多频道轮询。
//!
//! 统计仍由后端一路 UDP 收包完成；页面只负责：
//! - 参数（网卡 / 阈值 / 检测时间）的设置与持久化；
//! - 当前频道的单路连续检测；
//! - 从共享频道列表里勾选多个频道，逐路采样轮询；
//! - 清晰的坐标轴、网格和柱状图。

import { api, BurstStats, Channel, NicRow } from "./api";
import {
  ChannelBoard,
  RowState,
} from "./channels";
import { registerFlowTarget } from "./transfer";

const $ = <T extends HTMLElement>(id: string): T | null =>
  document.getElementById(id) as T | null;

type WinMs = 1 | 10 | 100 | 1000;

const WINDOWS: WinMs[] = [1, 10, 100, 1000];
const SERIES_N = 240;
const DEFAULT_POLL_SECS = 8; // 轮询时每个频道采样时长
const SETTINGS_KEY = "burst.settings";
const COLLAPSED_KEY = "burst.ch.collapsed";

const EMPTY: BurstStats = {
  running: false,
  secs: 0,
  packets: 0,
  bytes: 0,
  mbps_avg: 0,
  mbps_peak: 0,
  ms_peak_bytes: 0,
  peak_ratio: 0,
  burst_cnt: 0,
  bytes_per_ms_avg: 0,
  bytes_per_window_avg: 0,
  series: [],
  url: "",
  window_ms: 1,
  err: null,
};

interface Settings {
  nic: string;
  ratio: number;
  window_ms: WinMs;
  poll_secs: number;
}

interface RowMeta {
  state: RowState;
  text: string;
}

let settings: Settings = { nic: "", ratio: 2, window_ms: 1, poll_secs: DEFAULT_POLL_SECS };
let channels: Channel[] = [];
let activeUrl = "";
let board: ChannelBoard | null = null;
let nics: NicRow[] = [];
let snapshot: BurstStats = { ...EMPTY };
let running = false;
let starting = false;
let polling = false;
let pollStop = false;
let pollList: Channel[] = [];
let pollPos = 0;
let pollStartedAt = 0;
let snapshotTimer: number | null = null;
let modalOpen = false;
let collapsed = true;
const rowMeta = new Map<string, RowMeta>();

function esc(s: string): string {
  return (s || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function normalizeWin(v: unknown): WinMs {
  const n = Number(v);
  return WINDOWS.includes(n as WinMs) ? (n as WinMs) : 1;
}

function normalizePollSecs(v: unknown): number {
  const n = Math.round(Number(v));
  return Number.isFinite(n) && n >= 1 && n <= 120 ? n : DEFAULT_POLL_SECS;
}

function currentUrl(): string {
  return ($<HTMLInputElement>("b-url")?.value || "").trim();
}

function currentWin(): WinMs {
  return normalizeWin(snapshot.window_ms || settings.window_ms);
}

function locked(): boolean {
  return running || starting || polling;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

/** 网卡下拉文案：`IP · 网卡名`（与播放器/合规一致） */
function nicLabel(n: { ip: string; name: string; desc: string; up: boolean }): string {
  const nm = (n.name || n.desc || "").trim();
  const base = nm ? `${n.ip} · ${nm}` : n.ip;
  return n.up ? base : `${base}（未连接）`;
}

function currentNicLabel(): string {
  if (!settings.nic) return "自动";
  const n = nics.find((x) => x.ip === settings.nic);
  return n ? nicLabel(n) : settings.nic;
}

function toMbps(bytes: number, win: WinMs): number {
  const secs = win / 1000;
  return secs > 0 ? (bytes * 8) / secs / 1e6 : 0;
}

function niceMax(v: number): number {
  if (!Number.isFinite(v) || v <= 0) return 1;
  const pow = Math.pow(10, Math.floor(Math.log10(v)));
  const n = v / pow;
  const m = n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10;
  return m * pow;
}

function axLabel(v: number): string {
  if (v >= 100) return v.toFixed(0);
  if (v >= 10) return v.toFixed(1);
  return v.toFixed(1);
}

function spanLabel(ms: number): string {
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}min`;
  if (ms >= 1_000) {
    const s = ms / 1_000;
    return `${s >= 10 ? s.toFixed(0) : s.toFixed(1)}s`;
  }
  return `${Math.round(ms)}ms`;
}

function updateCurrentLabel() {
  const el = $<HTMLElement>("b-current-name");
  const box = $<HTMLElement>("b-current");
  const ch = channels.find((c) => c.url === activeUrl);
  if (el) el.textContent = ch ? ch.name || "未命名频道" : activeUrl ? "手动来源" : "未选择";
  box?.classList.toggle("on", !!activeUrl);
}

function updateCfgChips() {
  const el = $<HTMLElement>("b-cfg");
  if (!el) return;
  el.innerHTML =
    `<span class="bst-cfgchip"><i>网卡</i><b>${esc(settings.nic ? currentNicLabel() : "自动")}</b></span>` +
    `<span class="bst-cfgchip"><i>阈值</i><b>${settings.ratio.toFixed(2)} ×</b></span>` +
    `<span class="bst-cfgchip"><i>窗口</i><b>${settings.window_ms} ms</b></span>` +
    `<span class="bst-cfgchip"><i>轮询</i><b>${settings.poll_secs} s</b></span>`;
}

function setStatus(kind: "idle" | "running" | "error", text: string) {
  const dot = $<HTMLElement>("b-dot");
  if (dot) dot.className = `dot${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const st = $<HTMLElement>("b-status-text");
  if (st) st.textContent = text;
  const box = $<HTMLElement>("b-status");
  if (box) box.classList.toggle("err", kind === "error");
  const lamp = document.getElementById("lamp-burst");
  if (lamp) lamp.className = `lamp${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
}

function syncLockUi() {
  const isLocked = locked();
  const act = $<HTMLButtonElement>("b-act");
  const stop = $<HTMLButtonElement>("b-stop");
  const setBtn = $<HTMLButtonElement>("b-settings");
  const browse = $<HTMLButtonElement>("b-browse");
  if (act) act.disabled = isLocked;
  if (browse) browse.disabled = isLocked;
  if (setBtn) setBtn.disabled = isLocked;
  if (stop) stop.hidden = !(running || polling);
  board?.setBusy(isLocked);
}

function kpiCard(label: string, value: string, hint = ""): string {
  return (
    `<div class="kpi bst-kpi"><span class="kpi-l">${label}</span>` +
    `<b class="kpi-v">${value}</b>` +
    (hint ? `<span class="kpi-h">${hint}</span>` : "") +
    `</div>`
  );
}

function renderKpi(s: BurstStats) {
  const el = $<HTMLElement>("b-kpi");
  if (!el) return;
  const win = normalizeWin(s.window_ms || settings.window_ms);
  const avgWin =
    s.bytes_per_window_avg > 0 ? s.bytes_per_window_avg : s.bytes_per_ms_avg * win;
  el.innerHTML =
    kpiCard("平均码率", `${s.mbps_avg.toFixed(2)} Mbps`, `${Math.round(s.secs)} s`) +
    kpiCard("峰值瞬时", `${s.mbps_peak.toFixed(2)} Mbps`, `${win} ms 单窗口`) +
    kpiCard("峰值 / 均值", `${s.peak_ratio.toFixed(1)} ×`, `${s.ms_peak_bytes} B / ${win}ms`) +
    kpiCard(
      "微突发次数",
      `${s.burst_cnt} 次`,
      s.burst_cnt > 0 ? "已超过设定阈值" : `阈值 ${settings.ratio.toFixed(2)} ×`,
    ) +
    kpiCard("收包数", s.packets.toLocaleString(), `${(s.bytes / 1048576).toFixed(1)} MB`);
}

function renderXAxis(win: WinMs) {
  const el = $<HTMLElement>("b-xaxis");
  if (!el) return;
  const total = SERIES_N * win;
  const labels = [
    `−${spanLabel(total)}`,
    `−${spanLabel(total * 0.75)}`,
    `−${spanLabel(total * 0.5)}`,
    `−${spanLabel(total * 0.25)}`,
    "现在",
  ];
  el.innerHTML = labels.map((t) => `<span>${esc(t)}</span>`).join("");
}

function drawSpark(s: BurstStats) {
  const cv = $<HTMLCanvasElement>("b-spark");
  const plot = $<HTMLElement>("b-plot");
  if (!cv || !plot) return;

  const w = cv.clientWidth;
  const h = cv.clientHeight;
  if (w <= 0 || h <= 0) return;

  const dpr = window.devicePixelRatio || 1;
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
  }
  const ctx = cv.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const win = normalizeWin(s.window_ms || settings.window_ms);
  const ratio = Math.max(1.05, settings.ratio || 2);
  const series = s.series || [];
  const avgWin =
    s.bytes_per_window_avg > 0 ? s.bytes_per_window_avg : s.bytes_per_ms_avg * win;
  const thresholdB = avgWin * ratio;
  const peakMbps = series.length ? toMbps(Math.max(...series), win) : 0;
  const thresholdMbps = toMbps(thresholdB, win);
  const yMax = Math.max(
    1,
    niceMax(Math.max(peakMbps, thresholdMbps, 0.12)),
  );

  // 网格与坐标轴
  ctx.lineWidth = 1;
  ctx.strokeStyle = "rgba(217,205,169,.16)";
  ctx.fillStyle = "rgba(217,205,169,.035)";
  ctx.fillRect(0, 0, w, h);
  for (let i = 0; i <= 4; i++) {
    const x = Math.round((w * i) / 4) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, h);
    ctx.stroke();
  }
  for (let i = 0; i <= 3; i++) {
    const y = Math.round((h * i) / 3) + 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }
  ctx.strokeStyle = "rgba(217,205,169,.45)";
  ctx.beginPath();
  ctx.moveTo(0, h - 0.5);
  ctx.lineTo(w, h - 0.5);
  ctx.stroke();

  // 柱：最近 240 个窗口，右侧对齐，旧窗口在左侧
  const bw = w / SERIES_N;
  const start = w - Math.min(series.length, SERIES_N) * bw;
  for (let i = 0; i < Math.min(series.length, SERIES_N); i++) {
    const v = series[i];
    const vm = toMbps(v, win);
    const bh = Math.max(1, Math.min(h - 2, (vm / yMax) * (h - 2)));
    const x = start + i * bw;
    ctx.fillStyle =
      thresholdB > 0 && v >= thresholdB
        ? "rgba(248,113,113,.92)"
        : "rgba(217,180,106,.82)";
    ctx.fillRect(Math.floor(x) + 0.5, h - bh, Math.max(1, Math.floor(bw) - 1), bh);
  }

  // 阈值虚线
  if (thresholdMbps > 0) {
    const y = h - Math.min(h, (thresholdMbps / yMax) * h);
    ctx.strokeStyle = "rgba(248,160,140,.72)";
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.setLineDash([]);
  }

  renderYAxis(yMax);
  renderXAxis(win);
  renderThresholdLabel(thresholdMbps, yMax, plot, win);
  renderEmptyState(s, series);
  const chip = $<HTMLElement>("b-win-chip");
  if (chip) chip.textContent = `窗口 ${win} ms`;
}

function renderYAxis(max: number) {
  const el = $<HTMLElement>("b-yaxis");
  if (!el) return;
  const ticks = [0, 1 / 3, 2 / 3, 1];
  el.innerHTML = ticks
    .map((t) => {
      const value = max * t;
      const top = (1 - t) * 100;
      const transform = t === 1 ? "none" : t === 0 ? "translateY(-100%)" : "translateY(-50%)";
      return `<span class="bst-ytick" style="top:${top}%;transform:${transform}">${axLabel(value)}</span>`;
    })
    .join("");
}

function renderThresholdLabel(thresholdMbps: number, yMax: number, plot: HTMLElement, win: WinMs) {
  const el = $<HTMLElement>("b-thr");
  if (!el) return;
  if (!(thresholdMbps > 0) || !Number.isFinite(thresholdMbps)) {
    el.hidden = true;
    return;
  }
  el.hidden = false;
  const canvasTop = 8;
  const canvasBottom = 28;
  const canvasH = Math.max(1, plot.clientHeight - canvasTop - canvasBottom);
  const y = canvasTop + (1 - Math.min(1, thresholdMbps / yMax)) * canvasH;
  el.style.top = `${y}px`;
  el.textContent = `阈值 ${thresholdMbps.toFixed(thresholdMbps >= 10 ? 0 : 1)} Mbps（${settings.ratio.toFixed(2)}×）`;
  el.title = `每个 ${win}ms 窗口超过该瞬时码率即计一次微突发`;
}

function renderEmptyState(s: BurstStats, series: number[]) {
  const el = $<HTMLElement>("b-empty");
  if (!el) return;
  el.hidden = series.length > 0;
  if (series.length === 0) {
    el.textContent = running
      ? "等待首个检测窗口…"
      : "暂无数据 · 开始检测后显示最近 240 个窗口";
  }
}

function updateStatic(win: WinMs) {
  const title = $<HTMLElement>("b-spark-title");
  if (title) title.textContent = `最近 240 个 ${win}ms 窗口到达量（Mbps）`;
  const sub = $<HTMLElement>("b-spark-sub");
  if (sub) sub.textContent = `每根柱 = ${win}ms 内收到的字节`;
  const note = $<HTMLElement>("b-foot-note");
  if (note) {
    note.textContent =
      `口径：把到达时间按 ${win}ms 分桶，峰值 = 单窗口字节量换算的瞬时码率；` +
      `某个窗口 ≥ 均值 × 阈值即计一次微突发。`;
  }
  const chip = $<HTMLElement>("b-win-chip");
  if (chip) chip.textContent = `窗口 ${win} ms`;
  renderXAxis(win);
}

function renderSummary(s: BurstStats) {
  const el = $<HTMLElement>("b-sum");
  if (!el) return;
  if (!s.running && s.packets === 0) {
    el.innerHTML = `<span class="bst-chip quiet">等待检测</span>`;
    return;
  }
  el.innerHTML =
    `<span class="bst-chip ok">${s.mbps_avg.toFixed(2)} Mbps 平均</span>` +
    `<span class="bst-chip">峰值 ${s.mbps_peak.toFixed(2)} Mbps</span>` +
    `<span class="bst-chip ${s.burst_cnt > 0 ? "bad" : ""}">微突发 ${s.burst_cnt} 次</span>`;
}

function renderAll(s: BurstStats) {
  snapshot = s;
  const win = normalizeWin(s.window_ms || settings.window_ms);
  updateStatic(win);
  renderKpi(s);
  drawSpark(s);
  renderSummary(s);
  updateCurrentLabel();
}

function getRowMeta(url: string): RowMeta {
  return rowMeta.get(url) || { state: "idle", text: "—" };
}

function updateRow(url: string, state: RowState, text: string) {
  rowMeta.set(url, { state, text });
  board?.updateRowByUrl(url, state, text);
}

function summaryOf(s: BurstStats): RowMeta {
  if (s.packets === 0) return { state: "warn", text: "无数据" };
  const text = `平均 ${s.mbps_avg.toFixed(1)}M · 峰值 ${s.mbps_peak.toFixed(1)}M · 突发 ${s.burst_cnt}`;
  return { state: s.burst_cnt > 0 ? "warn" : "ok", text };
}

function setChannelCollapsed(on: boolean, persist = true) {
  collapsed = on;
  const body = $<HTMLElement>("b-body");
  const rail = $<HTMLButtonElement>("b-ch-rail");
  body?.classList.toggle("is-ch-collapsed", on);
  if (rail) {
    rail.setAttribute("aria-expanded", String(!on));
    rail.setAttribute("aria-label", on ? "展开频道列表" : "收起频道列表");
    rail.title = on ? "展开频道列表" : "收起频道列表";
  }
  if (persist) api.uiSet(COLLAPSED_KEY, on).catch(() => {});
  window.setTimeout(() => drawSpark(snapshot), 30);
}

async function loadBurstChannels() {
  if (!board) return;
  await board.load();
  channels = board.getChannels();
  if (!activeUrl) {
    const first = channels.find((c) => c.enabled && c.url);
    if (first) {
      activeUrl = first.url;
      const input = $<HTMLInputElement>("b-url");
      if (input) input.value = first.url;
    }
  }
  board.selectByUrl(activeUrl);
  updateCurrentLabel();
  syncLockUi();
}

async function loadNics() {
  try {
    nics = await api.listNics();
  } catch {
    nics = [];
  }
  const sel = $<HTMLSelectElement>("b-set-nic");
  if (sel) {
    sel.innerHTML =
      `<option value="">自动</option>` +
      nics.map((n) => `<option value="${esc(n.ip)}">${esc(nicLabel(n))}</option>`).join("");
  }
  syncSettingsInputs();
  updateCfgChips();
}

async function loadSettings() {
  let raw: any = null;
  let legacyWin: any = null;
  try {
    raw = await api.uiGet(SETTINGS_KEY);
    legacyWin = await api.uiGet("burst.window_ms");
  } catch {
    /* 首次运行，使用默认值 */
  }
  if (raw && typeof raw === "object") {
    const ratio = Number(raw.ratio);
    const win = normalizeWin(raw.window_ms);
    settings = {
      nic: typeof raw.nic === "string" ? raw.nic : "",
      ratio: Number.isFinite(ratio) && ratio >= 1.05 ? Math.min(20, ratio) : 2,
      window_ms: win,
      poll_secs: normalizePollSecs(raw.poll_secs),
    };
  } else {
    settings = {
      nic: "",
      ratio: 2,
      window_ms: normalizeWin(legacyWin),
      poll_secs: DEFAULT_POLL_SECS,
    };
  }
  updateStatic(settings.window_ms);
  syncSettingsInputs();
  updateCfgChips();
  snapshot = { ...EMPTY, window_ms: settings.window_ms };
  renderAll(snapshot);
  board?.refreshFootnote();
}

function syncSettingsInputs() {
  const nic = $<HTMLSelectElement>("b-set-nic");
  if (nic) {
    if (settings.nic && !Array.from(nic.options).some((o) => o.value === settings.nic)) {
      const opt = document.createElement("option");
      opt.value = settings.nic;
      opt.textContent = `${settings.nic}（未找到）`;
      nic.appendChild(opt);
    }
    nic.value = settings.nic;
  }
  const ratio = $<HTMLInputElement>("b-set-ratio");
  if (ratio) ratio.value = settings.ratio.toFixed(2);
  const win = $<HTMLSelectElement>("b-set-window");
  if (win) win.value = String(settings.window_ms);
  const poll = $<HTMLInputElement>("b-set-poll");
  if (poll) poll.value = String(settings.poll_secs);
}

function openSettings() {
  if (locked()) {
    setStatus("error", "检测进行中，停止后才能修改参数");
    return;
  }
  syncSettingsInputs();
  const modal = $<HTMLElement>("b-set-modal");
  if (!modal) return;
  modal.hidden = false;
  modalOpen = true;
  $<HTMLInputElement>("b-set-ratio")?.focus();
}

function closeSettings() {
  const modal = $<HTMLElement>("b-set-modal");
  if (modal) modal.hidden = true;
  modalOpen = false;
}

async function saveSettings() {
  if (locked()) {
    closeSettings();
    setStatus("error", "检测进行中，停止后才能修改参数");
    return;
  }
  const ratio = Number($<HTMLInputElement>("b-set-ratio")?.value || "2");
  const win = normalizeWin($<HTMLSelectElement>("b-set-window")?.value);
  const nic = $<HTMLSelectElement>("b-set-nic")?.value || "";
  const poll = normalizePollSecs($<HTMLInputElement>("b-set-poll")?.value);
  if (!Number.isFinite(ratio) || ratio < 1.05) {
    setStatus("error", "突发阈值需 ≥ 1.05");
    return;
  }
  const changed = settings.ratio !== ratio || settings.window_ms !== win;
  settings = { nic, ratio: Math.min(20, ratio), window_ms: win, poll_secs: poll };
  try {
    await api.uiSet(SETTINGS_KEY, settings);
    await api.uiSet("burst.window_ms", win);
  } catch {
    setStatus("error", "参数保存失败");
    return;
  }
  updateCfgChips();
  updateStatic(win);
  syncSettingsInputs();
  if (changed) {
    // 旧曲线属于旧口径，避免用新轴去读旧数据
    snapshot = { ...EMPTY, window_ms: win };
    renderAll(snapshot);
  } else {
    drawSpark(snapshot);
  }
  board?.refreshFootnote();
  closeSettings();
  setStatus("idle", "参数已保存 · 待机");
}

function startSnapshotTimer() {
  if (snapshotTimer !== null) return;
  snapshotTimer = window.setInterval(() => void pollSnapshot(), 500);
}

function stopSnapshotTimer() {
  if (snapshotTimer !== null) {
    window.clearInterval(snapshotTimer);
    snapshotTimer = null;
  }
}

async function pollSnapshot() {
  try {
    const s = await api.burstSnapshot();
    snapshot = s;
    renderAll(s);
    if (running) {
      updateRunStatus();
      if (!s.running) {
        running = false;
        stopSnapshotTimer();
        syncLockUi();
        setStatus("idle", `已停止 · 共 ${s.packets} 包`);
      }
    }
  } catch {
    /* 忽略瞬时失败，下一轮继续 */
  }
}

function updateRunStatus() {
  if (!running) return;
  if (polling) {
    const dwellMs = settings.poll_secs * 1000;
    const name = pollList[pollPos - 1]?.name || "轮询";
    const elapsed = Math.min(dwellMs, Date.now() - pollStartedAt);
    const countdown = Math.max(0, (dwellMs - elapsed) / 1000);
    setStatus(
      "running",
      `轮询 ${pollPos}/${pollList.length} · ${name} · ${countdown.toFixed(1)} s / ${settings.poll_secs} s`,
    );
  } else {
    setStatus(
      "running",
      `检测中 · ${Math.round(snapshot.secs)} s · ${normalizeWin(snapshot.window_ms || settings.window_ms)} ms`,
    );
  }
  const idx = channels.findIndex((c) => c.url === activeUrl);
  if (idx >= 0) {
    const txt = `平均 ${snapshot.mbps_avg.toFixed(1)}M · 峰值 ${snapshot.mbps_peak.toFixed(1)}M · 突发 ${snapshot.burst_cnt}`;
    updateRow(activeUrl, "run", txt);
  }
}

async function startContinuous() {
  if (locked()) return;
  const url = currentUrl();
  if (!url) {
    setStatus("error", "频道地址为空");
    return;
  }
  activeUrl = url;
  board?.selectByUrl(url);
  updateCurrentLabel();
  starting = true;
  syncLockUi();
  setStatus("running", url.startsWith("udp://") || url.startsWith("rtp://") ? "绑定端口…" : "启动本地文件回放…");
  try {
    const s = await api.burstStart(url, settings.nic, settings.ratio, settings.window_ms);
    starting = false;
    running = true;
    snapshot = s;
    const idx = channels.findIndex((c) => c.url === url);
    if (idx >= 0) updateRow(url, "run", "采集…");
    startSnapshotTimer();
    syncLockUi();
    renderAll(s);
    updateRunStatus();
  } catch (e: any) {
    starting = false;
    running = false;
    stopSnapshotTimer();
    syncLockUi();
    setStatus("error", String(e));
  }
}

async function stopSession(showStatus = true) {
  pollStop = true;
  stopSnapshotTimer();
  if (!running && !starting) {
    if (polling) {
      if (showStatus) setStatus("idle", "轮询已停止");
      syncLockUi();
    }
    return;
  }
  starting = false;
  let s: BurstStats | null = null;
  try {
    s = await api.burstStop();
    snapshot = s;
  } catch {
    /* 停止失败也把前端状态复位 */
  }
  running = false;
  syncLockUi();
  if (s) {
    renderAll(s);
    const meta = summaryOf(s);
    const idx = channels.findIndex((c) => c.url === activeUrl);
    if (idx >= 0) updateRow(activeUrl, meta.state, meta.text);
    if (showStatus) setStatus("idle", `已停止 · 共 ${s.packets} 包`);
  } else if (showStatus) {
    setStatus("idle", "已停止");
  }
}

async function pollSelected(list: Channel[]) {
  if (locked()) return;
  if (!list.length) {
    setStatus("error", "当前分组没有勾选可轮询的频道");
    return;
  }

  polling = true;
  pollStop = false;
  pollList = list;
  pollPos = 0;
  syncLockUi();

  for (let k = 0; k < list.length; k++) {
    if (pollStop) break;
    const ch = list[k];
    if (!ch) continue;
    activeUrl = ch.url;
    const input = $<HTMLInputElement>("b-url");
    if (input) input.value = ch.url;
    board?.selectByUrl(ch.url);
    pollPos = k + 1;
    updateRow(ch.url, "run", "准备…");
    setStatus(
      "running",
      `轮询 ${pollPos}/${list.length} · ${ch.name || "未命名频道"} · 准备中`,
    );

    try {
      await api.burstStart(ch.url, settings.nic, settings.ratio, settings.window_ms);
      running = true;
      pollStartedAt = Date.now();
      snapshot = { ...EMPTY, url: ch.url, window_ms: settings.window_ms };
      renderAll(snapshot);
      startSnapshotTimer();
      syncLockUi();
      updateRow(ch.url, "run", "采集…");
      while (!pollStop && Date.now() - pollStartedAt < settings.poll_secs * 1000) {
        await sleep(250);
      }
      if (pollStop) break;
      const s = await api.burstStop();
      running = false;
      stopSnapshotTimer();
      snapshot = s;
      renderAll(s);
      updateRunStatus();
      const meta = summaryOf(s);
      updateRow(ch.url, meta.state, meta.text);
      syncLockUi();
    } catch (e: any) {
      running = false;
      stopSnapshotTimer();
      updateRow(ch.url, "err", String(e).slice(0, 80));
      setStatus("error", `轮询 ${pollPos}/${list.length} 失败：${String(e)}`);
      syncLockUi();
    }

    if (pollStop) break;
    await sleep(350);
  }

  const stopped = pollStop;
  polling = false;
  pollStop = false;
  running = false;
  stopSnapshotTimer();
  syncLockUi();
  if (stopped) {
    setStatus("idle", "轮询已停止");
  } else {
    setStatus("idle", `轮询完成 · ${list.length} 个频道`);
  }
}

function bindModal() {
  const modal = $<HTMLElement>("b-set-modal");
  if (!modal) return;
  modal.querySelectorAll<HTMLElement>("[data-bst-close]").forEach((el) => {
    el.addEventListener("click", closeSettings);
  });
  $<HTMLButtonElement>("b-set-x")?.addEventListener("click", closeSettings);
  $<HTMLButtonElement>("b-set-cancel")?.addEventListener("click", closeSettings);
  $<HTMLButtonElement>("b-set-save")?.addEventListener("click", () => void saveSettings());
  modal.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") closeSettings();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && modalOpen) closeSettings();
  });
}

export function initBurst() {
  const host = $<HTMLElement>("b-chboard");
  if (host) {
    board = new ChannelBoard(host, {
      owner: "burst",
      module: "burst",
      title: "频道列表",
      sequential: true,
      compact: true,
      manual: true,
      pollLabel: "轮询选中",
      footnote: () =>
        `轮询时每路采样 ${settings.poll_secs} s · 名称和地址可在「编辑列表」里修改`,
      onCollapse: () => setChannelCollapsed(true),
      onLoaded: (list) => {
        channels = list;
      },
      onSelect: (ch) => {
        if (!ch) return;
        activeUrl = ch.url;
        const input = $<HTMLInputElement>("b-url");
        if (input) input.value = ch.url;
        updateCurrentLabel();
        syncLockUi();
      },
      onDetect: (ch) => {
        if (locked()) return;
        activeUrl = ch.url;
        const input = $<HTMLInputElement>("b-url");
        if (input) input.value = ch.url;
        updateCurrentLabel();
        void startContinuous();
      },
      onPoll: (list) => {
        void pollSelected(list);
      },
      initialRow: (ch) => getRowMeta(ch.url),
    });
  }

  $<HTMLButtonElement>("b-act")?.addEventListener("click", () => void startContinuous());
  $<HTMLButtonElement>("b-browse")?.addEventListener("click", async () => {
    const path = await api.browse();
    if (!path) return;
    const input = $<HTMLInputElement>("b-url");
    if (input) input.value = path;
    activeUrl = path;
    board?.selectByUrl(path);
    updateCurrentLabel();
    syncLockUi();
  });
  $<HTMLButtonElement>("b-stop")?.addEventListener("click", () => void stopSession());
  $<HTMLButtonElement>("b-settings")?.addEventListener("click", openSettings);
  $<HTMLButtonElement>("b-ch-rail")?.addEventListener("click", () =>
    setChannelCollapsed(false),
  );

  $<HTMLInputElement>("b-url")?.addEventListener("input", () => {
    activeUrl = currentUrl();
    board?.selectByUrl(activeUrl);
    updateCurrentLabel();
    syncLockUi();
  });

  bindModal();
  setChannelCollapsed(true, false);

  renderKpi(EMPTY);
  renderSummary(EMPTY);
  updateStatic(settings.window_ms);
  updateCfgChips();
  setStatus("idle", "待机");

  void (async () => {
    await Promise.all([loadNics(), loadSettings(), loadBurstChannels()]);
    try {
      const v = await api.uiGet(COLLAPSED_KEY);
      setChannelCollapsed(v === null || v === undefined ? true : !!v, false);
    } catch {
      /* 默认收起 */
    }
  })();

  const plot = $<HTMLElement>("b-plot");
  if (plot && "ResizeObserver" in window) {
    new ResizeObserver(() => drawSpark(snapshot)).observe(plot);
  }
  window.addEventListener("resize", () => drawSpark(snapshot));
  window.addEventListener("page-changed", (ev) => {
    if ((ev as CustomEvent).detail === "burst") {
      window.requestAnimationFrame(() => drawSpark(snapshot));
    }
  });

  registerFlowTarget(
    "burst",
    async (src) => {
      const u = src.trim();
      if (!u) return { state: "failed", message: "来源为空" };
      const input = $<HTMLInputElement>("b-url");
      if (input) input.value = u;
      if (locked()) {
        return { state: "filled", message: "微突发检测正在工作，地址已填入" };
      }
      await startContinuous();
      return running
        ? { state: "started", message: "已开始检测" }
        : { state: "failed", message: "启动失败，请查看微突发状态栏" };
    },
    () => locked(),
  );
}
