//! 来源地址流转：把当前来源栏里的地址送到其它模块的来源栏，并可立即开始工作。
//! 目标模块只负责“接收地址 + 按自己的当前设置启动”，流转层不直接操作各模块的启停按钮，
//! 避免把正在运行的目标误点成停止。

export type FlowModule = "player" | "analysis" | "compliance" | "transcode" | "burst" | "avsync" | "capture";

export interface FlowOutcome {
  state: "started" | "filled" | "failed";
  message: string;
}

type FlowRunner = (url: string) => FlowOutcome | Promise<FlowOutcome>;

interface FlowEntry {
  run: FlowRunner;
  busy?: () => boolean;
}

interface FlowTargetMeta {
  id: FlowModule;
  name: string;
  hint: string;
}

const SOURCE_INPUTS: Record<FlowModule, string> = {
  player: "p-url",
  analysis: "a-url",
  compliance: "k-url",
  transcode: "x-in",
  burst: "b-url",
  avsync: "av-src",
  capture: "c-url",
};

const TARGETS: FlowTargetMeta[] = [
  { id: "player", name: "播放器", hint: "载入并开始播放" },
  { id: "analysis", name: "视频分析", hint: "按当前参数开始扫描" },
  { id: "compliance", name: "合规检测", hint: "按当前依据开始检测" },
  { id: "transcode", name: "直播转码", hint: "按当前参数开始转码" },
  { id: "burst", name: "微突发检测", hint: "按当前时间窗开始检测" },
  { id: "avsync", name: "音画同步", hint: "按当前采样参数开始检测" },
];

const entries = new Map<FlowModule, FlowEntry>();

/** 各模块在 init 时注册自己的接收动作；busy 用于弹层里提前提示“只填入、不启动”。 */
export function registerFlowTarget(id: FlowModule, run: FlowRunner, busy?: () => boolean): void {
  entries.set(id, { run, busy });
}

let pop: HTMLElement | null = null;
let toastEl: HTMLElement | null = null;
let toastTimer = 0;
let popSource: FlowModule | null = null;
let popUrl = "";
let popAnchor: HTMLElement | null = null;
let running = false;

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function targetRow(id: FlowModule): HTMLElement | null {
  return pop?.querySelector<HTMLElement>(`.flowtgt[data-target="${id}"]`) ?? null;
}

function targetState(id: FlowModule): HTMLElement | null {
  return targetRow(id)?.querySelector<HTMLElement>(".flowtgt-state") ?? null;
}

function setTargetState(id: FlowModule, text: string, kind: FlowOutcome["state"] | "off" | "run" | "" = ""): void {
  const state = targetState(id);
  if (!state) return;
  state.textContent = text;
  state.dataset.k = kind;
}

function setStatus(text: string, kind: "idle" | "run" | "ok" | "err" = "idle"): void {
  const box = el<HTMLElement>("flowpop-status");
  if (!box) return;
  box.textContent = text;
  box.dataset.k = kind;
}

function selectedTargets(): FlowModule[] {
  if (!pop) return [];
  return Array.from(pop.querySelectorAll<HTMLInputElement>('.flowtgt input[type="checkbox"]:checked'))
    .map((cb) => cb.value as FlowModule);
}

function updateSelection(): void {
  const n = selectedTargets().length;
  const count = el<HTMLElement>("flowpop-count");
  if (count) count.textContent = n ? `已选 ${n} 个` : "请选择目标模块";
  const go = el<HTMLButtonElement>("flowpop-go");
  if (go) go.disabled = running || n === 0;
  const all = el<HTMLButtonElement>("flowpop-all");
  if (all) {
    const boxes = pop?.querySelectorAll<HTMLInputElement>('.flowtgt input[type="checkbox"]:not(:disabled)') ?? [];
    all.disabled = boxes.length === 0;
  }
}

function refreshRows(): void {
  if (!pop) return;
  TARGETS.forEach((t) => {
    const row = targetRow(t.id);
    const cb = row?.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!row || !cb) return;
    const isSource = t.id === popSource;
    const supported = entries.has(t.id);
    const off = isSource || !supported;
    cb.disabled = off;
    cb.checked = false;
    row.classList.toggle("is-off", off);
    if (isSource) {
      setTargetState(t.id, "当前页", "off");
    } else if (!supported) {
      setTargetState(t.id, "不可用", "off");
    } else if (entries.get(t.id)?.busy?.()) {
      setTargetState(t.id, "运行中 · 只填入", "run");
    } else {
      setTargetState(t.id, "", "");
    }
  });
}

function positionPop(anchor: HTMLElement): void {
  if (!pop) return;
  const rect = anchor.getBoundingClientRect();
  const margin = 8;
  const width = Math.min(348, window.innerWidth - margin * 2);
  let left = Math.min(window.innerWidth - width - margin, Math.max(margin, rect.right - width));
  let top = rect.bottom + 8;
  pop.style.width = `${width}px`;
  pop.style.left = `${left}px`;
  pop.style.top = `${top}px`;
  const h = pop.offsetHeight;
  if (top + h > window.innerHeight - margin) {
    top = Math.max(margin, rect.top - h - 8);
    pop.style.top = `${top}px`;
  }
}

function openPop(source: FlowModule, url: string, anchor: HTMLElement): void {
  if (!pop) return;
  popSource = source;
  popUrl = url;
  popAnchor = anchor;
  const srcEl = el<HTMLElement>("flowpop-src");
  if (srcEl) {
    srcEl.textContent = url;
    srcEl.title = url;
  }
  setStatus("勾选一个或多个模块；多选会同时启动，运行中的模块只填入地址。", "idle");
  refreshRows();
  pop.hidden = false;
  updateSelection();
  positionPop(anchor);
  el<HTMLButtonElement>("flowpop-go")?.focus();
}

function closePop(): void {
  if (!pop) return;
  pop.hidden = true;
  popSource = null;
  popUrl = "";
  popAnchor = null;
}

function showToast(text: string, kind: "ok" | "err" = "ok"): void {
  if (!toastEl) return;
  toastEl.textContent = text;
  toastEl.dataset.kind = kind;
  toastEl.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    if (toastEl) toastEl.hidden = true;
  }, 3200);
}

function gotoPage(id: FlowModule): void {
  const btn = document.querySelector<HTMLElement>(`#nav .mn[data-goto="${id}"]`);
  btn?.click();
}

function shortError(e: unknown): string {
  return String(e).replace(/\s+/g, " ").trim().slice(0, 100);
}

async function runOne(id: FlowModule, url: string): Promise<FlowOutcome> {
  const entry = entries.get(id);
  if (!entry) return { state: "failed", message: "该模块未就绪" };
  setTargetState(id, "正在流转…", "run");
  try {
    const out = await entry.run(url);
    setTargetState(id, out.message, out.state);
    return out;
  } catch (e) {
    const out: FlowOutcome = { state: "failed", message: `启动失败：${shortError(e)}` };
    setTargetState(id, out.message, out.state);
    return out;
  }
}

async function runSelected(): Promise<void> {
  if (running || !popSource) return;
  const ids = selectedTargets();
  if (!ids.length) {
    setStatus("请至少选择一个目标模块。", "err");
    return;
  }
  const url = popUrl;
  const names = ids.map((id) => TARGETS.find((t) => t.id === id)?.name || id);
  const single = ids.length === 1;
  running = true;
  updateSelection();

  // 单选时先切到目标页，让预览/播放区在可见状态下启动；多选时留在当前页后台启动。
  if (single) {
    closePop();
    gotoPage(ids[0]);
    await new Promise((r) => window.setTimeout(r, 80));
    const out = await runOne(ids[0], url);
    const name = names[0];
    if (out.state === "started") {
      showToast(`已流转到 ${name} 并开始工作`, "ok");
    } else if (out.state === "filled") {
      showToast(`已流转到 ${name}：${out.message}`, "ok");
    } else {
      showToast(`流转到 ${name} 失败：${out.message}`, "err");
    }
    running = false;
    return;
  }

  setStatus(`正在流转到 ${ids.length} 个模块…`, "run");
  let started = 0;
  let filled = 0;
  let failed = 0;
  for (const id of ids) {
    const out = await runOne(id, url);
    if (out.state === "started") started += 1;
    else if (out.state === "filled") filled += 1;
    else failed += 1;
  }
  const bits = [`已流转 ${ids.length} 个模块`, `启动 ${started}`];
  if (filled) bits.push(`仅填入 ${filled}`);
  if (failed) bits.push(`失败 ${failed}`);
  const text = bits.join(" · ");
  setStatus(text, failed ? "err" : "ok");
  showToast(`${text}：${names.join("、")}`, failed ? "err" : "ok");
  running = false;
  updateSelection();
}

function bindButtons(): void {
  document.querySelectorAll<HTMLButtonElement>("[data-flow]").forEach((btn) => {
    if (btn.dataset.flowBound === "1") return;
    btn.dataset.flowBound = "1";
    btn.addEventListener("click", (ev) => {
      ev.preventDefault();
      const source = btn.dataset.flow as FlowModule;
      const input = el<HTMLInputElement>(SOURCE_INPUTS[source] || "");
      const url = input?.value.trim() || "";
      if (!url) {
        showToast("来源地址为空，请先填写地址", "err");
        return;
      }
      openPop(source, url, btn);
    });
  });
}

function bindSourceInputs(): void {
  (Object.keys(SOURCE_INPUTS) as FlowModule[]).forEach((source) => {
    const input = el<HTMLInputElement>(SOURCE_INPUTS[source]);
    const sync = () => {
      // 频道列表点选、历史记录、浏览文件都是程序赋值，不一定触发 input 事件；
      // 因此按钮保持可点，真正为空时在点击处给提示，避免按钮状态卡在禁用。
      if (!input?.value.trim() && !pop?.hidden && popSource === source) closePop();
    };
    input?.addEventListener("input", sync);
    sync();
  });
}

function buildPop(): void {
  pop = document.createElement("div");
  pop.id = "flowpop";
  pop.className = "flowpop";
  pop.hidden = true;
  pop.setAttribute("role", "dialog");
  pop.setAttribute("aria-label", "流转来源到其它模块");
  pop.innerHTML = `
    <div class="flowpop-head">
      <div class="flowpop-headtxt">
        <b>流转来源</b>
        <span class="flowpop-src" id="flowpop-src"></span>
      </div>
      <button class="flowpop-x" id="flowpop-x" type="button" title="关闭" aria-label="关闭">×</button>
    </div>
    <div class="flowpop-list">
      ${TARGETS.map(
        (t) => `
        <label class="flowtgt" data-target="${t.id}">
          <input type="checkbox" value="${t.id}" />
          <span class="flowtgt-body">
            <b class="flowtgt-name">${t.name}</b>
            <small class="flowtgt-hint">${t.hint}</small>
          </span>
          <span class="flowtgt-state" data-k=""></span>
        </label>`,
      ).join("")}
    </div>
    <div class="flowpop-acts">
      <span class="flowpop-count" id="flowpop-count">请选择目标模块</span>
      <button class="flowpop-mini" id="flowpop-all" type="button">全选</button>
      <button class="flowpop-mini" id="flowpop-none" type="button">清空</button>
      <button class="flowpop-go" id="flowpop-go" type="button">流转并开始</button>
    </div>
    <div class="flowpop-status" id="flowpop-status" data-k="idle" role="status" aria-live="polite"></div>
  `;
  document.body.appendChild(pop);

  toastEl = document.createElement("div");
  toastEl.id = "flowtoast";
  toastEl.className = "flowtoast";
  toastEl.hidden = true;
  toastEl.setAttribute("role", "status");
  toastEl.setAttribute("aria-live", "polite");
  document.body.appendChild(toastEl);

  el<HTMLButtonElement>("flowpop-x")?.addEventListener("click", closePop);
  el<HTMLButtonElement>("flowpop-all")?.addEventListener("click", () => {
    pop?.querySelectorAll<HTMLInputElement>('.flowtgt input[type="checkbox"]:not(:disabled)').forEach((cb) => {
      cb.checked = true;
    });
    updateSelection();
  });
  el<HTMLButtonElement>("flowpop-none")?.addEventListener("click", () => {
    pop?.querySelectorAll<HTMLInputElement>('.flowtgt input[type="checkbox"]').forEach((cb) => {
      cb.checked = false;
    });
    updateSelection();
  });
  el<HTMLButtonElement>("flowpop-go")?.addEventListener("click", () => void runSelected());
  pop.querySelectorAll<HTMLInputElement>('.flowtgt input[type="checkbox"]').forEach((cb) => {
    cb.addEventListener("change", updateSelection);
  });
}

/** 页面初始化完成后调用一次：创建弹层、绑定来源栏按钮。 */
export function initFlow(): void {
  if (pop) return;
  buildPop();
  bindButtons();
  bindSourceInputs();

  document.addEventListener("pointerdown", (ev) => {
    if (!pop || pop.hidden) return;
    const t = ev.target as Node;
    if (pop.contains(t)) return;
    const btn = (ev.target as Element).closest?.("[data-flow]");
    if (btn) return;
    closePop();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && pop && !pop.hidden) closePop();
  });
  window.addEventListener(
    "scroll",
    (ev) => {
      if (!pop || pop.hidden) return;
      if (pop.contains(ev.target as Node)) return;
      closePop();
    },
    true,
  );
  window.addEventListener("resize", () => {
    if (pop && !pop.hidden && popAnchor) positionPop(popAnchor);
  });
  window.addEventListener("page-changed", () => closePop());
}
