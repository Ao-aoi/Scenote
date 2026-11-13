// 参照ファイル（世界観・キャラクター設定など）のエディタ。

import { api } from "./api";
import { askConfirm, h } from "./dom";
import { AutoSaver, errorMessage, type SaveState } from "./sceneEditor";
import type { ReferenceFile } from "./types";

const SAVE_DELAY = 1000;

export interface FileEditorOptions {
  onStateChange: (state: SaveState, message?: string) => void;
  /** 名前・固定設定の変更や削除で、一覧の更新が必要になったとき。 */
  onListChanged: () => void;
  onDeleted: () => void;
}

export class FileEditor {
  private file: ReferenceFile | null = null;
  private saver: AutoSaver | null = null;
  private dispose: (() => void) | null = null;
  private loadToken = 0;

  constructor(
    private readonly root: HTMLElement,
    private readonly options: FileEditorOptions,
  ) {}

  get fileId(): string | null {
    return this.file?.id ?? null;
  }

  async flush(): Promise<boolean> {
    return this.saver ? this.saver.flush() : true;
  }

  hasUnsavedChanges(): boolean {
    return this.saver !== null && this.saver.state !== "saved";
  }

  private report(): void {
    this.options.onStateChange(this.saver?.state ?? "saved", this.saver?.error ?? undefined);
  }

  async open(fileId: string | null): Promise<boolean> {
    if (!(await this.flush())) {
      const discard = await askConfirm("保存できていない変更があります。破棄して移動しますか？", "破棄して移動");
      if (!discard) return false;
    }
    const token = ++this.loadToken;
    this.dispose?.();
    this.dispose = null;
    this.saver = null;
    this.file = null;
    this.root.replaceChildren();
    this.report();
    if (fileId === null) return true;
    try {
      const file = await api.getFile(fileId);
      if (token !== this.loadToken) return true;
      await this.render(file);
    } catch (e) {
      if (token === this.loadToken) {
        this.root.replaceChildren(h("p", { class: "empty error-text" }, errorMessage(e)));
      }
    }
    return true;
  }

  /** サーバー側で内容が変わった後（AI 提案の承認など）に、表示を最新にする。 */
  async syncFromServer(): Promise<void> {
    const id = this.fileId;
    if (id && (await this.flush())) await this.open(id);
  }

  private async render(initial: ReferenceFile): Promise<void> {
    const { SourceEditor } = await import("./sourceEditor");
    let file = initial;
    this.file = file;
    let suppress = false;

    const name = h("input", {
      class: "scene-title",
      type: "text",
      value: file.name,
      maxLength: 200,
      "aria-label": "ファイル名",
    });
    const pinned = h("input", { type: "checkbox", checked: file.pinned });
    const notice = h("div", { class: "block-notice" });
    const host = h("div", { class: "source" });

    const saver = new AutoSaver(
      SAVE_DELAY,
      async () => {
        const listChanged = name.value.trim() !== file.name || pinned.checked !== file.pinned;
        file = await api.updateFile(file.id, file.revision, {
          name: name.value.trim() || file.name,
          content: source.text,
          pinned: pinned.checked,
        });
        this.file = file;
        if (listChanged) this.options.onListChanged();
      },
      () => {
        this.report();
        if (!saver.conflict) {
          notice.replaceChildren(saver.error ? h("p", { class: "error-text" }, `保存できません: ${saver.error}`) : "");
        }
      },
      () => void showConflict(),
    );
    this.saver = saver;

    const source = new SourceEditor({
      parent: host,
      text: file.content,
      ariaLabel: "参照ファイルの本文",
      placeholder: "世界観、キャラクター設定、文体の方針などを Markdown で書きます。",
      onEdit: () => {
        if (!suppress) saver.touch();
      },
      onBlur: () => {
        if (saver.dirty && !saver.conflict) void saver.flush();
      },
    });
    this.dispose = () => source.destroy();

    const showConflict = async () => {
      let latest: ReferenceFile;
      try {
        latest = await api.getFile(file.id);
      } catch (e) {
        notice.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
        return;
      }
      const adopt = (keepMine: boolean) => {
        file = latest;
        this.file = latest;
        if (!keepMine) {
          suppress = true;
          source.setText(latest.content);
          suppress = false;
          name.value = latest.name;
          pinned.checked = latest.pinned;
        }
        notice.replaceChildren();
        saver.resolve(keepMine);
      };
      notice.replaceChildren(
        h("p", { class: "error-text" }, "このファイルは他の画面・端末、または AI 提案の承認によって先に変更されています。どちらの内容を残すか選んでください。"),
        h("div", { class: "conflict-server" }, h("strong", {}, "先に保存された内容"), h("pre", {}, latest.content)),
        h(
          "div", { class: "conflict-actions" },
          h("button", { type: "button", onclick: () => adopt(true) }, "自分の内容を残す"),
          h("button", { type: "button", onclick: () => adopt(false) }, "先に保存された内容にする"),
        ),
      );
    };

    name.addEventListener("input", () => saver.touch());
    name.addEventListener("change", () => void saver.flush());
    pinned.addEventListener("change", () => {
      saver.touch();
      void saver.flush();
    });

    const remove = h("button", {
      type: "button",
      class: "danger-text",
      onclick: async () => {
        if (!(await askConfirm(`参照ファイル「${file.name}」を削除しますか？元に戻せません。`))) return;
        try {
          await api.deleteFile(file.id);
          saver.resolve(false);
          saver.dirty = false;
          this.options.onDeleted();
        } catch (e) {
          notice.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
        }
      },
    }, "削除");

    this.root.replaceChildren(
      h("div", { class: "scene-header" }, name, remove),
      h(
        "label", { class: "check file-pin" },
        pinned,
        "常に AI に参照させる（プロジェクト共通）",
      ),
      h("p", { class: "hint" }, "オフの場合は、チャットで指定したとき、または AI が依頼に関係すると判断したときだけ参照されます。"),
      notice,
      host,
    );
    this.report();
  }
}
