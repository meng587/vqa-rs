//! 合规说明页：把 253 号文的检测依据、检测项与判定口径单独成页，逐条一行显示。
//! 全部静态表格从后端 `compliance_doc` 取，前端不再维护第二份文案。

import { api, type ComplianceDoc } from "./api";

function rowno(no: string, item: string, std: string, how: string, cls = ""): string {
  return `<div class="docr ${cls}">
      <span class="docr-no">${no}</span>
      <span class="docr-item">${item}</span>
      <span class="docr-std">${std}</span>
      <span class="docr-how">${how}</span>
    </div>`;
}

function head(): string {
  return `<div class="dochead">
      <div class="docrow-ttl">
        <b>合规检测说明</b>
        <span class="doctag">《广电办发〔2024〕253 号》</span>
        <button class="docback" data-view="check"><span data-ic="chevrondown"></span>返回检测</button>
      </div>
      <p class="docsub">
        国家广播电视总局办公厅《关于开展广播电视和网络视听节目技术质量监督检测的通知》：
        表 1 为有线电视、表 2 为 IPTV 的节目技术质量与网络传输要求。本工作站按表内条目逐项判定，
        结论分「合格 / 不合格 / 未标注」——未标注表示数据不足或需专业设备，<b>不等于合格</b>。
      </p>
      <div class="docsteps">
        <span>① 选「依据」表 1 或表 2</span>
        <span>② 选文件（离线）或填 udp:// / rtp:// 地址（收流）</span>
        <span>③ 点「开始检测」，结果按原文序号排列，可导出 Markdown 报告</span>
      </div>
    </div>`;
}

function sec(title: string, hint: string, rows: string): string {
  return `<div class="docsec">
      <h3>${title}</h3>
      ${hint ? `<p class="dochint">${hint}</p>` : ""}
      <div class="docrs">${rows}</div>
    </div>`;
}

function build(wrap: HTMLElement, doc: ComplianceDoc) {
  const basisRows = doc.basis.map(({ code, name }, i) =>
    `<div class="docr basis">
      <span class="docr-no">${i + 1}</span>
      <span class="docr-item mono">${code}</span>
      <span class="docr-std">《${name}》</span>
      <span class="docr-how"></span>
    </div>`,
  ).join("");

  const progRows = doc.prog.map((r) => rowno(...r)).join("");
  const iptvRows = doc.net_iptv.map((r) => rowno(...r)).join("");
  const catvRows = doc.net_catv.map((r) => rowno(...r)).join("");
  const evRows = doc.events.map((r) => rowno(...r)).join("");

  wrap.innerHTML =
    head() +
    sec(
      "一、检测依据（原文 15 条）",
      "按原文「三、检测依据」顺序列出，页脚与导出报告使用同一份清单。",
      basisRows,
    ) +
    sec(
      "二、节目技术质量（表 1 / 表 2 序号 1~14，两表要求一致）",
      "离线文件与收流都判这 14 项；收流时节目参数取自流开头样本（≥800KB）落盘后探测。",
      progRows,
    ) +
    sec(
      "三、网络传输质量（表 2 序号 15~18，仅 IPTV 表 2 有）",
      "需要收流（udp:// 或 rtp://）。离线文件没有传输层数据，这四项显示「需实时收流」。",
      iptvRows,
    ) +
    sec(
      "四、有线 RF 与 IP 信号（表 1 序号 16~17）",
      "序号 16 属射频层，软件不判；序号 17 的两项在 IP 侧，收流时可实测（裸 UDP 无 RTP 序号时 MLR 不判）。",
      catvRows,
    ) +
    sec(
      "五、音视频质量（事件类）",
      "序号 19 / 21 按 TS 包头连续性实测（连续性计数错误即丢包，无有效净荷单独计）；20 / 22 需逐帧解码，标未标注。收流模式暂不做事件类统计，请用离线文件。",
      evRows,
    ) +
    sec(
      "六、判定口径与已知局限",
      "",
      doc.limits.map((t) => `<div class="docr limit"><span class="docr-no">·</span><span class="docr-std">${t}</span></div>`).join(""),
    );
}

/** 把说明内容渲染进合规检测页的 #k-doc 视图（不单独占一页）。 */
export function renderComplianceDoc(wrap: HTMLElement) {
  if (wrap.dataset.built === "1") return;
  wrap.dataset.built = "1";
  wrap.innerHTML = `<div class="docsec"><p class="dochint">正在加载合规说明…</p></div>`;
  void api
    .complianceDoc()
    .then((doc) => build(wrap, doc))
    .catch((e) => {
      wrap.innerHTML = `<div class="docsec"><p class="dochint">合规说明加载失败：${String(e)}。请回到桌面端或刷新页面重试。</p></div>`;
    });
}
