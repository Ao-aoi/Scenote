// 選択肢・遷移・変数の変更の、構造データ（移動先・条件式・変更式）の編集欄。
//
// 式の誤りは保存を妨げない。構文・未定義の変数などは「構造」画面のチェックで知らせる。

import { h } from "./dom";

export interface SceneOption {
  id: string;
  /** 「章 / 場面」の表示名。 */
  label: string;
}

export interface ChoiceOption {
  text: string;
  target: string | null;
  condition: string;
  effects: string;
}

export type BranchAttrs =
  | { options: ChoiceOption[] }
  | { target: string | null; condition: string }
  | { effects: string };

export const BRANCH_KINDS = new Set(["Choice", "Transition", "VariableChange"]);

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function targetSelect(scenes: SceneOption[], current: string | null, label: string): HTMLSelectElement {
  const known = current === null || scenes.some((s) => s.id === current);
  return h(
    "select", { class: "field branch-target", "aria-label": label },
    h("option", { value: "", selected: current === null }, "（移動しない・次の場面へ）"),
    ...scenes.map((s) => h("option", { value: s.id, selected: s.id === current }, s.label)),
    !known ? h("option", { value: current ?? "", selected: true }, "（削除された場面）") : null,
  );
}

/**
 * 構造データの編集欄を作る。`onChange` は入力のたびに呼ばれ、`read()` で現在の値を取り出す。
 * 対象外の種類では `null`。
 */
export function buildBranchEditor(
  kind: string,
  attrs: Record<string, unknown>,
  scenes: SceneOption[],
  onChange: () => void,
): { element: HTMLElement; read: () => BranchAttrs } | null {
  if (kind === "Transition") {
    const target = targetSelect(scenes, typeof attrs.target === "string" ? attrs.target : null, "遷移先の場面");
    const condition = h("input", { type: "text", class: "field branch-expr", value: str(attrs.condition), placeholder: "条件（空なら必ず移動）例: 勇気 >= 3", "aria-label": "遷移の条件" });
    target.addEventListener("change", onChange);
    condition.addEventListener("input", onChange);
    return {
      element: h("div", { class: "branch" }, h("label", { class: "branch-row" }, h("span", {}, "移動先"), target), h("label", { class: "branch-row" }, h("span", {}, "条件"), condition)),
      read: () => ({ target: target.value || null, condition: condition.value }),
    };
  }
  if (kind === "VariableChange") {
    const effects = h("input", { type: "text", class: "field branch-expr", value: str(attrs.effects), placeholder: "例: 勇気 += 1; 鍵を持っている = true", "aria-label": "変数の変更" });
    effects.addEventListener("input", onChange);
    return {
      element: h("div", { class: "branch" }, h("label", { class: "branch-row" }, h("span", {}, "変更"), effects)),
      read: () => ({ effects: effects.value }),
    };
  }
  if (kind !== "Choice") return null;

  const initial = Array.isArray(attrs.options) ? (attrs.options as Record<string, unknown>[]) : [];
  const list = h("div", { class: "branch-options" });
  const rows: { read: () => ChoiceOption; node: HTMLElement }[] = [];
  const addRow = (o: Partial<ChoiceOption>, notify: boolean) => {
    const text = h("input", { type: "text", class: "field", value: o.text ?? "", placeholder: "選択肢の文", "aria-label": "選択肢の文", maxLength: 500 });
    const target = targetSelect(scenes, o.target ?? null, "選択したときの移動先");
    const condition = h("input", { type: "text", class: "field branch-expr", value: o.condition ?? "", placeholder: "表示する条件（任意）", "aria-label": "選択肢を表示する条件" });
    const effects = h("input", { type: "text", class: "field branch-expr", value: o.effects ?? "", placeholder: "選んだときの変更（任意）例: 好感度 += 1", "aria-label": "選んだときの変数の変更" });
    for (const el of [text, condition, effects]) el.addEventListener("input", onChange);
    target.addEventListener("change", onChange);
    const row = {
      read: (): ChoiceOption => ({ text: text.value, target: target.value || null, condition: condition.value, effects: effects.value }),
      node: h("div", { class: "branch-option" }),
    };
    const remove = h("button", { type: "button", class: "tool", title: "この選択肢を削除", "aria-label": "この選択肢を削除" }, "✕");
    remove.addEventListener("click", () => {
      rows.splice(rows.indexOf(row), 1);
      row.node.remove();
      onChange();
    });
    row.node.append(
      h("div", { class: "branch-option-head" }, h("span", { class: "branch-index" }, "▸"), text, remove),
      h("label", { class: "branch-row" }, h("span", {}, "移動先"), target),
      h("label", { class: "branch-row" }, h("span", {}, "条件"), condition),
      h("label", { class: "branch-row" }, h("span", {}, "変更"), effects),
    );
    rows.push(row);
    list.append(row.node);
    if (notify) onChange();
  };
  for (const o of initial) {
    addRow({ text: str(o.text), target: typeof o.target === "string" ? o.target : null, condition: str(o.condition), effects: str(o.effects) }, false);
  }
  const add = h("button", { type: "button", class: "add" }, "＋ 選択肢の項目");
  add.addEventListener("click", () => {
    addRow({}, true);
    rows[rows.length - 1].node.querySelector("input")?.focus();
  });
  return {
    element: h(
      "div", { class: "branch" },
      h("div", { class: "branch-caption muted" }, "分岐の設定（本文は表示用のメモとして使えます）"),
      list,
      add,
    ),
    read: () => ({ options: rows.map((r) => r.read()) }),
  };
}
