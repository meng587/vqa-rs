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

// ---- 关于页：工具链路径 ----
api.toolPaths().then((t) => {
  const el = document.getElementById("ab-tools");
  if (!el) return;
  const row = (k: string, v: string | null) =>
    `<span class="k">${k}</span><span class="v">${v ?? "未找到"}</span>`;
  el.innerHTML =
    row("ffmpeg", t.ffmpeg) + row("ffprobe", t.ffprobe) + row("mpv", t.mpv) + row("davs2", t.davs2);
});

// ---- 页面初始化 ----
initPlayer();
initAnalysis();
goto("player");
