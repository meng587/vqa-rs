//! 直播转码页：起 ffmpeg 常态转码 → 每秒轮询统计（帧数 / fps / 输出码率 / 速度 / 丢帧）。

import { api } from "./api";
import { icon } from "./icons";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;

const inEl = () => $<HTMLInputElement>("x-in");
const outEl = () => $<HTMLInputElement>("x-out");
const encEl = () => $<HTMLSelectElement>("x-enc");
const brEl = () => $<HTMLInputElement>("x-br");
const audioEl = () => $<HTMLSelectElement>("x-audio");
const hEl = () => $<HTMLSelectElement>("x-h");
const reEl = () => $<HTMLInputElement>("x-re");
const actEl = () => $<HTMLButtonElement>("x-act");
const stopEl = () => $<HTMLButtonElement>("x-stop");
const dotEl = () => $<HTMLElement>("x-dot");
const stEl = () => $<HTMLElement>("x-status-text");
const sumEl = () => $<HTMLElement>("x-sum");
const kpiEl = () => $<HTMLElement>("x-kpi");
const logEl = () => $<HTMLElement>("x-log");

let timer: number | null = null;
let lastCmd = "";

function setState(kind: "idle" | "running" | "error", text: string) {
  const dot = dotEl();
  if (dot) dot.className = `dot${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const st = stEl();
  if (st) st.textContent = text;
  const lamp = document.getElementById("lamp-transcode");
  if (lamp) lamp.className = `lamp${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const act = actEl();
  if (act) act.disabled = kind === "running";
  const stop = stopEl();
  if (stop) stop.hidden = kind !== "running";
}

function kpiCard(label: string, value: string) {
  return `<div class="kpi"><span class="kpi-l">${label}</span><b class="kpi-v">${value}</b></div>`;
}

function renderKpi(s: {
  frame: number;
  fps: number;
  kbps: number;
  speed: number;
  drop: number;
  secs: number;
  out_time: string;
}) {
  const el = kpiEl();
  if (!el) return;
  el.innerHTML =
    kpiCard("已编帧数", s.frame.toLocaleString()) +
    kpiCard("编码帧率", `${(s.fps > 0 ? s.fps : s.secs > 0 && s.frame > 0 ? s.frame / s.secs : 0).toFixed(1)} fps`) +
    kpiCard("输出码率", `${(s.kbps / 1000).toFixed(2)} Mbps`) +
    kpiCard("转码速度", `${s.speed.toFixed(2)}×`) +
    kpiCard("丢帧", String(s.drop)) +
    kpiCard("已运行", `${Math.round(s.secs)} s`);
}

async function poll() {
  try {
    const s = await api.xcodeSnapshot();
    const cmdChanged = s.cmd && s.cmd !== lastCmd;
    if (cmdChanged) {
      lastCmd = s.cmd;
      const log = logEl();
      if (log) log.textContent = s.cmd;
    }
    if (s.running) {
      setState("running", `转码中 · ${s.out_time || "启动中"}`);
      const sum = sumEl();
      if (sum) {
        sum.innerHTML = `<span class="chip on">编码 ${(s.kbps / 1000).toFixed(2)} Mbps</span>`;
      }
      renderKpi(s);
    } else if (s.frame > 0 || s.kbps > 0) {
      setState("idle", s.err ? `已结束：${s.err}` : "转码结束");
      renderKpi(s);
      timerStop();
    } else if (s.err) {
      setState("error", s.err);
      timerStop();
    }
    const log = logEl();
    if (log && s.log && s.log.length) {
      const tail = s.log.slice(-24).join("\n");
      if (!log.textContent!.startsWith(tail)) log.textContent = tail;
    }
  } catch {
    /* 轮询失败不打扰 */
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
  const input = (inEl()?.value || "").trim();
  const output = (outEl()?.value || "").trim();
  if (!input || !output) {
    setState("error", "来源和输出都要填");
    return;
  }
  const br = Number(brEl()?.value || 8000) || 8000;
  const h = Number(hEl()?.value || 0);
  setState("running", "启动中…");
  try {
    await api.xcodeStart({
      input,
      output,
      encoder: encEl()?.value || "auto",
      vbr_kbps: Math.max(200, Math.round(br)),
      audio: audioEl()?.value || "copy",
      realtime: !!reEl()?.checked,
      height: h > 0 ? h : null,
      fps: null,
    });
    const log = logEl();
    if (log) log.textContent = "已启动，等待 ffmpeg 输出统计…";
    timerStart();
  } catch (e: any) {
    setState("error", String(e));
    timerStop();
  }
}

async function stop() {
  try {
    const s = await api.xcodeStop();
    timerStop();
    setState("idle", "已停止");
    renderKpi(s);
  } catch (e: any) {
    setState("error", `停止失败：${e}`);
  }
}

export function initXcode() {
  const browse = $<HTMLButtonElement>("x-browse");
  if (browse) {
    browse.innerHTML = icon("folder", 15);
    browse.addEventListener("click", async () => {
      const p = await api.browse();
      if (p && inEl()) {
        inEl()!.value = p;
        if (!outEl()?.value) {
          outEl()!.value = p.replace(/\.(ts|mp4|mkv|mov)$/i, "") + "-out.ts";
        }
      }
    });
  }
  actEl()?.addEventListener("click", start);
  stopEl()?.addEventListener("click", stop);

  api.xcodeEncoders().then((r) => {
    const sel = encEl();
    if (!sel) return;
    sel.innerHTML =
      `<option value="auto">${r.auto_label}</option>` +
      r.encoders.map((e) => `<option value="${e.id}">${e.label}</option>`).join("");
  });

  renderKpi({ frame: 0, fps: 0, kbps: 0, speed: 0, drop: 0, secs: 0, out_time: "" });
  setState("idle", "待机");
}
