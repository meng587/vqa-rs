//! 播放器页 —— 来源栏/控制条/快照轮询/视频子窗对齐。
//! 轮询模型对应 egui 版"每帧 poll"：100ms 拉一次 player_snapshot。

import { api, type PlayerStateJson, type Snap } from "./api";
import { icon } from "./icons";
import { SPEEDS } from "./constants";

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
    const visible = !!snapCache && !snapCache.idle && !!stateCache?.playing;
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
      render(st);
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
  else noteEl.className = "noteline";

  // 空态 / 视频区
  const empty = $("v-empty");
  const hint = $("v-hint");
  if (!st.playing) {
    empty.style.display = "";
    hint.textContent = "输入来源后点「开始」播放";
    (window as any).__muteCache = snap?.mute ?? false;
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
  tr.hidden = !st.bridge_note;
  if (st.bridge_note) tr.textContent = `⚠ ${st.bridge_note}`;

  // chips
  renderChips(st);

  // 视频子窗显隐
  (window as any).__placeChild?.();
}

function renderChips(st: PlayerStateJson) {
  const box = $("p-chips");
  const keep = (box.querySelector(".force-bridge") as HTMLElement).outerHTML;
  const parts: string[] = [];
  const snap = st.snap;
  if (!st.playing) {
    parts.push(`<span class="chip-m">◦ 待机 · 无流数据</span>`);
  } else if (snap) {
    if (snap.idle) {
      parts.push(`<span class="chip-m">◦ 已播完</span>`);
    } else {
      parts.push(
        `<span class="chip-m">${snap.width}×${snap.height} · ${shortCodec(snap.video_codec)} · 硬解 ${snap.hwdec}</span>`,
      );
      if (snap.vf_fps > 0) parts.push(`<span class="chip-m">${icon("gauge", 11)} ${snap.vf_fps.toFixed(1)} fps</span>`);
      if (snap.v_bitrate > 0)
        parts.push(`<span class="chip-m">${icon("wifi", 11)} ${(snap.v_bitrate / 1000).toFixed(0)} kbps</span>`);
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
  box.innerHTML = parts.join("") + keep;
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
