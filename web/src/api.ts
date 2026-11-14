// HTTP API クライアント。画面側はここを通してのみサーバーと通信する。

import type {
  Character,
  CharacterDraft,
  CharacterFact,
  ChapterDraft,
  AnalysisResult,
  PreferenceSettings,
  Structure,
  Variable,
  Device,
  Chat,
  CompareItem,
  HistoryEntry,
  PreferenceProfile,
  PreferenceRule,
  PreferenceSuggestion,
  ProposalContent,
  SnapshotInfo,
  ChatDetail,
  ElementResult,
  Exchange,
  LlmSettings,
  LlmModel,
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

/** この画面の識別子。自分が行った変更の通知を無視するために、すべての要求に付ける。 */
export const CLIENT_ID = (crypto.randomUUID?.() ?? `${Date.now()}-${Math.random()}`).replace(/[^\w-]/g, "");

let onUnauthenticated: (() => void) | null = null;

/** 未認証（端末の登録が解除されたなど）を検出したときの処理を登録する。 */
export function setUnauthenticatedHandler(handler: () => void): void {
  onUnauthenticated = handler;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  let response: Response;
  try {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const headers: Record<string, string> = {
      // 他サイトからの送信と区別するためのヘッダー（サーバーは状態を変える要求に必須としている）。
      "X-Scenote": "1",
      "X-Scenote-Client": CLIENT_ID,
    };
    if (payload !== undefined) headers["Content-Type"] = "application/json";
    response = await fetch(`/api${path}`, {
      method,
      headers,
      credentials: "same-origin",
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
    if (response.status === 401) onUnauthenticated?.();
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
  updateSceneSummary: (id: string, baseRevision: number, summary: string) =>
    request<Scene>("PATCH", `/scenes/${id}`, { summary, base_revision: baseRevision }),
  createSceneWithSummary: (chapterId: string, title: string, summary: string, after?: string) =>
    request<Scene>("POST", `/chapters/${chapterId}/scenes`, { title, summary, after }),
  appendOutline: (projectId: string, chapters: { title: string; scenes: { title: string; summary: string }[] }[]) =>
    request<Scene[]>("POST", `/projects/${projectId}/outline`, { chapters }),
  listCharacters: (projectId: string) => request<Character[]>("GET", `/projects/${projectId}/characters`),
  createCharacter: (projectId: string, name: string) => request<Character>("POST", `/projects/${projectId}/characters`, { name }),
  deleteCharacter: (id: string) => request<void>("DELETE", `/characters/${id}`),
  listFacts: (characterId: string) => request<CharacterFact[]>("GET", `/characters/${characterId}/facts`),
  createFact: (characterId: string, category: string, text: string) =>
    request<CharacterFact>("POST", `/characters/${characterId}/facts`, { category, text, source: "user" }),
  generateCharacters: (projectId: string, body: { premise: string; instructions: string; count: number }) =>
    request<{ drafts: CharacterDraft[]; warnings: string[] }>("POST", `/projects/${projectId}/generate/characters`, body),
  generateWorld: (projectId: string, body: { premise: string; instructions: string; characters: CharacterDraft[] }) =>
    request<{ drafts: { title: string; content: string }; warnings: string[] }>("POST", `/projects/${projectId}/generate/world`, body),
  generateStory: (projectId: string, body: { premise: string; instructions: string }) =>
    request<{ drafts: ChapterDraft[]; warnings: string[] }>("POST", `/projects/${projectId}/generate/story`, body),
  deleteScene: (id: string) => request<void>("DELETE", `/scenes/${id}`),
  reorderScenes: (chapterId: string, ids: string[]) =>
    request<void>("PUT", `/chapters/${chapterId}/scenes/order`, { ids }),

  createElement: (sceneId: string, input: ElementInput & { position?: number }) =>
    request<ElementResult>("POST", `/scenes/${sceneId}/elements`, input),
  updateElement: (id: string, baseRevision: number, input: ElementInput & { attrs?: unknown }) =>
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

  approveProposalEdited: (id: string, content: ProposalContent) =>
    request<Proposal>("POST", `/proposals/${id}/approve-edited`, { content }),

  getHistory: (projectId: string, options: { sceneId?: string; before?: number; limit?: number } = {}) => {
    const q = new URLSearchParams();
    if (options.sceneId) q.set("scene_id", options.sceneId);
    if (options.before !== undefined) q.set("before", String(options.before));
    q.set("limit", String(options.limit ?? 50));
    return request<HistoryEntry[]>("GET", `/projects/${projectId}/history?${q}`);
  },
  getHistorySettings: () => request<{ max_entries: number }>("GET", "/settings/history"),
  updateHistorySettings: (maxEntries: number) =>
    request<{ max_entries: number }>("PUT", "/settings/history", { max_entries: maxEntries }),
  listSnapshots: (projectId: string) => request<SnapshotInfo[]>("GET", `/projects/${projectId}/snapshots`),
  createSnapshot: (projectId: string, note: string) =>
    request<SnapshotInfo>("POST", `/projects/${projectId}/snapshots`, { note }),
  deleteSnapshot: (id: string) => request<void>("DELETE", `/snapshots/${id}`),
  compareSnapshot: (id: string) => request<CompareItem[]>("GET", `/snapshots/${id}/compare`),
  restoreSnapshot: (id: string) => request<SnapshotInfo>("POST", `/snapshots/${id}/restore`, {}),

  getPreferences: (projectId: string) => request<PreferenceProfile>("GET", `/projects/${projectId}/preferences`),
  analyzePreferences: (projectId: string) =>
    request<AnalysisResult>("POST", `/projects/${projectId}/preferences/analyze`, {}),
  importPreferences: (projectId: string, sourceProjectId: string) =>
    request<{ copied: number }>("POST", `/projects/${projectId}/preferences/import`, { source_project_id: sourceProjectId }),
  createRule: (projectId: string | null, text: string, category: string) =>
    request<PreferenceRule>(
      "POST",
      projectId ? `/projects/${projectId}/preferences/rules` : "/preferences/global/rules",
      { text, category },
    ),
  updateRule: (id: string, patch: { text?: string; category?: string; confidence?: number }) =>
    request<PreferenceRule>("PATCH", `/preferences/rules/${id}`, patch),
  deleteRule: (id: string) => request<void>("DELETE", `/preferences/rules/${id}`),
  copyRule: (id: string, projectId: string | null) =>
    request<{ copied: number }>("POST", `/preferences/rules/${id}/copy`, projectId ? { project_id: projectId } : {}),
  approveSuggestion: (id: string) => request<PreferenceSuggestion>("POST", `/preferences/suggestions/${id}/approve`, {}),
  rejectSuggestion: (id: string) => request<PreferenceSuggestion>("POST", `/preferences/suggestions/${id}/reject`, {}),
  updatePreferenceSettings: (patch: Partial<PreferenceSettings>) =>
    request<PreferenceSettings>("PUT", "/settings/preferences", patch),

  listVariables: (projectId: string) => request<Variable[]>("GET", `/projects/${projectId}/variables`),
  createVariable: (projectId: string, v: { name: string; var_type: string; initial: string; description: string }) =>
    request<Variable>("POST", `/projects/${projectId}/variables`, v),
  updateVariable: (id: string, v: Partial<{ name: string; var_type: string; initial: string; description: string }>) =>
    request<Variable>("PATCH", `/variables/${id}`, v),
  deleteVariable: (id: string) => request<void>("DELETE", `/variables/${id}`),
  getStructure: (projectId: string) => request<Structure>("GET", `/projects/${projectId}/structure`),

  authStatus: () =>
    request<{ authenticated: boolean; local: boolean; device_name: string | null; lan_enabled: boolean }>("GET", "/auth/status"),
  pair: (code: string, deviceName: string) =>
    request<{ device: Device }>("POST", "/auth/pair", { code, device_name: deviceName }),
  logout: () => request<void>("POST", "/auth/logout", {}),
  listDevices: () => request<Device[]>("GET", "/devices"),
  startPairing: () => request<{ code: string; expires_in_seconds: number; urls: string[] }>("POST", "/devices/pairing", {}),
  deleteDevice: (id: string) => request<void>("DELETE", `/devices/${id}`),

  getLlmSettings: () => request<LlmSettings>("GET", "/settings/llm"),
  getGeminiModels: () => request<LlmModel[]>("GET", "/settings/llm/models"),
  updateLlmSettings: (patch: { api_key?: string; model?: string; monthly_token_limit?: number; warn_percent?: number }) =>
    request<LlmSettings>("PUT", "/settings/llm", patch),
  getUsage: () => request<UsageStatus>("GET", "/usage"),
  resumeUsage: () => request<UsageStatus>("POST", "/usage/resume", {}),

  exportUrl: (projectId: string, format: "markdown" | "json") => `/api/projects/${projectId}/export/${format}`,
};
