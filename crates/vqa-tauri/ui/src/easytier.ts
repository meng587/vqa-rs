// EasyTier 组网页：内置内核的配置、启停、节点与日志。
// 运行态轮询只在页面可见时进行；离开页面不停止内核，组网继续在后台工作。

import { api, type EasytierConfig, type EasytierStatus } from "./api";

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T | null;

const DEFAULT_CFG: EasytierConfig = {
  network_name: "",
  network_secret: "",
  ipv4: "",
  dhcp: false,
  peers: [],
  proxy_networks: [],
  external_node: "",
  hostname: "",
  rpc_port: 15889,
  no_listener: false,
  no_tun: false,
};

let active = false;
let timer: number | null = null;
let loaded = false;
let dirty = false;
let lastLog = "";
let lastStatus: EasytierStatus | null = null;
let inflight = false;
/** 用户操作产生的错误，独立于一次状态轮询；避免下一次轮询把启动失败提示冲掉。 */
let actionError = "";

function errText(e: unknown): string {
  const s = e instanceof Error ? e.message : String(e);
  return s.replace(/^Error:\s*/i, "");
}

function setText(id: string, text: string) {
  const el = $(id);
  if (el) el.textContent = text;
}

function setActionError(msg: string) {
  actionError = msg;
  const errBox = $("et-error");
  if (errBox) {
    errBox.hidden = !msg;
    errBox.textContent = msg;
  }
}

function setBusy(busy: boolean) {
  const start = $<HTMLButtonElement>("et-start");
  const stop = $<HTMLButtonElement>("et-stop");
  const save = $<HTMLButtonElement>("et-save");
  if (start) start.disabled = busy;
  if (stop) stop.disabled = busy;
  if (save) save.disabled = busy;
  if (!busy) {
    const running = !!lastStatus?.running;
    // 忙碌态会临时禁用按钮，结束后必须重新按运行态恢复，否则首次启动后
    // “停止”会一直保持 disabled，首次停止后“启动”也会一直不可点。
    if (save) save.disabled = running;
    if (start) {
      start.disabled = false;
      start.hidden = running;
    }
    if (stop) {
      stop.disabled = false;
      stop.hidden = !running;
    }
  }
}

function lines(v: string): string[] {
  return v
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function cfgFromForm(): EasytierConfig {
  return {
    network_name: ($<HTMLInputElement>("et-name")?.value || "").trim(),
    network_secret: ($<HTMLInputElement>("et-secret")?.value || "").trim(),
    ipv4: ($<HTMLInputElement>("et-ipv4")?.value || "").trim(),
    dhcp: !!$<HTMLInputElement>("et-dhcp")?.checked,
    peers: lines($<HTMLTextAreaElement>("et-peers")?.value || ""),
    proxy_networks: lines($<HTMLTextAreaElement>("et-proxy")?.value || ""),
    external_node: ($<HTMLInputElement>("et-external")?.value || "").trim(),
    hostname: ($<HTMLInputElement>("et-hostname")?.value || "").trim(),
    rpc_port: Number($<HTMLInputElement>("et-rpc")?.value || 15889) || 15889,
    no_listener: !!$<HTMLInputElement>("et-nolisten")?.checked,
    no_tun: !!$<HTMLInputElement>("et-notun")?.checked,
  };
}

function cfgOrDefault(v: Partial<EasytierConfig> | null): EasytierConfig {
  return { ...DEFAULT_CFG, ...(v || {}) };
}

function fillForm(cfg: EasytierConfig) {
  const setVal = (id: string, v: string) => {
    const el = $<HTMLInputElement>(id);
    if (el) el.value = v;
  };
  setVal("et-name", cfg.network_name);
  setVal("et-secret", cfg.network_secret);
  setVal("et-ipv4", cfg.ipv4);
  setVal("et-hostname", cfg.hostname);
  setVal("et-external", cfg.external_node);
  setVal("et-rpc", String(cfg.rpc_port || 15889));
  const peers = $<HTMLTextAreaElement>("et-peers");
  if (peers) peers.value = cfg.peers.join("\n");
  const proxy = $<HTMLTextAreaElement>("et-proxy");
  if (proxy) proxy.value = cfg.proxy_networks.join("\n");
  const checks: Array<[string, boolean]> = [
    ["et-dhcp", cfg.dhcp],
    ["et-nolisten", cfg.no_listener],
    ["et-notun", cfg.no_tun],
  ];
  checks.forEach(([id, on]) => {
    const el = $<HTMLInputElement>(id);
    if (el) el.checked = on;
  });
  syncDhcpUI();
  markDirty(false);
}

function syncDhcpUI() {
  const dhcp = !!$<HTMLInputElement>("et-dhcp")?.checked;
  const ip = $<HTMLInputElement>("et-ipv4");
  if (ip && !lastStatus?.running) {
    ip.disabled = dhcp;
    ip.classList.toggle("is-off", dhcp);
  }
}

function markDirty(on: boolean, hint?: string) {
  dirty = on;
  const flag = $("et-cfgflag");
  if (flag) {
    flag.textContent = on ? "未保存" : "已同步";
    flag.classList.toggle("is-dirty", on);
  }
  if (hint) {
    setText("et-savehint", hint);
    window.setTimeout(() => {
      if (!dirty) setText("et-savehint", "参数只保存在本机");
    }, 2600);
  }
}

function lockForm(running: boolean) {
  const ids = ["et-name", "et-secret", "et-ipv4", "et-dhcp", "et-hostname", "et-rpc", "et-peers", "et-proxy", "et-external", "et-nolisten", "et-notun"];
  ids.forEach((id) => {
    const el = $<HTMLInputElement>(id);
    if (el) el.disabled = running;
  });
  if (!running) syncDhcpUI();
  const save = $<HTMLButtonElement>("et-save");
  if (save) save.disabled = running;
  const start = $<HTMLButtonElement>("et-start");
  const stop = $<HTMLButtonElement>("et-stop");
  if (start) start.hidden = running;
  if (stop) stop.hidden = !running;
}

function setPill(kind: "idle" | "run" | "err", text: string) {
  const pill = $("et-pill");
  if (pill) {
    pill.classList.toggle("is-run", kind === "run");
    pill.classList.toggle("is-err", kind === "err");
  }
  setText("et-pilltext", text);
  const lamp = $("lamp-easytier");
  if (lamp) lamp.className = `lamp${kind === "run" ? " run" : kind === "err" ? " err" : ""}`;
}

function fmtUptime(secs: number): string {
  const s = Math.max(0, secs || 0);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = Math.floor(s % 60);
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(ss).padStart(2, "0")}`;
}

function asString(v: unknown): string {
  if (v === null || v === undefined || v === "") return "—";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return "—";
}

/** CLI 的 node.ipv4_addr 带 /24，KPI 和表格里只显示地址本身。 */
function ipOnly(v: unknown): string {
  return asString(v).replace(/\/\d+$/, "");
}

function publicIp(node: any): string {
  if (!node) return "—";
  const s = node.stun_info;
  const pick = (v: unknown): string => {
    if (Array.isArray(v)) return v.length ? asString(v[0]) : "—";
    return asString(v);
  };
  if (Array.isArray(s)) {
    const hit = s.find((x) => x && (x.public_ip || x.public_ipv4));
    return pick(hit?.public_ip || hit?.public_ipv4);
  }
  return pick(s?.public_ip || s?.public_ipv4 || node.public_ip);
}

function latency(v: unknown): string {
  if (v === null || v === undefined || v === "" || v === "-") return "—";
  const n = Number(v);
  return Number.isFinite(n) ? `${n} ms` : asString(v);
}

function percent(v: unknown): string {
  if (v === null || v === undefined || v === "") return "—";
  // easytier-cli 的 JSON 已经格式化过，例如 "0.0%"；直接保留这个字符串。
  if (typeof v === "string") {
    const s = v.trim();
    if (s === "-") return "—";
    if (s.endsWith("%")) {
      const n = Number(s.slice(0, -1));
      return Number.isFinite(n) ? s : "—";
    }
  }
  const n = Number(v);
  if (!Number.isFinite(n)) return "—";
  return `${n.toFixed(n < 10 ? 1 : 0)}%`;
}

function peerCell(text: string, cls = ""): HTMLTableCellElement {
  const td = document.createElement("td");
  td.textContent = text || "—";
  if (cls) td.className = cls;
  return td;
}

function peerPath(p: any): string {
  const tunnel = p?.tunnel_proto && p.tunnel_proto !== "-" ? String(p.tunnel_proto) : "";
  const cost = p?.cost && p.cost !== "-" ? String(p.cost) : "";
  if (cost === "Local") return tunnel || "本地";
  return [tunnel, cost].filter(Boolean).join(" · ") || "—";
}

function renderPeers(peers: any[]) {
  const body = $("et-peers-body");
  if (!body) return;
  const remote = peers.filter((p) => !!p && String(p.cost || "") !== "Local");
  const count = $("et-peer-count");
  if (count) count.textContent = `${remote.length} 个连接`;
  body.replaceChildren();
  if (!remote.length) {
    const tr = document.createElement("tr");
    tr.className = "et-empty";
    const td = document.createElement("td");
    td.colSpan = 6;
    td.textContent = lastStatus?.running ? "暂无对端连接，等待其它节点接入" : "启动后显示已连接的 EasyTier 节点";
    tr.append(td);
    body.append(tr);
    return;
  }
  remote.forEach((p) => {
    const tr = document.createElement("tr");
    tr.append(
      peerCell(ipOnly(p.ipv4), "et-mono"),
      peerCell(asString(p.hostname), "et-host"),
      peerCell(latency(p.lat_ms), "et-mono"),
      peerCell(percent(p.loss_rate), "et-mono"),
      peerCell(peerPath(p)),
      peerCell(asString(p.nat_type), "et-nat"),
    );
    body.append(tr);
  });
}

function renderLog(log: string) {
  const pre = $("et-log");
  if (!pre) return;
  const text = log || "等待启动 EasyTier 内核…";
  if (text === lastLog) return;
  const nearBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 28;
  pre.textContent = text;
  lastLog = text;
  if (nearBottom) pre.scrollTop = pre.scrollHeight;
}

function renderStatus(st: EasytierStatus) {
  lastStatus = st;
  const admin = !!st.admin;
  const running = !!st.running;
  const mode = $("et-mode");
  if (mode) {
    mode.textContent = admin ? "管理员" : "受限";
    mode.classList.toggle("is-admin", admin);
    mode.classList.toggle("is-limited", !admin);
  }
  const version = (st.version || "2.6.4").replace(/^easytier-core\s*/i, "").trim();
  setText("et-ver", version || "2.6.4");

  if (running) {
    setPill("run", "运行中");
    setText("et-statebig", "组网运行中");
    setText("et-statesub", `PID ${st.pid || "—"} · RPC ${st.config?.rpc_port || 15889}`);
  } else if (st.error) {
    setPill("err", "已退出");
    setText("et-statebig", "组网已停止");
    setText("et-statesub", admin ? "检查内核日志后重新启动" : "当前非管理员，TUN 模式需要提权");
  } else {
    setPill("idle", "未启动");
    setText("et-statebig", "未启动");
    setText("et-statesub", admin ? "填写网络名称与密钥后启动" : "当前非管理员，仅可先试 no-TUN 模式");
  }

  const orb = $("et-orb");
  if (orb) {
    orb.classList.toggle("is-run", running);
    orb.classList.toggle("is-err", !running && !!st.error);
  }

  const node = st.node || {};
  setText("et-k-ip", ipOnly(node.ipv4_addr || node.ipv4));
  setText("et-k-id", asString(node.peer_id ?? node.inst_id));
  setText("et-k-pub", publicIp(node));
  setText("et-k-up", running ? fmtUptime(st.uptime_secs) : "—");

  const bar = $("et-adminbar");
  if (bar) bar.hidden = admin;

  const errBox = $("et-error");
  if (errBox) {
    const msg = st.error || actionError;
    errBox.hidden = !msg;
    errBox.textContent = msg;
  }

  lockForm(running);
  renderPeers(Array.isArray(st.peers) ? st.peers : []);
  renderLog(st.log || "");
}

async function refresh(showBusy = false) {
  if (inflight) {
    // 轮询已经在飞：普通轮询直接跳过，用户操作触发的刷新则等它结束再跑，
    // 避免启动/停止后的状态被上一轮旧结果覆盖或被直接跳过。
    if (!showBusy) return;
    while (inflight) await new Promise((resolve) => window.setTimeout(resolve, 100));
  }
  inflight = true;
  if (showBusy) setBusy(true);
  try {
    const st = await api.easytierStatus();
    renderStatus(st);
  } catch (e) {
    const msg = errText(e);
    setPill("err", "不可用");
    const errBox = $("et-error");
    if (errBox) {
      errBox.hidden = false;
      errBox.textContent = `读取 EasyTier 状态失败：${msg}`;
    }
  } finally {
    inflight = false;
    if (showBusy) setBusy(false);
  }
}

async function loadConfig(force = false) {
  if (loaded && !force) return;
  try {
    const cfg = cfgOrDefault(await api.easytierConfigGet());
    fillForm(cfg);
    loaded = true;
  } catch (e) {
    loaded = true;
    fillForm(DEFAULT_CFG);
    markDirty(false);
    setText("et-savehint", `配置读取失败：${errText(e)}`);
  }
}

async function saveConfig(silent = false): Promise<boolean> {
  const cfg = cfgFromForm();
  if (!cfg.network_name) {
    markDirty(true, "请先填写网络名称");
    return false;
  }
  if (!cfg.network_secret) {
    markDirty(true, "请先填写网络密钥");
    return false;
  }
  try {
    await api.easytierConfigSave(cfg);
    markDirty(false, silent ? undefined : "参数已保存");
    return true;
  } catch (e) {
    markDirty(true, `保存失败：${errText(e)}`);
    return false;
  }
}

async function onStart() {
  if (!(await saveConfig(true))) return;
  setActionError("");
  setBusy(true);
  try {
    await api.easytierStart(cfgFromForm());
    actionError = "";
    await refresh(true);
  } catch (e) {
    setPill("err", "启动失败");
    setActionError(errText(e));
  } finally {
    setBusy(false);
  }
}

async function onStop() {
  setActionError("");
  setBusy(true);
  try {
    await api.easytierStop();
    actionError = "";
    await refresh(true);
  } catch (e) {
    setActionError(errText(e));
  } finally {
    setBusy(false);
  }
}

async function onElevate() {
  try {
    setActionError("");
    await api.easytierRestartElevated();
  } catch (e) {
    setActionError(errText(e));
  }
}

async function copyLog() {
  const text = $("et-log")?.textContent || "";
  try {
    await navigator.clipboard.writeText(text);
    setText("et-copy", "已复制");
    window.setTimeout(() => setText("et-copy", "复制"), 1400);
  } catch {
    setText("et-copy", "复制失败");
    window.setTimeout(() => setText("et-copy", "复制"), 1400);
  }
}

export function initEasytier() {
  const bind = (id: string, ev: string, fn: () => void) => {
    $(id)?.addEventListener(ev, (e) => {
      e.preventDefault();
      fn();
    });
  };
  bind("et-start", "click", () => void onStart());
  bind("et-stop", "click", () => void onStop());
  bind("et-save", "click", () => void saveConfig(false));
  bind("et-refresh", "click", () => void refresh(true));
  bind("et-peer-refresh", "click", () => void refresh(true));
  bind("et-elevate", "click", () => void onElevate());
  bind("et-copy", "click", () => void copyLog());

  const edited = () => {
    setActionError("");
    markDirty(true);
  };
  ["et-name", "et-secret", "et-ipv4", "et-hostname", "et-rpc", "et-peers", "et-proxy", "et-external"].forEach((id) => {
    $(id)?.addEventListener("input", edited);
  });
  ["et-dhcp", "et-nolisten", "et-notun"].forEach((id) => {
    $(id)?.addEventListener("change", () => {
      edited();
      if (id === "et-dhcp") syncDhcpUI();
    });
  });
}

export function enterEasytier() {
  active = true;
  void loadConfig();
  void refresh(true);
  if (timer === null) {
    timer = window.setInterval(() => {
      if (active && !document.hidden) void refresh();
    }, 2000);
  }
}

export function leaveEasytier() {
  active = false;
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}
