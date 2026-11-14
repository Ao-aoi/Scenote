// AI でキャラクター・世界観・ストーリーを段階的に作る画面（#6）。
//
// 1. 作品の概要を入力する → 2. キャラクター案 → 3. 世界観案 → 4. ストーリー案。
// AI の出力は「案」として表示し、ユーザーが編集・選択して「採用」したものだけを登録する。
// 各段階は何度でもやり直せる。案は採用するまでこの端末に一時保存する（再読み込みしても消えない）。

import { api } from "./api";
import { askConfirm, h, toast } from "./dom";
import { errorMessage } from "./sceneEditor";
import type { ChapterDraft, Character, CharacterDraft, CharacterFact } from "./types";

interface DraftState {
  premise: string;
  characters: (CharacterDraft & { selected: boolean })[];
  charInstructions: string;
  world: { title: string; content: string } | null;
  worldInstructions: string;
  story: ChapterDraft[];
  storyInstructions: string;
}

const storeKey = (projectId: string) => `scenote.generation.${projectId}`;

function emptyState(): DraftState {
  return { premise: "", characters: [], charInstructions: "", world: null, worldInstructions: "", story: [], storyInstructions: "" };
}

export interface GenerationViewOptions {
  /** 章・場面・参照ファイルが追加されたとき（一覧の更新用）。 */
  onChanged: () => Promise<void>;
}

export class GenerationView {
  private projectId: string | null = null;
  private state: DraftState = emptyState();
  private busy: "characters" | "world" | "story" | null = null;
  private saved: { character: Character; facts: CharacterFact[] }[] = [];

  constructor(
    private readonly root: HTMLElement,
    private readonly options: GenerationViewOptions,
  ) {}

  async open(projectId: string): Promise<void> {
    if (this.projectId !== projectId) {
      this.projectId = projectId;
      try {
        this.state = { ...emptyState(), ...JSON.parse(localStorage.getItem(storeKey(projectId)) ?? "{}") };
      } catch {
        this.state = emptyState();
      }
    }
    await this.loadSaved();
    this.render();
  }

  private persist(): void {
    if (this.projectId) localStorage.setItem(storeKey(this.projectId), JSON.stringify(this.state));
  }

  private async loadSaved(): Promise<void> {
    if (!this.projectId) return;
    const chars = await api.listCharacters(this.projectId);
    this.saved = await Promise.all(chars.map(async (c) => ({ character: c, facts: await api.listFacts(c.id) })));
  }

  private async run(step: "characters" | "world" | "story", action: () => Promise<void>): Promise<void> {
    if (this.busy) return;
    this.busy = step;
    this.render();
    try {
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      this.busy = null;
      this.persist();
      this.render();
    }
  }

  private warn(warnings: string[]): void {
    if (warnings.length) toast(`一部の案を除外しました: ${warnings.join(" / ")}`);
  }

  private render(): void {
    const s = this.state;
    const premise = h("textarea", {
      class: "field",
      rows: 4,
      value: s.premise,
      placeholder: "例: 嵐の夜、山奥の洋館に招かれた探偵見習いの少女が、主人の失踪の謎を追う。ゴシックで少し切ない雰囲気。全 5 章程度。",
      "aria-label": "作品の概要",
    });
    premise.addEventListener("input", () => {
      s.premise = premise.value;
      this.persist();
    });
    this.root.replaceChildren(
      h("div", { class: "scene-header" }, h("h1", { class: "view-title" }, "作品づくり（AI）")),
      h("p", { class: "hint" }, "AI が出すのは案です。内容を確認・編集して「採用」したものだけが登録されます。どの段階も何度でもやり直せます。"),
      h("section", { class: "gen-step" }, h("h2", {}, "1. 作品の概要"), premise),
      this.renderCharacters(),
      this.renderWorld(),
      this.renderStory(),
    );
  }

  private instructions(value: string, set: (v: string) => void): HTMLInputElement {
    const input = h("input", { type: "text", class: "field", value, placeholder: "追加の指示（任意）例: もっと明るい雰囲気に" });
    input.addEventListener("input", () => {
      set(input.value);
      this.persist();
    });
    return input;
  }

  // ---- 2. キャラクター -------------------------------------------------

  private renderCharacters(): HTMLElement {
    const s = this.state;
    const count = h("input", { type: "number", class: "field gen-count", min: "1", max: "12", value: "4", "aria-label": "人数の目安" });
    const generate = h("button", { type: "button", class: "primary", disabled: this.busy !== null },
      this.busy === "characters" ? "作成中…" : s.characters.length ? "案を作り直す" : "キャラクター案を作る");
    generate.addEventListener("click", () => void this.run("characters", async () => {
      const r = await api.generateCharacters(this.projectId!, { premise: s.premise, instructions: s.charInstructions, count: Number(count.value) || 4 });
      s.characters = r.drafts.map((d) => ({ ...d, selected: true }));
      this.warn(r.warnings);
    }));

    const cards = s.characters.map((c, ci) => {
      const check = h("input", { type: "checkbox", checked: c.selected, "aria-label": `${c.name} を採用する` });
      check.addEventListener("change", () => { c.selected = check.checked; this.persist(); });
      const name = h("input", { type: "text", class: "field gen-name", value: c.name, maxLength: 100, "aria-label": "名前" });
      name.addEventListener("input", () => { c.name = name.value; this.persist(); });
      const facts = c.facts.map((f, fi) => {
        const cat = h("input", { type: "text", class: "field gen-cat", value: f.category, maxLength: 50, "aria-label": "項目" });
        const text = h("textarea", { class: "field", rows: 2, value: f.text, "aria-label": "内容" });
        cat.addEventListener("input", () => { f.category = cat.value; this.persist(); });
        text.addEventListener("input", () => { f.text = text.value; this.persist(); });
        const del = h("button", { type: "button", class: "tool", title: "この項目を削除", "aria-label": "この項目を削除" }, "✕");
        del.addEventListener("click", () => { c.facts.splice(fi, 1); this.persist(); this.render(); });
        return h("div", { class: "gen-fact" }, cat, text, del);
      });
      const addFact = h("button", { type: "button", class: "add" }, "＋ 項目");
      addFact.addEventListener("click", () => { c.facts.push({ category: "", text: "" }); this.persist(); this.render(); });
      return h("div", { class: "gen-card", "data-index": ci }, h("div", { class: "gen-card-head" }, check, name), ...facts, addFact);
    });

    const adopt = h("button", { type: "button", class: "primary", disabled: !s.characters.some((c) => c.selected) || this.busy !== null }, "選んだキャラクターを採用");
    adopt.addEventListener("click", () => void this.run("characters", async () => {
      const chosen = s.characters.filter((c) => c.selected && c.name.trim());
      for (const c of chosen) {
        const created = await api.createCharacter(this.projectId!, c.name.trim());
        for (const f of c.facts) {
          if (f.category.trim() && f.text.trim()) await api.createFact(created.id, f.category.trim(), f.text.trim());
        }
      }
      s.characters = s.characters.filter((c) => !chosen.includes(c));
      await this.loadSaved();
      toast(`${chosen.length} 人のキャラクターを登録しました`);
    }));

    return h(
      "section", { class: "gen-step" },
      h("h2", {}, "2. キャラクター"),
      this.renderSaved(),
      h("div", { class: "gen-actions" }, h("label", { class: "gen-inline" }, "人数", count), this.instructions(s.charInstructions, (v) => (s.charInstructions = v)), generate),
      cards.length ? h("div", { class: "gen-cards" }, ...cards) : "",
      cards.length ? h("div", { class: "gen-actions" }, adopt) : "",
    );
  }

  private renderSaved(): HTMLElement {
    if (!this.saved.length) return h("p", { class: "muted" }, "登録済みのキャラクターはまだいません。");
    return h(
      "div", { class: "gen-saved" },
      h("p", { class: "muted pref-meta" }, "登録済みのキャラクター（ストーリー案はこの設定を参照します）"),
      ...this.saved.map(({ character, facts }) => {
        const del = h("button", { type: "button", class: "danger-text" }, "削除");
        del.addEventListener("click", () => void this.run("characters", async () => {
          if (!(await askConfirm(`キャラクター「${character.name}」を削除しますか？台詞の話者名は残ります。`))) return;
          await api.deleteCharacter(character.id);
          await this.loadSaved();
        }));
        return h(
          "details", { class: "gen-saved-item" },
          h("summary", {}, character.name, h("span", { class: "muted" }, ` ・ ${facts.length} 項目`)),
          h("ul", {}, ...facts.map((f) => h("li", {}, h("strong", {}, `${f.category}: `), f.text))),
          del,
        );
      }),
    );
  }

  // ---- 3. 世界観 -------------------------------------------------------

  private renderWorld(): HTMLElement {
    const s = this.state;
    const generate = h("button", { type: "button", class: "primary", disabled: this.busy !== null },
      this.busy === "world" ? "作成中…" : s.world ? "案を作り直す" : "世界観の案を作る");
    generate.addEventListener("click", () => void this.run("world", async () => {
      // 採用前のキャラクター案も参考にする。
      const pending = s.characters.filter((c) => c.selected).map(({ name, facts }) => ({ name, facts }));
      const r = await api.generateWorld(this.projectId!, { premise: s.premise, instructions: s.worldInstructions, characters: pending });
      s.world = r.drafts;
    }));
    const parts: (HTMLElement | string)[] = [];
    if (s.world) {
      const w = s.world;
      const title = h("input", { type: "text", class: "field", value: w.title, maxLength: 200, "aria-label": "ファイル名" });
      const content = h("textarea", { class: "field gen-world", rows: 12, value: w.content, "aria-label": "世界観の内容" });
      title.addEventListener("input", () => { w.title = title.value; this.persist(); });
      content.addEventListener("input", () => { w.content = content.value; this.persist(); });
      const adopt = h("button", { type: "button", class: "primary", disabled: this.busy !== null }, "参照ファイルとして採用");
      adopt.addEventListener("click", () => void this.run("world", async () => {
        const file = await api.createFile(this.projectId!, w.title.trim() || "世界観");
        await api.updateFile(file.id, file.revision, { content: w.content, pinned: true });
        s.world = null;
        await this.options.onChanged();
        toast("世界観を参照ファイルに登録しました（常に AI が参照します）");
      }));
      parts.push(title, content, h("div", { class: "gen-actions" }, adopt));
    }
    return h(
      "section", { class: "gen-step" },
      h("h2", {}, "3. 世界観・設定"),
      h("div", { class: "gen-actions" }, this.instructions(s.worldInstructions, (v) => (s.worldInstructions = v)), generate),
      ...parts,
    );
  }

  // ---- 4. ストーリー ---------------------------------------------------

  private renderStory(): HTMLElement {
    const s = this.state;
    const generate = h("button", { type: "button", class: "primary", disabled: this.busy !== null || !this.saved.length },
      this.busy === "story" ? "作成中…" : s.story.length ? "案を作り直す" : "ストーリー案を作る");
    generate.addEventListener("click", () => void this.run("story", async () => {
      const r = await api.generateStory(this.projectId!, { premise: s.premise, instructions: s.storyInstructions });
      s.story = r.drafts;
      this.warn(r.warnings);
    }));
    const chapters = s.story.map((c, ci) => {
      const title = h("input", { type: "text", class: "field gen-chapter", value: c.title, maxLength: 200, "aria-label": "章の題名" });
      title.addEventListener("input", () => { c.title = title.value; this.persist(); });
      const del = h("button", { type: "button", class: "tool", title: "この章を除く", "aria-label": "この章を除く" }, "✕");
      del.addEventListener("click", () => { s.story.splice(ci, 1); this.persist(); this.render(); });
      const scenes = c.scenes.map((sc, si) => {
        const st = h("input", { type: "text", class: "field", value: sc.title, maxLength: 200, "aria-label": "場面の題名" });
        const sum = h("textarea", { class: "field", rows: 2, value: sc.summary, "aria-label": "場面の概要" });
        st.addEventListener("input", () => { sc.title = st.value; this.persist(); });
        sum.addEventListener("input", () => { sc.summary = sum.value; this.persist(); });
        const rm = h("button", { type: "button", class: "tool", title: "この場面を除く", "aria-label": "この場面を除く" }, "✕");
        rm.addEventListener("click", () => { c.scenes.splice(si, 1); this.persist(); this.render(); });
        return h("div", { class: "gen-scene" }, h("div", { class: "gen-card-head" }, st, rm), sum);
      });
      return h("div", { class: "gen-card" }, h("div", { class: "gen-card-head" }, title, del), ...scenes);
    });
    const adopt = h("button", { type: "button", class: "primary", disabled: !s.story.length || this.busy !== null }, "章・場面として追加");
    adopt.addEventListener("click", () => void this.run("story", async () => {
      const chapters = s.story
        .map((c) => ({ title: c.title.trim(), scenes: c.scenes.filter((x) => x.title.trim()).map((x) => ({ title: x.title.trim(), summary: x.summary })) }))
        .filter((c) => c.title && c.scenes.length);
      const added = await api.appendOutline(this.projectId!, chapters);
      s.story = [];
      await this.options.onChanged();
      toast(`${chapters.length} 章・${added.length} 場面を追加しました。各場面の概要はプロットとして編集できます`);
    }));
    return h(
      "section", { class: "gen-step" },
      h("h2", {}, "4. ストーリー（章・場面の概要）"),
      this.saved.length ? "" : h("p", { class: "hint" }, "ストーリー案は、登録済みのキャラクターと世界観を参照して作ります。先にキャラクターを採用してください。"),
      h("div", { class: "gen-actions" }, this.instructions(s.storyInstructions, (v) => (s.storyInstructions = v)), generate),
      chapters.length ? h("div", { class: "gen-cards" }, ...chapters) : "",
      chapters.length ? h("div", { class: "gen-actions" }, adopt) : "",
    );
  }
}
