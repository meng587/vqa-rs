//! Tauri 命令封装（前端 → Rust 后端）。

import { invoke } from "@tauri-apps/api/core";

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
}

/// 视频分析设置（只属于分析页）
export interface AnalysisSettings {
  report_dir: string;
  report_note: string;
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
  // 播放器
  play: (url: string, nicIp: string, forceBridge: boolean) =>
    invoke<void>("player_play", { url, nicIp, forceBridge }),
  stop: () => invoke<void>("player_stop"),
  pauseToggle: () => invoke<void>("player_pause_toggle"),
  seek: (fraction: number) => invoke<void>("player_seek", { fraction }),
  setVolume: (volume: number) => invoke<void>("player_set_volume", { volume }),
  setMute: (mute: boolean) => invoke<void>("player_set_mute", { mute }),
  setSpeed: (speed: number) => invoke<void>("player_set_speed", { speed }),
  setLoop: (on: boolean) => invoke<void>("player_set_loop", { on }),
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
  }) => invoke<PlayerSettings>("player_settings_set", v),
  analysisSettings: () => invoke<AnalysisSettings>("analysis_settings_get"),
  setAnalysisSettings: (reportDir: string) =>
    invoke<AnalysisSettings>("analysis_settings_set", { reportDir }),
  // 媒体信息（ffprobe 全量）
  mediaInfo: (path: string) => invoke<MediaInfo>("media_info", { path }),
};

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
