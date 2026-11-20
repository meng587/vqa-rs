//! 告警处理页：告警队列、证据回放、曲线/差异段、人工处理与存储设置。
//!
//! 告警由一致性页在异常时写入 `data/alarm/alarms.json`，证据片段异步录制。
//! 本页只负责筛选、展示和人工闭环；检测本身在一致性页。

import { AlarmRecord, AlarmSettings, api, BadSeg, downloadUrl, isWebMode, webDownloadUrl } from "./api";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T | null;
const esc = (s: unknown) =>
  String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

const STATUS_LABEL: Record<string, string> = {
  pending: "待处理",
  doing: "处理中",
  done: "已处理",
  false: "误报",
};
const SEV_LABEL: Record<string, string> = { high: "严重", mid: "中等", low: "轻微" };

let alarms: AlarmRecord[] = [];
let filter = "all";
let selId = "";
let query = "";
let rangeHours = 24;
let autoClean = true;
let lastSettings: AlarmSettings | null = null;
let pollTimer = 0;
let loading = false;
let alarmViewActive = false;

function filtered(): AlarmRecord[] {
  const q = query.trim().toLowerCase();
  const now = Date.now();
  const windowMs = rangeHours > 0 ? rangeHours * 3600_000 : 0;
  return alarms.filter((r) => {
    if (filter !== "all" && r.status !== filter) return false;
    if (windowMs > 0) {
      const t = Date.parse(String(r.last_time || "").replace(" ", "T"));
      if (Number.isFinite(t) && now - t > windowMs) return false;
    }
    if (q) {
      const hay = `${r.id} ${r.channel} ${r.pair} ${r.kind} ${r.detail} ${r.source} ${r.reference}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}
function selected(): AlarmRecord | undefined {
  return alarms.find((r) => r.id === selId);
}
function statusLabel(s: string): string {
  return STATUS_LABEL[s] || s || "待处理";
}
function sevLabel(s: string): string {
  return SEV_LABEL[s] || "中等";
}
function fmtBytes(n: number): string {
  if (!n || n < 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MB`;
  return `${(n / 1073741824).toFixed(2)} GB`;
}
function shortTime(t: string): string {
  const parts = String(t || "").split(" ");
  return parts.length > 1 ? `${parts[0].slice(5)} ${parts[1].slice(0, 5)}` : String(t || "—");
}
function flash(text: string) {
  const el = $("al-qsub");
  if (!el) return;
  el.textContent = text;
  const base = `${filtered().length} 条 · 按最近发生时间排序`;
  window.setTimeout(() => {
    if (el.textContent === text) el.textContent = base;
  }, 3600);
}

function renderTabs() {
  const counts: Record<string, number> = { all: alarms.length, pending: 0, doing: 0, done: 0, false: 0 };
  for (const r of alarms) if (counts[r.status] !== undefined) counts[r.status] += 1;
  document.querySelectorAll<HTMLElement>("#al-tabs button").forEach((b) => {
    const f = b.dataset.filter || "all";
    b.classList.toggle("is-on", f === filter);
    const n = b.querySelector("b");
    if (n) n.textContent = String(counts[f] ?? 0);
  });
  const badge = $("cg-alarmcount");
  if (badge) {
    const pending = counts.pending ?? 0;
    badge.textContent = String(pending);
    badge.hidden = pending === 0;
  }
}

function renderList() {
  renderTabs();
  const host = $("al-list");
  if (!host) return;
  const list = filtered();
  const sub = $("al-qsub");
  if (sub) sub.textContent = `${list.length} 条 · 按最近发生时间排序`;
  if (!list.length) {
    host.innerHTML = `<div class="al-empty" style="height:180px">当前筛选下没有告警</div>`;
    return;
  }
  host.innerHTML = list
    .map((r) => {
      const clip =
        r.clip_state === "ready"
          ? `<span class="rec">● 录像 ${r.clip_secs || ""}s</span>`
          : r.clip_state === "recording"
            ? `<span class="rec" style="color:var(--warn)">● 录制中</span>`
            : "";
      return `<div class="al-row${r.id === selId ? " sel" : ""}" data-id="${esc(r.id)}">
        <span class="al-sev ${esc(r.severity)}"></span>
        <div class="al-rmain">
          <div class="al-r1"><b>${esc(r.channel || "未命名频道")}</b><em>${esc(r.kind || "不一致")}</em></div>
          <div class="al-r2">${esc(r.pair || r.detail || "—")}</div>
          <div class="al-r3"><span>${esc(shortTime(r.last_time))}</span><span>相似 ${(r.similarity * 100).toFixed(1)}%</span><span>${r.offset_ms > 0 ? "+" : ""}${r.offset_ms}ms</span>${clip}</div>
        </div>
        <div class="al-rstat"><span class="st-${esc(r.status)}">${esc(statusLabel(r.status))}</span><span>${esc(sevLabel(r.severity))}</span></div>
      </div>`;
    })
    .join("");
  host.onclick = (e) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>("[data-id]");
    if (!row) return;
    selId = row.dataset.id || "";
    renderList();
    renderDetail();
  };
}

function renderDetail() {
  const host = $("al-detail");
  if (!host) return;
  const r = selected();
  if (!r) {
    host.innerHTML = `<div class="al-empty">选择左侧告警查看详情与人工处理</div>`;
    return;
  }
  const clipReady = r.clip_state === "ready" && r.clip_path;
  const clipActions = isWebMode
    ? `<button class="al-vbtn" id="al-vdl">下载片段</button>`
    : `<button class="al-vbtn" id="al-vplay">回放</button><button class="al-vbtn" id="al-vdir">打开目录</button>`;
  const clipLine = clipReady
    ? `<div class="al-vstrip"><div class="al-vcam">▶</div><div class="al-vmain"><b>证据片段 · ${r.clip_secs || 12}s · ${fmtBytes(r.clip_size)}</b><span title="${esc(r.clip_path)}">${esc(r.clip_path)}</span></div>${clipActions}</div>`
    : r.clip_state === "recording"
      ? `<div class="al-vstrip"><div class="al-vcam" style="color:var(--warn)">●</div><div class="al-vmain"><b>证据片段正在录制</b><span>完成后可在这里回放</span></div></div>`
      : r.clip_state === "failed"
        ? `<div class="al-vstrip"><div class="al-vcam" style="color:var(--bad)">!</div><div class="al-vmain"><b>证据片段录制失败</b><span>来源不可读或 ffmpeg 异常，可稍后重跑该对</span></div></div>`
        : `<div class="al-vstrip" style="border-color:var(--border);background:rgba(255,255,255,.02)"><div class="al-vcam" style="background:rgba(255,255,255,.06);color:var(--faint)">○</div><div class="al-vmain"><b>没有证据片段</b><span>该告警没有留存录像（可能是旧记录或无信号）</span></div></div>`;

  const segs = (r.bad_segments || []).length
    ? (r.bad_segments || [])
        .map((s: BadSeg) => {
          const dur = Math.max(0, s.to_s - s.from_s);
          return `<div style="display:flex;align-items:center;gap:10px;padding:5px 10px;border-top:1px solid var(--border);font:8.5px var(--mono);color:var(--faint)">
            <span style="color:var(--warn)">${s.from_s.toFixed(1)}s – ${s.to_s.toFixed(1)}s</span>
            <span>连续 ${Math.max(2, Math.round(dur * (r.fps || 10)))} 帧</span>
            <span style="margin-left:auto">最低 ${(s.sim * 100).toFixed(1)}%</span>
          </div>`;
        })
        .join("")
    : `<div style="padding:10px;color:var(--ok);font-size:9.5px">未记录差异段</div>`;

  const history = (r.history || [])
    .slice(-5)
    .reverse()
    .map(
      (h) => {
        const action = h.action.startsWith("日志：")
          ? `日志 ${h.action.split(/[\\/]/).pop()}`
          : h.action;
        return `<div class="al-hitem"><b title="${esc(h.action)}">${esc(action)}</b><span>${esc(h.time)} · ${esc(h.who || "系统")}</span></div>`;
      },
    )
    .join("") || `<div class="al-hitem"><b>暂无处理记录</b></div>`;

  const claim =
    r.status === "pending"
      ? `<button id="al-claim">认领</button>`
      : `<button id="al-claim" disabled style="opacity:.45;cursor:default">已认领</button>`;

  host.innerHTML = `
    <article class="al-dcard">
      <div class="al-dhead">
        <b>${esc(r.channel || "未命名频道")} · ${esc(r.kind || "不一致")}</b>
        <em>${esc(sevLabel(r.severity))}</em>
        <span class="st">${esc(statusLabel(r.status))}</span>
        ${claim}
      </div>
      <div class="al-dmeta">
        <span>${esc(r.id)}</span><span>首次 ${esc(r.first_time || "—")}</span><span>最近 ${esc(r.last_time || "—")}</span>
        <span>${r.segment_count || 0} 段</span><span>帧率 ${r.fps || 10}fps</span>
      </div>
      <div class="al-ev">
        <div class="al-evhd"><b>比对曲线</b><span>平均 ${(r.similarity * 100).toFixed(1)}% · 最低 ${(r.min_sim * 100).toFixed(1)}% · 偏移 ${r.offset_ms > 0 ? "+" : ""}${r.offset_ms}ms</span><span class="r">${esc(r.pair || "")}</span></div>
        <div class="al-evcurve"><canvas id="al-curve"></canvas></div>
        ${clipLine}
      </div>
    </article>
    <article class="al-dcard">
      <div class="al-dhead"><b>差异段</b><span style="font-size:8.5px;color:var(--faint)">低于阈值且连续 ≥2 帧</span><span style="margin-left:auto;font:8.5px var(--mono);color:var(--faint)">${r.segment_count || 0} 段</span></div>
      ${segs}
    </article>
    <article class="al-dcard">
      <div class="al-dhead"><b>人工处理</b><span style="font-size:8.5px;color:var(--faint)">确认后闭环，处理记录随告警留存</span></div>
      <div class="al-handbody">
        <div>
          <div class="al-field"><label>处理状态</label><div class="al-chips" id="al-statuschips">
            <button class="al-chip${r.status === "pending" ? " on" : ""}" data-st="pending">待处理</button>
            <button class="al-chip${r.status === "doing" ? " on" : ""}" data-st="doing">处理中</button>
            <button class="al-chip${r.status === "done" ? " on" : ""}" data-st="done">已处理</button>
            <button class="al-chip${r.status === "false" ? " on" : ""}" data-st="false">误报</button>
          </div></div>
          <div class="al-field"><label>处理人</label><input class="al-input" id="al-assignee" value="${esc(r.assignee || "本机用户")}" /></div>
          <div class="al-field"><label>结论</label><div class="al-chips" id="al-conclchips">
            ${["确认故障", "链路抖动", "设备异常", "误报", "待复核"]
              .map((c) => `<button class="al-chip${r.conclusion === c ? " on" : ""}" data-concl="${c}">${c}</button>`)
              .join("")}
          </div></div>
          <div class="al-field"><label>备注</label><textarea class="al-textarea" id="al-note" placeholder="例如：已切换备用链路，观察 10 分钟">${esc(r.note || "")}</textarea></div>
        </div>
        <div class="al-hist">${history}</div>
      </div>
      <div class="al-hfoot"><span style="font-size:8.5px;color:var(--faint);margin-right:auto">来源：${esc(r.source || "—")}</span><button class="save" id="al-save">保存处理</button></div>
    </article>`;

  requestAnimationFrame(() => drawCurve(r));

  $("al-claim")?.addEventListener("click", async () => {
    try {
      await api.alarmUpdate(r.id, "doing", r.conclusion, "本机用户", r.note);
      await refresh();
      flash("已认领，进入处理中");
    } catch (e: any) {
      flash(`认领失败：${e}`);
    }
  });

  let statusDraft = r.status || "pending";
  $("al-statuschips")?.querySelectorAll<HTMLElement>("[data-st]").forEach((b) => {
    b.onclick = () => {
      statusDraft = b.dataset.st || "pending";
      $("al-statuschips")?.querySelectorAll<HTMLElement>("[data-st]").forEach((x) => x.classList.toggle("on", x === b));
    };
  });
  let conclDraft = r.conclusion || "";
  $("al-conclchips")?.querySelectorAll<HTMLElement>("[data-concl]").forEach((b) => {
    b.onclick = () => {
      conclDraft = b.dataset.concl || "";
      if (conclDraft === "误报") {
        statusDraft = "false";
        $("al-statuschips")?.querySelectorAll<HTMLElement>("[data-st]").forEach((x) =>
          x.classList.toggle("on", x.dataset.st === "false"),
        );
      }
      $("al-conclchips")?.querySelectorAll<HTMLElement>("[data-concl]").forEach((x) => x.classList.toggle("on", x === b));
    };
  });
  $("al-save")?.addEventListener("click", async () => {
    const assignee = ($<HTMLInputElement>("al-assignee")?.value || "").trim();
    const note = ($<HTMLTextAreaElement>("al-note")?.value || "").trim();
    const btn = $<HTMLButtonElement>("al-save");
    if (btn) {
      btn.disabled = true;
      btn.textContent = "保存中…";
    }
    try {
      await api.alarmUpdate(r.id, statusDraft, conclDraft, assignee, note);
      await refresh();
      flash("处理已保存");
    } catch (e: any) {
      flash(`保存失败：${e}`);
      if (btn) {
        btn.disabled = false;
        btn.textContent = "保存处理";
      }
    }
  });
  $("al-vplay")?.addEventListener("click", () => {
    if (!r.clip_path) return;
    api.alarmOpen(r.clip_path).catch((e) => flash(`回放失败：${e}`));
  });
  $("al-vdir")?.addEventListener("click", () => {
    if (!r.clip_path) return;
    api.reveal(r.clip_path).catch((e) => flash(`打开目录失败：${e}`));
  });
  $("al-vdl")?.addEventListener("click", () => {
    if (!r.clip_path) return;
    downloadUrl(webDownloadUrl.alarm(r.id));
  });
}

function drawCurve(r: AlarmRecord) {
  const cv = $<HTMLCanvasElement>("al-curve");
  if (!cv) return;
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth || 700;
  const h = cv.clientHeight || 68;
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
  const GUT = 34;
  const PR = 8;
  const TOP = 10;
  const BOT = h - 14;
  const pw = w - GUT - PR;
  const ph = BOT - TOP;
  const arr = r.series_sim || [];
  ctx.strokeStyle = "rgba(255,255,255,.06)";
  ctx.font = "8px Consolas, monospace";
  ctx.fillStyle = "#CBBDA4";
  ctx.textAlign = "right";
  for (const t of [1, 0.5, 0]) {
    const y = TOP + (1 - t) * ph;
    ctx.beginPath();
    ctx.moveTo(GUT, y);
    ctx.lineTo(w - PR, y);
    ctx.stroke();
    ctx.fillText(`${Math.round(t * 100)}%`, GUT - 4, y + 3);
  }
  if (arr.length < 4) {
    ctx.fillStyle = "#CBBDA4";
    ctx.textAlign = "center";
    ctx.font = "9px Segoe UI, Microsoft YaHei, sans-serif";
    ctx.fillText(arr.length ? "曲线采样点不足" : "这条告警没有保存曲线（旧记录）", GUT + pw / 2, h / 2);
    return;
  }
  ctx.textAlign = "left";
  for (const s of r.bad_segments || []) {
    const x0 = GUT + (pw * s.from_s) / Math.max(0.1, arr.length / (r.fps || 10));
    const x1 = GUT + (pw * s.to_s) / Math.max(0.1, arr.length / (r.fps || 10));
    if (x1 > x0) {
      ctx.fillStyle = "rgba(248,113,113,.13)";
      ctx.fillRect(x0, TOP, x1 - x0, ph);
    }
  }
  ctx.strokeStyle = "rgba(168,212,189,.95)";
  ctx.lineWidth = 1.6;
  ctx.beginPath();
  arr.forEach((v, i) => {
    const x = GUT + (pw * i) / Math.max(1, arr.length - 1);
    const y = BOT - Math.max(0, Math.min(1, v)) * ph;
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.stroke();
  // 底部时间刻度，避免只有 Y 轴没有横轴参照
  ctx.fillStyle = "#CBBDA4";
  ctx.font = "8px Consolas, monospace";
  ctx.textAlign = "center";
  const totalS = arr.length / (r.fps || 10);
  for (let i = 0; i <= 2; i++) {
    const t = (totalS * i) / 2;
    ctx.fillText(`${t.toFixed(1)}s`, GUT + (pw * i) / 2, h - 3);
  }
}

async function refresh(silent = false) {
  if (loading) return;
  loading = true;
  try {
    const list = await api.alarmList();
    alarms = list || [];
    if (selId && !alarms.some((r) => r.id === selId)) selId = "";
    if (!selId) {
      const first = filtered()[0];
      if (first) selId = first.id;
    }
    renderList();
    renderDetail();
    if (!silent) flash(`已刷新 ${alarms.length} 条告警`);
  } catch (e: any) {
    flash(`读取告警失败：${e}`);
  } finally {
    loading = false;
  }
}

async function exportRecords() {
  if (!alarms.length) {
    flash("没有可导出的告警");
    return;
  }
  try {
    const p = await api.alarmExport(JSON.stringify(alarms, null, 2));
    flash(p ? `已导出：${p}` : "已取消");
  } catch (e: any) {
    flash(`导出失败：${e}`);
  }
}

async function batchClaim() {
  const list = filtered().filter((r) => r.status === "pending");
  if (!list.length) {
    flash("当前筛选下没有待处理告警");
    return;
  }
  try {
    for (const r of list) {
      await api.alarmUpdate(r.id, "doing", r.conclusion, "本机用户", r.note);
    }
    await refresh();
    flash(`已批量认领 ${list.length} 条`);
  } catch (e: any) {
    flash(`批量认领失败：${e}`);
  }
}

async function openSettings() {
  try {
    lastSettings = await api.alarmSettings();
    fillSettings(lastSettings);
    $("al-mask-set")?.classList.add("on");
  } catch (e: any) {
    flash(`读取存储设置失败：${e}`);
  }
}
function fillSettings(s: AlarmSettings) {
  const set = (id: string, v: string) => {
    const el = $<HTMLInputElement>(id);
    if (el) el.value = v;
  };
  set("al-logdir", s.log_dir);
  set("al-clipdir", s.clip_dir);
  set("al-days", String(s.retain_days));
  set("al-maxgb", String(s.max_gb));
  autoClean = !!s.auto_clean;
  $("al-autoclean")?.classList.toggle("on", autoClean);
  const usage = $("al-usage");
  if (usage) usage.textContent = fmtBytes((s.log_size || 0) + (s.clip_size || 0));
  const detail = $("al-usagedetail");
  if (detail) detail.textContent = `日志 ${fmtBytes(s.log_size || 0)} · 录像 ${fmtBytes(s.clip_size || 0)}`;
}
function closeSettings() {
  $("al-mask-set")?.classList.remove("on");
}
async function saveSettings() {
  const logDir = ($<HTMLInputElement>("al-logdir")?.value || "").trim();
  const clipDir = ($<HTMLInputElement>("al-clipdir")?.value || "").trim();
  const days = Number(($<HTMLInputElement>("al-days")?.value || "90").trim());
  const maxGb = Number(($<HTMLInputElement>("al-maxgb")?.value || "50").trim());
  if (!logDir || !clipDir) {
    flash("日志和录像路径都要填");
    return;
  }
  const btn = $<HTMLButtonElement>("al-set-save");
  if (btn) {
    btn.disabled = true;
    btn.textContent = "保存中…";
  }
  try {
    const s = await api.setAlarmSettings({
      logDir,
      clipDir,
      retainDays: Number.isFinite(days) ? Math.max(1, Math.round(days)) : 90,
      maxGb: Number.isFinite(maxGb) ? Math.max(1, maxGb) : 50,
      autoClean,
    });
    lastSettings = s;
    fillSettings(s);
    closeSettings();
    flash("存储设置已保存");
  } catch (e: any) {
    flash(`保存设置失败：${e}`);
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = "保存";
    }
  }
}

export function initAlarm() {
  document.querySelectorAll<HTMLElement>("#al-tabs button").forEach((b) => {
    b.onclick = () => {
      filter = b.dataset.filter || "all";
      selId = "";
      renderList();
      const first = filtered()[0];
      if (first) {
        selId = first.id;
        renderDetail();
      } else {
        renderDetail();
      }
    };
  });
  $<HTMLInputElement>("al-query")?.addEventListener("input", (e) => {
    query = (e.target as HTMLInputElement).value;
    renderList();
  });
  $<HTMLSelectElement>("al-range")?.addEventListener("change", (e) => {
    rangeHours = Number((e.target as HTMLSelectElement).value) || 0;
    renderList();
    const first = filtered()[0];
    if (first) {
      selId = first.id;
      renderDetail();
    }
  });
  $("al-export")?.addEventListener("click", exportRecords);
  $("al-settings")?.addEventListener("click", openSettings);
  $("al-batch")?.addEventListener("click", batchClaim);
  $("al-processing")?.addEventListener("click", () => {
    let r = selected();
    if (!r) {
      r = filtered()[0];
      if (r) {
        selId = r.id;
        renderList();
        renderDetail();
      }
    }
    document.querySelector(".al-handbody")?.scrollIntoView({ behavior: "smooth", block: "start" });
    $<HTMLInputElement>("al-assignee")?.focus();
  });

  $("al-set-x")?.addEventListener("click", closeSettings);
  $("al-set-cancel")?.addEventListener("click", closeSettings);
  $("al-set-save")?.addEventListener("click", saveSettings);
  $("al-mask-set")?.addEventListener("click", (e) => {
    if (e.target === $("al-mask-set")) closeSettings();
  });
  $("al-autoclean")?.addEventListener("click", () => {
    autoClean = !autoClean;
    $("al-autoclean")?.classList.toggle("on", autoClean);
  });
  $("al-set-reset")?.addEventListener("click", () => {
    if (!lastSettings) return;
    fillSettings({ ...lastSettings, retain_days: 90, max_gb: 50, auto_clean: true, log_dir: lastSettings.default_log_dir, clip_dir: lastSettings.default_clip_dir });
  });
  $("al-logbrowse")?.addEventListener("click", async () => {
    const p = await api.browseFolder("选择检测日志保存目录", ($<HTMLInputElement>("al-logdir")?.value || "").trim());
    if (p) ($<HTMLInputElement>("al-logdir")!).value = p;
  });
  $("al-clipbrowse")?.addEventListener("click", async () => {
    const p = await api.browseFolder("选择故障视频保存目录", ($<HTMLInputElement>("al-clipdir")?.value || "").trim());
    if (p) ($<HTMLInputElement>("al-clipdir")!).value = p;
  });
  $("al-logopen")?.addEventListener("click", () => {
    const p = ($<HTMLInputElement>("al-logdir")?.value || "").trim();
    if (p) api.reveal(p).catch((e) => flash(`打开目录失败：${e}`));
  });
  $("al-clipopen")?.addEventListener("click", () => {
    const p = ($<HTMLInputElement>("al-clipdir")?.value || "").trim();
    if (p) api.reveal(p).catch((e) => flash(`打开目录失败：${e}`));
  });

  if (isWebMode) {
    for (const id of ["al-logbrowse", "al-clipbrowse", "al-logopen", "al-clipopen"]) {
      $(id)?.setAttribute("hidden", "");
    }
    for (const id of ["al-logdir", "al-clipdir"]) {
      const input = $<HTMLInputElement>(id);
      if (input) {
        input.placeholder = "填写工作站本机绝对路径";
        input.title = "Web 端不能打开工作站文件夹，请直接填写工作站上的路径";
      }
    }
  }

  window.addEventListener("alarms-changed", () => {
    refresh(true);
  });

  // 证据录像在后台线程回填状态，告警子页显示时定期刷一次（保留当前选中）
  pollTimer = window.setInterval(() => {
    if (alarmViewActive && !document.querySelector<HTMLElement>(".al-mask.on")) {
      refresh(true);
    }
  }, 6000);

  refresh(true);
}

/** 一致性页切到告警子视图时调用；离开时传 false。 */
export function setAlarmViewActive(active: boolean) {
  alarmViewActive = active;
  if (active) refresh(true);
}

export function leaveAlarm() {
  if (pollTimer) {
    window.clearInterval(pollTimer);
    pollTimer = 0;
  }
}

