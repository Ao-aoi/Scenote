//! シナリオのデータモデル。
//!
//! 要素の種類 (`kind`) は文字列で保持し、後から種類を追加できるようにしている。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// DB スキーマおよび JSON エクスポート形式のバージョン。
pub const SCHEMA_VERSION: i64 = 8;

pub const KIND_DIALOGUE: &str = "Dialogue";
pub const KIND_NARRATION: &str = "Narration";
pub const KIND_CHARACTER_ACTION: &str = "CharacterAction";
pub const KIND_DIRECTION: &str = "Direction";
pub const KIND_CHOICE: &str = "Choice";
/// 場面の遷移（分岐先への移動）。`attrs`: `{ "target": 場面ID, "condition": 条件式 }`
pub const KIND_TRANSITION: &str = "Transition";
/// 変数の変更。`attrs`: `{ "effects": 変更式 }`
pub const KIND_VARIABLE_CHANGE: &str = "VariableChange";

/// 初期版で UI から選択できる要素の種類。
pub const BASIC_KINDS: [&str; 7] = [
    KIND_NARRATION,
    KIND_DIALOGUE,
    KIND_CHARACTER_ACTION,
    KIND_DIRECTION,
    KIND_CHOICE,
    KIND_TRANSITION,
    KIND_VARIABLE_CHANGE,
];

/// プロジェクト作成時に選択できるテンプレート（複数選択可）。
pub const TEMPLATES: [&str; 5] = ["linear_adv", "choice_adv", "status_adv", "rpg_sim", "other"];

/// 編集の由来: 人間が作成・編集した内容。
pub const ORIGIN_HUMAN: &str = "human";
/// AI の提案をそのまま承認して反映した内容。
pub const ORIGIN_AI_APPROVED: &str = "ai_approved";
/// AI の提案を人間が修正してから反映した内容。
pub const ORIGIN_AI_MODIFIED: &str = "ai_modified";
/// AI 由来の内容を人間が編集したもの。
pub const ORIGIN_MIXED: &str = "mixed";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub templates: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Chapter {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Scene {
    pub id: String,
    pub chapter_id: String,
    pub title: String,
    /// 場面の概要（プロット）。場面で起きることを短く書いたもの。
    pub summary: String,
    pub position: i64,
    /// 場面とその要素に対する変更ごとに増える。競合検出に使う。
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Element {
    pub id: String,
    pub scene_id: String,
    pub kind: String,
    pub body: String,
    /// 関連キャラクター（台詞の話者など）。キャラクター管理の導入までは名前の文字列で持つ。
    pub speaker: String,
    /// 独立したキャラクターへの参照（`speaker` と併存し、未設定なら None）。
    pub character_id: Option<String>,
    /// 種類ごとの追加情報（条件・遷移先など）を入れる拡張用 JSON オブジェクト。
    pub attrs: Value,
    pub position: i64,
    pub origin: String,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChapterNode {
    #[serde(flatten)]
    pub chapter: Chapter,
    pub scenes: Vec<Scene>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectTree {
    pub project: Project,
    pub chapters: Vec<ChapterNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SceneDetail {
    #[serde(flatten)]
    pub scene: Scene,
    pub elements: Vec<Element>,
}

/// 要素の内容（ID・順序などプログラムが管理する情報を除いたもの）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementContent {
    pub kind: String,
    #[serde(default)]
    pub speaker: String,
    #[serde(default)]
    pub body: String,
}

/// 種類名として使える文字列か（英字始まりの英数字と `_`）。
pub fn is_valid_kind(kind: &str) -> bool {
    let mut chars = kind.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && kind.len() <= 64
}

/// 話者名として使える文字列か。Markdown 表現の区切り文字と改行は使えない。
pub fn is_valid_speaker(speaker: &str) -> bool {
    speaker.chars().count() <= 100
        && speaker.trim() == speaker
        && !speaker
            .chars()
            .any(|c| matches!(c, '[' | ']' | ':' | '*') || c.is_control())
}
