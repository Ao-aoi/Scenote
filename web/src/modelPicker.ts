// モデル選択用のドロップダウン。
//
// 標準の <select> は選択肢の中で文字の配置や色を変えられない（iPad の Safari では特に）ため、
// 名前を左、補足情報を右に灰色で並べられる簡易なリストボックスを自作している。

import { h } from "./dom";
import type { LlmModel } from "./types";

export interface ModelChoice {
  id: string;
  label: string;
  /** 右寄せ・灰色で表示する補足情報。 */
  meta: string;
}

function formatTokens(n: number): string {
  // トークン上限は 2 のべき乗が多いため、1K = 1024 として表示する（65536 → 64K）。
  if (n >= 1024 * 1024) return `${+(n / (1024 * 1024)).toFixed(1)}M`;
  if (n >= 1024) return `${Math.round(n / 1024)}K`;
  return String(n);
}

/** モデルの補足情報（バージョン・入出力トークン上限・思考対応）を短く整形する。 */
export function describeModel(m: LlmModel): string {
  const parts: string[] = [];
  if (m.version) parts.push(`v${m.version}`);
  if (m.input_token_limit) parts.push(`入力 ${formatTokens(m.input_token_limit)}`);
  if (m.output_token_limit) parts.push(`出力 ${formatTokens(m.output_token_limit)}`);
  if (m.thinking) parts.push("思考");
  return parts.join(" · ");
}

export class ModelPicker {
  readonly element: HTMLElement;
  private choices: ModelChoice[] = [];
  private selected: string;
  private active = -1;
  private readonly button: HTMLButtonElement;
  private readonly list: HTMLElement;

  constructor(initial: string, label: string) {
    this.selected = initial;
    this.button = h("button", {
      type: "button",
      class: "field picker-button",
      "aria-haspopup": "listbox",
      "aria-expanded": "false",
      "aria-label": label,
    });
    this.list = h("div", { class: "picker-list", role: "listbox", tabIndex: -1, hidden: true });
    this.element = h("div", { class: "picker" }, this.button, this.list);

    this.button.addEventListener("click", () => (this.list.hidden ? this.openList() : this.closeList()));
    this.button.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        this.openList();
      }
    });
    this.list.addEventListener("keydown", (e) => this.onKey(e));
    this.list.addEventListener("focusout", (e) => {
      if (!(e.relatedTarget instanceof Node && this.element.contains(e.relatedTarget))) this.closeList();
    });
  }

  get value(): string {
    return this.selected;
  }

  setChoices(choices: ModelChoice[]): void {
    this.choices = choices;
    this.render();
  }

  private render(): void {
    const current = this.choices.find((c) => c.id === this.selected);
    this.button.replaceChildren(
      h("span", { class: "picker-label" }, current?.label ?? this.selected),
      h("span", { class: "picker-meta" }, current?.meta ?? ""),
      h("span", { class: "picker-caret", "aria-hidden": "true" }, "▾"),
    );
    this.list.replaceChildren(
      ...this.choices.map((c, i) =>
        h(
          "div",
          {
            class: i === this.active ? "picker-option active" : "picker-option",
            role: "option",
            id: `picker-opt-${i}`,
            "aria-selected": String(c.id === this.selected),
            onpointerdown: (e: Event) => e.preventDefault(),
            onclick: () => this.choose(i),
          },
          h("span", { class: "picker-label" }, c.label),
          h("span", { class: "picker-meta" }, c.meta),
        ),
      ),
    );
    if (this.active >= 0) this.list.setAttribute("aria-activedescendant", `picker-opt-${this.active}`);
  }

  private openList(): void {
    this.active = Math.max(0, this.choices.findIndex((c) => c.id === this.selected));
    this.list.hidden = false;
    this.button.setAttribute("aria-expanded", "true");
    this.render();
    this.list.focus();
    this.list.children[this.active]?.scrollIntoView({ block: "nearest" });
  }

  private closeList(): void {
    if (this.list.hidden) return;
    this.list.hidden = true;
    this.button.setAttribute("aria-expanded", "false");
  }

  private choose(index: number): void {
    const choice = this.choices[index];
    if (choice) this.selected = choice.id;
    this.closeList();
    this.render();
    this.button.focus();
  }

  private onKey(e: KeyboardEvent): void {
    const last = this.choices.length - 1;
    const move = (to: number) => {
      e.preventDefault();
      this.active = Math.min(last, Math.max(0, to));
      this.render();
      this.list.children[this.active]?.scrollIntoView({ block: "nearest" });
    };
    if (e.key === "ArrowDown") move(this.active + 1);
    else if (e.key === "ArrowUp") move(this.active - 1);
    else if (e.key === "Home") move(0);
    else if (e.key === "End") move(last);
    else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      this.choose(this.active);
    } else if (e.key === "Escape") {
      // ダイアログ全体を閉じず、一覧だけを閉じる。
      e.preventDefault();
      e.stopPropagation();
      this.closeList();
      this.button.focus();
    }
  }
}
