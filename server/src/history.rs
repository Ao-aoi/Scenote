//! 編集履歴とスナップショット。
//!
//! 履歴は要素・参照ファイル単位で変更前後の内容を残す。人間の連続した入力（自動保存）は、
//! 同じ対象への短時間の更新を 1 件にまとめて肥大化を防ぐ。保持件数はプロジェクトごとの上限で切り詰める。

use rusqlite::{params, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::diff::{diff_text, Segment};
use crate::error::{AppError, Result};
use crate::model::*;
use crate::store::{new_id, Store, NOW};
use crate::store_ai::ReferenceFile;

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
const COALESCE_SECONDS: i64 = 120;
/// プロジェクトごとの履歴の保持件数の初期値。
pub const DEFAULT_HISTORY_LIMIT: i64 = 5000;
pub(crate) const KEY_HISTORY_LIMIT: &str = "history.max_entries";

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

fn parse_json(text: Option<String>) -> Option<Value> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

fn text_field<'a>(v: &'a Option<Value>, key: &str) -> &'a str {
    v.as_ref().and_then(|v| v.get(key)).and_then(Value::as_str).unwrap_or("")
}

fn entry_from_row(row: &Row) -> rusqlite::Result<HistoryEntry> {
    let target_type: String = row.get("target_type")?;
    let before = parse_json(row.get("before")?);
    let after = parse_json(row.get("after")?);
    let key = if target_type == TARGET_FILE { "content" } else { "body" };
    let diff = diff_text(text_field(&before, key), text_field(&after, key));
    Ok(HistoryEntry {
        seq: row.get("seq")?,
        scene_id: row.get("scene_id")?,
        target_type,
        target_id: row.get("target_id")?,
        action: row.get("action")?,
        actor: row.get("actor")?,
        before,
        after,
        proposal_id: row.get("proposal_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        diff,
    })
}

/// 場面が属するプロジェクトの ID。
pub(crate) fn project_of_scene(tx: &Transaction, scene_id: &str) -> Result<String> {
    tx.query_row(
        "SELECT c.project_id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?1",
        [scene_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or(AppError::NotFound("場面"))
}

/// 変更を履歴に記録する。呼び出し側のトランザクション内で実行する。
pub(crate) fn record(tx: &Transaction, change: Change) -> Result<()> {
    if change.before.is_some() && change.before == change.after {
        return Ok(());
    }
    let before = change.before.as_ref().map(Value::to_string);
    let after = change.after.as_ref().map(Value::to_string);
    // 人間による同じ対象への連続した更新は、直前の 1 件にまとめる（変更前の内容は最初のものを保つ）。
    if change.action == ACTION_UPDATE && change.actor == ACTOR_HUMAN && change.proposal_id.is_none() {
        let latest: Option<(i64, String, String, String, Option<String>)> = tx
            .query_row(
                "SELECT seq, target_id, action, actor, proposal_id FROM history
                     WHERE project_id = ?1 ORDER BY seq DESC LIMIT 1",
                [change.project_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some((seq, target, action, actor, proposal)) = latest {
            let recent: bool = tx.query_row(
                &format!(
                    "SELECT (julianday({NOW}) - julianday(updated_at)) * 86400 <= ?2 FROM history WHERE seq = ?1"
                ),
                params![seq, COALESCE_SECONDS],
                |r| r.get(0),
            )?;
            if recent
                && target == change.target_id
                && action == ACTION_UPDATE
                && actor == ACTOR_HUMAN
                && proposal.is_none()
            {
                tx.execute(
                    &format!("UPDATE history SET after = ?2, updated_at = {NOW} WHERE seq = ?1"),
                    params![seq, after],
                )?;
                return Ok(());
            }
        }
    }
    tx.execute(
        &format!(
            "INSERT INTO history (project_id, scene_id, target_type, target_id, action, actor, before, after,
                                  proposal_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, {NOW}, {NOW})"
        ),
        params![
            change.project_id,
            change.scene_id,
            change.target_type,
            change.target_id,
            change.action,
            change.actor,
            before,
            after,
            change.proposal_id
        ],
    )?;
    // 保持件数を超えた古い履歴を削除する。
    let limit: i64 = tx
        .query_row("SELECT value FROM settings WHERE key = ?1", [KEY_HISTORY_LIMIT], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .and_then(|v| v.parse().ok())
        .filter(|n: &i64| *n > 0)
        .unwrap_or(DEFAULT_HISTORY_LIMIT);
    tx.execute(
        "DELETE FROM history WHERE project_id = ?1 AND seq <= (
             SELECT seq FROM history WHERE project_id = ?1 ORDER BY seq DESC LIMIT 1 OFFSET ?2)",
        params![change.project_id, limit],
    )?;
    Ok(())
}

/// 要素の変更を記録する簡易版。
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_element(
    tx: &Transaction,
    scene_id: &str,
    element_id: &str,
    action: &str,
    actor: &str,
    before: Option<&ElementContent>,
    after: Option<&ElementContent>,
    proposal_id: Option<&str>,
) -> Result<()> {
    let project_id = project_of_scene(tx, scene_id)?;
    let to_value = |c: &ElementContent| element_value(&c.kind, &c.speaker, &c.body);
    record(
        tx,
        Change {
            project_id: &project_id,
            scene_id: Some(scene_id),
            target_type: TARGET_ELEMENT,
            target_id: element_id,
            action,
            actor,
            before: before.map(to_value),
            after: after.map(to_value),
            proposal_id,
        },
    )
}

fn scene_markdown_of(scene: &SnapshotScene) -> String {
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

impl Store {
    // ---- 編集履歴 -----------------------------------------------------

    /// 履歴を新しい順に返す。`scene_id` を指定するとその場面の要素の履歴だけを返す。
    pub fn list_history(
        &self,
        project_id: &str,
        scene_id: Option<&str>,
        before_seq: Option<i64>,
        limit: i64,
    ) -> Result<Vec<HistoryEntry>> {
        self.get_project(project_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT * FROM history WHERE project_id = ?1 AND (?2 IS NULL OR scene_id = ?2)
               AND (?3 IS NULL OR seq < ?3)
             ORDER BY seq DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![project_id, scene_id, before_seq, limit.clamp(1, 500)],
            entry_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn history_limit(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [KEY_HISTORY_LIMIT], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_HISTORY_LIMIT))
    }

    pub fn set_history_limit(&mut self, limit: i64) -> Result<()> {
        if !(100..=1_000_000).contains(&limit) {
            return Err(AppError::Invalid("履歴の保持件数は 100〜1,000,000 件で指定してください".into()));
        }
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![KEY_HISTORY_LIMIT, limit.to_string()],
        )?;
        Ok(())
    }

    // ---- スナップショット ---------------------------------------------

    /// 現在のプロジェクトの内容を、スナップショット形式で取り出す。
    pub fn snapshot_data(&self, project_id: &str) -> Result<SnapshotData> {
        let tree = self.project_tree(project_id)?;
        let mut chapters = Vec::new();
        for node in &tree.chapters {
            let mut scenes = Vec::new();
            for scene in &node.scenes {
                let detail = self.scene_detail(&scene.id)?;
                scenes.push(SnapshotScene {
                    id: scene.id.clone(),
                    title: scene.title.clone(),
                    elements: detail
                        .elements
                        .into_iter()
                        .map(|e| SnapshotElement {
                            id: e.id,
                            kind: e.kind,
                            speaker: e.speaker,
                            body: e.body,
                            attrs: e.attrs,
                            origin: e.origin,
                        })
                        .collect(),
                });
            }
            chapters.push(SnapshotChapter {
                id: node.chapter.id.clone(),
                title: node.chapter.title.clone(),
                scenes,
            });
        }
        let files = self
            .list_files(project_id)?
            .into_iter()
            .map(|f| SnapshotFile {
                id: f.id,
                name: f.name,
                content: f.content,
                pinned: f.pinned,
            })
            .collect();
        Ok(SnapshotData {
            format: "scenote-snapshot".into(),
            schema_version: SCHEMA_VERSION,
            project: SnapshotProject {
                name: tree.project.name,
                description: tree.project.description,
                templates: tree.project.templates,
            },
            chapters,
            files,
        })
    }

    fn snapshot_info(&self, id: &str, project_id: &str, note: &str, kind: &str, created_at: &str, data: &SnapshotData) -> SnapshotInfo {
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

    pub fn create_snapshot(&mut self, project_id: &str, note: &str, kind: &str) -> Result<SnapshotInfo> {
        let note = note.trim();
        if note.chars().count() > 1000 {
            return Err(AppError::Invalid("メモは1000文字以内で入力してください".into()));
        }
        let data = self.snapshot_data(project_id)?;
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO snapshots (id, project_id, note, kind, data, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
            ),
            params![id, project_id, note, kind, serde_json::to_string(&data)?],
        )?;
        let created_at: String =
            self.conn.query_row("SELECT created_at FROM snapshots WHERE id = ?1", [&id], |r| r.get(0))?;
        Ok(self.snapshot_info(&id, project_id, note, kind, &created_at, &data))
    }

    fn load_snapshot(&self, id: &str) -> Result<(SnapshotInfo, SnapshotData)> {
        let (project_id, note, kind, data, created_at): (String, String, String, String, String) = self
            .conn
            .query_row(
                "SELECT project_id, note, kind, data, created_at FROM snapshots WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or(AppError::NotFound("スナップショット"))?;
        let data: SnapshotData = serde_json::from_str(&data)
            .map_err(|_| AppError::Invalid("スナップショットの内容を読み込めませんでした".into()))?;
        let info = self.snapshot_info(id, &project_id, &note, &kind, &created_at, &data);
        Ok((info, data))
    }

    pub fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotInfo>> {
        self.get_project(project_id)?;
        let ids: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM snapshots WHERE project_id = ?1 ORDER BY created_at DESC, rowid DESC",
            )?;
            let rows = stmt.query_map([project_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        ids.iter().map(|id| self.load_snapshot(id).map(|(info, _)| info)).collect()
    }

    pub fn delete_snapshot(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM snapshots WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("スナップショット"));
        }
        Ok(())
    }

    /// スナップショットと現在の状態を比較する（場面・参照ファイル単位）。
    pub fn compare_snapshot(&self, id: &str) -> Result<Vec<CompareItem>> {
        let (info, past) = self.load_snapshot(id)?;
        let now = self.snapshot_data(&info.project_id)?;
        let scenes = |d: &SnapshotData| -> Vec<(String, String, String)> {
            d.chapters
                .iter()
                .flat_map(|c| {
                    c.scenes
                        .iter()
                        .map(move |s| (s.id.clone(), format!("{} / {}", c.title, s.title), scene_markdown_of(s)))
                })
                .collect()
        };
        let files = |d: &SnapshotData| -> Vec<(String, String, String)> {
            d.files.iter().map(|f| (f.id.clone(), f.name.clone(), f.content.clone())).collect()
        };
        let mut out = Vec::new();
        for (item_type, past_items, now_items) in [
            ("scene", scenes(&past), scenes(&now)),
            ("file", files(&past), files(&now)),
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
        Ok(out)
    }

    /// スナップショットの状態へ復元する。
    ///
    /// 復元の直前に現在の状態を自動でスナップショットとして保存するため、復元しても現在の内容は失われない。
    /// チャット・提案・嗜好プロファイル・使用量は復元の対象外。ID はスナップショット時点のものを使う。
    pub fn restore_snapshot(&mut self, id: &str) -> Result<SnapshotInfo> {
        let (info, data) = self.load_snapshot(id)?;
        let project_id = info.project_id.clone();
        let backup = self.create_snapshot(
            &project_id,
            &format!("「{}」へ復元する前の状態（自動保存）", if info.note.is_empty() { &info.created_at } else { &info.note }),
            SNAPSHOT_BEFORE_RESTORE,
        )?;
        let current = self.snapshot_data(&project_id)?;
        // 復元後のリビジョンは、復元前のどの値よりも大きくする（古い画面からの上書きを競合として検出するため）。
        let max_revision: i64 = self.conn.query_row(
            "SELECT MAX(r) FROM (
                SELECT COALESCE(MAX(e.revision), 0) AS r FROM elements e JOIN scenes s ON s.id = e.scene_id
                  JOIN chapters c ON c.id = s.chapter_id WHERE c.project_id = ?1
                UNION ALL SELECT COALESCE(MAX(s.revision), 0) FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE c.project_id = ?1
                UNION ALL SELECT COALESCE(MAX(revision), 0) FROM reference_files WHERE project_id = ?1)",
            [&project_id],
            |r| r.get(0),
        )?;
        let revision = max_revision + 1;
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "UPDATE projects SET name = ?2, description = ?3, templates = ?4, updated_at = {NOW} WHERE id = ?1"
            ),
            params![
                project_id,
                data.project.name,
                data.project.description,
                serde_json::to_string(&data.project.templates)?
            ],
        )?;
        // ID が別プロジェクトで使われている場合（スナップショット後に移動したなど）に備え、新しい ID を振り直す。
        let taken = |table: &str, id: &str| -> Result<bool> {
            Ok(tx
                .query_row(&format!("SELECT 1 FROM {table} WHERE id = ?1"), [id], |_| Ok(true))
                .optional()?
                .unwrap_or(false))
        };
        tx.execute("DELETE FROM chapters WHERE project_id = ?1", [&project_id])?;
        tx.execute("DELETE FROM reference_files WHERE project_id = ?1", [&project_id])?;
        for (ci, chapter) in data.chapters.iter().enumerate() {
            let chapter_id = if taken("chapters", &chapter.id)? { new_id() } else { chapter.id.clone() };
            tx.execute(
                &format!(
                    "INSERT INTO chapters (id, project_id, title, position, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
                ),
                params![chapter_id, project_id, chapter.title, ci as i64],
            )?;
            for (si, scene) in chapter.scenes.iter().enumerate() {
                let scene_id = if taken("scenes", &scene.id)? { new_id() } else { scene.id.clone() };
                tx.execute(
                    &format!(
                        "INSERT INTO scenes (id, chapter_id, title, position, revision, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, {NOW}, {NOW})"
                    ),
                    params![scene_id, chapter_id, scene.title, si as i64, revision],
                )?;
                for (ei, e) in scene.elements.iter().enumerate() {
                    let element_id = if taken("elements", &e.id)? { new_id() } else { e.id.clone() };
                    let attrs = if e.attrs.is_object() { e.attrs.to_string() } else { "{}".to_string() };
                    tx.execute(
                        &format!(
                            "INSERT INTO elements (id, scene_id, kind, body, speaker, attrs, position, origin, revision,
                                                   created_at, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, {NOW}, {NOW})"
                        ),
                        params![element_id, scene_id, e.kind, e.body, e.speaker, attrs, ei as i64, e.origin, revision],
                    )?;
                }
            }
        }
        for f in &data.files {
            let file_id = if taken("reference_files", &f.id)? { new_id() } else { f.id.clone() };
            tx.execute(
                &format!(
                    "INSERT INTO reference_files (id, project_id, name, content, pinned, revision, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW}, {NOW})"
                ),
                params![file_id, project_id, f.name, f.content, f.pinned as i64, revision],
            )?;
        }
        record(
            &tx,
            Change {
                project_id: &project_id,
                scene_id: None,
                target_type: "project",
                target_id: &project_id,
                action: ACTION_RESTORE,
                actor: ACTOR_RESTORE,
                before: Some(json!({ "snapshot_id": backup.id, "scenes": current.chapters.iter().map(|c| c.scenes.len()).sum::<usize>() })),
                after: Some(json!({ "snapshot_id": info.id, "note": info.note })),
                proposal_id: None,
            },
        )?;
        tx.commit()?;
        Ok(backup)
    }
}
