// キャラクターの管理画面: 人物の一覧、名前・別名、人物の情報（性格・口調・過去など）、人物どうしの関係。
//
// 情報と関係には由来（ユーザーの設定・確認済み・AI の推定）と状態（有効・承認待ち・却下）がある。
// AI の推定はユーザーの設定を上書きせず、ここで「確認済みにする」か「却下」するまで区別して表示する。

import { api } from "./api";
import { askConfirm, askText, h, openDialog, toast } from "./dom";
import { icon } from "./icons";
import { errorMessage } from "./sceneEditor";
import type { Character, CharacterFact, CharacterRelation, InfoInput, InfoSource, InfoStatus } from "./types";

const SOURCE_LABELS: Record<InfoSource, string> = {
  user: "設定",
  confirmed: "確認済み",
  inferred: "AI の推定",
};

const STATUS_LABELS: Record<InfoStatus, string> = {
  active: "有効",
  proposed: "承認待ち",
  rejected: "却下",
};

/** よく使う分類（自由に入力もできる）。 */
const CATEGORY_SUGGESTIONS = ["役割", "年齢", "外見", "性格", "口調", "価値観", "背景", "目的", "秘密", "その他"];

export interface CharacterViewOptions {
  /** 一覧や名前が変わったとき（他の画面の表示を更新するため）。 */
  onChanged?: () => void;
}

export class CharacterView {
  private projectId: string | null = null;
  private characters: Character[] = [];
  private relations: CharacterRelation[] = [];
  private facts: CharacterFact[] = [];
  private selectedId: string | null = null;
  private showRejected = false;

  constructor(private host: HTMLElement, private options: CharacterViewOptions = {}) {}

  async open(projectId: string): Promise<void> {
    if (this.projectId !== projectId) this.selectedId = null;
    this.projectId = projectId;
    await this.reload();
  }

  /** 他の端末での変更を反映する。 */
  async refresh(): Promise<void> {
    if (this.projectId) await this.reload();
  }

  private async reload(): Promise<void> {
    const projectId = this.projectId!;
    [this.characters, this.relations] = await Promise.all([api.listCharacters(projectId), api.listRelations(projectId)]);
    if (!this.characters.some((c) => c.id === this.selectedId)) this.selectedId = this.characters[0]?.id ?? null;
    this.facts = this.selectedId ? await api.listFacts(this.selectedId) : [];
    this.render();
  }

  private async guard(action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  }

  private changed(): void {
    this.options.onChanged?.();
  }

  private nameOf(id: string): string {
    return this.characters.find((c) => c.id === id)?.name ?? "（削除された人物）";
  }

  private render(): void {
    const list = h(
      "ul", { class: "character-list" },
      ...this.characters.map((c) =>
        h(
          "li", {},
          h("button", {
            type: "button",
            class: c.id === this.selectedId ? "character-link active" : "character-link",
            "aria-current": c.id === this.selectedId ? "true" : undefined,
            onclick: () => void this.guard(async () => {
              this.selectedId = c.id;
              this.facts = await api.listFacts(c.id);
              this.render();
            }),
          }, c.name),
        ),
      ),
    );
    const add = h("button", {
      type: "button",
      class: "add",
      onclick: () => void this.guard(async () => {
        const name = await askText("新しいキャラクターの名前", "", "追加");
        if (!name) return;
        const c = await api.createCharacter(this.projectId!, name);
        this.selectedId = c.id;
        this.changed();
        await this.reload();
      }),
    }, icon("plus"), "キャラクター");

    const selected = this.characters.find((c) => c.id === this.selectedId);
    this.host.replaceChildren(
      h(
        "div", { class: "character-view" },
        h("div", { class: "character-head" }, h("h2", {}, "キャラクター")),
        h("p", { class: "hint" }, "人物の設定と関係を整理します。AI の推定は「AI の推定」と表示され、確認するまで設定と区別されます。"),
        h(
          "div", { class: "character-layout" },
          h("nav", { class: "character-side", "aria-label": "キャラクターの一覧" }, this.characters.length ? list : h("p", { class: "muted" }, "まだいません。"), add),
          h("section", { class: "character-detail" }, selected ? this.renderDetail(selected) : h("p", { class: "muted" }, "キャラクターを追加すると、ここで設定を編集できます。")),
        ),
      ),
    );
  }

  private renderDetail(c: Character): HTMLElement {
    const facts = this.facts.filter((f) => this.showRejected || f.status !== "rejected");
    const rejected = this.facts.length - this.facts.filter((f) => f.status !== "rejected").length;
    const byCategory = new Map<string, CharacterFact[]>();
    for (const f of facts) byCategory.set(f.category, [...(byCategory.get(f.category) ?? []), f]);
    const relations = this.relations.filter(
      (r) => (r.from_character_id === c.id || r.to_character_id === c.id) && (this.showRejected || r.status !== "rejected"),
    );

    return h(
      "div", {},
      h(
        "div", { class: "character-title" },
        h("h3", {}, c.name),
        h("div", { class: "character-actions" },
          h("button", { type: "button", onclick: () => void this.guard(() => this.editCharacter(c)) }, "名前・別名を編集"),
          h("button", { type: "button", class: "danger-outline", onclick: () => void this.guard(() => this.deleteCharacter(c)) }, "削除"),
        ),
      ),
      c.aliases.length ? h("p", { class: "muted" }, `別名: ${c.aliases.join("、")}`) : null,

      h("h4", {}, "人物の情報"),
      byCategory.size === 0 ? h("p", { class: "muted" }, "まだ情報がありません。") : null,
      ...[...byCategory.entries()].map(([category, list]) =>
        h("div", { class: "fact-group" }, h("div", { class: "fact-category" }, category), ...list.map((f) => this.renderFact(f))),
      ),
      h("button", { type: "button", class: "add", onclick: () => void this.guard(() => this.editFact(c.id, null)) }, icon("plus"), "情報"),

      h("h4", {}, "関係"),
      relations.length === 0 ? h("p", { class: "muted" }, "まだ関係がありません。") : null,
      ...relations.map((r) => this.renderRelation(r, c.id)),
      this.characters.length > 1
        ? h("button", { type: "button", class: "add", onclick: () => void this.guard(() => this.editRelation(c.id, null)) }, icon("plus"), "関係")
        : h("p", { class: "muted" }, "関係を設定するには、2 人以上のキャラクターが必要です。"),

      rejected > 0 || this.showRejected
        ? h("label", { class: "check" },
          h("input", {
            type: "checkbox",
            checked: this.showRejected,
            onchange: (e: Event) => {
              this.showRejected = (e.target as HTMLInputElement).checked;
              this.render();
            },
          }),
          `却下した項目も表示する（${rejected} 件）`)
        : null,
    );
  }

  private badges(source: InfoSource, status: InfoStatus, confidence: number | null): HTMLElement {
    return h(
      "span", { class: "info-badges" },
      h("span", { class: `badge source-${source}` }, SOURCE_LABELS[source], source === "inferred" && confidence !== null ? ` ${Math.round(confidence * 100)}%` : ""),
      status !== "active" ? h("span", { class: `badge status-${status}` }, STATUS_LABELS[status]) : null,
    );
  }

  /** AI の推定・承認待ちの項目に出す、確認と却下の操作。 */
  private reviewButtons(source: InfoSource, status: InfoStatus, update: (input: InfoInput) => Promise<unknown>): HTMLElement[] {
    const out: HTMLElement[] = [];
    if (source === "inferred" || status === "proposed") {
      out.push(h("button", {
        type: "button",
        onclick: () => void this.guard(async () => {
          await update({ source: "confirmed", status: "active" });
          await this.reload();
        }),
      }, "確認済みにする"));
    }
    if (status !== "rejected" && source !== "user") {
      out.push(h("button", {
        type: "button",
        onclick: () => void this.guard(async () => {
          await update({ status: "rejected" });
          await this.reload();
        }),
      }, "却下"));
    }
    if (status === "rejected") {
      out.push(h("button", {
        type: "button",
        onclick: () => void this.guard(async () => {
          await update({ status: "active" });
          await this.reload();
        }),
      }, "元に戻す"));
    }
    return out;
  }

  private renderFact(f: CharacterFact): HTMLElement {
    return h(
      "div", { class: f.status === "rejected" ? "info-item rejected" : "info-item" },
      h("div", { class: "info-text" }, f.text, this.badges(f.source, f.status, f.confidence)),
      h("div", { class: "info-actions" },
        ...this.reviewButtons(f.source, f.status, (input) => api.updateFact(f.id, input)),
        h("button", { type: "button", onclick: () => void this.guard(() => this.editFact(f.character_id, f)) }, "編集"),
        h("button", {
          type: "button",
          title: "削除",
          "aria-label": "この情報を削除",
          class: "icon",
          onclick: () => void this.guard(async () => {
            if (!(await askConfirm("この情報を削除しますか？"))) return;
            await api.deleteFact(f.id);
            await this.reload();
          }),
        }, icon("close")),
      ),
    );
  }

  private renderRelation(r: CharacterRelation, selfId: string): HTMLElement {
    const outgoing = r.from_character_id === selfId;
    const other = this.nameOf(outgoing ? r.to_character_id : r.from_character_id);
    const direction = outgoing ? `→ ${other}` : `← ${other}`;
    return h(
      "div", { class: r.status === "rejected" ? "info-item rejected" : "info-item" },
      h("div", { class: "info-text" },
        h("strong", {}, direction), "：", r.kind, r.text ? `（${r.text}）` : "",
        this.badges(r.source, r.status, r.confidence)),
      h("div", { class: "info-actions" },
        ...this.reviewButtons(r.source, r.status, (input) => api.updateRelation(r.id, input)),
        h("button", { type: "button", onclick: () => void this.guard(() => this.editRelation(selfId, r)) }, "編集"),
        h("button", {
          type: "button",
          title: "削除",
          "aria-label": "この関係を削除",
          class: "icon",
          onclick: () => void this.guard(async () => {
            if (!(await askConfirm("この関係を削除しますか？"))) return;
            await api.deleteRelation(r.id);
            await this.reload();
          }),
        }, icon("close")),
      ),
    );
  }

  private async editCharacter(c: Character): Promise<void> {
    const name = h("input", { type: "text", value: c.name, "aria-label": "名前" });
    const aliases = h("input", { type: "text", value: c.aliases.join("、"), placeholder: "例: あおちゃん、青葉", "aria-label": "別名" });
    const ok = await openDialog<boolean>((close) =>
      h(
        "form", {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(true);
          },
        },
        h("h2", {}, "名前・別名"),
        h("label", { class: "form-row" }, h("span", {}, "名前"), name),
        h("label", { class: "form-row" }, h("span", {}, "別名（「、」かカンマで区切る）"), aliases),
        h("div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "保存")),
      ),
    );
    if (!ok) return;
    const list = aliases.value.split(/[、,，]/).map((s) => s.trim()).filter(Boolean);
    await api.updateCharacter(c.id, { name: name.value, aliases: list });
    this.changed();
    await this.reload();
  }

  private async deleteCharacter(c: Character): Promise<void> {
    if (!(await askConfirm(`「${c.name}」を削除しますか？人物の情報と関係も削除されます（本文の話者名は残ります）。`))) return;
    await api.deleteCharacter(c.id);
    this.selectedId = null;
    this.changed();
    await this.reload();
  }

  private async editFact(characterId: string, fact: CharacterFact | null): Promise<void> {
    const listId = `fact-categories-${Math.random().toString(36).slice(2)}`;
    const category = h("input", { type: "text", value: fact?.category ?? "", list: listId, placeholder: "例: 性格", "aria-label": "分類" });
    const text = h("textarea", { rows: 3, "aria-label": "内容" }, fact?.text ?? "");
    const result = await openDialog<boolean>((close) =>
      h(
        "form", {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(true);
          },
        },
        h("h2", {}, fact ? "情報を編集" : "情報を追加"),
        h("label", { class: "form-row" }, h("span", {}, "分類"), category),
        h("datalist", { id: listId }, ...CATEGORY_SUGGESTIONS.map((c) => h("option", { value: c }))),
        h("label", { class: "form-row" }, h("span", {}, "内容"), text),
        fact && fact.source === "inferred" ? h("p", { class: "hint" }, "編集して保存すると、あなたが確認した情報（確認済み）になります。") : null,
        h("div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "保存")),
      ),
    );
    if (!result) return;
    if (fact) {
      const input: InfoInput = { category: category.value, text: text.value };
      if (fact.source === "inferred") Object.assign(input, { source: "confirmed", status: "active" });
      await api.updateFact(fact.id, input);
    } else {
      await api.addFact(characterId, { category: category.value, text: text.value, source: "user" });
    }
    await this.reload();
  }

  private async editRelation(selfId: string, relation: CharacterRelation | null): Promise<void> {
    const others = this.characters.filter((c) => c.id !== selfId);
    const target = h(
      "select", { "aria-label": "相手", disabled: relation !== null },
      ...others.map((c) => h("option", { value: c.id, selected: relation ? c.id === relation.to_character_id || c.id === relation.from_character_id : false }, c.name)),
    );
    const kind = h("input", { type: "text", value: relation?.kind ?? "", placeholder: "例: 幼なじみ・ライバル・片思い", "aria-label": "関係の種類" });
    const text = h("textarea", { rows: 2, "aria-label": "説明" }, relation?.text ?? "");
    const ok = await openDialog<boolean>((close) =>
      h(
        "form", {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(true);
          },
        },
        h("h2", {}, relation ? "関係を編集" : "関係を追加"),
        h("label", { class: "form-row" }, h("span", {}, `${this.nameOf(selfId)} から見た相手`), target),
        relation ? h("p", { class: "hint" }, "2 人の組み合わせは変えられません。変える場合は削除して作り直してください。") : null,
        h("label", { class: "form-row" }, h("span", {}, "関係の種類"), kind),
        h("label", { class: "form-row" }, h("span", {}, "説明（任意）"), text),
        h("div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "保存")),
      ),
    );
    if (!ok) return;
    if (relation) {
      const input: InfoInput = { kind: kind.value, text: text.value };
      if (relation.source === "inferred") Object.assign(input, { source: "confirmed", status: "active" });
      await api.updateRelation(relation.id, input);
    } else {
      await api.createRelation(this.projectId!, {
        from_character_id: selfId,
        to_character_id: target.value,
        kind: kind.value,
        text: text.value,
        source: "user",
      });
    }
    await this.reload();
  }
}
