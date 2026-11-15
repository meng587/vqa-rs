//! 前端入口：图标装配 + 侧栏路由 + 倍速菜单。
//! 页面业务逻辑在 player.ts / analysis.ts（Phase 1/2 接入后端）。

import { icon } from "./icons";
import { initPlayer } from "./player";
import { initAnalysis } from "./analysis";
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
  if (el) el.textContent = `v${v}（Tauri/Web UI 版）`;
});

// ---- 关于页：工具链路径 ----
api.toolPaths().then((t) => {
  const el = document.getElementById("ab-tools");
  if (!el) return;
  const row = (k: string, v: string | null) =>
    `<span class="k">${k}</span><span class="v">${v ?? "未找到"}</span>`;
  el.innerHTML =
    row("ffmpeg", t.ffmpeg) + row("ffprobe", t.ffprobe) + row("mpv", t.mpv) + row("davs2", t.davs2);
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

api.diagHasToken().then((has) => {
  const tok = document.getElementById("dg-token") as HTMLInputElement;
  if (has) {
    tok.placeholder = "已保存私人令牌（如需更换可重新粘贴）";
  }
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
  const tokEl = document.getElementById("dg-token") as HTMLInputElement;
  dgStatus.textContent = "生成并上传中…";
  try {
    if (!diagCache) diagCache = await api.diagCollect();
    const url = await api.diagUpload(tokEl.value.trim() || null, diagCache.name, diagCache.content);
    dgStatus.innerHTML = `上传成功：<a href="${url}" target="_blank" style="color:var(--acc)">${url}</a>（可把此链接发给维护者；ZCode 会自动阅读并分析）`;
    tokEl.value = "";
    tokEl.placeholder = "已保存私人令牌（如需更换可重新粘贴）";
  } catch (e: any) {
    dgStatus.textContent = `上传失败：${String(e).slice(0, 200)}`;
  }
});

// ---- 页面初始化 ----
initPlayer();
initAnalysis();
goto("player");
