// 場面エディタ。要素ごとのブロック編集と、場面全体の Markdown ソース編集を切り替えられる。

import { api, ApiError } from "./api";
import { BRANCH_KINDS, buildBranchEditor, type BranchAttrs, type SceneOption } from "./branchEditor";
import { askConfirm, h, toast } from "./dom";
import { kindLabel, type ScenarioElement, type SceneDetail } from "./types";

export type SaveState = "saved" | "dirty" | "saving" | "error" | "conflict";

const BLOCK_SAVE_DELAY = 700;
const MARKDOWN_SAVE_DELAY = 1000;
const MODE_KEY = "scenote.editorMode";

type Mode = "blocks" | "markdown";

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : "予期しないエラーが発生しました";
}

/**
 * 入力をまとめて保存する。保存は直列に行い、競合（409）を検出したら
 * ユーザーが解決するまで自動保存を止める。
 */
export class AutoSaver {
  dirty = false;
  conflict = false;
  error: string | null = null;
  private timer: number | undefined;
  private inFlight: Promise<void> | null = null;

  constructor(
    private readonly delay: number,
    private readonly send: () => Promise<void>,
    private readonly onChange: () => void,
    private readonly onConflict: () => void,
  ) {}

  get state(): SaveState {
    if (this.conflict) return "conflict";
    if (this.error) return "error";
    if (this.inFlight) return "saving";
    return this.dirty ? "dirty" : "saved";
  }

  touch(): void {
    this.dirty = true;
    this.error = null;
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.flush(), this.delay);
    this.onChange();
  }

  /** 競合をユーザーが解決した後に呼ぶ。 */
  resolve(keepLocalChanges: boolean): void {
    this.conflict = false;
    this.dirty = keepLocalChanges;
    this.onChange();
    if (keepLocalChanges) void this.flush();
  }

  /** 未保存の変更をすべて保存する。保存しきれなかった場合は false。 */
  async flush(): Promise<boolean> {
    window.clearTimeout(this.timer);
    for (;;) {
      while (this.inFlight) await this.inFlight;
      if (this.conflict) return false;
      if (!this.dirty) return true;
      this.dirty = false;
      this.error = null;
      let failed = false;
      this.inFlight = this.send()
        .catch((e: unknown) => {
          failed = true;
          this.dirty = true;
          if (e instanceof ApiError && e.isConflict) {
            this.conflict = true;
            this.onConflict();
          } else {
            this.error = errorMessage(e);
          }
        })
        .finally(() => {
          this.inFlight = null;
          this.onChange();
        });
      this.onChange();
      await this.inFlight;
      if (failed) return false;
    }
  }
}

export function autoGrow(textarea: HTMLTextAreaElement): void {
  textarea.style.height = "auto";
  textarea.style.height = `${textarea.scrollHeight + 2}px`;
}

export interface SceneEditorOptions {
  kinds: string[];
  /** 分岐の移動先として選べる場面。 */
  scenes: () => SceneOption[];
  onStateChange: (state: SaveState, message?: string) => void;
  /** 場面の題名が変わったとき（ナビゲーションの更新用）。 */
  onSceneRenamed: () => void;
}

export class SceneEditor {
  private scene: SceneDetail | null = null;
  private mode: Mode = localStorage.getItem(MODE_KEY) === "markdown" ? "markdown" : "blocks";
  private savers: AutoSaver[] = [];
  private loadToken = 0;
  private busy = false;
  private disposeSource: (() => void) | null = null;
  /** 編集中のため読み直しを保留している、他の端末での変更。 */
  private pendingRemote = false;

  constructor(
    private readonly root: HTMLElement,
    private readonly options: SceneEditorOptions,
  ) {
    root.addEventListener("focusout", (e) => {
      if (!this.pendingRemote) return;
      if (e.relatedTarget instanceof Node && root.contains(e.relatedTarget)) return;
      // 保存が終わってから読み直す。
      window.setTimeout(() => void this.syncFromRemote(), 1500);
    });
  }

  get sceneId(): string | null {
    return this.scene?.id ?? null;
  }

  /** 場面を開く。開いていた場面の未保存分は先に保存する。 */
  async open(sceneId: string | null): Promise<boolean> {
    if (!(await this.flush())) {
      const discard = await askConfirm(
        "保存できていない変更があります。破棄して移動しますか？",
        "破棄して移動",
      );
      if (!discard) return false;
    }
    const token = ++this.loadToken;
    this.savers = [];
    this.reportState();
    if (sceneId === null) {
      this.scene = null;
      this.root.replaceChildren(
        h("p", { class: "empty" }, "一覧から場面を選ぶか、新しい場面を作成してください。"),
      );
      return true;
    }
    try {
      const scene = await api.getScene(sceneId);
      if (token !== this.loadToken) return true;
      this.scene = scene;
      await this.render();
    } catch (e) {
      if (token !== this.loadToken) return true;
      this.scene = null;
      this.root.replaceChildren(h("p", { class: "empty error-text" }, errorMessage(e)));
    }
    return true;
  }

  /** 未保存の変更をすべて保存する。 */
  async flush(): Promise<boolean> {
    const results = await Promise.all(this.savers.map((s) => s.flush()));
    return results.every(Boolean);
  }

  /** サーバー側で内容が変わった後（AI 提案の承認など）に、表示を最新にする。 */
  async syncFromServer(): Promise<void> {
    if (await this.flush()) await this.reload();
  }

  hasUnsavedChanges(): boolean {
    return this.savers.some((s) => s.state !== "saved");
  }

  /**
   * 他の端末で場面が変更されたときに呼ぶ。
   *
   * 未保存の変更がある場合は読み直さない（保存時にリビジョンの照合で競合として検出し、
   * 両方の内容を見せて選んでもらう）。入力中のカーソル位置はできるだけ保つ。
   */
  async syncFromRemote(): Promise<void> {
    const scene = this.scene;
    if (!scene || this.busy || this.hasUnsavedChanges()) return;
    let latest: SceneDetail;
    try {
      latest = await api.getScene(scene.id);
    } catch {
      return;
    }
    if (this.scene !== scene || latest.revision === scene.revision || this.hasUnsavedChanges()) return;
    const active = document.activeElement;
    if (this.mode === "markdown" && active instanceof Node && this.root.contains(active)) {
      // ソース編集中に全体を差し替えるとカーソルが飛ぶため、編集欄を離れたときに読み直す。
      this.pendingRemote = true;
      return;
    }
    let restore: (() => void) | null = null;
    if (active instanceof HTMLElement && this.root.contains(active)) {
      const id = active.closest<HTMLElement>(".block")?.dataset.id;
      const cls = ["body", "speaker", "kind-select", "scene-title"].find((c) => active.classList.contains(c));
      const start = active instanceof HTMLTextAreaElement || active instanceof HTMLInputElement ? active.selectionStart : null;
      restore = () => {
        const selector = cls === "scene-title" ? ".scene-title" : id && cls ? `.block[data-id="${id}"] .${cls}` : null;
        const target = selector ? this.root.querySelector<HTMLElement>(selector) : null;
        if (!target) return;
        target.focus();
        if (start !== null && (target instanceof HTMLTextAreaElement || target instanceof HTMLInputElement)) {
          const pos = Math.min(start, target.value.length);
          target.setSelectionRange(pos, pos);
        }
      };
    }
    this.scene = latest;
    this.pendingRemote = false;
    await this.render();
    restore?.();
  }

  private reportState(): void {
    const order: SaveState[] = ["conflict", "error", "saving", "dirty"];
    const states = this.savers.map((s) => s.state);
    const state = order.find((s) => states.includes(s)) ?? "saved";
    const message = this.savers.find((s) => s.error)?.error ?? undefined;
    this.options.onStateChange(state, message);
  }

  /** 構造を変える操作の前に保存を済ませる。競合・エラーが残っていれば中止する。 */
  private async structural(action: () => Promise<void>): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      if (!(await this.flush())) {
        toast("未保存の変更を保存できていません。競合やエラーを解消してからやり直してください", "error");
        return;
      }
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
      await this.reload().catch(() => undefined);
    } finally {
      this.busy = false;
    }
  }

  private async reload(): Promise<void> {
    if (!this.scene) return;
    this.scene = await api.getScene(this.scene.id);
    await this.render();
  }

  private async render(): Promise<void> {
    const scene = this.scene;
    if (!scene) return;
    this.savers = [];
    this.disposeSource?.();
    this.disposeSource = null;

    const title = h("input", {
      class: "scene-title",
      type: "text",
      value: scene.title,
      maxLength: 200,
      "aria-label": "場面の題名",
      onchange: () => void this.renameScene(title),
      onkeydown: (e: KeyboardEvent) => {
        if (e.key === "Enter") title.blur();
      },
    });
    const modeButton = (mode: Mode, label: string) =>
      h(
        "button",
        {
          type: "button",
          class: this.mode === mode ? "seg active" : "seg",
          "aria-pressed": String(this.mode === mode),
          onclick: () => void this.setMode(mode),
        },
        label,
      );
    const header = h(
      "div", { class: "scene-header" },
      title,
      h("div", { class: "segmented", role: "group", "aria-label": "編集モード" },
        modeButton("blocks", "ブロック"),
        modeButton("markdown", "Markdown")),
    );
    const body = h("div", { class: "scene-body" });
    this.root.replaceChildren(header, body);
    if (this.mode === "blocks") {
      this.renderBlocks(body, scene);
    } else {
      await this.renderMarkdown(body, scene);
    }
    this.reportState();
  }

  private async renameScene(input: HTMLInputElement): Promise<void> {
    const scene = this.scene;
    if (!scene) return;
    const title = input.value.trim();
    if (!title || title === scene.title) {
      input.value = scene.title;
      return;
    }
    await this.structural(async () => {
      await api.renameScene(scene.id, title);
      this.options.onSceneRenamed();
      await this.reload();
    });
  }

  private async setMode(mode: Mode): Promise<void> {
    if (mode === this.mode) return;
    await this.structural(async () => {
      this.mode = mode;
      localStorage.setItem(MODE_KEY, mode);
      await this.reload();
    });
  }

  // ---- ブロック編集 ---------------------------------------------------

  private renderBlocks(body: HTMLElement, scene: SceneDetail): void {
    const list = h("div", { class: "blocks" });
    scene.elements.forEach((element, index) => {
      list.append(this.buildBlock(scene, element, index));
    });
    if (scene.elements.length === 0) {
      list.append(h("p", { class: "empty" }, "この場面にはまだ要素がありません。下のボタンから追加してください。"));
    }
    const adder = h(
      "div", { class: "adder" },
      h("span", { class: "adder-label" }, "末尾に追加:"),
      ...this.options.kinds.map((kind) =>
        h(
          "button",
          { type: "button", class: `chip kind-${kind}`, onclick: () => void this.addElement(kind, scene.elements.length) },
          `＋ ${kindLabel(kind)}`,
        ),
      ),
    );
    body.append(list, adder);
    requestAnimationFrame(() => list.querySelectorAll("textarea").forEach(autoGrow));
  }

  private buildBlock(scene: SceneDetail, initial: ScenarioElement, index: number): HTMLElement {
    let element = initial;
    const kinds = this.options.kinds.includes(element.kind)
      ? this.options.kinds
      : [...this.options.kinds, element.kind];
    const kindSelect = h(
      "select", { class: "kind-select", "aria-label": "要素の種類" },
      ...kinds.map((kind) => h("option", { value: kind, selected: kind === element.kind }, kindLabel(kind))),
    );
    const speaker = h("input", {
      class: "speaker",
      type: "text",
      value: element.speaker,
      placeholder: "キャラクター名",
      maxLength: 100,
      "aria-label": "キャラクター名",
    });
    const textarea = h("textarea", {
      class: "body",
      value: element.body,
      rows: 1,
      placeholder: `${kindLabel(element.kind)}を入力`,
      "aria-label": "本文",
    });
    const notice = h("div", { class: "block-notice" });
    const block = h("section", { class: `block kind-${element.kind}`, "data-id": element.id });

    const saver = new AutoSaver(
      BLOCK_SAVE_DELAY,
      async () => {
        const result = await api.updateElement(element.id, element.revision, {
          kind: kindSelect.value,
          speaker: speaker.value.trim(),
          body: textarea.value,
          ...(branch && branchKind === kindSelect.value ? { attrs: branch.read() } : {}),
        });
        element = result.element;
        scene.revision = result.scene_revision;
      },
      () => {
        this.reportState();
        if (!saver.conflict) {
          notice.replaceChildren(saver.error ? h("p", { class: "error-text" }, `保存できません: ${saver.error}`) : "");
        }
      },
      () => void showConflict(),
    );
    this.savers.push(saver);

    // 他の画面・端末で先に変更されていた場合、両方の内容を見せてユーザーに選んでもらう。
    const showConflict = async () => {
      let server: ScenarioElement | undefined;
      try {
        server = (await api.getScene(scene.id)).elements.find((e) => e.id === element.id);
      } catch (e) {
        notice.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
        return;
      }
      if (!server) {
        notice.replaceChildren(
          h("p", { class: "error-text" }, "この要素は他の画面・端末で削除されました。内容を残す場合はコピーしてから再読み込みしてください。"),
          h("button", { type: "button", onclick: () => { saver.resolve(false); void this.reload(); } }, "再読み込み"),
        );
        return;
      }
      const latest = server;
      const label = latest.speaker ? `${kindLabel(latest.kind)}（${latest.speaker}）` : kindLabel(latest.kind);
      notice.replaceChildren(
        h("p", { class: "error-text" }, "他の画面・端末で先に変更されています。どちらの内容を残すか選んでください。"),
        h("div", { class: "conflict-server" }, h("strong", {}, `先に保存された内容 — ${label}`), h("pre", {}, latest.body)),
        h(
          "div", { class: "conflict-actions" },
          h("button", {
            type: "button",
            onclick: () => {
              element = latest;
              notice.replaceChildren();
              saver.resolve(true);
            },
          }, "自分の内容を残す"),
          h("button", {
            type: "button",
            onclick: () => {
              element = latest;
              kindSelect.value = latest.kind;
              speaker.value = latest.speaker;
              textarea.value = latest.body;
              syncKind();
              autoGrow(textarea);
              notice.replaceChildren();
              saver.resolve(false);
            },
          }, "先に保存された内容にする"),
        ),
      );
    };

    // 選択肢・遷移・変数の変更では、移動先・条件などの編集欄を表示する。
    let branch: { element: HTMLElement; read: () => BranchAttrs } | null = null;
    let branchKind = "";
    const branchHost = h("div", { class: "branch-host" });
    const syncBranch = () => {
      const kind = kindSelect.value;
      if (kind === branchKind) return;
      branchKind = kind;
      const attrs = kind === element.kind ? (element.attrs as Record<string, unknown>) : {};
      branch = BRANCH_KINDS.has(kind) ? buildBranchEditor(kind, attrs ?? {}, this.options.scenes(), () => saver.touch()) : null;
      branchHost.replaceChildren(branch?.element ?? "");
    };
    const syncKind = () => {
      const kind = kindSelect.value;
      syncBranch();
      block.className = `block kind-${kind}`;
      textarea.placeholder = `${kindLabel(kind)}を入力`;
      const showSpeaker = kind === "Dialogue" || kind === "CharacterAction" || speaker.value !== "";
      speaker.hidden = !showSpeaker;
    };
    syncKind();

    kindSelect.addEventListener("change", () => {
      syncKind();
      saver.touch();
    });
    speaker.addEventListener("input", () => saver.touch());
    textarea.addEventListener("input", () => {
      autoGrow(textarea);
      saver.touch();
    });
    textarea.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        void this.addElement(kindSelect.value, index + 1);
      }
    });
    // フォーカスが外れたら待たずに保存する。
    block.addEventListener("focusout", () => {
      if (saver.dirty && !saver.conflict) void saver.flush();
    });

    const tool = (label: string, title: string, onclick: () => void, disabled = false) =>
      h("button", { type: "button", class: "tool", title, "aria-label": title, disabled, onclick }, label);
    const last = index === scene.elements.length - 1;
    block.append(
      h(
        "div", { class: "block-head" },
        kindSelect,
        speaker,
        h(
          "div", { class: "block-tools" },
          tool("↑", "上へ移動", () => void this.moveElement(element.id, -1), index === 0),
          tool("↓", "下へ移動", () => void this.moveElement(element.id, 1), last),
          tool("＋", "下に追加 (Ctrl+Enter)", () => void this.addElement(kindSelect.value, index + 1)),
          tool("✕", "削除", () => void this.deleteElement(element.id, textarea.value)),
        ),
      ),
      textarea,
      branchHost,
      notice,
    );
    return block;
  }

  private focusBlock(id: string, selector: string): void {
    const target = this.root.querySelector<HTMLElement>(`.block[data-id="${id}"] ${selector}`);
    target?.focus();
  }

  private async addElement(kind: string, position: number): Promise<void> {
    const scene = this.scene;
    if (!scene) return;
    await this.structural(async () => {
      const result = await api.createElement(scene.id, { kind, speaker: "", body: "", position });
      await this.reload();
      this.focusBlock(result.element.id, kind === "Dialogue" || kind === "CharacterAction" ? ".speaker" : ".body");
    });
  }

  private async moveElement(id: string, delta: -1 | 1): Promise<void> {
    const scene = this.scene;
    if (!scene) return;
    await this.structural(async () => {
      const ids = scene.elements.map((e) => e.id);
      const from = ids.indexOf(id);
      const to = from + delta;
      if (from < 0 || to < 0 || to >= ids.length) return;
      ids.splice(to, 0, ...ids.splice(from, 1));
      await api.reorderElements(scene.id, ids);
      await this.reload();
      this.focusBlock(id, ".body");
    });
  }

  private async deleteElement(id: string, body: string): Promise<void> {
    if (body.trim() !== "" && !(await askConfirm("この要素を削除しますか？"))) return;
    await this.structural(async () => {
      await api.deleteElement(id);
      await this.reload();
    });
  }

  // ---- Markdown ソース編集 --------------------------------------------

  private async renderMarkdown(body: HTMLElement, scene: SceneDetail): Promise<void> {
    const source = await api.getSceneMarkdown(scene.id);
    if (this.scene !== scene) return;
    let revision = source.revision;
    const { SourceEditor } = await import("./sourceEditor");
    if (this.scene !== scene) return;
    let suppress = false;
    const host = h("div", { class: "source" });
    const sourceEditor = new SourceEditor({
      parent: host,
      text: source.markdown,
      ariaLabel: "場面の Markdown ソース",
      placeholder: "地の文はそのまま書きます。空行で区切ると別の要素になります。",
      onEdit: () => {
        if (!suppress) saver.touch();
      },
      onBlur: () => {
        if (saver.dirty && !saver.conflict) void saver.flush();
      },
    });
    this.disposeSource = () => sourceEditor.destroy();
    const notice = h("div", { class: "block-notice" });
    const saver = new AutoSaver(
      MARKDOWN_SAVE_DELAY,
      async () => {
        const detail = await api.putSceneMarkdown(scene.id, revision, sourceEditor.text);
        revision = detail.revision;
        scene.revision = detail.revision;
        scene.elements = detail.elements;
      },
      () => {
        this.reportState();
        if (!saver.conflict) {
          notice.replaceChildren(saver.error ? h("p", { class: "error-text" }, `保存できません: ${saver.error}`) : "");
        }
      },
      () => void showConflict(),
    );
    this.savers.push(saver);

    const showConflict = async () => {
      let latest: { revision: number; markdown: string };
      try {
        latest = await api.getSceneMarkdown(scene.id);
      } catch (e) {
        notice.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
        return;
      }
      notice.replaceChildren(
        h("p", { class: "error-text" }, "この場面は他の画面・端末で先に変更されています。どちらの内容を残すか選んでください。"),
        h("div", { class: "conflict-server" }, h("strong", {}, "先に保存された内容"), h("pre", {}, latest.markdown)),
        h(
          "div", { class: "conflict-actions" },
          h("button", {
            type: "button",
            onclick: () => {
              revision = latest.revision;
              notice.replaceChildren();
              saver.resolve(true);
            },
          }, "自分の内容を残す"),
          h("button", {
            type: "button",
            onclick: () => {
              revision = latest.revision;
              suppress = true;
              sourceEditor.setText(latest.markdown);
              suppress = false;
              notice.replaceChildren();
              saver.resolve(false);
            },
          }, "先に保存された内容にする"),
        ),
      );
    };

    body.append(
      notice,
      host,
      h(
        "p", { class: "hint" },
        "空行で区切った 1 かたまりが 1 つの要素になります。",
        h("code", {}, "**名前**: 台詞"), " ",
        h("code", {}, "[動作:名前] …"), " ",
        h("code", {}, "[演出] …"), " ",
        h("code", {}, "[選択肢] …"),
        " 以外は地の文として扱います。",
      ),
    );
  }
}
