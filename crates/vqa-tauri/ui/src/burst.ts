//! 微突发巡检页：收一路组播，按 1ms 桶看到达量，画最近 240ms 柱状图。

import { api, BurstStats } from "./api";
import { icon } from "./icons";


const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;

/** 网卡下拉文案：`IP · 网卡名`（与播放器/合规一致） */
function nicLabel(n: { ip: string; name: string; desc: string; up: boolean }): string {
  const nm = (n.name || n.desc || "").trim();
  const base = nm ? `${n.ip} · ${nm}` : n.ip;
  return n.up ? base : `${base}（未连接）`;
}

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
  series: [],
  url: "",
  err: null,
};

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
  el.innerHTML =
    kpiCard("平均码率", `${s.mbps_avg.toFixed(2)} Mbps`, `${Math.round(s.secs)}s 窗口`) +
    kpiCard("峰值瞬时", `${s.mbps_peak.toFixed(2)} Mbps`, "单个 1ms 桶") +
    kpiCard("峰值/均值", `${s.peak_ratio.toFixed(1)} ×`, `${s.ms_peak_bytes} B/ms`) +
    kpiCard("微突发次数", `${s.burst_cnt} 次`, "超过阈值即计") +
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
  ctx.fillStyle = "rgba(255,255,255,.03)";
  ctx.fillRect(0, 0, w, h);

  // Y 轴刻度：max / max/2 / 0，标成瞬时 Mbps（188B/ms ≈ 1.5Mbps）
  const toMbps = (b: number) => (b * 8) / 1000;
  ctx.font = '9.5px Consolas, monospace';
  ctx.textAlign = 'right';
  ctx.fillStyle = 'rgba(255,255,255,.40)';
  ctx.strokeStyle = 'rgba(255,255,255,.10)';
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
  const thr = s.bytes_per_ms_avg * ratio;
  for (let i = 0; i < series.length; i++) {
    const bh = Math.max(1, (series[i] / max) * bmax);
    ctx.fillStyle = thr > 0 && series[i] >= thr ? "rgba(248,113,113,.9)" : "rgba(129,140,248,.75)";
    ctx.fillRect(GUT + i * bw + 0.5, y0 - bh, Math.max(1, bw - 1), bh);
  }
  if (s.bytes_per_ms_avg > 0) {
    const y = y0 - (s.bytes_per_ms_avg / max) * bmax;
    ctx.strokeStyle = "rgba(255,255,255,.28)";
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(GUT, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = "rgba(255,255,255,.45)";
    ctx.fillText(`均值 ${toMbps(s.bytes_per_ms_avg).toFixed(2)}`, GUT + 2, y - 3);
  }
}

async function poll() {
  try {
    const s = await api.burstSnapshot();
    renderKpi(s);
    drawSpark(s);
    if (s.running) {
      setState("running", `巡检中 · ${Math.round(s.secs)}s · ${s.url}`);
      const sum = $<HTMLElement>("b-sum");
      if (sum) sum.innerHTML = `<span class="chip on">${s.mbps_avg.toFixed(2)} Mbps 平均</span>`;
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
  setState("running", "绑定端口…");
  try {
    await api.burstStart(url, $<HTMLSelectElement>("b-nic")?.value || "", ratio);
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
  } catch (e: any) {
    setState("error", `停止失败：${e}`);
  }
}

export function initBurst() {
  const stopBtn = $<HTMLButtonElement>("b-stop");
  if (stopBtn) stopBtn.innerHTML = icon("square", 15);
  $<HTMLButtonElement>("b-act")?.addEventListener("click", start);
  stopBtn?.addEventListener("click", stop);
  api.listNics().then((rows) => {
    const sel = $<HTMLSelectElement>("b-nic");
    if (!sel) return;
    sel.innerHTML =
      `<option value="">自动</option>` +
      rows.map((n) => `<option value="${n.ip}">${nicLabel(n)}</option>`).join("");
  });
  renderKpi(EMPTY);
  drawSpark(EMPTY);
  setState("idle", "待机");
}
