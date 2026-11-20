//! Tauri 命令封装（前端 → Rust 后端）。

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/** 浏览器直开时没有 Tauri IPC，走同源 /api/invoke。 */
export const isWebMode =
  typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);

const DOWNLOAD_CMDS = new Set([
  "report_export",
  "report_export_b64",
  "compliance_export_md",
  "alarm_export",
  "ai_report_export",
  "diag_export",
]);

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isWebMode) return tauriInvoke<T>(cmd, args);
  if (cmd === "browse_media_file") return (await webPickFile()) as T;
  if (cmd === "browse_folder") return null as T;
  if (DOWNLOAD_CMDS.has(cmd)) return (await webDownload(cmd, args ?? {})) as T;
  const r = await fetch("/api/invoke", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ cmd, args: args ?? {} }),
  });
  const body = await r.json().catch(() => null);
  if (!r.ok || !body?.ok) {
    throw new Error(body?.error || `请求失败（HTTP ${r.status}）`);
  }
  return body.data as T;
}

/** Web 端选择本地文件：上传到工作站临时目录，返回服务端绝对路径。 */
async function webPickFile(): Promise<string | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".ts,.mpg,.mpeg,.mp4,.mkv,.avi,.flv,.m2t,.m2ts,.mts,.mov,.webm,.wmv";
    input.style.display = "none";
    document.body.appendChild(input);
    const done = (v: string | null) => {
      input.remove();
      resolve(v);
    };
    input.addEventListener("change", async () => {
      const file = input.files?.[0];
      if (!file) return done(null);
      try {
        const fd = new FormData();
        fd.append("file", file, file.name);
        const r = await fetch("/api/upload", { method: "POST", body: fd });
        const body = await r.json().catch(() => null);
        if (!r.ok || !body?.ok) throw new Error(body?.error || "上传失败");
        done(body.data as string);
      } catch (e) {
        alert(`文件上传失败：${e}`);
        done(null);
      }
    });
    input.addEventListener("cancel", () => done(null));
    input.click();
  });
}

/** Web 端导出：浏览器下载而不是弹本机保存对话框。 */
async function webDownload(cmd: string, args: Record<string, unknown>): Promise<string> {
  let name = String(args.name || "");
  let text = args.content as string | undefined;
  let b64 = args.contentB64 as string | undefined;
  let ext = String(args.ext || "");
  if (cmd === "alarm_export") {
    name = `告警记录_${new Date().toISOString().slice(0, 16).replace(/[-:T]/g, "")}.json`;
    ext = "json";
  } else if (cmd === "diag_export") {
    name = name || "诊断日志.txt";
    ext = "txt";
  } else if (cmd === "compliance_export_md") {
    ext = ext || "md";
  } else if (cmd === "ai_report_export") {
    ext = ext || "md";
  }
  if (!name) name = `导出_${Date.now()}.${ext || "txt"}`;
  if (!name.includes(".")) name = `${name}.${ext || "txt"}`;

  const mime = ext === "docx"
    ? "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    : ext === "html"
      ? "text/html;charset=utf-8"
      : ext === "json"
        ? "application/json;charset=utf-8"
        : "text/plain;charset=utf-8";
  let blob: Blob;
  if (b64) {
    const raw = atob(b64);
    const buf = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i);
    blob = new Blob([buf], { type: mime });
  } else {
    blob = new Blob([text ?? ""], { type: mime });
  }
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
  return name;
}

/** Web 端服务端产物下载地址（同源 Cookie 会一起带上）。 */
export const webDownloadUrl = {
  capture: (kind: "pcap" | "ts") => `/api/download/capture?kind=${kind}`,
  alarm: (id: string) => `/api/download/alarm?id=${encodeURIComponent(id)}`,
};

/** 用临时 <a> 触发浏览器下载，不离开当前页面。 */
export function downloadUrl(url: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

export interface WebStatus {
  running: boolean;
  enabled: boolean;
  port: number;
  code: string;
  lan_ip: string | null;
  url: string;
  local_url: string;
  login_url: string;
  error: string | null;
}

export interface Snap {
  time_ms: number;
  duration_ms: number;
  paused: boolean;
  idle: boolean;
  seekable: boolean;
  width: number;
  height: number;
  hwdec: string;
  video_codec: string;
  container_fps: number;
  vf_fps: number;
  v_bitrate: number;
  audio_codec: string;
  a_bitrate: number;
  avsync: number;
  render_drop: number;
  decoder_drop: number;
  dec_pixfmt: string;
  speed: number;
  volume: number;
  mute: boolean;
}

export interface Probe {
  w: number;
  h: number;
  codec: string;
  pix_fmt: string;
  /** 源帧率原始串，形如 "50/1" */
  fps: string;
  bit_rate: number;
}

/** 桥的"源 → 目标"目标侧描述（后端 player_snapshot 给出） */
export interface BridgeTo {
  mode: string;
  enc: string;
  enc_label: string;
  pix: string;
  fps: string;
  /** 桥输出实际宽高（8K 自动降级时用于显示 3840×2160） */
  w: number;
  h: number;
  audio: string;
  container: string;
}

export interface PlayerStateJson {
  playing: boolean;
  err: string | null;
  hint: string | null;
  /** 循环播放开关（mpv loop-file） */
  loop: boolean;
  /** 桥转码目标格式；无桥时为 null */
  bridge_to: BridgeTo | null;
  bridge_note: string;
  bridge: { mode: string; progress: string; alive: number; stderr_tail: string } | null;
  recording: boolean;
  probe: Probe | null;
  preparing: boolean;
  snap: Snap | null;
}

/// 播放器设置（只属于播放器页）
export interface PlayerSettings {
  snap_dir: string;
  rec_dir: string;
  snap_default: string;
  rec_default: string;
  snap_custom: string | null;
  rec_custom: string | null;
  default_loop: boolean;
  default_force_bridge: boolean;
  default_nic: string;
}

/// 视频分析设置（只属于分析页）
export interface AnalysisSettings {
  report_dir: string;
  report_note: string;
}

export interface EasytierConfig {
  network_name: string;
  network_secret: string;
  ipv4: string;
  dhcp: boolean;
  peers: string[];
  proxy_networks: string[];
  external_node: string;
  hostname: string;
  rpc_port: number;
  no_listener: boolean;
  no_tun: boolean;
}

export interface EasytierStatus {
  running: boolean;
  admin: boolean;
  version: string;
  config: EasytierConfig;
  uptime_secs: number;
  pid: number | null;
  node: any | null;
  peers: any[];
  log: string;
  error: string | null;
}

export interface ScanProg {
  mode: number; // 0 待机 1 扫描中 2 实时中
  done: number;
  total: number;
  live_bytes: number;
  err: string | null;
}

export interface Res {
  path: string;
  file_size: number;
  packet_size: number;
  packets: number;
  sync_errors: number;
  duration_pcr: number;
  bitrate_avg: number;
  encrypted: boolean;
  pcr_pid: number | null;
  pcr_cbr: boolean;
  has_scrambled: boolean;
  si_truncated: boolean;
  gop_note: string;
  programs: { number: number; pmt_pid: number; pcr_pid: number; streams: { pid: number; type_name: string; kind: string }[] }[];
  pids: { pid: number; kind: string; packets: number; bytes: number; payload_bytes: number; scrambled: number }[];
  si: Record<string, any>;
  tr290: { level: number; name: string; count: number; first: string; detail: string }[];
  rate: [number, number, number][];
  pcr_samples: [number, number, number, number, number][];
  gop: [number, number, number][];
}

export interface NicRow {
  ip: string;
  prefix: number;
  name: string;
  desc: string;
  up: boolean;
  mac: string;
  ifindex: number;
}

/** 多节目 TS 的单个节目（播放器「节目」下拉） */
export interface ProgRow {
  /** 节目号（PAT/PMT 里的 program_id，显示用） */
  number: number;
  /** ffmpeg `-map 0:p:N` 用的节目序号（从 1 起，按 PAT 顺序） */
  ord: number;
  pmt_pid: number;
  /** 「视频 h264」「音频 aac」 */
  streams: string[];
}

// ── AI 编排与模型管理 ──

export interface AiProviderCfg {
  base_url: string;
  model: string;
  timeout_secs: number;
}

export interface AiConfig {
  mode: "rule" | "local" | "cloud" | string;
  local: AiProviderCfg;
  cloud: AiProviderCfg;
  models_index_url: string;
  auto_check: boolean;
}

export interface AiStep {
  id: string;
  tool: string;
  title: string;
  args: Record<string, any>;
  depends_on: string[];
}

export interface AiPlan {
  goal: string;
  summary: string;
  source: string;
  nic_ip: string;
  provider: string;
  steps: AiStep[];
  warnings: string[];
}

export interface AiModelInfo {
  id: string;
  name: string;
  path: string;
  bytes: number;
  sha256: string;
  version: string;
  imported_at: string;
  source: string;
  present: boolean;
}

export interface AiModelCheckItem {
  id: string;
  name: string;
  version: string;
  size: number;
  sha256: string;
  url: string;
  notes: string;
  installed: boolean;
  installed_version: string;
  update_available: boolean;
}

export interface AiModelCheck {
  index_url: string;
  checked_at: string;
  models: AiModelCheckItem[];
}

export const api = {
  // 通用
  appVersion: () => invoke<string>("app_version"),
  listNics: () => invoke<NicRow[]>("list_nics"),
  sourceHistory: (key: string) => invoke<string[]>("source_history", { key }),
  pushSourceHistory: (key: string, url: string) =>
    invoke<void>("push_source_history", { key, url }),
  browse: () => invoke<string | null>("browse_media_file"),
  browseFolder: (title: string, start: string) =>
    invoke<string | null>("browse_folder", { title, start }),
  toolPaths: () =>
    invoke<{ ffmpeg: string | null; ffprobe: string | null; mpv: string | null; davs2: string | null }>("tool_paths"),
  /** 数据/日志目录（关于页「数据与日志」卡） */
  appPaths: () =>
    invoke<{
      data: string;
      shared: string;
      logs: string;
      log_file: string;
      scratch: string;
      snap: string;
      rec: string;
      crash: string;
    }>("app_paths"),
  /** 在资源管理器里打开目录 / 定位文件 */
  reveal: (path: string) => invoke<void>("reveal_path", { path }),
  // 抓包分析（UDP/RTP 业务流，不依赖 Npcap）
  captureStart: (url: string, nicIp: string) =>
    invoke<CaptureSnapshot>("capture_start", { url, nicIp }),
  captureStop: () => invoke<CaptureSnapshot | null>("capture_stop"),
  captureStatus: () => invoke<CaptureSnapshot | null>("capture_status"),
  // AI 编排（只走 API，不操作页面）
  aiConfigGet: () => invoke<AiConfig>("ai_config_get"),
  aiConfigSet: (cfg: AiConfig) => invoke<AiConfig>("ai_config_set", { cfg }),
  aiPlan: (goal: string, source: string, nicIp: string, apiKey?: string) =>
    invoke<AiPlan>("ai_plan", { goal, source, nicIp, apiKey: apiKey ?? null }),
  aiTest: (mode: string, baseUrl: string, model: string, apiKey?: string) =>
    invoke<{ ok: boolean; provider: string; model?: string; latency_ms?: number; reply?: string; error?: string; message?: string }>(
      "ai_test",
      { mode, baseUrl, model, apiKey: apiKey ?? null },
    ),
  aiTools: () => invoke<any[]>("ai_tools"),
  aiModelList: () => invoke<AiModelInfo[]>("ai_model_list"),
  aiModelCheck: (indexUrl?: string) =>
    invoke<AiModelCheck>("ai_model_check", { indexUrl: indexUrl ?? null }),
  aiModelImport: () => invoke<AiModelInfo>("ai_model_import"),
  aiModelRemove: (id: string) => invoke<void>("ai_model_remove", { id }),
  aiReportExport: (name: string, content: string, ext: string) =>
    invoke<string>("ai_report_export", { name, content, ext }),
  // 播放器
  play: (url: string, nicIp: string, forceBridge: boolean) =>
    invoke<void>("player_play", { url, nicIp, forceBridge }),
  adaptiveScale: (scaleW: number) =>
    invoke<void>("player_adaptive_scale", { scaleW }),
  /** ★ 开播前预热：文件一选定/拖入就在后台转码攒缓存，点播放直接读缓存秒开 */
  prewarm: (url: string, nicIp: string, forceBridge: boolean) =>
    invoke<void>("player_prewarm", { url, nicIp, forceBridge }),
  stop: () => invoke<void>("player_stop"),
  pauseToggle: () => invoke<boolean | null>("player_pause_toggle"),
  seek: (fraction: number) => invoke<void>("player_seek", { fraction }),
  setVolume: (volume: number) => invoke<void>("player_set_volume", { volume }),
  setMute: (mute: boolean) => invoke<void>("player_set_mute", { mute }),
  setSpeed: (speed: number) => invoke<void>("player_set_speed", { speed }),
  setLoop: (on: boolean) => invoke<void>("player_set_loop", { on }),
  /** 播放画面上的媒体信息浮层（Tab）；lines 为空 = 收起 */
  playerOsd: (lines: string[], resX: number, resY: number) =>
    invoke<void>("player_osd", { lines, resX, resY }),
  /** 多节目 TS 的节目列表（只读 PAT/PMT，单节目返回 1 条） */
  playerPrograms: (url: string, nicIp: string) =>
    invoke<ProgRow[]>("player_programs", { url, nicIp }),
  /** 切节目：后端用 ffmpeg 抽出该节目喂给 mpv（ord 为 ffmpeg 的节目序号，从 1 起） */
  playerSetProgram: (url: string, program: number, nicIp: string) =>
    invoke<void>("player_set_program", { url, program, nicIp }),
  snapshot: () => invoke<PlayerStateJson>("player_snapshot"),
  snapshotSave: () => invoke<string>("player_snapshot_save"),
  recordToggle: () => invoke<string>("player_record_toggle"),
  placeVideo: (x: number, y: number, w: number, h: number, visible: boolean) =>
    invoke<void>("place_video", { x, y, w, h, visible }),
  // 分析
  analysisStart: (url: string, nicIp: string) =>
    invoke<void>("analysis_start", { url, nicIp }),
  analysisStop: () => invoke<void>("analysis_stop"),
  analysisProgress: () => invoke<ScanProg>("analysis_progress"),
  analysisLog: () => invoke<string[]>("analysis_log"),
  analysisResult: () => invoke<Res | null>("analysis_result"),
  packetRows: (start: number, count: number) =>
    invoke<{ total: number; rows: { idx: number; pid: number; pusi: boolean; afc: number; cc: number; hex: string }[] }>("packet_rows", { start, count }),
  exportReport: (path?: string) =>
    invoke<string | null>("export_report_md", { path: path ?? null }),
  /** 导出 HTML/Markdown/CSV 报告 */
  reportExport: (name: string, content: string, ext: string) =>
    invoke<string | null>("report_export", { name, content, ext }),
  /** 导出二进制报告（Word .docx），content 为 base64 */
  reportExportB64: (name: string, contentB64: string, ext: string) =>
    invoke<string | null>("report_export_b64", { name, contentB64, ext }),
  // 诊断日志
  diagCollect: () => invoke<{ name: string; content: string }>("diag_collect"),
  diagExport: (name: string, content: string) =>
    invoke<string | null>("diag_export", { name, content }),
  diagUpload: (token: string | null, name: string, content: string) =>
    invoke<string>("diag_upload", { token, name, content }),
  diagHasToken: () => invoke<boolean>("diag_has_token"),
  // 分功能设置
  playerSettings: () => invoke<PlayerSettings>("player_settings_get"),
  setPlayerSettings: (v: {
    snapDir: string;
    recDir: string;
    defaultLoop: boolean;
    defaultForceBridge: boolean;
    defaultNic: string;
  }) => invoke<PlayerSettings>("player_settings_set", v),
  analysisSettings: () => invoke<AnalysisSettings>("analysis_settings_get"),
  setAnalysisSettings: (reportDir: string) =>
    invoke<AnalysisSettings>("analysis_settings_set", { reportDir }),
  // EasyTier 组网（随包内置 core/cli）
  easytierConfigGet: () => invoke<EasytierConfig>("easytier_config_get"),
  easytierConfigSave: (cfg: EasytierConfig) =>
    invoke<void>("easytier_config_save", { cfg }),
  easytierStart: (cfg: EasytierConfig) => invoke<void>("easytier_start", { cfg }),
  easytierStop: () => invoke<void>("easytier_stop"),
  easytierStatus: () => invoke<EasytierStatus>("easytier_status"),
  easytierRestartElevated: () => invoke<void>("easytier_restart_elevated"),
  // 媒体信息（ffprobe 全量）
  mediaInfo: (path: string) => invoke<MediaInfo>("media_info", { path }),
  // 合规检测（依据 广电办发〔2024〕253 号）
  complianceCheck: (path: string, kind: string) =>
    invoke<ComplianceResult>("compliance_check", { path, kind }),
  complianceLiveStart: (url: string, nicIp: string, kind: string) =>
    invoke<void>("compliance_live_start", { url, nicIp, kind }),
  complianceLiveSnapshot: (kind: string) =>
    invoke<ComplianceLive>("compliance_live_snapshot", { kind }),
  complianceLiveStop: () => invoke<void>("compliance_live_stop"),
  complianceExport: (name: string, content: string) =>
    invoke<string | null>("compliance_export_md", { name, content }),
  // 直播转码
  xcodeEncoders: () =>
    invoke<{ auto_label: string; encoders: { id: string; label: string }[] }>("xcode_encoders"),
  xcodeStart: (cfg: XcodeCfg) => invoke<XcodeStats>("xcode_start", { cfg }),
  xcodePreview: (cfg: XcodeCfg) => invoke<XcodePreview>("xcode_preview", { cfg }),
  xcodeSnapshot: () => invoke<XcodeStats>("xcode_snapshot"),
  xcodeStop: () => invoke<XcodeStats>("xcode_stop"),
  // 微突发检测
  burstStart: (url: string, nicIp: string, ratio: number, windowMs: number) =>
    invoke<BurstStats>("burst_start", { url, nicIp, ratio, windowMs }),
  burstSnapshot: () => invoke<BurstStats>("burst_snapshot"),
  burstStop: () => invoke<BurstStats>("burst_stop"),
  // 音画同步（job = 并发槽位 id）
  avsyncStart: (cfg: AvSyncCfg, job = "main") => invoke<void>("avsync_start", { cfg, job }),
  avsyncProgress: (job = "main") => invoke<AvPoll>("avsync_progress", { job }),
  avsyncStop: (job = "main") => invoke<void>("avsync_stop", { job }),
  // 一致性比对
  consistStart: (cfg: ConsistCfg, job = "main") => invoke<void>("consist_start", { cfg, job }),
  consistProgress: (job = "main") => invoke<ConsistPoll>("consist_progress", { job }),
  consistStop: (job = "main") => invoke<void>("consist_stop", { job }),
  // 页面设置（每个功能各存各的键）
  uiGet: (key: string) => invoke<any>("ui_pref_get", { key }),
  uiSet: (key: string, value: any) => invoke<void>("ui_pref_set", { key, value }),
  // 频道列表（两页共用一份，存 shared 配置）
  channelsGet: () => invoke<Channel[]>("channels_get"),
  channelsSave: (list: Channel[]) => invoke<void>("channels_save", { list }),
  // 检测画面预览
  previewStart: (owner: string, src: string, nicIp = "") =>
    invoke<void>("preview_start", { owner, src, nicIp }),
  previewStop: (owner: string) => invoke<void>("preview_stop", { owner }),
  previewFrame: (owner: string) => invoke<string | null>("preview_frame", { owner }),
  // 告警处理 / 证据录像 / 存储设置
  alarmList: () => invoke<AlarmRecord[]>("alarm_list"),
  alarmExport: (content: string) => invoke<string | null>("alarm_export", { content }),
  alarmDetectLog: (v: {
    channel: string;
    pair: string;
    similarity: number;
    minSim: number;
    offsetMs: number;
    segmentCount: number;
    verdict: string;
    source: string;
    reference: string;
    fps: number;
    bad: boolean;
  }) =>
    invoke<void>("alarm_detect_log", {
      channel: v.channel,
      pair: v.pair,
      similarity: v.similarity,
      minSim: v.minSim,
      offsetMs: v.offsetMs,
      segmentCount: v.segmentCount,
      verdict: v.verdict,
      source: v.source,
      reference: v.reference,
      fps: v.fps,
      bad: v.bad,
    }),
  alarmRecord: (input: AlarmInput) => invoke<AlarmRecord>("alarm_record", { input }),
  alarmUpdate: (id: string, status: string, conclusion: string, assignee: string, note: string) =>
    invoke<AlarmRecord>("alarm_update", { id, status, conclusion, assignee, note }),
  alarmCapture: (recordId: string, source: string, seconds: number, startS: number | null) =>
    invoke<void>("alarm_capture", { recordId, source, seconds, startS }),
  alarmOpen: (path: string) => invoke<void>("alarm_open", { path }),
  alarmSettings: () => invoke<AlarmSettings>("alarm_settings_get"),
  setAlarmSettings: (v: {
    logDir: string;
    clipDir: string;
    retainDays: number;
    maxGb: number;
    autoClean: boolean;
  }) =>
    invoke<AlarmSettings>("alarm_settings_set", {
      logDir: v.logDir,
      clipDir: v.clipDir,
      retainDays: v.retainDays,
      maxGb: v.maxGb,
      autoClean: v.autoClean,
    }),
  // 局域网 Web 访问（设置抽屉）
  webStatus: () => invoke<WebStatus>("web_status"),
  webApply: (enabled: boolean, port: number) =>
    invoke<WebStatus>("web_apply", { enabled, port }),
  webResetCode: () => invoke<string>("web_reset_code"),
  webOpen: () => invoke<void>("web_open"),
  webFirewallFix: () => invoke<string>("web_firewall_fix"),
};

export interface Channel {
  name: string;
  url: string;
  /** 所属分组；旧数据留空时按“未分组”处理 */
  group?: string;
  enabled: boolean;
}

export interface AvSyncCfg {
  source: string;
  secs: number;
  search_ms: number;
  fps: number;
  nic_ip: string;
}

export interface LagPeak {
  offset_ms: number;
  corr: number;
}

export interface AvSyncResult {
  ok: boolean;
  source: string;
  secs: number;
  fps: number;
  offset_ms: number;
  confidence: number;
  peaks: LagPeak[];
  video_frames: number;
  audio_windows: number;
  verdict: string;
  series_video: number[];
  series_audio: number[];
  err: string | null;
}

export interface AvPoll {
  running: boolean;
  pct: number;
  result: AvSyncResult | null;
  err: string | null;
}

export interface ConsistCfg {
  reference: string;
  target: string;
  fps: number;
  secs: number;
  max_lag_s: number;
  thr: number;
  logo: [number, number, number, number] | null;
}

export interface BadSeg {
  from_s: number;
  to_s: number;
  sim: number;
}

export interface ConsistResult {
  ok: boolean;
  reference: string;
  target: string;
  fps: number;
  frames_ref: number;
  frames_tgt: number;
  best_offset_ms: number;
  mean_sim: number;
  min_sim: number;
  bad_count: number;
  bad_segments: BadSeg[];
  series_sim: number[];
  verdict: string;
  note: string;
  err: string | null;
}

export interface ConsistPoll {
  running: boolean;
  pct: number;
  result: ConsistResult | null;
  err: string | null;
}

export interface XcodeCfg {
  input: string;
  output: string;
  encoder: string;
  vbr_kbps: number;
  audio: string;
  realtime: boolean;
  height?: number | null;
  fps?: number | null;
  nic_ip?: string | null;
  gop?: number | null;
  audio_kbps?: number | null;
  container?: string | null;
  pkt_size?: number | null;
  ttl?: number | null;
}

export interface XcodeStats {
  running: boolean;
  stage: string;
  encoder: string;
  frame: number;
  fps: number;
  kbps: number;
  speed: number;
  drop: number;
  out_time: string;
  secs: number;
  cmd: string;
  err: string | null;
  log: string[];
}

export interface XcodePreview {
  cmd: string;
  encoder: string;
}

export interface BurstStats {
  running: boolean;
  secs: number;
  packets: number;
  bytes: number;
  mbps_avg: number;
  mbps_peak: number;
  ms_peak_bytes: number;
  peak_ratio: number;
  burst_cnt: number;
  bytes_per_ms_avg: number;
  bytes_per_window_avg: number;
  series: number[];
  url: string;
  window_ms: number;
  err: string | null;
}

/** 媒体信息面板数据（后端 mediainfo::probe 的摘要 + 原始 JSON） */
export interface MediaInfo {
  path: string;
  format: {
    container: string | null;
    format_long: string | null;
    duration_s: number | null;
    size_bytes: number | null;
    bit_rate: number | null;
    nb_streams: number;
    probe_score: number | null;
  };
  video: {
    index: number;
    codec: string | null;
    codec_long: string | null;
    profile: string | null;
    width: number | null;
    height: number | null;
    pix_fmt: string | null;
    avg_frame_rate: string | null;
    r_frame_rate: string | null;
    bit_rate: number | null;
    color_space: string | null;
    color_transfer: string | null;
    color_primaries: string | null;
    field_order: string | null;
    sample_aspect_ratio: string | null;
    display_aspect_ratio: string | null;
    level: number | null;
    nb_frames: number | null;
    duration_s: number | null;
    language: string | null;
  }[];
  audio: {
    index: number;
    codec: string | null;
    codec_long: string | null;
    profile: string | null;
    sample_rate: number | null;
    channels: number | null;
    channel_layout: string | null;
    bit_rate: number | null;
    duration_s: number | null;
    language: string | null;
  }[];
  others: { index: number; type: string; codec: string | null; language: string | null }[];
  raw: unknown;
}

/** 合规检测：一条判定项 */
export interface CItem {
  key: string;
  name: string;
  standard: string;
  actual: string;
  verdict: "pass" | "fail" | "unknown";
  note: string;
}

export interface CSummary {
  total: number;
  pass: number;
  fail: number;
  unknown: number;
  ok: boolean;
}

/** 收流统计（IPTV 表 2 的网络四项都从这里出） */
export interface CNet {
  bitrate_mbps: number;
  jitter_ms: number;
  loss_ratio: number;
  df_bad_pct: number;
  mlr_good_pct: number;
  rtp: boolean;
  packets: number;
}

/** 抓包页最近数据报 */
export interface CapturePacket {
  ts_ms: number;
  src: string;
  dst: string;
  len: number;
  kind: string;
  summary: string;
}

export interface CaptureSnapshot {
  running: boolean;
  url: string;
  nic_ip: string;
  out_dir: string;
  pcap_path: string;
  ts_path: string;
  pcap_bytes: number;
  ts_bytes: number;
  started_secs: number;
  net: CNet;
  packets: CapturePacket[];
  error: string | null;
}

/** 检测依据条目（253 号文"三、检测依据"原文） */
export interface CBasis {
  code: string;
  name: string;
}

export interface ComplianceResult {
  mode: string;
  source: string;
  kind: string;
  items: CItem[];
  summary: CSummary;
  basis: CBasis[];
}

export interface ComplianceLive extends ComplianceResult {
  secs: number;
  alive: boolean;
  stats: CNet;
  probe_ready: boolean;
  probe_state: string;
  err: string | null;
}

// ── 告警处理 ──
export interface AlarmHistory {
  time: string;
  action: string;
  who: string;
}

export interface AlarmRecord {
  id: string;
  channel: string;
  pair: string;
  group_id: string;
  kind: string;
  severity: "high" | "mid" | "low" | string;
  status: "pending" | "doing" | "done" | "false" | string;
  similarity: number;
  min_sim: number;
  offset_ms: number;
  segment_count: number;
  first_time: string;
  last_time: string;
  source: string;
  reference: string;
  detail: string;
  fps: number;
  series_sim: number[];
  bad_segments: BadSeg[];
  clip_path: string;
  clip_secs: number;
  clip_size: number;
  clip_state: "none" | "recording" | "ready" | "failed" | string;
  assignee: string;
  conclusion: string;
  note: string;
  history: AlarmHistory[];
}

export interface AlarmInput {
  channel: string;
  pair: string;
  group_id?: string;
  kind?: string;
  severity?: string;
  similarity: number;
  min_sim: number;
  offset_ms: number;
  segment_count: number;
  source?: string;
  reference?: string;
  detail?: string;
  fps?: number;
  series_sim?: number[];
  bad_segments?: BadSeg[];
}

export interface AlarmSettings {
  log_dir: string;
  clip_dir: string;
  retain_days: number;
  max_gb: number;
  auto_clean: boolean;
  log_size: number;
  clip_size: number;
  default_log_dir: string;
  default_clip_dir: string;
}