// サーバー API のデータ型（server/src/model.rs と対応）。

export interface Project {
  id: string;
  name: string;
  description: string;
  templates: string[];
  created_at: string;
  updated_at: string;
}

export interface Scene {
  id: string;
  chapter_id: string;
  title: string;
  /** 場面の概要（プロット）。 */
  summary: string;
  position: number;
  revision: number;
}

export interface Chapter {
  id: string;
  project_id: string;
  title: string;
  position: number;
  scenes: Scene[];
}

export interface ProjectTree {
  project: Project;
  chapters: Chapter[];
}

export interface ScenarioElement {
  id: string;
  scene_id: string;
  kind: string;
  body: string;
  speaker: string;
  /** 独立したキャラクターへの参照（未設定なら null）。speaker と併存する。 */
  character_id: string | null;
  position: number;
  origin: string;
  revision: number;
  /** 分岐先・条件などの構造データ。 */
  attrs: Record<string, unknown>;
}

export interface SceneDetail extends Scene {
  elements: ScenarioElement[];
}

export interface ElementResult {
  element: ScenarioElement;
  scene_revision: number;
}

export interface Meta {
  schema_version: number;
  element_kinds: string[];
  templates: string[];
}

export const KIND_LABELS: Record<string, string> = {
  Narration: "地の文",
  Dialogue: "台詞",
  CharacterAction: "動作",
  Direction: "演出",
  Choice: "選択肢",
  Transition: "遷移",
  VariableChange: "変数",
};

export const TEMPLATE_LABELS: Record<string, string> = {
  linear_adv: "一本道のADV",
  choice_adv: "選択肢型ADV",
  status_adv: "ステータス型ADV",
  rpg_sim: "RPG・シミュレーション",
  other: "その他の物語",
};

export function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

// ---- Phase 2: 参照ファイル・チャット・提案 ----

export interface ReferenceFile {
  id: string;
  project_id: string;
  name: string;
  content: string;
  pinned: boolean;
  revision: number;
}

export interface Chat {
  id: string;
  project_id: string;
  title: string;
  instructions: string;
  auto_select: boolean;
  file_ids: string[];
}

export interface DiffSegment {
  op: "equal" | "delete" | "insert";
  text: string;
}

export type ProposalOp =
  | "update_element"
  | "insert_element"
  | "delete_element"
  | "update_file"
  | "create_file"
  | "update_summary"
  | "insert_scene";
export type ProposalStatus = "pending" | "approved" | "rejected" | "stale";

export interface ProposalContent {
  title?: string;
  kind?: string;
  speaker?: string;
  body?: string;
  name?: string;
  content?: string;
}

export interface Proposal {
  id: string;
  message_id: string;
  op: ProposalOp;
  target_id: string | null;
  scene_id: string | null;
  before: ProposalContent | null;
  after: ProposalContent | null;
  reason: string;
  status: ProposalStatus;
  reject_reason: string;
  result_id: string | null;
  /** 人間が修正して採用した場合の、実際に反映した内容。 */
  final_content: ProposalContent | null;
  /** かんたん執筆で、確認を挟まずに反映したもの。 */
  auto_applied: boolean;
  diff: DiffSegment[];
}

export interface ReferencedFile {
  id: string;
  name: string;
  source: "request" | "chat" | "project" | "auto";
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  scene_id: string | null;
  referenced_files: ReferencedFile[];
  warnings: string[];
  proposals: Proposal[];
}

export interface ChatDetail extends Chat {
  messages: ChatMessage[];
}

export interface UsageStatus {
  month: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  monthly_token_limit: number;
  warn_percent: number;
  state: "ok" | "warning" | "stopped";
  by_model: { provider: string; model: string; requests: number; input_tokens: number; output_tokens: number }[];
}

export interface LlmModel {
  id: string;
  display_name: string;
  version: string | null;
  input_token_limit: number | null;
  output_token_limit: number | null;
  thinking: boolean | null;
}

export interface LlmSettings {
  provider: string;
  model: string;
  has_api_key: boolean;
  monthly_token_limit: number;
  warn_percent: number;
}

export interface Exchange {
  messages: ChatMessage[];
  usage: UsageStatus;
}

// ---- Phase 3: 編集履歴・スナップショット・嗜好 ----

export interface HistoryEntry {
  seq: number;
  scene_id: string | null;
  target_type: "element" | "file" | "scene" | "project";
  target_id: string;
  action: "create" | "update" | "delete" | "reorder" | "restore";
  actor: "human" | "ai_approved" | "ai_modified" | "restore";
  before: Record<string, unknown> | null;
  after: Record<string, unknown> | null;
  proposal_id: string | null;
  created_at: string;
  updated_at: string;
  diff: DiffSegment[];
}

export interface SnapshotInfo {
  id: string;
  project_id: string;
  note: string;
  kind: "manual" | "before_restore";
  created_at: string;
  scene_count: number;
  element_count: number;
  file_count: number;
}

export interface CompareItem {
  item_type: "scene" | "file";
  id: string;
  title: string;
  status: "added" | "removed" | "changed";
  diff: DiffSegment[];
}

export interface PreferenceRule {
  id: string;
  project_id: string | null;
  text: string;
  category: string;
  confidence: number;
  evidence: string[];
  source: "user" | "ai" | "copied";
  created_at: string;
  updated_at: string;
}

export interface PreferenceSuggestion {
  id: string;
  project_id: string;
  op: "add" | "update" | "remove";
  rule_id: string | null;
  rule_text: string | null;
  text: string;
  category: string;
  confidence: number;
  evidence: string[];
  reason: string;
  status: "pending" | "approved" | "rejected" | "stale";
}

export interface PreferenceProfile {
  global_rules: PreferenceRule[];
  project_rules: PreferenceRule[];
  suggestions: PreferenceSuggestion[];
  settings: PreferenceSettings;
  unanalyzed: number;
  status: AnalysisStatus;
}

export interface PreferenceSettings {
  /** 分析結果を承認なしで適用する。 */
  auto_update: boolean;
  include_direct_edits: boolean;
  /** 分析そのものを自動で実行する。 */
  auto_analyze: boolean;
}

export interface AnalysisStatus {
  state: "idle" | "running" | "error";
  last_success_at: string | null;
  last_error_at: string | null;
  last_error: string | null;
  last_trigger: "manual" | "auto" | null;
}

export interface AnalysisResult {
  summary: string;
  suggestions: PreferenceSuggestion[];
  applied: boolean;
  warnings: string[];
  profile: PreferenceProfile;
}

export const CATEGORY_LABELS: Record<string, string> = {
  style: "文体",
  vocabulary: "語彙",
  length: "文章の長さ",
  avoid: "避けたい表現",
  balance: "台詞と地の文",
  tempo: "テンポ",
  character: "キャラクターの話し方",
  density: "演出・説明の密度",
  other: "その他",
};

// ---- Phase 4: 端末 ----

export interface Device {
  id: string;
  name: string;
  created_at: string;
  last_seen_at: string;
}

// ---- Phase 5: 変数・構造 ----

export interface Variable {
  id: string;
  project_id: string;
  name: string;
  var_type: "number" | "bool" | "text";
  initial: string;
  description: string;
}

export interface GraphNode {
  id: string;
  title: string;
  chapter_id: string;
  chapter_title: string;
  order: number;
  is_start: boolean;
  is_end: boolean;
  reachable: boolean;
  element_count: number;
  choice_count: number;
}

export interface GraphEdge {
  from: string;
  to: string;
  kind: "next" | "choice" | "transition";
  label: string;
  condition: string;
  effects: string;
  element_id: string | null;
}

export interface StructureIssue {
  severity: "error" | "warning" | "info";
  message: string;
  scene_id: string | null;
  element_id: string | null;
}

export interface Structure {
  nodes: GraphNode[];
  edges: GraphEdge[];
  characters: { name: string; lines: number; scenes: number }[];
  relations: { a: string; b: string; shared_scenes: number }[];
  variables: Variable[];
  variable_usage: { name: string; read_in: string[]; written_in: string[] }[];
  issues: StructureIssue[];
}

export const VAR_TYPE_LABELS: Record<string, string> = { number: "数値", bool: "真偽値", text: "文字列" };

// ---- キャラクター・段階的な生成 ----

export interface Character {
  id: string;
  project_id: string;
  name: string;
  aliases: string[];
}

export interface CharacterFact {
  id: string;
  character_id: string;
  category: string;
  text: string;
  source: "user" | "confirmed" | "inferred";
  status: "active" | "proposed" | "rejected";
}

export interface CharacterDraft {
  name: string;
  facts: { category: string; text: string }[];
}

export interface ChapterDraft {
  title: string;
  scenes: { title: string; summary: string }[];
}
