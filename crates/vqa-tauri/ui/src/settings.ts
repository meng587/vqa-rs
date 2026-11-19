//! 各功能自己的设置抽屉：播放器页齿轮 →「播放器设置」，分析页齿轮 →「视频分析设置」。
//! 每个功能各存一份配置，互不影响：
//!   播放器 → shared/settings.json 的 out_snap_dir / out_rec_dir / player.default_*
//!   分析   → analysis.report_dir / analysis.nic
//!   合规   → compliance.nic
//! 抽屉不切换页面，改完立刻对新截图/录像/导出生效。

import { api } from "./api";
import { icon } from "./icons";
import { drawerHold } from "./drawer";
import { setForceBridge } from "./flags";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

export function initSettings() {
  buildDrawers();
  wirePlayerDrawer();
  wireAnalysisDrawer();
  wireComplianceDrawer();
}

// ═══════════════════ 抽屉骨架 ═══════════════════

function buildDrawers() {
  const frag = document.createElement("div");
  frag.innerHTML = `
  <div class="drmask" id="dr-mask" hidden></div>

  <aside class="drawer" id="dr-player" hidden>
    <div class="drhead"><b>播放器设置</b><button class="iconbtn drx txtbtn" id="dr-p-x" title="关闭"></button></div>
    <div class="drbody">
      <div class="drsec">网络</div>
      <div class="setrow">
        <span class="k">组播网卡</span>
        <select id="p-nic" class="setnic"><option value="">自动</option></select>
      </div>
      <div class="drhint">自动 = 按系统路由选择；指定网卡后，UDP / RTP 组播取流会绑定该网卡。保存后作为默认，下次启动仍沿用；找不到该网卡时回到自动。</div>
      <div class="drsec">输出目录</div>
      <div class="setrow">
        <span class="k">截图</span>
        <input id="dr-p-snap" spellcheck="false" />
        <button class="iconbtn txtbtn" id="dr-p-snapb" title="选择文件夹…"></button>
      </div>
      <div class="setrow">
        <span class="k">录像</span>
        <input id="dr-p-rec" spellcheck="false" />
        <button class="iconbtn txtbtn" id="dr-p-recb" title="选择文件夹…"></button>
      </div>
      <div class="drsec">播放默认</div>
      <label class="drchk"><input type="checkbox" id="dr-p-loop" /><span>默认开启循环播放</span></label>
      <label class="drchk"><input type="checkbox" id="dr-p-force" /><span>默认强制走转码桥</span></label>
      <div class="drhint">
        目录留空 = 默认（图片\vqa_snapshots、视频\vqa_recordings）。保存时会建目录并做写权限自检，
        失败原样报错且不改配置；改完立刻对新的截图/录像生效，不影响正在播放的内容。
      </div>
      <div class="setmsg" id="dr-p-msg"></div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-p-save">保存</button>
      <button class="md-export" id="dr-p-reset">恢复默认</button>
    </div>
  </aside>

  <aside class="drawer" id="dr-analysis" hidden>
    <div class="drhead"><b>视频分析设置</b><button class="iconbtn drx txtbtn" id="dr-a-x" title="关闭"></button></div>
    <div class="drbody">
      <div class="drsec">网络</div>
      <div class="setrow">
        <span class="k">组播网卡</span>
        <select id="a-nic"><option value="">自动</option></select>
      </div>
      <div class="drhint">自动 = 按系统路由选择；分析 UDP / RTP 组播时绑定该网卡。保存后作为默认，下次启动仍沿用；找不到该网卡时回到自动。</div>
      <div class="drsec">导出</div>
      <div class="setrow">
        <span class="k">报告目录</span>
        <input id="dr-a-report" spellcheck="false" />
        <button class="iconbtn txtbtn" id="dr-a-reportb" title="选择文件夹…"></button>
      </div>
      <div class="drhint">
        留空 = 每次点「导出报告」时弹保存对话框。填了目录就直接落盘成
        <code>TS流质量检测报告_时间戳.md</code>，不覆盖上一次。
      </div>
      <div class="setmsg" id="dr-a-msg"></div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-a-save">保存</button>
      <button class="md-export" id="dr-a-reset">恢复默认</button>
    </div>
  </aside>

  <aside class="drawer" id="dr-compliance" hidden>
    <div class="drhead"><b>合规检测设置</b><button class="iconbtn drx txtbtn" id="dr-k-x" title="关闭"></button></div>
    <div class="drbody">
      <div class="drsec">网络</div>
      <div class="setrow">
        <span class="k">组播网卡</span>
        <select id="k-nic"><option value="">自动</option></select>
      </div>
      <div class="drhint">自动 = 按系统路由选择；合规检测收 UDP / RTP 组播流时绑定该网卡。保存后作为默认，下次启动仍沿用；找不到该网卡时回到自动。</div>
      <div class="setmsg" id="dr-k-msg"></div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-k-save">保存</button>
      <button class="md-export" id="dr-k-reset">恢复默认</button>
    </div>
  </aside>`;
  while (frag.firstElementChild) document.body.appendChild(frag.firstElementChild);

  // 关闭按钮与遮罩装配图标
  for (const id of ["dr-p-x", "dr-a-x", "dr-k-x"]) $(id).innerHTML = `${icon("x", 14)}<span class="btxt">关闭</span>`;
  for (const id of ["dr-p-snapb", "dr-p-recb", "dr-a-reportb"]) $(id).innerHTML = `${icon("folder", 14)}<span class="btxt">选择</span>`;
  $("dr-mask").addEventListener("click", () => closeAll());
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeAll();
  });
  window.addEventListener("page-changed", () => closeAll());
}

function open(id: string) {
  closeAll();
  $("dr-mask").hidden = false;
  const d = $(id);
  d.hidden = false;
  requestAnimationFrame(() => d.classList.add("on"));
  drawerHold(id, true);
}

function closeAll() {
  drawerHold("dr-player", false);
  drawerHold("dr-analysis", false);
  drawerHold("dr-compliance", false);
  $("dr-mask").hidden = true;
  for (const id of ["dr-player", "dr-analysis", "dr-compliance"]) {
    const d = $(id);
    d.classList.remove("on");
    d.hidden = true;
  }
}

// ═══════════════════ 播放器 ═══════════════════

function wirePlayerDrawer() {
  const snap = $("dr-p-snap") as HTMLInputElement;
  const rec = $("dr-p-rec") as HTMLInputElement;
  const loop = $("dr-p-loop") as HTMLInputElement;
  const force = $("dr-p-force") as HTMLInputElement;
  const nic = $("p-nic") as HTMLSelectElement;
  const msg = $("dr-p-msg") as HTMLElement;
  const saveBtn = $("dr-p-save") as HTMLButtonElement;

  const say = (t: string, kind: "" | "ok" | "err" = "") => {
    msg.textContent = t;
    msg.className = `setmsg${kind ? ` ${kind}` : ""}`;
  };

  $("p-set").addEventListener("click", () => {
    open("dr-player");
    load();
  });
  $("dr-p-x").addEventListener("click", closeAll);

  async function load() {
    try {
      const p = await api.playerSettings();
      snap.value = p.snap_custom ?? "";
      rec.value = p.rec_custom ?? "";
      snap.placeholder = `默认：${p.snap_default}`;
      rec.placeholder = `默认：${p.rec_default}`;
      loop.checked = !!p.default_loop;
      force.checked = !!p.default_force_bridge;
      nic.value = p.default_nic ?? "";
      say("");
    } catch (e) {
      say(`读取失败：${e}`, "err");
    }
  }

  // 启动时把"默认强制走碑桥"应用到当前会话的勾选框
  api
    .playerSettings()
    .then((p) => {
      setForceBridge(!!p.default_force_bridge);
      if (p.default_loop) api.setLoop(true).catch(() => {});
    })
    .catch(() => {});

  async function pick(input: HTMLInputElement, title: string) {
    try {
      const p = await api.browseFolder(title, input.value.trim());
      if (p) input.value = p;
    } catch (e) {
      say(`选择目录失败：${e}`, "err");
    }
  }
  $("dr-p-snapb").addEventListener("click", () => pick(snap, "选择截图存放目录"));
  $("dr-p-recb").addEventListener("click", () => pick(rec, "选择录像存放目录"));

  saveBtn.addEventListener("click", async () => {
    saveBtn.disabled = true;
    say("保存中…");
    try {
      const p = await api.setPlayerSettings({
        snapDir: snap.value.trim(),
        recDir: rec.value.trim(),
        defaultLoop: loop.checked,
        defaultForceBridge: force.checked,
        defaultNic: nic.value,
      });
      snap.value = p.snap_custom ?? "";
      rec.value = p.rec_custom ?? "";
      setForceBridge(!!p.default_force_bridge);
      say(`已保存 · 截图 → ${p.snap_dir} · 录像 → ${p.rec_dir}`, "ok");
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-p-reset").addEventListener("click", () => {
    snap.value = "";
    rec.value = "";
    loop.checked = false;
    force.checked = false;
    nic.value = "";
    saveBtn.click();
  });
}

// ═══════════════════ 视频分析 ═══════════════════

function wireAnalysisDrawer() {
  const report = $("dr-a-report") as HTMLInputElement;
  const nic = $("a-nic") as HTMLSelectElement;
  const msg = $("dr-a-msg") as HTMLElement;
  const saveBtn = $("dr-a-save") as HTMLButtonElement;

  const say = (t: string, kind: "" | "ok" | "err" = "") => {
    msg.textContent = t;
    msg.className = `setmsg${kind ? ` ${kind}` : ""}`;
  };

  $("a-set").addEventListener("click", () => {
    open("dr-analysis");
    load();
  });
  $("dr-a-x").addEventListener("click", closeAll);

  async function load() {
    try {
      const p = await api.analysisSettings();
      let saved: unknown = null;
      try {
        saved = await api.uiGet("analysis.nic");
      } catch {
        /* 没保存过就保持自动 */
      }
      report.value = p.report_dir;
      report.placeholder = p.report_note;
      nic.value = typeof saved === "string" ? saved : "";
      say("");
    } catch (e) {
      say(`读取失败：${e}`, "err");
    }
  }

  $("dr-a-reportb").addEventListener("click", async () => {
    try {
      const p = await api.browseFolder("选择报告输出目录", report.value.trim());
      if (p) report.value = p;
    } catch (e) {
      say(`选择目录失败：${e}`, "err");
    }
  });

  saveBtn.addEventListener("click", async () => {
    saveBtn.disabled = true;
    say("保存中…");
    try {
      const p = await api.setAnalysisSettings(report.value.trim());
      await api.uiSet("analysis.nic", nic.value);
      report.value = p.report_dir;
      const nicText = nic.selectedOptions[0]?.textContent || "自动";
      const where = p.report_dir ? `报告 → ${p.report_dir}` : "报告 → 每次导出弹保存对话框";
      say(`已保存 · ${where} · 网卡 → ${nicText}`, "ok");
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-a-reset").addEventListener("click", () => {
    report.value = "";
    nic.value = "";
    saveBtn.click();
  });
}

// ═══════════════════ 合规检测 ═══════════════════

function wireComplianceDrawer() {
  const nic = $("k-nic") as HTMLSelectElement;
  const msg = $("dr-k-msg") as HTMLElement;
  const saveBtn = $("dr-k-save") as HTMLButtonElement;

  const say = (t: string, kind: "" | "ok" | "err" = "") => {
    msg.textContent = t;
    msg.className = `setmsg${kind ? ` ${kind}` : ""}`;
  };

  $("k-set").addEventListener("click", () => {
    open("dr-compliance");
    load();
  });
  $("dr-k-x").addEventListener("click", closeAll);

  async function load() {
    try {
      const saved = await api.uiGet("compliance.nic");
      nic.value = typeof saved === "string" ? saved : "";
      say("");
    } catch (e) {
      say(`读取失败：${e}`, "err");
    }
  }

  saveBtn.addEventListener("click", async () => {
    saveBtn.disabled = true;
    say("保存中…");
    try {
      await api.uiSet("compliance.nic", nic.value);
      const nicText = nic.selectedOptions[0]?.textContent || "自动";
      say(`已保存 · 网卡 → ${nicText}`, "ok");
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-k-reset").addEventListener("click", () => {
    nic.value = "";
    saveBtn.click();
  });
}
