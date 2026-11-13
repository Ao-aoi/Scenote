// 履歴とスナップショットの画面。
//
// - スナップショット: 任意の時点の保存（メモつき）、現在との比較、復元（復元前の状態は自動保存）。
// - 編集履歴: 要素・参照ファイルの変更を、由来（人間・AI 提案の承認・修正して採用・復元）つきで表示する。

import { api } from "./api";
import { askConfirm, h, openDialog, toast } from "./dom";
import { renderSide } from "./chat";
import { errorMessage } from "./sceneEditor";
import { kindLabel, type CompareItem, type HistoryEntry, type SnapshotInfo } from "./types";

const ACTOR_LABELS: Record<string, string> = {
  human: "人間が編集",
  ai_approved: "AI 提案を承認",
  ai_modified: "AI 提案を修正して採用",
  restore: "スナップショットから復元",
};

const ACTION_LABELS: Record<string, string> = {
  create: "追加",
  update: "変更",
  delete: "削除",
  reorder: "並べ替え",
  restore: "復元",
};

const TARGET_LABELS: Record<string, string> = {
  element: "要素",
  file: "参照ファイル",
  scene: "場面",
  project: "プロジェクト",
};

const PAGE = 50;

/** サーバーの UTC 時刻を、この端末の時刻で短く表示する。 */
export function formatTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("ja-JP", { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

function diffColumns(diff: HistoryEntry["diff"], hasBefore: boolean, hasAfter: boolean): HTMLElement {
  return h(
    "div", { class: "diff" },
    hasBefore ? h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "変更前"), renderSide(diff, "before")) : null,
    hasAfter ? h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "変更後"), renderSide(diff, "after")) : null,
  );
}

export interface HistoryViewOptions {
  /** 開いている場面（「この場面のみ」の絞り込みに使う）。 */
  currentSceneId: () => string | null;
  /** 編集中の内容を保存する。保存できなければ false。 */
  flushEditors: () => Promise<boolean>;
  /** 復元によってプロジェクトの内容が変わったとき。 */
  onRestored: () => Promise<void>;
}

export class HistoryView {
  private projectId: string | null = null;
  private tab: "snapshots" | "history" = "snapshots";
  private onlyScene = true;
  private entries: HistoryEntry[] = [];
  private hasMore = false;

  constructor(
    private readonly root: HTMLElement,
    private readonly options: HistoryViewOptions,
  ) {}

  async open(projectId: string): Promise<void> {
    this.projectId = projectId;
    await this.render();
  }

  private async guard(action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  }

  private async render(): Promise<void> {
    if (!this.projectId) return;
    const tab = (id: "snapshots" | "history", label: string) =>
      h("button", {
        type: "button",
        class: this.tab === id ? "seg active" : "seg",
        "aria-pressed": String(this.tab === id),
        onclick: () => void this.guard(async () => {
          this.tab = id;
          await this.render();
        }),
      }, label);
    const body = h("div", { class: "history-body" }, h("p", { class: "muted" }, "読み込み中…"));
    this.root.replaceChildren(
      h(
        "div", { class: "scene-header" },
        h("h1", { class: "view-title" }, "履歴とスナップショット"),
        h("div", { class: "segmented", role: "group", "aria-label": "表示" }, tab("snapshots", "スナップショット"), tab("history", "編集履歴")),
      ),
      body,
    );
    try {
      if (this.tab === "snapshots") await this.renderSnapshots(body);
      else await this.renderHistory(body, true);
    } catch (e) {
      body.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
    }
  }

  // ---- スナップショット -------------------------------------------------

  private async renderSnapshots(body: HTMLElement): Promise<void> {
    const projectId = this.projectId!;
    const snapshots = await api.listSnapshots(projectId);
    const note = h("input", { type: "text", class: "field", maxLength: 1000, placeholder: "メモ（任意）例: 第1章の初稿" });
    const create = h("form", {
      class: "snapshot-create",
      onsubmit: (e: Event) => {
        e.preventDefault();
        void this.guard(async () => {
          if (!(await this.options.flushEditors())) {
            throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから保存してください");
          }
          await api.createSnapshot(projectId, note.value);
          toast("スナップショットを保存しました");
          await this.render();
        });
      },
    }, note, h("button", { type: "submit", class: "primary" }, "現在の状態を保存"));

    const items = snapshots.map((s) => this.renderSnapshot(s));
    body.replaceChildren(
      h("p", { class: "hint" }, "プロジェクト全体（章・場面・本文・参照ファイル）をその時点のまま保存します。チャット・提案・嗜好プロファイルは対象外です。"),
      create,
      items.length ? h("div", { class: "snapshot-list" }, ...items) : h("p", { class: "empty" }, "スナップショットはまだありません。"),
    );
  }

  private renderSnapshot(s: SnapshotInfo): HTMLElement {
    const detail = h("div", { class: "snapshot-detail" });
    const title = s.note || (s.kind === "before_restore" ? "復元前の状態（自動保存）" : "（メモなし）");
    const compare = h("button", { type: "button" }, "現在と比較");
    compare.addEventListener("click", () => void this.guard(async () => {
      if (detail.hasChildNodes()) {
        detail.replaceChildren();
        compare.textContent = "現在と比較";
        return;
      }
      if (!(await this.options.flushEditors())) {
        throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから比較してください");
      }
      detail.replaceChildren(h("p", { class: "muted" }, "比較しています…"));
      const items = await api.compareSnapshot(s.id);
      detail.replaceChildren(...this.renderCompare(items));
      compare.textContent = "比較を閉じる";
    }));
    return h(
      "section", { class: `snapshot ${s.kind === "before_restore" ? "snapshot-auto" : ""}` },
      h(
        "div", { class: "snapshot-head" },
        h("div", {},
          h("strong", {}, title),
          h("div", { class: "muted snapshot-meta" }, `${formatTime(s.created_at)} ・ 場面 ${s.scene_count} ・ 要素 ${s.element_count} ・ 参照ファイル ${s.file_count}`)),
        h(
          "div", { class: "snapshot-actions" },
          compare,
          h("button", { type: "button", onclick: () => void this.restore(s, title) }, "この状態に戻す"),
          h("button", {
            type: "button",
            class: "danger-text",
            onclick: () => void this.guard(async () => {
              if (!(await askConfirm(`スナップショット「${title}」を削除しますか？`))) return;
              await api.deleteSnapshot(s.id);
              await this.render();
            }),
          }, "削除"),
        ),
      ),
      detail,
    );
  }

  private renderCompare(items: CompareItem[]): HTMLElement[] {
    if (items.length === 0) return [h("p", { class: "muted" }, "現在の状態と違いはありません。")];
    const statusLabel = { added: "現在のみ（後から追加）", removed: "スナップショットのみ（後から削除）", changed: "変更あり" };
    return [
      h("p", { class: "hint" }, "左がスナップショットの時点、右が現在の内容です。場面の本文は Markdown 表現で比較しています。"),
      ...items.map((item) =>
        h(
          "div", { class: "compare-item" },
          h("h3", {}, `${item.item_type === "scene" ? "場面" : "参照ファイル"}: ${item.title}`, h("span", { class: "muted" }, ` — ${statusLabel[item.status]}`)),
          h(
            "div", { class: "diff" },
            h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "スナップショット"), renderSide(item.diff, "before")),
            h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "現在"), renderSide(item.diff, "after")),
          ),
        ),
      ),
    ];
  }

  private async restore(s: SnapshotInfo, title: string): Promise<void> {
    await this.guard(async () => {
      const ok = await askConfirm(
        `「${title}」の状態に戻しますか？現在の状態は、復元の直前に自動でスナップショットとして保存されるため、後から戻せます。`,
        "この状態に戻す",
      );
      if (!ok) return;
      if (!(await this.options.flushEditors())) {
        throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから復元してください");
      }
      await api.restoreSnapshot(s.id);
      toast("復元しました。復元前の状態はスナップショットに保存されています");
      await this.options.onRestored();
      await this.render();
    });
  }

  // ---- 編集履歴 -------------------------------------------------------

  private async renderHistory(body: HTMLElement, reset: boolean): Promise<void> {
    const projectId = this.projectId!;
    const sceneId = this.options.currentSceneId();
    const filterScene = this.onlyScene && sceneId !== null;
    if (reset) this.entries = [];
    const before = this.entries.length ? this.entries[this.entries.length - 1].seq : undefined;
    const page = await api.getHistory(projectId, { sceneId: filterScene ? sceneId! : undefined, before, limit: PAGE });
    this.entries.push(...page);
    this.hasMore = page.length === PAGE;

    const toggle = h("input", { type: "checkbox", checked: this.onlyScene, disabled: sceneId === null });
    toggle.addEventListener("change", () => void this.guard(async () => {
      this.onlyScene = toggle.checked;
      await this.render();
    }));
    const limitButton = h("button", { type: "button", class: "link" }, "保持件数の設定");
    limitButton.addEventListener("click", () => void this.guard(() => this.editLimit()));

    const list = this.entries.map((e) => this.renderEntry(e));
    const more = this.hasMore
      ? h("button", {
          type: "button",
          class: "add",
          onclick: () => void this.guard(() => this.renderHistory(body, false)),
        }, "さらに読み込む")
      : "";
    body.replaceChildren(
      h(
        "div", { class: "history-toolbar" },
        h("label", { class: "check" }, toggle, "開いている場面のみ表示"),
        limitButton,
      ),
      h("p", { class: "hint" }, "自動保存による連続した入力は 1 件にまとめて記録しています。過去の状態へ戻すにはスナップショットを使います。"),
      list.length ? h("div", { class: "history-list" }, ...list) : h("p", { class: "empty" }, "履歴はまだありません。"),
      more,
    );
  }

  private renderEntry(e: HistoryEntry): HTMLElement {
    const describe = (v: Record<string, unknown> | null) => {
      if (!v) return "";
      if (typeof v.kind === "string") {
        const speaker = typeof v.speaker === "string" && v.speaker ? `（${v.speaker}）` : "";
        return `${kindLabel(v.kind)}${speaker}`;
      }
      if (typeof v.name === "string") return v.name;
      if (typeof v.title === "string") return v.title;
      return "";
    };
    const subject = describe(e.after ?? e.before);
    const head = h(
      "div", { class: "history-head" },
      h("span", { class: `actor actor-${e.actor}` }, ACTOR_LABELS[e.actor] ?? e.actor),
      h("strong", {}, `${TARGET_LABELS[e.target_type] ?? e.target_type}の${ACTION_LABELS[e.action] ?? e.action}`),
      subject ? h("span", { class: "muted" }, subject) : null,
      h("span", { class: "muted history-time" }, formatTime(e.updated_at)),
    );
    const node = h("section", { class: "history-entry" }, head);
    if (e.action === "reorder") {
      node.append(h("p", { class: "muted" }, "要素の順序を変更しました。"));
    } else if (e.action === "restore") {
      const note = typeof e.after?.note === "string" && e.after.note ? `「${e.after.note}」` : "スナップショット";
      node.append(h("p", { class: "muted" }, `${note}の状態に復元しました。`));
    } else {
      node.append(diffColumns(e.diff, e.before !== null, e.after !== null));
    }
    return node;
  }

  private async editLimit(): Promise<void> {
    const current = await api.getHistorySettings();
    const value = await openDialog<number>((close) => {
      const input = h("input", { type: "number", class: "field", min: "100", max: "1000000", step: "100", value: String(current.max_entries) });
      return h(
        "form",
        {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(Math.floor(Number(input.value)));
          },
        },
        h("h2", {}, "編集履歴の保持件数"),
        h("label", { class: "form-row" }, h("span", {}, "プロジェクトごとの最大件数（超えた分は古いものから削除）"), input),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "保存"),
        ),
      );
    });
    if (value === null) return;
    await api.updateHistorySettings(value);
    toast("保持件数を保存しました");
  }
}
