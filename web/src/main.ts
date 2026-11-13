// アプリ全体のシェル: プロジェクト選択、章・場面のナビゲーション、エクスポート。

import "./style.css";
import { api } from "./api";
import { askConfirm, askProject, askText, h, openMenu, toast, type MenuItem } from "./dom";
import { SceneEditor, type SaveState, type SceneEditorOptions } from "./sceneEditor";
import { TEMPLATE_LABELS, type Meta, type Project, type ProjectTree } from "./types";

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

const projectSelect = h("select", { class: "project-select", "aria-label": "プロジェクト" });
const status = h("span", { class: "status", role: "status" });
const nav = h("nav", { class: "nav", "aria-label": "章と場面" });
const main = h("main", { class: "main" });
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
  ),
  h("div", { class: "workspace" }, nav, main),
);

const editorOptions: SceneEditorOptions = {
  kinds: [],
  onStateChange: (state, message) => {
    status.textContent = STATE_LABELS[state];
    status.className = `status status-${state}`;
    status.title = message ?? "";
  },
  onSceneRenamed: () => void refreshTree(),
};
const editor = new SceneEditor(main, editorOptions);

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
    if (!(await editor.open(null))) return;
    tree = null;
    renderProjectSelect();
    renderNav();
    main.replaceChildren(
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
  if (!(await editor.open(sceneId))) {
    renderProjectSelect();
    return;
  }
  tree = next;
  localStorage.setItem(LAST_PROJECT_KEY, projectId);
  renderProjectSelect();
  renderNav();
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
  if (!(await editor.open(null))) return;
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

async function selectScene(sceneId: string | null): Promise<void> {
  if (!(await editor.open(sceneId))) return;
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
      const active = scene.id === editor.sceneId;
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

settingsButton.addEventListener("click", () =>
  openMenu(settingsButton, [
    { label: "プロジェクト設定…", action: () => void run(editProject) },
    { label: "プロジェクトを削除…", danger: true, action: () => void run(deleteProject) },
  ]),
);

async function download(format: "markdown" | "json"): Promise<void> {
  if (!tree) return;
  if (!(await editor.flush())) {
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
  if (document.visibilityState === "hidden") void editor.flush();
});
window.addEventListener("beforeunload", (e) => {
  if (editor.hasUnsavedChanges()) {
    void editor.flush();
    e.preventDefault();
  }
});

async function start(): Promise<void> {
  document.querySelector("#app")!.append(shell);
  try {
    meta = await api.meta();
    editorOptions.kinds = meta.element_kinds;
    await loadProjects();
    const remembered = localStorage.getItem(LAST_PROJECT_KEY);
    const initial = projects.find((p) => p.id === remembered) ?? projects[0];
    await openProject(initial?.id ?? null);
  } catch (e) {
    main.replaceChildren(
      h(
        "div", { class: "empty" },
        h("p", { class: "error-text" }, e instanceof Error ? e.message : "起動に失敗しました"),
        h("button", { type: "button", onclick: () => location.reload() }, "再読み込み"),
      ),
    );
  }
}

void start();
