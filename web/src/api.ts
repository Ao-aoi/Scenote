// HTTP API クライアント。画面側はここを通してのみサーバーと通信する。

import type { ElementResult, Meta, Project, ProjectTree, Scene, SceneDetail } from "./types";

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly currentRevision?: number,
  ) {
    super(message);
  }

  get isConflict(): boolean {
    return this.code === "conflict";
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  let response: Response;
  try {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    response = await fetch(`/api${path}`, {
      method,
      headers: payload === undefined ? undefined : { "Content-Type": "application/json" },
      body: payload,
      // ページを離れる直前の自動保存が中断されないようにする（上限 64KB のため小さい要求のみ）。
      keepalive: payload !== undefined && payload.length < 16_000,
    });
  } catch {
    throw new ApiError("サーバーに接続できません。Scenote サーバーが起動しているか確認してください", 0, "network");
  }
  if (response.status === 204) return undefined as T;
  const text = await response.text();
  let data: unknown;
  try {
    data = text ? JSON.parse(text) : undefined;
  } catch {
    data = undefined;
  }
  if (!response.ok) {
    const error = (data as { error?: { code?: string; message?: string; current_revision?: number } } | undefined)?.error;
    throw new ApiError(
      error?.message ?? `サーバーがエラーを返しました (HTTP ${response.status})`,
      response.status,
      error?.code ?? "unknown",
      error?.current_revision,
    );
  }
  return data as T;
}

export interface ElementInput {
  kind: string;
  speaker: string;
  body: string;
}

export const api = {
  meta: () => request<Meta>("GET", "/meta"),

  listProjects: () => request<Project[]>("GET", "/projects"),
  createProject: (input: { name: string; description: string; templates: string[] }) =>
    request<Project>("POST", "/projects", input),
  getProjectTree: (id: string) => request<ProjectTree>("GET", `/projects/${id}`),
  updateProject: (id: string, patch: Partial<Pick<Project, "name" | "description" | "templates">>) =>
    request<Project>("PATCH", `/projects/${id}`, patch),
  deleteProject: (id: string) => request<void>("DELETE", `/projects/${id}`),

  createChapter: (projectId: string, title: string) =>
    request<{ id: string }>("POST", `/projects/${projectId}/chapters`, { title }),
  renameChapter: (id: string, title: string) => request<unknown>("PATCH", `/chapters/${id}`, { title }),
  deleteChapter: (id: string) => request<void>("DELETE", `/chapters/${id}`),
  reorderChapters: (projectId: string, ids: string[]) =>
    request<void>("PUT", `/projects/${projectId}/chapters/order`, { ids }),

  createScene: (chapterId: string, title: string) =>
    request<Scene>("POST", `/chapters/${chapterId}/scenes`, { title }),
  getScene: (id: string) => request<SceneDetail>("GET", `/scenes/${id}`),
  renameScene: (id: string, title: string) => request<Scene>("PATCH", `/scenes/${id}`, { title }),
  deleteScene: (id: string) => request<void>("DELETE", `/scenes/${id}`),
  reorderScenes: (chapterId: string, ids: string[]) =>
    request<void>("PUT", `/chapters/${chapterId}/scenes/order`, { ids }),

  createElement: (sceneId: string, input: ElementInput & { position?: number }) =>
    request<ElementResult>("POST", `/scenes/${sceneId}/elements`, input),
  updateElement: (id: string, baseRevision: number, input: ElementInput) =>
    request<ElementResult>("PATCH", `/elements/${id}`, { base_revision: baseRevision, ...input }),
  deleteElement: (id: string) => request<void>("DELETE", `/elements/${id}`),
  reorderElements: (sceneId: string, ids: string[]) =>
    request<void>("PUT", `/scenes/${sceneId}/elements/order`, { ids }),

  getSceneMarkdown: (sceneId: string) =>
    request<{ revision: number; markdown: string }>("GET", `/scenes/${sceneId}/markdown`),
  putSceneMarkdown: (sceneId: string, baseRevision: number, markdown: string) =>
    request<SceneDetail>("PUT", `/scenes/${sceneId}/markdown`, { base_revision: baseRevision, markdown }),

  exportUrl: (projectId: string, format: "markdown" | "json") => `/api/projects/${projectId}/export/${format}`,
};
