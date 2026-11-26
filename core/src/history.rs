//! 編集履歴とスナップショットの型と、DB に依存しない処理（比較など）。PC 版と Cloudflare 版で共有する。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::characters::{CharacterFact, CharacterRelation};
use crate::diff::{diff_text, Segment};
use crate::files::ReferenceFile;
use crate::model::ElementContent;

pub const TARGET_ELEMENT: &str = "element";
pub const TARGET_FILE: &str = "file";
pub const TARGET_SCENE: &str = "scene";

pub const ACTION_CREATE: &str = "create";
pub const ACTION_UPDATE: &str = "update";
pub const ACTION_DELETE: &str = "delete";
pub const ACTION_REORDER: &str = "reorder";
pub const ACTION_RESTORE: &str = "restore";

pub const ACTOR_HUMAN: &str = "human";
pub const ACTOR_AI_APPROVED: &str = "ai_approved";
pub const ACTOR_AI_MODIFIED: &str = "ai_modified";
pub const ACTOR_RESTORE: &str = "restore";

/// 人間の連続した更新を 1 件にまとめる時間（秒）。
pub const COALESCE_SECONDS: i64 = 120;
/// プロジェクトごとの履歴の保持件数の初期値。
pub const DEFAULT_HISTORY_LIMIT: i64 = 5000;
pub const KEY_HISTORY_LIMIT: &str = "history.max_entries";

pub const SNAPSHOT_MANUAL: &str = "manual";
pub const SNAPSHOT_BEFORE_RESTORE: &str = "before_restore";

/// 履歴に記録する 1 件の変更。
pub struct Change<'a> {
    pub project_id: &'a str,
    pub scene_id: Option<&'a str>,
    pub target_type: &'a str,
    pub target_id: &'a str,
    pub action: &'a str,
    pub actor: &'a str,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub proposal_id: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub seq: i64,
    pub scene_id: Option<String>,
    pub target_type: String,
    pub target_id: String,
    pub action: String,
    pub actor: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub proposal_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// 本文の変更前後の差分。
    pub diff: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub project_id: String,
    pub note: String,
    pub kind: String,
    pub created_at: String,
    pub scene_count: usize,
    pub element_count: usize,
    pub file_count: usize,
}

/// スナップショットに保存するプロジェクトの内容。ID をそのまま保持する。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotData {
    pub format: String,
    pub schema_version: i64,
    pub project: SnapshotProject,
    pub chapters: Vec<SnapshotChapter>,
    pub files: Vec<SnapshotFile>,
    /// キャラクター（情報つき）。v6 より前のスナップショットにはないため、なければ空として読む。
    #[serde(default)]
    pub characters: Vec<SnapshotCharacter>,
    #[serde(default)]
    pub character_relations: Vec<CharacterRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotCharacter {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub facts: Vec<CharacterFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotProject {
    pub name: String,
    pub description: String,
    pub templates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotChapter {
    pub id: String,
    pub title: String,
    pub scenes: Vec<SnapshotScene>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotScene {
    pub id: String,
    pub title: String,
    /// 場面の概要。v7 より前のスナップショットにはない。
    #[serde(default)]
    pub summary: String,
    pub elements: Vec<SnapshotElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotElement {
    pub id: String,
    pub kind: String,
    pub speaker: String,
    pub body: String,
    #[serde(default)]
    pub attrs: Value,
    pub origin: String,
    #[serde(default)]
    pub character_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotFile {
    pub id: String,
    pub name: String,
    pub content: String,
    pub pinned: bool,
}

/// 比較結果の 1 項目（場面またはファイル単位）。
#[derive(Debug, Clone, Serialize)]
pub struct CompareItem {
    /// scene / file
    pub item_type: &'static str,
    pub id: String,
    /// 章名を含む表示名。
    pub title: String,
    /// added / removed / changed
    pub status: &'static str,
    /// 本文（場面は Markdown 表現）の差分。`before` が過去、`after` が現在。
    pub diff: Vec<Segment>,
}

pub fn element_value(kind: &str, speaker: &str, body: &str) -> Value {
    json!({ "kind": kind, "speaker": speaker, "body": body })
}

pub fn file_value(f: &ReferenceFile) -> Value {
    json!({ "name": f.name, "content": f.content, "pinned": f.pinned })
}

pub fn parse_json(text: Option<String>) -> Option<Value> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

pub fn text_field<'a>(v: &'a Option<Value>, key: &str) -> &'a str {
    v.as_ref().and_then(|v| v.get(key)).and_then(Value::as_str).unwrap_or("")
}


pub fn scene_markdown_of(scene: &SnapshotScene) -> String {
    let contents: Vec<ElementContent> = scene
        .elements
        .iter()
        .map(|e| ElementContent {
            kind: e.kind.clone(),
            speaker: e.speaker.clone(),
            body: e.body.clone(),
        })
        .collect();
    crate::markdown::serialize_elements(&contents)
}


/// 履歴の 1 件の、本文の変更前後の差分。
pub fn entry_diff(target_type: &str, before: &Option<Value>, after: &Option<Value>) -> Vec<Segment> {
    let key = if target_type == TARGET_FILE { "content" } else { "body" };
    diff_text(text_field(before, key), text_field(after, key))
}

/// スナップショットの件数などの概要。
pub fn snapshot_info(id: &str, project_id: &str, note: &str, kind: &str, created_at: &str, data: &SnapshotData) -> SnapshotInfo {
    SnapshotInfo {
        id: id.to_string(),
        project_id: project_id.to_string(),
        note: note.to_string(),
        kind: kind.to_string(),
        created_at: created_at.to_string(),
        scene_count: data.chapters.iter().map(|c| c.scenes.len()).sum(),
        element_count: data.chapters.iter().flat_map(|c| &c.scenes).map(|s| s.elements.len()).sum(),
        file_count: data.files.len(),
    }
}

/// スナップショットのメモの検証。
pub fn clean_note(note: &str) -> crate::validate::Checked<String> {
    let note = note.trim();
    if note.chars().count() > 1000 {
        return Err("メモは1000文字以内で入力してください".into());
    }
    Ok(note.to_string())
}

/// 過去（`past`）と現在（`now`）を、場面・参照ファイル単位で比較する。
pub fn compare(past: &SnapshotData, now: &SnapshotData) -> Vec<CompareItem> {
    let scenes = |d: &SnapshotData| -> Vec<(String, String, String)> {
        d.chapters
        .iter()
        .flat_map(|c| {
            c.scenes
            .iter()
            .map(move |s| {
                let text = if s.summary.is_empty() {
                scene_markdown_of(s)
                } else {
                format!("【概要】{}\n\n{}", s.summary, scene_markdown_of(s))
                };
                (s.id.clone(), format!("{} / {}", c.title, s.title), text)
            })
        })
        .collect()
    };
    let files = |d: &SnapshotData| -> Vec<(String, String, String)> {
        d.files.iter().map(|f| (f.id.clone(), f.name.clone(), f.content.clone())).collect()
    };
    let mut out = Vec::new();
    for (item_type, past_items, now_items) in [
        ("scene", scenes(past), scenes(now)),
        ("file", files(past), files(now)),
    ] {
        for (id, title, text) in &now_items {
        match past_items.iter().find(|(pid, _, _)| pid == id) {
            Some((_, past_title, past_text)) => {
            if past_text != text || past_title != title {
                out.push(CompareItem {
                item_type,
                id: id.clone(),
                title: if past_title == title { title.clone() } else { format!("{past_title} → {title}") },
                status: "changed",
                diff: diff_text(past_text, text),
                });
            }
            }
            None => out.push(CompareItem {
            item_type,
            id: id.clone(),
            title: title.clone(),
            status: "added",
            diff: diff_text("", text),
            }),
        }
        }
        for (id, title, text) in &past_items {
        if !now_items.iter().any(|(nid, _, _)| nid == id) {
            out.push(CompareItem {
            item_type,
            id: id.clone(),
            title: title.clone(),
            status: "removed",
            diff: diff_text(text, ""),
            });
        }
        }
    }
    out
}
