//! 跨页面共享的小开关（不放任何一页的 UI 状态，避免 import 成环）。
//! 播放器条上的「强制走转码桥」勾选框已移除，改由设置抽屉里的
//! 「默认强制走转码桥」单独控制，这里只存当前生效值。

export const flags = {
  /** 强制走转码桥（player_play / player_prewarm 的 forceBridge 参数） */
  forceBridge: false,
};

export function setForceBridge(v: boolean) {
  flags.forceBridge = v;
}
