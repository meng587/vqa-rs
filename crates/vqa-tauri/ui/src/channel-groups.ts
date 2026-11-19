//! 频道分组的共用口径：旧数据没有 group 时统一归入「未分组」。

import type { Channel } from "./api";

export const DEFAULT_GROUP = "未分组";
/** 筛选器里代表“不限分组”的内部值，不会作为真实分组保存 */
export const ALL_GROUPS = "__all__";

export function normalizeGroup(raw: string | undefined | null): string {
  const name = (raw || "").replace(/\s+/g, " ").trim().slice(0, 24);
  return name || DEFAULT_GROUP;
}

export function groupOf(ch: Channel | null | undefined): string {
  return normalizeGroup(ch?.group);
}

/** 合并已保存的分组清单和频道上实际出现的分组；默认组始终排在第一个 */
export function groupNames(chans: Channel[], saved: string[] = []): string[] {
  const out: string[] = [DEFAULT_GROUP];
  const add = (raw: string | undefined | null) => {
    const name = normalizeGroup(raw);
    if (name !== DEFAULT_GROUP && !out.includes(name)) out.push(name);
  };
  for (const g of saved) add(g);
  for (const ch of chans) add(ch.group);
  return out;
}

export function countInGroup(chans: Channel[], group: string): number {
  return chans.reduce((n, ch) => n + (groupOf(ch) === group ? 1 : 0), 0);
}

export function groupMatches(ch: Channel, filter: string): boolean {
  return filter === ALL_GROUPS || groupOf(ch) === filter;
}
