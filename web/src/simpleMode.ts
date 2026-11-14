// かんたんモード（#15。#11・#8 を含む）。
//
// スマートフォン・iPad で、思いついた展開をすぐ書き留めて、気軽にプロットを進めるための画面。
// 制作用の画面と同じデータ（場面の概要＝プロット、要素、チャット、提案）を使い、表示と操作だけを絞る。
//
// - 「会話」：AI チャットを基本画面にし、入力欄を画面下部に固定する（#11）。よく使う依頼はワンタップ。
// - 「プロット」：章ごとの場面を概要つきのカードで並べ、一度に 1 枚だけ開いて編集する。
// - 「本文」：選んだ場面の本文。補助的な操作は折りたたむ（#8）。
// - AI の提案は、承認したものだけが反映される（制作用の画面と同じ）。

import { maybePromptAnalysis } from "./analysisPrompt";
import { api, ApiError, setUnauthenticatedHandler } from "./api";
import { ChatPanel } from "./chat";
import { disconnectThisDevice, openDeviceManager, showPairingScreen } from "./devices";
import { askConfirm, askText, h, openDialog, openMenu, toast, type MenuItem } from "./dom";
import { connectEvents } from "./events";
import { openAiSettings } from "./aiSettings";
import { icon } from "./icons";
import { errorMessage, SceneEditor, type SaveState } from "./sceneEditor";
import type { Meta, Project, ProjectTree, Scene } from "./types";

export type UiMode = "auto" | "simple" | "full";

const MODE_KEY = "scenote.uiMode";
const LAST_PROJECT_KEY = "scenote.lastProject";
const lastSceneKey = (projectId: string) => `scenote.lastScene.${projectId}`;
/** これ以下の幅（スマートフォン）では、自動でかんたんモードにする。 */
export const SIMPLE_MAX_WIDTH = 600;
const SUMMARY_SAVE_DELAY = 800;

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
  // 自動判定と同じ結果になる場合は「自動」に戻す（端末を回転させたときなどに自然に切り替わるように）。
  const auto = window.innerWidth <= SIMPLE_MAX_WIDTH ? "simple" : "full";
  if (mode === auto) localStorage.removeItem(MODE_KEY);
  else localStorage.setItem(MODE_KEY, mode);
  location.reload();
}

/** よく使う AI への依頼（ワンタップで送る）。 */
const QUICK_REQUESTS: { label: string; text: (scene: string | null) => string }[] = [
  { label: "続きの展開", text: (s) => (s ? `場面「${s}」の次に続く展開を、場面の追加（概要つき）として提案して。` : "次に続く展開を考えて。") },
  { label: "この場面の別案", text: (s) => (s ? `場面「${s}」の概要（プロット）の別案を提案して。` : "いまの展開の別案を考えて。") },
  { label: "盛り上げたい", text: (s) => (s ? `場面「${s}」をもっと盛り上げるには？ 概要の変更として提案して。` : "物語をもっと盛り上げるには？") },
  { label: "台詞を整える", text: () => "開いている場面の台詞を、キャラクターらしく自然に整えて。" },
];

type Tab = "chat" | "plot" | "body";

interface Deps {
  meta: Meta;
  local: boolean;
  deviceName: string | null;
}

export async function startSimple(appRoot: HTMLElement, deps: Deps): Promise<void> {
  let projects: Project[] = [];
  let tree: ProjectTree | null = null;
  let sceneId: string | null = null;
  let chapterId: string | null = null;
  let tab: Tab = (localStorage.getItem("scenote.simpleTab") as Tab | null) ?? "chat";
  let openCard: string | null = null;

  const shell = h("div", { class: "simple" });
  const projectSelect = h("select", { class: "simple-project", "aria-label": "プロジェクト" });
  const context = h("button", { type: "button", class: "simple-context", "aria-label": "場面を選ぶ" });
  const status = h("span", { class: "simple-status", role: "status" });
  const menu = h("button", { type: "button", class: "icon", title: "メニュー", "aria-label": "メニュー" }, icon("settings"));
  const tabs = h("div", { class: "segmented simple-tabs", role: "tablist" });
  const chatPane = h("section", { class: "simple-pane simple-chat" });
  const plotPane = h("section", { class: "simple-pane simple-plot" });
  const bodyPane = h("section", { class: "simple-pane simple-body" });
  shell.append(
    h("header", { class: "simple-head" }, projectSelect, menu),
    h("div", { class: "simple-subhead" }, context, status),
    tabs,
    chatPane,
    plotPane,
    bodyPane,
  );
  appRoot.replaceChildren(shell);

  // ソフトウェアキーボードが出ても入力欄が隠れないよう、見えている高さに画面を合わせる（#11）。
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
  const editor = new SceneEditor(bodyPane, {
    kinds: deps.meta.element_kinds,
    scenes: () => tree?.chapters.flatMap((c) => c.scenes.map((s) => ({ id: s.id, label: `${c.title} / ${s.title}` }))) ?? [],
    onStateChange: (state) => {
      status.textContent = STATE_LABELS[state];
      status.className = `simple-status status-${state}`;
    },
    onSceneRenamed: () => void refreshTree(),
  });

  const chat = new ChatPanel(chatPane, {
    currentSceneId: () => sceneId,
    flushEditors: () => editor.flush(),
    onApplied: async (p) => {
      await refreshTree();
      if (p.scene_id && p.scene_id === editor.sceneId) await editor.syncFromServer();
    },
    openAiSettings: () => void guard(() => openAiSettings(deps.local).then(() => chat.refreshUsage())),
    onDecided: () => {
      if (!tree) return;
      const pid = tree.project.id;
      void maybePromptAnalysis(pid, () => void guard(async () => {
        const r = await api.analyzePreferences(pid);
        toast(r.suggestions.length ? `${r.suggestions.length} 件の嗜好の更新案があります（制作モードの「執筆の嗜好」で確認できます）` : "新しい傾向は見つかりませんでした");
      }));
    },
  });
  // ワンタップの依頼を、入力欄のすぐ上に置く。
  const quick = h("div", { class: "simple-quick", role: "group", "aria-label": "よく使う依頼" });
  for (const q of QUICK_REQUESTS) {
    quick.append(h("button", {
      type: "button",
      class: "chip",
      onclick: () => {
        if (chat.isSending) return;
        void chat.sendText(q.text(currentScene()?.title ?? null));
      },
    }, q.label));
  }
  chatPane.querySelector(".chat-compose")?.before(quick);
  // 長い依頼も書けるよう、入力欄の高さを内容に合わせる（上限あり、超えたらスクロール）。
  const input = chatPane.querySelector<HTMLTextAreaElement>(".chat-input");
  if (input) {
    input.rows = 1;
    input.placeholder = "AI への依頼を入力";
    const grow = () => {
      input.style.height = "auto";
      input.style.height = `${Math.min(input.scrollHeight + 2, 160)}px`;
    };
    input.addEventListener("input", grow);
    new MutationObserver(grow).observe(input, { attributes: true, attributeFilter: ["disabled"] });
  }

  async function guard(action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 401)) toast(errorMessage(e), "error");
    }
  }

  function currentScene(): (Scene & { chapterTitle: string }) | null {
    for (const c of tree?.chapters ?? []) {
      const s = c.scenes.find((x) => x.id === sceneId);
      if (s) return { ...s, chapterTitle: c.title };
    }
    return null;
  }

  // ---- 表示 -----------------------------------------------------------

  function renderHead(): void {
    projectSelect.replaceChildren(
      ...projects.map((p) => h("option", { value: p.id, selected: p.id === tree?.project.id }, p.name)),
      h("option", { value: "__new__" }, "＋ 新規プロジェクト…"),
    );
    const s = currentScene();
    context.replaceChildren(
      h("span", { class: "simple-context-text" }, s ? `${s.chapterTitle} / ${s.title}` : "場面を選ぶ"),
      h("span", { "aria-hidden": "true" }, " ▾"),
    );
    tabs.replaceChildren(
      ...([["chat", "会話"], ["plot", "プロット"], ["body", "本文"]] as [Tab, string][]).map(([id, label]) =>
        h("button", {
          type: "button",
          role: "tab",
          class: tab === id ? "seg active" : "seg",
          "aria-selected": String(tab === id),
          onclick: () => void guard(() => setTab(id)),
        }, label),
      ),
    );
    chatPane.hidden = tab !== "chat";
    plotPane.hidden = tab !== "plot";
    bodyPane.hidden = tab !== "body";
  }

  async function setTab(next: Tab): Promise<void> {
    if (tab === "body" && next !== "body" && !(await editor.flush())) {
      toast("本文の変更を保存できていません", "error");
    }
    tab = next;
    localStorage.setItem("scenote.simpleTab", next);
    renderHead();
    if (next === "plot") renderPlot();
    if (next === "body" && editor.sceneId !== sceneId) await editor.open(sceneId);
  }

  async function selectScene(id: string | null): Promise<void> {
    if (tab === "body" || editor.sceneId) {
      if (!(await editor.open(id))) return;
    }
    sceneId = id;
    const chapter = tree?.chapters.find((c) => c.scenes.some((s) => s.id === id));
    if (chapter) chapterId = chapter.id;
    if (tree && id) localStorage.setItem(lastSceneKey(tree.project.id), id);
    renderHead();
    if (tab === "plot") renderPlot();
  }

  // ---- プロット（#15） ---------------------------------------------------

  function renderPlot(): void {
    if (!tree) {
      plotPane.replaceChildren(h("p", { class: "empty" }, "プロジェクトを選んでください。"));
      return;
    }
    const current = tree;
    const chapter = current.chapters.find((c) => c.id === chapterId) ?? current.chapters[0];
    if (!chapter) {
      plotPane.replaceChildren(
        h("p", { class: "empty" }, "章がありません。"),
        h("button", { type: "button", class: "primary", onclick: () => void guard(addChapter) }, "＋ 章を追加"),
      );
      return;
    }
    chapterId = chapter.id;
    const chapterSelect = h(
      "select", { class: "field", "aria-label": "章" },
      ...current.chapters.map((c) => h("option", { value: c.id, selected: c.id === chapter.id }, c.title)),
      h("option", { value: "__new__" }, "＋ 章を追加…"),
    );
    chapterSelect.addEventListener("change", () => void guard(async () => {
      if (chapterSelect.value === "__new__") await addChapter();
      else {
        chapterId = chapterSelect.value;
        openCard = null;
        renderPlot();
      }
    }));
    const ids = chapter.scenes.map((s) => s.id);
    const cards = chapter.scenes.map((s, i) => plotCard(s, i, ids));
    plotPane.replaceChildren(
      h("div", { class: "simple-plot-head" }, chapterSelect),
      h("p", { class: "hint" }, "場面ごとに「何が起きるか」を短く書きます。カードを押すと編集できます。"),
      h("ol", { class: "plot-list" }, ...cards),
      h("button", {
        type: "button",
        class: "add plot-add",
        onclick: () => void guard(() => addScene(chapter.id, undefined)),
      }, "＋ 場面を追加"),
    );
  }

  function plotCard(s: Scene, index: number, ids: string[]): HTMLElement {
    const open = openCard === s.id;
    const isCurrent = s.id === sceneId;
    const head = h(
      "button",
      {
        type: "button",
        class: "plot-card-head",
        "aria-expanded": String(open),
        onclick: () => void guard(async () => {
          openCard = open ? null : s.id;
          if (!open) await selectScene(s.id);
          renderPlot();
        }),
      },
      h("span", { class: "plot-no" }, String(index + 1)),
      h("span", { class: "plot-title" }, s.title),
      isCurrent ? h("span", { class: "plot-current" }, "選択中") : "",
    );
    const item = h("li", { class: `plot-card${open ? " open" : ""}${isCurrent ? " current" : ""}` }, head);
    if (!open) {
      item.append(h("p", { class: s.summary ? "plot-summary" : "plot-summary muted" }, s.summary || "（概要はまだありません）"));
      return item;
    }
    // 開いているカード: 概要を自動保存で編集する。
    let revision = s.revision;
    let timer: number | undefined;
    const note = h("p", { class: "plot-note muted" });
    const summary = h("textarea", { class: "field plot-input", rows: 4, value: s.summary, placeholder: "この場面で起きること（例: アリスが館に着き、執事に迎えられる）", "aria-label": "場面の概要" });
    const save = async () => {
      window.clearTimeout(timer);
      if (summary.value === s.summary) return;
      note.textContent = "保存中…";
      try {
        const updated = await api.updateSceneSummary(s.id, revision, summary.value);
        revision = updated.revision;
        s.summary = updated.summary;
        s.revision = updated.revision;
        note.textContent = "保存しました";
      } catch (e) {
        if (e instanceof ApiError && e.isConflict) {
          note.textContent = "";
          toast("他の端末でこの場面が変更されています。最新の内容を読み込みました", "error");
          await refreshTree();
          renderPlot();
        } else {
          note.textContent = `保存できません: ${errorMessage(e)}`;
        }
      }
    };
    summary.addEventListener("input", () => {
      note.textContent = "";
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void save(), SUMMARY_SAVE_DELAY);
    });
    summary.addEventListener("blur", () => void save());
    setTimeout(() => summary.focus());
    const pos = ids.indexOf(s.id);
    const move = (delta: number) => guard(async () => {
      await save();
      const next = [...ids];
      next.splice(pos + delta, 0, ...next.splice(pos, 1));
      await api.reorderScenes(s.chapter_id, next);
      await refreshTree();
      renderPlot();
    });
    item.append(
      summary,
      note,
      h(
        "div", { class: "plot-actions" },
        h("button", { type: "button", onclick: () => void guard(async () => { await save(); await selectScene(s.id); await setTab("body"); }) }, "本文を書く"),
        h("button", { type: "button", onclick: () => void guard(async () => { await save(); await selectScene(s.id); await setTab("chat"); await chat.sendText(QUICK_REQUESTS[0].text(s.title)); }) }, "AI に続きを相談"),
        h("button", { type: "button", onclick: () => void guard(async () => { await save(); await addScene(s.chapter_id, s.id); }) }, "＋ この後に場面"),
        h("button", { type: "button", class: "icon", title: "上へ", "aria-label": "上へ移動", disabled: pos === 0, onclick: () => void move(-1) }, "↑"),
        h("button", { type: "button", class: "icon", title: "下へ", "aria-label": "下へ移動", disabled: pos === ids.length - 1, onclick: () => void move(1) }, "↓"),
        h("button", {
          type: "button",
          class: "danger-text",
          onclick: () => void guard(async () => {
            if (!(await askConfirm(`場面「${s.title}」を削除しますか？本文も削除されます（履歴とスナップショットからは戻せます）。`))) return;
            if (editor.sceneId === s.id && !(await editor.open(null))) return;
            await api.deleteScene(s.id);
            if (sceneId === s.id) sceneId = null;
            openCard = null;
            await refreshTree();
            renderPlot();
          }),
        }, "削除"),
      ),
    );
    return item;
  }

  async function addScene(chapter: string, after: string | undefined): Promise<void> {
    const title = await askText("新しい場面の題名", "", "追加");
    if (!title) return;
    const scene = await api.createSceneWithSummary(chapter, title, "", after);
    await refreshTree();
    openCard = scene.id;
    await selectScene(scene.id);
    renderPlot();
  }

  async function addChapter(): Promise<void> {
    if (!tree) return;
    const title = await askText("新しい章の題名", `第${tree.chapters.length + 1}章`, "追加");
    if (!title) {
      renderPlot();
      return;
    }
    const c = await api.createChapter(tree.project.id, title);
    chapterId = c.id;
    await refreshTree();
    renderPlot();
  }

  // ---- 場面の選択 -------------------------------------------------------

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
          ),
        ),
        h("div", { class: "dialog-actions" }, h("button", { type: "button", onclick: () => close(null) }, "閉じる")),
      ),
    );
    if (chosen) await selectScene(chosen);
  }

  // ---- データの読み込み -------------------------------------------------

  async function refreshTree(): Promise<void> {
    if (!tree) return;
    tree = await api.getProjectTree(tree.project.id);
    const ids = tree.chapters.flatMap((c) => c.scenes.map((s) => s.id));
    if (sceneId && !ids.includes(sceneId)) sceneId = ids[0] ?? null;
    renderHead();
  }

  async function openProject(id: string | null): Promise<void> {
    projects = await api.listProjects();
    if (!id || !projects.some((p) => p.id === id)) id = projects[0]?.id ?? null;
    if (!(await editor.open(null))) return;
    if (!id) {
      tree = null;
      sceneId = null;
      renderHead();
      chatPane.hidden = plotPane.hidden = bodyPane.hidden = true;
      return;
    }
    tree = await api.getProjectTree(id);
    localStorage.setItem(LAST_PROJECT_KEY, id);
    const ids = tree.chapters.flatMap((c) => c.scenes.map((s) => s.id));
    const remembered = localStorage.getItem(lastSceneKey(id));
    sceneId = remembered && ids.includes(remembered) ? remembered : (ids[0] ?? null);
    chapterId = tree.chapters.find((c) => c.scenes.some((s) => s.id === sceneId))?.id ?? tree.chapters[0]?.id ?? null;
    openCard = null;
    renderHead();
    await chat.setProject(id);
    if (tab === "body") await editor.open(sceneId);
    if (tab === "plot") renderPlot();
  }

  // ---- イベント -------------------------------------------------------

  projectSelect.addEventListener("change", () => void guard(async () => {
    if (projectSelect.value === "__new__") {
      const name = await askText("新しいプロジェクトの名前", "", "作成");
      if (!name) {
        renderHead();
        return;
      }
      const p = await api.createProject({ name, description: "", templates: [] });
      await openProject(p.id);
    } else {
      await openProject(projectSelect.value);
    }
  }));
  context.addEventListener("click", () => void guard(pickScene));
  menu.addEventListener("click", () => {
    const items: MenuItem[] = [
      { label: "制作モード（すべての機能）に切り替え", action: () => switchMode("full") },
      { label: "AI 設定・使用量…", action: () => void guard(() => openAiSettings(deps.local).then(() => chat.refreshUsage())) },
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
      if (scopes.has("tree") || scopes.has("scene")) {
        // プロットを編集中なら、カードを作り直さない（保存時に競合として検出する）。
        const editingPlot = tab === "plot" && plotPane.contains(document.activeElement);
        await refreshTree();
        if (tab === "plot" && !editingPlot) renderPlot();
      }
      if (scopes.has("scene")) await editor.syncFromRemote();
      if (scopes.has("chat")) await chat.refreshFromRemote();
      if (scopes.has("settings")) await chat.refreshUsage();
    }),
    onRevoked: () => showPairingScreen(appRoot, "この端末の接続が PC 側で解除されました。もう一度ペアリングしてください。"),
    onConnection: (connected) => shell.classList.toggle("offline", !connected),
  });

  await openProject(localStorage.getItem(LAST_PROJECT_KEY));
}
