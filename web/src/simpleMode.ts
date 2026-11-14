// かんたんモード（スマートフォン向け。iPad・PC でも選べる）。
//
// 本文だけを表示し、画面下部の入力欄から書き進める。
// - 送った内容は AI が判断する。
//   - 本文として書いた内容（地の文・台詞・選択肢など）: AI が要素に整えて、そのまま本文に反映する。
//     すぐに「元に戻す」で取り消せる。本人が書いた文章なので確認を挟まず、嗜好の学習にも数えない。
//   - 変更の依頼・展開の相談: AI が案を考え、本文の下に提案として表示する。承認したものだけが反映される。
// - 制作用の画面と同じデータ・提案の仕組みを使う（会話は「かんたん執筆」チャットに残る）。

import { maybePromptAnalysis } from "./analysisPrompt";
import { api, ApiError, setUnauthenticatedHandler } from "./api";
import { renderSide } from "./chat";
import { disconnectThisDevice, openDeviceManager, showPairingScreen } from "./devices";
import { askText, h, openDialog, openMenu, snackbar, toast, type MenuItem } from "./dom";
import { connectEvents } from "./events";
import { openAiSettings } from "./aiSettings";
import { icon } from "./icons";
import { errorMessage, SceneEditor, type SaveState } from "./sceneEditor";
import { kindLabel, type Meta, type Project, type Proposal, type ProjectTree } from "./types";

export type UiMode = "auto" | "simple" | "full";

const MODE_KEY = "scenote.uiMode";
const LAST_PROJECT_KEY = "scenote.lastProject";
const lastSceneKey = (projectId: string) => `scenote.lastScene.${projectId}`;
/** これ以下の幅（スマートフォン）では、自動でかんたんモードにする。 */
export const SIMPLE_MAX_WIDTH = 600;
/** かんたん執筆の会話を残すチャットの名前。 */
const COMPOSE_CHAT_TITLE = "かんたん執筆";

export function preferredMode(): UiMode {
  const v = localStorage.getItem(MODE_KEY);
  return v === "simple" || v === "full" ? v : "auto";
}

export function shouldUseSimple(): boolean {
  const mode = preferredMode();
  return mode === "simple" || (mode === "auto" && window.innerWidth <= SIMPLE_MAX_WIDTH);
}

/** 画面の種類を切り替えて読み込み直す。 */
export function switchMode(mode: "simple" | "full"): void {
  // 自動判定と同じ結果になる場合は「自動」に戻す。
  const auto = window.innerWidth <= SIMPLE_MAX_WIDTH ? "simple" : "full";
  if (mode === auto) localStorage.removeItem(MODE_KEY);
  else localStorage.setItem(MODE_KEY, mode);
  location.reload();
}

interface Deps {
  meta: Meta;
  local: boolean;
  deviceName: string | null;
}

const OP_LABELS: Record<string, string> = {
  update_element: "本文の変更",
  insert_element: "本文の追加",
  delete_element: "本文の削除",
  update_file: "設定ファイルの変更",
  create_file: "設定ファイルの作成",
  update_summary: "場面の概要の変更",
  insert_scene: "場面の追加",
};

export async function startSimple(appRoot: HTMLElement, deps: Deps): Promise<void> {
  let projects: Project[] = [];
  let tree: ProjectTree | null = null;
  let sceneId: string | null = null;
  let chatId: string | null = null;
  let pending: { reply: string; proposals: Proposal[] }[] = [];
  let sending = false;

  const shell = h("div", { class: "simple" });
  const sceneButton = h("button", { type: "button", class: "simple-context", "aria-label": "場面を選ぶ" });
  const status = h("span", { class: "simple-status", role: "status" });
  const menu = h("button", { type: "button", class: "icon", title: "メニュー", "aria-label": "メニュー" }, icon("settings"));
  const scroller = h("main", { class: "simple-scroll" });
  const bodyHost = h("div", { class: "simple-body" });
  const pendingHost = h("div", { class: "simple-pending", "aria-live": "polite" });
  const thinking = h("p", { class: "simple-thinking", hidden: true }, "AI が考えています…");
  const toBottom = h("button", { type: "button", class: "simple-to-bottom", title: "いちばん下へ", "aria-label": "いちばん下へ", hidden: true }, "↓");
  const input = h("textarea", { class: "simple-input", rows: 1, placeholder: "本文や、AI への依頼を入力", "aria-label": "本文や AI への依頼" });
  const send = h("button", { type: "button", class: "simple-send primary", title: "送信", "aria-label": "送信" }, "↑");
  scroller.append(bodyHost, pendingHost, thinking);
  shell.append(
    h("header", { class: "simple-head" }, sceneButton, status, menu),
    scroller,
    toBottom,
    h("footer", { class: "simple-compose" }, input, send),
  );
  appRoot.replaceChildren(shell);

  // ソフトウェアキーボードが出ても入力欄が隠れないよう、見えている高さに画面を合わせる。
  const fit = () => {
    const vv = window.visualViewport;
    shell.style.setProperty("--simple-height", `${vv ? vv.height : window.innerHeight}px`);
    if (vv) window.scrollTo(0, 0);
  };
  window.visualViewport?.addEventListener("resize", fit);
  window.addEventListener("resize", fit);
  fit();

  const STATE_LABELS: Record<SaveState, string> = {
    saved: "保存済み",
    dirty: "未保存",
    saving: "保存中…",
    error: "保存エラー",
    conflict: "競合あり",
  };
  const editor = new SceneEditor(bodyHost, {
    kinds: deps.meta.element_kinds,
    scenes: () => tree?.chapters.flatMap((c) => c.scenes.map((s) => ({ id: s.id, label: `${c.title} / ${s.title}` }))) ?? [],
    onStateChange: (state) => {
      status.textContent = STATE_LABELS[state];
      status.className = `simple-status status-${state}`;
    },
    onSceneRenamed: () => void refreshTree(),
  });

  async function guard(action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 401)) toast(errorMessage(e), "error");
    }
  }

  // ---- スクロール -----------------------------------------------------

  const nearBottom = () => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
  const scrollToBottom = (smooth = true) =>
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  scroller.addEventListener("scroll", () => {
    toBottom.hidden = nearBottom();
  });
  toBottom.addEventListener("click", () => scrollToBottom());

  // ---- 入力欄 ---------------------------------------------------------

  const grow = () => {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight + 2, 160)}px`;
  };
  input.addEventListener("input", grow);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void submit();
    }
  });
  send.addEventListener("click", () => void submit());

  function setSending(value: boolean): void {
    sending = value;
    input.disabled = value;
    send.disabled = value;
    thinking.hidden = !value;
    if (value) scrollToBottom();
  }

  async function ensureChat(): Promise<string> {
    if (chatId) return chatId;
    const chats = await api.listChats(tree!.project.id);
    const found = chats.find((c) => c.title === COMPOSE_CHAT_TITLE);
    chatId = found ? found.id : (await api.createChat(tree!.project.id, COMPOSE_CHAT_TITLE)).id;
    return chatId;
  }

  async function submit(): Promise<void> {
    const content = input.value.trim();
    if (!content || sending || !tree) return;
    if (!sceneId) {
      toast("先に場面を選ぶか、作成してください", "error");
      return;
    }
    if (!(await editor.flush())) {
      toast("本文の変更を保存できていません。競合を解消してから送ってください", "error");
      return;
    }
    setSending(true);
    try {
      const chat = await ensureChat();
      const ex = await api.sendMessage(chat, { content, scene_id: sceneId, include_file_ids: [], exclude_file_ids: [], compose: true });
      input.value = "";
      grow();
      const reply = ex.messages[ex.messages.length - 1];
      const applied = reply.proposals.filter((p) => p.auto_applied && p.status === "approved");
      const open = reply.proposals.filter((p) => p.status === "pending");
      if (applied.length) {
        await editor.syncFromServer();
        scrollToBottom();
        const ids = applied.map((p) => p.result_id).filter((id): id is string => !!id);
        snackbar(`本文に ${applied.length} 件追加しました`, [{ label: "元に戻す", action: () => void undo(ids) }], () => undefined, 8000);
      }
      if (open.length || !applied.length) {
        pending.push({ reply: reply.content, proposals: open });
        renderPending();
        scrollToBottom();
      }
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSending(false);
    }
  }

  /** 自動で追加した要素を取り除く。 */
  async function undo(elementIds: string[]): Promise<void> {
    await guard(async () => {
      if (!(await editor.flush())) return;
      for (const id of elementIds) {
        await api.deleteElement(id).catch(() => undefined);
      }
      await editor.syncFromServer();
      toast("取り消しました");
    });
  }

  // ---- AI の提案（変更の依頼・展開の相談） ------------------------------

  function renderPending(): void {
    pendingHost.replaceChildren(
      ...pending.map((group, gi) => {
        const card = h("section", { class: "simple-reply" }, h("p", { class: "simple-reply-text" }, group.reply));
        for (const p of group.proposals) card.append(proposalCard(p, gi));
        if (group.proposals.every((p) => p.status !== "pending")) {
          const close = h("button", { type: "button", class: "link" }, "閉じる");
          close.addEventListener("click", () => {
            pending.splice(gi, 1);
            renderPending();
          });
          card.append(h("div", { class: "simple-reply-actions" }, close));
        }
        return card;
      }),
    );
  }

  function proposalCard(p: Proposal, gi: number): HTMLElement {
    const target = p.after ?? p.before;
    const what =
      target?.title !== undefined
        ? `場面「${target.title}」`
        : target?.kind
          ? kindLabel(target.kind) + (target.speaker ? `（${target.speaker}）` : "")
          : "";
    const card = h(
      "div", { class: `proposal proposal-${p.status}` },
      h("h3", {}, OP_LABELS[p.op] ?? p.op, what ? h("span", { class: "muted" }, ` — ${what}`) : ""),
      p.reason ? h("p", { class: "proposal-reason" }, p.reason) : "",
      h(
        "div", { class: "diff" },
        p.before ? h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "変更前"), renderSide(p.diff, "before")) : "",
        p.after ? h("div", { class: "diff-col" }, h("div", { class: "diff-label" }, "変更後"), renderSide(p.diff, "after")) : "",
      ),
    );
    if (p.status !== "pending") {
      const labels: Record<string, string> = { approved: "反映しました", rejected: "却下しました", stale: "対象が変わったため反映しませんでした" };
      card.append(h("p", { class: `proposal-status status-${p.status}` }, labels[p.status] ?? p.status));
      return card;
    }
    const decide = (action: "approve" | "reject") => guard(async () => {
      if (action === "approve" && !(await editor.flush())) return;
      const updated = action === "approve" ? await api.approveProposal(p.id) : await api.rejectProposal(p.id, "");
      pending[gi].proposals = pending[gi].proposals.map((x) => (x.id === updated.id ? updated : x));
      if (updated.status === "approved") {
        await editor.syncFromServer();
        await refreshTree();
      }
      if (updated.status === "stale") toast("提案の後に対象が変わったため、反映しませんでした", "error");
      renderPending();
      if (tree) {
        const pid = tree.project.id;
        void maybePromptAnalysis(pid, () => void guard(async () => {
          const r = await api.analyzePreferences(pid);
          toast(r.suggestions.length ? `${r.suggestions.length} 件の嗜好の更新案があります（制作モードの「執筆の嗜好」で確認できます）` : "新しい傾向は見つかりませんでした");
        }));
      }
    });
    card.append(
      h(
        "div", { class: "proposal-actions" },
        h("button", { type: "button", class: "primary", onclick: () => void decide("approve") }, "反映する"),
        h("button", { type: "button", onclick: () => void decide("reject") }, "却下"),
      ),
    );
    return card;
  }

  /** 開いている場面について、まだ判断していない提案を読み込む（再読み込み後も続けられるように）。 */
  async function loadPending(): Promise<void> {
    pending = [];
    if (!tree || !sceneId) {
      renderPending();
      return;
    }
    const chats = await api.listChats(tree.project.id);
    const found = chats.find((c) => c.title === COMPOSE_CHAT_TITLE);
    chatId = found?.id ?? null;
    if (found) {
      const detail = await api.getChat(found.id);
      for (const m of detail.messages) {
        const open = m.proposals.filter((p) => p.status === "pending" && (p.scene_id === sceneId || !p.scene_id));
        if (open.length) pending.push({ reply: m.content, proposals: open });
      }
    }
    renderPending();
  }

  // ---- 場面・プロジェクトの選択 -----------------------------------------

  function renderHead(): void {
    let label = "場面を選ぶ";
    for (const c of tree?.chapters ?? []) {
      const s = c.scenes.find((x) => x.id === sceneId);
      if (s) label = `${c.title} / ${s.title}`;
    }
    sceneButton.replaceChildren(h("span", { class: "simple-context-text" }, label), h("span", { "aria-hidden": "true" }, " ▾"));
  }

  async function selectScene(id: string | null): Promise<void> {
    if (!(await editor.open(id))) return;
    sceneId = id;
    if (tree && id) localStorage.setItem(lastSceneKey(tree.project.id), id);
    renderHead();
    await loadPending();
    scrollToBottom(false);
  }

  async function pickScene(): Promise<void> {
    if (!tree) return;
    const current = tree;
    const chosen = await openDialog<string>((close) =>
      h(
        "div", {},
        h("h2", {}, "場面を選ぶ"),
        ...current.chapters.map((c) =>
          h(
            "div", { class: "picker-chapter" },
            h("h3", {}, c.title),
            ...c.scenes.map((s) =>
              h("button", {
                type: "button",
                class: s.id === sceneId ? "picker-scene active" : "picker-scene",
                onclick: () => close(s.id),
              }, h("strong", {}, s.title), s.summary ? h("span", { class: "muted" }, s.summary) : ""),
            ),
            h("button", { type: "button", class: "add", onclick: () => close(`new:${c.id}`) }, "＋ この章に場面を追加"),
          ),
        ),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close("new-chapter") }, "＋ 章を追加"),
          h("button", { type: "button", onclick: () => close(null) }, "閉じる"),
        ),
      ),
    );
    if (!chosen) return;
    if (chosen === "new-chapter") {
      const title = await askText("新しい章の題名", `第${current.chapters.length + 1}章`, "追加");
      if (!title) return;
      const c = await api.createChapter(current.project.id, title);
      const s = await api.createScene(c.id, "場面1");
      await refreshTree();
      await selectScene(s.id);
    } else if (chosen.startsWith("new:")) {
      const title = await askText("新しい場面の題名", "", "追加");
      if (!title) return;
      const s = await api.createScene(chosen.slice(4), title);
      await refreshTree();
      await selectScene(s.id);
    } else {
      await selectScene(chosen);
    }
  }

  async function refreshTree(): Promise<void> {
    if (!tree) return;
    tree = await api.getProjectTree(tree.project.id);
    renderHead();
  }

  async function openProject(id: string | null): Promise<void> {
    projects = await api.listProjects();
    if (!id || !projects.some((p) => p.id === id)) id = projects[0]?.id ?? null;
    if (!(await editor.open(null))) return;
    chatId = null;
    if (!id) {
      tree = null;
      sceneId = null;
      renderHead();
      bodyHost.replaceChildren(
        h("div", { class: "empty" },
          h("p", {}, "プロジェクトがありません。"),
          h("button", { type: "button", class: "primary", onclick: () => void guard(newProject) }, "新しいプロジェクトを作る")),
      );
      return;
    }
    tree = await api.getProjectTree(id);
    localStorage.setItem(LAST_PROJECT_KEY, id);
    const ids = tree.chapters.flatMap((c) => c.scenes.map((s) => s.id));
    const remembered = localStorage.getItem(lastSceneKey(id));
    await selectScene(remembered && ids.includes(remembered) ? remembered : (ids[0] ?? null));
  }

  async function newProject(): Promise<void> {
    const name = await askText("新しいプロジェクトの名前", "", "作成");
    if (!name) return;
    const p = await api.createProject({ name, description: "", templates: [] });
    await openProject(p.id);
  }

  async function pickProject(): Promise<void> {
    const chosen = await openDialog<string>((close) =>
      h(
        "div", {},
        h("h2", {}, "プロジェクト"),
        ...projects.map((p) =>
          h("button", { type: "button", class: p.id === tree?.project.id ? "picker-scene active" : "picker-scene", onclick: () => close(p.id) }, h("strong", {}, p.name)),
        ),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close("__new__") }, "＋ 新規"),
          h("button", { type: "button", onclick: () => close(null) }, "閉じる"),
        ),
      ),
    );
    if (chosen === "__new__") await newProject();
    else if (chosen) await openProject(chosen);
  }

  // ---- イベント -------------------------------------------------------

  sceneButton.addEventListener("click", () => void guard(pickScene));
  menu.addEventListener("click", () => {
    const items: MenuItem[] = [
      { label: `プロジェクト: ${tree?.project.name ?? "なし"}…`, action: () => void guard(pickProject) },
      { label: "制作モード（すべての機能）に切り替え", action: () => switchMode("full") },
      { label: "AI 設定・使用量…", action: () => void guard(() => openAiSettings(deps.local).then(() => undefined)) },
      deps.local
        ? { label: "端末の接続…", action: () => void guard(openDeviceManager) }
        : { label: `この端末（${deps.deviceName ?? "端末"}）の接続を解除…`, action: () => void guard(disconnectThisDevice) },
    ];
    openMenu(menu, items);
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") void editor.flush();
  });

  setUnauthenticatedHandler(() => showPairingScreen(appRoot, "この端末の接続が解除されました。もう一度ペアリングしてください。"));
  connectEvents({
    onChange: (scopes) => void guard(async () => {
      if (scopes.has("projects")) projects = await api.listProjects();
      if (scopes.has("tree")) await refreshTree();
      if (scopes.has("scene")) {
        const stay = nearBottom();
        await editor.syncFromRemote();
        if (stay) scrollToBottom(false);
      }
      if (scopes.has("chat") && !sending) await loadPending();
    }),
    onRevoked: () => showPairingScreen(appRoot, "この端末の接続が PC 側で解除されました。もう一度ペアリングしてください。"),
    onConnection: (connected) => shell.classList.toggle("offline", !connected),
  });

  await openProject(localStorage.getItem(LAST_PROJECT_KEY));
}
