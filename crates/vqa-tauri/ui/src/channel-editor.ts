//! 频道列表编辑页：微突发 / 音画同步共用一份本机频道库。
//!
//! 列表本身不在这里持有，页面只负责“打开编辑页 → 改名称/地址/启用 →
//! 保存并广播 channels-changed”，各业务页收到后重新拉取列表。

import { api, Channel } from "./api";
import { normalizeAddressText } from "./address";
import {
  ALL_GROUPS,
  DEFAULT_GROUP,
  countInGroup,
  groupMatches,
  groupNames,
  groupOf,
  normalizeGroup,
} from "./channel-groups";

const $ = <T extends HTMLElement>(id: string): T | null =>
  document.getElementById(id) as T | null;

const GROUPS_KEY = "channels.groups";

let built = false;
let draft: Channel[] = [];
let groups: string[] = [DEFAULT_GROUP];
let filter = "";
let groupFilter = ALL_GROUPS;
let importOpen = false;
let msgTimer = 0;

const esc = (s: string) =>
  (s || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

function ensure() {
  if (built) return;
  built = true;
  const frag = document.createElement("div");
  frag.innerHTML = `
  <div class="che-overlay" id="che-overlay" hidden>
    <div class="che-page" role="dialog" aria-modal="true" aria-labelledby="che-title">
      <div class="che-head">
        <div class="che-titlewrap">
          <b id="che-title">频道列表编辑</b>
          <span>微突发检测 / 音画同步共用 · 保存后立即同步到各页面</span>
        </div>
        <div class="che-headacts">
          <button class="che-mini" id="che-close" type="button">关闭</button>
        </div>
      </div>
      <div class="che-tools">
        <label class="che-search">搜索<input id="che-search" placeholder="按频道名或地址筛选" spellcheck="false" /></label>
        <span class="che-count" id="che-count">0 个 / 启用 0</span>
        <span class="che-sp"></span>
        <button class="che-mini" id="che-all" type="button">全部启用</button>
        <button class="che-mini" id="che-none" type="button">全部停用</button>
        <button class="che-mini" id="che-importbtn" type="button">批量导入</button>
        <button class="che-mini" id="che-addbtn" type="button">新增频道</button>
      </div>
      <div class="che-groupbar">
        <label class="che-groupsel">分组
          <select id="che-group-filter"></select>
        </label>
        <button class="che-mini" id="che-group-newbtn" type="button">新建分组</button>
        <button class="che-mini" id="che-group-managebtn" type="button">管理分组</button>
        <span class="che-groupcount" id="che-group-count"></span>
      </div>
      <div class="che-groupmanage" id="che-groupmanage" hidden>
        <div class="che-groupmanage-head">
          <b>分组管理</b>
          <span>改名会同步到该组所有频道；删除后频道移到「${DEFAULT_GROUP}」</span>
        </div>
        <div class="che-groupmanage-cols"><span>分组名称</span><span>频道数</span><span></span><span>操作</span></div>
        <div class="che-groupmanage-list" id="che-groupmanage-list"></div>
        <div class="che-groupmanage-add">
          <input id="che-group-new" placeholder="新分组名称" maxlength="24" spellcheck="false" />
          <button class="che-mini on" id="che-group-newadd" type="button">添加</button>
        </div>
      </div>
      <div class="che-import" id="che-import" hidden>
        <textarea id="che-import-text" spellcheck="false" placeholder="每行一个频道：名称,地址&#10;例如：CCTV-1 综合,udp://@239.1.1.1:5000&#10;只填地址也可以，名称自动补"></textarea>
        <div class="che-importacts">
          <span>支持逗号 / 制表符分隔；已存在的地址会跳过。</span>
          <button class="che-mini" id="che-import-cancel" type="button">取消</button>
          <button class="che-mini on" id="che-import-ok" type="button">导入</button>
        </div>
      </div>
      <div class="che-tablewrap">
        <table class="che-table">
          <thead>
            <tr>
              <th class="che-chkcol"><input type="checkbox" id="che-allbox" /></th>
              <th>分组</th>
              <th>频道名称</th>
              <th>地址</th>
              <th class="che-stcol">启用</th>
              <th class="che-actcol">操作</th>
            </tr>
          </thead>
          <tbody id="che-rows"></tbody>
        </table>
        <div class="che-empty" id="che-empty" hidden></div>
      </div>
      <div class="che-foot">
        <span class="che-msg" id="che-msg"></span>
        <button class="che-mini" id="che-cancel" type="button">取消</button>
        <button class="gradbtn che-save" id="che-save" type="button">保存列表</button>
      </div>
    </div>
  </div>`;
  while (frag.firstElementChild) document.body.appendChild(frag.firstElementChild);

  $("che-close")?.addEventListener("click", close);
  $("che-cancel")?.addEventListener("click", close);
  $("che-save")?.addEventListener("click", () => void save());
  $("che-addbtn")?.addEventListener("click", addRow);
  $("che-importbtn")?.addEventListener("click", () => toggleImport(true));
  $("che-import-cancel")?.addEventListener("click", () => toggleImport(false));
  $("che-import-ok")?.addEventListener("click", importRows);
  $("che-all")?.addEventListener("click", () => setAll(true));
  $("che-none")?.addEventListener("click", () => setAll(false));
  $("che-group-filter")?.addEventListener("change", () => {
    groupFilter = $<HTMLSelectElement>("che-group-filter")?.value || ALL_GROUPS;
    render();
  });
  $("che-group-managebtn")?.addEventListener("click", () => toggleGroupManage());
  $("che-group-newbtn")?.addEventListener("click", () => {
    toggleGroupManage(true);
    $<HTMLInputElement>("che-group-new")?.focus();
  });
  $("che-group-newadd")?.addEventListener("click", addGroup);
  $("che-group-new")?.addEventListener("keydown", (ev) => {
    if ((ev as KeyboardEvent).key === "Enter") addGroup();
  });
  $("che-groupmanage-list")?.addEventListener("click", (ev) => {
    const btn = (ev.target as HTMLElement).closest("button") as HTMLButtonElement | null;
    if (!btn) return;
    const name = btn.dataset.group || "";
    if (btn.dataset.act === "group-rename") renameGroup(name, btn);
    if (btn.dataset.act === "group-del") deleteGroup(name);
  });
  $("che-search")?.addEventListener("input", () => {
    filter = ($<HTMLInputElement>("che-search")?.value || "").trim();
    render();
  });
  $("che-allbox")?.addEventListener("change", (ev) => {
    const on = (ev.target as HTMLInputElement).checked;
    setAll(on, true);
  });
  $("che-overlay")?.addEventListener("click", (ev) => {
    if ((ev.target as HTMLElement).id === "che-overlay") close();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && !$("che-overlay")?.hidden) close();
  });
  $("che-rows")?.addEventListener("input", (ev) => {
    const t = ev.target as HTMLInputElement;
    if (!t.dataset.i) return;
    const i = Number(t.dataset.i);
    if (!draft[i]) return;
    if (t.dataset.field === "name") draft[i].name = t.value;
    if (t.dataset.field === "url") draft[i].url = t.value;
  });
  $("che-rows")?.addEventListener("change", (ev) => {
    const t = ev.target as HTMLInputElement | HTMLSelectElement;
    if (!t.dataset.i) return;
    const i = Number(t.dataset.i);
    if (!draft[i]) return;
    if (t.dataset.field === "enabled") {
      draft[i].enabled = (t as HTMLInputElement).checked;
      render();
    }
    if (t.dataset.field === "group") {
      draft[i].group = t.value;
      render();
    }
  });
  $("che-rows")?.addEventListener("click", (ev) => {
    const btn = (ev.target as HTMLElement).closest("button") as HTMLButtonElement | null;
    if (!btn) return;
    const i = Number(btn.dataset.i);
    if (btn.dataset.act === "del" && draft[i]) {
      draft.splice(i, 1);
      render();
    }
  });
}

function setMsg(text: string, err = false) {
  const el = $<HTMLElement>("che-msg");
  if (!el) return;
  el.textContent = text;
  el.classList.toggle("err", err);
  if (msgTimer) window.clearTimeout(msgTimer);
  if (text) msgTimer = window.setTimeout(() => { el.textContent = ""; }, 3000);
}

function updateCount() {
  const el = $<HTMLElement>("che-count");
  if (!el) return;
  const on = draft.filter((c) => c.enabled).length;
  el.textContent = `${draft.length} 个 / 启用 ${on}`;
  const gc = $<HTMLElement>("che-group-count");
  if (gc) gc.textContent = `共 ${groups.length} 个分组`;
}

function ensureGroups() {
  groups = groupNames(draft, groups);
}

function renderGroupFilter() {
  const sel = $<HTMLSelectElement>("che-group-filter");
  if (!sel) return;
  ensureGroups();
  if (groupFilter !== ALL_GROUPS && !groups.includes(groupFilter)) groupFilter = ALL_GROUPS;
  sel.innerHTML =
    `<option value="${ALL_GROUPS}">全部分组（${draft.length}）</option>` +
    groups.map((g) => `<option value="${esc(g)}">${esc(g)}（${countInGroup(draft, g)}）</option>`).join("");
  sel.value = groupFilter;
}

function renderGroupManager() {
  const list = $<HTMLElement>("che-groupmanage-list");
  if (!list) return;
  ensureGroups();
  list.innerHTML = groups
    .map((g) => {
      const fixed = g === DEFAULT_GROUP;
      const actions = fixed
        ? `<span class="che-groupfixed">默认</span><span></span>`
        : `<button class="che-mini" data-act="group-rename" data-group="${esc(g)}" type="button">保存</button>` +
          `<button class="che-del" data-act="group-del" data-group="${esc(g)}" type="button">删除</button>`;
      return (
        `<div class="che-groupitem" data-group="${esc(g)}">` +
        `<input class="che-groupname" value="${esc(g)}" maxlength="24" spellcheck="false" ${fixed ? "readonly" : ""} />` +
        `<span class="che-groupstat">${countInGroup(draft, g)} 个频道</span>` +
        actions +
        `</div>`
      );
    })
    .join("");
}

function toggleGroupManage(force?: boolean) {
  const panel = $<HTMLElement>("che-groupmanage");
  if (!panel) return;
  const show = force === undefined ? panel.hidden : force;
  panel.hidden = !show;
  const btn = $<HTMLButtonElement>("che-group-managebtn");
  if (btn) {
    btn.classList.toggle("on", show);
    btn.textContent = show ? "收起分组" : "管理分组";
  }
  if (show) renderGroupManager();
}

function addGroup() {
  const input = $<HTMLInputElement>("che-group-new");
  if (!input) return;
  if (!input.value.trim()) {
    setMsg("请输入分组名称", true);
    return;
  }
  const name = normalizeGroup(input.value);
  if (name === DEFAULT_GROUP || groups.includes(name)) {
    setMsg(`分组「${name}」已存在`, true);
    return;
  }
  groups.push(name);
  input.value = "";
  render();
  setMsg(`已添加分组「${name}」，保存后生效`);
}

function renameGroup(oldName: string, btn: HTMLButtonElement) {
  if (!oldName || oldName === DEFAULT_GROUP) return;
  const item = btn.closest<HTMLElement>(".che-groupitem");
  const input = item?.querySelector<HTMLInputElement>(".che-groupname");
  const next = normalizeGroup(input?.value);
  if (next === oldName) return;
  if (next === DEFAULT_GROUP || groups.includes(next)) {
    setMsg(`分组「${next}」已存在`, true);
    renderGroupManager();
    return;
  }
  groups = groups.map((g) => (g === oldName ? next : g));
  draft.forEach((c) => {
    if (groupOf(c) === oldName) c.group = next;
  });
  if (groupFilter === oldName) groupFilter = next;
  render();
  setMsg(`分组「${oldName}」已改名为「${next}」`);
}

function deleteGroup(name: string) {
  if (!name || name === DEFAULT_GROUP) return;
  const moved = countInGroup(draft, name);
  draft.forEach((c) => {
    if (groupOf(c) === name) c.group = DEFAULT_GROUP;
  });
  groups = groups.filter((g) => g !== name);
  if (groupFilter === name) groupFilter = ALL_GROUPS;
  render();
  setMsg(`已删除分组「${name}」，${moved} 个频道移到「${DEFAULT_GROUP}」`);
}

function toggleImport(on: boolean) {
  importOpen = on;
  const box = $<HTMLElement>("che-import");
  if (box) box.hidden = !on;
  if (on) ($<HTMLTextAreaElement>("che-import-text") as HTMLTextAreaElement | null)?.focus();
}

function addRow() {
  draft.push({
    name: "",
    url: "",
    group: groupFilter === ALL_GROUPS ? DEFAULT_GROUP : groupFilter,
    enabled: true,
  });
  filter = "";
  const search = $<HTMLInputElement>("che-search");
  if (search) search.value = "";
  render();
  const rows = document.querySelectorAll<HTMLInputElement>("#che-rows .che-name");
  rows[rows.length - 1]?.focus();
}

function importRows() {
  const ta = $<HTMLTextAreaElement>("che-import-text");
  const text = (ta?.value || "").trim();
  if (!text) return;
  const seen = new Set(draft.map((c) => c.url.trim().toLowerCase()).filter(Boolean));
  let added = 0;
  for (const line of text.split(/\r?\n/).map((x) => x.trim()).filter(Boolean)) {
    let name = "";
    let url = line;
    const m = line.match(/^(.+?)[,，\t]\s*((?:udp|rtp|rtsp|http|https|file):.*|\S.*)$/);
    if (m) {
      name = m[1].trim();
      url = normalizeAddressText(m[2].trim());
    } else {
      url = normalizeAddressText(url);
    }
    if (!url || seen.has(url.toLowerCase())) continue;
    seen.add(url.toLowerCase());
    draft.push({
      name: name || `频道${draft.length + 1}`,
      url,
      group: groupFilter === ALL_GROUPS ? DEFAULT_GROUP : groupFilter,
      enabled: true,
    });
    added += 1;
  }
  if (ta) ta.value = "";
  toggleImport(false);
  filter = "";
  const search = $<HTMLInputElement>("che-search");
  if (search) search.value = "";
  render();
  setMsg(added ? `已导入 ${added} 个频道，记得保存` : "没有新增频道（可能地址已存在）", !added);
}

function setAll(on: boolean, onlyVisible = false) {
  const list = onlyVisible ? visibleRows().map(({ ch }) => ch) : draft;
  list.forEach((c) => (c.enabled = on));
  render();
}

function visibleRows(): { ch: Channel; i: number }[] {
  const q = filter.toLowerCase();
  return draft
    .map((ch, i) => ({ ch, i }))
    .filter(({ ch }) => groupMatches(ch, groupFilter) && (!q || `${ch.name} ${ch.url}`.toLowerCase().includes(q)));
}

function groupOptions(selected: string): string {
  ensureGroups();
  return groups
    .map((g) => `<option value="${esc(g)}"${g === selected ? " selected" : ""}>${esc(g)}</option>`)
    .join("");
}

function render() {
  renderGroupFilter();
  renderGroupManager();
  updateCount();
  const rows = $<HTMLElement>("che-rows");
  const empty = $<HTMLElement>("che-empty");
  if (!rows) return;
  const list = visibleRows();
  if (!list.length) {
    rows.innerHTML = "";
    if (empty) {
      empty.hidden = false;
      empty.textContent = draft.length
        ? groupFilter === ALL_GROUPS
          ? "没有匹配的频道，换个关键词试试。"
          : `「${groupFilter}」分组暂无频道。`
        : "还没有频道。点右上角「新增频道」或「批量导入」开始。";
    }
  } else {
    if (empty) empty.hidden = true;
    rows.innerHTML = list
      .map(
        ({ ch, i }) =>
          `<tr data-i="${i}">` +
          `<td class="che-chkcol"><input type="checkbox" data-i="${i}" data-field="enabled" ${ch.enabled ? "checked" : ""} /></td>` +
          `<td><select class="che-gselect" data-i="${i}" data-field="group" title="修改所属分组">${groupOptions(groupOf(ch))}</select></td>` +
          `<td><input class="che-name" data-i="${i}" data-field="name" value="${esc(ch.name)}" placeholder="频道名称" spellcheck="false" /></td>` +
          `<td><input class="che-url mono" data-i="${i}" data-field="url" value="${esc(ch.url)}" placeholder="udp://@239.x.x.x:port 或本地文件" spellcheck="false" /></td>` +
          `<td class="che-stcol"><span class="che-state ${ch.enabled ? "on" : "off"}">${ch.enabled ? "启用" : "停用"}</span></td>` +
          `<td class="che-actcol"><button class="che-del" data-i="${i}" data-act="del" type="button">删除</button></td>` +
          `</tr>`,
      )
      .join("");
  }
  const all = $<HTMLInputElement>("che-allbox");
  if (all) {
    const on = list.filter(({ ch }) => ch.enabled).length;
    all.checked = list.length > 0 && on === list.length;
    all.indeterminate = on > 0 && on < list.length;
  }
}

async function loadDraft() {
  let list: Channel[] = [];
  let savedGroups: unknown = null;
  try {
    list = await api.channelsGet();
  } catch {
    list = [];
  }
  try {
    savedGroups = await api.uiGet(GROUPS_KEY);
  } catch {
    savedGroups = null;
  }
  const seen = new Set<string>();
  draft = (list || [])
    .map((c) => ({
      name: c.name || "",
      url: normalizeAddressText(c.url || ""),
      group: normalizeGroup(c.group),
      enabled: c.enabled !== false,
    }))
    .filter((c) => {
      const k = c.url.trim().toLowerCase();
      if (!k || seen.has(k)) return false;
      seen.add(k);
      return true;
    });
  const saved = Array.isArray(savedGroups)
    ? savedGroups.filter((g): g is string => typeof g === "string")
    : [];
  groups = groupNames(draft, saved);
  groupFilter = ALL_GROUPS;
  filter = "";
  const search = $<HTMLInputElement>("che-search");
  if (search) search.value = "";
  toggleImport(false);
  toggleGroupManage(false);
  setMsg("");
  render();
}

export async function openChannelEditor() {
  ensure();
  const overlay = $<HTMLElement>("che-overlay");
  if (!overlay) return;
  overlay.hidden = false;
  await loadDraft();
  ($<HTMLInputElement>("che-search") as HTMLInputElement | null)?.focus();
}

function close() {
  const overlay = $<HTMLElement>("che-overlay");
  if (overlay) overlay.hidden = true;
}

async function save() {
  const cleaned: Channel[] = [];
  const seen = new Set<string>();
  for (const c of draft) {
    const url = normalizeAddressText(c.url.trim());
    if (!url) continue;
    const key = url.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    cleaned.push({
      name: c.name.trim() || `频道${cleaned.length + 1}`,
      url,
      group: normalizeGroup(c.group),
      enabled: c.enabled !== false,
    });
  }
  groups = groupNames(cleaned, groups);
  try {
    await api.channelsSave(cleaned);
  } catch (e: any) {
    setMsg(`保存失败：${String(e)}`, true);
    return;
  }
  let groupSaveFailed = false;
  try {
    await api.uiSet(GROUPS_KEY, groups);
  } catch {
    groupSaveFailed = true;
  }
  draft = cleaned;
  window.dispatchEvent(new CustomEvent("channels-changed"));
  if (groupSaveFailed) {
    setMsg("频道已保存，但分组清单保存失败", true);
  } else {
    setMsg(`已保存 ${cleaned.length} 个频道 · ${groups.length} 个分组`);
  }
  window.setTimeout(close, 450);
}
