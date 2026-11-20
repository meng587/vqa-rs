//! 来源输入框的历史下拉：读取/写入按页面 key 分开的最近来源。
//! 播放器、分析、合规、抓包、微突发、音画同步共用同一套交互。

import { api } from "./api";

interface SourceHistoryOptions {
  input: HTMLInputElement;
  button: HTMLButtonElement;
  pop: HTMLElement;
  key: string;
}

function esc(s: string): string {
  return (s || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function wireSourceHistory(opts: SourceHistoryOptions): void {
  const { input, button, pop, key } = opts;

  const close = () => {
    pop.hidden = true;
  };

  button.addEventListener("click", (ev) => {
    ev.stopPropagation();
    if (!pop.hidden) {
      close();
      return;
    }
    api
      .sourceHistory(key)
      .then((list) => {
        pop.innerHTML = [
          `<button class="hi clear" type="button">清除（输入新地址）</button>`,
          ...list.map((x) => `<button class="hi" type="button" title="${esc(x)}">${esc(x)}</button>`),
        ].join("");
        pop.hidden = false;
      })
      .catch(() => {
        pop.hidden = true;
      });
  });

  pop.addEventListener("click", (ev) => {
    const btn = (ev.target as HTMLElement).closest("button.hi") as HTMLButtonElement | null;
    if (!btn) return;
    ev.stopPropagation();
    if (btn.classList.contains("clear")) input.value = "";
    else input.value = btn.title || btn.textContent || "";
    close();
    input.focus();
  });

  document.addEventListener("click", (ev) => {
    if (ev.target === button || button.contains(ev.target as Node)) return;
    if (pop.contains(ev.target as Node)) return;
    close();
  });
}
