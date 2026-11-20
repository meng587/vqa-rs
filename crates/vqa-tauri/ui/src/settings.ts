//! 各功能自己的设置抽屉：播放器页齿轮 →「播放器设置」，分析页齿轮 →「视频分析设置」。
//! 每个功能各存一份配置，互不影响：
//!   播放器 → shared/settings.json 的 out_snap_dir / out_rec_dir / player.default_*
//!   分析   → analysis.report_dir / analysis.nic
//!   合规   → compliance.nic
//! 抽屉不切换页面，改完立刻对新截图/录像/导出生效。

import { api, isWebMode, NicRow, WebStatus } from "./api";
import { icon } from "./icons";
import { drawerHold } from "./drawer";
import { setForceBridge } from "./flags";

const $ = (id: string) => document.getElementById(id) as HTMLElement;

export function initSettings() {
  buildDrawers();
  if (isWebMode) {
    for (const id of ["dr-p-snapb", "dr-p-recb", "dr-a-reportb"]) {
      $(id)?.setAttribute("hidden", "");
    }
    for (const id of ["dr-p-snap", "dr-p-rec", "dr-a-report"]) {
      const input = document.getElementById(id) as HTMLInputElement | null;
      if (input) {
        input.placeholder = "填写工作站本机绝对路径";
        if (id === "dr-a-report") input.disabled = true;
      }
    }
    // 主机网卡只能由桌面端修改；Web 端只做只读展示。
    const nicTip = "Web 端不能修改主机网卡，请在桌面端设置";
    for (const id of ["p-nic", "a-nic", "k-nic", "v-nic", "c-nic", "b-set-nic"]) {
      const el = document.getElementById(id) as HTMLSelectElement | null;
      if (el) {
        el.disabled = true;
        el.title = nicTip;
      }
    }
    // 抓包设置抽屉当前只有网卡一项，Web 端不提供入口。
    const captureSet = document.getElementById("c-set");
    if (captureSet) captureSet.hidden = true;
  }
  wirePlayerDrawer();
  wireAnalysisDrawer();
  wireComplianceDrawer();
  wireAvSyncDrawer();
  wireCaptureDrawer();
  wireGlobalDrawer();
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
      <div class="drsec">轮询播放</div>
      <div class="setrow">
        <span class="k">每路时长</span>
        <input id="dr-p-poll" type="number" min="5" max="600" step="5" />
        <span class="unit">秒</span>
      </div>
      <div class="drhint">「轮询播放」按勾选频道顺序播放，每路停留这里设置的秒数；一遍播完自动停止。</div>
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
      <div class="drsec">轮询扫描</div>
      <div class="setrow">
        <span class="k">每路时长</span>
        <input id="dr-a-poll" type="number" min="5" max="600" step="5" />
        <span class="unit">秒</span>
      </div>
      <div class="drhint">「轮询扫描」按勾选频道顺序执行；直播流每路采集这里设置的秒数，文件按自身时长跑完。一遍结束后汇总成一份结果。</div>
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
      <div class="drsec">轮询检测</div>
      <div class="setrow">
        <span class="k">每路时长</span>
        <input id="dr-k-poll" type="number" min="5" max="600" step="5" />
        <span class="unit">秒</span>
      </div>
      <div class="drhint">「轮询检测」按勾选频道顺序执行；直播流每路采集这里设置的秒数后收结果，文件按自身时长检测。一遍结束后汇总成一份结果。</div>
      <div class="setmsg" id="dr-k-msg"></div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-k-save">保存</button>
      <button class="md-export" id="dr-k-reset">恢复默认</button>
    </div>
  </aside>

  <aside class="drawer" id="dr-avsync" hidden>
    <div class="drhead"><b>音画同步设置</b><button class="iconbtn drx txtbtn" id="dr-v-x" title="关闭"></button></div>
    <div class="drbody">
      <div class="drsec">网络</div>
      <div class="setrow">
        <span class="k">组播网卡</span>
        <select id="v-nic"><option value="">自动</option></select>
      </div>
      <div class="drhint">自动 = 按系统路由选择；检测和画面预览收 UDP / RTP 组播流时绑定该网卡。找不到该网卡时回到自动。</div>
      <div class="drsec">检测参数</div>
      <div class="setrow">
        <span class="k">分析时长</span>
        <input id="v-secs" class="drnum" type="number" min="3" max="600" step="1" value="20" />
        <span class="drunit">秒</span>
      </div>
      <div class="setrow">
        <span class="k">搜索范围</span>
        <input id="v-search" class="drnum" type="number" min="40" max="5000" step="20" value="800" />
        <span class="drunit">±ms</span>
      </div>
      <div class="setrow">
        <span class="k">采样帧率</span>
        <input id="v-fps" class="drnum" type="number" min="10" max="60" step="1" value="25" />
        <span class="drunit">fps</span>
      </div>
      <div class="drhint">分析时长决定每次采样多长；实时流建议 20–60 秒。搜索范围决定允许的最大音画偏移；采样帧率越高，时间分辨率越细，但 CPU 占用也更高。</div>
      <div class="setmsg" id="dr-v-msg"></div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-v-save">保存</button>
      <button class="md-export" id="dr-v-reset">恢复默认</button>
    </div>
  </aside>

  <aside class="drawer" id="dr-capture" hidden>
    <div class="drhead"><b>抓包分析设置</b><button class="iconbtn drx txtbtn" id="dr-c-x" title="关闭"></button></div>
    <div class="drbody">
      <div class="drsec">网络</div>
      <div class="setrow">
        <span class="k">抓包网卡</span>
        <select id="c-nic" class="setnic"><option value="">自动</option></select>
      </div>
      <div class="drhint">自动 = 按系统路由选择；抓 UDP / RTP 组播时绑定该网卡，组播建议选承载组播的内网口。保存后作为默认，下次启动仍沿用；找不到该网卡时回到自动。</div>
      <div class="setmsg" id="dr-c-msg"></div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-c-save">保存</button>
      <button class="md-export" id="dr-c-reset">恢复默认</button>
    </div>
  </aside>

  <aside class="drawer" id="dr-global" hidden>
    <div class="drhead"><b>设置</b><button class="iconbtn drx txtbtn" id="dr-g-x" title="关闭"></button></div>
    <div class="drbody">
      <div class="drsec">局域网访问</div>
      <div class="setrow web-toggle-row">
        <span class="k">访问开关</span>
        <label class="web-switch">
          <input type="checkbox" id="dr-g-on" />
          <span class="web-switch-ui" aria-hidden="true"></span>
          <span class="web-switch-t" id="dr-g-ontext">已关闭</span>
        </label>
      </div>
      <div class="setrow">
        <span class="k">自定义端口</span>
        <input id="dr-g-port" class="drnum" type="number" min="1024" max="65535" step="1" value="8766" />
      </div>
      <div class="drhint">
        端口可填 1024–65535，默认 8766。修改端口会重启 Web 服务并更新访问地址；端口被占用时会自动恢复到原端口。
        开启后，同一局域网内的电脑用浏览器访问下面的地址。播放器依赖桌面端 mpv 子窗口，Web 端只开放检测、控制与报告；直播转码和 EasyTier 也保留在桌面端。
      </div>

      <div class="web-access" id="dr-g-card">
        <div class="web-access-head">
          <span class="web-dot" id="dr-g-dot"></span>
          <b id="dr-g-state">未开启</b>
          <span class="web-code" id="dr-g-code" hidden>口令 <strong>--------</strong></span>
        </div>
        <div class="web-kv"><span>访问地址</span><code id="dr-g-url">—</code></div>
        <div class="web-kv" id="dr-g-localrow" hidden><span>本机地址</span><code id="dr-g-local">—</code></div>
        <div class="web-kv" id="dr-g-lanrow" hidden><span>局域网 IP</span><code id="dr-g-lan">—</code></div>
        <div class="web-error" id="dr-g-error" hidden></div>
      </div>

      <div class="web-actions">
        <button class="md-export" id="dr-g-copy" type="button">复制访问地址</button>
        <button class="md-export" id="dr-g-open" type="button">本机打开</button>
        <button class="md-export" id="dr-g-reset" type="button">重置口令</button>
        <button class="md-export" id="dr-g-firewall" type="button" disabled>放行防火墙</button>
        <button class="md-export" id="dr-g-logout" type="button" hidden>退出登录</button>
      </div>

      <div class="setmsg" id="dr-g-msg"></div>

      <div class="drsec">说明</div>
      <div class="drhint">
        本机访问（127.0.0.1）免口令；局域网访问需要 8 位访问码，登录状态最长 12 小时。关闭开关、修改端口或重置访问码后，所有已登录浏览器立即失效。
        请仅在可信局域网内开启，不要直接映射到公网。若另一台电脑打不开，请检查 Windows 防火墙是否放行本程序或对应 TCP 端口（域/专用网络）。
      </div>
    </div>
    <div class="drfoot">
      <button class="gradbtn" id="dr-g-save">保存并应用</button>
    </div>
  </aside>`;
  while (frag.firstElementChild) document.body.appendChild(frag.firstElementChild);

  // 关闭按钮与遮罩装配图标
  for (const id of ["dr-p-x", "dr-a-x", "dr-k-x", "dr-v-x", "dr-c-x", "dr-g-x"]) $(id).innerHTML = `${icon("x", 14)}<span class="btxt">关闭</span>`;
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
  drawerHold("dr-avsync", false);
  drawerHold("dr-capture", false);
  drawerHold("dr-global", false);
  $("dr-mask").hidden = true;
  for (const id of ["dr-player", "dr-analysis", "dr-compliance", "dr-avsync", "dr-capture", "dr-global"]) {
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
  const poll = $("dr-p-poll") as HTMLInputElement;
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
      const saved = await api.uiGet("player.poll_secs");
      poll.value = String(typeof saved === "number" && saved >= 5 ? saved : 30);
      say("");
    } catch (e) {
      poll.value = poll.value || "30";
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
      const secs = Math.max(5, Math.min(600, Math.round(Number(poll.value) || 30)));
      const p = await api.setPlayerSettings({
        snapDir: snap.value.trim(),
        recDir: rec.value.trim(),
        defaultLoop: loop.checked,
        defaultForceBridge: force.checked,
        defaultNic: nic.value,
      });
      await api.uiSet("player.poll_secs", secs);
      poll.value = String(secs);
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
    poll.value = "30";
    saveBtn.click();
  });
}

// ═══════════════════ 视频分析 ═══════════════════

function wireAnalysisDrawer() {
  const report = $("dr-a-report") as HTMLInputElement;
  const nic = $("a-nic") as HTMLSelectElement;
  const poll = $("dr-a-poll") as HTMLInputElement;
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
      const pollSaved = await api.uiGet("analysis.poll_secs");
      poll.value = String(typeof pollSaved === "number" && pollSaved >= 5 ? pollSaved : 30);
      say("");
    } catch (e) {
      poll.value = poll.value || "30";
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
      const secs = Math.max(5, Math.min(600, Math.round(Number(poll.value) || 30)));
      if (!isWebMode) {
        const p = await api.setAnalysisSettings(report.value.trim());
        await api.uiSet("analysis.nic", nic.value);
        report.value = p.report_dir;
      }
      await api.uiSet("analysis.poll_secs", secs);
      poll.value = String(secs);
      if (isWebMode) {
        say(`已保存 · 轮询每路 ${secs} 秒 · 网卡与报告目录由桌面端设置`, "ok");
      } else {
        const nicText = nic.selectedOptions[0]?.textContent || "自动";
        const where = report.value ? `报告 → ${report.value}` : "报告 → 每次导出弹保存对话框";
        say(`已保存 · ${where} · 网卡 → ${nicText}`, "ok");
      }
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-a-reset").addEventListener("click", () => {
    report.value = "";
    nic.value = "";
    poll.value = "30";
    saveBtn.click();
  });
}

// ═══════════════════ 合规检测 ═══════════════════

function wireComplianceDrawer() {
  const nic = $("k-nic") as HTMLSelectElement;
  const poll = $("dr-k-poll") as HTMLInputElement;
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
      const pollSaved = await api.uiGet("compliance.poll_secs");
      poll.value = String(typeof pollSaved === "number" && pollSaved >= 5 ? pollSaved : 30);
      say("");
    } catch (e) {
      poll.value = poll.value || "30";
      say(`读取失败：${e}`, "err");
    }
  }

  saveBtn.addEventListener("click", async () => {
    saveBtn.disabled = true;
    say("保存中…");
    try {
      const secs = Math.max(5, Math.min(600, Math.round(Number(poll.value) || 30)));
      if (!isWebMode) await api.uiSet("compliance.nic", nic.value);
      await api.uiSet("compliance.poll_secs", secs);
      poll.value = String(secs);
      if (isWebMode) {
        say(`已保存 · 轮询每路 ${secs} 秒 · 网卡由桌面端设置`, "ok");
      } else {
        const nicText = nic.selectedOptions[0]?.textContent || "自动";
        say(`已保存 · 网卡 → ${nicText} · 轮询每路 ${secs} 秒`, "ok");
      }
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-k-reset").addEventListener("click", () => {
    nic.value = "";
    poll.value = "30";
    saveBtn.click();
  });
}

// ═══════════════════ 音画同步 ═══════════════════

const AV_PREF_KEY = "avsync.prefs";
const AV_DEFAULTS = { nic: "", secs: 20, search: 800, fps: 25 };

function wireAvSyncDrawer() {
  const nic = $("v-nic") as HTMLSelectElement;
  const secs = $("v-secs") as HTMLInputElement;
  const search = $("v-search") as HTMLInputElement;
  const fps = $("v-fps") as HTMLInputElement;
  const msg = $("dr-v-msg") as HTMLElement;
  const saveBtn = $("dr-v-save") as HTMLButtonElement;

  const say = (t: string, kind: "" | "ok" | "err" = "") => {
    msg.textContent = t;
    msg.className = `setmsg${kind ? ` ${kind}` : ""}`;
  };

  const num = (v: unknown, fallback: number, lo: number, hi: number) => {
    const n = Number(v);
    return Number.isFinite(n) ? Math.min(hi, Math.max(lo, n)) : fallback;
  };

  const label = (n: NicRow) =>
    `${n.ip} · ${n.desc || n.name}${n.up ? "" : "（未连接）"}`;

  const fillNics = (list: NicRow[], selected: string) => {
    const opts = [`<option value="">自动</option>`].concat(
      list.map((n) => `<option value="${n.ip}">${label(n)}</option>`),
    );
    if (selected && !list.some((n) => n.ip === selected)) {
      opts.push(`<option value="${selected}">${selected}（未找到）</option>`);
    }
    nic.innerHTML = opts.join("");
    nic.value = selected;
  };

  $("av-set").addEventListener("click", () => {
    open("dr-avsync");
    load();
  });
  $("dr-v-x").addEventListener("click", closeAll);

  async function load() {
    say("读取中…");
    try {
      const [raw, nics] = await Promise.all([
        api.uiGet(AV_PREF_KEY).catch(() => null),
        api.listNics().catch(() => [] as NicRow[]),
      ]);
      const p: any = raw && typeof raw === "object" ? raw : {};
      const selected = typeof p.nic === "string" ? p.nic : "";
      fillNics(nics, selected);
      secs.value = String(num(p.secs, AV_DEFAULTS.secs, 3, 600));
      search.value = String(num(p.search, AV_DEFAULTS.search, 40, 5000));
      fps.value = String(num(p.fps, AV_DEFAULTS.fps, 10, 60));
      say("");
    } catch (e) {
      say(`读取失败：${e}`, "err");
    }
  }

  saveBtn.addEventListener("click", async () => {
    saveBtn.disabled = true;
    say("保存中…");
    try {
      const value = {
        nic: nic.value,
        secs: num(secs.value, AV_DEFAULTS.secs, 3, 600),
        search: num(search.value, AV_DEFAULTS.search, 40, 5000),
        fps: num(fps.value, AV_DEFAULTS.fps, 10, 60),
      };
      await api.uiSet(AV_PREF_KEY, value);
      secs.value = String(value.secs);
      search.value = String(value.search);
      fps.value = String(value.fps);
      window.dispatchEvent(new CustomEvent("avsync-prefs-changed"));
      const nicText = isWebMode
        ? "网卡沿用桌面端"
        : `网卡 → ${nic.selectedOptions[0]?.textContent || "自动"}`;
      say(
        `已保存 · ${nicText} · ` +
          `时长 ${value.secs} 秒 · 范围 ±${value.search} ms · ${value.fps} fps`,
        "ok",
      );
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-v-reset").addEventListener("click", () => {
    nic.value = "";
    secs.value = String(AV_DEFAULTS.secs);
    search.value = String(AV_DEFAULTS.search);
    fps.value = String(AV_DEFAULTS.fps);
    saveBtn.click();
  });
}

// ═══════════════════ 抓包分析设置 ═══════════════════

const CAPTURE_PREF_KEY = "capture.prefs";

function wireCaptureDrawer() {
  const nic = $("c-nic") as HTMLSelectElement;
  const msg = $("dr-c-msg") as HTMLElement;
  const saveBtn = $("dr-c-save") as HTMLButtonElement;

  const say = (t: string, kind: "" | "ok" | "err" = "") => {
    msg.textContent = t;
    msg.className = `setmsg${kind ? ` ${kind}` : ""}`;
  };

  const fill = (list: NicRow[], selected: string) => {
    const label = (n: NicRow) => `${n.ip} · ${n.desc || n.name}${n.up ? "" : "（未连接）"}`;
    const opts = [`<option value="">自动（系统默认出口）</option>`].concat(
      list.map((n) => `<option value="${n.ip}" title="${n.desc || n.name}">${label(n)}</option>`),
    );
    if (selected && !list.some((n) => n.ip === selected)) {
      opts.push(`<option value="${selected}">${selected}（未找到）</option>`);
    }
    nic.innerHTML = opts.join("");
    nic.value = selected;
  };

  const load = async () => {
    say("读取中…");
    try {
      const [pref, list] = await Promise.all([
        api.uiGet(CAPTURE_PREF_KEY).catch(() => null),
        api.listNics().catch(() => [] as NicRow[]),
      ]);
      const selected = typeof pref?.nic === "string" ? pref.nic : "";
      fill(list, selected);
      say("");
    } catch (e) {
      say(`读取失败：${e}`, "err");
    }
  };

  $("c-set").addEventListener("click", () => {
    open("dr-capture");
    void load();
  });
  $("dr-c-x").addEventListener("click", closeAll);

  saveBtn.addEventListener("click", async () => {
    saveBtn.disabled = true;
    say("保存中…");
    try {
      await api.uiSet(CAPTURE_PREF_KEY, { nic: nic.value });
      say(`已保存 · 网卡 → ${nic.selectedOptions[0]?.textContent || "自动"}`, "ok");
    } catch (e) {
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  $("dr-c-reset").addEventListener("click", () => {
    nic.value = "";
    saveBtn.click();
  });
}

// ═══════════════════ 全局设置：局域网访问 ═══════════════════

function wireGlobalDrawer() {
  const on = $("dr-g-on") as HTMLInputElement;
  const port = $("dr-g-port") as HTMLInputElement;
  const saveBtn = $("dr-g-save") as HTMLButtonElement;
  const msg = $("dr-g-msg") as HTMLElement;
  const state = $("dr-g-state") as HTMLElement;
  const dot = $("dr-g-dot") as HTMLElement;
  const code = $("dr-g-code") as HTMLElement;
  const url = $("dr-g-url") as HTMLElement;
  const local = $("dr-g-local") as HTMLElement;
  const lan = $("dr-g-lan") as HTMLElement;
  const error = $("dr-g-error") as HTMLElement;
  const copyBtn = $("dr-g-copy") as HTMLButtonElement;
  const openBtn = $("dr-g-open") as HTMLButtonElement;
  const resetBtn = $("dr-g-reset") as HTMLButtonElement;
  const firewallBtn = $("dr-g-firewall") as HTMLButtonElement;
  let current: WebStatus | null = null;

  const say = (t: string, kind: "" | "ok" | "err" = "") => {
    msg.textContent = t;
    msg.className = `setmsg${kind ? ` ${kind}` : ""}`;
  };

  const onText = (v: boolean) => {
    $("dr-g-ontext").textContent = v ? "已开启" : "已关闭";
    $("dr-g-ontext").className = `web-switch-t${v ? " is-on" : ""}`;
  };

  const render = (st: WebStatus) => {
    current = st;
    on.checked = st.running;
    port.value = String(st.port);
    onText(st.running);
    state.textContent = st.running ? "正在提供 Web 访问" : "未开启";
    dot.className = `web-dot${st.running ? " is-on" : ""}`;
    $("dr-g-card").classList.toggle("is-on", st.running);
    code.hidden = !st.running;
    code.innerHTML = st.running ? `口令 <strong>${st.code}</strong>` : "";
    url.textContent = st.running ? st.url : "—";
    local.textContent = st.local_url;
    local.parentElement!.hidden = !st.running;
    lan.textContent = st.lan_ip ?? "未检测到局域网地址";
    lan.parentElement!.hidden = !st.running || !st.lan_ip;
    error.hidden = !st.error;
    error.textContent = st.error ?? "";
    copyBtn.disabled = !st.running;
    openBtn.disabled = !st.running;
    resetBtn.disabled = !st.running;
    firewallBtn.disabled = !st.running;
    if (!st.running) code.hidden = true;
  };

  $("global-set").addEventListener("click", async () => {
    open("dr-global");
    $("dr-g-logout").hidden = !isWebMode;
    $("dr-g-firewall").hidden = isWebMode;
    if (isWebMode) {
      on.disabled = true;
      port.disabled = true;
      saveBtn.disabled = true;
      resetBtn.disabled = true;
      openBtn.disabled = true;
      copyBtn.disabled = true;
      $("dr-g-ontext").textContent = "桌面端管理";
      $("dr-g-state").textContent = "当前是 Web 检测端";
      $("dr-g-card").classList.remove("is-on");
      $("dr-g-url").textContent = window.location.origin;
      say("局域网访问开关只能在桌面端修改。", "");
      return;
    }
    say("读取状态…");
    try {
      const st = await api.webStatus();
      render(st);
      say("");
    } catch (e) {
      say(`读取失败：${e}`, "err");
    }
  });
  $("dr-g-x").addEventListener("click", closeAll);
  on.addEventListener("change", () => onText(on.checked));

  saveBtn.addEventListener("click", async () => {
    if (isWebMode) return;
    const p = Number(port.value);
    if (!Number.isFinite(p) || p < 1024 || p > 65535) {
      say("端口范围应为 1024–65535", "err");
      return;
    }
    saveBtn.disabled = true;
    say(on.checked ? "正在开启…" : "正在关闭…");
    try {
      const st = await api.webApply(on.checked, Math.round(p));
      render(st);
      if (st.error) {
        say(st.error, "err");
      } else if (st.running) {
        say(`已开启 · ${st.url} · 访问码 ${st.code}`, "ok");
      } else {
        say("已关闭局域网访问", "ok");
      }
    } catch (e) {
      // 端口失败时后端会尽量恢复旧端口；以实际状态为准，不能简单把开关翻回去。
      try {
        render(await api.webStatus());
      } catch {
        on.checked = !on.checked;
        onText(on.checked);
      }
      say(String(e), "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  resetBtn.addEventListener("click", async () => {
    if (isWebMode) return;
    resetBtn.disabled = true;
    try {
      const c = await api.webResetCode();
      if (current) {
        current.code = c;
      }
      code.innerHTML = `口令 <strong>${c}</strong>`;
      code.hidden = false;
      say(`口令已重置为 ${c}，旧浏览器需重新登录`, "ok");
    } catch (e) {
      say(String(e), "err");
    } finally {
      resetBtn.disabled = false;
    }
  });

  copyBtn.addEventListener("click", async () => {
    if (!current?.running) return;
    const ok = await copyText(current.login_url);
    say(ok ? "访问链接已复制，可直接发给局域网同事" : "复制失败，请手动选中地址", ok ? "ok" : "err");
  });

  openBtn.addEventListener("click", async () => {
    try {
      await api.webOpen();
      say("已在本机浏览器打开", "ok");
    } catch (e) {
      say(String(e), "err");
    }
  });

  firewallBtn.addEventListener("click", async () => {
    if (isWebMode) return;
    firewallBtn.disabled = true;
    try {
      const text = await api.webFirewallFix();
      say(text, "ok");
    } catch (e) {
      say(`放行失败：${e}`, "err");
    } finally {
      firewallBtn.disabled = !current?.running;
    }
  });

  $("dr-g-logout").addEventListener("click", () => {
    window.location.href = "/logout";
  });
}

async function copyText(t: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(t);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = t;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  }
}
