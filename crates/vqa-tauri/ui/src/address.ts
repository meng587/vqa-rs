//! 地址输入框的中文标点归一化。
//!
//! 中文输入法下很容易打出「。」「：」「／」等全角符号，粘贴到
//! UDP / RTSP / 文件地址里会导致后端识别失败。这里统一转成 ASCII，
//! 只处理地址类输入框，不影响频道名称等普通文本。

const ADDRESS_SELECTOR = [
  "#p-url",
  "#a-url",
  "#k-url",
  "#x-in",
  "#x-out",
  "#b-url",
  "#av-src",
  'input[data-role="url"]',
  "input.che-url",
  "input.url",
].join(",");

/** 中文/全角标点 → ASCII；NFKC 先处理全角字母、数字与大部分标点。 */
export function normalizeAddressText(value: string): string {
  if (!value) return value;
  return value
    .normalize("NFKC")
    .replace(/。/g, ".")
    .replace(/、/g, ",");
}

function normalizeInput(input: HTMLInputElement): void {
  const before = input.value;
  const after = normalizeAddressText(before);
  if (after === before) return;
  const start = input.selectionStart ?? before.length;
  const end = input.selectionEnd ?? before.length;
  const delta = after.length - before.length;
  input.value = after;
  if (document.activeElement === input) {
    const nextStart = Math.max(0, Math.min(after.length, start + delta));
    const nextEnd = Math.max(0, Math.min(after.length, end + delta));
    input.setSelectionRange(nextStart, nextEnd);
  }
}

const bound = new WeakSet<HTMLInputElement>();

export function bindAddressInput(input: HTMLInputElement): void {
  if (bound.has(input)) return;
  bound.add(input);

  // 捕获阶段先归一化，业务页在冒泡阶段读到的已经是 ASCII 地址。
  input.addEventListener(
    "input",
    (ev) => {
      if ((ev as InputEvent).isComposing) return;
      normalizeInput(input);
    },
    true,
  );
  input.addEventListener("compositionend", () => normalizeInput(input));
  input.addEventListener("blur", () => normalizeInput(input));
}

export function bindAddressInputs(root: ParentNode = document): void {
  root.querySelectorAll<HTMLInputElement>(ADDRESS_SELECTOR).forEach(bindAddressInput);
}

/** 绑定当前页面，并接管之后动态生成的频道编辑/新增输入框。 */
export function watchAddressInputs(): void {
  bindAddressInputs();
  if (typeof MutationObserver === "undefined") return;
  const observer = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      for (const node of mutation.addedNodes) {
        if (!(node instanceof Element)) continue;
        if (node instanceof HTMLInputElement) {
          if (node.matches(ADDRESS_SELECTOR)) bindAddressInput(node);
        }
        bindAddressInputs(node);
      }
    }
  });
  observer.observe(document.body, { childList: true, subtree: true });
}
