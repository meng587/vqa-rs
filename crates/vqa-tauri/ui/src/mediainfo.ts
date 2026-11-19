//! 媒体信息抽屉 —— ffprobe 全量探测（容器 + 每条流的编码/分辨率/帧率/码率/色彩）。
//! 入口：播放器页来源输入框右侧的 ⓘ 按钮，取「来源」里的地址探测，
//! 不管此刻有没有在播（没播也能先看片子是什么格式）。

import { api, MediaInfo } from "./api";
import { icon } from "./icons";
import { drawerHold } from "./drawer";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

let inited = false;
let lastPath = "";

export function initMediaInfo() {
  if (inited) return;
  inited = true;
  build();
  $("p-info").addEventListener("click", open);
}

function build() {
  const frag = document.createElement("div");
  frag.innerHTML = `
  <div class="drmask" id="mi-mask" hidden></div>
  <aside class="drawer" id="mi-drawer" hidden>
    <div class="drhead"><b>媒体信息</b>
      <button class="iconbtn txtbtn" id="mi-refresh" title="重新探测"></button>
      <button class="iconbtn drx txtbtn" id="mi-x" title="关闭"></button>
    </div>
    <div class="drbody" id="mi-body"></div>
  </aside>`;
  while (frag.firstElementChild) document.body.appendChild(frag.firstElementChild);
  $("mi-x").innerHTML = `${icon("x", 14)}<span class="btxt">关闭</span>`;
  $("mi-x").addEventListener("click", close);
  $("mi-refresh").innerHTML = `${icon("repeat", 14)}<span class="btxt">刷新</span>`;
  $("mi-refresh").addEventListener("click", () => {
    if (lastPath) probe(lastPath);
  });
  $("mi-mask").addEventListener("click", close);
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") close();
  });
  window.addEventListener("page-changed", close);
}

function close() {
  $("mi-mask").hidden = true;
  const d = $("mi-drawer");
  d.classList.remove("on");
  d.hidden = true;
  drawerHold("mi-drawer", false);
}

async function open() {
  const path = ($("p-url") as HTMLInputElement).value.trim();
  close();
  $("mi-mask").hidden = false;
  const d = $("mi-drawer");
  d.hidden = false;
  requestAnimationFrame(() => d.classList.add("on"));
  drawerHold("mi-drawer", true);
  const body = $("mi-body");
  if (!path) {
    body.innerHTML = `<div class="mi-hint">先在「来源」里填好文件路径或流地址，再看媒体信息。</div>`;
    return;
  }
  await probe(path);
}

async function probe(path: string) {
  lastPath = path;
  const body = $("mi-body");
  body.innerHTML = `<div class="mi-hint">探测中…（ffprobe 正在分析）<br>${esc(path)}</div>`;
  try {
    const info = await api.mediaInfo(path);
    body.innerHTML = render(info);
  } catch (e: any) {
    body.innerHTML = `<div class="mi-hint">探测失败：${esc(String(e))}</div>`;
  }
}

// ═══════════════════ 渲染 ═══════════════════

function esc(s: string) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function row(k: string, v: string | null | undefined) {
  if (!v || v === "?") return "";
  return `<div class="mi-row"><span class="mi-k">${k}</span><span class="mi-v">${esc(v)}</span></div>`;
}

function fps(x: string | null) {
  if (!x) return null;
  const [a, b] = x.split("/").map(Number);
  if (!a || !b) return null;
  const f = a / b;
  return `${f.toFixed(3)} fps${b !== 1 ? `（${x}）` : ""}`;
}

function mbps(bps: number | null) {
  if (!bps || bps <= 0) return null;
  return `${(bps / 1e6).toFixed(2)} Mbps`;
}

function size(bytes: number | null) {
  if (!bytes || bytes <= 0) return null;
  return bytes >= 1048576
    ? `${(bytes / 1048576).toFixed(1)} MB（${bytes} 字节）`
    : `${bytes} 字节`;
}

function hms(sec: number | null) {
  if (sec == null || sec <= 0) return null;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  return `${h ? `${h}:` : ""}${String(m).padStart(2, "0")}:${s.toFixed(2)}（${sec.toFixed(3)} s）`;
}

function colorLine(v: MediaInfo["video"][number]) {
  const parts = [v.color_space, v.color_transfer, v.color_primaries].filter(
    (x) => x && x !== "unknown"
  );
  return parts.length ? parts.join(" / ") : null;
}

function render(info: MediaInfo) {
  const f = info.format;
  const video = info.video;
  const audio = info.audio;
  let html = "";

  html += `<div class="mi-sec">容器 / 总体</div>`;
  html += row("来源", info.path);
  html += row("容器", f.container);
  html += row("格式全称", f.format_long);
  html += row("时长", hms(f.duration_s));
  html += row("总码率", mbps(f.bit_rate));
  html += row("体积", size(f.size_bytes));
  html += row(
    "流数量",
    `${f.nb_streams} 条（视频 ${video.length} · 音频 ${audio.length} · 其他 ${info.others.length}）`
  );
  html += row("探测评分", f.probe_score != null ? `${f.probe_score}` : null);

  video.forEach((v, i) => {
    const codec = [v.codec, v.profile, v.level != null ? `L${v.level}` : null]
      .filter(Boolean)
      .join(" · ");
    html += `<div class="mi-sec">视频流 #${v.index ?? i}${
      video.length > 1 ? `（第 ${i + 1} 条）` : ""
    }</div>`;
    html += row("编码", codec || null);
    html += row(
      "分辨率",
      v.width && v.height ? `${v.width}×${v.height}` : null
    );
    html += row("像素格式", v.pix_fmt);
    html += row("帧率", fps(v.avg_frame_rate) ?? fps(v.r_frame_rate));
    html += row("原始帧率", v.r_frame_rate ? fps(v.r_frame_rate) : null);
    html += row("码率", mbps(v.bit_rate));
    html += row("色彩", colorLine(v));
    html += row("扫描方式", v.field_order && v.field_order !== "unknown" ? v.field_order : null);
    html += row(
      "宽高比",
      [v.display_aspect_ratio, v.sample_aspect_ratio && `SAR ${v.sample_aspect_ratio}`]
        .filter(Boolean)
        .join(" · ") || null
    );
    html += row("帧数", v.nb_frames != null ? `${v.nb_frames}` : null);
    html += row("语言", v.language);
  });

  audio.forEach((a, i) => {
    html += `<div class="mi-sec">音频流 #${a.index ?? i}${
      audio.length > 1 ? `（第 ${i + 1} 条）` : ""
    }</div>`;
    html += row("编码", [a.codec, a.profile].filter(Boolean).join(" · ") || null);
    html += row(
      "采样率",
      a.sample_rate ? `${a.sample_rate} Hz` : null
    );
    html += row(
      "声道",
      [a.channels != null ? `${a.channels} 声道` : null, a.channel_layout]
        .filter(Boolean)
        .join(" · ") || null
    );
    html += row("码率", mbps(a.bit_rate));
    html += row("语言", a.language);
  });

  if (info.others.length) {
    html += `<div class="mi-sec">其他流</div>`;
    for (const o of info.others) {
      html += row(`#${o.index}`, [o.type, o.codec, o.language].filter(Boolean).join(" · "));
    }
  }

  html += `<div class="mi-sec">原始 ffprobe JSON</div>`;
  html += `<details><summary class="mi-hint" style="cursor:pointer">展开查看完整字段</summary>
    <pre class="mi-raw">${esc(JSON.stringify(info.raw, null, 2))}</pre></details>`;

  return html;
}
