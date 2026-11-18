//! 直播转码页：设置子页（输入 / 转码 / 输出）→ 按实际配置启动 ffmpeg →
//! 阶段条与统计轮询 → 正常结束 / 失败 / 手动停止。

import { api, type XcodeCfg, type XcodeStats } from "./api";
import { icon } from "./icons";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;

const inEl = () => $<HTMLInputElement>("x-in");
const outEl = () => $<HTMLInputElement>("x-out");
const encEl = () => $<HTMLSelectElement>("x-enc");
const brEl = () => $<HTMLInputElement>("x-br");
const audioEl = () => $<HTMLSelectElement>("x-audio");
const hEl = () => $<HTMLSelectElement>("x-h");
const fpsEl = () => $<HTMLInputElement>("x-fps");
const gopEl = () => $<HTMLInputElement>("x-gop");
const audioBrEl = () => $<HTMLInputElement>("x-audio-br");
const nicEl = () => $<HTMLSelectElement>("x-nic");
const containerEl = () => $<HTMLSelectElement>("x-container");
const pktEl = () => $<HTMLInputElement>("x-pkt");
const ttlEl = () => $<HTMLInputElement>("x-ttl");
const reEl = () => $<HTMLInputElement>("x-re");
const passEl = () => $<HTMLInputElement>("x-pass");
const actEl = () => $<HTMLButtonElement>("x-act");
const applyEl = () => $<HTMLButtonElement>("x-apply");
const stopEl = () => $<HTMLButtonElement>("x-stop");
const dotEl = () => $<HTMLElement>("x-dot");
const stEl = () => $<HTMLElement>("x-status-text");
const stageEl = () => $<HTMLElement>("x-stage");
const titleEl = () => $<HTMLElement>("x-run-title");
const sumEl = () => $<HTMLElement>("x-summary");
const statChipsEl = () => $<HTMLElement>("x-sum");
const flowEl = () => $<HTMLElement>("x-flow");
const kpiEl = () => $<HTMLElement>("x-kpi");
const logEl = () => $<HTMLElement>("x-log");
const cmdMainEl = () => $<HTMLElement>("x-cmd-main");
const cmdActualEl = () => $<HTMLElement>("x-enc-actual");
const monitorEl = () => $<HTMLElement>("x-monitor");
const setupEl = () => $<HTMLElement>("x-setup");
const previewEl = () => $<HTMLElement>("x-cmd-preview");
const previewEncEl = () => $<HTMLElement>("x-preview-enc");
const setupMsgEl = () => $<HTMLElement>("x-setup-msg");

type SetupPage = "in" | "code" | "out";
let timer: number | null = null;
let lastCmd = "";
let encLabel = "自动";
let currentPage: SetupPage = "in";
let passApplied = false;
let prevEncBeforePass = "auto";
let prevAudioBeforePass = "copy";
const encoderLabels = new Map<string, string>([["auto", "自动"], ["copy", "透传（不重编码）"]]);
const pageLabels: Record<SetupPage, string> = {
  in: "输入参数",
  code: "转码参数",
  out: "输出参数",
};
const pageOrder: SetupPage[] = ["in", "code", "out"];

const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] || c));

const short = (s: string, n = 46) => {
  const t = s.trim();
  return t.length > n ? `${t.slice(0, n - 1)}…` : t;
};

const numOrNull = (v: string): number | null => {
  const t = v.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
};

function makeCfg(): XcodeCfg {
  const height = Number(hEl()?.value || 0);
  const fps = numOrNull(fpsEl()?.value || "");
  const pass = !!passEl()?.checked;
  return {
    input: (inEl()?.value || "").trim(),
    output: (outEl()?.value || "").trim(),
    encoder: pass ? "copy" : encEl()?.value || "auto",
    vbr_kbps: Math.max(200, Math.round(Number(brEl()?.value || 8000) || 8000)),
    audio: pass ? "copy" : audioEl()?.value || "copy",
    realtime: !!reEl()?.checked,
    height: height > 0 ? height : null,
    fps: fps && fps > 0 ? fps : null,
    nic_ip: (nicEl()?.value || "").trim() || null,
    gop: numOrNull(gopEl()?.value || "") ?? 25,
    audio_kbps: numOrNull(audioBrEl()?.value || "") ?? 128,
    container: containerEl()?.value || "auto",
    pkt_size: numOrNull(pktEl()?.value || "") ?? 1316,
    ttl: numOrNull(ttlEl()?.value || "") ?? 0,
  };
}

function setState(kind: "idle" | "running" | "error", text: string, stage?: string) {
  const dot = dotEl();
  if (dot) dot.className = `dot${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const st = stEl();
  if (st) st.textContent = text;
  const lamp = document.getElementById("lamp-transcode");
  if (lamp) lamp.className = `lamp${kind === "running" ? " run" : kind === "error" ? " err" : ""}`;
  const stageEl2 = stageEl();
  if (stageEl2) {
    stageEl2.textContent = stage || (kind === "running" ? "运行中" : kind === "error" ? "失败" : "待机");
    stageEl2.className = `x-stage${kind === "running" ? " run" : kind === "error" ? " err" : stage === "已完成" ? " done" : ""}`;
  }
  const act = actEl();
  if (act) act.disabled = kind === "running";
  const apply = applyEl();
  if (apply) apply.disabled = kind === "running";
  const stop = stopEl();
  if (stop) stop.hidden = kind !== "running";
}

function kpiCard(label: string, value: string, hint = "") {
  return `<div class="kpi"><span class="kpi-l">${label}</span><b class="kpi-v">${value}</b>${hint ? `<span class="kpi-h">${hint}</span>` : ""}</div>`;
}

function renderKpi(s: Pick<XcodeStats, "frame" | "fps" | "kbps" | "speed" | "drop" | "secs" | "out_time">) {
  const el = kpiEl();
  if (!el) return;
  const fps = s.fps > 0 ? s.fps : s.secs > 0 && s.frame > 0 ? s.frame / s.secs : 0;
  el.innerHTML =
    kpiCard("已编帧数", s.frame.toLocaleString()) +
    kpiCard("编码帧率", `${fps.toFixed(1)} fps`) +
    kpiCard("输出码率", `${(s.kbps / 1000).toFixed(2)} Mbps`) +
    kpiCard("转码速度", `${s.speed.toFixed(2)}×`) +
    kpiCard("丢帧", String(s.drop)) +
    kpiCard("已运行", `${Math.round(s.secs)} s`, s.out_time ? `输出时间 ${s.out_time}` : "");
}

function summarizeCfg(cfg: XcodeCfg) {
  const passthrough = cfg.encoder === "copy";
  const bits = [
    ["来源", cfg.input ? short(cfg.input, 38) : "未配置"],
    ["输出", cfg.output ? short(cfg.output, 38) : "未配置"],
    ["编码", passthrough ? "透传 · 原编码" : `${encoderLabels.get(cfg.encoder) || cfg.encoder} · ${cfg.vbr_kbps} kbps`],
    ["画面", passthrough ? "原分辨率 · 原帧率" : `${cfg.height ? `${cfg.height}p` : "原分辨率"} · ${cfg.fps ? `${cfg.fps}fps` : "源帧率"}`],
    ["音频", cfg.audio === "copy" ? "直通" : cfg.audio === "aac" ? `AAC ${cfg.audio_kbps || 128}k` : "无音频"],
  ];
  const sum = sumEl();
  if (sum) {
    sum.innerHTML = bits
      .map(([k, v]) => `<span class="x-chip"><span>${esc(k)}</span><b title="${esc(v)}">${esc(v)}</b></span>`)
      .join("");
  }
  const title = titleEl();
  if (title) title.textContent = cfg.input ? short(cfg.input, 34) : "尚未配置";
}

function renderFlow(s?: XcodeStats, cfg?: XcodeCfg) {
  const el = flowEl();
  if (!el) return;
  const c = cfg || makeCfg();
  const running = !!s?.running;
  const failed = !!s?.err || s?.stage === "失败";
  const ended = !!s && !s.running && (s.frame > 0 || s.kbps > 0 || ["已完成", "已停止", "失败", "已结束"].includes(s.stage));
  const hasStarted = running || ended;
  const inputDone = hasStarted || (s?.frame || 0) > 0;
  const encDone = !!s && ((s.frame || 0) > 0 || (s.fps || 0) > 0 || (!running && !!s.cmd && ended));
  const outDone = !!s && ((s.kbps || 0) > 0 || (!running && ended));
  const outActive = running && !outDone;
  const encActive = running && !encDone;
  const resultText = failed
    ? (s?.err || "转码失败")
    : running
      ? `运行 ${Math.round(s?.secs || 0)} s`
      : ended
        ? (s?.stage === "已停止" ? "已手动停止" : `结束 ${Math.round(s?.secs || 0)} s`)
        : "等待开始";
  const steps = [
    {
      cls: failed ? "err" : inputDone ? "done" : c.input ? "ready" : "",
      title: "输入读取",
      desc: c.input ? short(c.input, 30) : "未配置来源",
    },
    {
      cls: failed ? "err" : encDone ? "done" : encActive ? "on" : hasStarted ? "on" : "",
      title: "编码转码",
      desc:
        c.encoder === "copy"
          ? "透传 · 不重编码"
          : `${encoderLabels.get(c.encoder) || c.encoder} · ${c.vbr_kbps}k · ${c.height ? `${c.height}p` : "原分辨率"}`,
    },
    {
      cls: failed ? "err" : outDone ? "done" : outActive ? "on" : hasStarted ? "on" : "",
      title: "封装输出",
      desc: c.output ? `${c.container === "auto" ? "自动封装" : String(c.container || "auto").toUpperCase()} · ${short(c.output, 26)}` : "未配置输出",
    },
    {
      cls: failed ? "err" : ended ? "done" : running ? "on" : "",
      title: "运行结果",
      desc: resultText,
    },
  ];
  el.innerHTML = steps
    .map(
      (x) =>
        `<div class="x-flow-step ${x.cls}"><div class="x-fh"><i></i><b>${esc(x.title)}</b></div><p title="${esc(x.desc)}">${esc(x.desc)}</p></div>`,
    )
    .join("");
}

function renderStatChips(s: XcodeStats) {
  const sum = statChipsEl();
  if (!sum) return;
  const chips: string[] = [];
  if (s.running) chips.push(`输出 ${(s.kbps / 1000).toFixed(2)} Mbps`);
  if (s.stage) chips.push(s.stage);
  if (s.encoder) chips.push(`编码器 ${encoderLabels.get(s.encoder) || s.encoder}`);
  sum.innerHTML = chips.map((x) => `<span class="chip on">${esc(x)}</span>`).join("");
}

function setPreview(text: string, enc?: string) {
  const pre = previewEl();
  if (pre) pre.textContent = text;
  const pe = previewEncEl();
  if (pe) pe.textContent = enc ? `实际编码器：${encoderLabels.get(enc) || enc}` : "未解析";
}

function setSetupMsg(text?: string) {
  const el = setupMsgEl();
  if (!el) return;
  el.hidden = !text;
  el.textContent = text || "";
}

function showPage(page: SetupPage) {
  currentPage = page;
  document.querySelectorAll<HTMLElement>(".x-page[data-xpage]").forEach((el) => {
    const active = el.dataset.xpage === page;
    el.hidden = !active;
    el.classList.toggle("on", active);
  });
  document.querySelectorAll<HTMLElement>(".x-tab[data-xpage]").forEach((el) => {
    el.classList.toggle("on", el.dataset.xpage === page);
  });
  const label = $<HTMLElement>("x-page-label");
  if (label) label.textContent = `${pageLabels[page]} · ${pageOrder.indexOf(page) + 1} / ${pageOrder.length}`;
  const prev = $<HTMLButtonElement>("x-prev");
  const next = $<HTMLButtonElement>("x-next");
  if (prev) prev.disabled = pageOrder.indexOf(page) === 0;
  if (next) next.hidden = pageOrder.indexOf(page) === pageOrder.length - 1;
  summarizeCfg(makeCfg());
}

function showSetup(page: SetupPage = "in") {
  const m = monitorEl();
  const s = setupEl();
  if (m) m.hidden = true;
  if (s) s.hidden = false;
  showPage(page);
  syncPassUi();
  syncAudioUi();
}

function showMonitor() {
  const m = monitorEl();
  const s = setupEl();
  if (m) m.hidden = false;
  if (s) s.hidden = true;
}

function ensureCopyOption(on: boolean) {
  const sel = encEl();
  if (!sel) return;
  const has = Array.from(sel.options).some((o) => o.value === "copy");
  if (on && !has) {
    const o = document.createElement("option");
    o.value = "copy";
    o.textContent = "透传（不重编码）";
    sel.appendChild(o);
  } else if (!on && has) {
    const o = Array.from(sel.options).find((x) => x.value === "copy");
    o?.remove();
  }
}

function syncPassUi() {
  const pass = !!passEl()?.checked;
  const note = $<HTMLElement>("x-pass-note");
  if (note) note.hidden = !pass;
  if (pass) {
    if (!passApplied) {
      prevEncBeforePass = encEl()?.value || "auto";
      prevAudioBeforePass = audioEl()?.value || "copy";
      passApplied = true;
    }
    ensureCopyOption(true);
    if (encEl()) encEl()!.value = "copy";
    if (audioEl()) audioEl()!.value = "copy";
  } else if (passApplied) {
    ensureCopyOption(false);
    if (encEl()) encEl()!.value = prevEncBeforePass;
    if (audioEl()) audioEl()!.value = prevAudioBeforePass;
    passApplied = false;
  }
  for (const el of [encEl(), brEl(), hEl(), fpsEl(), gopEl(), audioEl(), audioBrEl()]) {
    if (!el) continue;
    el.disabled = pass;
    el.style.opacity = pass ? ".45" : "1";
  }
}

function syncAudioUi() {
  const pass = !!passEl()?.checked;
  const a = pass ? "copy" : audioEl()?.value || "copy";
  if (pass && audioEl()) audioEl()!.value = "copy";
  const br = audioBrEl();
  if (br) {
    br.disabled = pass || a !== "aac";
    br.style.opacity = pass || a !== "aac" ? ".45" : "1";
  }
}

function validate(cfg: XcodeCfg): string | null {
  if (!cfg.input) return "请先在「输入参数」填写来源地址或本地文件";
  if (!cfg.output) return "请先在「输出参数」填写输出地址或文件";
  const liveOut = /^(udp|rtp):\/\//i.test(cfg.output);
  if (liveOut && String(cfg.container).toLowerCase() === "mp4") {
    return "组播输出不能使用 MP4 封装，请在「输出参数」选择 MPEG-TS 或自动";
  }
  if (cfg.encoder === "copy" && cfg.audio === "aac") {
    return "透传模式下音频只能直通或无音频，请在「转码参数」调整";
  }
  return null;
}

function issuePage(cfg: XcodeCfg): SetupPage {
  if (!cfg.input) return "in";
  if (!cfg.output) return "out";
  if (/^(udp|rtp):\/\//i.test(cfg.output) && String(cfg.container).toLowerCase() === "mp4") return "out";
  return "code";
}

async function poll() {
  try {
    const s = await api.xcodeSnapshot();
    if (s.cmd && s.cmd !== lastCmd) {
      lastCmd = s.cmd;
      const cmd = cmdMainEl();
      if (cmd) cmd.textContent = s.cmd;
      const act = cmdActualEl();
      if (act) act.textContent = s.encoder ? `· ${encoderLabels.get(s.encoder) || s.encoder}` : "";
    }
    if (s.running) {
      setState("running", `转码中 · ${s.out_time || "等待首帧"}`, s.stage || "运行中");
      renderKpi(s);
      renderStatChips(s);
      renderFlow(s);
    } else if (s.stage === "已完成" || s.stage === "已停止" || s.stage === "失败" || s.err || s.frame > 0 || s.kbps > 0) {
      if (s.stage === "失败" || s.err) {
        setState("error", s.err || "转码失败", "失败");
      } else if (s.stage === "已停止") {
        setState("idle", "已停止", "已停止");
      } else {
        setState("idle", "转码完成", "已完成");
      }
      renderKpi(s);
      renderStatChips(s);
      renderFlow(s);
      timerStop();
    }
    const log = logEl();
    if (log && s.log && s.log.length) {
      const tail = s.log.slice(-30).join("\n");
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

async function start(): Promise<boolean> {
  const cfg = makeCfg();
  const err = validate(cfg);
  if (err) {
    setState("error", err);
    showSetup(issuePage(cfg));
    setSetupMsg(err);
    setPreview(err);
    return false;
  }
  setSetupMsg();
  setState("running", "启动中…", "启动中");
  renderFlow(undefined, cfg);
  try {
    const snap = await api.xcodeStart(cfg);
    showMonitor();
    lastCmd = snap.cmd || "";
    const cmd = cmdMainEl();
    if (cmd && snap.cmd) cmd.textContent = snap.cmd;
    const enc = cmdActualEl();
    if (enc && snap.encoder) enc.textContent = `· ${encoderLabels.get(snap.encoder) || snap.encoder}`;
    const log = logEl();
    if (log) log.textContent = "已启动，等待 ffmpeg 输出统计…";
    setState("running", `转码中 · ${snap.stage || "启动中"}`, "运行中");
    renderKpi(snap);
    renderStatChips(snap);
    renderFlow(snap, cfg);
    timerStart();
    poll();
    return true;
  } catch (e: any) {
    const msg = String(e?.message || e || "启动失败");
    setState("error", msg);
    showSetup(issuePage(cfg));
    setSetupMsg(msg);
    setPreview(`启动失败：${msg}`);
    timerStop();
    return false;
  }
}

async function stop() {
  try {
    const s = await api.xcodeStop();
    timerStop();
    setState("idle", "已停止", "已停止");
    renderKpi(s);
    renderStatChips(s);
    renderFlow(s);
    const log = logEl();
    if (log) log.textContent = "已手动停止。";
  } catch (e: any) {
    setState("error", `停止失败：${e}`);
  }
}

async function previewCommand() {
  const cfg = makeCfg();
  const err = validate(cfg);
  if (err) {
    showSetup(issuePage(cfg));
    setPreview(err);
    return;
  }
  const btn = $<HTMLButtonElement>("x-preview");
  if (btn) btn.disabled = true;
  setPreview("正在解析实际编码器并生成命令…");
  try {
    const r = await api.xcodePreview(cfg);
    setPreview(r.cmd, r.encoder);
  } catch (e: any) {
    setPreview(`预览失败：${String(e?.message || e)}`);
  } finally {
    if (btn) btn.disabled = false;
  }
}

function bindFieldUpdates() {
  const ids = [
    "x-in",
    "x-out",
    "x-enc",
    "x-br",
    "x-audio",
    "x-h",
    "x-fps",
    "x-gop",
    "x-audio-br",
    "x-nic",
    "x-container",
    "x-pkt",
    "x-ttl",
    "x-re",
    "x-pass",
  ];
  for (const id of ids) {
    const el = $(id);
    if (!el) continue;
    const changed = () => {
      syncPassUi();
      summarizeCfg(makeCfg());
      syncAudioUi();
      setSetupMsg();
      setPreview("配置已修改，点击「预览命令」查看实际执行参数。");
    };
    el.addEventListener("input", changed);
    el.addEventListener("change", changed);
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
        summarizeCfg(makeCfg());
        setPreview("配置已修改，点击「预览命令」查看实际执行参数。");
      }
    });
  }

  actEl()?.addEventListener("click", () => start());
  applyEl()?.addEventListener("click", async () => {
    await start();
  });
  stopEl()?.addEventListener("click", stop);
  $<HTMLButtonElement>("x-set")?.addEventListener("click", () => showSetup());
  $<HTMLButtonElement>("x-back")?.addEventListener("click", showMonitor);
  $<HTMLButtonElement>("x-preview")?.addEventListener("click", previewCommand);

  document.querySelectorAll<HTMLButtonElement>(".x-tab[data-xpage]").forEach((el) => {
    el.addEventListener("click", () => {
      const page = el.dataset.xpage;
      if (page === "in" || page === "code" || page === "out") showPage(page);
    });
  });
  $<HTMLButtonElement>("x-prev")?.addEventListener("click", () => {
    const i = pageOrder.indexOf(currentPage);
    if (i > 0) showPage(pageOrder[i - 1]);
  });
  $<HTMLButtonElement>("x-next")?.addEventListener("click", () => {
    const i = pageOrder.indexOf(currentPage);
    if (i < pageOrder.length - 1) showPage(pageOrder[i + 1]);
  });

  bindFieldUpdates();
  syncPassUi();
  syncAudioUi();

  api.xcodeEncoders().then((r) => {
    const sel = encEl();
    if (!sel) return;
    sel.innerHTML =
      `<option value="auto">${esc(r.auto_label)}</option>` +
      r.encoders.map((e) => `<option value="${esc(e.id)}">${esc(e.label)}</option>`).join("");
    encoderLabels.clear();
    encoderLabels.set("auto", "自动");
    encoderLabels.set("copy", "透传（不重编码）");
    for (const e of r.encoders) encoderLabels.set(e.id, e.label);
    syncPassUi();
    syncAudioUi();
    summarizeCfg(makeCfg());
  });

  api
    .listNics()
    .then((nics) => {
      const sel = nicEl();
      if (!sel) return;
      const up = nics.filter((n) => n.up);
      sel.innerHTML =
        `<option value="">自动路由</option>` +
        up.map((n) => `<option value="${esc(n.ip)}">${esc(`${n.ip} · ${n.name}`)}</option>`).join("");
    })
    .catch(() => {
      /* 网卡列表失败不阻断转码 */
    });

  renderKpi({ frame: 0, fps: 0, kbps: 0, speed: 0, drop: 0, secs: 0, out_time: "" });
  renderFlow();
  summarizeCfg(makeCfg());
  setState("idle", "待机", "待机");
  showMonitor();
}
