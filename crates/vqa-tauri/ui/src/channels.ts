//! 通用频道列表：两页共用一份配置，支持单检 / 轮巡 / 预览。
//!
//! 页面只提供「怎么跑一条」和「怎么预览一条」，列表本身的状态、选中、
//! 增删、持久化都在这里，避免两页各写一套。

import { api, Channel } from "./api";
import { normalizeAddressText } from "./address";
import {
  ALL_GROUPS,
  DEFAULT_GROUP,
  groupMatches,
  groupNames,
  groupOf,
  loadChannelGroups,
  normalizeGroup,
} from "./channel-groups";
import { openChannelEditor } from "./channel-editor";

export interface RunOutcome {
  ok: boolean;
  summary: string;
  /** 检出问题但不属于「跑失败」，行状态显示异常（黄） */
  warn?: boolean;
  err?: string;
}

export interface ChannelHooks {
  /** 预览槽位名：av / consist */
  owner: "av" | "consist";
  /** 列表标题 */
  title?: string;
  /** 初始并发度（1 = 逐条） */
  conc?: number;
  /** 单机顺序检测：固定逐条执行，不显示并发设置 */
  sequential?: boolean;
  /** 左侧窄栏模式：只保留编辑/预览/检测，增删改走列表编辑页 */
  compact?: boolean;
  /** 窄栏模式下收起整个频道栏 */
  onCollapse?: () => void;
  /** 并发度变化时回调（页面负责持久化） */
  onConc?: (n: number) => void;
  /** 额外动作（如一致性页的「设为参考」） */
  extra?: { label: string; run: (ch: Channel) => void };
  /** 跑一条频道；job 是后端并发槽位 id，focus 表示它负责刷新页面的 KPI/曲线 */
  runOne: (ch: Channel, job: string, focus: boolean) => Promise<RunOutcome>;
  /** 中止当前检测 */
  stopRun: () => void;
  /** 预览一条频道（页面负责起帧轮询） */
  preview: (ch: Channel) => void;
  /** 停止预览 */
  stopPreview: () => void;
}

type RowState = "idle" | "run" | "ok" | "warn" | "err";

const STATE_TEXT: Record<RowState, string> = {
  idle: "待检",
  run: "检测中",
  ok: "通过",
  warn: "异常",
  err: "失败",
};

export class ChannelBoard {
  private chans: Channel[] = [];
  private sel = -1;
  private busy = false;
  private stopFlag = false;
  private editMode: "none" | "add" | "import" = "none";
  private conc = 1;
  private groupFilter = ALL_GROUPS;
  private groups: string[] = [DEFAULT_GROUP];
  private host: HTMLElement;
  private hooks: ChannelHooks;

  constructor(host: HTMLElement, hooks: ChannelHooks) {
    this.host = host;
    this.hooks = hooks;
    this.conc = hooks.sequential ? 1 : clampConc(hooks.conc ?? 1);
  }

  setConc(n: number) {
    if (this.hooks.sequential) return;
    this.conc = clampConc(n);
    const inp = this.host.querySelector<HTMLInputElement>('[data-role="conc"]');
    if (inp) inp.value = String(this.conc);
  }

  getConc(): number {
    return this.conc;
  }

  async load() {
    this.sel = -1;
    try {
      const list = await api.channelsGet();
      // 老版本重复导入留下的同地址行在这里合并掉
      const seen = new Set<string>();
      this.chans = (list || [])
        .map((c) => ({
          name: c.name || "",
          url: c.url || "",
          group: normalizeGroup(c.group),
          enabled: c.enabled !== false,
        }))
        .filter((c) => {
          const k = c.url.trim().toLowerCase();
          if (!k || seen.has(k)) return false;
          seen.add(k);
          return true;
        });
      this.groups = await loadChannelGroups(this.chans);
    } catch {
      this.chans = [];
      this.groups = [DEFAULT_GROUP];
    }
    this.build();
  }

  private build() {
    const compact = this.hooks.compact === true;
    const concCtl = this.hooks.sequential
      ? ""
      : `<label class="chconc" title="同时检测的频道数；1 = 逐条，检测时预览框跟第一个开跑的频道">并发 <input type="number" min="1" max="8" step="1" data-role="conc" value="${this.conc}" /></label>`;
    const toolbar = compact
      ? `
      <div class="chbar is-compact">
        <div class="chttlrow"><b class="chttl">${this.hooks.title || "频道列表"}</b><span class="chcount">0</span>${this.hooks.onCollapse ? `<button class="chcollapse" data-a="collapse" type="button" title="收起频道列表">收起 <span aria-hidden="true">›</span></button>` : ""}</div>
        <div class="chsub">
          ${concCtl}
          <label class="chgroupfilter" title="只显示并轮巡当前分组">
            分组 <select data-role="group"></select>
          </label>
        </div>
        <div class="chacts">
          <button class="cbtn" data-a="edit">编辑列表</button>
          <button class="cbtn" data-a="one">检测选中</button>
          <button class="gradbtn csm" data-a="all">轮巡检测</button>
          <button class="cbtn" data-a="stop" hidden>停止</button>
        </div>
      </div>`
      : `
      <div class="chbar">
        <b class="chttl">${this.hooks.title || "频道列表"}</b>
        <span class="chcount">0</span>
        <label class="chgroupfilter" title="只显示并轮巡当前分组">
          分组 <select data-role="group"></select>
        </label>
        ${concCtl}
        <span class="chsp"></span>
        <button class="cbtn" data-a="add">添加</button>
        <button class="cbtn" data-a="import">批量导入</button>
        <button class="cbtn" data-a="del">删除选中</button>
        <button class="cbtn" data-a="prev">预览</button>
        ${this.hooks.extra ? `<button class="cbtn" data-a="extra">${this.hooks.extra.label}</button>` : ""}
        <button class="cbtn" data-a="one">检测选中</button>
        <button class="gradbtn csm" data-a="all">轮巡检测</button>
        <button class="cbtn" data-a="stop" hidden>停止</button>
      </div>`;
    this.host.innerHTML = `
      ${toolbar}
      <div class="chimp" data-role="import" hidden>
        <textarea class="chta" placeholder="每行一个频道：名称,地址&#10;例如：CCTV-1 综合,udp://@239.1.1.1:5000&#10;只填地址也可以，名称自动补"></textarea>
        <div class="chimpa">
          <button class="cbtn" data-a="import-ok">导入</button>
          <button class="cbtn" data-a="import-no">取消</button>
        </div>
      </div>
      <div class="chadd" data-role="add" hidden>
        <input class="chni" data-role="name" placeholder="频道名（可留空）" />
        <input class="chni" data-role="url" placeholder="地址：本地文件 / udp://@239.x.x.x:port" />
        <button class="cbtn" data-a="add-ok">添加</button>
        <button class="cbtn" data-a="add-no">取消</button>
      </div>
      <div class="chtable">
        <div class="chrow chead">
          <label class="chk"><input type="checkbox" data-a="allbox" /></label>
          <span>频道</span><span>地址</span><span>状态</span><span>结果</span>
        </div>
        <div class="chrows" data-role="rows"></div>
      </div>`;
    this.bindBar();
    this.renderRows();
    window.addEventListener("channels-changed", () => void this.load());
  }

  private q<T extends HTMLElement>(sel: string) {
    return this.host.querySelector(sel) as T | null;
  }

  private bindBar() {
    // 事件挂在整块上：工具栏、添加框、导入框里的按钮都要能点
    this.host.addEventListener("click", (ev) => {
      const btn = (ev.target as HTMLElement).closest("button") as HTMLButtonElement | null;
      if (!btn) return;
      const a = btn.dataset.a;
      if (a === "add") this.showEdit("add");
      else if (a === "import") this.showEdit("import");
      else if (a === "add-no") this.showEdit("none");
      else if (a === "import-no") this.showEdit("none");
      else if (a === "add-ok") this.commitAdd();
      else if (a === "import-ok") this.commitImport();
      else if (a === "del") this.deleteSelected();
      else if (a === "edit") void openChannelEditor();
      else if (a === "prev") this.previewSelected();
      else if (a === "one") this.runSelected();
      else if (a === "all") this.runAll();
      else if (a === "stop") this.stopAll();
      else if (a === "extra") this.extraSelected();
      else if (a === "collapse") this.hooks.onCollapse?.();
    });
    const allbox = this.q<HTMLInputElement>('[data-a="allbox"]');
    allbox?.addEventListener("change", () => {
      const on = allbox.checked;
      this.visibleRows().forEach(({ i }) => this.toggleEnabled(i, on, false));
      this.renderRows();
      this.persist();
    });
    const group = this.q<HTMLSelectElement>('[data-role="group"]');
    group?.addEventListener("change", () => {
      this.groupFilter = group.value || ALL_GROUPS;
      this.sel = -1;
      this.renderRows();
    });
    const conc = this.q<HTMLInputElement>('[data-role="conc"]');
    conc?.addEventListener("change", () => {
      this.conc = clampConc(Number(conc.value) || 1);
      conc.value = String(this.conc);
      this.hooks.onConc?.(this.conc);
    });
  }

  private showEdit(mode: "none" | "add" | "import") {
    this.editMode = mode;
    const add = this.q<HTMLElement>('[data-role="add"]');
    const imp = this.q<HTMLElement>('[data-role="import"]');
    if (add) add.hidden = mode !== "add";
    if (imp) imp.hidden = mode !== "import";
    if (mode === "add") this.q<HTMLInputElement>('[data-role="name"]')?.focus();
    if (mode === "import") this.q<HTMLTextAreaElement>(".chta")?.focus();
  }

  private commitAdd() {
    const name = (this.q<HTMLInputElement>('[data-role="name"]')?.value || "").trim();
    const url = normalizeAddressText((this.q<HTMLInputElement>('[data-role="url"]')?.value || "").trim());
    if (!url || this.hasUrl(url)) return;
    this.chans.push({
      name: name || `频道${this.chans.length + 1}`,
      url,
      group: this.groupFilter === ALL_GROUPS ? DEFAULT_GROUP : this.groupFilter,
      enabled: true,
    });
    if (this.q<HTMLInputElement>('[data-role="name"]')) this.q<HTMLInputElement>('[data-role="name"]')!.value = "";
    if (this.q<HTMLInputElement>('[data-role="url"]')) this.q<HTMLInputElement>('[data-role="url"]')!.value = "";
    this.showEdit("none");
    this.renderRows();
    this.persist();
  }

  private commitImport() {
    const ta = this.q<HTMLTextAreaElement>(".chta");
    const txt = (ta?.value || "").trim();
    if (!txt) return;
    const lines = txt.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
    let added = 0;
    for (const line of lines) {
      let name = "";
      let url = line;
      const m = line.match(/^(.+?)[,，\t]\s*((?:udp|rtp|rtsp|http|https|file):.*|\S.*)$/);
      if (m) {
        name = m[1].trim();
        url = normalizeAddressText(m[2].trim());
      } else {
        url = normalizeAddressText(url);
      }
      if (!url || this.hasUrl(url)) continue;
      this.chans.push({
        name: name || `频道${this.chans.length + 1}`,
        url,
        group: this.groupFilter === ALL_GROUPS ? DEFAULT_GROUP : this.groupFilter,
        enabled: true,
      });
      added += 1;
    }
    if (ta) ta.value = "";
    this.showEdit("none");
    this.renderRows();
    if (added) this.persist();
  }

  private hasUrl(url: string): boolean {
    const k = url.trim().toLowerCase();
    return this.chans.some((c) => c.url.trim().toLowerCase() === k);
  }

  private deleteSelected() {
    if (this.sel < 0) return;
    this.chans.splice(this.sel, 1);
    this.sel = -1;
    this.renderRows();
    this.persist();
  }

  private toggleEnabled(i: number, on: boolean, rerender = true) {
    if (!this.chans[i]) return;
    this.chans[i].enabled = on;
    if (rerender) this.renderRows();
  }

  private visibleRows(): { ch: Channel; i: number }[] {
    return this.chans
      .map((ch, i) => ({ ch, i }))
      .filter(({ ch }) => groupMatches(ch, this.groupFilter));
  }

  private updateCount(list = this.visibleRows()) {
    const cnt = this.q<HTMLElement>(".chcount");
    if (!cnt) return;
    const on = list.filter(({ ch }) => ch.enabled).length;
    cnt.textContent =
      this.groupFilter === ALL_GROUPS
        ? `${this.chans.length} 个 / 启用 ${on}`
        : `${list.length} 个 · ${this.groupFilter} / 启用 ${on}`;
  }

  private renderGroupSelect() {
    const sel = this.q<HTMLSelectElement>('[data-role="group"]');
    if (!sel) return;
    this.groups = groupNames(this.chans, this.groups);
    if (this.groupFilter !== ALL_GROUPS && !this.groups.includes(this.groupFilter)) {
      this.groupFilter = ALL_GROUPS;
    }
    sel.innerHTML =
      `<option value="${ALL_GROUPS}">全部分组</option>` +
      this.groups.map((g) => `<option value="${esc(g)}">${esc(g)}</option>`).join("");
    sel.value = this.groupFilter;
  }

  private renderRows() {
    const rows = this.q<HTMLElement>('[data-role="rows"]');
    if (!rows) return;
    this.renderGroupSelect();
    const list = this.visibleRows();
    rows.innerHTML = list.length
      ? list
          .map(({ ch: c, i }) => {
            const st: RowState = "idle";
            return (
              `<div class="chrow${i === this.sel ? " sel" : ""}" data-i="${i}">` +
              `<label class="chk"><input type="checkbox" data-i="${i}" ${c.enabled ? "checked" : ""} /></label>` +
              `<span class="chname" title="${esc(c.name)} · ${esc(groupOf(c))}"><span class="chname-t">${esc(c.name || "未命名")}</span><i class="chgroup">${esc(groupOf(c))}</i></span>` +
              `<span class="churl mono" title="${esc(c.url)}">${esc(c.url)}</span>` +
              `<span class="chst ${st}">${STATE_TEXT[st]}</span>` +
              `<span class="chsum mono">—</span>` +
              `</div>`
            );
          })
          .join("")
      : `<div class="ch-empty">${this.chans.length ? "当前分组暂无频道" : "还没有频道，点「编辑列表」添加"}</div>`;
    this.updateCount(list);
    const allbox = this.q<HTMLInputElement>('[data-a="allbox"]');
    if (allbox) {
      const on = list.filter(({ ch }) => ch.enabled).length;
      allbox.checked = list.length > 0 && on === list.length;
      allbox.indeterminate = on > 0 && on < list.length;
    }

    rows.querySelectorAll<HTMLInputElement>('input[type="checkbox"]').forEach((box) => {
      box.addEventListener("change", (ev) => {
        ev.stopPropagation();
        const i = Number(box.dataset.i);
        this.toggleEnabled(i, box.checked, false);
        this.updateCount();
        this.persist();
      });
    });
    rows.querySelectorAll<HTMLElement>(".chrow").forEach((row) => {
      row.addEventListener("click", (ev) => {
        if ((ev.target as HTMLElement).closest(".chk")) return;
        const i = Number(row.dataset.i);
        this.select(i, true);
      });
    });
    this.updateBar();
  }

  private updateBar() {
    this.host.querySelectorAll<HTMLElement>(".chrow").forEach((row) => {
      row.classList.toggle("sel", Number(row.dataset.i) === this.sel);
      row.classList.toggle("off", !this.chans[Number(row.dataset.i)]?.enabled);
    });
    const bar = this.q<HTMLElement>(".chbar");
    bar?.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
      const a = b.dataset.a;
      if (!a) return;
      if (a === "stop") {
        b.hidden = !this.busy;
        return;
      }
      const needsSel = a === "del" || a === "prev" || a === "one" || a === "extra";
      b.disabled = this.busy || (needsSel && this.sel < 0);
    });
    const conc = this.q<HTMLInputElement>('[data-role="conc"]');
    if (conc) conc.disabled = this.busy;
    const group = this.q<HTMLSelectElement>('[data-role="group"]');
    if (group) group.disabled = this.busy;
  }

  private select(i: number, preview: boolean) {
    this.sel = i;
    this.updateBar();
    if (preview) this.hooks.preview(this.chans[i]);
  }

  private previewSelected() {
    if (this.sel < 0) return;
    this.hooks.preview(this.chans[this.sel]);
  }

  private extraSelected() {
    if (this.sel < 0 || !this.hooks.extra) return;
    this.hooks.extra.run(this.chans[this.sel]);
  }

  async runSelected() {
    if (this.sel < 0 || this.busy) return;
    const i = this.sel;
    await this.runIndex([i]);
  }

  async runAll() {
    if (this.busy) return;
    const list = this.visibleRows()
      .map(({ ch, i }) => (ch.enabled && ch.url ? i : -1))
      .filter((i) => i >= 0);
    if (!list.length) return;
    await this.runIndex(list);
  }

  private async runIndex(list: number[]) {
    this.busy = true;
    this.stopFlag = false;
    this.updateBar();
    const conc = this.hooks.sequential ? 1 : Math.max(1, Math.min(this.conc, list.length));
    let next = 0;
    let previewOn = false;
    const worker = async (slot: number) => {
      for (;;) {
        if (this.stopFlag) break;
        const k = next++;
        if (k >= list.length) break;
        const i = list[k];
        const ch = this.chans[i];
        if (!ch) continue;
        this.setRow(i, "run", "准备…");
        // 并发时只有一个焦点任务刷 KPI/曲线、占预览、并保持选中；
        // 其余任务只回写自己那一行，避免选中行和指标跟着多路乱跳
        const focus = conc === 1 || !previewOn;
        if (focus) {
          previewOn = true;
          this.select(i, false);
          this.hooks.preview(ch);
        }
        let out: RunOutcome = { ok: false, summary: "未执行" };
        try {
          out = await this.hooks.runOne(ch, `${this.hooks.owner}-${slot}`, focus);
        } catch (e: any) {
          out = { ok: false, summary: String(e), err: String(e) };
        }
        this.setRow(i, out.ok ? "ok" : out.warn ? "warn" : "err", out.summary || (out.ok ? "完成" : "失败"));
      }
    };
    await Promise.all(Array.from({ length: conc }, (_, s) => worker(s)));
    this.busy = false;
    this.updateBar();
    this.hooks.stopPreview();
  }

  private stopAll() {
    this.stopFlag = true;
    this.hooks.stopRun();
    this.hooks.stopPreview();
  }

  setRow(i: number, st: RowState, text: string) {
    const row = this.host.querySelector<HTMLElement>(`.chrow[data-i="${i}"]`);
    if (!row) return;
    const stEl = row.querySelector<HTMLElement>(".chst");
    const sumEl = row.querySelector<HTMLElement>(".chsum");
    if (stEl) {
      stEl.className = `chst ${st}`;
      stEl.textContent = STATE_TEXT[st];
    }
    if (sumEl) sumEl.textContent = text || "—";
    row.classList.toggle("running", st === "run");
  }

  private persist() {
    api.channelsSave(this.chans).catch(() => {});
  }

  getChannels(): Channel[] {
    return this.chans;
  }
}

function esc(s: string): string {
  return (s || "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function clampConc(n: number): number {
  return Math.max(1, Math.min(8, Math.round(n) || 1));
}
