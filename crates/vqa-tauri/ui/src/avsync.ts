//! 音画同步页：频道列表（单检 / 轮巡 / 预览）+ 一次检测 → KPI + 双曲线 + 候选峰。

import { api, AvSyncResult, Channel } from "./api";
import { ChannelBoard, RunOutcome } from "./channels";
import { icon } from "./icons";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

let board: ChannelBoard | null = null;
let prevTimer = 0;
let prevKey = "";
let lastName = "";
let lastResult: AvSyncResult | null = null;
let chCollapsed = false;
const AV_CH_COLLAPSED_KEY = "avsync.ch.collapsed";

function setChannelCollapsed(on: boolean, persist = true) {
  chCollapsed = on;
  const body = document.getElementById("av-body");
  const rail = document.getElementById("av-ch-rail") as HTMLButtonElement | null;
  body?.classList.toggle("is-ch-collapsed", on);
  if (rail) rail.setAttribute("aria-expanded", String(!on));
  if (persist) api.uiSet(AV_CH_COLLAPSED_KEY, on).catch(() => {});
  window.setTimeout(() => drawCurve(lastResult), 30);
}

async function loadChannelCollapsed() {
  try {
    const v = await api.uiGet(AV_CH_COLLAPSED_KEY);
    setChannelCollapsed(!!v, false);
  } catch {
    /* 默认展开 */
  }
}

function setState(kind: "idle" | "running" | "error", text: string) {
  const dot = $<HTMLElement>("av-dot");
  if (dot) dot.className = `dot${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const st = $<HTMLElement>("av-status-text");
  if (st) st.textContent = text;
  const lamp = document.getElementById("lamp-avsync");
  if (lamp) lamp.className = `lamp${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const act = $<HTMLButtonElement>("av-act");
  if (act) act.disabled = kind === "running";
  const stop = $<HTMLButtonElement>("av-stop");
  if (stop) stop.hidden = kind !== "running";
}

function kpi(label: string, value: string, hint = "") {
  return (
    `<div class="kpi"><span class="kpi-l">${label}</span><b class="kpi-v">${value}</b>` +
    (hint ? `<span class="kpi-h">${hint}</span>` : "") +
    `</div>`
  );
}

function verdictOf(ms: number) {
  const a = Math.abs(ms);
  return a <= 40 ? "同步" : a <= 120 ? "轻微偏移" : "不同步";
}

function renderKpi(r: AvSyncResult | null, pct = 0) {
  const el = $<HTMLElement>("av-kpi");
  if (!el) return;
  if (!r) {
    el.innerHTML =
      kpi("音频偏移", "—") +
      kpi("相关度", "—") +
      kpi("判定", "待检测") +
      kpi("视频帧", "—") +
      kpi("音频窗口", "—");
    return;
  }
  const offCls = Math.abs(r.offset_ms) <= 40 ? "ok" : Math.abs(r.offset_ms) <= 120 ? "warn" : "bad";
  const confOk = r.confidence >= 0.25;
  el.innerHTML =
    kpi(
      "音频偏移",
      `${r.offset_ms > 0 ? "+" : ""}${r.offset_ms} ms`,
      r.offset_ms > 0 ? "音频滞后" : r.offset_ms < 0 ? "音频超前" : "完全对齐",
    ) +
    kpi(
      "相关度",
      r.confidence.toFixed(2),
      confOk ? `采样 ${r.fps.toFixed(0)} fps` : "偏低，结论仅供参考",
    ) +
    kpi(
      "判定",
      confOk
        ? `<span class="badge ${offCls}">${verdictOf(r.offset_ms)}</span>`
        : `<span class="badge warn">置信不足</span>`,
    ) +
    kpi("视频帧", r.video_frames.toLocaleString(), `≈ ${(r.video_frames / r.fps).toFixed(1)} s`) +
    kpi("音频窗口", r.audio_windows.toLocaleString(), pct > 0 && pct < 100 ? `进度 ${pct}%` : "40ms/窗");
}

function drawCurve(r: AvSyncResult | null) {
  const cv = $<HTMLCanvasElement>("av-curve");
  if (!cv) return;
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth || 900;
  const h = cv.clientHeight || 150;
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
  }
  const ctx = cv.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = "rgba(255,255,255,.03)";
  ctx.fillRect(0, 0, w, h);
  const empty = document.getElementById("av-empty") as HTMLElement | null;
  if (!r || r.series_video.length < 4) {
    if (empty) empty.hidden = false;
    return;
  }
  if (empty) empty.hidden = true;
  const GUT = 34;
  const PAD_R = 10;
  const TOP = 10;
  const BOT = h - 20;
  const plotW = w - GUT - PAD_R;
  const plotH = BOT - TOP;
  const norm = (arr: number[]) => {
    const m = Math.max(0.0001, ...arr.map((x) => Math.abs(x)));
    return arr.map((x) => Math.abs(x) / m);
  };
  const sv = norm(r.series_video);
  const sa = norm(r.series_audio);
  const n = Math.max(sv.length, sa.length);
  const barW = plotW / Math.max(1, n - 1);
  const totalS = n / (r.fps || 25);
  ctx.strokeStyle = "rgba(255,255,255,.08)";
  for (let i = 0; i <= 4; i++) {
    const y = TOP + (plotH * i) / 4;
    ctx.beginPath();
    ctx.moveTo(GUT, y);
    ctx.lineTo(w - PAD_R, y);
    ctx.stroke();
  }
  ctx.font = "10px Consolas, monospace";
  ctx.fillStyle = "#CBBDA4";
  ctx.textAlign = "right";
  ctx.fillText("1.0", GUT - 4, TOP + 4);
  ctx.fillText("0.5", GUT - 4, TOP + plotH / 2 + 4);
  ctx.fillText("0", GUT - 4, BOT + 4);
  ctx.textAlign = "center";
  for (let i = 0; i <= 4; i++) {
    const t = (totalS * i) / 4;
    ctx.fillText(`${t.toFixed(1)}s`, GUT + (plotW * i) / 4, h - 6);
  }
  ctx.textAlign = "left";
  const drawLine = (arr: number[], color: string) => {
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.4;
    ctx.beginPath();
    for (let i = 0; i < arr.length; i++) {
      const x = GUT + i * barW;
      const y = BOT - arr[i] * plotH;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();
  };
  drawLine(sv, "rgba(168,212,189,.9)");
  drawLine(sa, "rgba(201,147,43,.9)");
  const offText = `偏移 ${r.offset_ms > 0 ? "+" : ""}${r.offset_ms} ms`;
  ctx.fillStyle = "#D9CDA9";
  ctx.font = "11px Consolas, monospace";
  ctx.fillText(offText, GUT + 4, 14);
}

function renderPeaks(r: AvSyncResult | null) {
  const el = $<HTMLElement>("av-peaks");
  if (!el) return;
  if (!r || !r.peaks.length) {
    el.innerHTML = "";
    return;
  }
  const rows = r.peaks
    .map(
      (p, i) =>
        `<span class="peak${i === 0 ? " on" : ""}">${i + 1}. ${p.offset_ms > 0 ? "+" : ""}${p.offset_ms} ms · 相关 ${p.corr.toFixed(2)}</span>`,
    )
    .join("");
  el.innerHTML = `<span class="peaks-l">候选峰</span>${rows}<span class="peaks-h">峰差越小说明偏移越不唯一，可加长样本复核</span>`;
}

/** 跑一条来源，直到后端落地结果；focus=false 时只跑不刷页面指标（并发从属任务）。 */
async function runOnce(src: string, job = "main", focus = true): Promise<AvSyncResult | null> {
  const secs = Number($<HTMLInputElement>("av-secs")?.value || 20) || 20;
  const search = Number($<HTMLInputElement>("av-search")?.value || 800) || 800;
  const st = (k: "idle" | "running" | "error", t: string) => focus && setState(k, t);
  if (focus) {
    renderKpi(null, 0);
    drawCurve(null);
    renderPeaks(null);
    lastResult = null;
  }
  st("running", "启动中…");
  try {
    await api.avsyncStart({ source: src, secs, search_ms: search, fps: 25 }, job);
  } catch (e: any) {
    st("error", String(e));
    return null;
  }
  let guard = 0;
  for (;;) {
    await sleep(600);
    let p;
    try {
      p = await api.avsyncProgress(job);
    } catch {
      continue;
    }
    if (p.running) {
      st("running", `分析中 · ${p.pct}%`);
      if (focus) renderKpi(p.result, p.pct);
      if (++guard > 600) {
        st("error", "等待结果超时");
        return null;
      }
      continue;
    }
    if (p.result) {
      const r = p.result;
      if (r.ok) st("idle", "检测完成");
      else st("error", r.err || "检测失败");
      if (focus) {
        lastResult = r;
        renderKpi(r, 0);
        drawCurve(r);
        renderPeaks(r);
      }
      return r;
    }
    if (p.err) {
      st("error", p.err);
      return null;
    }
    await sleep(400);
  }
}

function previewChannel(ch: Channel) {
  if (document.querySelector<HTMLElement>('.pg[data-page="avsync"]')?.hidden) return;
  lastName = ch.name || "频道";
  const tag = $<HTMLElement>("av-prevtag");
  if (tag) tag.textContent = `${ch.name || "频道"} · ${ch.url}`;
  api.previewStart("av", ch.url).catch(() => {});
  if (prevTimer) return;
  prevTimer = window.setInterval(async () => {
    try {
      const f = await api.previewFrame("av");
      const img = $<HTMLImageElement>("av-prev");
      if (f && img && f !== prevKey) {
        img.src = f;
        prevKey = f;
      }
    } catch {
      /* 忽略抓帧失败 */
    }
  }, 400);
}

function stopPreview() {
  if (prevTimer) {
    window.clearInterval(prevTimer);
    prevTimer = 0;
  }
  prevKey = "";
  const tag = $<HTMLElement>("av-prevtag");
  if (tag) tag.textContent = lastName ? `${lastName} · 已停止` : "无预览";
  api.previewStop("av").catch(() => {});
}

/** 单个检测（手填来源走这里）。 */
async function startManual() {
  const src = ($<HTMLInputElement>("av-src")?.value || "").trim();
  if (!src) {
    setState("error", "来源为空");
    return;
  }
  previewChannel({ name: "手动来源", url: src, enabled: true });
  const r = await runOnce(src, "main");
  if (!r || !r.ok) stopPreview();
}

async function stop() {
  try {
    await api.avsyncStop("all");
  } catch {}
}

// ── 本页设置（时长 / 搜索），存 shared 配置 ──
interface AvPrefs {
  secs: number;
  search: number;
}

const AV_PREF_KEY = "avsync.prefs";
let saveTimer = 0;

function readPrefs(): AvPrefs {
  return {
    secs: Number($<HTMLInputElement>("av-secs")?.value || 20) || 20,
    search: Number($<HTMLInputElement>("av-search")?.value || 800) || 800,
  };
}

function queueSave() {
  if (saveTimer) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    api.uiSet(AV_PREF_KEY, readPrefs()).catch(() => {});
  }, 300);
}

async function loadPrefs() {
  try {
    const p: any = await api.uiGet(AV_PREF_KEY);
    if (!p || typeof p !== "object") return;
    const secs = $<HTMLInputElement>("av-secs");
    const search = $<HTMLInputElement>("av-search");
    if (secs && Number.isFinite(p.secs)) secs.value = String(p.secs);
    if (search && Number.isFinite(p.search)) search.value = String(p.search);
  } catch {
    /* 设置读不到就用默认值 */
  }
}

export function initAvSync() {
  const b = $<HTMLButtonElement>("av-browse");
  if (b) {
    b.innerHTML = `${icon("folder", 15)}<span class="btxt">浏览</span>`;
    b.addEventListener("click", async () => {
      const p = await api.browse();
      if (p && $<HTMLInputElement>("av-src")) $<HTMLInputElement>("av-src")!.value = p;
    });
  }
  $<HTMLButtonElement>("av-act")?.addEventListener("click", startManual);
  const s = $<HTMLButtonElement>("av-stop");
  if (s) s.innerHTML = `${icon("square", 15)}<span class="btxt">停止检测</span>`;
  s?.addEventListener("click", stop);
  ["av-secs", "av-search"].forEach((id) =>
    $<HTMLInputElement>(id)?.addEventListener("change", queueSave),
  );

  const host = $<HTMLElement>("av-chboard");
  if (host) {
    board = new ChannelBoard(host, {
      owner: "av",
      title: "频道列表",
      sequential: true,
      compact: true,
      onCollapse: () => setChannelCollapsed(true),
      runOne: async (ch, job, focus): Promise<RunOutcome> => {
        if (focus) {
          const input = $<HTMLInputElement>("av-src");
          if (input) input.value = ch.url;
        }
        const r = await runOnce(ch.url, job, focus);
        if (!r) return { ok: false, summary: "失败" };
        if (!r.ok) {
          const msg = r.err || "失败";
          return { ok: false, warn: true, summary: msg.includes("取消") ? "已停止" : msg };
        }
        const bad = r.confidence < 0.25 || Math.abs(r.offset_ms) > 40;
        return {
          ok: !bad,
          warn: bad,
          summary:
            r.confidence < 0.25
              ? `相关度 ${r.confidence.toFixed(2)} · 置信不足`
              : `${r.offset_ms > 0 ? "+" : ""}${r.offset_ms} ms · ${verdictOf(r.offset_ms)}`,
        };
      },
      stopRun: () => {
        api.avsyncStop("all").catch(() => {});
      },
      preview: previewChannel,
      stopPreview,
    });
    board.load();
    loadPrefs();
    loadChannelCollapsed();
  }

  $<HTMLButtonElement>("av-ch-rail")?.addEventListener("click", () =>
    setChannelCollapsed(false),
  );
  window.addEventListener("page-changed", (ev) => {
    if ((ev as CustomEvent).detail === "avsync") {
      window.setTimeout(() => drawCurve(lastResult), 30);
    }
  });

  renderKpi(null, 0);
  drawCurve(null);
  setState("idle", "待机");
}

/** 离开页面时释放预览进程（后台检测任务本身不受影响）。 */
export function leaveAvSync() {
  stopPreview();
}
