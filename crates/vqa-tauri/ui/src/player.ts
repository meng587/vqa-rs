//! 播放器页 —— 来源栏/控制条/快照轮询/视频子窗对齐。
//! 轮询模型对应 egui 版"每帧 poll"：100ms 拉一次 player_snapshot。

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen } from "@tauri-apps/api/event";
import { api, type MediaInfo, type NicRow, type PlayerStateJson, type ProgRow, type Snap } from "./api";
import { icon } from "./icons";
import { SPEEDS } from "./constants";
import { setNavLamp, setEngineLamp, nicLabel } from "./main";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

let snapCache: Snap | null = null;
let stateCache: PlayerStateJson | null = null;
let lastSpeed = 1.0;
let dragSeek = -1.0; // 拖动中的 fraction（-1 = 未拖动）
let osdOn = false; // Tab 触发的画面内媒体信息浮层
let osdPending = false; // 起播瞬间按的 Tab 先记下，画面出来再补
let osdRefreshAt = 0; // 浮层刷新节流（播放中数字会变）
let lastVb = 0; // mpv 的视频码率会间歇回 0，留住上一次有效值，浮层数字才不闪
let miPath = ""; // 当前媒体信息对应的源
let miInfo: MediaInfo | null = null;

/** 取媒体信息（按路径缓存）。换片时清掉上一片的码率兜底值。 */
async function ensureMediaInfo(path: string): Promise<MediaInfo | null> {
  if (miPath === path) return miInfo;
  miPath = path;
  miInfo = null;
  lastVb = 0; // 换片了：上一个片子的视频码率不能带到新片子上
  const got = await api.mediaInfo(path).catch(() => null);
  if (miPath === path) miInfo = got;
  return got;
}

/** 软解/硬解标签。8K H.264 是 NVDEC 的硬上限，明确点出来，免得把卡顿当机器问题。 */
function hwLabel(snap: Snap): string {
  if (snap.hwdec && snap.hwdec !== "no") return `硬解 ${snap.hwdec}`;
  const k = snap.height >= 4000 ? "8K" : snap.height >= 2000 ? "4K" : "";
  if (k && /264/i.test(snap.video_codec)) return `软解 · ${k} H.264 无硬解`;
  if (k) return `软解 · ${k}`;
  return "软解";
}

/** 当前媒体里「能播但不解」的编码：AC-4 音频、H.266 视频。
 *  本地文件看 ffprobe 的媒体信息；网络源 ffprobe 常探不到，就退回 mpv
 *  自己报的编码名（snap.audio_codec / video_codec），组播 AC-4 也能提示。 */
function codecWarns(snap?: Snap): string[] {
  const mi = miInfo;
  const v = (mi?.video[0]?.codec || snap?.video_codec || "").trim();
  const auds = [...(mi?.audio.map((a) => a.codec) || []), snap?.audio_codec || ""];
  const out: string[] = [];
  if (/^(vvc|h266|h\.266)/i.test(v)) out.push("H.266 需更新解码器");
  if (auds.some((a) => /^(ac4|ac-4)/i.test((a || "").trim())))
    out.push("AC-4 音频无解码 · 无声");
  return out;
}

/** 收起画面浮层：id=1 这一层单独清掉，不影响 mpv 的其他提示 */
function clearOsd() {
  if (!osdOn) return;
  osdOn = false;
  api.playerOsd([], 1280, 720).catch(() => undefined);
}

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
  return (c.split("/")[0] || c).trim();
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
  const progSel = $("p-prog") as HTMLSelectElement;
  const progLbl = $("p-proglbl") as HTMLElement;

  // 网卡下拉：值 = IP（取流绑定用），显示「IP · 网卡名」，断开的标注出来
  api.listNics().then((list: NicRow[]) => {
    nic.innerHTML = [
      `<option value="">自动</option>`,
      ...list.map((n) => `<option value="${esc(n.ip)}" title="${esc(n.desc || n.name)}">${esc(nicLabel(n))}</option>`),
    ].join("");
  });

  // 节目下拉：只有多节目 TS 才露出来。起播后探一次 PAT/PMT，>1 个节目才显示。
  let progKey = "";
  let progRows: ProgRow[] = [];
  const hideProg = () => {
    progSel.hidden = true;
    progLbl.hidden = true;
    progSel.innerHTML = "";
    progRows = [];
  };
  const loadPrograms = (src: string, nicIp: string) => {
    const key = `${src}|${nicIp}`;
    if (key === progKey) return;
    progKey = key;
    hideProg();
    api
      .playerPrograms(src, nicIp)
      .then((rows) => {
        if (key !== progKey) return;
        progRows = rows || [];
        if (progRows.length <= 1) return;
        progSel.innerHTML = progRows
          .map(
            (p) =>
              `<option value="${p.ord}" title="${esc(p.streams.join(' · '))}">节目 ${p.number}</option>`,
          )
          .join("");
        progSel.value = String(progRows[0].ord);
        progSel.hidden = false;
        progLbl.hidden = false;
      })
      .catch(() => hideProg());
  };
  progSel.addEventListener("change", () => {
    const ord = Number(progSel.value);
    const row = progRows.find((p) => p.ord === ord);
    if (!ord) return;
    const src = url.value.trim();
    api
      .playerSetProgram(src, ord, nic.value)
      .then(() => showNote(`正在切到节目 ${row?.number ?? ord}（重新抽流，约 1-2 秒）`, false))
      .catch((e) => showNote(`✗ ${String(e)}`, true));
  });

  // 拖文件进来直接播：两条来源 ——
  //  1) WebView2 的 Tauri 原生拖放（视频区没被 mpv 子窗盖住时）；
  //  2) 视频子窗自己接的 WM_DROPFILES（播放中 mpv 盖住视频区时），Rust 转成 video://drop。
  const vcard = $("vcard");
  const dropHint = $("v-drophint");
  const MEDIA_EXT = /\.(ts|m2ts|mp4|mkv|flv|mov|avi|mpg|mpeg|wmv|webm)$/i;
  let lastDrop: { path: string; at: number } | null = null;
  const clearDropUi = () => {
    vcard.classList.remove("drop-hot");
    dropHint.hidden = true;
  };
  const handleDrop = (paths: string[]) => {
    if (document.body.dataset.page !== "player") return;
    const file = paths.find((p) => MEDIA_EXT.test(p)) || paths[0];
    if (!file) return;
    const now = Date.now();
    // 两条来源可能对同一次拖放各报一次，去抖
    if (lastDrop && lastDrop.path === file && now - lastDrop.at < 400) return;
    lastDrop = { path: file, at: now };
    url.value = file;
    const name = file.split(/[\\/]/).pop() || file;
    // ★ 拖入只预热、不自动播：转码在后台先跑起来，用户点播放时直接读缓存
    //   （4K AVS2 10bit 冷启动的 7 秒准备是压不掉的，只能提前花掉）
    api.prewarm(file, nic.value, force.checked).catch(() => undefined);
    showNote(`${name} 已加入，正在后台预热——点播放即秒开`, false);
  };
  getCurrentWebview().onDragDropEvent((e) => {
    const t = e.payload.type;
    if (t === "enter" || t === "over") {
      if (document.body.dataset.page === "player") {
        vcard.classList.add("drop-hot");
        dropHint.hidden = false;
      }
      return;
    }
    clearDropUi();
    if (t === "drop") handleDrop(e.payload.paths);
  });
  // mpv 子窗那条路（播放中拖到画面上）：只收路径，不区分页
  listen<string[]>("video://drop", (e) => handleDrop(e.payload));

  // ── Tab：播放中在画面上叠加媒体信息 ────────────────────────────────
  // HTML 浮层画不出来（mpv 是原生子窗、压在 WebView 之上），所以交给
  // mpv 自己的 OSD 渲染到画面上；子窗加了 WS_EX_NOACTIVATE，点画面不会
  // 把键盘焦点从 WebView 抢走，Tab 才能落到这里。

  /** 取数并推一层浮层；失败就把开关打回关（下次 Tab 重新开） */
  function showOsd(): void {
    osdLines()
      .then((lines) => {
        if (!lines.length) return;
        const snap = snapCache;
        osdOn = true;
        return api
          .playerOsd(lines, snap?.width || 1280, snap?.height || 720)
          .catch(() => {
            osdOn = false;
          });
      })
      .catch(() => {
        osdOn = false;
      });
  }

  window.addEventListener("keydown", (e) => {
    if (e.key !== "Tab") return;
    if (document.body.dataset.page !== "player") return;
    const t = e.target as HTMLElement | null;
    if (t && /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName)) return; // 表单里保留 Tab 换焦点
    const st = stateCache;
    if (!st?.playing) return; // 根本没在播：不抢 Tab
    e.preventDefault();
    if (osdOn) {
      clearOsd();
      return;
    }
    // 点了播放、mpv 还在一秒半秒的起播中：先记一笔，等有画面了自动出来
    if (!snapCache || snapCache.idle) {
      osdPending = !osdPending;
      return;
    }
    osdPending = false;
    showOsd();
  });

  async function osdLines(): Promise<string[]> {
    const st = stateCache;
    const snap = snapCache;
    const path = url.value.trim();
    if (!path || !st || !snap || snap.idle) return [];
    await ensureMediaInfo(path);
    const info = miInfo;
    const name = path.split(/[\\/]/).pop() || path;
    const out: string[] = [`媒体信息 · ${name}`];
    // ffprobe 给的是小写短名（h264 / mpegts），浮层上用行业写法更好读
    const CODE: Record<string, string> = {
      h264: "H.264",
      hevc: "H.265",
      h265: "H.265",
      avs2: "AVS2",
      avs3: "AVS3",
      mpeg2video: "MPEG-2",
      mpeg4: "MPEG-4",
      aac: "AAC",
      ac3: "AC-3",
      eac3: "E-AC-3",
      mp2: "MP2",
      mp3: "MP3",
      opus: "Opus",
      vc1: "VC-1",
    };
    const CONT: Record<string, string> = {
      mpegts: "MPEG-TS",
      "matroska,webm": "MKV",
      matroska: "MKV",
      "mov,mp4,m4a,3gp,3g2,mj2": "MP4",
      mov: "MOV",
      flv: "FLV",
      mpeg: "MPEG-PS",
      avi: "AVI",
      m2ts: "M2TS",
    };
    const lab = (m: Record<string, string>, s?: string | null) =>
      !s ? "" : m[s.toLowerCase()] || s;
    if (info) {
      const f = info.format;
      out.push(
        `容器  ${[
          lab(CONT, f.container),
          f.duration_s ? `${f.duration_s.toFixed(2)} s` : "",
          f.bit_rate ? `${(f.bit_rate / 1e6).toFixed(2)} Mbps` : "",
          f.size_bytes ? `${(f.size_bytes / 1048576).toFixed(1)} MB` : "",
        ]
          .filter(Boolean)
          .join(" · ")}`,
      );
      const v = info.video?.[0];
      if (v) {
        const br = v.bit_rate || snap.v_bitrate || lastVb;
        const wh =
          v.width && v.height
            ? `${v.width}×${v.height}`
            : snap.width > 0
              ? `${snap.width}×${snap.height}`
              : "";
        const nominal = v.avg_frame_rate ? fmtFps(v.avg_frame_rate) : "";
        out.push(
          `视频  ${[
            [lab(CODE, v.codec), v.profile].filter(Boolean).join(" "),
            wh,
            nominal ? `${nominal} fps` : "",
            br ? `${(br / 1e6).toFixed(2)} Mbps` : "",
          ]
            .filter(Boolean)
            .join(" · ")}`,
        );
        const color = [v.pix_fmt, v.field_order]
          .filter((x) => x && x !== "unknown")
          .join(" · ");
        if (color) out.push(`画质  ${color}`);
      }
      const a = info.audio?.[0];
      if (a) {
        const sr = a.sample_rate
          ? a.sample_rate % 1000 === 0
            ? `${(a.sample_rate / 1000).toFixed(1)} kHz`
            : `${a.sample_rate} Hz`
          : "";
        out.push(
          `音频  ${[
            [lab(CODE, a.codec), a.profile].filter(Boolean).join(" "),
            sr,
            a.channels ? `${a.channels} 声道` : "",
          ]
            .filter(Boolean)
            .join(" · ")}`,
        );
      }
    } else {
      out.push("媒体信息  探测失败（ffprobe 没取到）");
    }
    // 解码态：硬解/软解 + 丢帧（帧率/码率上面已经写过，不再重复）
    out.push(`解码  ${hwLabel(snap)} · 丢帧 ${snap.decoder_drop}/${snap.render_drop}`);
    const warns = codecWarns(snap);
    if (warns.length) out.push(`提示  ${warns.join(" · ")}`);
    if (st.bridge_to && st.playing) {
      out.push(`桥    ${$("bf-from").textContent} → ${$("bf-to").textContent}`);
    }
    return out;
  }

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
    if (p) {
      url.value = p;
      // ★ 选定即预热：后台先把转码跑起来，点播放读缓存秒开
      api.prewarm(p, nic.value, force.checked).catch(() => undefined);
      showNote("已选定，正在后台预热——点播放即秒开", false);
    }
  });
  // 空态里的「选择本地文件」复用同一个浏览动作
  $("v-empty-browse").addEventListener("click", () => ($("p-browse") as HTMLElement).click());

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
      await api.play(u, nic.value, force.checked);
      running = true;
      setActUi(true);
    } catch (e: any) {
      showNote(String(e), true);
    }
  }
  function setActUi(on: boolean) {
    act.classList.toggle("stop", on);
    act.innerHTML = on
      ? `<span>${icon("square", 13)}</span>停止`
      : `<span>${icon("play", 13)}</span>开始播放`;
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
    moveThumb(dragSeek);
    api.seek(dragSeek);
  });
  seek.addEventListener("pointermove", (e) => {
    if (dragSeek < 0) return;
    dragSeek = fracOf(e, seek);
    moveThumb(dragSeek);
    api.seek(dragSeek);
  });
  seek.addEventListener("pointerup", () => {
    dragSeek = -1.0;
  });
  function moveThumb(frac: number) {
    ($("c-seekfill") as HTMLElement).style.width = `${frac * 100}%`;
    ($("c-seekthumb") as HTMLElement).style.left = `${frac * 100}%`;
  }

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
  window.addEventListener("page-changed", () => {
    if ((window as any).__currentPage !== "player") clearOsd();
  });

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
      if (st.snap && st.snap.v_bitrate > 0) lastVb = st.snap.v_bitrate;
      // 右上「开始/停止」跟随后端状态：播完（mpv 回 idle 且不在准备中）要回到「开始」，
      // 否则 running 只由点击维护，播到结尾按钮会一直卡在「停止」。
      const active = !!st.playing && !(st.snap?.idle && !st.preparing);
      if (active !== running) {
        running = active;
        setActUi(active);
      }
      render(st);
      // 起播瞬间按的 Tab：画面出来（不再 idle）就补上浮层
      if (osdPending && !osdOn && st.playing && st.snap && !st.snap.idle) {
        osdPending = false;
        osdRefreshAt = Date.now();
        showOsd();
      }
      // 节目下拉：播起来了才探 PAT/PMT；停了就收回去
      if (!st.playing || !st.snap || st.snap.idle) {
        if (progKey) {
          progKey = "";
          hideProg();
        }
      } else if (!st.bridge_to) {
        loadPrograms(url.value.trim(), nic.value);
      }
      // 本地文件预取媒体信息：状态栏要拿它标「AC-4 无解码」「H.266 需更新」这类
      {
        const src = url.value.trim();
        if (st.playing && src && !/^(udp|rtp|rtsp|rtmp|rtmps|https?):/i.test(src)) {
          void ensureMediaInfo(src);
        }
      }
      // Tab 浮层开着时刷新一次数字（码率/帧率/丢帧会变），2s 一次足够
      if (osdOn && Date.now() - osdRefreshAt > 2000) {
        osdRefreshAt = Date.now();
        showOsd();
      }
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

  // 浮层只在播放中有效：播完/停止时收掉，别把一块信息板留在画面上
  if (!st.playing) osdPending = false;
  if (!st.playing || (snap && snap.idle && !st.preparing)) clearOsd();

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
  const emptyTitle = $("v-empty-title");
  const emptyBtns = empty.querySelector(".empty-btns") as HTMLElement;
  if (!st.playing) {
    empty.style.display = "";
    emptyTitle.textContent = "还没有载入媒体";
    hint.textContent = "支持 TS / MP4 / MKV / FLV 本地文件（也可直接拖入窗口），或 UDP / RTP 组播地址";
    emptyBtns.style.display = "";
    (window as any).__muteCache = snap?.mute ?? false;
  } else if (st.preparing) {
    // 起播管线进行中（抽流/桥启动约数秒）
    empty.style.display = "";
    emptyTitle.textContent = "正在启动解码管线";
    hint.textContent = "桥转码启动中，数秒后出画…";
    emptyBtns.style.display = "none";
  } else if (snap && snap.idle) {
    empty.style.display = "";
    emptyTitle.textContent = "已播完";
    hint.textContent = "点上方 ▶ 重播，或换一个来源";
    emptyBtns.style.display = "none";
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
    const pct = seekable ? (pos / dur) * 100 : 0;
    ($("c-seekfill") as HTMLElement).style.width = `${pct}%`;
    ($("c-seekthumb") as HTMLElement).style.left = `${pct}%`;
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
  // mpv 是原生子窗，会盖住视频区内的 WebView 浮层 —— 状态一律走底部状态栏，这里不再放浮层
  tr.hidden = true;

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
      const hw = hwLabel(snap);
      const soft = snap.hwdec === "no" || !snap.hwdec;
      // 播放在跑且没走桥 = 直放；走了桥则由状态栏的桥条写明源→目标
      parts.push(
        bridged
          ? `<span class="chip-m">${hw}</span>`
          : `<span class="chip-m ok">● 直放</span><span class="chip-m ${soft ? "warn" : ""}">${snap.width}×${snap.height} · ${shortCodec(snap.video_codec)} · ${hw}</span>`,
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
      // AC-4 / H.266：mpv 侧没有解码器，播成无声/黑画面，必须说出来
      for (const w of codecWarns(snap))
        parts.push(`<span class="chip-m warn">${icon("alert", 11)} ${w}</span>`);
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
