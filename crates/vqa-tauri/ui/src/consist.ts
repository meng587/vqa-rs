//! 一致性比对页（严格三层）：分组 → 频道 → 来源，组内多频道、频道内多来源，双画面预览、单对/整组/全部轮询。
//!
//! 分组数据存 shared/settings.json 的 `consist.groups`；参数存 `consist.prefs`。
//! 每次比对复用后端 consist 任务槽，全部轮询按并发路数调度，任务 id 为 all-1…all-8。

import { api, AlarmRecord, Channel, ConsistResult } from "./api";
import { openConsistEditor } from "./consist-editor";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const esc = (s: string) =>
  String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

export interface CSource {
  id: string;
  name: string;
  url: string;
  base: boolean;
  enabled: boolean;
}
export interface CChannel {
  id: string;
  name: string;
  mode: "base" | "pairwise";
  sources: CSource[];
}
export interface CGroup {
  id: string;
  name: string;
  channels: CChannel[];
}
interface Pair {
  g: CGroup;
  ch: CChannel;
  ref: CSource;
  tgt: CSource;
}
interface ResultRow {
  r: ConsistResult;
  at: string;
}
interface GroupState {
  text: string;
  cls: "idle" | "ok" | "warn" | "bad";
  summary: string;
}
interface Cfg {
  fps: number;
  secs: number;
  lag: number;
  thr: number;
  lx: string;
  ly: string;
  lw: string;
  lh: string;
  conc: number;
}

const G_KEY = "consist.groups";
const P_KEY = "consist.prefs";
const DEFAULT_CFG: Cfg = { fps: 10, secs: 20, lag: 3, thr: 0.75, lx: "", ly: "", lw: "", lh: "", conc: 2 };

let groups: CGroup[] = [];
let activeGid = "";
let activeCid = "";
let activeTgt = "";
let cfg: Cfg = { ...DEFAULT_CFG };
let results = new Map<string, ResultRow>();
let groupStates = new Map<string, GroupState>();
let channelStates = new Map<string, GroupState>();
let channelErrors = new Map<string, string>();
let runningAll = false;
let running = false;
let stopRequested = false;
let collapsed = false;
let cgGroupFilter = "all";
let cgQuery = "";
let selectedPairs = new Set<string>();
let selectionReady = false;
let editGid = "";
let editCid = "";
let groupDeleteArmed = false;
let channelDeleteArmed = false;
let closedGroups = new Set<string>();

let prevTimer = 0;
let prevRefKey = "";
let prevTgtKey = "";
let lastRefUrl = "";
let lastTgtUrl = "";

const uid = () => `s${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
const chKey = (g: CGroup, ch: CChannel) => `${g.id}:${ch.id}`;
const resultKey = (p: Pair) => `${p.g.id}:${p.ch.id}:${p.ref.id}:${p.tgt.id}`;
const pairLabel = (p: Pair) => `${p.ref.name || "基准"} ↔ ${p.tgt.name || "待比"}`;

function enabledSources(ch: CChannel): CSource[] {
  return ch.sources.filter((s) => s.enabled && s.url.trim());
}
function baseOf(ch: CChannel): CSource | null {
  const on = enabledSources(ch);
  return on.find((s) => s.base) || on[0] || null;
}
function pairOf(ch: CChannel, targetId = activeTgt, g = activeGroup()): Pair | null {
  const on = enabledSources(ch);
  const ref = baseOf(ch);
  if (!g || !ref || on.length < 2) return null;
  const tgt =
    on.find((s) => s.id === targetId && s.id !== ref.id && s.enabled) ??
    on.find((s) => s.id !== ref.id && s.enabled) ??
    null;
  if (!tgt) return null;
  return { g, ch, ref, tgt };
}
function channelPairs(ch: CChannel, g = activeGroup()): Pair[] {
  const on = enabledSources(ch);
  if (!g || on.length < 2) return [];
  if (ch.mode === "pairwise") {
    const out: Pair[] = [];
    for (let i = 0; i < on.length; i++)
      for (let j = i + 1; j < on.length; j++) out.push({ g, ch, ref: on[i], tgt: on[j] });
    return out;
  }
  const ref = baseOf(ch)!;
  return on.filter((s) => s.id !== ref.id).map((tgt) => ({ g, ch, ref, tgt }));
}
function groupPairs(g: CGroup): Pair[] {
  return g.channels.flatMap((ch) => channelPairs(ch, g));
}
function activeGroup(): CGroup | null {
  return groups.find((g) => g.id === activeGid) || null;
}
function activeChannel(): CChannel | null {
  const g = activeGroup();
  if (!g) return null;
  if (!activeCid || !g.channels.some((c) => c.id === activeCid)) {
    activeCid = g.channels[0]?.id || "";
  }
  return g.channels.find((c) => c.id === activeCid) || null;
}
function activePair(): Pair | null {
  const ch = activeChannel();
  if (!ch) return null;
  return pairOf(ch);
}

function allPairs(): Pair[] {
  const out: Pair[] = [];
  for (const g of groups) {
    for (const ch of g.channels) {
      const on = enabledSources(ch);
      if (on.length < 2) continue;
      if (ch.mode === "pairwise") {
        for (let i = 0; i < on.length; i++) {
          for (let j = i + 1; j < on.length; j++) out.push({ g, ch, ref: on[i], tgt: on[j] });
        }
      } else {
        const ref = baseOf(ch)!;
        for (const s of on) if (s.id !== ref.id) out.push({ g, ch, ref, tgt: s });
      }
    }
  }
  return out;
}

function setRunInfo(text: string, kind: "" | "run" | "err" = "") {
  const el = $("cg-runinfo");
  if (el) {
    el.textContent = text;
    el.className = `cg-runinfo${kind ? ` ${kind}` : ""}`;
  }
  const lamp = document.getElementById("lamp-consist");
  if (lamp) lamp.className = `lamp${kind === "run" ? " run" : kind === "err" ? " err" : ""}`;
}
function setRunning(on: boolean) {
  running = on;
  runningAll = on;
  const all = $<HTMLButtonElement>("cg-all");
  if (all) all.disabled = on;
  const stop = $<HTMLButtonElement>("cg-stop");
  if (stop) stop.hidden = !on;
  const rp = $<HTMLButtonElement>("cg-runpair");
  if (rp) rp.disabled = on || !activePair();
  const rg = $<HTMLButtonElement>("cg-rungroup");
  if (rg) {
    const picked = visiblePairs().filter((p) => selectedPairs.has(resultKey(p))).length;
    rg.disabled = on || picked === 0;
  }
}

// ═════════════ 分组渲染 ═════════════

function markGroupsRunning(list: Pair[]) {
  const gids = new Set(list.map((p) => p.g.id));
  const cids = new Set(list.map((p) => chKey(p.g, p.ch)));
  for (const g of groups) {
    if (gids.has(g.id)) groupStates.set(g.id, { text: "检测中", cls: "idle", summary: "等待结果" });
    for (const ch of g.channels) {
      if (cids.has(chKey(g, ch))) channelStates.set(chKey(g, ch), { text: "检测中", cls: "idle", summary: "等待结果" });
    }
  }
}

function groupState(g: CGroup): GroupState {
  return groupStates.get(g.id) || { text: "待检", cls: "idle", summary: "—" };
}
function channelState(g: CGroup, ch: CChannel): GroupState {
  return channelStates.get(chKey(g, ch)) || { text: "待检", cls: "idle", summary: "—" };
}
function pairState(p: Pair): ResultRow | null {
  return results.get(resultKey(p)) || null;
}

/** 由一组结果得出状态；没有任何结果时返回 null，保留“待检/检测中”。 */
function stateFromRows(rows: ResultRow[]): GroupState | null {
  if (!rows.length) return null;
  const failed = rows.find((x) => !x.r.ok);
  if (failed) return { text: "失败", cls: "bad", summary: (failed.r.err || "检测失败").slice(0, 18) };
  const badRows = rows.filter((x) => x.r.bad_count > 0);
  if (badRows.length) {
    const segs = badRows.reduce((n, x) => n + x.r.bad_count, 0);
    const min = Math.min(...badRows.map((x) => x.r.mean_sim));
    return { text: "差异", cls: "warn", summary: `最差 ${(min * 100).toFixed(1)}% · ${segs} 段` };
  }
  const driftRows = rows.filter((x) => Math.abs(x.r.best_offset_ms) >= 200);
  if (driftRows.length) {
    const max = Math.max(...driftRows.map((x) => Math.abs(x.r.best_offset_ms)));
    return { text: "不同步", cls: "warn", summary: `最大偏移 ${max}ms` };
  }
  const min = Math.min(...rows.map((x) => x.r.mean_sim));
  return { text: "通过", cls: "ok", summary: `${(min * 100).toFixed(1)}% · 对齐` };
}

/** 频道状态按本频道最差的一对聚合。 */
function refreshChannelState(g: CGroup, ch: CChannel) {
  const err = channelErrors.get(chKey(g, ch));
  if (err) {
    channelStates.set(chKey(g, ch), { text: "失败", cls: "bad", summary: err.slice(0, 18) });
    refreshGroupState(g);
    return;
  }
  const rows = channelPairs(ch, g)
    .map((p) => pairState(p))
    .filter((x): x is ResultRow => !!x);
  const st = stateFromRows(rows);
  if (st) channelStates.set(chKey(g, ch), st);
  refreshGroupState(g);
}

/** 分组状态按组内所有频道、所有对的最差结果聚合。 */
function refreshGroupState(g: CGroup, clearIfEmpty = false) {
  for (const ch of g.channels) {
    const err = channelErrors.get(chKey(g, ch));
    if (err) {
      groupStates.set(g.id, { text: "失败", cls: "bad", summary: `${ch.name}: ${err}`.slice(0, 20) });
      return;
    }
  }
  const rows = groupPairs(g)
    .map((p) => pairState(p))
    .filter((x): x is ResultRow => !!x);
  if (!rows.length) {
    if (clearIfEmpty) groupStates.delete(g.id);
    return;
  }
  const st = stateFromRows(rows);
  if (st) groupStates.set(g.id, st);
}

function clearChannelResults(g: CGroup, ch: CChannel) {
  channelErrors.delete(chKey(g, ch));
  channelStates.delete(chKey(g, ch));
  // 按频道前缀清理，覆盖“来源被改名/删除后旧结果仍残留”的情况
  const prefix = `${chKey(g, ch)}:`;
  for (const k of [...results.keys()]) if (k.startsWith(prefix)) results.delete(k);
  refreshGroupState(g, true);
}

function clearGroupResults(g: CGroup) {
  groupStates.delete(g.id);
  for (const ch of g.channels) {
    channelErrors.delete(chKey(g, ch));
    channelStates.delete(chKey(g, ch));
  }
  const prefix = `${g.id}:`;
  for (const k of [...results.keys()]) if (k.startsWith(prefix)) results.delete(k);
}

// ═════════════ 分组 / 比对对列表 ═════════════

function seedPairSelection() {
  if (selectionReady) return;
  selectedPairs.clear();
  for (const p of allPairs()) selectedPairs.add(resultKey(p));
  selectionReady = true;
}

function groupMatchesFilter(g: CGroup): boolean {
  return cgGroupFilter === "all" || cgGroupFilter === g.id;
}

function pairMatchesSearch(p: Pair): boolean {
  const q = cgQuery.trim().toLowerCase();
  if (!q) return true;
  return `${p.g.name} ${p.ch.name} ${p.ref.name} ${p.ref.url} ${p.tgt.name} ${p.tgt.url}`.toLowerCase().includes(q);
}

function unpairedMatchesSearch(g: CGroup, ch: CChannel): boolean {
  const q = cgQuery.trim().toLowerCase();
  if (!q) return true;
  const urls = enabledSources(ch).map((s) => `${s.name} ${s.url}`).join(" ");
  return `${g.name} ${ch.name} ${urls}`.toLowerCase().includes(q);
}

function visiblePairs(): Pair[] {
  return groups.filter(groupMatchesFilter).flatMap((g) => groupPairs(g).filter(pairMatchesSearch));
}

function pairStatus(p: Pair): GroupState {
  const rr = pairState(p);
  if (rr) {
    if (!rr.r.ok) return { text: "失败", cls: "bad", summary: (rr.r.err || "比对失败").slice(0, 24) };
    const sim = (rr.r.mean_sim * 100).toFixed(1);
    const off = rr.r.best_offset_ms ? `${rr.r.best_offset_ms > 0 ? "+" : ""}${rr.r.best_offset_ms}ms` : "对齐";
    if (rr.r.bad_count > 0) return { text: "差异", cls: "warn", summary: `${sim}% · ${rr.r.bad_count} 段` };
    if (Math.abs(rr.r.best_offset_ms) >= 200) return { text: "不同步", cls: "warn", summary: `${sim}% · ${off}` };
    return { text: "通过", cls: "ok", summary: `${sim}% · ${off}` };
  }
  const cs = channelState(p.g, p.ch);
  if (cs.text === "检测中") return cs;
  return { text: "待检", cls: "idle", summary: "—" };
}

function renderGroupFilter() {
  const sel = $<HTMLSelectElement>("cg-groupfilter");
  if (!sel) return;
  if (cgGroupFilter !== "all" && !groups.some((g) => g.id === cgGroupFilter)) cgGroupFilter = "all";
  sel.innerHTML =
    `<option value="all">全部（${groups.length} 组）</option>` +
    groups.map((g) => `<option value="${g.id}">${esc(g.name)}（${groupPairs(g).length} 对）</option>`).join("");
  sel.value = cgGroupFilter;
}

function syncSelectionUi() {
  const box = $<HTMLInputElement>("cg-allbox");
  if (!box) return;
  const list = visiblePairs();
  const picked = list.filter((p) => selectedPairs.has(resultKey(p))).length;
  box.checked = list.length > 0 && picked === list.length;
  box.indeterminate = picked > 0 && picked < list.length;
  const rg = $<HTMLButtonElement>("cg-rungroup");
  if (rg) {
    rg.hidden = allPairs().length === 0;
    rg.disabled = running || picked === 0;
  }
}

function pairRowHtml(p: Pair): string {
  const key = resultKey(p);
  const cp = activePair();
  const active = !!cp && resultKey(cp) === key;
  const checked = selectedPairs.has(key);
  const st = pairStatus(p);
  const siblings = channelPairs(p.ch, p.g);
  const index = siblings.findIndex((x) => resultKey(x) === key) + 1;
  return `<div class="cg-prow${active ? " sel" : ""}${checked ? "" : " off"}" data-g="${p.g.id}" data-c="${p.ch.id}" data-t="${p.tgt.id}" data-pkey="${key}">
    <label class="cg-pchk" title="勾选后参与轮询"><input type="checkbox" data-pick="${key}" ${checked ? "checked" : ""} /></label>
    <div class="cg-pmain">
      <div class="cg-ptop">
        <b class="cg-pname" title="${esc(p.ch.name)}">${esc(p.ch.name)}</b>
        ${siblings.length > 1 ? `<span class="cg-pidx">${index}/${siblings.length}</span>` : ""}
        <span class="cg-pstate ${st.cls}" title="${esc(st.summary)}">${esc(st.text)}</span>
        <button class="cg-pedit" data-cedit="${p.g.id}:${p.ch.id}" title="频道设置">⋯</button>
      </div>
      <div class="cg-pduo">
        <span class="cg-prole ref">基准</span><span class="cg-psname" title="${esc(p.ref.name)}">${esc(p.ref.name)}</span>
        <span class="cg-parrow">→</span>
        <span class="cg-prole tgt">待比</span><span class="cg-psname" title="${esc(p.tgt.name)}">${esc(p.tgt.name)}</span>
      </div>
      <div class="cg-pmeta">
        <span class="cg-purls"><span title="${esc(p.ref.url)}">${esc(p.ref.url || "未填地址")}</span><span title="${esc(p.tgt.url)}">${esc(p.tgt.url || "未填地址")}</span></span>
        <span class="cg-pres">${esc(st.summary)}</span>
      </div>
    </div>
  </div>`;
}

function unpairedRowHtml(g: CGroup, ch: CChannel): string {
  const on = enabledSources(ch);
  const base = baseOf(ch);
  return `<div class="cg-prow unpaired" data-g="${g.id}" data-c="${ch.id}">
    <span class="cg-pchk-off" title="至少需要 2 路启用来源才能配对"></span>
    <div class="cg-pmain">
      <div class="cg-ptop">
        <b class="cg-pname" title="${esc(ch.name)}">${esc(ch.name)}</b>
        <span class="cg-pstate idle">未配对</span>
        <button class="cg-pedit" data-cedit="${g.id}:${ch.id}" title="频道设置">⋯</button>
      </div>
      <div class="cg-pduo">
        <span class="cg-prole ref">基准</span><span class="cg-psname" title="${esc(base?.name || "未配置")}">${esc(base?.name || "未配置")}</span>
        <span class="cg-parrow">→</span>
        <span class="cg-prole tgt">待比</span><span class="cg-psname muted">未配置</span>
      </div>
      <div class="cg-pmeta">
        <span class="cg-purls">${base ? `<span title="${esc(base.url)}">${esc(base.url || "未填地址")}</span>` : `<span>至少需要 2 路启用来源</span>`}</span>
        <span class="cg-pres">${on.length} 路启用</span>
      </div>
    </div>
  </div>`;
}

function renderGroups() {
  const host = $("cg-list");
  if (!host) return;
  renderGroupFilter();
  const all = allPairs();
  const cnt = $("cg-count");
  if (cnt) {
    const matched = visiblePairs().length;
    cnt.textContent = cgQuery.trim() ? `${groups.length} 组 / ${all.length} 对 · 匹配 ${matched}` : `${groups.length} 组 / ${all.length} 对`;
  }
  if (!groups.length) {
    host.innerHTML = `<div class="cg-seg-empty" style="padding:22px 14px;line-height:1.7">还没有分组。点右上角「＋ 新建分组」，再点分组里的「＋ 新建频道」添加频道。</div>`;
    const rp = $<HTMLButtonElement>("cg-runpair");
    if (rp) rp.disabled = true;
    const rg = $<HTMLButtonElement>("cg-rungroup");
    if (rg) rg.hidden = true;
    renderRail();
    syncSelectionUi();
    stopPreview();
    renderPair(null);
    return;
    }
  if (!activeGroup()) activeGid = groups[0].id;
  activeChannel();

  const searching = cgQuery.trim().length > 0;
  host.innerHTML = groups
    .filter(groupMatchesFilter)
    .map((g) => {
      const pairs = groupPairs(g).filter(pairMatchesSearch);
      const unpaired = g.channels.filter((ch) => enabledSources(ch).length < 2 && unpairedMatchesSearch(g, ch));
      if (searching && !pairs.length && !unpaired.length) return "";
      const active = g.id === activeGid;
      const open = !closedGroups.has(g.id) || searching;
      const st = groupState(g);
      const rows =
        pairs.map(pairRowHtml).join("") +
        unpaired.map((ch) => unpairedRowHtml(g, ch)).join("");
      return `<div class="cg-group${active ? " active" : ""}" data-gid="${g.id}">
        <div class="cg-ghead" data-g="${g.id}">
          <span class="cg-caret">${open ? "▾" : "▸"}</span>
          <div class="cg-gname"><b>${esc(g.name)}</b><span class="cg-pair">${g.channels.length} 频道 · ${groupPairs(g).length} 对</span></div>
          <span class="cg-gst ${st.cls}">${esc(st.text)}</span>
          <button class="cg-gbtn" data-gedit="${g.id}" title="分组设置">⋯</button>
        </div>
        ${open ? `<div class="cg-plist">${rows || `<div class="cg-ch-empty">这个分组下还没有可比对的频道；至少需要 2 路启用来源。</div>`}<button class="cg-addch" data-addch="${g.id}">＋ 新建频道</button></div>` : ""}
      </div>`;
    })
    .join("");
  if (!host.innerHTML.trim()) {
    host.innerHTML = `<div class="cg-seg-empty" style="padding:22px 14px;line-height:1.7">没有匹配的频道。换个关键词，或把分组筛选切回「全部」。</div>`;
  }
  bindGroupClicks(host);
  const cp = activePair();
  if (!cp) stopPreview();
  renderPair(cp);
  renderRail();
  syncSelectionUi();
  const rp = $<HTMLButtonElement>("cg-runpair");
  if (rp) rp.disabled = running || !cp;
  const rg = $<HTMLButtonElement>("cg-rungroup");
  if (rg) {
    const picked = visiblePairs().filter((p) => selectedPairs.has(resultKey(p))).length;
    rg.hidden = all.length === 0;
    rg.disabled = running || picked === 0;
  }
}

function bindGroupClicks(host: HTMLElement) {
  host.onclick = (e) => {
    const t = e.target as HTMLElement;
    const gedit = t.closest<HTMLElement>("[data-gedit]");
    if (gedit) {
      e.stopPropagation();
      openGroupMeta(gedit.dataset.gedit!);
      return;
    }
    const cedit = t.closest<HTMLElement>("[data-cedit]");
    if (cedit) {
      e.stopPropagation();
      const [gid, cid] = (cedit.dataset.cedit || "").split(":");
      openChannel(gid, cid);
      return;
    }
    const addch = t.closest<HTMLElement>("[data-addch]");
    if (addch) {
      e.stopPropagation();
      openChannel(addch.dataset.addch!, "");
      return;
    }
    if (t.closest(".cg-pchk") || t.closest(".cg-pchk-off")) return;
    const unpaired = t.closest<HTMLElement>(".cg-prow.unpaired");
    if (unpaired) {
      openChannel(unpaired.dataset.g!, unpaired.dataset.c!);
      return;
    }
    const row = t.closest<HTMLElement>(".cg-prow");
    if (row) {
      activeGid = row.dataset.g!;
      activeCid = row.dataset.c!;
      activeTgt = row.dataset.t!;
      closedGroups.delete(activeGid);
      renderGroups();
      startPreview(activePair());
      return;
    }
    const head = t.closest<HTMLElement>(".cg-ghead");
    if (head) {
      const id = head.dataset.g!;
      if (activeGid !== id) {
        activeGid = id;
        const g = activeGroup();
        activeCid = g?.channels[0]?.id || "";
        activeTgt = "";
        closedGroups.delete(id);
      } else if (closedGroups.has(id)) closedGroups.delete(id);
      else closedGroups.add(id);
      renderGroups();
      startPreview(activePair());
    }
  };
  host.onchange = (e) => {
    const box = (e.target as HTMLElement).closest<HTMLInputElement>("input[data-pick]");
    if (!box) return;
    const key = box.dataset.pick || "";
    if (!key) return;
    if (box.checked) selectedPairs.add(key);
    else selectedPairs.delete(key);
    box.closest<HTMLElement>(".cg-prow")?.classList.toggle("off", !box.checked);
    syncSelectionUi();
  };
}

function renderRail() {
  const rail = $("cg-rail");
  if (!rail) return;
  rail.innerHTML = groups
    .slice(0, 7)
    .map((g) => {
      const st = groupState(g);
      return `<button class="cg-rgroup${g.id === activeGid ? " on" : ""}" data-r="${g.id}" title="${esc(g.name)}"><span>${esc((g.name || "组").slice(0, 1))}</span><i class="${st.cls === "ok" ? "ok" : st.cls === "bad" ? "bad" : ""}"></i></button>`;
    })
    .join("");
  rail.onclick = (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>("[data-r]");
    if (b) {
      activeGid = b.dataset.r!;
      activeTgt = "";
      closedGroups.delete(activeGid);
      renderGroups();
      startPreview(activePair());
    }
  };
}

// ═════════════ 双画面预览 ═════════════

function startPreview(p: Pair | null) {
  if (!p) {
    stopPreview();
    return;
  }
  const rf = $<HTMLImageElement>("cg-refprev");
  const tf = $<HTMLImageElement>("cg-tgtprev");
  if (rf) rf.style.opacity = "0.35";
  if (tf) tf.style.opacity = "0.35";
  if (p.ref.url !== lastRefUrl) {
    lastRefUrl = p.ref.url;
    prevRefKey = "";
    if (rf) rf.removeAttribute("src");
    api.previewStart("consist-ref", p.ref.url).catch(() => {});
  }
  if (p.tgt.url !== lastTgtUrl) {
    lastTgtUrl = p.tgt.url;
    prevTgtKey = "";
    if (tf) tf.removeAttribute("src");
    api.previewStart("consist-tgt", p.tgt.url).catch(() => {});
  }
  if (prevTimer) return;
  prevTimer = window.setInterval(async () => {
    if (document.querySelector<HTMLElement>('.pg[data-page="consist"]')?.hidden) return;
    if (document.getElementById("cg-view-compare")?.hidden) return;
    try {
      const [a, b] = await Promise.all([api.previewFrame("consist-ref"), api.previewFrame("consist-tgt")]);
      const rf = $<HTMLImageElement>("cg-refprev");
      const tf = $<HTMLImageElement>("cg-tgtprev");
      if (a && a !== prevRefKey && rf) {
        rf.src = a;
        rf.style.opacity = "1";
        prevRefKey = a;
      }
      if (b && b !== prevTgtKey && tf) {
        tf.src = b;
        tf.style.opacity = "1";
        prevTgtKey = b;
      }
    } catch {
      /* 预览抓帧失败不影响检测 */
    }
  }, 400);
}

function stopPreview() {
  if (prevTimer) {
    window.clearInterval(prevTimer);
    prevTimer = 0;
  }
  lastRefUrl = "";
  lastTgtUrl = "";
  prevRefKey = "";
  prevTgtKey = "";
  api.previewStop("consist-ref").catch(() => {});
  api.previewStop("consist-tgt").catch(() => {});
}

// ═════════════ 右侧画面 / KPI / 曲线 ═════════════

function renderPair(p: Pair | null) {
  const st = $("cg-mstatus");
  if (!p) {
    const nm = $("cg-pairname");
    const mt = $("cg-pairmeta");
    if (nm) nm.textContent = "未选择频道";
    if (mt) mt.textContent = "选择左侧分组下的频道开始";
    if (st) {
      st.textContent = "待机";
      st.className = "cg-mstatus";
    }
    renderKpi(null);
    drawCurve(null);
    renderSegs(null);
    return;
  }
  const rr = pairState(p);
  const nm = $("cg-pairname");
  const mt = $("cg-pairmeta");
  const rn = $("cg-refname");
  const tn = $("cg-tgtname");
  const rc = $("cg-refcap");
  const tc = $("cg-tgtcap");
  const rf = $("cg-refres");
  const tf = $("cg-tgtres");
  if (nm) nm.textContent = `${p.g.name} / ${p.ch.name} · ${pairLabel(p)}`;
  if (mt) mt.textContent = "频道内比对 · 左基准右待比 · 点来源行切换当前对";
  if (rn) rn.textContent = p.ref.name || "基准";
  if (tn) tn.textContent = p.tgt.name || "待比";
  if (rc) rc.textContent = `基准源：${p.ref.url}`;
  if (tc) tc.textContent = `待比源：${p.tgt.url}`;
  if (rf) rf.textContent = rr ? `${(rr.r.mean_sim * 100).toFixed(1)}%` : "—";
  if (tf) tf.textContent = rr ? `${rr.r.best_offset_ms > 0 ? "+" : ""}${rr.r.best_offset_ms} ms` : "—";
  if (st) {
    if (rr) {
      if (!rr.r.ok) {
        st.textContent = "检测失败";
        st.className = "cg-mstatus bad";
      } else {
        const drift = Math.abs(rr.r.best_offset_ms) >= 200;
        const ok = rr.r.bad_count === 0 && !drift;
        st.textContent = ok
          ? "一致"
          : rr.r.bad_count
            ? `差异 ${rr.r.bad_count} 段`
            : `不同步 ${rr.r.best_offset_ms > 0 ? "+" : ""}${rr.r.best_offset_ms}ms`;
        st.className = `cg-mstatus ${ok ? "ok" : rr.r.bad_count ? "bad" : "warn"}`;
      }
    } else {
      st.textContent = "待检";
      st.className = "cg-mstatus";
    }
  }
  renderKpi(rr?.r || null);
  drawCurve(rr?.r || null);
  renderSegs(rr?.r || null);
}

function kpi(k: string, v: string, n: string, cls = "") {
  return `<div class="cg-kpi"><div class="k">${k}</div><div class="v">${v}</div><div class="n ${cls}">${n}</div></div>`;
}

function renderKpi(r: ConsistResult | null) {
  const el = $("cg-kpi");
  if (!el) return;
  if (!r) {
    el.innerHTML =
      kpi("最佳偏移", "—", "待比对") +
      kpi("平均相似度", "—", "阈值 " + cfg.thr.toFixed(2)) +
      kpi("最低相似度", "—", "—") +
      kpi("差异段", "—", "低于阈值") +
      kpi("有效帧数", "—", "参考 / 待比");
    return;
  }
  if (!r.ok) {
    el.innerHTML =
      kpi("最佳偏移", "—", "检测失败", "b") +
      kpi("平均相似度", "—", "无有效结果", "b") +
      kpi("最低相似度", "—", "—") +
      kpi("差异段", "—", "—") +
      kpi("有效帧数", "—", "参考 / 待比");
    return;
  }
  const ok = r.mean_sim >= cfg.thr && r.bad_count === 0;
  el.innerHTML =
    kpi(
      "最佳偏移",
      `${r.best_offset_ms > 0 ? "+" : ""}${r.best_offset_ms}<small>ms</small>`,
      r.best_offset_ms > 0 ? "待比内容更晚" : r.best_offset_ms < 0 ? "待比内容更早" : "起点对齐",
      r.best_offset_ms > 0 ? "w" : "",
    ) +
    kpi(
      "平均相似度",
      `${(r.mean_sim * 100).toFixed(1)}<small>%</small>`,
      ok ? "内容一致" : "低于阈值",
      ok ? "g" : "b",
    ) +
    kpi("最低相似度", `${(r.min_sim * 100).toFixed(1)}<small>%</small>`, `阈值 ${cfg.thr.toFixed(2)}`, r.min_sim < cfg.thr ? "b" : "") +
    kpi("差异段", `${r.bad_count}<small>段</small>`, r.bad_count ? "连续 ≥2 帧" : "未发现", r.bad_count ? "w" : "g") +
    kpi("有效帧数", `${Math.min(r.frames_ref, r.frames_tgt)}<small>帧</small>`, `参考 ${r.frames_ref} / 待比 ${r.frames_tgt}`);
}

function drawCurve(r: ConsistResult | null) {
  const cv = $<HTMLCanvasElement>("cg-curve");
  if (!cv) return;
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth || 700;
  const h = cv.clientHeight || 160;
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
  }
  const ctx = cv.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = "rgba(255,255,255,.02)";
  ctx.fillRect(0, 0, w, h);
  const GUT = 42;
  const PR = 10;
  const TOP = 10;
  const BOT = h - 18;
  const pw = w - GUT - PR;
  const ph = BOT - TOP;
  ctx.strokeStyle = "rgba(255,255,255,.07)";
  ctx.font = "9px Consolas, monospace";
  ctx.fillStyle = "#CBBDA4";
  ctx.textAlign = "right";
  for (const t of [1, 0.75, 0.5, 0.25, 0]) {
    const y = TOP + (1 - t) * ph;
    ctx.beginPath();
    ctx.moveTo(GUT, y);
    ctx.lineTo(w - PR, y);
    ctx.stroke();
    ctx.fillText(`${Math.round(t * 100)}%`, GUT - 4, y + 3);
  }
  ctx.textAlign = "left";
  const empty = document.getElementById("cg-empty") as HTMLElement | null;
  if (!r || r.series_sim.length < 4) {
    if (empty) {
      empty.hidden = false;
      empty.textContent = r && !r.ok ? "比对失败，无逐帧曲线" : "比对后这里显示逐帧相似度曲线";
    }
    return;
  }
  if (empty) empty.hidden = true;
  const arr = r.series_sim;
  const totalS = arr.length / (r.fps || 1);
  ctx.textAlign = "center";
  for (let i = 0; i <= 4; i++) {
    const t = (totalS * i) / 4;
    ctx.fillText(`${t.toFixed(1)}s`, GUT + (pw * i) / 4, h - 5);
  }
  ctx.textAlign = "left";
  ctx.strokeStyle = "rgba(168,212,189,.95)";
  ctx.lineWidth = 1.4;
  ctx.beginPath();
  for (let i = 0; i < arr.length; i++) {
    const x = GUT + (pw * i) / Math.max(1, arr.length - 1);
    const y = BOT - arr[i] * ph;
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.stroke();
  const ty = BOT - cfg.thr * ph;
  ctx.strokeStyle = "rgba(248,113,113,.6)";
  ctx.setLineDash([4, 3]);
  ctx.beginPath();
  ctx.moveTo(GUT, ty);
  ctx.lineTo(w - PR, ty);
  ctx.stroke();
  ctx.setLineDash([]);
}

function fmtT(t: number): string {
  const m = Math.floor(t / 60);
  const s = t - m * 60;
  return m > 0 ? `${m}:${s.toFixed(1).padStart(4, "0")}` : `${s.toFixed(1)}s`;
}
function renderSegs(r: ConsistResult | null) {
  const el = $("cg-segs");
  if (!el) return;
  const sub = $("cg-segsub");
  if (!r) {
    el.innerHTML = `<div class="cg-seg-empty">还没有比对结果。</div>`;
    if (sub) sub.textContent = "低于阈值且连续 ≥ 2 帧";
    return;
  }
  if (!r.ok) {
    el.innerHTML = `<div class="cg-seg-empty" style="color:var(--bad)">${esc((r.err || "比对失败").slice(0, 120))}</div>`;
    if (sub) sub.textContent = "检测失败";
    return;
  }
  if (!r.bad_segments.length) {
    el.innerHTML = `<div class="cg-seg-empty" style="color:var(--ok)">整段未发现差异段，平均相似度 ${(r.mean_sim * 100).toFixed(1)}%。</div>`;
    if (sub) sub.textContent = "未发现差异";
    return;
  }
  if (sub) sub.textContent = `${r.bad_count} 段 · 最差 ${(r.min_sim * 100).toFixed(1)}%`;
  el.innerHTML = r.bad_segments
    .map((s) => {
      const d = Math.max(0, s.to_s - s.from_s);
      return `<div class="cg-seg-row"><span class="t">${fmtT(s.from_s)} – ${fmtT(s.to_s)}</span><span class="why">画面/内容不一致</span><span class="st">连续 ${Math.max(2, Math.round(d * (r.fps || 1)))} 帧</span><span class="st" style="color:var(--warn)">${(s.sim * 100).toFixed(1)}%</span></div>`;
    })
    .join("");
}

// ═════════════ 比对任务 ═════════════

async function runPair(p: Pair, job: string, focus: boolean): Promise<ConsistResult | null> {
  const st = (kind: "run" | "err", text: string) => {
    if (focus) setRunInfo(text, kind);
  };
  if (focus) {
    renderKpi(null);
    drawCurve(null);
    renderSegs(null);
    const ms = $("cg-mstatus");
    if (ms) {
      ms.textContent = "比对中";
      ms.className = "cg-mstatus run";
    }
    startPreview(p);
  }
  try {
    await api.consistStart(
      {
        reference: p.ref.url,
        target: p.tgt.url,
        fps: cfg.fps,
        secs: cfg.secs,
        max_lag_s: cfg.lag,
        thr: cfg.thr,
        logo: logoBox(),
      },
      job,
    );
  } catch (e: any) {
    if (focus) {
      setRunInfo(String(e), "err");
      const ms = $("cg-mstatus");
      if (ms) {
        ms.textContent = "启动失败";
        ms.className = "cg-mstatus bad";
      }
    }
    return null;
  }
  let guard = 0;
  let idle = 0;
  for (;;) {
    await sleep(650);
    let poll;
    try {
      poll = await api.consistProgress(job);
    } catch {
      if (stopRequested) return null;
      continue;
    }
    if (poll.running) {
      idle = 0;
      if (focus) st("run", `比对中 · ${poll.pct}%`);
      if (++guard > 900) {
        if (focus) st("err", "等待结果超时");
        return null;
      }
      continue;
    }
    idle += 1;
    if (poll.result) {
      const r = poll.result;
      results.set(resultKey(p), { r, at: new Date().toLocaleTimeString() });
      const badNow = r.bad_count > 0 || Math.abs(r.best_offset_ms) >= 200;
      // 异常由 alarm_record 一并写日志（含 alarm_id），这里只补通过项的检测日志
      if (!badNow) {
        api.alarmDetectLog({
          channel: p.ch.name,
          pair: `${p.g.name} · ${pairLabel(p)}`,
          similarity: r.mean_sim,
          minSim: r.min_sim,
          offsetMs: r.best_offset_ms,
          segmentCount: r.bad_count,
          verdict: r.verdict,
          source: p.tgt.url,
          reference: p.ref.url,
          fps: r.fps,
          bad: false,
        }).catch(() => {});
      }
      refreshChannelState(p.g, p.ch);
      if (focus) {
        const driftNow = Math.abs(r.best_offset_ms) >= 200;
        setRunInfo(
          r.ok ? (r.bad_count ? "发现差异" : driftNow ? "发现不同步" : "比对完成") : r.err || "比对失败",
          r.ok ? "" : "err",
        );
        renderPair(p);
        renderGroups();
      } else {
        renderGroups();
      }
      maybeAlarm(p, r).catch(() => {});
      if (r.bad_count > 0) {
        window.dispatchEvent(new CustomEvent("alarms-changed"));
      }
      return r;
    }
    if (poll.err) {
      channelErrors.set(chKey(p.g, p.ch), poll.err);
      refreshChannelState(p.g, p.ch);
      if (focus) {
        setRunInfo(poll.err, "err");
        const ms = $("cg-mstatus");
        if (ms) {
          ms.textContent = "失败";
          ms.className = "cg-mstatus bad";
        }
        renderGroups();
      } else renderGroups();
      return null;
    }
    if (stopRequested) return null;
    if (idle >= 6) {
      if (focus) st("err", "任务已结束但没有返回结果");
      return null;
    }
  }
}

async function runCurrent() {
  const p = activePair();
  if (!p || running) return;
  stopRequested = false;
  clearChannelResults(p.g, p.ch);
  markGroupsRunning([p]);
  renderGroups();
  setRunning(true);
  setRunInfo("启动中…", "run");
  const r = await runPair(p, "main", true);
  if (!r && !stopRequested) setRunInfo("失败", "err");
  if (stopRequested) {
    channelStates.delete(chKey(p.g, p.ch));
    channelErrors.delete(chKey(p.g, p.ch));
    groupStates.delete(p.g.id);
    refreshGroupState(p.g);
    renderGroups();
  }
  setRunning(false);
  stopRequested = false;
}

async function runGroup() {
  const g = activeGroup();
  if (!g || running) return;
  const pairs = groupPairs(g);
  if (!pairs.length) return;
  stopRequested = false;
  clearGroupResults(g);
  markGroupsRunning(pairs);
  renderGroups();
  setRunning(true);
  const conc = Math.max(1, Math.min(8, Math.round(cfg.conc) || 1, pairs.length));
  setRunInfo(`本组 ${pairs.length} 对 · 并发 ${conc}`, "run");
  let cursor = 0;
  const workers = Array.from({ length: conc }, async (_, i) => {
    while (!stopRequested) {
      const idx = cursor++;
      if (idx >= pairs.length) return;
      await runPair(pairs[idx], `group-${i}`, false);
    }
  });
  await Promise.all(workers);
  if (stopRequested) {
    for (const p of pairs) {
      channelStates.delete(chKey(p.g, p.ch));
      channelErrors.delete(chKey(p.g, p.ch));
    }
    for (const g of groups) {
      if (groupStates.get(g.id)?.text === "检测中") groupStates.delete(g.id);
      refreshGroupState(g);
    }
  }
  setRunInfo(stopRequested ? "已停止" : "本组完成");
  setRunning(false);
  stopRequested = false;
  renderGroups();
}

async function runSelected() {
  if (running) return;
  const pairs = allPairs().filter((p) => selectedPairs.has(resultKey(p)));
  if (!pairs.length) {
    setRunInfo("先在左侧勾选要比对的频道", "err");
    return;
  }
  stopRequested = false;
  const cleared = new Set<string>();
  for (const p of pairs) {
    if (!cleared.has(p.g.id)) {
      clearGroupResults(p.g);
      cleared.add(p.g.id);
    }
  }
  markGroupsRunning(pairs);
  renderGroups();
  setRunning(true);
  const conc = Math.max(1, Math.min(8, Math.round(cfg.conc) || 1, pairs.length));
  setRunInfo(`选中 ${pairs.length} 对 · 并发 ${conc}`, "run");
  let cursor = 0;
  const workers = Array.from({ length: conc }, async (_, i) => {
    while (!stopRequested) {
      const idx = cursor++;
      if (idx >= pairs.length) return;
      await runPair(pairs[idx], `sel-${i}`, false);
    }
  });
  await Promise.all(workers);
  if (stopRequested) {
    for (const p of pairs) {
      channelStates.delete(chKey(p.g, p.ch));
      channelErrors.delete(chKey(p.g, p.ch));
    }
    for (const g of groups) {
      if (groupStates.get(g.id)?.text === "检测中") groupStates.delete(g.id);
      refreshGroupState(g);
    }
    setRunInfo("已停止");
  } else {
    const bad = [...groupStates.values()].filter((s) => s.cls === "warn" || s.cls === "bad").length;
    setRunInfo(bad ? `选中完成 · ${pairs.length} 对 · ${bad} 组异常` : `选中完成 · ${pairs.length} 对 · 未发现异常`, bad ? "err" : "");
  }
  setRunning(false);
  stopRequested = false;
  renderGroups();
}

async function runAll() {
  if (running) return;
  const pairs = allPairs();
  if (!pairs.length) {
    setRunInfo("没有可轮询的分组（至少 2 个启用来源）", "err");
    return;
  }
  stopRequested = false;
  for (const g of groups) clearGroupResults(g);
  markGroupsRunning(pairs);
  renderGroups();
  setRunning(true);
  const conc = Math.max(1, Math.min(8, Math.round(cfg.conc) || 1, pairs.length));
  setRunInfo(`全部轮询 · ${pairs.length} 对 · 并发 ${conc}`, "run");
  let cursor = 0;
  const workers = Array.from({ length: conc }, async (_, i) => {
    while (!stopRequested) {
      const idx = cursor++;
      if (idx >= pairs.length) return;
      await runPair(pairs[idx], `all-${i}`, false);
    }
  });
  await Promise.all(workers);
  if (stopRequested) setRunInfo("已停止");
  else {
    const bad = [...groupStates.values()].filter((s) => s.cls === "warn" || s.cls === "bad").length;
    setRunInfo(bad ? `完成 · ${bad} 组异常` : "全部完成 · 未发现异常", bad ? "err" : "");
  }
  if (stopRequested) {
    for (const p of pairs) {
      channelStates.delete(chKey(p.g, p.ch));
      channelErrors.delete(chKey(p.g, p.ch));
    }
    for (const g of groups) {
      if (groupStates.get(g.id)?.text === "检测中") groupStates.delete(g.id);
      refreshGroupState(g);
    }
  }
  setRunning(false);
  stopRequested = false;
  renderGroups();
}

async function stopAll() {
  stopRequested = true;
  setRunInfo("正在停止…");
  try {
    await api.consistStop("all");
  } catch {}
  window.setTimeout(() => {
    if (running) {
      setRunning(false);
      setRunInfo("已停止");
      stopRequested = false;
    }
  }, 800);
}

// ═════════════ 告警落库 / 证据录像 ═════════════

async function maybeAlarm(p: Pair, r: ConsistResult) {
  if (!r.ok) return;
  const bad = r.bad_count > 0 || Math.abs(r.best_offset_ms) >= 200;
  if (!bad) return;
  const kind = r.bad_count > 0 ? "不一致" : "不同步";
  const drift = Math.abs(r.best_offset_ms) >= 200;
  const detail = r.bad_segments.length
    ? `${kind}：首段 ${fmtT(r.bad_segments[0].from_s)}–${fmtT(r.bad_segments[0].to_s)}；${r.verdict}`
    : drift
      ? `不同步：偏移 ${r.best_offset_ms > 0 ? "+" : ""}${r.best_offset_ms}ms，超过 200ms 判定线`
      : `${kind}：${r.verdict}`;
  let rec: AlarmRecord;
  try {
    rec = await api.alarmRecord({
      channel: p.ch.name,
      pair: `${p.g.name} · ${pairLabel(p)}`,
      group_id: p.g.id,
      kind,
      similarity: r.mean_sim,
      min_sim: r.min_sim,
      offset_ms: r.best_offset_ms,
      segment_count: r.bad_count,
      source: p.tgt.url,
      reference: p.ref.url,
      detail,
      fps: r.fps,
      series_sim: r.series_sim,
      bad_segments: r.bad_segments,
    });
  } catch {
    return;
  }
  if (rec.clip_state === "ready" || rec.clip_state === "recording") return;
  const isFile = !/^(udp|rtp|http|https|rtsp):/i.test(p.tgt.url);
  const start = isFile && r.bad_segments.length ? Math.max(0, r.bad_segments[0].from_s - 5) : null;
  api.alarmCapture(rec.id, p.tgt.url, 12, start).catch(() => {});
}

// ═════════════ 参数/分组弹窗 ═════════════

function logoBox(): [number, number, number, number] | null {
  const n = (v: string) => {
    const t = v.trim();
    if (!t) return null;
    const x = Number(t);
    return Number.isFinite(x) ? x / 100 : null;
  };
  const x = n(cfg.lx);
  const y = n(cfg.ly);
  const w = n(cfg.lw);
  const h = n(cfg.lh);
  if (x === null && y === null && w === null && h === null) return null;
  return [x ?? 0.02, y ?? 0.02, w ?? 0.2, h ?? 0.12];
}

function syncChips() {
  const set = (id: string, text: string) => {
    const el = $(id);
    if (el) el.textContent = text;
  };
  set("cg-chip-fps", `${cfg.fps} fps`);
  set("cg-chip-secs", `${cfg.secs} s`);
  set("cg-chip-lag", `±${cfg.lag} s`);
  set("cg-chip-thr", cfg.thr.toFixed(2));
  set("cg-chip-logo", logoBox() ? "区域" : "整帧");
  set("cg-chip-conc", String(cfg.conc));
}

function openParam() {
  const setV = (id: string, v: string) => {
    const el = $<HTMLInputElement>(id);
    if (el) el.value = v;
  };
  setV("cg-fps", String(cfg.fps));
  setV("cg-secs", String(cfg.secs));
  setV("cg-lag", String(cfg.lag));
  setV("cg-thr", String(cfg.thr));
  setV("cg-lx", cfg.lx);
  setV("cg-ly", cfg.ly);
  setV("cg-lw", cfg.lw);
  setV("cg-lh", cfg.lh);
  setV("cg-conc", String(cfg.conc));
  $("cg-mask-param")?.classList.add("on");
}
function closeParam() {
  $("cg-mask-param")?.classList.remove("on");
}
function saveParam() {
  const num = (id: string, def: number) => {
    const v = Number(($<HTMLInputElement>(id)?.value || "").trim());
    return Number.isFinite(v) ? v : def;
  };
  const str = (id: string) => ($<HTMLInputElement>(id)?.value || "").trim();
  cfg = {
    fps: Math.min(60, Math.max(0.5, num("cg-fps", 10))),
    secs: Math.min(3600, Math.max(2, num("cg-secs", 20))),
    lag: Math.min(30, Math.max(0.2, num("cg-lag", 3))),
    thr: Math.min(1, Math.max(0, num("cg-thr", 0.75))),
    lx: str("cg-lx"),
    ly: str("cg-ly"),
    lw: str("cg-lw"),
    lh: str("cg-lh"),
    conc: Math.min(8, Math.max(1, Math.round(num("cg-conc", 2)))),
  };
  syncChips();
  savePrefs();
  closeParam();
  if (activePair()) {
    renderPair(activePair());
    startPreview(activePair());
  }
}
function savePrefs() {
  api.uiSet(P_KEY, cfg).catch(() => {});
}
async function loadPrefs() {
  try {
    const p: any = await api.uiGet(P_KEY);
    if (p && typeof p === "object") {
      const n = (v: any, d: number) => (Number.isFinite(Number(v)) ? Number(v) : d);
      cfg = {
        fps: n(p.fps, 10),
        secs: n(p.secs, 20),
        lag: n(p.lag, 3),
        thr: n(p.thr, 0.75),
        lx: typeof p.lx === "string" ? p.lx : "",
        ly: typeof p.ly === "string" ? p.ly : "",
        lw: typeof p.lw === "string" ? p.lw : "",
        lh: typeof p.lh === "string" ? p.lh : "",
        conc: Math.min(8, Math.max(1, Math.round(n(p.conc, 2)))),
      };
    }
  } catch {}
  syncChips();
}

function blankSource(base = false): CSource {
  return { id: uid(), name: base ? "主用源" : "备用源", url: "", base, enabled: true };
}

// ── 分组设置（只改分组名） ──

function openGroupMeta(gid?: string) {
  const found = gid ? groups.find((x) => x.id === gid) : null;
  if (gid && !found) return;
  editGid = gid || "";
  const gt = $("cg-gset-title");
  if (gt) gt.textContent = gid ? "分组设置" : "新建分组";
  const name = $<HTMLInputElement>("cg-gset-name");
  if (name) {
    name.value = found?.name || "新分组";
    window.setTimeout(() => name.select(), 0);
  }
  const del = $<HTMLButtonElement>("cg-gset-del");
  if (del) {
    del.hidden = !gid;
    del.textContent = "删除分组";
    del.classList.remove("is-arm");
  }
  groupDeleteArmed = false;
  $("cg-mask-gset")?.classList.add("on");
}
function closeGroupMeta() {
  groupDeleteArmed = false;
  $("cg-mask-gset")?.classList.remove("on");
}
function saveGroupMeta() {
  const name = $<HTMLInputElement>("cg-gset-name")?.value.trim() || "未命名分组";
  if (editGid) {
    const g = groups.find((x) => x.id === editGid);
    if (!g) return;
    g.name = name;
  } else {
    const g: CGroup = { id: uid(), name, channels: [] };
    groups.push(g);
    activeGid = g.id;
    activeCid = "";
    closedGroups.delete(g.id);
  }
  activeTgt = "";
  persistGroups();
  closeGroupMeta();
  renderGroups();
  startPreview(activePair());
}
function deleteGroupMeta() {
  if (!editGid) {
    closeGroupMeta();
    return;
  }
  const g = groups.find((x) => x.id === editGid);
  if (!g) {
    closeGroupMeta();
    return;
  }
  if (!groupDeleteArmed) {
    groupDeleteArmed = true;
    const del = $<HTMLButtonElement>("cg-gset-del");
    if (del) {
      del.textContent = g.channels.length ? `确认删除分组及 ${g.channels.length} 个频道` : "确认删除分组";
      del.classList.add("is-arm");
    }
    return;
  }
  clearGroupResults(g);
  groups = groups.filter((x) => x.id !== g.id);
  closedGroups.delete(g.id);
  if (activeGid === g.id) {
    activeGid = groups[0]?.id || "";
    activeCid = groups[0]?.channels[0]?.id || "";
    activeTgt = "";
    if (activeGid) closedGroups.delete(activeGid);
  }
  persistGroups();
  closeGroupMeta();
  renderGroups();
  startPreview(activePair());
}

// ── 频道设置（频道名、比对方式、来源） ──

function openChannel(gid: string, cid?: string) {
  const g = groups.find((x) => x.id === gid);
  if (!g) return;
  let ch: CChannel;
  if (cid) {
    const found = g.channels.find((x) => x.id === cid);
    if (!found) return;
    editCid = cid;
    ch = JSON.parse(JSON.stringify(found));
  } else {
    editCid = "";
    ch = { id: uid(), name: "新频道", mode: "base", sources: [blankSource(true), blankSource(false)] };
  }
  editGid = gid;
  const gt = $("cg-gtitle");
  if (gt) gt.textContent = cid ? "频道设置" : "新建频道";
  const owner = $("cg-cowner");
  if (owner) owner.textContent = `所属分组：${g.name}`;
  const name = $<HTMLInputElement>("cg-gname");
  if (name) name.value = ch.name;
  const mode = $<HTMLSelectElement>("cg-gmode");
  if (mode) mode.value = ch.mode;
  const del = $<HTMLButtonElement>("cg-group-del");
  if (del) {
    del.textContent = "删除频道";
    del.classList.remove("is-arm");
  }
  channelDeleteArmed = false;
  renderDraftSources(ch);
  $("cg-mask-group")?.classList.add("on");
  (window as any).__cgDraft = ch;
}
function renderDraftSources(ch: CChannel) {
  const host = $("cg-srcbody");
  if (!host) return;
  if (!ch.sources.length) ch.sources.push(blankSource(true));
  host.innerHTML = ch.sources
    .map(
      (s) => `<div class="cg-src-edit" data-sid="${s.id}">
        <input type="checkbox" class="en" ${s.enabled ? "checked" : ""} title="启用" />
        <input class="nm" value="${esc(s.name)}" placeholder="来源名称" />
        <input class="url" value="${esc(s.url)}" placeholder="udp://239.x.x.x:port 或本地文件" />
        <input type="radio" name="cg-base" class="bs" ${s.base ? "checked" : ""} title="设为基准" />
        <button class="del" title="删除来源">×</button>
      </div>`,
    )
    .join("");
  host.onclick = (e) => {
    const del = (e.target as HTMLElement).closest<HTMLElement>(".del");
    if (del) {
      const row = del.closest<HTMLElement>(".cg-src-edit");
      if (row) {
        ch.sources = ch.sources.filter((x) => x.id !== row.dataset.sid);
        renderDraftSources(ch);
      }
    }
  };
  host.onchange = (e) => {
    const radio = e.target as HTMLInputElement;
    if (radio.type === "radio" && radio.classList.contains("bs")) {
      const row = radio.closest<HTMLElement>(".cg-src-edit");
      if (row) {
        ch.sources.forEach((x) => (x.base = x.id === row.dataset.sid));
      }
    }
  };
}
function collectChannelDraft(): CChannel | null {
  const ch: CChannel | undefined = (window as any).__cgDraft;
  if (!ch) return null;
  const name = $<HTMLInputElement>("cg-gname")?.value.trim() || "未命名频道";
  const mode = ($<HTMLSelectElement>("cg-gmode")?.value || "base") as "base" | "pairwise";
  const rows = [...document.querySelectorAll<HTMLElement>("#cg-srcbody .cg-src-edit")];
  const sources = rows.map((row, i) => {
    const en = row.querySelector<HTMLInputElement>(".en")!.checked;
    const nm = row.querySelector<HTMLInputElement>(".nm")!.value.trim();
    const url = row.querySelector<HTMLInputElement>(".url")!.value.trim();
    const bs = row.querySelector<HTMLInputElement>(".bs")!.checked;
    return { id: row.dataset.sid || uid(), name: nm || `来源 ${i + 1}`, url, base: bs, enabled: en };
  });
  return { id: ch.id, name, mode, sources };
}
function channelShape(ch: CChannel): string {
  return `${ch.mode}|${ch.sources.map((s) => `${s.id}:${s.url.trim()}:${s.enabled ? 1 : 0}:${s.base ? 1 : 0}`).join(";")}`;
}
function saveChannel() {
  const g = groups.find((x) => x.id === editGid);
  const ch = collectChannelDraft();
  if (!g || !ch) return;
  if (!ch.sources.some((s) => s.base) && ch.sources[0]) ch.sources[0].base = true;
  const idx = g.channels.findIndex((x) => x.id === ch.id);
  const wasActive = activeGid === g.id && activeCid === ch.id;
  const changed = idx < 0 || channelShape(g.channels[idx]) !== channelShape(ch);
  if (idx >= 0) g.channels[idx] = ch;
  else g.channels.push(ch);
  if (changed) {
    clearChannelResults(g, ch);
    if (wasActive || idx < 0) setRunInfo("待机");
  }
  if (selectionReady) channelPairs(ch, g).forEach((p) => selectedPairs.add(resultKey(p)));
  activeGid = g.id;
  activeCid = ch.id;
  activeTgt = "";
  closedGroups.delete(g.id);
  persistGroups();
  closeChannel();
  renderGroups();
  startPreview(activePair());
}
function closeChannel() {
  channelDeleteArmed = false;
  $("cg-mask-group")?.classList.remove("on");
  (window as any).__cgDraft = null;
}
function deleteChannel() {
  const ch: CChannel | undefined = (window as any).__cgDraft;
  const g = groups.find((x) => x.id === editGid);
  if (!ch || !g || !g.channels.some((x) => x.id === ch.id)) {
    closeChannel();
    return;
  }
  if (!channelDeleteArmed) {
    channelDeleteArmed = true;
    const del = $<HTMLButtonElement>("cg-group-del");
    if (del) {
      del.textContent = `确认删除频道“${ch.name}”`;
      del.classList.add("is-arm");
    }
    return;
  }
  clearChannelResults(g, ch);
  g.channels = g.channels.filter((x) => x.id !== ch.id);
  if (activeGid === g.id && activeCid === ch.id) {
    activeCid = g.channels[0]?.id || "";
    activeTgt = "";
    setRunInfo("待机");
  }
  refreshGroupState(g, true);
  persistGroups();
  closeChannel();
  renderGroups();
  startPreview(activePair());
}
function persistGroups() {
  api.uiSet(G_KEY, groups).catch(() => {});
}

async function importChannels() {
  let chs: Channel[] = [];
  try {
    chs = await api.channelsGet();
  } catch {}
  const valid = (chs || []).filter((c) => c.enabled !== false && (c.url || "").trim());
  if (!valid.length) {
    setRunInfo("没有可导入的频道（先在设置里维护频道列表）", "err");
    return;
  }
  let g = activeGroup();
  if (!g) {
    g = { id: uid(), name: "导入频道", channels: [] };
    groups.push(g);
    activeGid = g.id;
  }
  let addedCh = 0;
  let addedSrc = 0;
  for (const c of valid) {
    const name = (c.name || "导入频道").trim();
    const url = c.url.trim();
    let ch = g.channels.find((x) => x.name === name);
    if (!ch) {
      ch = { id: uid(), name, mode: "base", sources: [] };
      g.channels.push(ch);
      addedCh += 1;
    }
    if (!ch.sources.some((s) => s.url.trim() === url)) {
      ch.sources.push({ id: uid(), name: "导入来源", url, base: !ch.sources.some((s) => s.base), enabled: true });
      addedSrc += 1;
    }
    if (!ch.sources.some((s) => s.base) && ch.sources[0]) ch.sources[0].base = true;
  }
  if (selectionReady) {
    for (const ch of g.channels) channelPairs(ch, g).forEach((p) => selectedPairs.add(resultKey(p)));
  }
  if (!activeCid || !g.channels.some((c) => c.id === activeCid)) activeCid = g.channels[0]?.id || "";
  activeTgt = "";
  closedGroups.delete(g.id);
  persistGroups();
  renderGroups();
  startPreview(activePair());
  setRunInfo(`导入完成 · 新增 ${addedCh} 个频道 / ${addedSrc} 个来源`);
}

function normalizeSources(raw: any): CSource[] {
  const list: CSource[] = Array.isArray(raw)
    ? raw.map((s: any, i: number) => ({
        id: String(s?.id || uid()),
        name: String(s?.name || `来源 ${i + 1}`),
        url: String(s?.url || ""),
        base: !!s?.base,
        enabled: s?.enabled !== false,
      }))
    : [];
  let baseIdx = list.findIndex((s) => s.base && s.enabled);
  if (baseIdx < 0) baseIdx = list.findIndex((s) => s.enabled);
  list.forEach((s, i) => (s.base = i === baseIdx));
  return list;
}

function normalizeChannel(raw: any, fallbackName: string, fallbackId: string): CChannel {
  return {
    id: String(raw?.id || fallbackId),
    name: String(raw?.name || fallbackName),
    mode: raw?.mode === "pairwise" ? "pairwise" : "base",
    sources: normalizeSources(raw?.sources),
  };
}

function normalizeGroup(raw: any): CGroup {
  const id = String(raw?.id || uid());
  const name = String(raw?.name || "未命名分组");
  // 旧版本：一个分组直接挂 sources，没有 channels 层级；迁移为“分组 + 同名频道”
  if (!Array.isArray(raw?.channels)) {
    return {
      id,
      name,
      channels: [
        {
          id: `${id}-ch`,
          name,
          mode: raw?.mode === "pairwise" ? "pairwise" : "base",
          sources: normalizeSources(raw?.sources),
        },
      ],
    };
  }
  return {
    id,
    name,
    channels: raw.channels
      .filter((c: any) => c && typeof c === "object")
      .map((c: any, i: number) => normalizeChannel(c, `频道 ${i + 1}`, `${id}-ch${i}`)),
  };
}

async function loadGroups() {
  try {
    const v: any = await api.uiGet(G_KEY);
    if (Array.isArray(v)) {
      groups = v.filter((g: any) => g && typeof g.id === "string").map((g: any) => normalizeGroup(g));
    }
  } catch {}
  if (!groups.length) {
    try {
      const chs = await api.channelsGet();
      if (chs?.length) {
        groups = [
          {
            id: uid(),
            name: "导入频道",
            channels: chs.map((c, i) => ({
              id: uid(),
              name: c.name || `频道 ${i + 1}`,
              mode: "base" as const,
              sources: normalizeSources([
                { id: uid(), name: "导入来源", url: c.url || "", base: true, enabled: c.enabled !== false },
              ]),
            })),
          },
        ];
      }
    } catch {}
  }
  if (!groups.length) {
    groups = [{ id: uid(), name: "默认分组", channels: [] }];
  }
  if (!activeGid || !groups.some((g) => g.id === activeGid)) activeGid = groups[0]?.id || "";
  const g = activeGroup();
  if (g && (!activeCid || !g.channels.some((c) => c.id === activeCid))) activeCid = g.channels[0]?.id || "";
  const firstPair = activePair();
  activeTgt = firstPair?.tgt.id || "";
  seedPairSelection();
  persistGroups();
}

// ═════════════ 初始化 ═════════════

function openListEditor() {
  openConsistEditor(groups, async (next) => {
    await api.uiSet(G_KEY, next);
    groups = next;
    results.clear();
    groupStates.clear();
    channelStates.clear();
    channelErrors.clear();
    closedGroups.clear();
    if (cgGroupFilter !== "all" && !groups.some((g) => g.id === cgGroupFilter)) cgGroupFilter = "all";
    if (!groups.some((g) => g.id === activeGid)) activeGid = groups[0]?.id || "";
    const g = activeGroup();
    if (!g || !g.channels.some((c) => c.id === activeCid)) activeCid = g?.channels[0]?.id || "";
    activeTgt = "";
    selectedPairs.clear();
    for (const p of allPairs()) selectedPairs.add(resultKey(p));
    selectionReady = true;
    renderGroups();
    startPreview(activePair());
    setRunInfo("列表已保存");
  });
}

export function initConsist() {
  $("cg-param")?.addEventListener("click", openParam);
  $("cg-param-x")?.addEventListener("click", closeParam);
  $("cg-param-cancel")?.addEventListener("click", closeParam);
  $("cg-param-save")?.addEventListener("click", saveParam);
  $("cg-param-reset")?.addEventListener("click", () => {
    const setV = (id: string, v: string) => {
      const el = $<HTMLInputElement>(id);
      if (el) el.value = v;
    };
    setV("cg-fps", "10");
    setV("cg-secs", "20");
    setV("cg-lag", "3");
    setV("cg-thr", "0.75");
    setV("cg-lx", "");
    setV("cg-ly", "");
    setV("cg-lw", "");
    setV("cg-lh", "");
    setV("cg-conc", "2");
    document.querySelectorAll<HTMLElement>(".cg-preset").forEach((b) => b.classList.toggle("is-on", b.dataset.fps === "10"));
  });
  document.querySelectorAll<HTMLElement>(".cg-preset").forEach((b) => {
    b.onclick = () => {
      document.querySelectorAll<HTMLElement>(".cg-preset").forEach((x) => x.classList.remove("is-on"));
      b.classList.add("is-on");
      ($<HTMLInputElement>("cg-fps")!).value = b.dataset.fps || "10";
      ($<HTMLInputElement>("cg-secs")!).value = b.dataset.secs || "20";
    };
  });
  $("cg-mask-param")?.addEventListener("click", (e) => {
    if (e.target === $("cg-mask-param")) closeParam();
  });

  $("cg-new")?.addEventListener("click", () => openGroupMeta());
  $("cg-import")?.addEventListener("click", importChannels);
  $("cg-editlist")?.addEventListener("click", openListEditor);
  $("cg-all")?.addEventListener("click", runAll);
  $("cg-runpair")?.addEventListener("click", runCurrent);
  $("cg-rungroup")?.addEventListener("click", runSelected);
  $("cg-stop")?.addEventListener("click", stopAll);
  $("cg-groupfilter")?.addEventListener("change", (e) => {
    cgGroupFilter = ($<HTMLSelectElement>("cg-groupfilter")?.value || "all");
    if (cgGroupFilter !== "all") {
      activeGid = cgGroupFilter;
      const g = activeGroup();
      activeCid = g?.channels[0]?.id || "";
      activeTgt = "";
      closedGroups.delete(cgGroupFilter);
    }
    renderGroups();
  });
  $("cg-search")?.addEventListener("input", (e) => {
    cgQuery = ($<HTMLInputElement>("cg-search")?.value || "");
    renderGroups();
  });
  $("cg-allbox")?.addEventListener("change", (e) => {
    const box = e.target as HTMLInputElement;
    for (const p of visiblePairs()) {
      const key = resultKey(p);
      if (box.checked) selectedPairs.add(key);
      else selectedPairs.delete(key);
    }
    renderGroups();
  });
  $("cg-collapse")?.addEventListener("click", () => {
    collapsed = !collapsed;
    $("cg-grid")?.classList.toggle("collapsed", collapsed);
    const left = $("cg-left");
    if (left) left.classList.toggle("is-collapsed", collapsed);
    const rail = $("cg-rail");
    if (rail) rail.hidden = !collapsed;
    const btn = $("cg-collapse");
    if (btn) btn.textContent = collapsed ? "» 展开" : "« 收起";
    renderRail();
  });
  $("cg-mask-gset")?.addEventListener("click", (e) => {
    if (e.target === $("cg-mask-gset")) closeGroupMeta();
  });
  $("cg-gset-x")?.addEventListener("click", closeGroupMeta);
  $("cg-gset-cancel")?.addEventListener("click", closeGroupMeta);
  $("cg-gset-save")?.addEventListener("click", saveGroupMeta);
  $("cg-gset-del")?.addEventListener("click", deleteGroupMeta);

  $("cg-mask-group")?.addEventListener("click", (e) => {
    if (e.target === $("cg-mask-group")) closeChannel();
  });
  $("cg-group-x")?.addEventListener("click", closeChannel);
  $("cg-group-cancel")?.addEventListener("click", closeChannel);
  $("cg-group-save")?.addEventListener("click", saveChannel);
  $("cg-group-del")?.addEventListener("click", deleteChannel);
  $("cg-addsrc")?.addEventListener("click", () => {
    const ch: CChannel | undefined = (window as any).__cgDraft;
    if (!ch) return;
    ch.sources.push(blankSource(!ch.sources.some((s) => s.base)));
    renderDraftSources(ch);
  });

  loadGroups().then(() => {
    renderGroups();
    const p = activePair();
    if (p) {
      activeTgt = p.tgt.id;
      startPreview(p);
    }
  });
  loadPrefs().then(() => {
    renderGroups();
  });
  syncChips();
  renderKpi(null);
  drawCurve(null);
  renderSegs(null);
  setRunInfo("待机");
}

/** 离开页面：只停预览，后台比对任务继续。 */
export function leaveConsist() {
  stopPreview();
}

/** 从告警子页回到比对工作台时，恢复当前对的画面预览。 */
export function enterConsist() {
  const p = activePair();
  if (!p) return;
  activeTgt = p.tgt.id;
  startPreview(p);
}








