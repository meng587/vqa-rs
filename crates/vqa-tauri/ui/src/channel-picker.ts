//! 频道侧栏：播放器 / 视频分析 / 合规检测共用。
//!
//! 默认是“仅选频道”模式；页面提供 onDetect/onPoll 后切换成完整频道栏
//! （勾选、全选、检测选中、轮询选中），检测动作仍由页面自己执行。

import { api, type Channel } from "./api";
import { ChannelBoard, type RowState } from "./channels";

interface PickerOptions {
  /** ChannelBoard owner，用于日志和预览槽位区分 */
  page: string;
  hostId: string;
  bodyId: string;
  railId: string;
  inputId: string;
  /** 收起状态独立保存，避免三个页面互相影响 */
  collapsedKey: string;
  /** 检测选中按钮文案；给了 onDetect/onPoll 才显示完整频道栏 */
  detectLabel?: string;
  /** 轮询选中按钮文案 */
  pollLabel?: string;
  onDetect?: (ch: Channel) => void;
  onPoll?: (chans: Channel[]) => void;
  stopRun?: () => void;
  initialRow?: (ch: Channel) => { state: RowState; text: string } | null;
  footnote?: () => string;
  onSelect?: (ch: Channel | null) => void;
}

export function initChannelPicker(opts: PickerOptions): ChannelBoard | null {
  const host = document.getElementById(opts.hostId);
  const input = document.getElementById(opts.inputId) as HTMLInputElement | null;
  if (!host || !input) return null;

  const body = document.getElementById(opts.bodyId);
  const rail = document.getElementById(opts.railId) as HTMLButtonElement | null;
  const runMode = !!opts.onDetect || !!opts.onPoll;

  const setCollapsed = (on: boolean, persist = true) => {
    body?.classList.toggle("is-ch-collapsed", on);
    rail?.setAttribute("aria-expanded", String(!on));
    if (persist) api.uiSet(opts.collapsedKey, on).catch(() => {});
    window.setTimeout(() => window.dispatchEvent(new Event("resize")), 30);
  };

  const loadCollapsed = async () => {
    try {
      const value = await api.uiGet(opts.collapsedKey);
      setCollapsed(!!value, false);
    } catch {
      /* 默认展开 */
    }
  };

  host.classList.toggle("is-picker", !runMode);
  const board = new ChannelBoard(host, {
    owner: opts.page,
    title: "频道列表",
    compact: true,
    sequential: runMode || undefined,
    manual: runMode || undefined,
    picker: runMode ? undefined : true,
    detectLabel: opts.detectLabel || "检测选中",
    pollLabel: opts.pollLabel || "轮询选中",
    initialRow: opts.initialRow,
    footnote: opts.footnote,
    onCollapse: () => setCollapsed(true),
    onDetect: opts.onDetect,
    onPoll: opts.onPoll,
    stopRun: opts.stopRun,
    onSelect: (ch) => {
      if (ch) input.value = ch.url;
      opts.onSelect?.(ch);
    },
  });

  rail?.addEventListener("click", () => setCollapsed(false));
  input.addEventListener("input", () => board.selectByUrl(input.value.trim()));

  void board.load().then(() => board.selectByUrl(input.value.trim()));
  void loadCollapsed();

  return board;
}
