// DOM 構築とダイアログの小さなヘルパー。

type Child = Node | string | null | undefined | false;

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith("on") && typeof value === "function") {
      el.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
    } else if (key === "class") {
      el.className = String(value);
    } else if (key in el) {
      (el as unknown as Record<string, unknown>)[key] = value;
    } else {
      el.setAttribute(key, value === true ? "" : String(value));
    }
  }
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child);
  }
  return el;
}

/** 画面下部に短い通知を表示する。 */
export function toast(message: string, kind: "info" | "error" = "info"): void {
  let host = document.querySelector<HTMLElement>(".toasts");
  if (!host) {
    host = h("div", { class: "toasts", role: "status", "aria-live": "polite" });
    document.body.append(host);
  }
  const item = h("div", { class: `toast toast-${kind}` }, message);
  host.append(item);
  setTimeout(() => item.remove(), kind === "error" ? 7000 : 3500);
}

/** モーダルダイアログを開き、閉じたときの戻り値で解決する。 */
export function openDialog<T>(build: (close: (result: T | null) => void) => HTMLElement): Promise<T | null> {
  return new Promise((resolve) => {
    const dialog = h("dialog", { class: "dialog" });
    let result: T | null = null;
    const close = (value: T | null) => {
      result = value;
      dialog.close();
    };
    dialog.append(build(close));
    dialog.addEventListener("close", () => {
      dialog.remove();
      resolve(result);
    });
    document.body.append(dialog);
    dialog.showModal();
  });
}

export function askText(title: string, initial = "", okLabel = "決定"): Promise<string | null> {
  return openDialog<string>((close) => {
    const input = h("input", { type: "text", value: initial, required: true, maxLength: 200, class: "field" });
    setTimeout(() => input.select());
    return h(
      "form",
      {
        onsubmit: (e: Event) => {
          e.preventDefault();
          const value = input.value.trim();
          if (value) close(value);
        },
      },
      h("h2", {}, title),
      input,
      h(
        "div", { class: "dialog-actions" },
        h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
        h("button", { type: "submit", class: "primary" }, okLabel),
      ),
    );
  });
}

export async function askConfirm(message: string, okLabel = "削除する"): Promise<boolean> {
  const result = await openDialog<boolean>((close) =>
    h(
      "div", {},
      h("p", { class: "dialog-message" }, message),
      h(
        "div", { class: "dialog-actions" },
        h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
        h("button", { type: "button", class: "danger", onclick: () => close(true) }, okLabel),
      ),
    ),
  );
  return result === true;
}

export interface ProjectFormValue {
  name: string;
  description: string;
  templates: string[];
}

export function askProject(
  title: string,
  templates: { id: string; label: string }[],
  initial: ProjectFormValue,
  okLabel: string,
): Promise<ProjectFormValue | null> {
  return openDialog<ProjectFormValue>((close) => {
    const name = h("input", { type: "text", value: initial.name, required: true, maxLength: 200, class: "field" });
    const description = h("textarea", { value: initial.description, rows: 3, class: "field" });
    const boxes = templates.map((t) => ({
      id: t.id,
      box: h("input", { type: "checkbox", checked: initial.templates.includes(t.id) }),
      label: t.label,
    }));
    return h(
      "form",
      {
        onsubmit: (e: Event) => {
          e.preventDefault();
          if (!name.value.trim()) return;
          close({
            name: name.value.trim(),
            description: description.value,
            templates: boxes.filter((b) => b.box.checked).map((b) => b.id),
          });
        },
      },
      h("h2", {}, title),
      h("label", { class: "form-row" }, h("span", {}, "プロジェクト名"), name),
      h("label", { class: "form-row" }, h("span", {}, "説明（任意）"), description),
      h(
        "fieldset", { class: "form-row" },
        h("legend", {}, "テンプレート（複数選択可・後から変更可）"),
        ...boxes.map((b) => h("label", { class: "check" }, b.box, b.label)),
      ),
      h(
        "div", { class: "dialog-actions" },
        h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
        h("button", { type: "submit", class: "primary" }, okLabel),
      ),
    );
  });
}

export interface MenuItem {
  label: string;
  danger?: boolean;
  disabled?: boolean;
  action: () => void;
}

/** ボタンの近くに小さなメニューを表示する。 */
export function openMenu(anchor: HTMLElement, items: MenuItem[]): void {
  document.querySelector(".menu")?.remove();
  const menu = h(
    "div", { class: "menu", role: "menu" },
    ...items.map((item) =>
      h(
        "button",
        {
          type: "button",
          role: "menuitem",
          class: item.danger ? "danger-text" : "",
          disabled: item.disabled,
          onclick: () => {
            menu.remove();
            item.action();
          },
        },
        item.label,
      ),
    ),
  );
  document.body.append(menu);
  const rect = anchor.getBoundingClientRect();
  const left = Math.min(rect.left, window.innerWidth - menu.offsetWidth - 8);
  const below = rect.bottom + 4;
  const top = below + menu.offsetHeight > window.innerHeight ? rect.top - menu.offsetHeight - 4 : below;
  menu.style.left = `${Math.max(8, left)}px`;
  menu.style.top = `${Math.max(8, top)}px`;
  const dismiss = (e: Event) => {
    if (e.target instanceof Node && menu.contains(e.target)) return;
    menu.remove();
    document.removeEventListener("pointerdown", dismiss, true);
  };
  setTimeout(() => document.addEventListener("pointerdown", dismiss, true));
}

export interface SnackbarAction {
  label: string;
  primary?: boolean;
  action: () => void;
}

/**
 * 操作ボタンつきの案内を画面下部に表示する。編集の邪魔をしないよう、フォーカスは奪わない。
 * 何も押されないまま `timeoutMs` が過ぎた場合は `onDismiss` を呼んで閉じる。
 */
export function snackbar(message: string, actions: SnackbarAction[], onDismiss: () => void, timeoutMs = 30000): void {
  let host = document.querySelector<HTMLElement>(".toasts");
  if (!host) {
    host = h("div", { class: "toasts", role: "status", "aria-live": "polite" });
    document.body.append(host);
  }
  host.querySelector(".snackbar")?.remove();
  const item = h("div", { class: "toast snackbar" }, h("span", { class: "snackbar-text" }, message));
  const timer = window.setTimeout(() => {
    item.remove();
    onDismiss();
  }, timeoutMs);
  const close = () => {
    window.clearTimeout(timer);
    item.remove();
  };
  for (const a of actions) {
    item.append(
      h("button", {
        type: "button",
        class: a.primary ? "primary" : "",
        onclick: () => {
          close();
          a.action();
        },
      }, a.label),
    );
  }
  host.append(item);
}
