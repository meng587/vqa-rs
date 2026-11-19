//! 前端入口：图标装配 + 侧栏路由 + 倍速菜单。
//! 页面业务逻辑在 player.ts / analysis.ts（Phase 1/2 接入后端）。

import { icon } from "./icons";
import { initPlayer } from "./player";
import { initAnalysis } from "./analysis";
import { initCompliance } from "./compliance";
import { initXcode } from "./xcode";
import { initBurst } from "./burst";
import { initAvSync, leaveAvSync } from "./avsync";
import { initConsist, leaveConsist, enterConsist } from "./consist";
import { initAlarm, setAlarmViewActive } from "./alarm";
import { initSettings } from "./settings";
import { initMediaInfo } from "./mediainfo";
import { api } from "./api";
import { SPEEDS } from "./constants";
import { watchAddressInputs } from "./address";

export { SPEEDS };
export let speedIdx = 2;

// ---- data-ic 图标装配 ----
document.querySelectorAll<HTMLElement>("[data-ic]").forEach((el) => {
  const name = el.getAttribute("data-ic")!;
  el.innerHTML = icon(name, el.classList.contains("logo") ? 17 : 15);
});

// ---- 侧栏路由 ----
const pages = document.querySelectorAll<HTMLElement>(".pg");
export type Page = "player" | "analysis" | "compliance" | "transcode" | "burst" | "avsync" | "consist" | "about";
type ConsistView = "compare" | "alarm";

/** 页头文案：只做壳层，页面内保留唯一的主操作与参数入口。 */
const PAGE_CHROME: Record<Page, { kicker: string; title: string; sub: string; seal: string }> = {
  player: { kicker: "PLAYER & MONITOR", title: "播放器", sub: "本地文件 / UDP 组播实时监看", seal: "播" },
  analysis: { kicker: "STREAM ANALYSIS", title: "视频分析", sub: "TS 结构 / PCR / TR101 290 / 码率曲线", seal: "析" },
  compliance: { kicker: "COMPLIANCE CHECK", title: "合规检测", sub: "依据 广电办发〔2024〕253 号 逐项判定", seal: "规" },
  transcode: { kicker: "LIVE TRANSCODE", title: "直播转码", sub: "输入 → 解码 → 转码 → 输出 全链路", seal: "转" },
  burst: { kicker: "BURST DETECT", title: "微突发检测", sub: "按设定时间窗检测瞬时拥塞", seal: "检" },
  avsync: { kicker: "A/V SYNC", title: "音画同步", sub: "音频包络与视频帧对齐，测量偏移", seal: "音" },
  consist: { kicker: "CONSISTENCY", title: "一致性比对", sub: "同频道多来源双画面逐帧比对", seal: "鉴" },
  about: { kicker: "ABOUT", title: "关于", sub: "版本 / 更新 / 诊断与工具链", seal: "关" },
};

function updatePageChrome(page: Page) {
  const meta = PAGE_CHROME[page];
  const kicker = document.getElementById("ph-kicker");
  const title = document.getElementById("ph-title");
  const sub = document.getElementById("ph-sub");
  const seal = document.getElementById("ph-seal");
  if (kicker) kicker.textContent = meta.kicker;
  if (title) title.textContent = meta.title;
  if (sub) sub.textContent = meta.sub;
  if (seal) seal.textContent = meta.seal;
  document.title = `${meta.title} · 视听质量检测工作站`;
}

/** 一致性页内部子视图：比对工作台 / 告警处理。 */
function showConsistView(view: ConsistView) {
  const cmp = document.getElementById("cg-view-compare");
  const alm = document.getElementById("cg-view-alarm");
  if (cmp) cmp.hidden = view !== "compare";
  if (alm) alm.hidden = view !== "alarm";
  document.querySelectorAll<HTMLElement>("[data-cgview]").forEach((b) => {
    b.classList.toggle("is-on", b.dataset.cgview === view);
  });
  document.getElementById("cg-acts-compare")?.toggleAttribute("hidden", view !== "compare");
  document.getElementById("cg-acts-alarm")?.toggleAttribute("hidden", view !== "alarm");
  if (view === "alarm") {
    leaveConsist();
    setAlarmViewActive(true);
  } else {
    setAlarmViewActive(false);
    enterConsist();
  }
}

function goto(page: Page) {
  const prev = document.body.dataset.page;
  // 离开这两个页时停掉预览 ffmpeg，避免后台一直占用一路解码
  if (prev === "avsync") leaveAvSync();
  if (prev === "consist") {
    setAlarmViewActive(false);
    leaveConsist();
  }
  document.querySelectorAll<HTMLElement>("#nav .mn").forEach((b) => {
    b.classList.toggle("is-on", b.getAttribute("data-goto") === page);
  });
  pages.forEach((p) => {
    p.hidden = p.getAttribute("data-page") !== page;
  });
  // 状态栏按页切换：播放器页显示指标与桥说明，其它页只显示页名
  document.body.dataset.page = page;
  if (page === "consist") showConsistView("compare");
  updatePageChrome(page);
  const sbTitle = document.getElementById("sb-title");
  if (sbTitle) {
    sbTitle.textContent =
      page === "player"
        ? "播放器"
        : page === "analysis"
          ? "视频分析"
          : page === "compliance"
            ? "合规检测"
            : page === "transcode"
              ? "直播转码"
              : page === "burst"
                ? "微突发检测"
                : page === "avsync"
                  ? "音画同步"
                  : page === "consist"
                    ? "一致性比对"
                    : "关于";
  }
  (window as any).__currentPage = page;
  window.dispatchEvent(new CustomEvent("page-changed", { detail: page }));
}

/** nav 行状态灯：idle=熄 / running=绿闪 / error=红。 */
export function setNavLamp(page: "player" | "analysis" | "compliance" | "transcode" | "burst" | "avsync" | "consist", state: "idle" | "running" | "error") {
  const lamp = document.getElementById(`lamp-${page}`);
  if (!lamp) return;
  lamp.className = `lamp${state === "running" ? " run" : state === "error" ? " err" : ""}`;
}

/** 左栏底部引擎状态点（mpv 存活 = 绿）。 */
export function setEngineLamp(ok: boolean) {
  const dot = document.getElementById("engine-dot");
  if (dot) dot.style.background = ok ? "var(--ok)" : "var(--bad)";
}

document.querySelectorAll<HTMLElement>("#nav .mn[data-goto]").forEach((btn) => {
  btn.addEventListener("click", () => goto(btn.getAttribute("data-goto") as Page));
});

// ---- 左栏收起 / 展开（表格页可把宽度让给内容）----
const SIDEBAR_KEY = "vqa-sidebar-collapsed";
const sidebar = document.getElementById("sidebar");
const sideToggle = document.getElementById("side-collapse");

function setSidebarCollapsed(collapsed: boolean, persist = true) {
  document.body.classList.toggle("side-collapsed", collapsed);
  if (sideToggle) {
    sideToggle.setAttribute("aria-expanded", String(!collapsed));
    sideToggle.title = collapsed ? "展开导航" : "收起导航";
    sideToggle.setAttribute("aria-label", sideToggle.title);
  }
  sidebar?.classList.toggle("is-collapsed", collapsed);
  if (persist) {
    try { localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0"); } catch { /* WebView 隐私模式 */ }
  }
}

let sidebarSaved = false;
try { sidebarSaved = localStorage.getItem(SIDEBAR_KEY) === "1"; } catch { /* 默认展开 */ }
setSidebarCollapsed(sidebarSaved, false);
sideToggle?.addEventListener("click", () => {
  setSidebarCollapsed(!document.body.classList.contains("side-collapsed"));
});

updatePageChrome("player");

document.querySelectorAll<HTMLElement>("[data-cgview]").forEach((btn) => {
  btn.addEventListener("click", () => showConsistView(btn.dataset.cgview as ConsistView));
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
  speedBtn.innerHTML = `<span class="btxt">倍速</span><span id="c-speedval">${SPEEDS[speedIdx]}x</span><span class="caret">▾</span>`;
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
  const tb = document.getElementById("tb-ver");
  if (tb) tb.textContent = `v${v}`;
  const sv = document.getElementById("side-ver");
  if (sv) sv.textContent = `v${v} / LOCAL`;
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
const upBar = document.getElementById("up-bar") as HTMLElement;
const upFill = document.getElementById("up-fill") as HTMLElement;
const upMeta = document.getElementById("up-meta") as HTMLElement;
const upHint = document.getElementById("up-hint") as HTMLElement;

function upMb(n: number): string {
  return (n / 1048576).toFixed(1);
}
function upDur(sec: number): string {
  if (!Number.isFinite(sec) || sec <= 0) return "—";
  sec = Math.round(sec);
  if (sec < 60) return `${sec} 秒`;
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  if (m < 60) return `${m} 分 ${s} 秒`;
  const h = Math.floor(m / 60);
  return `${h} 小时 ${m % 60} 分`;
}
function upClear(): void {
  upBar.hidden = true;
  upMeta.hidden = true;
  upBar.classList.remove("is-indeterminate");
  upMeta.classList.remove("is-slow");
  upFill.style.width = "0%";
}

if (import.meta.env.MODE !== "dev") {
  upBtn.addEventListener("click", async () => {
    upBtn.disabled = true;
    upInstall.hidden = true;
    upClear();
    upStatus.textContent = "正在检查更新…（网络较慢时请稍候）";
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      // 检查请求也加超时：Gitee 偶尔连不上时，不能让界面永远停在
      // “正在检查”，至少明确告诉用户是网络问题。
      const upd = await Promise.race([
        check({ timeout: 20000 }),
        new Promise<null>((_, reject) =>
          setTimeout(() => reject(new Error("检查超时（20 秒），请检查网络后重试")), 22000),
        ),
      ]);
      if (!upd) {
        upStatus.textContent = "当前已是最新版本 ✓";
        upHint.textContent = "没有发现新版本。";
        upBtn.disabled = false;
        return;
      }
      upStatus.innerHTML = `发现新版本 <b style="color:var(--acc)">${upd.version}</b>`;
      upHint.textContent = upd.body
        ? `${upd.version} 更新说明：${upd.body}`
        : `检测到 ${upd.version}，可以下载安装。`;
      upInstall.hidden = false;
      upInstall.disabled = false;
      upInstall.onclick = async () => {
        upInstall.disabled = true;
        upBtn.disabled = true;
        upBar.hidden = false;
        upMeta.hidden = false;
        upBar.classList.remove("is-indeterminate");
        upStatus.textContent = `正在下载 ${upd.version}…`;
        upMeta.textContent = "正在连接下载服务器…";
        upFill.style.width = "0%";

        let totalBytes = 0;
        let receivedBytes = 0;
        let startedAt = performance.now();
        let lastChunkAt = startedAt;
        let lastPaint = 0;
        let finished = false;
        let ticker = 0;

        const paint = (force = false) => {
          const now = performance.now();
          if (!force && now - lastPaint < 150) return;
          lastPaint = now;
          const elapsed = Math.max(0.1, (now - startedAt) / 1000);
          const ratio = totalBytes > 0
            ? Math.min(1, receivedBytes / totalBytes)
            : 0;
          const percent = totalBytes > 0 ? `${(ratio * 100).toFixed(1)}%` : "";
          const speed = receivedBytes > 0 ? receivedBytes / elapsed : 0;
          const remain = totalBytes > 0 && speed > 0
            ? (totalBytes - receivedBytes) / speed
            : 0;
          // 还没有拿到总长度时不画固定比例，改成来回走的进度条，
          // 让用户明确知道程序在下载而不是卡死。
          upBar.classList.toggle("is-indeterminate", totalBytes <= 0 && !finished);
          upFill.style.width = finished
            ? "100%"
            : totalBytes > 0
              ? `${(ratio * 100).toFixed(1)}%`
              : "35%";
          const left = totalBytes > 0
            ? `已接收 ${upMb(receivedBytes)} / ${upMb(totalBytes)} MB（${percent}）`
            : `已接收 ${upMb(receivedBytes)} MB`;
          let right = speed > 0 ? `${upMb(speed)} MB/s` : "等待数据…";
          if (remain > 0) right += ` · 剩余约 ${upDur(remain)}`;
          const silence = (now - lastChunkAt) / 1000;
          if (!finished && silence >= 8) {
            right = `网络已 ${upDur(silence)} 无新数据，仍在等待…`;
            upMeta.classList.add("is-slow");
          } else {
            upMeta.classList.remove("is-slow");
          }
          upMeta.innerHTML =
            `<span>${left}</span><span class="upmeta-right">${right}</span>`;
        };

        ticker = window.setInterval(() => paint(true), 1000);

        try {
          await upd.downloadAndInstall((ev) => {
            if (ev.event === "Started") {
              totalBytes = Number(ev.data.contentLength) || 0;
              receivedBytes = 0;
              startedAt = lastChunkAt = performance.now();
              upStatus.textContent = totalBytes > 0
                ? `正在下载 ${upd.version} · 共 ${upMb(totalBytes)} MB`
                : `正在下载 ${upd.version}…`;
              paint(true);
            } else if (ev.event === "Progress") {
              receivedBytes += Number(ev.data.chunkLength) || 0;
              lastChunkAt = performance.now();
              upStatus.textContent = `正在下载 ${upd.version}…`;
              paint(false);
            } else if (ev.event === "Finished") {
              finished = true;
              if (totalBytes > 0) receivedBytes = totalBytes;
              paint(true);
              upStatus.textContent = "下载完成，正在校验并安装…（程序随后会自动重启）";
              upMeta.textContent = `已下载 ${upMb(receivedBytes)} MB · 正在安装`;
            }
          }, { timeout: 15 * 60 * 1000 });
          window.clearInterval(ticker);
          upStatus.textContent = "安装程序已启动，程序正在重启…";
          const { relaunch } = await import("@tauri-apps/plugin-process");
          await relaunch();
        } catch (e: any) {
          window.clearInterval(ticker);
          upBar.classList.remove("is-indeterminate");
          upMeta.classList.remove("is-slow");
          upStatus.textContent = `更新失败：${e}`;
          upMeta.textContent = `已接收 ${upMb(receivedBytes)} MB，可重新点击「下载并安装」重试`;
          upInstall.disabled = false;
          upBtn.disabled = false;
        }
      };
      upBtn.disabled = false;
    } catch (e: any) {
      upStatus.textContent = `检查失败：${e}`;
      upHint.textContent = "请确认网络可以访问 gitee.com，然后重试。";
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
initSettings();
initPlayer();
initAnalysis();
initCompliance();
initXcode();
initBurst();
initAvSync();
initConsist();
initAlarm();
initMediaInfo();
watchAddressInputs();
// 空态里的「选择 TS 文件」复用顶栏的浏览动作
document.getElementById("a-empty-browse")?.addEventListener("click", () =>
  (document.getElementById("a-browse") as HTMLElement | null)?.click(),
);
goto("player");
