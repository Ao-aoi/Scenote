// AI チャットパネル: 複数チャット、依頼の送信、変更提案の差分レビュー（承認・却下）。

import { api, ApiError } from "./api";
import { askConfirm, askText, h, openDialog, openMenu, toast } from "./dom";
import { errorMessage } from "./sceneEditor";
import {
  kindLabel,
  type Chat,
  type ChatDetail,
  type ChatMessage,
  type DiffSegment,
  type Proposal,
  type ProposalContent,
  type ReferenceFile,
  type UsageStatus,
} from "./types";

const lastChatKey = (projectId: string) => `scenote.lastChat.${projectId}`;

const SOURCE_LABELS: Record<string, string> = {
  request: "今回指定",
  chat: "チャット指定",
  project: "共通",
  auto: "自動選択",
};

const OP_LABELS: Record<string, string> = {
  update_element: "要素の変更",
  insert_element: "要素の追加",
  delete_element: "要素の削除",
  update_file: "参照ファイルの変更",
  create_file: "参照ファイルの作成",
};

const STATUS_LABELS: Record<string, string> = {
  approved: "承認済み（反映しました）",
  rejected: "却下済み",
  stale: "適用できません（提案の後に対象が変更・削除されました）",
};

export interface ChatPanelOptions {
  /** 依頼時に開いている場面（要素への提案の対象）。 */
  currentSceneId: () => string | null;
  /** 送信・承認の前に、編集中の内容を保存する。保存できなければ false。 */
  flushEditors: () => Promise<boolean>;
  /** 提案の承認によってサーバー側のデータが変わったとき。 */
  onApplied: (proposal: Proposal) => Promise<void>;
  openAiSettings: () => void;
}

function describeContent(c: ProposalContent | null): string {
  if (!c) return "";
  if (c.kind === undefined) return c.name ?? "";
  return c.speaker ? `${kindLabel(c.kind)}（${c.speaker}）` : kindLabel(c.kind);
}

/** 差分の片側（変更前または変更後）を、変更箇所を強調して描画する。 */
export function renderSide(diff: DiffSegment[], side: "before" | "after"): HTMLElement {
  const skip = side === "before" ? "insert" : "delete";
  const pre = h("pre", { class: `diff-text diff-${side}` });
  for (const seg of diff) {
    if (seg.op === skip) continue;
    pre.append(seg.op === "equal" ? seg.text : h(side === "before" ? "del" : "ins", {}, seg.text));
  }
  if (!pre.hasChildNodes()) pre.append(h("span", { class: "muted" }, "（空）"));
  return pre;
}

export class ChatPanel {
  private projectId: string | null = null;
  private chats: Chat[] = [];
  private chat: ChatDetail | null = null;
  private files: ReferenceFile[] = [];
  private include = new Set<string>();
  private exclude = new Set<string>();
  private sending = false;
  private usage: UsageStatus | null = null;

  private readonly select = h("select", { class: "chat-select", "aria-label": "チャット" });
  private readonly menuButton = h("button", { type: "button", class: "icon", "aria-label": "チャットの操作" }, "⋯");
  private readonly banner = h("div", { class: "chat-banner" });
  private readonly list = h("div", { class: "chat-messages", "aria-live": "polite" });
  private readonly input = h("textarea", {
    class: "chat-input",
    rows: 3,
    placeholder: "AI への依頼を入力（Ctrl+Enter で送信）",
    "aria-label": "AI への依頼",
  });
  private readonly sendButton = h("button", { type: "button", class: "primary" }, "送信");
  private readonly refButton = h("button", { type: "button" }, "参照ファイル");
  private readonly refSummary = h("span", { class: "muted ref-summary" });

  constructor(
    root: HTMLElement,
    private readonly options: ChatPanelOptions,
  ) {
    root.append(
      h("div", { class: "chat-head" }, this.select, this.menuButton),
      this.banner,
      this.list,
      h(
        "div", { class: "chat-compose" },
        this.input,
        h("div", { class: "chat-actions" }, this.refButton, this.refSummary, this.sendButton),
      ),
    );
    this.select.addEventListener("change", () => void this.guard(() => this.onSelect()));
    this.menuButton.addEventListener("click", () => this.openChatMenu());
    this.sendButton.addEventListener("click", () => void this.send());
    this.refButton.addEventListener("click", () => void this.guard(() => this.editRequestFiles()));
    this.input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        void this.send();
      }
    });
  }

  private async guard(action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  }

  /** プロジェクトを切り替える。 */
  async setProject(projectId: string | null): Promise<void> {
    this.projectId = projectId;
    this.chat = null;
    this.chats = [];
    this.include.clear();
    this.exclude.clear();
    if (projectId) {
      this.chats = await api.listChats(projectId);
      const remembered = localStorage.getItem(lastChatKey(projectId));
      const initial = this.chats.find((c) => c.id === remembered) ?? this.chats[this.chats.length - 1];
      if (initial) this.chat = await api.getChat(initial.id);
      void this.refreshUsage();
    }
    this.render();
  }

  /** 他の端末でチャットや提案が変わったときに呼ぶ。入力中の依頼文はそのまま残す。 */
  async refreshFromRemote(): Promise<void> {
    if (!this.projectId || this.sending) return;
    const projectId = this.projectId;
    const chats = await api.listChats(projectId);
    if (this.projectId !== projectId || this.sending) return;
    this.chats = chats;
    if (this.chat && chats.some((c) => c.id === this.chat?.id)) {
      this.chat = await api.getChat(this.chat.id);
    } else if (this.chat) {
      this.chat = null;
    }
    const atBottom = this.list.scrollHeight - this.list.scrollTop - this.list.clientHeight < 40;
    this.render();
    if (atBottom) this.list.scrollTop = this.list.scrollHeight;
    void this.refreshUsage();
  }

  async refreshUsage(): Promise<void> {
    try {
      this.usage = await api.getUsage();
    } catch {
      // 使用量を取得できなくても、チャット自体は使える。
    }
    this.renderBanner();
  }

  private async onSelect(): Promise<void> {
    const value = this.select.value;
    if (value === "__new__") {
      await this.createChat();
    } else if (value) {
      await this.openChat(value);
    }
  }

  private async openChat(id: string): Promise<void> {
    this.chat = await api.getChat(id);
    if (this.projectId) localStorage.setItem(lastChatKey(this.projectId), id);
    this.include.clear();
    this.exclude.clear();
    this.render();
  }

  private async createChat(): Promise<void> {
    if (!this.projectId) return;
    const title = await askText("新しいチャットの名前", `チャット${this.chats.length + 1}`, "作成");
    if (!title) {
      this.renderHead();
      return;
    }
    const chat = await api.createChat(this.projectId, title);
    this.chats = await api.listChats(this.projectId);
    await this.openChat(chat.id);
  }

  private openChatMenu(): void {
    const chat = this.chat;
    openMenu(this.menuButton, [
      { label: "新しいチャット…", disabled: !this.projectId, action: () => void this.guard(() => this.createChat()) },
      {
        label: "名前を変更…",
        disabled: !chat,
        action: () => void this.guard(async () => {
          if (!chat) return;
          const title = await askText("チャットの名前", chat.title);
          if (!title) return;
          await api.updateChat(chat.id, { title });
          await this.reloadChats();
        }),
      },
      { label: "追加指示と参照設定…", disabled: !chat, action: () => void this.guard(() => this.editChatSettings()) },
      { label: "AI 設定・使用量…", action: () => this.options.openAiSettings() },
      {
        label: "チャットを削除…",
        danger: true,
        disabled: !chat,
        action: () => void this.guard(async () => {
          if (!chat || !this.projectId) return;
          if (!(await askConfirm(`チャット「${chat.title}」と履歴を削除しますか？承認済みの変更はそのまま残ります。`))) return;
          await api.deleteChat(chat.id);
          await this.setProject(this.projectId);
        }),
      },
    ]);
  }

  private async reloadChats(): Promise<void> {
    if (!this.projectId) return;
    this.chats = await api.listChats(this.projectId);
    if (this.chat) this.chat = await api.getChat(this.chat.id);
    this.render();
  }

  /** チャットごとの追加指示・参照ファイル・自動選択の設定。 */
  private async editChatSettings(): Promise<void> {
    const chat = this.chat;
    if (!chat || !this.projectId) return;
    const files = await api.listFiles(this.projectId);
    const result = await openDialog<{ instructions: string; auto_select: boolean; file_ids: string[] }>((close) => {
      const instructions = h("textarea", { class: "field", rows: 4, value: chat.instructions });
      const auto = h("input", { type: "checkbox", checked: chat.auto_select });
      const boxes = files.map((f) => ({
        id: f.id,
        box: h("input", { type: "checkbox", checked: chat.file_ids.includes(f.id) }),
        label: f.pinned ? `${f.name}（共通で常に参照）` : f.name,
      }));
      return h(
        "form",
        {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close({
              instructions: instructions.value,
              auto_select: auto.checked,
              file_ids: boxes.filter((b) => b.box.checked).map((b) => b.id),
            });
          },
        },
        h("h2", {}, `「${chat.title}」の設定`),
        h("label", { class: "form-row" }, h("span", {}, "追加指示（このチャットの依頼すべてに添えられます）"), instructions),
        h("label", { class: "check" }, auto, "依頼に関係する参照ファイルを AI が自動で選ぶ"),
        h(
          "fieldset", { class: "form-row" },
          h("legend", {}, "このチャットで常に参照するファイル"),
          ...(boxes.length ? boxes.map((b) => h("label", { class: "check" }, b.box, b.label)) : [h("p", { class: "muted" }, "参照ファイルはまだありません。")]),
        ),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "保存"),
        ),
      );
    });
    if (!result) return;
    await api.updateChat(chat.id, result);
    await this.reloadChats();
  }

  /** この依頼に限った参照ファイルの追加・除外。 */
  private async editRequestFiles(): Promise<void> {
    if (!this.projectId) return;
    this.files = await api.listFiles(this.projectId);
    const result = await openDialog<Map<string, string>>((close) => {
      const rows = this.files.map((f) => {
        const current = this.include.has(f.id) ? "include" : this.exclude.has(f.id) ? "exclude" : "default";
        const select = h(
          "select", { class: "field", "aria-label": `${f.name} の扱い` },
          h("option", { value: "default", selected: current === "default" }, "設定どおり"),
          h("option", { value: "include", selected: current === "include" }, "今回は必ず参照"),
          h("option", { value: "exclude", selected: current === "exclude" }, "今回は参照しない"),
        );
        return { id: f.id, select, row: h("label", { class: "ref-row" }, h("span", {}, f.name), select) };
      });
      return h(
        "form",
        {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(new Map(rows.map((r) => [r.id, r.select.value])));
          },
        },
        h("h2", {}, "次の依頼で参照するファイル"),
        h("p", { class: "hint" }, "ここでの指定は次の 1 回の依頼にだけ使われ、チャットやプロジェクトの設定より優先されます。"),
        ...(rows.length ? rows.map((r) => r.row) : [h("p", { class: "muted" }, "参照ファイルはまだありません。左の一覧から作成できます。")]),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "決定"),
        ),
      );
    });
    if (!result) return;
    this.include.clear();
    this.exclude.clear();
    for (const [id, value] of result) {
      if (value === "include") this.include.add(id);
      if (value === "exclude") this.exclude.add(id);
    }
    this.renderCompose();
  }

  private async send(): Promise<void> {
    const content = this.input.value.trim();
    if (!content || this.sending || !this.projectId) return;
    this.sending = true;
    this.renderCompose();
    const pending = h("div", { class: "msg msg-assistant pending" }, "AI が考えています…");
    try {
      if (!(await this.options.flushEditors())) {
        throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから送信してください");
      }
      if (!this.chat) {
        const chat = await api.createChat(this.projectId, "チャット1");
        this.chats = await api.listChats(this.projectId);
        this.chat = await api.getChat(chat.id);
        localStorage.setItem(lastChatKey(this.projectId), chat.id);
        this.render();
      }
      const chat = this.chat;
      this.list.append(h("div", { class: "msg msg-user" }, content), pending);
      pending.scrollIntoView({ block: "end" });
      const exchange = await api.sendMessage(chat.id, {
        content,
        scene_id: this.options.currentSceneId(),
        include_file_ids: [...this.include],
        exclude_file_ids: [...this.exclude],
      });
      if (this.chat?.id === chat.id) {
        chat.messages.push(...exchange.messages);
        this.input.value = "";
        this.include.clear();
        this.exclude.clear();
      }
      this.usage = exchange.usage;
    } catch (e) {
      toast(errorMessage(e), "error");
      if (e instanceof ApiError && e.code === "llm_stopped") void this.refreshUsage();
    } finally {
      this.sending = false;
      this.render();
      this.list.lastElementChild?.scrollIntoView({ block: "nearest" });
    }
  }

  // ---- 描画 -----------------------------------------------------------

  private render(): void {
    this.renderHead();
    this.renderBanner();
    this.renderMessages();
    this.renderCompose();
  }

  private renderHead(): void {
    this.select.replaceChildren(
      ...(this.chats.length === 0 ? [h("option", { value: "" }, "（チャットなし）")] : []),
      ...this.chats.map((c) => h("option", { value: c.id, selected: c.id === this.chat?.id }, c.title)),
      h("option", { value: "__new__" }, "＋ 新しいチャット…"),
    );
    this.select.disabled = !this.projectId;
  }

  private renderBanner(): void {
    const usage = this.usage;
    if (!usage || usage.state === "ok") {
      this.banner.replaceChildren();
      return;
    }
    const used = `${usage.total_tokens.toLocaleString()} / ${usage.monthly_token_limit.toLocaleString()} トークン`;
    if (usage.state === "warning") {
      this.banner.replaceChildren(h("p", { class: "warn-text" }, `今月の API 使用量が上限に近づいています（${used}）。`));
      return;
    }
    this.banner.replaceChildren(
      h("p", { class: "error-text" }, `今月の API 使用量が上限に達したため、AI の利用を停止しています（${used}）。`),
      h(
        "div", { class: "conflict-actions" },
        h("button", {
          type: "button",
          onclick: () => void this.guard(async () => {
            const ok = await askConfirm("上限を超えたまま AI の利用を再開しますか？今月は自動停止されなくなります。", "再開する");
            if (!ok) return;
            this.usage = await api.resumeUsage();
            this.renderBanner();
          }),
        }, "利用を再開"),
        h("button", { type: "button", onclick: () => this.options.openAiSettings() }, "上限を変更"),
      ),
    );
  }

  private renderCompose(): void {
    const enabled = this.projectId !== null && !this.sending;
    this.input.disabled = !enabled;
    this.sendButton.disabled = !enabled;
    this.refButton.disabled = !enabled;
    this.sendButton.textContent = this.sending ? "送信中…" : "送信";
    const parts = [];
    if (this.include.size) parts.push(`追加 ${this.include.size}`);
    if (this.exclude.size) parts.push(`除外 ${this.exclude.size}`);
    this.refSummary.textContent = parts.join("・");
  }

  private renderMessages(): void {
    if (!this.projectId) {
      this.list.replaceChildren(h("p", { class: "empty" }, "プロジェクトを開くと AI に相談できます。"));
      return;
    }
    const messages = this.chat?.messages ?? [];
    if (messages.length === 0) {
      this.list.replaceChildren(
        h(
          "p", { class: "empty" },
          "開いている場面について AI に相談や推敲を依頼できます。AI の変更は提案として表示され、承認したものだけが反映されます。",
        ),
      );
      return;
    }
    this.list.replaceChildren(...messages.map((m) => this.renderMessage(m)));
  }

  private renderMessage(message: ChatMessage): HTMLElement {
    if (message.role === "user") return h("div", { class: "msg msg-user" }, message.content);
    const node = h("div", { class: "msg msg-assistant" }, h("div", { class: "msg-text" }, message.content));
    if (message.referenced_files.length) {
      node.append(
        h(
          "p", { class: "msg-refs" },
          "参照: ",
          message.referenced_files.map((f) => `${f.name}（${SOURCE_LABELS[f.source] ?? f.source}）`).join("、"),
        ),
      );
    }
    if (message.warnings.length) {
      node.append(h("ul", { class: "msg-warnings" }, ...message.warnings.map((w) => h("li", {}, w))));
    }
    const pending = message.proposals.filter((p) => p.status === "pending");
    if (pending.length >= 2) {
      node.append(
        h(
          "div", { class: "proposal-bulk" },
          h("span", { class: "muted" }, `未処理の提案 ${pending.length} 件`),
          h("button", { type: "button", onclick: () => void this.decideAll(message, "approve") }, "すべて承認"),
          h("button", { type: "button", onclick: () => void this.decideAll(message, "reject") }, "すべて却下"),
        ),
      );
    }
    for (const proposal of message.proposals) node.append(this.renderProposal(message, proposal));
    return node;
  }

  private renderProposal(message: ChatMessage, p: Proposal): HTMLElement {
    const target = describeContent(p.after ?? p.before);
    const card = h("section", { class: `proposal proposal-${p.status}` });
    card.append(h("h3", {}, OP_LABELS[p.op] ?? p.op, target ? h("span", { class: "muted" }, ` — ${target}`) : null));
    if (p.reason) card.append(h("p", { class: "proposal-reason" }, p.reason));

    const meta: string[] = [];
    if (p.op === "update_element" && p.before && p.after) {
      if (p.before.kind !== p.after.kind) meta.push(`種類: ${kindLabel(p.before.kind ?? "")} → ${kindLabel(p.after.kind ?? "")}`);
      if (p.before.speaker !== p.after.speaker) meta.push(`キャラクター: ${p.before.speaker || "（なし）"} → ${p.after.speaker || "（なし）"}`);
    }
    if (meta.length) card.append(h("p", { class: "proposal-meta" }, meta.join(" / ")));

    const columns = h("div", { class: "diff" });
    if (p.before) columns.append(h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "変更前"), renderSide(p.diff, "before")));
    if (p.after) columns.append(h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "変更後"), renderSide(p.diff, "after")));
    card.append(columns);

    if (p.status === "pending") {
      card.append(
        h(
          "div", { class: "proposal-actions" },
          h("button", { type: "button", class: "primary", onclick: () => void this.decide(message, p, "approve") }, "承認して反映"),
          p.op !== "delete_element"
            ? h("button", { type: "button", onclick: () => void this.decideEdited(message, p) }, "修正して採用")
            : null,
          h("button", { type: "button", onclick: () => void this.decide(message, p, "reject") }, "却下"),
        ),
      );
    } else {
      const label = p.final_content ? "修正して採用しました" : STATUS_LABELS[p.status] ?? p.status;
      card.append(h("p", { class: `proposal-status status-${p.status}` }, p.reject_reason ? `${label} — 理由: ${p.reject_reason}` : label));
    }
    return card;
  }

  /** 提案の内容を修正するダイアログ。キャンセルした場合は null。 */
  private askEdit(p: Proposal): Promise<ProposalContent | null> {
    const after = p.after ?? {};
    const isFile = p.op === "update_file" || p.op === "create_file";
    return openDialog<ProposalContent>((close) => {
      const body = h("textarea", { class: "field edit-body", rows: 8, value: (isFile ? after.content : after.body) ?? "" });
      const speaker = h("input", { type: "text", class: "field", maxLength: 100, value: after.speaker ?? "" });
      const name = h("input", { type: "text", class: "field", maxLength: 200, value: after.name ?? "" });
      setTimeout(() => body.focus());
      return h(
        "form",
        {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(
              isFile
                ? { name: name.value.trim() || after.name, content: body.value }
                : { kind: after.kind, speaker: speaker.value.trim(), body: body.value },
            );
          },
        },
        h("h2", {}, "提案を修正して採用"),
        h("p", { class: "hint" }, "修正した内容が反映されます。AI の提案との違いは、執筆嗜好の分析に使われます。"),
        p.op === "create_file" ? h("label", { class: "form-row" }, h("span", {}, "ファイル名"), name) : null,
        !isFile && (after.kind === "Dialogue" || after.kind === "CharacterAction" || after.speaker)
          ? h("label", { class: "form-row" }, h("span", {}, "キャラクター名"), speaker)
          : null,
        h("label", { class: "form-row" }, h("span", {}, isFile ? "ファイルの内容" : `${kindLabel(after.kind ?? "")}の本文`), body),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "この内容で反映"),
        ),
      );
    });
  }

  private async decideEdited(message: ChatMessage, p: Proposal): Promise<void> {
    await this.guard(async () => {
      const content = await this.askEdit(p);
      if (!content) return;
      if (!(await this.options.flushEditors())) {
        throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから承認してください");
      }
      try {
        const updated = await api.approveProposalEdited(p.id, content);
        message.proposals = message.proposals.map((x) => (x.id === updated.id ? updated : x));
        if (updated.status === "approved") await this.options.onApplied(updated);
        if (updated.status === "stale") toast("提案の後に対象が変更または削除されたため、反映しませんでした", "error");
      } finally {
        this.renderMessages();
      }
    });
  }

  /** 却下理由を尋ねる（任意）。キャンセルした場合は null。 */
  private askRejectReason(): Promise<string | null> {
    return openDialog<string>((close) => {
      const reason = h("input", { type: "text", class: "field", maxLength: 500, placeholder: "例: 口調がキャラクターに合わない" });
      setTimeout(() => reason.focus());
      return h(
        "form",
        {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close(reason.value.trim());
          },
        },
        h("h2", {}, "提案を却下"),
        h("label", { class: "form-row" }, h("span", {}, "理由（任意）"), reason),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "danger" }, "却下する"),
        ),
      );
    });
  }

  private async apply(message: ChatMessage, p: Proposal, action: "approve" | "reject", reason: string): Promise<void> {
    const updated = action === "approve" ? await api.approveProposal(p.id) : await api.rejectProposal(p.id, reason);
    message.proposals = message.proposals.map((x) => (x.id === updated.id ? updated : x));
    if (updated.status === "approved") await this.options.onApplied(updated);
    if (updated.status === "stale") {
      toast("提案の後に対象が変更または削除されたため、反映しませんでした", "error");
    }
  }

  private async decide(message: ChatMessage, p: Proposal, action: "approve" | "reject"): Promise<void> {
    await this.guard(async () => {
      let reason = "";
      if (action === "reject") {
        const answer = await this.askRejectReason();
        if (answer === null) return;
        reason = answer;
      } else if (!(await this.options.flushEditors())) {
        throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから承認してください");
      }
      try {
        await this.apply(message, p, action, reason);
      } finally {
        this.renderMessages();
      }
    });
  }

  private async decideAll(message: ChatMessage, action: "approve" | "reject"): Promise<void> {
    await this.guard(async () => {
      let reason = "";
      if (action === "reject") {
        const answer = await this.askRejectReason();
        if (answer === null) return;
        reason = answer;
      } else if (!(await this.options.flushEditors())) {
        throw new Error("編集中の内容を保存できていません。競合やエラーを解消してから承認してください");
      }
      try {
        // 提案時の順序で 1 件ずつ処理する。
        for (const p of message.proposals.filter((x) => x.status === "pending")) {
          await this.apply(message, p, action, reason);
        }
      } finally {
        this.renderMessages();
      }
    });
  }
}
