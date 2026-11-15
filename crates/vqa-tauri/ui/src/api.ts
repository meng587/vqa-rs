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
  bit_rate: number;
}

export interface PlayerStateJson {
  playing: boolean;
  err: string | null;
  hint: string | null;
  bridge_note: string;
  bridge: { mode: string; port: number; alive: number; stderr_tail: string } | null;
  recording: boolean;
  probe: Probe | null;
  snap: Snap | null;
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

export const api = {
  // 通用
  appVersion: () => invoke<string>("app_version"),
  listNics: () => invoke<string[]>("list_nics"),
  sourceHistory: (key: string) => invoke<string[]>("source_history", { key }),
  pushSourceHistory: (key: string, url: string) =>
    invoke<void>("push_source_history", { key, url }),
  browse: () => invoke<string | null>("browse_media_file"),
  toolPaths: () =>
    invoke<{ ffmpeg: string | null; ffprobe: string | null; mpv: string | null; davs2: string | null }>("tool_paths"),
  // 播放器
  play: (url: string, nicIp: string, forceBridge: boolean) =>
    invoke<void>("player_play", { url, nicIp, forceBridge }),
  stop: () => invoke<void>("player_stop"),
  pauseToggle: () => invoke<void>("player_pause_toggle"),
  seek: (fraction: number) => invoke<void>("player_seek", { fraction }),
  setVolume: (volume: number) => invoke<void>("player_set_volume", { volume }),
  setMute: (mute: boolean) => invoke<void>("player_set_mute", { mute }),
  setSpeed: (speed: number) => invoke<void>("player_set_speed", { speed }),
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
};
