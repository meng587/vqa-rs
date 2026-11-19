//! 一致性比对专用「编辑列表」页。
//!
//! 独立于微突发/音画同步共用的扁平频道库：这里编辑的是
//! 分组 → 频道 → 多个来源（基准/待比）的三层结构。
//! 草稿只在保存时回写，取消不会影响当前比对页。

import { normalizeAddressText } from "./address";
import type { CChannel, CGroup, CSource } from "./consist";

const overlayId = "cge-overlay";

const $ = <T extends HTMLElement>(id: string): T | null =>
  document.getElementById(id) as T | null;

const esc = (s: string) =>
  String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

const uid = () => `e${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

let built = false;
let draft: CGroup[] = [];
let filter = "";
let groupFilter = "all";
let msgTimer = 0;
let saveHandler: ((next: CGroup[]) => Promise<void> | void) | null = null;

function ensure() {
  if (built) return;
  built = true;
  const frag = document.createElement("div");
  frag.innerHTML = `
  <div class="che-overlay" id="${overlayId}" hidden>
    <div class="che-page cge-page" role="dialog" aria-modal="true" aria-labelledby="cge-title">
      <div class="che-head">
        <div class="che-titlewrap">
          <b id="cge-title">一致性比对 · 编辑列表</b>
          <span>分组 → 频道 → 基准/待比来源；保存后立即同步到比对页，取消不生效</span>
        </div>
        <div class="che-headacts">
          <button class="che-mini" id="cge-close" type="button">关闭</button>
        </div>
      </div>
      <div class="che-tools">
        <label class="che-search">搜索<input id="cge-search" placeholder="按分组、频道名、来源名或地址筛选" spellcheck="false" /></label>
        <span class="che-count" id="cge-count">0 组 / 0 频道 / 0 对</span>
        <span class="che-sp"></span>
        <label class="che-groupsel">分组<select id="cge-groupfilter"></select></label>
        <button class="che-mini" id="cge-addgroup" type="button">新建分组</button>
      </div>
      <div class="cge-body" id="cge-body"></div>
      <div class="che-foot">
        <span class="che-msg" id="cge-msg"></span>
        <button class="che-mini" id="cge-cancel" type="button">取消</button>
        <button class="gradbtn che-save" id="cge-save" type="button">保存列表</button>
      </div>
    </div>
  </div>`;
  while (frag.firstElementChild) document.body.appendChild(frag.firstElementChild);

  $("cge-close")?.addEventListener("click", close);
  $("cge-cancel")?.addEventListener("click", close);
  $("cge-save")?.addEventListener("click", () => void save());
  $("cge-addgroup")?.addEventListener("click", addGroup);
  $("cge-search")?.addEventListener("input", () => {
    filter = ($<HTMLInputElement>("cge-search")?.value || "").trim();
    render();
  });
  $("cge-groupfilter")?.addEventListener("change", () => {
    groupFilter = $<HTMLSelectElement>("cge-groupfilter")?.value || "all";
    render();
  });
  $("cge-overlay")?.addEventListener("click", (ev) => {
    if ((ev.target as HTMLElement).id === overlayId) close();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && !$<HTMLElement>(overlayId)?.hidden) close();
  });

  const body = $<HTMLElement>("cge-body");
  body?.addEventListener("input", (ev) => {
    const t = ev.target as HTMLInputElement;
    const act = t.dataset.act;
    if (!act) return;
    const loc = locate(t);
    if (!loc) return;
    if (act === "gname") loc.g.name = t.value;
    if (act === "cname" && loc.ch) loc.ch.name = t.value;
    if (act === "sname" && loc.src) loc.src.name = t.value;
    if (act === "surl" && loc.src) loc.src.url = t.value;
  });
  body?.addEventListener("change", (ev) => {
    const t = ev.target as HTMLInputElement | HTMLSelectElement;
    const act = t.dataset.act;
    const loc = locate(t);
    if (!loc) return;
    if (act === "en" && loc.src) {
      loc.src.enabled = (t as HTMLInputElement).checked;
      render();
    }
    if (act === "base" && loc.src && loc.ch) {
      loc.ch.sources.forEach((s) => (s.base = s.id === loc.src!.id));
      render();
    }
    if (act === "mode" && loc.ch) {
      loc.ch.mode = (t as HTMLSelectElement).value === "pairwise" ? "pairwise" : "base";
      render();
    }
    if (act === "moveg" && loc.ch) {
      moveChannel(loc.g.id, loc.ch.id, (t as HTMLSelectElement).value);
      render();
    }
  });
  body?.addEventListener("click", (ev) => {
    const btn = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-act]");
    if (!btn) return;
    const act = btn.dataset.act;
    const loc = locate(btn);
    if (!loc && act !== "addch" && act !== "delgroup") return;
    if (act === "addch" && btn.dataset.gid) {
      const g = draft.find((x) => x.id === btn.dataset.gid);
      if (!g) return;
      const ch: CChannel = {
        id: uid(),
        name: "新频道",
        mode: "base",
        sources: [blankSource(true), blankSource(false)],
      };
      g.channels.push(ch);
      render();
      focusField(g.id, ch.id, "", "cname");
      return;
    }
    if (!loc) return;
    if (act === "addsrc" && loc.ch) {
      const s = blankSource(!loc.ch.sources.some((x) => x.base));
      loc.ch.sources.push(s);
      render();
      focusField(loc.g.id, loc.ch.id, s.id, "sname");
      return;
    }
    if (act === "delsrc" && loc.ch && loc.src) {
      loc.ch.sources = loc.ch.sources.filter((s) => s.id !== loc.src!.id);
      render();
      return;
    }
    if (act === "delch" && loc.ch) {
      loc.g.channels = loc.g.channels.filter((c) => c.id !== loc.ch!.id);
      render();
      return;
    }
    if (act === "delgroup" && loc.g) {
      deleteGroup(loc.g.id);
    }
  });
}

function locate(el: HTMLElement): { g: CGroup; ch?: CChannel; src?: CSource } | null {
  const gid = el.dataset.gid || el.closest<HTMLElement>("[data-gid]")?.dataset.gid || "";
  const cid = el.dataset.cid || el.closest<HTMLElement>("[data-cid]")?.dataset.cid || "";
  const sid = el.dataset.sid || el.closest<HTMLElement>("[data-sid]")?.dataset.sid || "";
  const g = draft.find((x) => x.id === gid);
  if (!g) return null;
  const ch = cid ? g.channels.find((x) => x.id === cid) : undefined;
  const src = ch && sid ? ch.sources.find((x) => x.id === sid) : undefined;
  return { g, ch, src };
}

function blankSource(base: boolean): CSource {
  return {
    id: uid(),
    name: base ? "主用源" : "备用源",
    url: "",
    base,
    enabled: true,
  };
}

function setMsg(text: string, err = false) {
  const el = $<HTMLElement>("cge-msg");
  if (!el) return;
  el.textContent = text;
  el.classList.toggle("err", err);
  if (msgTimer) window.clearTimeout(msgTimer);
  if (text) msgTimer = window.setTimeout(() => (el.textContent = ""), 3500);
}

function enabled(ch: CChannel): CSource[] {
  return ch.sources.filter((s) => s.enabled && s.url.trim());
}

function pairCount(ch: CChannel): number {
  const on = enabled(ch);
  if (on.length < 2) return 0;
  if (ch.mode === "pairwise") return (on.length * (on.length - 1)) / 2;
  return on.length - 1;
}

function groupPairCount(g: CGroup): number {
  return g.channels.reduce((n, ch) => n + pairCount(ch), 0);
}

function totalPairs(): number {
  return draft.reduce((n, g) => n + groupPairCount(g), 0);
}

function channelText(g: CGroup, ch: CChannel): string {
  return `${g.name} ${ch.name} ${ch.sources.map((s) => `${s.name} ${s.url}`).join(" ")}`.toLowerCase();
}

function visibleChannels(g: CGroup): CChannel[] {
  const q = filter.toLowerCase();
  if (!q || g.name.toLowerCase().includes(q)) return g.channels;
  return g.channels.filter((ch) => channelText(g, ch).includes(q));
}

function renderGroupFilter() {
  const sel = $<HTMLSelectElement>("cge-groupfilter");
  if (!sel) return;
  if (groupFilter !== "all" && !draft.some((g) => g.id === groupFilter)) groupFilter = "all";
  sel.innerHTML =
    `<option value="all">全部分组（${draft.length}）</option>` +
    draft.map((g) => `<option value="${esc(g.id)}">${esc(g.name)}（${groupPairCount(g)} 对）</option>`).join("");
  sel.value = groupFilter;
}

function renderGroupSelect(selectedGid: string): string {
  return draft
    .map((g) => `<option value="${esc(g.id)}"${g.id === selectedGid ? " selected" : ""}>${esc(g.name)}</option>`)
    .join("");
}

function groupHtml(g: CGroup): string {
  const channels = visibleChannels(g);
  const q = filter.trim();
  if (q && !g.name.toLowerCase().includes(q.toLowerCase()) && !channels.length) return "";
  return `<section class="cge-group" data-gid="${g.id}">
    <div class="cge-ghead">
      <input class="cge-in cge-gname" data-act="gname" data-gid="${g.id}" value="${esc(g.name)}" placeholder="分组名称" spellcheck="false" />
      <span class="cge-gmeta">${g.channels.length} 频道 · ${groupPairCount(g)} 对</span>
      <button class="che-mini" data-act="addch" data-gid="${g.id}" type="button">＋ 新增频道</button>
      <button class="cge-del" data-act="delgroup" data-gid="${g.id}" type="button">删除分组</button>
    </div>
    <div class="cge-chlist">
      ${channels.map((ch) => channelHtml(g, ch)).join("") || `<div class="cge-empty">这个分组下还没有频道，点右上角「新增频道」。</div>`}
    </div>
  </section>`;
}

function channelHtml(g: CGroup, ch: CChannel): string {
  const on = enabled(ch).length;
  const count = pairCount(ch);
  const sources = ch.sources.length
    ? ch.sources
        .map(
          (s) => `<div class="cge-src${s.enabled ? "" : " off"}" data-gid="${g.id}" data-cid="${ch.id}" data-sid="${s.id}">
            <input type="checkbox" data-act="en" data-gid="${g.id}" data-cid="${ch.id}" data-sid="${s.id}" ${s.enabled ? "checked" : ""} title="启用该来源" />
            <input type="radio" name="cge-base-${ch.id}" data-act="base" data-gid="${g.id}" data-cid="${ch.id}" data-sid="${s.id}" ${s.base ? "checked" : ""} title="设为基准" />
            <input class="cge-in cge-sname" data-act="sname" data-gid="${g.id}" data-cid="${ch.id}" data-sid="${s.id}" value="${esc(s.name)}" placeholder="来源名称" spellcheck="false" />
            <input class="cge-in cge-url mono" data-act="surl" data-gid="${g.id}" data-cid="${ch.id}" data-sid="${s.id}" value="${esc(s.url)}" placeholder="udp://239.x.x.x:port 或本地文件" spellcheck="false" />
            <button class="cge-del" data-act="delsrc" data-gid="${g.id}" data-cid="${ch.id}" data-sid="${s.id}" type="button">删除</button>
          </div>`,
        )
        .join("")
    : `<div class="cge-empty">没有来源，点下方添加。</div>`;
  return `<div class="cge-channel" data-gid="${g.id}" data-cid="${ch.id}">
    <div class="cge-chead">
      <input class="cge-in cge-cname" data-act="cname" data-gid="${g.id}" data-cid="${ch.id}" value="${esc(ch.name)}" placeholder="频道名称" spellcheck="false" />
      <select class="cge-sel" data-act="moveg" data-gid="${g.id}" data-cid="${ch.id}" title="移动到其他分组">${renderGroupSelect(g.id)}</select>
      <select class="cge-sel" data-act="mode" data-gid="${g.id}" data-cid="${ch.id}" title="基准对比：基准对每个其他来源；两两对比：所有启用来源两两比对">
        <option value="base"${ch.mode === "base" ? " selected" : ""}>基准对比</option>
        <option value="pairwise"${ch.mode === "pairwise" ? " selected" : ""}>两两对比</option>
      </select>
      <span class="cge-pairs">${on} 路 · ${count} 对</span>
      <button class="cge-del" data-act="delch" data-gid="${g.id}" data-cid="${ch.id}" type="button">删除频道</button>
    </div>
    <div class="cge-srcs">
      <div class="cge-srchead"><span>启用</span><span>基准</span><span>来源名称</span><span>来源地址</span><span>操作</span></div>
      ${sources}
      <button class="cge-addsrc" data-act="addsrc" data-gid="${g.id}" data-cid="${ch.id}" type="button">＋ 添加来源（如备份链路）</button>
    </div>
  </div>`;
}

function render() {
  renderGroupFilter();
  const host = $<HTMLElement>("cge-body");
  if (!host) return;
  const totalCh = draft.reduce((n, g) => n + g.channels.length, 0);
  const cnt = $<HTMLElement>("cge-count");
  if (cnt) cnt.textContent = `${draft.length} 组 / ${totalCh} 频道 / ${totalPairs()} 对`;
  const shown = draft.filter((g) => groupFilter === "all" || g.id === groupFilter);
  const html = shown.map(groupHtml).join("");
  host.innerHTML = html || `<div class="cge-empty">没有匹配的频道，换个关键词或切回「全部分组」。</div>`;
}

function focusField(gid: string, cid: string, sid: string, act: string) {
  window.setTimeout(() => {
    if (!cid) {
      document.querySelector<HTMLInputElement>(`#cge-body .cge-group[data-gid="${gid}"] input[data-act="gname"]`)?.focus();
      return;
    }
    const selector = sid
      ? `.cge-src[data-gid="${gid}"][data-cid="${cid}"][data-sid="${sid}"] input[data-act="${act}"]`
      : `.cge-channel[data-gid="${gid}"][data-cid="${cid}"] input[data-act="${act}"]`;
    document.querySelector<HTMLInputElement>(selector)?.focus();
  }, 30);
}

function moveChannel(fromGid: string, cid: string, toGid: string) {
  if (fromGid === toGid) return;
  const from = draft.find((g) => g.id === fromGid);
  const to = draft.find((g) => g.id === toGid);
  if (!from || !to) return;
  const idx = from.channels.findIndex((c) => c.id === cid);
  if (idx < 0) return;
  const [ch] = from.channels.splice(idx, 1);
  to.channels.push(ch);
  setMsg(`已移到「${to.name}」，保存后生效`);
}

function uniqueGroupName(base: string): string {
  const root = base.trim() || "新分组";
  if (!draft.some((g) => g.name === root)) return root;
  let n = 2;
  while (draft.some((g) => g.name === `${root} ${n}`)) n += 1;
  return `${root} ${n}`;
}

function addGroup() {
  const g: CGroup = { id: uid(), name: uniqueGroupName("新分组"), channels: [] };
  draft.push(g);
  groupFilter = "all";
  filter = "";
  const search = $<HTMLInputElement>("cge-search");
  if (search) search.value = "";
  render();
  focusField(g.id, "", "", "gname");
  setMsg(`已新增「${g.name}」，保存后生效`);
}

function deleteGroup(gid: string) {
  if (draft.length <= 1) {
    setMsg("至少保留一个分组", true);
    return;
  }
  const g = draft.find((x) => x.id === gid);
  if (!g) return;
  const target = draft.find((x) => x.id !== gid);
  if (!target) return;
  if (g.channels.length) {
    target.channels.push(...g.channels);
  }
  draft = draft.filter((x) => x.id !== gid);
  if (groupFilter === gid) groupFilter = "all";
  render();
  setMsg(g.channels.length ? `已删除分组，${g.channels.length} 个频道移到「${target.name}」` : "已删除分组，保存后生效");
}

function cleanDraft(): CGroup[] {
  const names = new Set<string>();
  return draft.map((g, gi) => {
    let name = g.name.trim() || `分组 ${gi + 1}`;
    if (names.has(name)) {
      let n = 2;
      while (names.has(`${name} ${n}`)) n += 1;
      name = `${name} ${n}`;
    }
    names.add(name);
    return {
      id: g.id,
      name,
      channels: g.channels.map((ch, ci) => {
        const sources = ch.sources.map((s) => ({
          ...s,
          name: s.name.trim() || "来源",
          url: normalizeAddressText(s.url.trim()),
          enabled: s.enabled !== false,
        }));
        const usable = sources.filter((s) => s.enabled && s.url);
        if (usable.length) {
          const base = usable.find((s) => s.base) || usable[0];
          sources.forEach((s) => (s.base = s.id === base.id));
        } else {
          sources.forEach((s) => (s.base = false));
        }
        return {
          ...ch,
          name: ch.name.trim() || `频道 ${ci + 1}`,
          mode: ch.mode === "pairwise" ? "pairwise" : "base",
          sources,
        };
      }),
    };
  });
}

async function save() {
  if (!saveHandler) return;
  const next = cleanDraft();
  try {
    await saveHandler(next);
  } catch (e: any) {
    setMsg(`保存失败：${String(e)}`, true);
    return;
  }
  setMsg(`已保存 ${next.length} 组 / ${next.reduce((n, g) => n + g.channels.length, 0)} 频道`, false);
  window.setTimeout(close, 350);
}

export function openConsistEditor(groups: CGroup[], onSave: (next: CGroup[]) => Promise<void> | void) {
  ensure();
  draft = JSON.parse(JSON.stringify(groups)) as CGroup[];
  if (!draft.length) draft = [{ id: uid(), name: "默认分组", channels: [] }];
  saveHandler = onSave;
  filter = "";
  groupFilter = "all";
  const search = $<HTMLInputElement>("cge-search");
  if (search) search.value = "";
  setMsg("");
  const overlay = $<HTMLElement>(overlayId);
  if (overlay) overlay.hidden = false;
  const body = $<HTMLElement>("cge-body");
  if (body) body.scrollTop = 0;
  render();
  window.setTimeout(() => search?.focus(), 30);
}

function close() {
  const overlay = $<HTMLElement>(overlayId);
  if (overlay) overlay.hidden = true;
}
