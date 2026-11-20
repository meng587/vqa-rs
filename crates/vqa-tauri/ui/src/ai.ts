//! AI 助手抽屉：自然语言 → 计划 → 通过现有 Tauri API 执行 → 汇总报告。
//!
//! 纪律：这里不点击任何页面控件，也不模拟 DOM 事件；所有模块动作都调 `api.*`
//! （即 Tauri 命令）。页面只负责展示状态和确认，AI 与手动操作共用同一套后端。

import {
  api,
  type AiConfig,
  type AiDownloadStatus,
  type AiModelCheckItem,
  type AiModelInfo,
  type AiPlan,
  type AiStep,
  type LlamaStatus,
} from "./api";
import { icon } from "./icons";
import { buildAiHtml, buildAiMarkdown, overallVerdict, reportFileName, stepStateLabel, type RunView, type StepState, type StepStatus } from "./ai-report";

const $id = (id: string) => document.getElementById(id);
const q = <T extends HTMLElement>(id: string) => $id(id) as T;

const PAGE_SOURCES: Record<string, string> = {
  player: "p-url",
  analysis: "a-url",
  compliance: "k-url",
  transcode: "x-in",
  burst: "b-url",
  avsync: "av-src",
  capture: "c-url",
};

const JOB = "ai-avsync";
const EXAMPLES = [
  "全面体检：抓包、分析、微突发、音画同步，出报告",
  "抓包 30 秒，分析 TS 并生成报告",
  "检查合规和音画同步",
  "只做微突发检测，30 秒",
  "播放并截一张画面",
];

let cfg: AiConfig | null = null;
let plan: AiPlan | null = null;
let running = false;
let cancelled = false;
let sessionKey = "";
let results: Record<string, any> = {};
let statuses: Record<string, StepStatus> = {};
let stepRows = new Map<string, HTMLElement>();
let activeTools = new Set<string>();
let startedAt = 0;
let finishedAt: number | undefined;
let lastReport: { name: string; md_path?: string | null; html_path?: string | null } | null = null;
let models: AiModelInfo[] = [];
let checkItems = new Map<string, AiModelCheckItem>();
let llama: LlamaStatus | null = null;
let download: AiDownloadStatus | null = null;
let downloadPollTimer: number | undefined;
let modelBusy = "";
let nicsLoaded = false;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const h = (s: any) =>
  String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

function readCurrentSource(): string {
  const el = document.getElementById(PAGE_SOURCES[document.body.dataset.page || "player"] || "");
  return (el as HTMLInputElement | HTMLTextAreaElement | null)?.value?.trim() || "";
}

function setNote(text: string, kind: "normal" | "err" | "ok" = "normal") {
  const el = $id("ai-note");
  if (!el) return;
  el.textContent = text;
  el.className = `ai-note${kind === "err" ? " err" : kind === "ok" ? " ok" : ""}`;
}

function setSettingsNote(text: string, kind: "normal" | "err" | "ok" = "normal") {
  const el = $id("ai-model-out");
  if (!el) return;
  el.textContent = text;
  el.className = `ai-note${kind === "err" ? " err" : kind === "ok" ? " ok" : ""}`;
}

function setConfigNote(text: string, kind: "normal" | "err" | "ok" = "normal") {
  const el = $id("ai-save-out");
  if (!el) return;
  el.hidden = !text;
  el.textContent = text;
  el.className = `ai-note${kind === "err" ? " err" : kind === "ok" ? " ok" : ""}`;
}

function fmtBytes(n: number): string {
  if (!n || !Number.isFinite(n)) return "—";
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024).toFixed(0)} KB`;
}

function addLog(text: string) {
  const box = $id("ai-log");
  if (!box) return;
  box.hidden = false;
  const line = document.createElement("div");
  line.className = "ai-logline";
  const t = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  line.innerHTML = `<span>${p(t.getHours())}:${p(t.getMinutes())}:${p(t.getSeconds())}</span>${h(text)}`;
  box.appendChild(line);
  while (box.childElementCount > 200) box.firstElementChild?.remove();
  box.scrollTop = box.scrollHeight;
}

function toolIcon(tool: string): string {
  if (tool.startsWith("capture")) return "scanline";
  if (tool.startsWith("analysis")) return "activity";
  if (tool.startsWith("compliance")) return "shieldcheck";
  if (tool.startsWith("burst")) return "gauge";
  if (tool.startsWith("avsync")) return "audiolines";
  if (tool.startsWith("player")) return "monitorplay";
  if (tool.startsWith("report")) return "bookopen";
  return "spark";
}

function modeText(): string {
  if (!cfg) return "读取配置…";
  if (cfg.mode === "local") return `本机 · ${cfg.local.model || "未配置"}`;
  if (cfg.mode === "cloud") return `云端 · ${cfg.cloud.model || "未配置"}`;
  return "规则模式 · 无需模型";
}

function modeTipText(mode: string | undefined): string {
  if (mode === "local") return "本机模型：由内置 llama-server 加载，只把目标和工具清单发给本机接口，来源地址不出本机。";
  if (mode === "cloud") return "云端模型：只发送目标和工具清单，不发送原始码流、截图或完整 IP；需要显式配置。";
  return "规则模式：不调用模型，按关键词生成计划，稳定且不消耗额度。";
}

function updateBadge() {
  const b = $id("ai-mode-badge");
  if (!b) return;
  b.textContent = modeText();
  b.className = `ai-mode-badge${cfg?.mode === "rule" ? " rule" : cfg?.mode === "cloud" ? " cloud" : " local"}`;
  const tip = $id("ai-mode-tip");
  if (tip) tip.textContent = modeTipText(cfg?.mode);
}

function fillConfigForm(c: AiConfig) {
  const mode = q<HTMLSelectElement>("ai-mode");
  mode.value = c.mode || "rule";
  q<HTMLInputElement>("ai-local-url").value = c.local?.base_url || "";
  q<HTMLInputElement>("ai-local-model").value = c.local?.model || "";
  q<HTMLInputElement>("ai-local-timeout").value = String(c.local?.timeout_secs || 60);
  q<HTMLInputElement>("ai-cloud-url").value = c.cloud?.base_url || "";
  q<HTMLInputElement>("ai-cloud-model").value = c.cloud?.model || "";
  q<HTMLInputElement>("ai-index-url").value = c.models_index_url || "";
  // Key 不落盘；同一会话内保留，重新打开设置不会清空。
  q<HTMLInputElement>("ai-cloud-key").value = sessionKey;
  updateModeBoxes();
}

function updateModeBoxes() {
  const mode = q<HTMLSelectElement>("ai-mode").value;
  $id("ai-local-box")?.toggleAttribute("hidden", mode !== "local");
  $id("ai-cloud-box")?.toggleAttribute("hidden", mode !== "cloud");
  const tip = $id("ai-mode-tip");
  if (tip) tip.textContent = modeTipText(mode);
}

function collectConfig(): AiConfig {
  const mode = q<HTMLSelectElement>("ai-mode").value;
  const timeout = Math.max(5, Math.min(300, Number(q<HTMLInputElement>("ai-local-timeout").value) || 60));
  return {
    mode,
    local: {
      base_url: q<HTMLInputElement>("ai-local-url").value.trim(),
      model: q<HTMLInputElement>("ai-local-model").value.trim(),
      timeout_secs: timeout,
    },
    cloud: {
      base_url: q<HTMLInputElement>("ai-cloud-url").value.trim(),
      model: q<HTMLInputElement>("ai-cloud-model").value.trim(),
      timeout_secs: timeout,
    },
    models_index_url: q<HTMLInputElement>("ai-index-url").value.trim(),
    auto_check: true,
  };
}

async function loadConfig() {
  try {
    cfg = await api.aiConfigGet();
    fillConfigForm(cfg);
    updateBadge();
    void checkModels(true);
  } catch (e: any) {
    setNote(`读取 AI 配置失败：${String(e).slice(0, 160)}`, "err");
  }
}

async function saveConfig(): Promise<AiConfig | null> {
  try {
    cfg = await api.aiConfigSet(collectConfig());
    updateBadge();
    setConfigNote("AI 设置已保存，下一次生成计划生效。", "ok");
    return cfg;
  } catch (e: any) {
    setConfigNote(`保存失败：${String(e).slice(0, 200)}`, "err");
    return null;
  }
}

function fmtSpeed(bps: number): string {
  if (!bps || !Number.isFinite(bps)) return "—";
  if (bps >= 1024 ** 2) return `${(bps / 1024 ** 2).toFixed(1)} MB/s`;
  if (bps >= 1024) return `${(bps / 1024).toFixed(0)} KB/s`;
  return `${bps.toFixed(0)} B/s`;
}

function fmtEta(secs: number): string {
  if (!secs || !Number.isFinite(secs)) return "—";
  if (secs >= 60) return `${Math.floor(secs / 60)} 分 ${Math.round(secs % 60)} 秒`;
  return `${Math.round(secs)} 秒`;
}

async function loadLlamaStatus() {
  try {
    llama = await api.llamaStatus();
  } catch {
    llama = null;
  }
  renderLlamaStatus();
  renderModels();
}

function renderLlamaStatus() {
  const badge = $id("llama-badge");
  const text = $id("llama-status");
  const stop = $id("llama-stop") as HTMLButtonElement | null;
  if (!badge || !text) return;
  let cls = "";
  let label = "待命";
  let message = "内置引擎待命；选择下面的 GGUF 模型后点「启用」。";
  if (llama) {
    if (!llama.engine_present) {
      cls = "bad";
      label = "缺少运行时";
      message = "当前安装包缺少 llama-server 运行时；本地模型不可用。请重新安装完整版本。";
    } else if (llama.starting) {
      cls = "up";
      label = "加载中";
      message = `正在加载 ${llama.model_name || llama.model_id || "模型"}…首次加载需要几十秒。`;
    } else if (llama.running) {
      cls = "ok";
      label = "运行中";
      const warn = llama.error ? ` · ${llama.error}` : "";
      message = `运行中 · ${llama.model_name} · ${llama.base_url}/v1 · PID ${llama.pid}${warn}`;
    } else if (llama.error) {
      cls = "bad";
      label = "异常";
      message = `引擎未运行：${llama.error}`;
    } else {
      message = `内置引擎待命 · ${llama.engine_version}；选择模型后点「启用」。`;
    }
  } else {
    cls = "bad";
    label = "不可用";
    message = "读取引擎状态失败；Web 检测端不提供本机模型。";
  }
  badge.className = `ai-model-badge${cls ? ` ${cls}` : ""}`;
  badge.textContent = label;
  text.textContent = message;
  text.className = `ai-note${cls === "bad" ? " err" : cls === "ok" ? " ok" : ""}`;
  if (stop) stop.hidden = !(llama?.running || llama?.starting);
  const wrap = $id("llama-log-wrap") as HTMLDetailsElement | null;
  const log = $id("llama-log");
  if (wrap && log) {
    const tail = llama?.log_tail?.trim() || "";
    wrap.hidden = !tail;
    log.textContent = tail;
    if (tail && llama?.error) wrap.open = true;
  }
}

function renderDownload() {
  const box = $id("ai-dl");
  const text = $id("ai-dl-text");
  const fill = $id("ai-dl-fill") as HTMLElement | null;
  const cancel = $id("ai-dl-cancel") as HTMLButtonElement | null;
  if (!box || !text || !fill) return;
  const active = !!download?.active;
  box.hidden = !active;
  if (!active || !download) return;
  const pct = Math.max(0, Math.min(100, download.percent || 0));
  fill.style.width = `${pct}%`;
  const total = download.total > 0 ? fmtBytes(download.total) : "未知大小";
  text.textContent = `${download.cancelling ? "正在取消 · " : ""}${download.name} · ${fmtBytes(download.downloaded)} / ${total} · ${pct.toFixed(1)}% · ${fmtSpeed(download.speed_bps)} · 剩余 ${fmtEta(download.eta_secs)}`;
  if (cancel) cancel.disabled = !!download.cancelling;
}

async function loadModels() {
  try {
    models = await api.aiModelList();
    renderModels();
  } catch (e: any) {
    const box = $id("ai-models");
    if (box) box.innerHTML = `<div class="ai-empty">读取模型列表失败：${h(String(e).slice(0, 180))}</div>`;
  }
}

function renderModels() {
  const box = $id("ai-models");
  if (!box) return;
  const rows: { id: string; info?: AiModelInfo; cat?: AiModelCheckItem }[] = [];
  const seen = new Set<string>();
  for (const m of models) {
    rows.push({ id: m.id, info: m, cat: checkItems.get(m.id) });
    seen.add(m.id);
  }
  for (const c of checkItems.values()) {
    if (!seen.has(c.id)) rows.push({ id: c.id, cat: c });
  }
  if (!rows.length) {
    box.innerHTML =
      '<div class="ai-empty"><b>没有可显示的模型</b><small>内置清单读取失败或清单为空；可点「刷新清单」，或直接导入本地 GGUF。</small></div>';
    renderLlamaStatus();
    return;
  }
  const anyDownload = !!download?.active;
  box.innerHTML = rows
    .map((row) => {
      const m = row.info;
      const c = row.cat;
      const id = row.id;
      const installed = !!m && m.present;
      const runningThis = !!llama?.running && llama.model_id === id;
      const startingThis = !!llama?.starting && llama.model_id === id;
      const downloadingThis = !!download?.active && download.id === id;
      const busy = modelBusy === id;
      const name = c?.name || m?.name || id;
      const size = (installed ? m?.bytes : c?.size) || 0;
      const version = c?.version || m?.version || "";
      let badge = '<span class="ai-model-badge">未安装</span>';
      if (runningThis) badge = '<span class="ai-model-badge ok">运行中</span>';
      else if (startingThis) badge = '<span class="ai-model-badge up">加载中</span>';
      else if (downloadingThis) badge = '<span class="ai-model-badge up">下载中</span>';
      else if (m && !m.present) badge = '<span class="ai-model-badge bad">文件缺失</span>';
      else if (installed && c?.update_available) badge = '<span class="ai-model-badge up">可升级</span>';
      else if (installed) badge = '<span class="ai-model-badge ok">已安装</span>';
      const meta = [
        size ? fmtBytes(size) : "—",
        version ? `v${version}` : "",
        c?.notes || (m?.source === "download" ? "清单下载" : m?.source === "import" ? "本地导入" : ""),
      ].filter(Boolean).join(" · ");
      let actions = "";
      if (installed && !runningThis && !startingThis) {
        actions += `<button class="ai-mini" data-model-start="${h(id)}" ${busy || anyDownload ? "disabled" : ""}>启用</button>`;
      }
      if (runningThis || startingThis) {
        actions += `<button class="ai-mini" data-model-stop="${h(id)}" ${modelBusy === "stop" ? "disabled" : ""}>${startingThis ? "取消" : "停止"}</button>`;
      }
      if (c && (!installed || c.update_available) && !downloadingThis) {
        actions += `<button class="ai-mini" data-model-download="${h(id)}" ${anyDownload || busy ? "disabled" : ""}>${installed ? "更新" : "下载"}</button>`;
      }
      if (m) {
        actions += `<button class="ai-mini danger" data-model-remove="${h(id)}" ${runningThis || startingThis || busy || anyDownload ? "disabled" : ""}>移除</button>`;
      }
      return `<div class="ai-model"><div class="ai-model-main"><b title="${h(name)}">${h(name)}</b><small title="${h(meta)}">${h(meta)}</small></div>${badge}<div class="ai-model-acts">${actions}</div></div>`;
    })
    .join("");
  box.querySelectorAll<HTMLButtonElement>("[data-model-start]").forEach((b) => {
    b.addEventListener("click", () => void startModel(b.dataset.modelStart || ""));
  });
  box.querySelectorAll<HTMLButtonElement>("[data-model-stop]").forEach((b) => {
    b.addEventListener("click", () => void stopModel());
  });
  box.querySelectorAll<HTMLButtonElement>("[data-model-download]").forEach((b) => {
    b.addEventListener("click", () => void downloadModel(b.dataset.modelDownload || ""));
  });
  box.querySelectorAll<HTMLButtonElement>("[data-model-remove]").forEach((b) => {
    b.addEventListener("click", () => void removeModel(b.dataset.modelRemove || ""));
  });
  renderLlamaStatus();
  renderDownload();
}

function stopDownloadPoll() {
  if (downloadPollTimer !== undefined) {
    window.clearInterval(downloadPollTimer);
    downloadPollTimer = undefined;
  }
}

async function pollDownloadOnce() {
  try {
    download = await api.aiModelDownloadStatus();
    renderDownload();
    renderModels();
    if (!download.active) {
      stopDownloadPoll();
      const finished = download;
      download = null;
      renderDownload();
      if (finished.done) {
        setSettingsNote(`模型下载完成：${finished.name}（${fmtBytes(finished.downloaded)}），可点「启用」。`, "ok");
        await loadModels();
        await checkModels(true);
      } else if (finished.error === "已取消") {
        setSettingsNote("下载已取消，未写入模型", "normal");
      } else if (finished.error) {
        setSettingsNote(`下载失败：${finished.error}`, "err");
      }
      renderModels();
    }
  } catch (e: any) {
    stopDownloadPoll();
    download = null;
    renderDownload();
    setSettingsNote(`读取下载进度失败：${String(e).slice(0, 200)}`, "err");
  }
}

function ensureDownloadPoll() {
  if (downloadPollTimer !== undefined) return;
  downloadPollTimer = window.setInterval(() => void pollDownloadOnce(), 500);
}

async function loadDownloadStatus() {
  try {
    const s = await api.aiModelDownloadStatus();
    if (s.active) {
      download = s;
      renderDownload();
      ensureDownloadPoll();
    }
  } catch {
    /* 下载状态不可用不阻塞设置页 */
  }
}

async function startModel(id: string) {
  if (!id || modelBusy || llama?.starting) return;
  modelBusy = id;
  renderModels();
  setSettingsNote(`正在启动 ${id}…首次加载模型需要几十秒。`);
  let ok = false;
  try {
    llama = await api.llamaStart(id);
    renderLlamaStatus();
    renderModels();
    for (let i = 0; i < 300 && llama?.starting; i++) {
      await sleep(500);
      if (modelBusy === "stop") break;
      try {
        llama = await api.llamaStatus();
      } catch {
        break;
      }
      renderLlamaStatus();
      renderModels();
    }
    if (modelBusy === "stop") {
      setSettingsNote("已停止模型加载；可以稍后重新启用。", "normal");
    } else if (llama?.running && llama.model_id === id) {
      ok = true;
      try {
        cfg = await api.aiConfigGet();
        fillConfigForm(cfg);
        updateBadge();
      } catch {
        /* 配置读取失败不影响引擎已启动 */
      }
      setSettingsNote(
        `已启用 ${llama.model_name || id}；本机接口 ${llama.base_url}/v1。${llama.error ? ` ${llama.error}` : ""}`,
        llama.error ? "normal" : "ok",
      );
    } else if (llama?.error) {
      setSettingsNote(`模型启动失败：${llama.error}`, "err");
    } else {
      setSettingsNote("模型启动未完成；请查看上方引擎状态与启动日志。", "err");
    }
  } catch (e: any) {
    setSettingsNote(`模型启动失败：${String(e).slice(0, 300)}`, "err");
  } finally {
    modelBusy = "";
    renderModels();
    renderLlamaStatus();
    if (!ok && llama?.log_tail) {
      const wrap = $id("llama-log-wrap") as HTMLDetailsElement | null;
      if (wrap) wrap.open = true;
    }
  }
}

async function stopModel() {
  if (modelBusy === "stop") return;
  modelBusy = "stop";
  renderModels();
  try {
    llama = await api.llamaStop();
    renderLlamaStatus();
    renderModels();
    setSettingsNote("内置引擎已停止；AI 配置保留，下次可重新启用。", "ok");
  } catch (e: any) {
    setSettingsNote(`停止引擎失败：${String(e).slice(0, 240)}`, "err");
  } finally {
    modelBusy = "";
    renderModels();
  }
}

async function downloadModel(id: string) {
  if (!id || download?.active) return;
  const input = $id("ai-index-url") as HTMLInputElement | null;
  const indexUrl = input?.value.trim() || undefined;
  setSettingsNote(`准备下载 ${id}…`);
  try {
    download = await api.aiModelDownload(id, indexUrl);
    renderDownload();
    renderModels();
    if (download.active) {
      ensureDownloadPoll();
    } else if (download.done) {
      setSettingsNote(`模型已在本地：${download.name}`, "ok");
      await loadModels();
      await checkModels(true);
    } else if (download.error) {
      setSettingsNote(`下载失败：${download.error}`, "err");
    }
  } catch (e: any) {
    setSettingsNote(`下载启动失败：${String(e).slice(0, 280)}`, "err");
  }
}

async function cancelDownload() {
  try {
    download = await api.aiModelDownloadCancel();
    renderDownload();
    setSettingsNote("正在取消下载…");
  } catch (e: any) {
    setSettingsNote(`取消下载失败：${String(e).slice(0, 220)}`, "err");
  }
}

async function removeModel(id: string) {
  if (!id) return;
  if (!window.confirm(`移除模型「${id}」？会删除已下载/导入的模型文件。`)) return;
  try {
    if (llama && (llama.running || llama.starting) && llama.model_id === id) {
      llama = await api.llamaStop();
      renderLlamaStatus();
    }
    await api.aiModelRemove(id);
    await loadModels();
    setSettingsNote("模型已移除；清单项仍保留，可重新下载", "ok");
  } catch (e: any) {
    setSettingsNote(`移除失败：${String(e).slice(0, 220)}`, "err");
  }
}

async function checkModels(silent = false) {
  const input = $id("ai-index-url") as HTMLInputElement | null;
  const url = input?.value.trim() || "";
  if (!silent) setSettingsNote("检查模型清单中…");
  try {
    const res = await api.aiModelCheck(url || undefined);
    checkItems = new Map(res.models.map((m) => [m.id, m]));
    renderModels();
    if (silent) return;
    const up = res.models.filter((m) => m.update_available).length;
    const installable = res.models.filter((m) => !m.installed).length;
    const parts: string[] = [];
    if (res.builtin) parts.push("内置清单");
    if (installable) parts.push(`${installable} 个可下载`);
    if (up) parts.push(`${up} 个可升级`);
    setSettingsNote(
      parts.length
        ? `检查完成：${parts.join("，")}。可直接点「下载」，离线环境也可导入 GGUF。`
        : "检查完成：清单模型都已安装。",
      parts.length ? "ok" : "normal",
    );
  } catch (e: any) {
    if (!silent) setSettingsNote(`检查失败：${String(e).slice(0, 240)}`, "err");
  }
}

async function importModel() {
  setSettingsNote("正在复制模型并计算 SHA256，模型较大时请稍候…");
  try {
    const m = await api.aiModelImport();
    await loadModels();
    setSettingsNote(`已导入：${m.name}（${fmtBytes(m.bytes)}）`, "ok");
  } catch (e: any) {
    const text = String(e);
    setSettingsNote(text.includes("未选择") ? "未选择模型文件" : `导入失败：${text.slice(0, 240)}`, text.includes("未选择") ? "normal" : "err");
  }
}

async function testProvider() {
  const mode = q<HTMLSelectElement>("ai-mode").value;
  const base = mode === "cloud" ? q<HTMLInputElement>("ai-cloud-url").value : q<HTMLInputElement>("ai-local-url").value;
  const model = mode === "cloud" ? q<HTMLInputElement>("ai-cloud-model").value : q<HTMLInputElement>("ai-local-model").value;
  sessionKey = q<HTMLInputElement>("ai-cloud-key").value.trim();
  const out = $id("ai-test-out");
  if (out) {
    out.textContent = "测试中…";
    out.className = "ai-test-out";
  }
  try {
    const r = await api.aiTest(mode, base, model, sessionKey || undefined);
    if (out) {
      out.textContent = r.ok ? `连接正常 · ${r.latency_ms ?? "—"} ms${r.reply ? ` · ${r.reply}` : ""}` : `连接失败：${r.error || "未知错误"}`;
      out.className = `ai-test-out${r.ok ? " ok" : " err"}`;
    }
  } catch (e: any) {
    if (out) {
      out.textContent = `测试失败：${String(e).slice(0, 220)}`;
      out.className = "ai-test-out err";
    }
  }
}

function refreshAiSettingsPanel() {
  if (cfg) fillConfigForm(cfg);
  updateBadge();
  // 重新读一次磁盘配置：模型可能在本页之外被启用/切换，避免表单显示旧端点。
  void api
    .aiConfigGet()
    .then((c) => {
      cfg = c;
      fillConfigForm(c);
      updateBadge();
    })
    .catch(() => {});
  void loadModels();
  void loadLlamaStatus();
  void loadDownloadStatus();
  void checkModels(true);
}

function openDrawer() {
  const scrim = $id("ai-scrim");
  const drawer = $id("ai-drawer");
  if (!scrim || !drawer) return;
  scrim.hidden = false;
  drawer.classList.add("open");
  drawer.setAttribute("aria-hidden", "false");
  const src = q<HTMLInputElement>("ai-src");
  if (!src.value.trim()) {
    const cur = readCurrentSource();
    if (cur) src.value = cur;
  }
  if (!nicsLoaded) void loadNics();
  setTimeout(() => q<HTMLTextAreaElement>("ai-goal").focus(), 120);
}

function closeDrawer() {
  const scrim = $id("ai-scrim");
  const drawer = $id("ai-drawer");
  if (scrim) scrim.hidden = true;
  if (drawer) {
    if (drawer.contains(document.activeElement)) {
      (document.activeElement as HTMLElement | null)?.blur();
    }
    drawer.classList.remove("open");
    drawer.setAttribute("aria-hidden", "true");
  }
}

async function loadNics() {
  try {
    const rows = await api.listNics();
    const sel = q<HTMLSelectElement>("ai-nic");
    if (!sel) return;
    const opts = rows
      .filter((n) => n.ip)
      .sort((a, b) => Number(b.up) - Number(a.up))
      .map((n) => `<option value="${h(n.ip)}">${h(n.ip)} · ${h(n.name)}</option>`)
      .join("");
    sel.innerHTML = `<option value="">自动选择</option>${opts}`;
    nicsLoaded = true;
  } catch {
    /* 网卡列表不可用不阻塞 AI；留空由后端自动路由 */
  }
}

function useCurrentSource() {
  const src = readCurrentSource();
  if (!src) {
    setNote("当前页面没有可读取的来源；请先在对应页面填写地址，或直接在下面输入。", "err");
    return;
  }
  q<HTMLInputElement>("ai-src").value = src;
  setNote("已取用当前页面的来源。");
}

function stepSummary(s: AiStep): string {
  const a: any = s.args || {};
  const bits: string[] = [];
  const src = a.url || a.source;
  if (src) bits.push(String(src) === "$capture.ts_path" ? "抓包 TS 样本" : String(src));
  if (a.duration_sec) bits.push(`${a.duration_sec} 秒`);
  if (a.secs) bits.push(`${a.secs} 秒`);
  if (a.kind) bits.push(String(a.kind));
  if (s.depends_on?.length) bits.push(`依赖 ${s.depends_on.join("、")}`);
  return bits.join(" · ");
}

function renderPlan(p: AiPlan) {
  plan = p;
  statuses = {};
  results = {};
  lastReport = null;
  finishedAt = undefined;
  $id("ai-planwrap")?.removeAttribute("hidden");
  $id("ai-runlog")?.setAttribute("hidden", "");
  $id("ai-report")?.setAttribute("hidden", "");
  const summary = $id("ai-plan-summary");
  if (summary) summary.textContent = p.summary;
  const provider = $id("ai-plan-provider");
  if (provider) {
    provider.textContent =
      p.provider === "rule"
        ? "规则"
        : p.provider.startsWith("local:")
          ? `本机 · ${p.provider.slice(6)}`
          : p.provider.startsWith("cloud:")
            ? `云端 · ${p.provider.slice(6)}`
            : p.provider;
    provider.className = `ai-tag${p.provider.includes("cloud") ? " cloud" : p.provider.includes("local") ? " local" : ""}`;
  }
  const warn = $id("ai-plan-warn");
  if (warn) {
    const w = p.warnings || [];
    warn.toggleAttribute("hidden", !w.length);
    warn.innerHTML = w.map((x) => `<div>• ${h(x)}</div>`).join("");
  }

  const wrap = $id("ai-steps");
  if (!wrap) return;
  stepRows.clear();
  wrap.innerHTML = p.steps
    .map((s) => {
      statuses[s.id] = { state: "pending" };
      const row = `<div class="ai-step" data-state="pending" data-stepid="${h(s.id)}">
        <div class="ai-step-ico">${icon(toolIcon(s.tool), 15)}</div>
        <div class="ai-step-main"><b>${h(s.title)}</b><small>${h(s.tool)}${stepSummary(s) ? ` · ${h(stepSummary(s))}` : ""}</small><em class="ai-step-msg"></em></div>
        <span class="ai-step-st">待执行</span>
      </div>`;
      return row;
    })
    .join("");
  wrap.querySelectorAll<HTMLElement>("[data-stepid]").forEach((el) => {
    const id = el.dataset.stepid || "";
    if (id) stepRows.set(id, el);
    el.querySelector<HTMLElement>(".ai-step-st")!.textContent = "待执行";
  });
  setRunUI();
}

function setStatus(id: string, state: StepState, msg = "") {
  statuses[id] = { state, msg };
  const row = stepRows.get(id);
  if (row) {
    row.dataset.state = state;
    const st = row.querySelector<HTMLElement>(".ai-step-st");
    if (st) st.textContent = stepStateLabel(state);
    const em = row.querySelector<HTMLElement>(".ai-step-msg");
    if (em) em.textContent = msg || "";
  }
}

function setStepMsg(id: string, msg: string) {
  const row = stepRows.get(id);
  const em = row?.querySelector<HTMLElement>(".ai-step-msg");
  if (em) em.textContent = msg;
}

function setRunUI() {
  const runBtn = q<HTMLButtonElement>("ai-run");
  const stopBtn = q<HTMLButtonElement>("ai-stop");
  const clearBtn = q<HTMLButtonElement>("ai-clear");
  if (runBtn) {
    runBtn.hidden = running;
    runBtn.disabled = running || !plan || !plan.steps.length;
  }
  if (stopBtn) stopBtn.hidden = !running;
  if (clearBtn) clearBtn.disabled = running;
  $id("ai-open")?.classList.toggle("is-running", running);
}

async function generatePlan() {
  if (running) return;
  const goal = q<HTMLTextAreaElement>("ai-goal").value.trim();
  const source = q<HTMLInputElement>("ai-src").value.trim();
  const nic = q<HTMLSelectElement>("ai-nic").value;
  if (!goal) {
    setNote("先写一句要做什么，例如“抓包 30 秒并分析 TS”。", "err");
    return;
  }
  if (!source) {
    setNote("先选择来源；可以点“取当前页”，也可以直接填地址/文件。", "err");
    return;
  }
  const btn = q<HTMLButtonElement>("ai-generate");
  btn.disabled = true;
  setNote("正在生成计划…");
  try {
    const p = await api.aiPlan(goal, source, nic, sessionKey || undefined);
    renderPlan(p);
    setNote(`计划已生成：${p.steps.length} 个步骤，确认后点“执行计划”。`, "ok");
  } catch (e: any) {
    setNote(`生成计划失败：${String(e).slice(0, 260)}`, "err");
  } finally {
    btn.disabled = false;
  }
}

function currentRun(): RunView | null {
  if (!plan) return null;
  return {
    plan,
    results,
    statuses,
    startedAt,
    finishedAt,
    cancelled,
  };
}

async function executePlan() {
  if (!plan || running) return;
  running = true;
  cancelled = false;
  results = {};
  lastReport = null;
  finishedAt = undefined;
  startedAt = Date.now();
  for (const s of plan.steps) statuses[s.id] = { state: "pending" };
  setRunUI();
  $id("ai-report")?.setAttribute("hidden", "");
  addLog(`开始执行计划：${plan.summary}`);
  setNote("执行中；AI 只调用后端 API，不操作页面。");

  const terminal = new Set<string>();
  const failed = new Set<string>();
  const skipped = new Set<string>();
  let guard = 0;
  while (terminal.size < plan.steps.length) {
    if (++guard > 2000) {
      setNote("执行调度异常：步骤依赖没有收敛", "err");
      break;
    }
    if (cancelled) {
      for (const s of plan.steps) {
        if (!terminal.has(s.id)) {
          setStatus(s.id, "cancelled", "已取消");
          terminal.add(s.id);
        }
      }
      break;
    }
    // 普通步骤的依赖失败/跳过 → 本步骤跳过；报告步骤例外，仍可汇总失败信息。
    let changed = false;
    for (const s of plan.steps) {
      if (terminal.has(s.id) || s.tool === "report.build") continue;
      const bad = s.depends_on?.find((d) => failed.has(d) || skipped.has(d));
      if (bad) {
        setStatus(s.id, "skipped", `依赖 ${bad} 未完成`);
        terminal.add(s.id);
        skipped.add(s.id);
        changed = true;
      }
    }
    if (changed) continue;

    const ready = plan.steps.filter((s) => !terminal.has(s.id) && (s.depends_on || []).every((d) => terminal.has(d)));
    if (!ready.length) {
      for (const s of plan.steps) {
        if (!terminal.has(s.id)) {
          setStatus(s.id, "skipped", "依赖未完成，无法执行");
          terminal.add(s.id);
          skipped.add(s.id);
        }
      }
      break;
    }
    await Promise.all(
      ready.map(async (s) => {
        const ok = await runOne(s);
        terminal.add(s.id);
        if (ok) {
          /* done */
        } else if (cancelled) {
          skipped.add(s.id);
        } else {
          failed.add(s.id);
        }
      }),
    );
  }

  running = false;
  finishedAt = Date.now();
  setRunUI();
  if (cancelled) addLog("执行已停止");
  else addLog("执行结束");
  renderReport();
  setNote(cancelled ? "已停止；已完成步骤的结果仍保留。" : "执行结束。报告数据来自各模块 API 返回。", cancelled ? "normal" : "ok");
}

async function stopRun() {
  if (!running) return;
  cancelled = true;
  addLog("正在停止当前检测…");
  const tasks: Promise<any>[] = [];
  if (activeTools.has("capture.run")) tasks.push(api.captureStop().catch(() => undefined));
  if (activeTools.has("analysis.run")) tasks.push(api.analysisStop().catch(() => undefined));
  if (activeTools.has("compliance.run")) tasks.push(api.complianceLiveStop().catch(() => undefined));
  if (activeTools.has("burst.run")) tasks.push(api.burstStop().catch(() => undefined));
  if (activeTools.has("avsync.run")) tasks.push(api.avsyncStop(JOB).catch(() => undefined));
  if (activeTools.has("player.play")) tasks.push(api.stop().catch(() => undefined));
  await Promise.all(tasks);
}

async function waitSeconds(secs: number, id: string, label: string): Promise<void> {
  const end = Date.now() + Math.max(0, secs) * 1000;
  while (Date.now() < end) {
    if (cancelled) throw new Error("已取消");
    const left = Math.max(0, Math.ceil((end - Date.now()) / 1000));
    setStepMsg(id, `${label} ${left}s…`);
    await sleep(Math.min(250, Math.max(50, end - Date.now())));
  }
  if (cancelled) throw new Error("已取消");
}

async function ensureCaptureIdle() {
  const st = await api.captureStatus();
  if (st?.running) throw new Error("抓包模块正在使用，请先在抓包页停止当前抓包");
}

async function ensureAnalysisIdle() {
  const p = await api.analysisProgress();
  if (!p) throw new Error("读取视频分析状态失败");
  if (p.mode !== 0) throw new Error("视频分析模块正在使用，请先停止当前分析");
}

async function ensureBurstIdle() {
  const b = await api.burstSnapshot();
  if (b?.running) throw new Error("微突发模块正在使用，请先停止当前检测");
}

async function ensurePlayerIdle() {
  const p = await api.snapshot();
  if (p?.playing) throw new Error("播放器正在播放，请先在播放器页停止");
}

async function waitAnalysisDone(id: string, timeoutSecs: number) {
  const end = Date.now() + timeoutSecs * 1000;
  while (Date.now() < end) {
    if (cancelled) {
      await api.analysisStop().catch(() => undefined);
      throw new Error("已取消");
    }
    const p = await api.analysisProgress();
    if (!p) throw new Error("读取视频分析进度失败");
    if (p.err) throw new Error(p.err);
    if (p.mode === 0) return;
    setStepMsg(id, p.mode === 2 ? `实时收流分析 ${p.live_bytes ? `${Math.round(p.live_bytes / 1048576)} MB` : ""}…` : `扫描 ${p.total ? Math.round((p.done / p.total) * 100) : 0}%…`);
    await sleep(300);
  }
  throw new Error("视频分析超时；可缩短检测时长后重试");
}

function resultForTool(tool: string): any {
  const step = plan?.steps.find((s) => s.tool === tool);
  return step ? results[step.id] : undefined;
}

function resolveSource(s: AiStep): string {
  const a: any = s.args || {};
  let src = String(a.source || a.url || plan?.source || "");
  if (src === "$capture.ts_path" || src.includes("$capture")) {
    src = resultForTool("capture.run")?.ts_path || "";
    if (!src) throw new Error("抓包步骤没有产出 TS 样本，无法继续分析");
  }
  if (!src) throw new Error("步骤没有可用来源");
  return src;
}

function stepSeconds(s: AiStep, fallback = 30): number {
  const a: any = s.args || {};
  const n = Number(a.duration_sec || a.secs || fallback);
  return Math.max(5, Math.min(600, Number.isFinite(n) ? n : fallback));
}

async function runOne(s: AiStep): Promise<boolean> {
  if (cancelled) {
    setStatus(s.id, "cancelled", "已取消");
    return false;
  }
  setStatus(s.id, "running", "准备中…");
  addLog(`▶ ${s.title}`);
  activeTools.add(s.tool);
  try {
    const r = await executeStep(s);
    if (cancelled) throw new Error("已取消");
    results[s.id] = r;
    setStatus(s.id, "ok", summarizeResult(s.tool, r));
    addLog(`✓ ${s.title} 完成`);
    return true;
  } catch (e: any) {
    const msg = String(e).replace(/^Error:\s*/, "").slice(0, 260);
    results[s.id] = { error: msg };
    if (cancelled) {
      setStatus(s.id, "cancelled", "已取消");
      addLog(`■ ${s.title} 已取消`);
    } else {
      setStatus(s.id, "error", msg);
      addLog(`✗ ${s.title} 失败：${msg}`);
    }
    return false;
  } finally {
    activeTools.delete(s.tool);
  }
}

function summarizeResult(tool: string, r: any): string {
  if (!r) return "完成";
  if (tool === "capture.run") return r.ts_path ? `TS 样本 ${fmtBytes(r.ts_bytes)}` : "已停止";
  if (tool === "analysis.run") return r.packets ? `${r.packets} 包${r.sync_errors ? `，同步错误 ${r.sync_errors}` : ""}` : "完成";
  if (tool === "compliance.run") return r.summary ? `通过 ${r.summary.pass ?? 0} / 不通过 ${r.summary.fail ?? 0}` : "完成";
  if (tool === "burst.run") return `突发 ${r.burst_cnt ?? 0} 次，峰值 ${r.mbps_peak != null ? Number(r.mbps_peak).toFixed(2) : "—"} Mbps`;
  if (tool === "avsync.run") return `偏移 ${Number(r.offset_ms || 0).toFixed(0)} ms`;
  if (tool === "player.play") return r.snapshot_path ? "已截图" : "已抽检";
  if (tool === "report.build") return r.md_path ? "报告已导出" : "完成";
  return "完成";
}

async function executeStep(s: AiStep): Promise<any> {
  const a: any = s.args || {};
  const src = resolveSource(s);
  const nic = String(a.nic_ip ?? plan?.nic_ip ?? "");
  const secs = stepSeconds(s);

  switch (s.tool) {
    case "capture.run": {
      await ensureCaptureIdle();
      const first = await api.captureStart(src, nic);
      if (!first) throw new Error("抓包启动后没有返回状态");
      if (first.error) throw new Error(first.error);
      await waitSeconds(secs, s.id, "已收流");
      if (cancelled) {
        await api.captureStop().catch(() => undefined);
        throw new Error("已取消");
      }
      const snap = await api.captureStop();
      if (!snap) throw new Error("抓包停止后没有返回结果");
      if (snap.error) throw new Error(snap.error);
      if (!snap.ts_path || !snap.ts_bytes) throw new Error("没有产出 TS 样本；请确认来源是 TS over UDP/RTP");
      return snap;
    }
    case "analysis.run": {
      await ensureAnalysisIdle();
      await api.analysisStart(src, nic);
      const live = Boolean(a.live);
      if (live) {
        await waitSeconds(secs, s.id, "实时分析");
        await api.analysisStop().catch(() => undefined);
        await waitAnalysisDone(s.id, 20);
      } else {
        await waitAnalysisDone(s.id, secs + 120);
      }
      const res = await api.analysisResult();
      if (!res) throw new Error("分析结束但没有结果");
      return res;
    }
    case "compliance.run": {
      const kind = String(a.kind || "IPTV");
      const live = Boolean(a.live);
      if (live) {
        await api.complianceLiveStart(src, nic, kind);
        await waitSeconds(secs, s.id, "实时合规");
        const snap = await api.complianceLiveSnapshot(kind);
        await api.complianceLiveStop().catch(() => undefined);
        if (!snap) throw new Error("实时合规结束但没有结果");
        if (snap.err && (!snap.items || !snap.items.length)) throw new Error(snap.err);
        return snap;
      }
      const r = await api.complianceCheck(src, kind);
      if (!r) throw new Error("合规检测结束但没有结果");
      return r;
    }
    case "burst.run": {
      await ensureBurstIdle();
      const ratio = Number(a.ratio || 2);
      const windowMs = Number(a.window_ms || 1);
      const started = await api.burstStart(src, nic, ratio, windowMs);
      if (!started) throw new Error("微突发启动后没有返回状态");
      await waitSeconds(secs, s.id, "微突发");
      if (cancelled) {
        await api.burstStop().catch(() => undefined);
        throw new Error("已取消");
      }
      const st = await api.burstStop();
      if (!st) throw new Error("微突发结束但没有结果");
      if (st?.err) throw new Error(st.err);
      return st;
    }
    case "avsync.run": {
      // 清掉上一次 AI 任务可能留下的残留（只动 AI 专用 job，不影响手动任务）
      try {
        const old = await api.avsyncProgress(JOB);
        if (old.running) await api.avsyncStop(JOB);
      } catch {
        /* 没有旧任务 */
      }
      await api.avsyncStart(
        { source: src, secs, search_ms: 800, fps: 25, nic_ip: nic },
        JOB,
      );
      const end = Date.now() + (secs + 90) * 1000;
      while (Date.now() < end) {
        if (cancelled) {
          await api.avsyncStop(JOB).catch(() => undefined);
          throw new Error("已取消");
        }
        const p = await api.avsyncProgress(JOB);
        if (!p) throw new Error("读取音画同步进度失败");
        if (p.err) throw new Error(p.err);
        if (!p.running) {
          if (!p.result) throw new Error("音画同步结束但没有结果");
          if (p.result.err) throw new Error(p.result.err);
          return p.result;
        }
        setStepMsg(s.id, `音画同步 ${p.pct || 0}%…`);
        await sleep(300);
      }
      await api.avsyncStop(JOB).catch(() => undefined);
      throw new Error("音画同步超时");
    }
    case "player.play": {
      await ensurePlayerIdle();
      await api.play(src, nic, false);
      await waitSeconds(2, s.id, "播放");
      const snap = await api.snapshot();
      if (!snap) throw new Error("播放器没有返回状态");
      let path: string | null = null;
      try {
        path = await api.snapshotSave();
      } catch {
        /* 截图失败不影响播放器状态 */
      }
      await api.stop().catch(() => undefined);
      return { ...snap, snapshot_path: path };
    }
    case "report.build": {
      const formats = Array.isArray(a.formats) ? a.formats.map(String) : ["md"];
      return await exportReportFiles(formats);
    }
    default:
      throw new Error(`不支持的工具：${s.tool}`);
  }
}

async function exportReportFiles(formats: string[]): Promise<any> {
  const run = currentRun();
  if (!run) throw new Error("没有可汇总的执行结果");
  // 报告内容是在 report.build 步骤内部生成的，摘要里先标成完成，
  // 避免导出文件里的“执行摘要”把报告步骤显示成“执行中”。
  const reportStep = run.plan.steps.find((s) => s.tool === "report.build");
  if (reportStep) statuses[reportStep.id] = { state: "ok", msg: "报告已生成" };
  const name = reportFileName();
  const md = buildAiMarkdown(run);
  const mdPath = await api.aiReportExport(`${name}.md`, md, "md");
  let htmlPath: string | null = null;
  let htmlError = "";
  if (formats.includes("html")) {
    try {
      const html = buildAiHtml(run);
      htmlPath = await api.aiReportExport(`${name}.html`, html, "html");
    } catch (e: any) {
      htmlError = String(e).slice(0, 180);
      addLog(`HTML 报告导出失败：${htmlError}`);
    }
  }
  const out = { name, md_path: mdPath, html_path: htmlPath, html_error: htmlError };
  lastReport = out;
  return out;
}

async function manualReport(fmt: "md" | "html") {
  const run = currentRun();
  if (!run) return;
  try {
    const name = lastReport?.name || reportFileName();
    const content = fmt === "md" ? buildAiMarkdown(run) : buildAiHtml(run);
    const path = await api.aiReportExport(`${name}.${fmt}`, content, fmt);
    if (!path) return;
    lastReport = { ...(lastReport || { name }), ...(fmt === "md" ? { md_path: path } : { html_path: path }) };
    renderReport();
    addLog(`报告已导出：${path}`);
  } catch (e: any) {
    setNote(`导出失败：${String(e).slice(0, 220)}`, "err");
  }
}

function reportPaths(): { name: string; md_path?: string | null; html_path?: string | null } | null {
  if (lastReport) return lastReport;
  const report = resultForTool("report.build");
  if (report?.md_path) return { name: report.name || reportFileName(), md_path: report.md_path, html_path: report.html_path };
  return null;
}

function renderReport() {
  const run = currentRun();
  const box = $id("ai-report");
  if (!box || !run) return;
  const hasAny = Object.values(results).some((r) => r && !r.error);
  const failedAny = run.plan.steps.some((s) => statuses[s.id]?.state === "error");
  if (!hasAny && !reportPaths() && !failedAny) {
    box.setAttribute("hidden", "");
    return;
  }
  box.removeAttribute("hidden");
  const v = overallVerdict(run);
  const cls = v.level === "bad" ? "bad" : v.level === "warn" ? "warn" : v.level === "good" ? "good" : "unknown";
  const paths = reportPaths();
  box.innerHTML = `<div class="ai-report-head"><span>本次结论</span><b class="ai-verdict ${cls}">${h(v.title)}</b></div>
    <div class="ai-report-findings">${v.findings.map((f) => `<div>• ${h(f)}</div>`).join("")}</div>
    ${paths ? `<div class="ai-report-files">${paths.md_path ? `<div><span>Markdown</span><small title="${h(paths.md_path)}">${h(paths.md_path)}</small><button class="ai-mini" data-reveal="${h(paths.md_path)}">定位</button></div>` : ""}${paths.html_path ? `<div><span>HTML</span><small title="${h(paths.html_path)}">${h(paths.html_path)}</small><button class="ai-mini" data-reveal="${h(paths.html_path)}">定位</button></div>` : ""}</div>` : `<div class="ai-report-actions"><button class="ai-mini" data-report="md">导出 Markdown</button><button class="ai-mini" data-report="html">导出 HTML</button></div>`}`;
  box.querySelectorAll<HTMLButtonElement>("[data-reveal]").forEach((b) => {
    b.addEventListener("click", () => void api.reveal(b.dataset.reveal || "").catch(() => undefined));
  });
  box.querySelectorAll<HTMLButtonElement>("[data-report]").forEach((b) => {
    b.addEventListener("click", () => void manualReport((b.dataset.report as "md" | "html") || "md"));
  });
}

function clearPlan() {
  if (running) return;
  plan = null;
  results = {};
  statuses = {};
  lastReport = null;
  finishedAt = undefined;
  stepRows.clear();
  $id("ai-planwrap")?.setAttribute("hidden", "");
  $id("ai-runlog")?.setAttribute("hidden", "");
  $id("ai-report")?.setAttribute("hidden", "");
  const log = $id("ai-log");
  if (log) {
    log.innerHTML = "";
    log.hidden = true;
  }
  setNote("AI 只通过后端 API 操作模块，不点击页面。规则模式无需模型；本机/云端模型失败会自动回退规则计划。");
  setRunUI();
}

function bindAiSettingsEvents() {
  const saveCfg = $id("ai-save-cfg") as HTMLButtonElement | null;
  if (!saveCfg) return;
  saveCfg.addEventListener("click", () => void saveConfig());
  q<HTMLButtonElement>("ai-test").addEventListener("click", () => void testProvider());
  q<HTMLButtonElement>("ai-import").addEventListener("click", () => void importModel());
  q<HTMLButtonElement>("ai-check").addEventListener("click", () => void checkModels());
  q<HTMLButtonElement>("llama-stop").addEventListener("click", () => void stopModel());
  q<HTMLButtonElement>("ai-dl-cancel").addEventListener("click", () => void cancelDownload());
  q<HTMLSelectElement>("ai-mode").addEventListener("change", updateModeBoxes);
  q<HTMLInputElement>("ai-cloud-key").addEventListener("input", (e) => {
    sessionKey = (e.target as HTMLInputElement).value.trim();
  });
}

function bindEvents() {
  q<HTMLButtonElement>("ai-open").addEventListener("click", openDrawer);
  q<HTMLButtonElement>("ai-close").addEventListener("click", closeDrawer);
  q<HTMLElement>("ai-scrim").addEventListener("click", closeDrawer);
  q<HTMLButtonElement>("ai-use-page").addEventListener("click", useCurrentSource);
  q<HTMLButtonElement>("ai-generate").addEventListener("click", () => void generatePlan());
  q<HTMLButtonElement>("ai-run").addEventListener("click", () => void executePlan());
  q<HTMLButtonElement>("ai-stop").addEventListener("click", () => void stopRun());
  q<HTMLButtonElement>("ai-clear").addEventListener("click", clearPlan);

  document.querySelectorAll<HTMLElement>("[data-ai-example]").forEach((b) => {
    b.addEventListener("click", () => {
      q<HTMLTextAreaElement>("ai-goal").value = b.dataset.aiExample || "";
      q<HTMLTextAreaElement>("ai-goal").focus();
    });
  });

  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if ($id("ai-drawer")?.classList.contains("open")) closeDrawer();
  });

  window.addEventListener("page-changed", () => {
    if ($id("ai-drawer")?.classList.contains("open")) {
      const src = q<HTMLInputElement>("ai-src");
      if (!src.value.trim()) {
        const cur = readCurrentSource();
        if (cur) src.value = cur;
      }
    }
  });

  window.addEventListener("global-settings-tab", (e) => {
    if ((e as CustomEvent).detail === "ai") refreshAiSettingsPanel();
  });
}

function drawerMarkup(): string {
  const examples = EXAMPLES.map((x) => `<button class="ai-chip" data-ai-example="${h(x)}">${h(x)}</button>`).join("");
  return `<div id="ai-scrim" class="ai-scrim" hidden></div>
<aside id="ai-drawer" class="ai-drawer" aria-hidden="true">
  <header class="ai-head">
    <div class="ai-brand"><span class="ai-mark">${icon("spark", 18)}</span><div><b>AI 检测助手</b><small id="ai-mode-badge" class="ai-mode-badge">读取配置…</small></div></div>
    <div class="ai-head-acts"><button class="iconbtn" id="ai-close" title="关闭">${icon("x", 15)}</button></div>
  </header>
  <div class="ai-body">
    <section class="ai-compose">
      <div class="ai-label">要做什么</div>
      <textarea id="ai-goal" rows="4" spellcheck="false" placeholder="例如：对这个源抓包 30 秒，分析 TS、微突发和音画同步，最后出报告"></textarea>
      <div class="ai-chips">${examples}</div>
      <div class="ai-label">来源</div>
      <div class="ai-srcrow"><input id="ai-src" class="ai-src" spellcheck="false" placeholder="udp://@239.x.x.x:port 或本地 TS 文件" /><button class="ai-mini" id="ai-use-page">取当前页</button></div>
      <div class="ai-srcrow"><select id="ai-nic" class="nic ai-nic"><option value="">自动选择网卡</option></select><button class="gradbtn ai-primary" id="ai-generate">${icon("route", 13)} 生成计划</button></div>
      <div id="ai-note" class="ai-note">AI 只通过后端 API 操作模块，不点击页面。规则模式无需模型；本机/云端模型失败会自动回退规则计划。</div>
      <div class="ai-settings-hint">模型、规则模式与 GGUF 模型统一在左侧「设置 → AI 助手」中配置；内置 llama-server 可直接加载本机模型。</div>
    </section>
    <section id="ai-planwrap" class="ai-planwrap" hidden>
      <div class="ai-sec-head"><span>执行计划</span><span id="ai-plan-provider" class="ai-tag">规则</span></div>
      <div id="ai-plan-summary" class="ai-summary"></div>
      <div id="ai-plan-warn" class="ai-warn" hidden></div>
      <div id="ai-steps" class="ai-steps"></div>
      <div class="ai-plan-actions"><button class="gradbtn" id="ai-run">${icon("play", 12)} 执行计划</button><button class="ai-mini danger" id="ai-stop" hidden>停止</button><button class="ai-mini" id="ai-clear">清空</button></div>
    </section>
    <section id="ai-runlog" class="ai-runlog" hidden><div class="ai-sec-head"><span>执行日志</span></div><div id="ai-log" class="ai-log"></div></section>
    <section id="ai-report" class="ai-report" hidden></section>
  </div>
</aside>`;
}

function aiSettingsMarkup(): string {
  return `<div class="gset-ai-panel">
  <section class="gset-ai-block">
    <div class="gset-ai-block-head"><b>运行模式</b><span>规则模式不调用模型；本机/云端只发送目标和工具清单。</span></div>
    <div class="ai-field"><label>模式</label><select id="ai-mode"><option value="rule">规则模式（无需模型）</option><option value="local">本机模型</option><option value="cloud">云端模型</option></select></div>
    <div id="ai-mode-tip" class="ai-note ai-mode-tip"></div>
    <div id="ai-local-box" class="ai-provbox" hidden>
      <div class="ai-field"><label>本机接口</label><input id="ai-local-url" placeholder="http://127.0.0.1:11434/v1" spellcheck="false" /></div>
      <div class="ai-field"><label>模型名</label><input id="ai-local-model" placeholder="qwen3:4b" spellcheck="false" /></div>
      <div class="ai-note">本机接口由内置 llama-server 启动后自动填写；也可改为 Ollama / LM Studio 的 OpenAI 兼容地址。</div>
    </div>
    <div id="ai-cloud-box" class="ai-provbox" hidden>
      <div class="ai-field"><label>云端接口</label><input id="ai-cloud-url" placeholder="https://…/v1" spellcheck="false" /></div>
      <div class="ai-field"><label>模型名</label><input id="ai-cloud-model" placeholder="模型名" spellcheck="false" /></div>
      <div class="ai-field"><label>API Key（仅本次会话）</label><input id="ai-cloud-key" type="password" placeholder="留空则读环境变量 VQA_AI_API_KEY" spellcheck="false" /></div>
      <div class="ai-note">Key 不写入配置文件；关闭应用后需重新输入。云端只发送目标和工具清单，不发送原始码流/截图/完整 IP。</div>
    </div>
  </section>
  <section class="gset-ai-block">
    <div class="gset-ai-block-head"><b>本机推理引擎</b><span>安装包内置 llama-server，无需另装 Ollama 或 LM Studio。</span></div>
    <div class="ai-engine-row"><span id="llama-badge" class="ai-model-badge">待命</span><button class="ai-mini" id="llama-stop" hidden>停止引擎</button></div>
    <div id="llama-status" class="ai-note">内置引擎待命；选择下面的 GGUF 模型后点「启用」。</div>
    <details id="llama-log-wrap" class="ai-engine-log" hidden><summary>启动日志</summary><pre id="llama-log"></pre></details>
  </section>
  <section class="gset-ai-block">
    <div class="gset-ai-block-head"><b>连接参数</b><span>超时对本机与云端同时生效；模型清单留空时使用内置清单。</span></div>
    <div class="ai-field"><label>超时（秒）</label><input id="ai-local-timeout" type="number" min="5" max="300" value="60" /></div>
    <div class="ai-field"><label>模型清单地址（可选）</label><input id="ai-index-url" placeholder="留空使用内置清单；可填企业镜像 models.json" spellcheck="false" /></div>
    <div class="ai-actions"><button class="gradbtn" id="ai-save-cfg">保存 AI 设置</button><button class="ai-mini" id="ai-test">测试连接</button><span id="ai-test-out" class="ai-test-out"></span></div>
    <div id="ai-save-out" class="ai-note" hidden></div>
  </section>
  <section class="gset-ai-block">
    <div class="gset-ai-block-head"><b>GGUF 模型</b><span>清单模型可一键下载；离线环境可导入本地 GGUF 文件。</span></div>
    <div id="ai-models" class="ai-models"></div>
    <div id="ai-dl" class="ai-dl" hidden>
      <div class="ai-dl-head"><span id="ai-dl-text"></span><button class="ai-mini" id="ai-dl-cancel">取消</button></div>
      <div class="ai-dl-bar"><i id="ai-dl-fill"></i></div>
    </div>
    <div class="ai-actions"><button class="ai-mini" id="ai-import">导入 GGUF</button><button class="ai-mini" id="ai-check">刷新清单</button></div>
    <div id="ai-model-out" class="ai-note">模型不随应用更新反复重下；下载完成后可随时启用或停止。</div>
  </section>
</div>`;
}

export function initAI() {
  const head = document.querySelector(".pagehead");
  if (!head || $id("ai-open")) return;
  const side = document.createElement("div");
  side.className = "ph-side ai-ph";
  side.innerHTML = `<button id="ai-open" class="ai-entry" title="用自然语言编排检测">${icon("spark", 14)}<span>AI 助手</span><i class="ai-entry-dot"></i></button>`;
  head.appendChild(side);
  document.body.insertAdjacentHTML("beforeend", drawerMarkup());
  const settingsHost = $id("gset-ai-slot");
  if (settingsHost) {
    settingsHost.innerHTML = aiSettingsMarkup();
    bindAiSettingsEvents();
  }
  bindEvents();
  updateBadge();
  void loadConfig();
  void loadModels();
  void loadLlamaStatus();
  void loadDownloadStatus();
}
