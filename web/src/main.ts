// アプリ全体のシェル: プロジェクト選択、章・場面のナビゲーション、エクスポート。

import "./style.css";
import { openAiSettings } from "./aiSettings";
import { api } from "./api";
import { ChatPanel } from "./chat";
import { askConfirm, askProject, askText, h, openMenu, toast, type MenuItem } from "./dom";
import { FileEditor } from "./fileEditor";
import { SceneEditor, type SaveState, type SceneEditorOptions } from "./sceneEditor";
import { TEMPLATE_LABELS, type Meta, type Project, type ProjectTree, type ReferenceFile } from "./types";

const LAST_PROJECT_KEY = "scenote.lastProject";
const lastSceneKey = (projectId: string) => `scenote.lastScene.${projectId}`;

const STATE_LABELS: Record<SaveState, string> = {
  saved: "保存済み",
  dirty: "未保存の変更あり",
  saving: "保存中…",
  error: "保存エラー",
  conflict: "競合あり",
};

let meta: Meta;
let projects: Project[] = [];
let tree: ProjectTree | null = null;
let files: ReferenceFile[] = [];
/** 本文領域に表示しているもの。 */
let view: "scene" | "file" = "scene";
const saveStates: Record<"scene" | "file", { state: SaveState; message?: string }> = {
  scene: { state: "saved" },
  file: { state: "saved" },
};

const projectSelect = h("select", { class: "project-select", "aria-label": "プロジェクト" });
const status = h("span", { class: "status", role: "status" });
const nav = h("nav", { class: "nav", "aria-label": "章と場面" });
const sceneHost = h("div", { class: "editor-host" });
const fileHost = h("div", { class: "editor-host", hidden: true });
const main = h("main", { class: "main" }, sceneHost, fileHost);
const chatHost = h("aside", { class: "chat", "aria-label": "AI チャット" });
const chatToggle = h("button", { type: "button", "aria-pressed": "true", "aria-label": "AI チャットを開閉" }, "AI");
const navToggle = h("button", { type: "button", class: "icon", "aria-label": "章と場面の一覧を開閉" }, "☰");
const settingsButton = h("button", { type: "button" }, "設定");
const exportButton = h("button", { type: "button" }, "書き出し");
const shell = h(
  "div", { class: "shell" },
  h(
    "header", { class: "topbar" },
    navToggle,
    h("span", { class: "brand" }, "Scenote"),
    projectSelect,
    settingsButton,
    exportButton,
    status,
    chatToggle,
  ),
  h("div", { class: "workspace" }, nav, main, chatHost),
);

function showSaveState(): void {
  const { state, message } = saveStates[view];
  status.textContent = STATE_LABELS[state];
  status.className = `status status-${state}`;
  status.title = message ?? "";
}

const editorOptions: SceneEditorOptions = {
  kinds: [],
  onStateChange: (state, message) => {
    saveStates.scene = { state, message };
    showSaveState();
  },
  onSceneRenamed: () => void refreshTree(),
};
const editor = new SceneEditor(sceneHost, editorOptions);

const fileEditor = new FileEditor(fileHost, {
  onStateChange: (state, message) => {
    saveStates.file = { state, message };
    showSaveState();
  },
  onListChanged: () => void run(refreshFiles),
  onDeleted: () => void run(async () => {
    await refreshFiles();
    await selectScene(editor.sceneId ?? allSceneIds()[0] ?? null);
  }),
});

async function flushEditors(): Promise<boolean> {
  const results = await Promise.all([editor.flush(), fileEditor.flush()]);
  return results.every(Boolean);
}

const chat = new ChatPanel(chatHost, {
  currentSceneId: () => editor.sceneId,
  flushEditors,
  onApplied: async (proposal) => {
    if (proposal.op === "update_file" || proposal.op === "create_file") {
      await refreshFiles();
      if (proposal.target_id && proposal.target_id === fileEditor.fileId) await fileEditor.syncFromServer();
    } else if (proposal.scene_id && proposal.scene_id === editor.sceneId) {
      await editor.syncFromServer();
    }
  },
  openAiSettings: () => void run(showAiSettings),
});

async function showAiSettings(): Promise<void> {
  if (await openAiSettings()) await chat.refreshUsage();
}

function setView(next: "scene" | "file"): void {
  view = next;
  sceneHost.hidden = next !== "scene";
  fileHost.hidden = next !== "file";
  showSaveState();
}

function report(e: unknown): void {
  toast(e instanceof Error ? e.message : "予期しないエラーが発生しました", "error");
}

/** 操作を実行し、失敗したら通知する。 */
async function run(action: () => Promise<void>): Promise<void> {
  try {
    await action();
  } catch (e) {
    report(e);
  }
}

function templateOptions() {
  return meta.templates.map((id) => ({ id, label: TEMPLATE_LABELS[id] ?? id }));
}

function allSceneIds(): string[] {
  return tree?.chapters.flatMap((c) => c.scenes.map((s) => s.id)) ?? [];
}

// ---- プロジェクト -----------------------------------------------------

function renderProjectSelect(): void {
  projectSelect.replaceChildren(
    ...(projects.length === 0 ? [h("option", { value: "" }, "（プロジェクトなし）")] : []),
    ...projects.map((p) => h("option", { value: p.id, selected: p.id === tree?.project.id }, p.name)),
    h("option", { value: "__new__" }, "＋ 新規プロジェクト…"),
  );
  const has = tree !== null;
  settingsButton.disabled = !has;
  exportButton.disabled = !has;
}

async function loadProjects(): Promise<void> {
  projects = await api.listProjects();
  renderProjectSelect();
}

async function openProject(projectId: string | null): Promise<void> {
  if (projectId === null) {
    if (!(await fileEditor.open(null)) || !(await editor.open(null))) return;
    tree = null;
    files = [];
    setView("scene");
    renderProjectSelect();
    renderNav();
    await chat.setProject(null);
    sceneHost.replaceChildren(
      h(
        "div", { class: "empty" },
        h("p", {}, "プロジェクトがありません。最初のプロジェクトを作成してください。"),
        h("button", { type: "button", class: "primary", onclick: () => void run(createProject) }, "新規プロジェクト"),
      ),
    );
    return;
  }
  const next = await api.getProjectTree(projectId);
  const ids = next.chapters.flatMap((c) => c.scenes.map((s) => s.id));
  const remembered = localStorage.getItem(lastSceneKey(projectId));
  const sceneId = remembered && ids.includes(remembered) ? remembered : (ids[0] ?? null);
  if (!(await fileEditor.open(null)) || !(await editor.open(sceneId))) {
    renderProjectSelect();
    return;
  }
  tree = next;
  files = await api.listFiles(projectId);
  setView("scene");
  localStorage.setItem(LAST_PROJECT_KEY, projectId);
  renderProjectSelect();
  renderNav();
  await chat.setProject(projectId);
}

async function createProject(): Promise<void> {
  const value = await askProject("新規プロジェクト", templateOptions(), { name: "", description: "", templates: [] }, "作成");
  if (!value) {
    renderProjectSelect();
    return;
  }
  const project = await api.createProject(value);
  await loadProjects();
  await openProject(project.id);
}

async function editProject(): Promise<void> {
  if (!tree) return;
  const project = tree.project;
  const value = await askProject("プロジェクト設定", templateOptions(), project, "保存");
  if (!value) return;
  await api.updateProject(project.id, value);
  await loadProjects();
  await refreshTree();
}

async function deleteProject(): Promise<void> {
  if (!tree) return;
  const project = tree.project;
  const ok = await askConfirm(
    `プロジェクト「${project.name}」を削除しますか？章・場面・本文もすべて削除され、元に戻せません。必要であれば先に書き出してください。`,
  );
  if (!ok) return;
  if (!(await fileEditor.open(null)) || !(await editor.open(null))) return;
  await api.deleteProject(project.id);
  localStorage.removeItem(lastSceneKey(project.id));
  tree = null;
  await loadProjects();
  await openProject(projects[0]?.id ?? null);
}

// ---- 章・場面 ---------------------------------------------------------

async function refreshTree(): Promise<void> {
  if (!tree) return;
  tree = await api.getProjectTree(tree.project.id);
  renderProjectSelect();
  renderNav();
}

async function refreshFiles(): Promise<void> {
  if (!tree) return;
  files = await api.listFiles(tree.project.id);
  renderNav();
}

/** 参照ファイルを本文領域で開く。開いていた場面は保存したうえで、チャットの対象として保持する。 */
async function selectFile(fileId: string): Promise<void> {
  if (!(await editor.flush())) {
    toast("場面の変更を保存できていません。競合やエラーを解消してからやり直してください", "error");
    return;
  }
  if (!(await fileEditor.open(fileId))) return;
  setView("file");
  renderNav();
  shell.classList.remove("nav-open");
}

async function selectScene(sceneId: string | null): Promise<void> {
  if (!(await fileEditor.open(null))) return;
  if (!(await editor.open(sceneId))) return;
  setView("scene");
  if (tree && sceneId) localStorage.setItem(lastSceneKey(tree.project.id), sceneId);
  renderNav();
  shell.classList.remove("nav-open");
}

function moved(ids: string[], id: string, delta: number): string[] {
  const next = [...ids];
  const from = next.indexOf(id);
  next.splice(from + delta, 0, ...next.splice(from, 1));
  return next;
}

function renderNav(): void {
  if (!tree) {
    nav.replaceChildren();
    return;
  }
  const current = tree;
  const chapterIds = current.chapters.map((c) => c.id);
  const more = (label: string, items: () => MenuItem[]) =>
    h("button", {
      type: "button",
      class: "icon more",
      "aria-label": `${label}の操作`,
      onclick: (e: Event) => openMenu(e.currentTarget as HTMLElement, items()),
    }, "⋯");

  const sections = current.chapters.map((chapter, ci) => {
    const sceneIds = chapter.scenes.map((s) => s.id);
    const chapterMenu = (): MenuItem[] => [
      {
        label: "名前を変更",
        action: () => void run(async () => {
          const title = await askText("章の題名", chapter.title);
          if (!title) return;
          await api.renameChapter(chapter.id, title);
          await refreshTree();
        }),
      },
      {
        label: "上へ移動",
        disabled: ci === 0,
        action: () => void run(async () => {
          await api.reorderChapters(current.project.id, moved(chapterIds, chapter.id, -1));
          await refreshTree();
        }),
      },
      {
        label: "下へ移動",
        disabled: ci === chapterIds.length - 1,
        action: () => void run(async () => {
          await api.reorderChapters(current.project.id, moved(chapterIds, chapter.id, 1));
          await refreshTree();
        }),
      },
      {
        label: "章を削除",
        danger: true,
        action: () => void run(async () => {
          const ok = await askConfirm(`章「${chapter.title}」と、その中の場面 ${chapter.scenes.length} 件を削除しますか？元に戻せません。`);
          if (!ok) return;
          if (sceneIds.includes(editor.sceneId ?? "") && !(await editor.open(null))) return;
          await api.deleteChapter(chapter.id);
          await refreshTree();
          if (editor.sceneId === null) await selectScene(allSceneIds()[0] ?? null);
        }),
      },
    ];
    const sceneItems = chapter.scenes.map((scene, si) => {
      const sceneMenu = (): MenuItem[] => [
        {
          label: "上へ移動",
          disabled: si === 0,
          action: () => void run(async () => {
            await api.reorderScenes(chapter.id, moved(sceneIds, scene.id, -1));
            await refreshTree();
          }),
        },
        {
          label: "下へ移動",
          disabled: si === sceneIds.length - 1,
          action: () => void run(async () => {
            await api.reorderScenes(chapter.id, moved(sceneIds, scene.id, 1));
            await refreshTree();
          }),
        },
        {
          label: "場面を削除",
          danger: true,
          action: () => void run(async () => {
            if (!(await askConfirm(`場面「${scene.title}」を削除しますか？元に戻せません。`))) return;
            if (editor.sceneId === scene.id && !(await editor.open(null))) return;
            await api.deleteScene(scene.id);
            await refreshTree();
            if (editor.sceneId === null) await selectScene(allSceneIds()[0] ?? null);
          }),
        },
      ];
      const active = view === "scene" && scene.id === editor.sceneId;
      return h(
        "li", { class: active ? "scene-item active" : "scene-item" },
        h("button", {
          type: "button",
          class: "scene-link",
          "aria-current": active ? "true" : undefined,
          onclick: () => void run(() => selectScene(scene.id)),
        }, scene.title),
        more(scene.title, sceneMenu),
      );
    });
    return h(
      "section", { class: "chapter" },
      h("div", { class: "chapter-head" }, h("h2", {}, chapter.title), more(chapter.title, chapterMenu)),
      h("ul", {}, ...sceneItems),
      h("button", {
        type: "button",
        class: "add",
        onclick: () => void run(async () => {
          const title = await askText("新しい場面の題名", `場面${chapter.scenes.length + 1}`, "作成");
          if (!title) return;
          const scene = await api.createScene(chapter.id, title);
          await refreshTree();
          await selectScene(scene.id);
        }),
      }, "＋ 場面"),
    );
  });

  const fileItems = files.map((file) => {
    const active = view === "file" && file.id === fileEditor.fileId;
    return h(
      "li", { class: active ? "scene-item active" : "scene-item" },
      h("button", {
        type: "button",
        class: "scene-link",
        "aria-current": active ? "true" : undefined,
        onclick: () => void run(() => selectFile(file.id)),
      }, file.name, file.pinned ? h("span", { class: "pin", title: "常に AI に参照させる" }, " 常時") : null),
    );
  });
  const fileSection = h(
    "section", { class: "chapter files" },
    h("div", { class: "chapter-head" }, h("h2", {}, "参照ファイル")),
    h("ul", {}, ...fileItems),
    h("button", {
      type: "button",
      class: "add",
      onclick: () => void run(async () => {
        const name = await askText("新しい参照ファイルの名前", "", "作成");
        if (!name) return;
        const file = await api.createFile(current.project.id, name);
        await refreshFiles();
        await selectFile(file.id);
      }),
    }, "＋ 参照ファイル"),
  );

  nav.replaceChildren(
    ...sections,
    h("button", {
      type: "button",
      class: "add add-chapter",
      onclick: () => void run(async () => {
        const title = await askText("新しい章の題名", `第${current.chapters.length + 1}章`, "作成");
        if (!title) return;
        await api.createChapter(current.project.id, title);
        await refreshTree();
      }),
    }, "＋ 章"),
    fileSection,
  );
}

// ---- イベント ---------------------------------------------------------

projectSelect.addEventListener("change", () => {
  const value = projectSelect.value;
  void run(async () => {
    if (value === "__new__") await createProject();
    else if (value) await openProject(value);
  });
});

navToggle.addEventListener("click", () => shell.classList.toggle("nav-open"));
chatToggle.addEventListener("click", () => {
  const hidden = shell.classList.toggle("chat-closed");
  chatToggle.setAttribute("aria-pressed", String(!hidden));
});

settingsButton.addEventListener("click", () =>
  openMenu(settingsButton, [
    { label: "プロジェクト設定…", action: () => void run(editProject) },
    { label: "AI 設定・使用量…", action: () => void run(showAiSettings) },
    { label: "プロジェクトを削除…", danger: true, action: () => void run(deleteProject) },
  ]),
);

async function download(format: "markdown" | "json"): Promise<void> {
  if (!tree) return;
  if (!(await flushEditors())) {
    toast("未保存の変更があるため、書き出しには反映されません", "error");
  }
  const link = h("a", { href: api.exportUrl(tree.project.id, format) });
  document.body.append(link);
  link.click();
  link.remove();
}

exportButton.addEventListener("click", () =>
  openMenu(exportButton, [
    { label: "Markdown (.md)", action: () => void run(() => download("markdown")) },
    { label: "JSON（構造・ID を含む）", action: () => void run(() => download("json")) },
  ]),
);

// 画面を離れる・隠すときは待たずに保存し、保存しきれていなければ警告する。
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") void flushEditors();
});
window.addEventListener("beforeunload", (e) => {
  if (editor.hasUnsavedChanges() || fileEditor.hasUnsavedChanges()) {
    void flushEditors();
    e.preventDefault();
  }
});

async function start(): Promise<void> {
  // 狭い画面ではチャットが本文を覆うため、閉じた状態で始める。
  if (window.innerWidth <= 820) {
    shell.classList.add("chat-closed");
    chatToggle.setAttribute("aria-pressed", "false");
  }
  document.querySelector("#app")!.append(shell);
  try {
    meta = await api.meta();
    editorOptions.kinds = meta.element_kinds;
    await loadProjects();
    const remembered = localStorage.getItem(LAST_PROJECT_KEY);
    const initial = projects.find((p) => p.id === remembered) ?? projects[0];
    await openProject(initial?.id ?? null);
  } catch (e) {
    sceneHost.replaceChildren(
      h(
        "div", { class: "empty" },
        h("p", { class: "error-text" }, e instanceof Error ? e.message : "起動に失敗しました"),
        h("button", { type: "button", onclick: () => location.reload() }, "再読み込み"),
      ),
    );
  }
}

void start();
