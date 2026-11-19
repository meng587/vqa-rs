//! 微突发检测页：收一路组播，按可配置时间窗看到达量，画最近 240 个窗口的柱状图。

import { api, BurstStats } from "./api";
import { icon } from "./icons";


const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;

/** 网卡下拉文案：`IP · 网卡名`（与播放器/合规一致） */
function nicLabel(n: { ip: string; name: string; desc: string; up: boolean }): string {
  const nm = (n.name || n.desc || "").trim();
  const base = nm ? `${n.ip} · ${nm}` : n.ip;
  return n.up ? base : `${base}（未连接）`;
}

type WinMs = 1 | 10 | 100 | 1000;
const WINDOWS: WinMs[] = [1, 10, 100, 1000];

let timer: number | null = null;

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

function normalizeWin(v: unknown): WinMs {
  const n = Number(v);
  return WINDOWS.includes(n as WinMs) ? (n as WinMs) : 1;
}

function currentWin(): WinMs {
  return normalizeWin($<HTMLSelectElement>("b-window")?.value);
}

function windowBytes(s: BurstStats, win: WinMs): number {
  return s.bytes_per_window_avg > 0 ? s.bytes_per_window_avg : s.bytes_per_ms_avg * win;
}

function updateStatic(win: WinMs) {
  const title = $<HTMLElement>("b-spark-title");
  if (title) title.textContent = `最近 240 个 ${win}ms 窗口到达量`;
  const sub = $<HTMLElement>("b-spark-sub");
  if (sub) sub.textContent = `每根柱 = ${win}ms 内收到的字节`;
  const note = $<HTMLElement>("b-foot-note");
  if (note) {
    note.textContent =
      `口径：把到达时间按 ${win}ms 分桶，峰值 = 单窗口字节量换算的瞬时码率；` +
      `某个窗口 ≥ 均值 × 阈值即计一次微突发。`;
  }
}

function setState(kind: "idle" | "running" | "error", text: string) {
  const dot = $<HTMLElement>("b-dot");
  if (dot) dot.className = `dot${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const st = $<HTMLElement>("b-status-text");
  if (st) st.textContent = text;
  const lamp = document.getElementById("lamp-burst");
  if (lamp) lamp.className = `lamp${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const act = $<HTMLButtonElement>("b-act");
  if (act) act.disabled = kind === "running";
  const stopBtn = $<HTMLButtonElement>("b-stop");
  if (stopBtn) stopBtn.hidden = kind !== "running";
  const win = $<HTMLSelectElement>("b-window");
  if (win) win.disabled = kind === "running";
}

function kpiCard(label: string, value: string, hint = "") {
  return (
    `<div class="kpi"><span class="kpi-l">${label}</span><b class="kpi-v">${value}</b>` +
    (hint ? `<span class="kpi-h">${hint}</span>` : "") +
    `</div>`
  );
}

function renderKpi(s: BurstStats) {
  const el = $<HTMLElement>("b-kpi");
  if (!el) return;
  const win = normalizeWin(s.window_ms);
  const avgWin = windowBytes(s, win);
  el.innerHTML =
    kpiCard("平均码率", `${s.mbps_avg.toFixed(2)} Mbps`, `${Math.round(s.secs)}s`) +
    kpiCard("峰值瞬时", `${s.mbps_peak.toFixed(2)} Mbps`, `单个 ${win}ms 窗口`) +
    kpiCard("峰值/均值", `${s.peak_ratio.toFixed(1)} ×`, `${s.ms_peak_bytes} B/${win}ms`) +
    kpiCard("微突发次数", `${s.burst_cnt} 次`, "窗口 ≥ 均值 × 阈值") +
    kpiCard("收包数", s.packets.toLocaleString(), `${(s.bytes / 1048576).toFixed(1)} MB`);
}

function drawSpark(s: BurstStats) {
  const cv = $<HTMLCanvasElement>("b-spark");
  if (!cv) return;
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth || 800;
  const h = cv.clientHeight || 110;
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
  }
  const ctx = cv.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const series = s.series || [];
  const max = Math.max(1, ...series);
  const GUT = 74; // 左侧刻度区
  const plotW = Math.max(40, w - GUT);
  const n = Math.max(240, series.length);
  const bw = plotW / n;
  const y0 = h - 12; // 基线（留出底部刻度文字）
  const bmax = y0 - 8;
  const win = normalizeWin(s.window_ms);
  const avgWin = windowBytes(s, win);
  ctx.fillStyle = "rgba(255,255,255,.03)";
  ctx.fillRect(0, 0, w, h);

  // Y 轴刻度：max / max/2 / 0，标成该检测窗口对应的瞬时 Mbps
  const toMbps = (b: number) => (b * 8) / (win / 1000) / 1e6;
  ctx.font = '10px Consolas, monospace';
  ctx.textAlign = 'right';
  ctx.fillStyle = '#D9CDA9';
  ctx.strokeStyle = 'rgba(203,189,164,.18)';
  ctx.lineWidth = 1;
  for (const t of [0, 0.5, 1]) {
    const y = y0 - bmax * t;
    ctx.beginPath();
    ctx.moveTo(GUT, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.fillText(`${toMbps(max * t).toFixed(t === 1 ? 0 : 1)} Mbps`, GUT - 6, Math.min(y0, y + 3));
  }
  ctx.textAlign = 'left';

  const ratio = Number($<HTMLInputElement>("b-ratio")?.value || "2") || 2;
  const thr = avgWin * ratio;
  for (let i = 0; i < series.length; i++) {
    const bh = Math.max(1, (series[i] / max) * bmax);
    ctx.fillStyle = thr > 0 && series[i] >= thr ? "rgba(248,113,113,.9)" : "rgba(168,121,58,.8)";
    ctx.fillRect(GUT + i * bw + 0.5, y0 - bh, Math.max(1, bw - 1), bh);
  }
  if (avgWin > 0) {
    const y = y0 - (avgWin / max) * bmax;
    ctx.strokeStyle = "rgba(232,220,180,.55)";
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(GUT, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = "#D9CDA9";
    ctx.fillText(`均值 ${toMbps(avgWin).toFixed(2)}`, GUT + 2, y - 3);
  }
}

async function poll() {
  try {
    const s = await api.burstSnapshot();
    renderKpi(s);
    drawSpark(s);
    updateStatic(normalizeWin(s.window_ms));
    if (s.running) {
      setState("running", `检测中 · ${Math.round(s.secs)}s · ${normalizeWin(s.window_ms)}ms`);
      const sum = $<HTMLElement>("b-sum");
      if (sum) {
        sum.innerHTML =
          `<span class="chip on">${s.mbps_avg.toFixed(2)} Mbps 平均</span>` +
          `<span class="chip">峰值 ${s.mbps_peak.toFixed(2)} Mbps</span>`;
      }
    } else if (s.packets > 0) {
      setState("idle", `已停止 · 共 ${s.packets} 包`);
      timerStop();
    }
  } catch {
    /* 忽略瞬时失败 */
  }
}

function timerStart() {
  if (timer !== null) return;
  timer = window.setInterval(poll, 1000);
}

function timerStop() {
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

async function start() {
  const url = ($<HTMLInputElement>("b-url")?.value || "").trim();
  if (!url) {
    setState("error", "组播地址为空");
    return;
  }
  const ratio = Number($<HTMLInputElement>("b-ratio")?.value || 2) || 2;
  const win = currentWin();
  updateStatic(win);
  setState("running", "绑定端口…");
  try {
    await api.burstStart(url, $<HTMLSelectElement>("b-nic")?.value || "", ratio, win);
    timerStart();
  } catch (e: any) {
    setState("error", String(e));
    timerStop();
  }
}

async function stop() {
  try {
    const s = await api.burstStop();
    timerStop();
    setState("idle", `已停止 · 共 ${s.packets} 包`);
    renderKpi(s);
    drawSpark(s);
    updateStatic(normalizeWin(s.window_ms));
  } catch (e: any) {
    setState("error", `停止失败：${e}`);
  }
}

export function initBurst() {
  const stopBtn = $<HTMLButtonElement>("b-stop");
  if (stopBtn) stopBtn.innerHTML = icon("square", 15);
  $<HTMLButtonElement>("b-act")?.addEventListener("click", start);
  stopBtn?.addEventListener("click", stop);

  const winSel = $<HTMLSelectElement>("b-window");
  const applyWin = (v: unknown) => {
    const win = normalizeWin(v);
    if (winSel) winSel.value = String(win);
    updateStatic(win);
    const idle = { ...EMPTY, window_ms: win };
    renderKpi(idle);
    drawSpark(idle);
  };
  applyWin(1);
  winSel?.addEventListener("change", () => {
    const win = currentWin();
    updateStatic(win);
    api.uiSet("burst.window_ms", win).catch(() => {});
  });

  api.listNics().then((rows) => {
    const sel = $<HTMLSelectElement>("b-nic");
    if (!sel) return;
    sel.innerHTML =
      `<option value="">自动</option>` +
      rows.map((n) => `<option value="${n.ip}">${nicLabel(n)}</option>`).join("");
  });

  // 恢复上次选择；没有记录时保持默认 1ms。
  api.uiGet("burst.window_ms").then((v) => applyWin(v)).catch(() => {});

  renderKpi(EMPTY);
  drawSpark(EMPTY);
  setState("idle", "待机");
}
