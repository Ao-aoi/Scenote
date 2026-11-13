// HTTP API クライアント。画面側はここを通してのみサーバーと通信する。

import type {
  Chat,
  ChatDetail,
  ElementResult,
  Exchange,
  LlmSettings,
  Meta,
  Project,
  ProjectTree,
  Proposal,
  ReferenceFile,
  Scene,
  SceneDetail,
  UsageStatus,
} from "./types";

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

  listFiles: (projectId: string) => request<ReferenceFile[]>("GET", `/projects/${projectId}/files`),
  createFile: (projectId: string, name: string) =>
    request<ReferenceFile>("POST", `/projects/${projectId}/files`, { name, content: "" }),
  getFile: (id: string) => request<ReferenceFile>("GET", `/files/${id}`),
  updateFile: (id: string, baseRevision: number, patch: { name?: string; content?: string; pinned?: boolean }) =>
    request<ReferenceFile>("PATCH", `/files/${id}`, { base_revision: baseRevision, ...patch }),
  deleteFile: (id: string) => request<void>("DELETE", `/files/${id}`),

  listChats: (projectId: string) => request<Chat[]>("GET", `/projects/${projectId}/chats`),
  createChat: (projectId: string, title: string) => request<Chat>("POST", `/projects/${projectId}/chats`, { title }),
  getChat: (id: string) => request<ChatDetail>("GET", `/chats/${id}`),
  updateChat: (id: string, patch: Partial<Pick<Chat, "title" | "instructions" | "auto_select" | "file_ids">>) =>
    request<Chat>("PATCH", `/chats/${id}`, patch),
  deleteChat: (id: string) => request<void>("DELETE", `/chats/${id}`),
  sendMessage: (
    chatId: string,
    input: { content: string; scene_id: string | null; include_file_ids: string[]; exclude_file_ids: string[] },
  ) => request<Exchange>("POST", `/chats/${chatId}/messages`, input),
  approveProposal: (id: string) => request<Proposal>("POST", `/proposals/${id}/approve`, {}),
  rejectProposal: (id: string, reason: string) => request<Proposal>("POST", `/proposals/${id}/reject`, { reason }),

  getLlmSettings: () => request<LlmSettings>("GET", "/settings/llm"),
  updateLlmSettings: (patch: { api_key?: string; model?: string; monthly_token_limit?: number; warn_percent?: number }) =>
    request<LlmSettings>("PUT", "/settings/llm", patch),
  getUsage: () => request<UsageStatus>("GET", "/usage"),
  resumeUsage: () => request<UsageStatus>("POST", "/usage/resume", {}),

  exportUrl: (projectId: string, format: "markdown" | "json") => `/api/projects/${projectId}/export/${format}`,
};
