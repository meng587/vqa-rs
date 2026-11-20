//! 播放器「多路同看」：来源弹窗、宫格、选中出声、双击放大页。
//!
//! 后端每一格是一个独立 mpv + 独立 WS_CHILD 视频子窗；前端只负责：
//! 1) 输入多个地址并启动；2) 把每格 `.mv-cell` 的 CSS 矩形换成物理像素，
//!    调 `multi_place` 对齐原生子窗；3) 点击格切换 `multi_select`；4) 双击
//!    把选中格铺到全屏放大页。

import { api, isWebMode, type MultiRect, type MultiSlotState } from "./api";
import { icon } from "./icons";

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function shortUrl(u: string): string {
  let t = u.replace(/^[a-z0-9+.-]+:\/\//i, "").replace(/^@/, "");
  if (t.length > 56) t = `${t.slice(0, 32)}…${t.slice(-20)}`;
  return t || u;
}

interface Tile {
  el: HTMLElement;
  cell: HTMLElement;
  state: HTMLElement;
  sound: HTMLElement;
  url: HTMLElement;
  startedAt: number;
}

export interface MultiViewCtl {
  /** 是否处于多路同看（宫格或放大页） */
  isActive(): boolean;
  /** 是否有多路正在出画面（导航状态灯用） */
  anyPlaying(): boolean;
  /** 弹窗打开或多路激活时，单路视频子窗必须让位 */
  blocksSingle(): boolean;
  /** 重新按当前 DOM 摆放原生子窗（页面切换/窗口 resize 调用） */
  place(): void;
  /** 单路播放前调用：停掉多路并恢复单路 UI */
  exitForSingle(): Promise<void>;
  /** 打开输入弹窗（active 时用作“重新输入”） */
  openPanel(): void;
}

export interface MultiViewOpts {
  nicIp: () => string;
  currentUrl: () => string;
  stopSingle: () => Promise<void>;
}

export function initMultiView(opts: MultiViewOpts): MultiViewCtl {
  const vcard = $("vcard");
  const grid = $("mv-grid");
  const bar = $("mv-bar");
  const selLabel = $("mv-sel");
  const pauseBtn = $<HTMLButtonElement>("mv-pause");
  const stopBtn = $("mv-stop");
  const editBtn = $("mv-edit");
  const multiBtn = $<HTMLButtonElement>("p-multi");

  const panel = $("mv-panel");
  const panelMask = $("mv-panel-mask");
  const panelX = $("mv-panel-x");
  const panelCancel = $("mv-panel-cancel");
  const startBtn = $<HTMLButtonElement>("mv-start");
  const urlsInput = $<HTMLTextAreaElement>("mv-urls");
  const countEl = $("mv-count");

  const zoom = $("mv-zoom");
  const zoomRect = $("mv-zoom-rect");
  const zoomName = $("mv-zoom-name");
  const zoomBack = $("mv-zoom-back");
  const zoomClose = $("mv-zoom-close");
  const zoomStage = $("mv-zoom-stage");

  let active = false;
  let panelOpen = false;
  let zoomed = -1;
  let sel = 0;
  let urls: string[] = [];
  let tiles: Tile[] = [];
  let busy = false;
  let lastPlace = 0;
  let tickBusy = false;
  let playingCount = 0;

  if (isWebMode) {
    multiBtn.style.display = "none";
  }

  const pageOn = () => document.body.dataset.page === "player";

  function buildTiles(list: string[]) {
    grid.className = `mv-grid n${Math.min(4, Math.max(1, list.length))}`;
    grid.innerHTML = list
      .map(
        (u, i) => `
        <div class="mv-tile" data-slot="${i}" title="${esc(u)}">
          <div class="mv-head">
            <b class="mv-name">第 ${i + 1} 路</b>
            <span class="mv-url" title="${esc(u)}">${esc(shortUrl(u))}</span>
            <span class="mv-state">正在连接…</span>
            <span class="mv-snd off">${icon("volumex", 11)}静音</span>
          </div>
          <div class="mv-cell" id="mv-cell-${i}"></div>
        </div>`,
      )
      .join("");
    tiles = list.map((_, i) => {
      const el = grid.querySelector(`.mv-tile[data-slot="${i}"]`) as HTMLElement;
      return {
        el,
        cell: $(`mv-cell-${i}`),
        state: el.querySelector(".mv-state") as HTMLElement,
        sound: el.querySelector(".mv-snd") as HTMLElement,
        url: el.querySelector(".mv-url") as HTMLElement,
        startedAt: Date.now(),
      };
    });
    grid.hidden = false;
  }

  function setActive(next: number, syncBackend = true) {
    if (!tiles.length) return;
    sel = Math.max(0, Math.min(tiles.length - 1, next));
    for (let i = 0; i < tiles.length; i++) tiles[i].el.classList.toggle("active", i === sel);
    for (let i = 0; i < tiles.length; i++) {
      const t = tiles[i];
      t.sound.className = `mv-snd ${i === sel ? "on" : "off"}`;
      t.sound.innerHTML = i === sel ? `${icon("volume", 11)}出声` : `${icon("volumex", 11)}静音`;
    }
    selLabel.textContent = `第 ${sel + 1} 路出声`;
    pauseBtn.textContent = "暂停";
    if (syncBackend) api.multiSelect(sel).catch(() => undefined);
  }

  function hideAll() {
    api.multiPlace([]).catch(() => undefined);
  }

  function rectOf(el: HTMLElement): Omit<MultiRect, "slot"> {
    const r = el.getBoundingClientRect();
    if (r.width <= 1 || r.height <= 1) return { x: -20000, y: -20000, w: 1, h: 1, visible: false };
    const dpr = window.devicePixelRatio || 1;
    return {
      x: Math.round(r.left * dpr),
      y: Math.round(r.top * dpr),
      w: Math.round(r.width * dpr),
      h: Math.round(r.height * dpr),
      visible: true,
    };
  }

  function place() {
    if (!active) return;
    lastPlace = Date.now();
    if (!pageOn() || panelOpen) {
      hideAll();
      return;
    }
    if (zoomed >= 0 && tiles[zoomed]) {
      const rects: MultiRect[] = [];
      for (let i = 0; i < tiles.length; i++) {
        rects.push({ slot: i, ...rectOf(zoomRect), visible: i === zoomed });
      }
      api.multiPlace(rects).catch(() => undefined);
      return;
    }
    const rects = tiles.map((t, i) => ({ slot: i, ...rectOf(t.cell) }));
    api.multiPlace(rects).catch(() => undefined);
  }

  function applyRows(rows: MultiSlotState[]) {
    const now = Date.now();
    let alive = 0;
    for (let i = 0; i < tiles.length; i++) {
      const t = tiles[i];
      const r = rows[i];
      if (!r) continue;
      if (r.dblclick) {
        openZoom(i);
        continue;
      }
      if (r.click && zoomed < 0) setActive(i);
      if (r.playing) alive += 1;
      t.el.classList.toggle("active", i === sel);
      t.url.title = r.url || "未填写";
      if (r.error) {
        t.state.textContent = "错误";
        t.state.className = "mv-state err";
        t.state.title = r.error;
      } else if (r.playing) {
        const dim = r.width > 0 && r.height > 0 ? `${r.width}×${r.height}` : "";
        t.state.textContent = r.paused ? "已暂停" : "播放中";
        t.state.className = "mv-state ok";
        t.state.title = [dim, r.video_codec, r.audio_codec].filter(Boolean).join(" · ");
      } else if (r.idle && r.url) {
        const stuck = now - t.startedAt > 12000;
        t.state.textContent = stuck ? "无画面" : "等待画面…";
        t.state.className = stuck ? "mv-state err" : "mv-state";
        t.state.title = r.status || "";
      } else {
        t.state.textContent = r.status || "待机";
        t.state.className = "mv-state";
        t.state.title = "";
      }
    }
    playingCount = alive;
    const activeRow = rows[sel];
    if (activeRow) pauseBtn.textContent = activeRow.paused ? "继续" : "暂停";
  }

  async function tick() {
    if (!active || tickBusy) return;
    tickBusy = true;
    try {
      const rows = await api.multiSnapshot();
      applyRows(rows);
      // 原生子窗要定期提顶，防 WebView2 重新压上来；500ms 一次足够。
      if (Date.now() - lastPlace > 500) place();
    } catch {
      /* 后端未就绪时静默 */
    } finally {
      tickBusy = false;
    }
  }

  function openZoom(i: number) {
    if (!active || !tiles[i]) return;
    setActive(i);
    zoomed = i;
    const u = urls[i] || "";
    zoomName.textContent = `第 ${i + 1} 路 · ${shortUrl(u)}`;
    grid.hidden = true;
    zoom.hidden = false;
    // 等固定层的布局生效后再量矩形，否则子窗会按上一帧尺寸摆放。
    requestAnimationFrame(() => place());
  }

  function closeZoom() {
    if (zoomed < 0) return;
    zoomed = -1;
    zoom.hidden = true;
    if (active) grid.hidden = false;
    requestAnimationFrame(() => place());
  }

  function updateCount() {
    const n = urlsInput.value
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean).length;
    countEl.textContent = n > 4 ? `已填写 ${n} 路（只取前 4 路）` : `已填写 ${n} 路`;
    return Math.min(4, n);
  }

  function openPanel() {
    if (busy) return;
    if (active) hideAll();
    panelOpen = true;
    urlsInput.value = (active && urls.length ? urls : urls.length ? urls : [opts.currentUrl()])
      .filter(Boolean)
      .join("\n");
    updateCount();
    panel.hidden = false;
    // 单路视频子窗也要让位，否则会盖住弹窗
    (window as any).__placeChild?.();
    urlsInput.focus();
  }

  function closePanel() {
    if (!panelOpen) return;
    panelOpen = false;
    panel.hidden = true;
    (window as any).__placeChild?.();
    if (active) place();
  }

  async function start() {
    if (busy) return;
    const list = urlsInput.value
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean);
    if (!list.length) {
      countEl.textContent = "请至少填写一路来源";
      urlsInput.focus();
      return;
    }
    if (list.length > 4) {
      list.length = 4;
      urlsInput.value = list.join("\n");
      updateCount();
    }
    busy = true;
    startBtn.disabled = true;
    startBtn.innerHTML = `${icon("activity", 13)}启动中…`;
    try {
      await opts.stopSingle();
      closePanel();
      urls = list;
      sel = 0;
      zoomed = -1;
      playingCount = 0;
      buildTiles(list);
      vcard.classList.add("multi");
      bar.hidden = false;
      multiBtn.classList.add("is-on");
      active = true;
      setActive(0, false);
      list.forEach((u) => {
        void api.pushSourceHistory("player", u).catch(() => undefined);
      });
      const rows = await api.multiStart(list, opts.nicIp());
      applyRows(rows);
      const firstOk = rows.findIndex((r) => r.url && !r.error);
      if (firstOk >= 0) setActive(firstOk);
      else setActive(0, false);
      requestAnimationFrame(() => place());
      setTimeout(() => place(), 60);
      if (zoom.hidden === false) closeZoom();
    } catch (e: any) {
      await api.multiStop().catch(() => undefined);
      active = false;
      vcard.classList.remove("multi");
      bar.hidden = true;
      grid.hidden = true;
      zoom.hidden = true;
      multiBtn.classList.remove("is-on");
      (window as any).__placeChild?.();
      panelOpen = true;
      panel.hidden = false;
      urlsInput.value = list.join("\n");
      countEl.textContent = `启动失败：${String(e).slice(0, 180)}`;
    } finally {
      busy = false;
      startBtn.disabled = false;
      startBtn.innerHTML = `${icon("grid2x2", 13)}开始同看`;
    }
  }

  async function stopAll(showNote = false) {
    if (!active && !panelOpen) return;
    busy = true;
    closePanel();
    await api.multiStop().catch(() => undefined);
    active = false;
    zoomed = -1;
    playingCount = 0;
    grid.hidden = true;
    bar.hidden = true;
    zoom.hidden = true;
    vcard.classList.remove("multi");
    multiBtn.classList.remove("is-on");
    hideAll();
    (window as any).__placeChild?.();
    busy = false;
    if (showNote) alertNote("已退出多路同看", false);
  }

  // 弹窗
  multiBtn.addEventListener("click", () => openPanel());
  panelMask.addEventListener("click", closePanel);
  panelX.addEventListener("click", closePanel);
  panelCancel.addEventListener("click", closePanel);
  urlsInput.addEventListener("input", updateCount);
  urlsInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void start();
    }
  });
  startBtn.addEventListener("click", () => void start());

  // 宫格交互
  grid.addEventListener("click", (e) => {
    const tile = (e.target as HTMLElement | null)?.closest(".mv-tile") as HTMLElement | null;
    if (!tile) return;
    const i = Number(tile.dataset.slot);
    if (Number.isFinite(i)) setActive(i);
  });
  grid.addEventListener("dblclick", (e) => {
    const tile = (e.target as HTMLElement | null)?.closest(".mv-tile") as HTMLElement | null;
    if (!tile) return;
    e.preventDefault();
    const i = Number(tile.dataset.slot);
    if (Number.isFinite(i)) openZoom(i);
  });

  // 原生子窗的单击/双击由后端 wndproc 排队，随 multi_snapshot 一起回来；
  // 这样点在画面上（而不是细窄的标题条上）也能切声/放大。
  // 具体处理在 applyRows 里，避免依赖 Tauri event listen 的注册时序。

  pauseBtn.addEventListener("click", async () => {
    try {
      const paused = await api.multiTogglePause(sel);
      pauseBtn.textContent = paused ? "继续" : "暂停";
      alertNote(paused ? `第 ${sel + 1} 路已暂停` : `第 ${sel + 1} 路继续播放`, false);
    } catch (e: any) {
      alertNote(`暂停失败：${String(e)}`, true);
    }
  });
  stopBtn.addEventListener("click", () => void stopAll(true));
  editBtn.addEventListener("click", () => openPanel());

  // 放大页
  zoomBack.addEventListener("click", closeZoom);
  zoomClose.addEventListener("click", () => void stopAll(false));
  zoomStage.addEventListener("dblclick", (e) => {
    e.preventDefault();
    closeZoom();
  });

  // Esc：先关弹窗，再关放大页
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (panelOpen) {
      closePanel();
      return;
    }
    if (zoomed >= 0) closeZoom();
  });

  window.addEventListener("resize", () => place());
  window.addEventListener("page-changed", () => place());
  const ro = new ResizeObserver(() => place());
  ro.observe(vcard);

  setInterval(() => void tick(), 150);

  return {
    isActive: () => active,
    anyPlaying: () => active && playingCount > 0,
    blocksSingle: () => active || panelOpen,
    place,
    exitForSingle: async () => {
      if (active || panelOpen) await stopAll(false);
    },
    openPanel,
  };
}

/** 播放器页顶部的提示行；多路模块独立使用，避免和单路提示互相覆盖。 */
function alertNote(text: string, isErr: boolean) {
  const el = document.getElementById("p-note");
  if (!el) return;
  el.textContent = text;
  el.className = `noteline ${isErr ? "err" : "hint"}`;
}
