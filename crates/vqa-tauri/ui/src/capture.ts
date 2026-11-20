//! 抓包分析页：绑定一路 UDP/RTP 业务流，写 pcap / TS 样本，并展示网络统计。
//! 抓包线程在后端独立运行，离开页面不自动停止；导航灯持续显示运行状态。

import { api, downloadUrl, isWebMode, webDownloadUrl, type CapturePacket, type CaptureSnapshot, type NicRow } from "./api";
import { icon } from "./icons";
import { setNavLamp } from "./main";

const $ = <T extends HTMLElement>(id: string): T => document.getElementById(id) as T;

let snap: CaptureSnapshot | null = null;
let active = false;
let busy = false;
let pollTimer = 0;
let localError = "";
const CAPTURE_PREF_KEY = "capture.prefs";

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function fmtBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtTimer(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const hh = Math.floor(s / 3600);
  const mm = Math.floor((s % 3600) / 60);
  const ss = s % 60;
  return hh > 0
    ? `${String(hh).padStart(2, "0")}:${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}`
    : `${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}`;
}

function fmtTs(ms: number): string {
  const total = Math.max(0, ms);
  const mm = Math.floor(total / 60000);
  const ss = Math.floor((total % 60000) / 1000);
  const mmm = Math.floor(total % 1000);
  return `${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}.${String(mmm).padStart(3, "0")}`;
}

function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function setText(id: string, text: string): void {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
}

function setBusyUi(): void {
  const btn = $<HTMLButtonElement>("c-act");
  const running = !!snap?.running;
  btn.disabled = busy;
  btn.classList.toggle("stop", running);
  btn.innerHTML = busy
    ? `<span class="cap-spin"></span>${running ? "停止中…" : "启动中…"}`
    : running
      ? `${icon("square")}停止抓包`
      : `${icon("scanline")}开始抓包`;
  const url = $<HTMLInputElement>("c-url");
  const nic = $<HTMLSelectElement>("c-nic");
  url.disabled = running || busy;
  nic.disabled = running || busy;
}

function setError(msg: string): void {
  localError = msg;
  const box = $<HTMLElement>("c-err");
  box.hidden = !msg;
  box.textContent = msg;
}

function setDot(state: "idle" | "run" | "err"): void {
  const dot = $<HTMLElement>("c-dot");
  dot.className = `cap-dot${state === "run" ? " is-run" : state === "err" ? " is-err" : ""}`;
}

function renderRows(packets: CapturePacket[]): void {
  const tbody = $<HTMLElement>("c-rows");
  const empty = $<HTMLElement>("c-empty");
  const wrap = tbody.closest(".cap-tblwrap") as HTMLElement | null;
  const keep = wrap?.scrollTop ?? 0;
  empty.hidden = packets.length > 0;
  const rows = packets
    .slice()
    .reverse()
    .map((p) => {
      const kindCls = p.kind === "RTP" ? " is-rtp" : p.kind === "TS" ? " is-ts" : "";
      return (
        `<tr>` +
        `<td class="cap-mono">${fmtTs(p.ts_ms)}</td>` +
        `<td class="cap-mono">${esc(p.src)}</td>` +
        `<td class="cap-mono">${esc(p.dst)}</td>` +
        `<td class="cap-mono cap-num">${p.len}</td>` +
        `<td><span class="cap-kindtag${kindCls}">${esc(p.kind)}</span></td>` +
        `<td class="cap-sum">${esc(p.summary)}</td>` +
        `</tr>`
      );
    })
    .join("");
  tbody.innerHTML = rows;
  if (wrap) wrap.scrollTop = keep;
}

function render(s: CaptureSnapshot | null): void {
  snap = s;
  const running = !!s?.running;
  const error = localError || s?.error || "";
  setError(error);

  if (!s) {
    setDot("idle");
    setText("c-state", "待机");
    setText("c-state-sub", "填写 UDP / RTP 地址后开始抓包");
    setText("c-timer", "00:00");
    setText("c-live-url", "—");
    setText("c-live-nic", "—");
    setText("c-rtp", "—");
    setText("c-pcap-name", "尚未生成");
    setText("c-ts-name", "尚未生成");
    setText("c-pcap-size", "0 B");
    setText("c-ts-size", "0 B");
    setText("c-outdir", "—");
    setText("c-bitrate", "—");
    setText("c-jitter", "—");
    setText("c-loss", "—");
    setText("c-df", "—");
    setText("c-mlr", "—");
    setText("c-pkts", "—");
    setText("c-total", "0 包");
    renderRows([]);
    disableFileActions(true, true);
    setBusyUi();
    setNavLamp("capture", error ? "error" : "idle");
    return;
  }

  setDot(error ? "err" : running ? "run" : "idle");
  setText("c-state", error && !running ? "抓包异常" : running ? "抓包中" : "已停止");
  setText(
    "c-state-sub",
    error
      ? error
      : running
        ? `正在接收 ${s.url}`
        : s.net.packets > 0
          ? `共收 ${s.net.packets} 个数据报，产物已落盘`
          : "未收到数据，检查地址、网卡或组播权限",
  );
  setText("c-timer", fmtTimer(s.started_secs));
  setText("c-live-url", s.url || "—");
  setText("c-live-nic", s.nic_ip ? `网卡 ${s.nic_ip}` : "自动选择");
  setText("c-rtp", s.net.packets > 0 ? (s.net.rtp ? "RTP / UDP" : "UDP（未识别 RTP）") : "—");

  setText("c-pcap-name", s.pcap_path ? baseName(s.pcap_path) : "尚未生成");
  setText("c-ts-name", s.ts_path ? baseName(s.ts_path) : "尚未生成");
  const pcapName = document.getElementById("c-pcap-name");
  if (pcapName) pcapName.title = s.pcap_path || "";
  const tsName = document.getElementById("c-ts-name");
  if (tsName) tsName.title = s.ts_path || "";
  setText("c-pcap-size", fmtBytes(s.pcap_bytes));
  setText("c-ts-size", fmtBytes(s.ts_bytes));
  setText("c-outdir", s.out_dir || "—");
  const outDir = document.getElementById("c-outdir");
  if (outDir) outDir.title = s.out_dir || "";

  const hasNet = s.net.packets > 0;
  setText("c-bitrate", hasNet ? s.net.bitrate_mbps.toFixed(2) : "—");
  setText("c-jitter", hasNet ? s.net.jitter_ms.toFixed(1) : "—");
  setText("c-loss", s.net.rtp ? (s.net.loss_ratio * 100).toFixed(2) : "—");
  setText("c-df", hasNet ? s.net.df_bad_pct.toFixed(3) : "—");
  setText("c-mlr", s.net.rtp ? s.net.mlr_good_pct.toFixed(2) : "—");
  setText("c-pkts", hasNet ? String(s.net.packets) : "—");
  setText("c-total", `${s.net.packets} 包 · 最近 ${s.packets.length} 条`);

  const lossEl = document.getElementById("c-loss");
  if (lossEl) lossEl.classList.toggle("is-bad", s.net.rtp && s.net.loss_ratio > 0);
  const dfEl = document.getElementById("c-df");
  if (dfEl) dfEl.classList.toggle("is-bad", s.net.df_bad_pct > 1);
  const mlrEl = document.getElementById("c-mlr");
  if (mlrEl) mlrEl.classList.toggle("is-bad", s.net.rtp && s.net.mlr_good_pct < 99.9);

  renderRows(s.packets);
  disableFileActions(s.pcap_bytes <= 0 || running, s.ts_bytes <= 0 || running);
  const dirBtn = $<HTMLButtonElement>("c-dir-open");
  dirBtn.disabled = !s.out_dir;
  setBusyUi();
  setNavLamp("capture", error ? "error" : running ? "running" : "idle");
}

function disableFileActions(noPcap: boolean, noTs: boolean): void {
  $<HTMLButtonElement>("c-pcap-open").disabled = noPcap;
  $<HTMLButtonElement>("c-ts-open").disabled = noTs;
  $<HTMLButtonElement>("c-ts-analysis").disabled = noTs;
  $<HTMLButtonElement>("c-ts-player").disabled = noTs;
}

async function refresh(): Promise<void> {
  try {
    const s = await api.captureStatus();
    localError = "";
    render(s);
  } catch (e) {
    setError(`读取抓包状态失败：${String(e).slice(0, 160)}`);
  }
}

function schedule(): void {
  window.clearTimeout(pollTimer);
  pollTimer = 0;
  if (!active && !snap?.running) return;
  if (!snap?.running) return;
  pollTimer = window.setTimeout(async () => {
    await refresh();
    if (snap?.running) schedule();
  }, 1000);
}

function beginPolling(): void {
  window.clearTimeout(pollTimer);
  pollTimer = 0;
  void refresh().then(() => {
    if (snap?.running) schedule();
  });
}

async function toggleCapture(): Promise<void> {
  if (busy) return;
  busy = true;
  setError("");
  setBusyUi();
  try {
    if (snap?.running) {
      const s = await api.captureStop();
      snap = s;
      localError = "";
    } else {
      const url = $<HTMLInputElement>("c-url").value.trim();
      if (!url) {
        setError("请先填写 UDP / RTP 抓包地址");
        return;
      }
      if (!/^(udp|rtp):\/\//i.test(url)) {
        setError("抓包页只接受 udp:// 或 rtp:// 业务流地址；本地 TS 文件请到视频分析页打开。");
        return;
      }
      const nicIp = $<HTMLSelectElement>("c-nic").value;
      const s = await api.captureStart(url, nicIp);
      localError = "";
      snap = s;
      await api.pushSourceHistory("capture", url).catch(() => undefined);
      if (active) void refresh();
    }
  } catch (e) {
    setError(String(e).replace(/^Error:\s*/, "").slice(0, 240));
  } finally {
    busy = false;
    if (snap) render(snap);
    else setBusyUi();
    if (snap?.running) schedule();
    else window.clearTimeout(pollTimer);
  }
}

function gotoPage(page: string): void {
  document.querySelector<HTMLElement>(`#nav .mn[data-goto="${page}"]`)?.click();
}

function sendTs(target: "analysis" | "player"): void {
  if (!snap || snap.running || !snap.ts_bytes) return;
  const path = snap.ts_path;
  const inputId = target === "analysis" ? "a-url" : "p-url";
  const actId = target === "analysis" ? "a-act" : "p-act";
  const input = document.getElementById(inputId) as HTMLInputElement | null;
  if (input) input.value = path;
  gotoPage(target);
  const act = document.getElementById(actId) as HTMLButtonElement | null;
  if (act && !act.classList.contains("stop")) {
    act.click();
  }
}

async function loadNics(): Promise<void> {
  const sel = $<HTMLSelectElement>("c-nic");
  const current = sel.value;
  try {
    const [list, pref] = await Promise.all([
      api.listNics(),
      api.uiGet(CAPTURE_PREF_KEY).catch(() => null),
    ]);
    const saved = typeof pref?.nic === "string" ? pref.nic : "";
    const keep = current || saved;
    const label = (n: NicRow) => `${n.ip} · ${n.desc || n.name}${n.up ? "" : "（未连接）"}`;
    sel.innerHTML = [
      `<option value="">自动（系统默认出口）</option>`,
      ...list.map((n) => `<option value="${esc(n.ip)}" title="${esc(n.desc || n.name)}">${esc(label(n))}</option>`),
    ].join("");
    if (keep && !list.some((n) => n.ip === keep)) {
      sel.innerHTML += `<option value="${esc(keep)}">${esc(keep)}（未找到）</option>`;
    }
    sel.value = keep;
  } catch (e) {
    setError(`读取网卡失败：${String(e).slice(0, 120)}`);
  }
}

function initHistory(): void {
  const btn = $<HTMLButtonElement>("c-hist");
  const pop = $<HTMLElement>("c-histpop");
  const input = $<HTMLInputElement>("c-url");
  btn.addEventListener("click", (ev) => {
    ev.stopPropagation();
    if (!pop.hidden) {
      pop.hidden = true;
      return;
    }
    api.sourceHistory("capture").then((h) => {
      pop.innerHTML = [
        `<button class="hi clear" type="button">清除（输入新地址）</button>`,
        ...h.map((x) => `<button class="hi" type="button" title="${esc(x)}">${esc(x)}</button>`),
      ].join("");
      pop.hidden = false;
    });
  });
  pop.addEventListener("click", (ev) => {
    const t = (ev.target as HTMLElement).closest("button.hi") as HTMLButtonElement | null;
    if (!t) return;
    ev.stopPropagation();
    input.value = t.classList.contains("clear") ? "" : t.textContent || "";
    pop.hidden = true;
  });
  document.addEventListener("click", () => {
    pop.hidden = true;
  });
}

export function enterCapture(): void {
  active = true;
  beginPolling();
}

export function leaveCapture(): void {
  active = false;
  window.clearTimeout(pollTimer);
  pollTimer = 0;
  // 抓包线程仍在后台跑：保持低频轮询，让导航灯和产物大小在离开页面后也准确。
  if (snap?.running) schedule();
}

export function initCapture(): void {
  initHistory();
  void loadNics();
  $<HTMLButtonElement>("c-act").addEventListener("click", () => void toggleCapture());
  $<HTMLInputElement>("c-url").addEventListener("keydown", (ev) => {
    if (ev.key === "Enter" && !snap?.running) void toggleCapture();
  });
  if (isWebMode) {
    $("c-pcap-open").textContent = "下载";
    $("c-ts-open").textContent = "下载";
    $("c-ts-player").hidden = true;
    $("c-dir-open").hidden = true;
    $("c-note").textContent =
      "只抓绑定的这一路 UDP/RTP 业务流，不做全网卡混杂抓包，因此无需 Npcap。pcap 保留原始 UDP 负载（含 RTP 头），TS 样本按 188 字节对齐，最多 64 MB。停止抓包后可下载 pcap / TS，或把 TS 直接送分析。";
  }
  $<HTMLButtonElement>("c-pcap-open").addEventListener("click", () => {
    if (isWebMode) {
      downloadUrl(webDownloadUrl.capture("pcap"));
      return;
    }
    if (snap?.pcap_path) void api.reveal(snap.pcap_path).catch((e) => setError(String(e)));
  });
  $<HTMLButtonElement>("c-ts-open").addEventListener("click", () => {
    if (isWebMode) {
      downloadUrl(webDownloadUrl.capture("ts"));
      return;
    }
    if (snap?.ts_path) void api.reveal(snap.ts_path).catch((e) => setError(String(e)));
  });
  $<HTMLButtonElement>("c-dir-open").addEventListener("click", () => {
    if (snap?.out_dir) void api.reveal(snap.out_dir).catch((e) => setError(String(e)));
  });
  $<HTMLButtonElement>("c-ts-analysis").addEventListener("click", () => sendTs("analysis"));
  $<HTMLButtonElement>("c-ts-player").addEventListener("click", () => sendTs("player"));
  render(null);
}
