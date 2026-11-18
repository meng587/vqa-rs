//! 合规说明页：把 253 号文的检测依据、检测项与判定口径单独成页，逐条一行显示。
//! 内容与 vqa-core/src/compliance.rs 的 BASIS / 各项 standard 文案保持一致。

type Basis = [code: string, name: string];
type Row = [no: string, item: string, std: string, how: string];

/** 三、检测依据（原文 15 条，顺序照原文） */
const BASIS: Basis[] = [
  ["GB/T 41808—2022", "高动态范围电视节目制作和交换图像参数值"],
  ["GB/T 41809—2022", "超高清晰度电视系统节目制作和交换参数值"],
  ["GB/T 42758—2023", "用于节目制作的先进声音系统"],
  ["GY/T 257.1—2012", "广播电视先进音视频编解码 第 1 部分：视频"],
  ["GY/T 299.1—2016", "高效音视频编码 第 1 部分：视频"],
  ["GY/T 353—2021", "网络视听节目视频格式命名及参数规范"],
  ["GY/T 375—2023", "有线数字电视音视频技术质量要求和测量方法"],
  ["GY/T 376—2023", "IPTV 音视频技术质量要求和测量方法"],
  ["报批稿", "网络视听节目视频技术质量要求和主观评价方法"],
  ["征求意见稿", "超高清视频图像质量客观评价方法"],
  ["总局令第 67 号", "有线广播电视运营服务管理暂行规定"],
  ["广电办发〔2023〕187 号", "4K 超高清电视技术应用实施指南（2023 版）"],
  ["广电发〔2022〕37 号", "关于进一步加强推进高清超高清电视发展的意见"],
  ["新广电发〔2017〕230 号", "关于进一步规范和促进 4K 超高清电视发展的通知"],
  ["新广电发〔2014〕75 号", "广播电视先进视频编解码（AVS+）技术应用实施指南"],
];

/** 节目技术质量：表 1 / 表 2 序号 1~14（两表要求一致） */
const PROG: Row[] = [
  ["1", "分辨率", "1920×1080 / 3840×2160", "ffprobe 流信息"],
  ["2", "帧率", "50i / 50p（高清 50i 或 25p，超高清 50p）", "ffprobe 流信息"],
  ["3", "色域", "BT.709 / BT.2020", "码流 color_space"],
  ["4", "量化精度", "8bit / 10bit", "bits_per_raw_sample、pix_fmt"],
  ["5", "转换曲线", "电视伽马 / HLG 或 PQ", "码流 color_transfer"],
  [
    "6",
    "视频编码方式",
    "高清：AVS+、H.264、AVS2、H.265；超高清：AVS2、H.265",
    "codec_name",
  ],
  [
    "7~10",
    "视频编码类和级",
    "高清：AVS+ 广播类 6.0 级及以上；H.264 High Profile L4.1 及以上；AVS2 基准 8 位类 6.0.30 级及以上；H.265 Main Profile L4.0 及以上。超高清：AVS2 基准 10 位类 8.0.60 级及以上；H.265 Main10 Profile L5.1 及以上",
    "profile / level",
  ],
  [
    "11",
    "视频编码码率",
    "高清：电视台输出信号检测点 ≥11Mbps；有线前端输出点 / IPTV 网络传输信号检测点 AVS+、H.264 ≥8Mbps，AVS2、H.265 ≥6Mbps。超高清：≥36Mbps",
    "流级 bit_rate，缺则取容器码率",
  ],
  [
    "12",
    "音频格式",
    "MPEG-1 Layer2、AAC、AC-3（立体声或 5.1 环绕声；超高清可含三维声）",
    "codec_name + 声道数",
  ],
  ["13", "音频采样率", "48kHz", "sample_rate"],
  [
    "14",
    "音频编码码率",
    "立体声 ≥256kbps、5.1 环绕声 ≥384kbps、三维声 ≥384kbps",
    "音频流 bit_rate",
  ],
];

/** 网络与传输 */
const NET_IPTV: Row[] = [
  ["15", "RTP 抖动", "不大于 50ms", "收流，2s 窗口"],
  ["16", "RTP 丢包率", "不高于 10E-6（即 1×10⁻⁶）", "RTP 序号跳变统计"],
  [
    "17",
    "MDI-DF 时延",
    "以 5s 为计算周期，MDI-DF ≥100ms 的时间占比不大于 0.05%",
    "收流，5s 窗口",
  ],
  [
    "18",
    "MDI-MLR 媒体丢包率",
    "以 10s 为计算周期，MDI-MLR=0 的时间占比不小于 99.99%",
    "收流，10s 窗口",
  ],
];

const NET_CATV: Row[] = [
  [
    "16",
    "QAM 调制信号",
    "电平 50dBuV～75dBuV、MER 不低于 24dB、BER 纠错前不高于 10E-4 且纠错后不高于 10E-11，超门限持续小于 2s",
    "需电平表 / 码流分析仪，软件不判",
  ],
  [
    "17",
    "MDI-DF（IP 信号）",
    "单节目码流 DF 不高于 80ms，多节目复用码流不高于 40ms，超门限持续小于 2s",
    "收流实测（平均包间隔偏差近似）",
  ],
  [
    "17",
    "MDI-MLR（IP 信号）",
    "每秒丢包不超过 1 个 IP 包或 7 个 TS 包；每 15min 不超过 3 个 IP 包或 21 个 TS 包",
    "收流实测（RTP 序号丢包 / 秒）",
  ],
];

const EVENTS: Row[] = [
  [
    "15（表 1）",
    "视频质量要求",
    "指定 PID 视频流丢包或无有效净荷 ≤2s；画面不应出现黑场、静帧、马赛克，异常持续 ≤2s",
    "本版本按 TS PID 层判丢包/无净荷；画面异常需逐帧解码",
  ],
  ["19（表 2）", "音视频质量（一）视频连续无丢帧", "无指定视频 PID 或无有效净荷 ≤2s", "可判：离线按 TS PID 连续性实测"],
  ["20（表 2）", "音视频质量（二）解码正确", "有有效包但解码器无法还原帧 <2s", "本版本未实现（需解码器）"],
  ["21（表 2）", "音视频质量（三）声音连续无中断", "无指定音频 PID 或无有效净荷 <2s", "可判：离线按 TS PID 连续性实测"],
  [
    "22（表 2）",
    "音视频质量（四）画面主观异常",
    "马赛克、静帧、黑屏、可察觉音画不同步 <2s",
    "本版本未实现",
  ],
];

const LIMITS = [
  "事件类：表 2 序号 19 / 21 已按 TS 包头实测（连续性计数错误 = 丢包，仅适配字段 = 无有效净荷）；序号 20 解码正确、22 画面主观异常，以及表 1 序号 15 的画面部分，仍需逐帧解码与参考比对，标未标注。",
  "表 1 序号 16 的电平 / MER / BER 属 QAM 射频层，需电平表与码流分析仪，软件不判。",
  "视频码率优先用流级 bit_rate；TS 未给时取容器码率，含音频与节目 SI，读数会略高于纯视频码率。",
  "AVS、AVS2 的 Profile / Level ffprobe 取不到（level = -99），该项显示未标注，需专业设备核对。",
  "多数 TS 不写 color_space / color_transfer，色域与转换曲线常常只能显示未标注。",
  "MDI-MLR 的「每 15min ≤3 个 IP 包」口径未单列，页面按每秒丢包数判定；样本时长不足 15min 时仅供参考。",
  "表 3（互联网电视）、表 4（互联网视听平台）的主观评分与格式命名不在本页范围内。",
];

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

/** 把说明内容渲染进合规检测页的 #k-doc 视图（不单独占一页）。 */
export function renderComplianceDoc(wrap: HTMLElement) {
  if (wrap.dataset.built === "1") return;

  const basisRows = BASIS.map(([code, name], i) =>
    `<div class="docr basis">
      <span class="docr-no">${i + 1}</span>
      <span class="docr-item mono">${code}</span>
      <span class="docr-std">《${name}》</span>
      <span class="docr-how"></span>
    </div>`,
  ).join("");

  const progRows = PROG.map((r) => rowno(...r)).join("");
  const iptvRows = NET_IPTV.map((r) => rowno(...r)).join("");
  const catvRows = NET_CATV.map((r) => rowno(...r)).join("");
  const evRows = EVENTS.map((r) => rowno(...r)).join("");

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
      LIMITS.map((t) => `<div class="docr limit"><span class="docr-no">·</span><span class="docr-std">${t}</span></div>`).join(""),
    );
  wrap.dataset.built = "1";
}
