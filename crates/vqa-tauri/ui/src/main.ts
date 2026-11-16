//! 前端入口：图标装配 + 侧栏路由 + 倍速菜单。
//! 页面业务逻辑在 player.ts / analysis.ts（Phase 1/2 接入后端）。

import { icon } from "./icons";
import { initPlayer } from "./player";
import { initAnalysis } from "./analysis";
import { initSettings } from "./settings";
import { initMediaInfo } from "./mediainfo";
import { api } from "./api";
import { SPEEDS } from "./constants";

export { SPEEDS };
export let speedIdx = 2;

// ---- data-ic 图标装配 ----
document.querySelectorAll<HTMLElement>("[data-ic]").forEach((el) => {
  const name = el.getAttribute("data-ic")!;
  el.innerHTML = icon(name, el.classList.contains("logo") ? 17 : 15);
});

// ---- 侧栏路由 ----
const pages = document.querySelectorAll<HTMLElement>(".pg");
export type Page = "player" | "analysis" | "about";

function goto(page: Page) {
  document.querySelectorAll<HTMLElement>("#nav .mn").forEach((b) => {
    b.classList.toggle("is-on", b.getAttribute("data-goto") === page);
  });
  pages.forEach((p) => {
    p.hidden = p.getAttribute("data-page") !== page;
  });
  // 状态栏按页切换：播放器页显示指标与桥说明，其它页只显示页名
  document.body.dataset.page = page;
  const sbTitle = document.getElementById("sb-title");
  if (sbTitle) {
    sbTitle.textContent =
      page === "player" ? "播放器" : page === "analysis" ? "视频分析" : "关于";
  }
  (window as any).__currentPage = page;
  window.dispatchEvent(new CustomEvent("page-changed", { detail: page }));
}

/** nav 行状态灯：idle=熄 / running=绿闪 / error=红。 */
export function setNavLamp(page: "player" | "analysis", state: "idle" | "running" | "error") {
  const lamp = document.getElementById(`lamp-${page}`);
  if (!lamp) return;
  lamp.className = `lamp${state === "running" ? " run" : state === "error" ? " err" : ""}`;
}

/** 页脚引擎状态点（mpv 存活 = 绿）。 */
export function setEngineLamp(ok: boolean) {
  const dot = document.querySelector<HTMLElement>(".engine .dot");
  if (dot) dot.style.background = ok ? "var(--ok)" : "var(--bad)";
}

document.querySelectorAll<HTMLElement>("#nav .mn[data-goto]").forEach((btn) => {
  btn.addEventListener("click", () => goto(btn.getAttribute("data-goto") as Page));
});

// ---- 倍速菜单 ----
const speedBtn = document.getElementById("c-speedbtn") as HTMLButtonElement;
const speedMenu = document.getElementById("c-speedmenu") as HTMLElement;

function renderSpeedMenu() {
  speedMenu.innerHTML = SPEEDS.map(
    (s, i) =>
      `<button data-i="${i}" class="${i === speedIdx ? "on" : ""}">${s}x</button>`,
  ).join("");
}
renderSpeedMenu();

speedBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  speedMenu.hidden = !speedMenu.hidden;
});
speedMenu.addEventListener("click", (e) => {
  const t = (e.target as HTMLElement).closest("button[data-i]") as HTMLButtonElement | null;
  if (!t) return;
  speedIdx = Number(t.getAttribute("data-i"));
  speedBtn.textContent = `${SPEEDS[speedIdx]}x ▾`;
  speedMenu.hidden = true;
  window.dispatchEvent(new CustomEvent("speed-changed", { detail: SPEEDS[speedIdx] }));
});
document.addEventListener("click", () => {
  speedMenu.hidden = true;
  document.querySelectorAll<HTMLElement>(".histpop").forEach((p) => (p.hidden = true));
});

// ---- 关于页：版本号（单一来源 = Cargo package version）----
api.appVersion().then((v) => {
  const el = document.getElementById("ab-ver");
  if (el) el.textContent = `v${v}`;
});

// ---- 路径行：单行省略 + 复制 / 打开 ----
const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** 网卡下拉显示文案：`IP · 网卡名`，断开的补「未连接」；空描述则只显示 IP */
export function nicLabel(n: { ip: string; name: string; desc: string; up: boolean }): string {
  const nm = (n.name || n.desc || "").trim();
  const base = nm ? `${n.ip} · ${nm}` : n.ip;
  return n.up ? base : `${base}（未连接）`;
}

/** 长路径取中段省略（保留盘符与文件名），完整值仍放在 title 里 */
function shortPath(p: string | null): string {  if (!p) return "未找到";
  if (p.length <= 62) return p;
  return `${p.slice(0, 26)}…${p.slice(-30)}`;
}

function pathRow(k: string, p: string | null, cls = ""): string {
  const full = p ?? "";
  const copy = full
    ? `<button class="oicon" data-copy="${esc(full)}" title="复制路径">${icon("copy", 13)}</button>`
    : "";
  const open = full
    ? `<button class="oicon" data-open="${esc(full)}" title="打开所在目录">${icon("folder", 13)}</button>`
    : "";
  return (
    `<div class="orow"><span class="k">${esc(k)}</span>` +
    `<span class="v mono ${cls}" title="${esc(p ?? "未找到")}">${esc(shortPath(p))}</span>${copy}${open}</div>`
  );
}

/** 复制文本（优先剪贴板 API，退回 execCommand，兼容 WebView2） */
async function copyText(t: string) {
  try {
    await navigator.clipboard.writeText(t);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = t;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  }
}

/** 行内复制/打开按钮的事件委托 */
function bindRowActions(root: HTMLElement, onMsg?: (m: string) => void) {
  root.addEventListener("click", async (e) => {
    const t = (e.target as HTMLElement).closest("button") as HTMLButtonElement | null;
    if (!t) return;
    const cp = t.getAttribute("data-copy");
    const op = t.getAttribute("data-open");
    if (cp) {
      const ok = await copyText(cp);
      onMsg?.(ok ? "已复制路径" : "复制失败，可手动选中");
    } else if (op) {
      try {
        await api.reveal(op);
      } catch (e2: any) {
        onMsg?.(`打开失败：${e2}`);
      }
    }
  });
}

// ---- 关于页：工具链路径 ----
api.toolPaths().then((t) => {
  const el = document.getElementById("ab-tools");
  if (!el) return;
  el.innerHTML =
    pathRow("ffmpeg", t.ffmpeg) +
    pathRow("ffprobe", t.ffprobe) +
    pathRow("mpv", t.mpv) +
    pathRow("davs2", t.davs2);
  bindRowActions(el, (m) => {
    const hint = document.getElementById("up-hint");
    if (hint) hint.textContent = m;
  });
});

// ---- 关于页：数据与日志目录 ----
api.appPaths().then((p) => {
  const el = document.getElementById("ab-paths");
  if (!el) return;
  el.innerHTML =
    pathRow("应用数据", p.shared) +
    pathRow("运行日志", p.log_file) +
    pathRow("临时目录", p.scratch) +
    pathRow("快照目录", p.snap) +
    pathRow("录像目录", p.rec);
  bindRowActions(el, (m) => {
    const hint = document.getElementById("up-hint");
    if (hint) hint.textContent = m;
  });
  const btn = document.getElementById("ab-open-data");
  btn?.addEventListener("click", () => api.reveal(p.data).catch(() => {}));
});

// ---- 关于页：在线更新 ----
const upStatus = document.getElementById("up-status") as HTMLElement;
const upBtn = document.getElementById("up-check") as HTMLButtonElement;
const upInstall = document.getElementById("up-install") as HTMLButtonElement;

if (import.meta.env.MODE !== "dev") {
  upBtn.addEventListener("click", async () => {
    upBtn.disabled = true;
    upStatus.textContent = "正在检查…";
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const upd = await check();
      if (!upd) {
        upStatus.textContent = "当前已是最新版本 ✓";
        upBtn.disabled = false;
        return;
      }
      upStatus.innerHTML = `发现新版本 <b style="color:var(--acc)">${upd.version}</b>`;
      upInstall.hidden = false;
      upInstall.onclick = async () => {
        upInstall.disabled = true;
        upStatus.textContent = "下载中…（完成后自动重启安装）";
        try {
          let last = 0;
          await upd.downloadAndInstall((ev) => {
            if (ev.event === "Started" && ev.data.contentLength) {
              last = ev.data.contentLength;
              upStatus.textContent = `下载中… 0 / ${(last / 1048576).toFixed(1)} MB`;
            } else if (ev.event === "Progress" && ev.data.chunkLength) {
              // 简化：不做逐块累计 UI，避免频繁 DOM 更新
            } else if (ev.event === "Finished") {
              upStatus.textContent = `下载完成（${(last / 1048576).toFixed(1)} MB），正在重启安装…`;
            }
          });
          const { relaunch } = await import("@tauri-apps/plugin-process");
          await relaunch();
        } catch (e: any) {
          upStatus.textContent = `更新失败：${e}`;
          upInstall.disabled = false;
        }
      };
      upBtn.disabled = false;
    } catch (e: any) {
      upStatus.textContent = `检查失败：${e}`;
      upBtn.disabled = false;
    }
  });
} else {
  upBtn.disabled = true;
  upStatus.textContent = "dev 模式不支持在线更新（构建版可用）";
}

// ---- 关于页：诊断日志 ----
let diagCache: { name: string; content: string } | null = null;
const dgStatus = document.getElementById("dg-status") as HTMLElement;

api.diagHasToken().then(() => {
  // 令牌已保存则不再在界面暴露输入框（改用「上传诊断包给开发者」按钮）
});

// 「包含什么 ▸」展开/收起
document.getElementById("dg-more")?.addEventListener("click", () => {
  const d = document.getElementById("dg-detail") as HTMLElement;
  const more = document.getElementById("dg-more") as HTMLElement;
  const opening = d.hidden;
  d.hidden = !opening;
  more.textContent = opening ? "收起 ▴" : "包含什么 ▸";
});

document.getElementById("dg-export")?.addEventListener("click", async () => {
  dgStatus.textContent = "生成中…";
  try {
    diagCache = await api.diagCollect();
    const p = await api.diagExport(diagCache.name, diagCache.content);
    dgStatus.textContent = p ? `已导出：${p}` : "已取消";
  } catch (e: any) {
    dgStatus.textContent = `导出失败：${e}`;
  }
});

document.getElementById("dg-upload")?.addEventListener("click", async () => {
  dgStatus.textContent = "生成并上传中…";
  try {
    if (!diagCache) diagCache = await api.diagCollect();
    // 上传凭据不在界面上暴露，直接用本机已保存的
    const url = await api.diagUpload(null, diagCache.name, diagCache.content);
    dgStatus.innerHTML = `已上传给开发者：<a href="${url}" target="_blank" style="color:var(--acc)">${url}</a>`;
  } catch (e: any) {
    dgStatus.textContent = `上传失败：${String(e).slice(0, 200)}`;
  }
});

// ---- 页面初始化 ----
initPlayer();
initAnalysis();
initSettings();
initMediaInfo();
// 空态里的「选择 TS 文件」复用顶栏的浏览动作
document.getElementById("a-empty-browse")?.addEventListener("click", () =>
  (document.getElementById("a-browse") as HTMLElement | null)?.click(),
);
goto("player");
