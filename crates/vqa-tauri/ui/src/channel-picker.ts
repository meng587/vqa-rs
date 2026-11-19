//! 仅选频道模式：播放器 / 视频分析 / 合规检测共用。
//!
//! 列表只负责选频道并回填来源地址，不在这里启动播放或检测；
//! 分组、搜索、编辑列表和收起状态沿用 ChannelBoard。

import { api, type Channel } from "./api";
import { ChannelBoard } from "./channels";

interface PickerOptions {
  /** ChannelBoard owner，用于日志和预览槽位区分 */
  page: string;
  hostId: string;
  bodyId: string;
  railId: string;
  inputId: string;
  /** 收起状态独立保存，避免三个页面互相影响 */
  collapsedKey: string;
  onSelect?: (ch: Channel | null) => void;
}

export function initChannelPicker(opts: PickerOptions): ChannelBoard | null {
  const host = document.getElementById(opts.hostId);
  const input = document.getElementById(opts.inputId) as HTMLInputElement | null;
  if (!host || !input) return null;

  const body = document.getElementById(opts.bodyId);
  const rail = document.getElementById(opts.railId) as HTMLButtonElement | null;

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

  const board = new ChannelBoard(host, {
    owner: opts.page,
    title: "频道列表",
    compact: true,
    picker: true,
    onCollapse: () => setCollapsed(true),
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
