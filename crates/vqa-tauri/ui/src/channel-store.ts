//! 各功能模块独立的频道库。
//!
//! 每个模块使用自己的 `channels.<module>` 键；旧版本全局 `channels` 只在
//! 首次读取时作为迁移来源复制一次，之后模块之间不再互相影响。

import { api, type Channel } from "./api";
import { groupNames } from "./channel-groups";

export type ChannelModule = "player" | "analysis" | "compliance" | "burst" | "avsync";

export const MODULE_LABELS: Record<ChannelModule, string> = {
  player: "播放器",
  analysis: "视频分析",
  compliance: "合规检测",
  burst: "微突发检测",
  avsync: "音画同步",
};

const LEGACY_CHANNELS_KEY = "channels";
const LEGACY_GROUPS_KEY = "channels.groups";

export function channelsKey(module: ChannelModule): string {
  return `channels.${module}`;
}

export function groupsKey(module: ChannelModule): string {
  return `channels.groups.${module}`;
}

/** 读取模块频道库；模块键不存在时，把旧全局列表复制一份作为初始数据。 */
export async function loadModuleChannels(module: ChannelModule): Promise<Channel[]> {
  try {
    const raw = await api.uiGet(channelsKey(module));
    if (Array.isArray(raw)) return raw as Channel[];
  } catch {
    /* 模块键还没建立，走旧数据迁移 */
  }

  let legacy: Channel[] = [];
  try {
    legacy = await api.channelsGet();
  } catch {
    return [];
  }
  await api.uiSet(channelsKey(module), legacy).catch(() => {});
  return legacy;
}

export async function saveModuleChannels(module: ChannelModule, list: Channel[]): Promise<void> {
  await api.uiSet(channelsKey(module), list);
}

/** 读取模块分组清单；首次也从旧全局分组复制一份。 */
export async function loadModuleGroups(module: ChannelModule, chans: Channel[]): Promise<string[]> {
  let raw: unknown = null;
  try {
    raw = await api.uiGet(groupsKey(module));
  } catch {
    /* 模块分组还没建立 */
  }

  if (!Array.isArray(raw)) {
    try {
      const legacy = await api.uiGet(LEGACY_GROUPS_KEY);
      if (Array.isArray(legacy)) {
        raw = legacy;
        await api.uiSet(groupsKey(module), legacy).catch(() => {});
      }
    } catch {
      /* 旧数据没有分组清单 */
    }
  }

  const saved = Array.isArray(raw) ? raw.filter((g): g is string => typeof g === "string") : [];
  return groupNames(chans, saved);
}

export async function saveModuleGroups(module: ChannelModule, groups: string[]): Promise<void> {
  await api.uiSet(groupsKey(module), groups);
}
