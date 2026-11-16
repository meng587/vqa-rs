//! 抽屉让位：mpv 是嵌进主窗口的原生子窗（WS_CHILD），永远盖在 WebView 之上，
//! 所以右侧抽屉打开时必须把视频区（#v-rect）让出 360px，否则内容会被画面挡住。
//! 用「谁打开着」的集合而不是计数器，重复调用不会错位。

const openers = new Set<string>();

export function drawerHold(id: string, on: boolean) {
  if (on) openers.add(id);
  else openers.delete(id);
  apply();
}

/// 页面切换时强制收起（避免切页后视频被永久挤压）
export function drawerReleaseAll() {
  openers.clear();
  apply();
}

function apply() {
  const open = openers.size > 0;
  document.body.classList.toggle("drawer-open", open);
  const rect = document.getElementById("v-rect");
  if (rect) rect.style.right = open ? "360px" : "0px";
}
