//! AI 助手抽屉：自然语言 → 计划 → 通过现有 Tauri API 执行 → 汇总报告。
//!
//! 纪律：这里不点击任何页面控件，也不模拟 DOM 事件；所有模块动作都调 `api.*`
//! （即 Tauri 命令）。页面只负责展示状态和确认，AI 与手动操作共用同一套后端。

import { api, type AiConfig, type AiModelCheckItem, type AiModelInfo, type AiPlan, type AiStep } from "./api";
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
let settingsOpen = false;
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

function updateBadge() {
  const b = $id("ai-mode-badge");
  if (!b) return;
  b.textContent = modeText();
  b.className = `ai-mode-badge${cfg?.mode === "rule" ? " rule" : cfg?.mode === "cloud" ? " cloud" : " local"}`;
  const tip = $id("ai-mode-tip");
  if (tip) {
    tip.textContent =
      cfg?.mode === "local"
        ? "本机模型：只把目标和工具清单发给本机接口，来源地址不出本机。"
        : cfg?.mode === "cloud"
          ? "云端模型：只发送目标和工具清单，不发送原始码流、截图或完整 IP；需要显式配置。"
          : "规则模式：不调用模型，按关键词生成计划，稳定且不消耗额度。";
  }
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
  } catch (e: any) {
    setNote(`读取 AI 配置失败：${String(e).slice(0, 160)}`, "err");
  }
}

async function saveConfig(show = true): Promise<AiConfig | null> {
  try {
    cfg = await api.aiConfigSet(collectConfig());
    updateBadge();
    if (show) setSettingsNote("设置已保存", "ok");
    return cfg;
  } catch (e: any) {
    if (show) setSettingsNote(`保存失败：${String(e).slice(0, 200)}`, "err");
    return null;
  }
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
  if (!models.length) {
    box.innerHTML =
      '<div class="ai-empty"><b>还没有导入模型</b><small>请自行从模型发布页下载 GGUF 文件，再点“导入 GGUF”。应用不会自动下载模型。</small></div>';
    return;
  }
  box.innerHTML = models
    .map((m) => {
      const c = checkItems.get(m.id);
      const badge = c?.update_available
        ? '<span class="ai-model-badge up">可升级</span>'
        : m.present
          ? '<span class="ai-model-badge ok">已导入</span>'
          : '<span class="ai-model-badge bad">文件缺失</span>';
      return `<div class="ai-model"><div class="ai-model-main"><b>${h(m.name)}</b><small>${fmtBytes(m.bytes)} · ${m.version ? `v${h(m.version)}` : "版本未知"}${c?.version ? ` · 清单 ${h(c.version)}` : ""}</small></div>${badge}<button class="ai-mini" data-model-remove="${h(m.id)}">移除</button></div>`;
    })
    .join("");
  box.querySelectorAll<HTMLButtonElement>("[data-model-remove]").forEach((b) => {
    b.addEventListener("click", () => void removeModel(b.dataset.modelRemove || ""));
  });
}

async function removeModel(id: string) {
  if (!id) return;
  if (!window.confirm(`移除模型「${id}」？会删除已导入的模型文件。`)) return;
  try {
    await api.aiModelRemove(id);
    checkItems.delete(id);
    await loadModels();
    setSettingsNote("模型已移除", "ok");
  } catch (e: any) {
    setSettingsNote(`移除失败：${String(e).slice(0, 200)}`, "err");
  }
}

async function checkModels() {
  const url = q<HTMLInputElement>("ai-index-url").value.trim();
  setSettingsNote("检查模型清单中…");
  try {
    const res = await api.aiModelCheck(url || undefined);
    checkItems = new Map(res.models.map((m) => [m.id, m]));
    renderModels();
    const up = res.models.filter((m) => m.update_available).length;
    const installable = res.models.filter((m) => !m.installed).length;
    const parts: string[] = [];
    if (up) parts.push(`${up} 个有可升级版本`);
    if (installable) parts.push(`${installable} 个清单模型尚未导入`);
    setSettingsNote(parts.length ? `检查完成：${parts.join("，")}。请自行下载后导入。` : "检查完成：当前没有可升级版本，也未发现未导入的清单模型。", parts.length ? "ok" : "normal");
  } catch (e: any) {
    setSettingsNote(`检查失败：${String(e).slice(0, 240)}`, "err");
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

function openSettings() {
  settingsOpen = true;
  $id("ai-settings")?.removeAttribute("hidden");
  fillConfigForm(cfg || collectConfig());
  updateBadge();
  void loadModels();
}

function closeSettings() {
  settingsOpen = false;
  $id("ai-settings")?.setAttribute("hidden", "");
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
  if (settingsOpen) {
    closeSettings();
    return;
  }
  const scrim = $id("ai-scrim");
  const drawer = $id("ai-drawer");
  if (scrim) scrim.hidden = true;
  if (drawer) {
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
    const saved = await saveConfig(false);
    if (!saved) {
      setNote("模型设置未保存成功，请到“模型设置”检查接口地址/模型名。", "err");
      return;
    }
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

function bindEvents() {
  q<HTMLButtonElement>("ai-open").addEventListener("click", openDrawer);
  q<HTMLButtonElement>("ai-close").addEventListener("click", closeDrawer);
  q<HTMLElement>("ai-scrim").addEventListener("click", closeDrawer);
  q<HTMLButtonElement>("ai-settings-btn").addEventListener("click", openSettings);
  q<HTMLButtonElement>("ai-settings-close").addEventListener("click", closeSettings);
  q<HTMLButtonElement>("ai-use-page").addEventListener("click", useCurrentSource);
  q<HTMLButtonElement>("ai-generate").addEventListener("click", () => void generatePlan());
  q<HTMLButtonElement>("ai-run").addEventListener("click", () => void executePlan());
  q<HTMLButtonElement>("ai-stop").addEventListener("click", () => void stopRun());
  q<HTMLButtonElement>("ai-clear").addEventListener("click", clearPlan);
  q<HTMLButtonElement>("ai-save-cfg").addEventListener("click", () => void saveConfig());
  q<HTMLButtonElement>("ai-test").addEventListener("click", () => void testProvider());
  q<HTMLButtonElement>("ai-import").addEventListener("click", () => void importModel());
  q<HTMLButtonElement>("ai-check").addEventListener("click", () => void checkModels());
  q<HTMLSelectElement>("ai-mode").addEventListener("change", updateModeBoxes);
  q<HTMLInputElement>("ai-cloud-key").addEventListener("input", (e) => {
    sessionKey = (e.target as HTMLInputElement).value.trim();
  });

  document.querySelectorAll<HTMLElement>("[data-ai-example]").forEach((b) => {
    b.addEventListener("click", () => {
      q<HTMLTextAreaElement>("ai-goal").value = b.dataset.aiExample || "";
      q<HTMLTextAreaElement>("ai-goal").focus();
    });
  });

  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (settingsOpen) closeSettings();
    else if ($id("ai-drawer")?.classList.contains("open")) closeDrawer();
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
}

function drawerMarkup(): string {
  const examples = EXAMPLES.map((x) => `<button class="ai-chip" data-ai-example="${h(x)}">${h(x)}</button>`).join("");
  return `<div id="ai-scrim" class="ai-scrim" hidden></div>
<aside id="ai-drawer" class="ai-drawer" aria-hidden="true">
  <header class="ai-head">
    <div class="ai-brand"><span class="ai-mark">${icon("spark", 18)}</span><div><b>AI 检测助手</b><small id="ai-mode-badge" class="ai-mode-badge">读取配置…</small></div></div>
    <div class="ai-head-acts"><button class="iconbtn" id="ai-settings-btn" title="模型与设置">${icon("settings", 15)}</button><button class="iconbtn" id="ai-close" title="关闭">${icon("x", 15)}</button></div>
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
  <div class="ai-settings" id="ai-settings" hidden>
    <div class="ai-settings-head"><b>模型与设置</b><button class="iconbtn" id="ai-settings-close" title="关闭设置">${icon("x", 13)}</button></div>
    <div class="ai-settings-body">
      <div class="ai-field"><label>运行模式</label><select id="ai-mode"><option value="rule">规则模式（无需模型）</option><option value="local">本机模型</option><option value="cloud">云端模型</option></select></div>
      <div id="ai-mode-tip" class="ai-note ai-mode-tip"></div>
      <div id="ai-local-box" class="ai-provbox" hidden>
        <div class="ai-field"><label>本机接口</label><input id="ai-local-url" placeholder="http://127.0.0.1:11434/v1" spellcheck="false" /></div>
        <div class="ai-field"><label>模型名</label><input id="ai-local-model" placeholder="qwen3:4b" spellcheck="false" /></div>
        <div class="ai-note">本机模型由你自行下载并启动（如 Ollama / llama.cpp 的 OpenAI 兼容接口）；应用不内置模型文件。</div>
      </div>
      <div id="ai-cloud-box" class="ai-provbox" hidden>
        <div class="ai-field"><label>云端接口</label><input id="ai-cloud-url" placeholder="https://…/v1" spellcheck="false" /></div>
        <div class="ai-field"><label>模型名</label><input id="ai-cloud-model" placeholder="模型名" spellcheck="false" /></div>
        <div class="ai-field"><label>API Key（仅本次会话）</label><input id="ai-cloud-key" type="password" placeholder="留空则读 VQA_AI_API_KEY" spellcheck="false" /></div>
        <div class="ai-note">Key 不写入配置文件；关闭应用后需重新输入。云端只发送目标和工具清单，不发送原始码流/截图/完整 IP。</div>
      </div>
      <div class="ai-field"><label>超时（秒）</label><input id="ai-local-timeout" type="number" min="5" max="300" value="60" /></div>
      <div class="ai-field"><label>模型清单地址（可选）</label><input id="ai-index-url" placeholder="https://…/models.json；留空只支持本地导入" spellcheck="false" /></div>
      <div class="ai-actions"><button class="gradbtn" id="ai-save-cfg">保存设置</button><button class="ai-mini" id="ai-test">测试连接</button><span id="ai-test-out" class="ai-test-out"></span></div>
      <div class="ai-models-head"><b>本机模型</b><small>只检查/导入，不自动下载</small></div>
      <div id="ai-models" class="ai-models"></div>
      <div class="ai-actions"><button class="ai-mini" id="ai-import">导入 GGUF</button><button class="ai-mini" id="ai-check">检查可升级</button></div>
      <div id="ai-model-out" class="ai-note">模型文件请自行从发布页下载；应用只读取清单版本与 SHA256，不替你下载。</div>
    </div>
  </div>
</aside>`;
}

export function initAI() {
  const head = document.querySelector(".pagehead");
  if (!head || $id("ai-open")) return;
  const side = document.createElement("div");
  side.className = "ph-side ai-ph";
  side.innerHTML = `<button id="ai-open" class="ai-entry" title="用自然语言编排检测">${icon("spark", 14)}<span>AI 助手</span><i class="ai-entry-dot"></i></button>`;
  head.appendChild(side);
  document.body.insertAdjacentHTML("beforeend", drawerMarkup());
  bindEvents();
  updateBadge();
  void loadConfig();
  void loadModels();
}
