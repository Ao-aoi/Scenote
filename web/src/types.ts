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
  position: number;
  origin: string;
  revision: number;
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
