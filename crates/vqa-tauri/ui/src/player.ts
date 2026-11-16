//! 播放器页 —— 来源栏/控制条/快照轮询/视频子窗对齐。
//! 轮询模型对应 egui 版"每帧 poll"：100ms 拉一次 player_snapshot。

import { api, type PlayerStateJson, type Snap } from "./api";
import { icon } from "./icons";
import { SPEEDS } from "./constants";
import { setNavLamp, setEngineLamp } from "./main";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

let snapCache: Snap | null = null;
let stateCache: PlayerStateJson | null = null;
let lastSpeed = 1.0;
let dragSeek = -1.0; // 拖动中的 fraction（-1 = 未拖动）

function fmtHms(ms: number): string {
  if (!isFinite(ms) || ms < 0) ms = 0;
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(h)}:${p(m)}:${p(sec)}`;
}

function shortCodec(c: string): string {
  return c.split("/")[0] || c;
}

/** 源帧率原始串 "50/1" → "50"；取不到返回空 */
function fmtFps(raw?: string): string {
  if (!raw) return "";
  if (!raw.includes("/")) return raw === "0/0" ? "" : raw;
  const [a, b] = raw.split("/").map(Number);
  if (!b || !isFinite(a / b)) return "";
  const v = a / b;
  return v > 0 ? String(Math.round(v * 100) / 100) : "";
}

// ═══════════════════════════ 初始化 ═══════════════════════════

export function initPlayer() {
  const url = $("p-url") as HTMLInputElement;
  const nic = $("p-nic") as HTMLSelectElement;
  const act = $("p-act") as HTMLButtonElement;
  const histBtn = $("p-hist") as HTMLButtonElement;
  const histPop = $("p-histpop") as HTMLElement;
  const force = $("p-force") as HTMLInputElement;

  // 网卡下拉
  api.listNics().then((list) => {
    nic.innerHTML = [`<option>自动</option>`, ...list.map((n) => `<option>${esc(n)}</option>`)].join("");
  });

  // 历史弹层
  histBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (!histPop.hidden) {
      histPop.hidden = true;
      return;
    }
    api.sourceHistory("player").then((h) => {
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

  // 浏览
  $("p-browse").addEventListener("click", async () => {
    const p = await api.browse();
    if (p) url.value = p;
  });

  // 开始/停止
  let running = false;
  act.addEventListener("click", () => go());
  url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") go();
  });
  async function go() {
    if (running) {
      await api.stop();
      running = false;
      setActUi(false);
      return;
    }
    const u = url.value.trim();
    if (!u) return;
    await api.pushSourceHistory("player", u);
    try {
      await api.play(u, nic.value === "自动" ? "" : nic.value, force.checked);
      running = true;
      setActUi(true);
    } catch (e: any) {
      showNote(String(e), true);
    }
  }
  function setActUi(on: boolean) {
    act.classList.toggle("stop", on);
    act.innerHTML = on ? `<span>${icon("square", 13)}</span>停止` : `<span>${icon("play", 13)}</span>开始`;
  }
  setActUi(false);

  // 控制条
  const playBtn = $("c-play") as HTMLButtonElement;
  playBtn.addEventListener("click", () => api.pauseToggle());
  $("c-stop").addEventListener("click", async () => {
    await api.stop();
    running = false;
    setActUi(false);
  });
  $("c-mute").addEventListener("click", () => {
    const m = playBtn && (window as any).__muteCache;
    api.setMute(!m);
    (window as any).__muteCache = !m;
  });
  $("c-snap").addEventListener("click", async () => {
    try {
      const p = await api.snapshotSave();
      showNote(`已存 ${p}`, false);
    } catch (e: any) {
      showNote(String(e), true);
    }
  });
  $("c-rec").addEventListener("click", async () => {
    try {
      const msg = await api.recordToggle();
      showNote(msg, false);
    } catch (e: any) {
      showNote(String(e), true);
    }
  });

  // 循环播放：开关状态由后端持有（跨起播保持），这里只负责点击与回显
  let loopOn = false;
  $("c-loop").addEventListener("click", async () => {
    const want = !loopOn;
    try {
      await api.setLoop(want);
      loopOn = want;
      showNote(want ? "循环播放：开（播完自动从头）" : "循环播放：关", false);
    } catch (e: any) {
      showNote(String(e), true);
    }
  });

  // seek 拖动
  const seek = $("c-seek") as HTMLElement;
  seek.addEventListener("pointerdown", (e) => {
    if (seek.classList.contains("off")) return;
    seek.setPointerCapture(e.pointerId);
    dragSeek = fracOf(e, seek);
    api.seek(dragSeek);
  });
  seek.addEventListener("pointermove", (e) => {
    if (dragSeek < 0) return;
    dragSeek = fracOf(e, seek);
    api.seek(dragSeek);
  });
  seek.addEventListener("pointerup", () => {
    dragSeek = -1.0;
  });

  // 音量拖动
  const vol = $("c-vol") as HTMLElement;
  let volDrag = false;
  vol.addEventListener("pointerdown", (e) => {
    volDrag = true;
    vol.setPointerCapture(e.pointerId);
    volSet(e);
  });
  vol.addEventListener("pointermove", (e) => {
    if (volDrag) volSet(e);
  });
  vol.addEventListener("pointerup", () => {
    volDrag = false;
  });
  function volSet(e: PointerEvent) {
    const rect = vol.getBoundingClientRect();
    const frac = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const v = Math.round(frac * 130);
    api.setVolume(v);
    drawVol(v);
  }

  // 倍速
  window.addEventListener("speed-changed", (e) => {
    lastSpeed = (e as CustomEvent).detail;
    api.setSpeed(lastSpeed);
  });

  // 视频子窗对齐：测量 #v-rect → 物理像素
  const vrect = $("v-rect");
  const ro = new ResizeObserver(() => placeChild());
  ro.observe(vrect);
  window.addEventListener("resize", placeChild);
  window.addEventListener("page-changed", placeChild);

  function placeChild() {
    const r = vrect.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    // 只在播放器页可见（切到其他页时视频子窗必须移出屏幕，否则盖在内容上）
    const onPage = (window as any).__currentPage === "player";
    const visible = onPage && !!snapCache && !snapCache.idle && !!stateCache?.playing;
    api
      .placeVideo(
        Math.round(r.left * dpr),
        Math.round(r.top * dpr),
        Math.round(r.width * dpr),
        Math.round(r.height * dpr),
        visible,
      )
      .catch(() => {});
  }
  (window as any).__placeChild = placeChild;

  // 轮询（100ms）
  setInterval(poll, 100);
  async function poll() {
    try {
      const st = await api.snapshot();
      stateCache = st;
      snapCache = st.snap;
      // 右上「开始/停止」跟随后端状态：播完（mpv 回 idle 且不在准备中）要回到「开始」，
      // 否则 running 只由点击维护，播到结尾按钮会一直卡在「停止」。
      const active = !!st.playing && !(st.snap?.idle && !st.preparing);
      if (active !== running) {
        running = active;
        setActUi(active);
      }
      render(st);
      // nav 状态灯 + 页脚引擎灯
      setNavLamp(
        "player",
        st.err ? "error" : st.playing && !st.snap?.idle ? "running" : "idle",
      );
      setEngineLamp(!!st.snap || !st.err);
    } catch {
      /* 后端未就绪时静默 */
    }
  }
}

// ═══════════════════════════ 渲染 ═══════════════════════════

function render(st: PlayerStateJson) {
  const snap = st.snap;

  // 错误/提示行
  if (st.err) showNote(`✗ ${st.err}`, true);
  else if (st.hint) showNote(st.hint, false);
  else {
    noteEl.className = "noteline";
    noteEl.textContent = "";
  }

  // 空态 / 视频区
  const empty = $("v-empty");
  const hint = $("v-hint");
  if (!st.playing) {
    empty.style.display = "";
    hint.textContent = "输入来源后点「开始」播放";
    (window as any).__muteCache = snap?.mute ?? false;
  } else if (st.preparing) {
    // 起播管线进行中（抽流/桥启动约数秒）
    empty.style.display = "";
    hint.textContent = "⏳ AVS 桥启动中，数秒后出画…";
  } else if (snap && snap.idle) {
    empty.style.display = "";
    hint.textContent = "已播完 · 点 ▶ 重播";
  } else {
    empty.style.display = "none";
  }
  ($("v-replay") as HTMLButtonElement).onclick = () => api.pauseToggle();

  // 播放/暂停图标
  const paused = snap ? snap.paused || snap.idle : true;
  ($("c-play") as HTMLElement).innerHTML = icon(paused ? "play" : "pause");

  // seek/时间
  const dur = snap?.duration_ms ?? 0;
  const pos = snap?.time_ms ?? 0;
  const seekable = !!snap && snap.seekable && dur > 0 && !snap.idle;
  const seek = $("c-seek");
  seek.classList.toggle("off", !seekable);
  if (dragSeek < 0) {
    ($("c-seekfill") as HTMLElement).style.width = `${seekable ? (pos / dur) * 100 : 0}%`;
  }
  $("c-time").textContent = `${fmtHms(pos)} / ${seekable ? fmtHms(dur) : "--:--"}`;

  // 音量/静音
  if (snap) {
    drawVol(snap.mute ? 0 : snap.volume);
    const muteBtn = $("c-mute") as HTMLElement;
    muteBtn.innerHTML = icon(snap.mute ? "volumex" : "volume");
    muteBtn.classList.toggle("muted", snap.mute);
    (window as any).__muteCache = snap.mute;
  }

  // 录制态
  const recBtn = $("c-rec") as HTMLElement;
  recBtn.classList.toggle("rec-on", st.recording);

  // 循环态
  ($("c-loop") as HTMLElement).classList.toggle("on", !!st.loop);

  // 桥格式条（源 → 目标）
  renderBridgeBar(st);

  // 倍速同步
  if (snap && snap.speed && Math.abs(snap.speed - lastSpeed) > 0.01) {
    const idx = SPEEDS.findIndex((s) => Math.abs(s - snap.speed) < 0.01);
    if (idx >= 0) {
      ($("c-speedbtn") as HTMLButtonElement).textContent = `${SPEEDS[idx]}x ▾`;
    }
  }

  // 角落 tag
  const tl = $("tag-tl");
  if (snap && !snap.idle && snap.width > 0) {
    const hw = snap.hwdec && snap.hwdec !== "no" ? ` · 硬解 ${snap.hwdec}` : "";
    tl.textContent = `${snap.width}×${snap.height} · ${shortCodec(snap.video_codec)}${hw}`;
    tl.style.display = "";
  } else if (st.probe && st.probe.w > 0) {
    const ten = st.probe.pix_fmt.includes("10") ? " · 10-bit" : "";
    tl.textContent = `${st.probe.w}×${st.probe.h} · ${shortCodec(st.probe.codec)}${ten}`;
    tl.style.display = "";
  } else {
    tl.style.display = "none";
  }
  const tr = $("tag-tr") as HTMLElement;
  // 桥 tag：显示「由什么转什么」——源规格 → H.264 + 输出码率（Mbps）
  const bridging = !!(st.bridge && st.playing && st.snap && !st.snap.idle);
  tr.hidden = !bridging;
  if (bridging && st.probe) {
    const ten = st.probe.pix_fmt.includes("10") ? "10-bit" : "8-bit";
    const mbps = snap && snap.v_bitrate > 0 ? (snap.v_bitrate / 1e6).toFixed(1) : "?";
    tr.textContent = `⚠ 桥：AVS2 ${st.probe.w}×${st.probe.h} ${ten} → H.264 8-bit · ${mbps} Mbps`;
  }

  // chips
  renderChips(st);

  // 视频子窗显隐
  (window as any).__placeChild?.();
}

/** 播放器页下方的"桥转码：源格式 → 目标格式"说明条 */
function renderBridgeBar(st: PlayerStateJson) {
  const bar = $("bridge-bar");
  const to = st.bridge_to;
  if (!to || !st.playing) {
    bar.hidden = true;
    return;
  }
  const p = st.probe;
  const srcFps = fmtFps(p?.fps);
  const depth = p ? (p.pix_fmt.includes("10") ? "10-bit" : "8-bit") : "";
  // 与运行时指标同处一行，文字要短：源侧只留编码/分辨率/位深/帧率
  const fromText =
    p && p.w > 0
      ? `${p.codec.toUpperCase()} ${p.w}×${p.h} ${depth}${srcFps ? ` ${srcFps}fps` : ""}`
      : "源格式未知";
  // 输出帧率优先用实测（vf_fps），次选后端给的标称值（chain10 固定 25）
  const outFps =
    st.snap && st.snap.vf_fps > 0
      ? `${st.snap.vf_fps.toFixed(1)}fps`
      : to.fps
        ? `${to.fps}fps`
        : "";
  const outDepth = /10/.test(to.pix) ? "10-bit" : "8-bit";
  // 音频（AAC 96k）右侧 chips 已有，这里不再重复
  const toText = `${to.enc_label.replace(/ \/ /g, "/")} ${outDepth}${outFps ? ` ${outFps}` : ""}`;
  $("bf-from").textContent = fromText;
  $("bf-to").textContent = toText;
  $("bf-tag").textContent = `桥转码 ${to.mode}`;
  bar.hidden = false;
}

function renderChips(st: PlayerStateJson) {  // 只重建动态 chips 区；force-bridge 是固定 DOM（重建会丢勾选状态）
  const box = $("p-chips-dyn");
  const parts: string[] = [];
  const snap = st.snap;
  // 桥说明条已写了源→目标的分辨率/帧率/编码，指标里不重复，只留解码、码率、丢帧、音频
  const bridged = !!st.bridge_to && !!st.playing;
  if (!st.playing) {
    parts.push(`<span class="chip-m">◦ 待机 · 无流数据</span>`);
  } else if (st.preparing) {
    parts.push(`<span class="chip-m">⏳ AVS 源转码中…请稍候</span>`);
  } else if (snap) {
    if (snap.idle) {
      parts.push(`<span class="chip-m">◦ 已播完</span>`);
    } else {
      const hw = snap.hwdec && snap.hwdec !== "no" ? `硬解 ${snap.hwdec}` : "软解";
      parts.push(
        bridged
          ? `<span class="chip-m">${hw}</span>`
          : `<span class="chip-m">${snap.width}×${snap.height} · ${shortCodec(snap.video_codec)} · ${hw}</span>`,
      );
      if (snap.vf_fps > 0 && !bridged)
        parts.push(`<span class="chip-m">${icon("gauge", 11)} ${snap.vf_fps.toFixed(1)} fps</span>`);
      if (snap.v_bitrate > 0)
        parts.push(
          `<span class="chip-m">${icon("wifi", 11)} ${(snap.v_bitrate / 1e6).toFixed(2)} Mbps</span>`,
        );
      const drop = snap.decoder_drop + snap.render_drop;
      parts.push(
        `<span class="chip-m ${drop > 0 ? "warn" : "ok"}">${icon("check", 11)} 丢帧 ${snap.decoder_drop}/${snap.render_drop}</span>`,
      );
      if (snap.audio_codec) parts.push(`<span class="chip-m">${icon("audiolines", 11)} ${snap.audio_codec}</span>`);
      if (Math.abs(snap.avsync) > 0.001)
        parts.push(
          `<span class="chip-m ${Math.abs(snap.avsync) > 0.1 ? "warn" : ""}">avsync ${snap.avsync >= 0 ? "+" : ""}${snap.avsync.toFixed(3)}s</span>`,
        );
    }
  }
  box.innerHTML = parts.join("");
}

// ═══════════════════════════ 小工具 ═══════════════════════════

const noteEl = document.getElementById("p-note") as HTMLElement;

function showNote(text: string, isErr: boolean) {
  noteEl.textContent = text;
  noteEl.className = `noteline ${isErr ? "err" : "hint"}`;
}

function fracOf(e: PointerEvent, el: HTMLElement): number {
  const rect = el.getBoundingClientRect();
  return Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
}

function drawVol(v: number) {
  const frac = Math.min(1, Math.max(0, v / 130));
  ($("c-volfill") as HTMLElement).style.width = `${frac * 64}px`;
  ($("c-volthumb") as HTMLElement).style.left = `calc(${frac * 100}% - 5.5px)`;
}

export function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}
