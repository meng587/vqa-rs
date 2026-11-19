//! 频道列表编辑页：微突发 / 音画同步共用一份本机频道库。
//!
//! 列表本身不在这里持有，页面只负责“打开编辑页 → 改名称/地址/启用 →
//! 保存并广播 channels-changed”，各业务页收到后重新拉取列表。

import { api, Channel } from "./api";
import { normalizeAddressText } from "./address";

const $ = <T extends HTMLElement>(id: string): T | null =>
  document.getElementById(id) as T | null;

let built = false;
let draft: Channel[] = [];
let filter = "";
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
  $("che-search")?.addEventListener("input", () => {
    filter = ($<HTMLInputElement>("che-search")?.value || "").trim();
    render();
  });
  $("che-allbox")?.addEventListener("change", (ev) => {
    const on = (ev.target as HTMLInputElement).checked;
    setAll(on);
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
    const t = ev.target as HTMLInputElement;
    if (!t.dataset.i) return;
    const i = Number(t.dataset.i);
    if (!draft[i]) return;
    if (t.dataset.field === "enabled") {
      draft[i].enabled = t.checked;
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
}

function toggleImport(on: boolean) {
  importOpen = on;
  const box = $<HTMLElement>("che-import");
  if (box) box.hidden = !on;
  if (on) ($<HTMLTextAreaElement>("che-import-text") as HTMLTextAreaElement | null)?.focus();
}

function addRow() {
  draft.push({ name: "", url: "", enabled: true });
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
    draft.push({ name: name || `频道${draft.length + 1}`, url, enabled: true });
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

function setAll(on: boolean) {
  draft.forEach((c) => (c.enabled = on));
  render();
}

function visibleRows(): { ch: Channel; i: number }[] {
  const q = filter.toLowerCase();
  return draft
    .map((ch, i) => ({ ch, i }))
    .filter(({ ch }) => !q || `${ch.name} ${ch.url}`.toLowerCase().includes(q));
}

function render() {
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
        ? "没有匹配的频道，换个关键词试试。"
        : "还没有频道。点右上角「新增频道」或「批量导入」开始。";
    }
  } else {
    if (empty) empty.hidden = true;
    rows.innerHTML = list
      .map(
        ({ ch, i }) =>
          `<tr data-i="${i}">` +
          `<td class="che-chkcol"><input type="checkbox" data-i="${i}" data-field="enabled" ${ch.enabled ? "checked" : ""} /></td>` +
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
    const on = draft.filter((c) => c.enabled).length;
    all.checked = draft.length > 0 && on === draft.length;
    all.indeterminate = on > 0 && on < draft.length;
  }
}

async function loadDraft() {
  try {
    const list = await api.channelsGet();
    const seen = new Set<string>();
    draft = (list || [])
      .map((c) => ({ name: c.name || "", url: normalizeAddressText(c.url || ""), enabled: c.enabled !== false }))
      .filter((c) => {
        const k = c.url.trim().toLowerCase();
        if (!k || seen.has(k)) return false;
        seen.add(k);
        return true;
      });
  } catch {
    draft = [];
  }
  filter = "";
  const search = $<HTMLInputElement>("che-search");
  if (search) search.value = "";
  toggleImport(false);
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
      enabled: c.enabled !== false,
    });
  }
  try {
    await api.channelsSave(cleaned);
  } catch (e: any) {
    setMsg(`保存失败：${String(e)}`, true);
    return;
  }
  draft = cleaned;
  window.dispatchEvent(new CustomEvent("channels-changed"));
  setMsg(`已保存 ${cleaned.length} 个频道`);
  window.setTimeout(close, 450);
}
