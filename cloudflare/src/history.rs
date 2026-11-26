//! 編集履歴とスナップショット（Cloudflare 版）。規則は PC 版の `server/src/history.rs` と同じ。
//!
//! - 要素・参照ファイル・場面の変更を、変更前後の内容と由来（人間・AI（MCP）・復元）つきで記録する。
//! - 人間による同じ対象への連続した更新は、120 秒以内なら 1 件にまとめる。保持件数を超えた古い履歴は削除する。
//! - スナップショットはプロジェクト全体を ID つきの JSON で保存する。復元の直前に現在の状態を自動で保存する。

use serde::Deserialize;
use serde_json::{json, Value};

use scenote_core::history::*;
use scenote_core::model::*;

use crate::db::NOW;
use crate::error::{AppError, Result};
use crate::p;
use crate::store::{new_id, Store};

/// 履歴に記録する 1 件の変更。
pub struct Change<'a> {
    pub project_id: &'a str,
    pub scene_id: Option<&'a str>,
    pub target_type: &'a str,
    pub target_id: &'a str,
    pub action: &'a str,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

#[derive(Deserialize)]
struct EntryRow {
    seq: i64,
    scene_id: Option<String>,
    target_type: String,
    target_id: String,
    action: String,
    actor: String,
    before: Option<String>,
    after: Option<String>,
    proposal_id: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<EntryRow> for HistoryEntry {
    fn from(r: EntryRow) -> Self {
        let before = parse_json(r.before);
        let after = parse_json(r.after);
        let diff = entry_diff(&r.target_type, &before, &after);
        HistoryEntry {
            seq: r.seq,
            scene_id: r.scene_id,
            target_type: r.target_type,
            target_id: r.target_id,
            action: r.action,
            actor: r.actor,
            before,
            after,
            proposal_id: r.proposal_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
            diff,
        }
    }
}

#[derive(Deserialize)]
struct LatestRow {
    seq: i64,
    target_id: String,
    action: String,
    actor: String,
    proposal_id: Option<String>,
    recent: i64,
}

#[derive(Deserialize)]
struct SnapshotRow {
    project_id: String,
    note: String,
    kind: String,
    data: String,
    created_at: String,
}

#[derive(Deserialize)]
struct ValueRow {
    value: String,
}

impl Store {
    /// 変更の主体（由来から決める）。
    pub fn actor(&self) -> &'static str {
        if self.origin == ORIGIN_AI_DIRECT {
            ACTOR_AI_DIRECT
        } else {
            ACTOR_HUMAN
        }
    }

    pub fn project_of_scene(&self, scene_id: &str) -> Result<String> {
        self.db.scalar(
            "SELECT c.project_id AS v FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?",
            p![scene_id],
        )
    }

    /// 変更を履歴に記録する。呼び出し側のトランザクション内で実行する。
    pub fn record(&self, change: Change) -> Result<()> {
        if change.before.is_some() && change.before == change.after {
            return Ok(());
        }
        let actor = self.actor();
        let before = change.before.as_ref().map(Value::to_string);
        let after = change.after.as_ref().map(Value::to_string);
        // 同じ主体による同じ対象への連続した更新は、直前の 1 件にまとめる（変更前の内容は最初のものを保つ）。
        if change.action == ACTION_UPDATE {
            let latest: Option<LatestRow> = self.db.opt(
                &format!(
                    "SELECT seq, target_id, action, actor, proposal_id,
                            ((julianday({NOW}) - julianday(updated_at)) * 86400 <= ?) AS recent
                     FROM history WHERE project_id = ? ORDER BY seq DESC LIMIT 1"
                ),
                p![COALESCE_SECONDS, change.project_id],
            )?;
            if let Some(l) = latest {
                if l.recent != 0
                    && l.target_id == change.target_id
                    && l.action == ACTION_UPDATE
                    && l.actor == actor
                    && l.proposal_id.is_none()
                {
                    self.db.exec(
                        &format!("UPDATE history SET after = ?, updated_at = {NOW} WHERE seq = ?"),
                        p![after, l.seq],
                    )?;
                    return Ok(());
                }
            }
        }
        self.db.exec(
            &format!(
                "INSERT INTO history (project_id, scene_id, target_type, target_id, action, actor, before, after,
                                      proposal_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, {NOW}, {NOW})"
            ),
            p![change.project_id, change.scene_id, change.target_type, change.target_id, change.action, actor, before, after],
        )?;
        let limit = self.history_limit()?;
        self.db.exec(
            "DELETE FROM history WHERE project_id = ? AND seq <= (
                 SELECT seq FROM history WHERE project_id = ? ORDER BY seq DESC LIMIT 1 OFFSET ?)",
            p![change.project_id, change.project_id, limit],
        )
    }

    /// 要素の変更を記録する。
    pub fn record_element(
        &self,
        scene_id: &str,
        element_id: &str,
        action: &str,
        before: Option<(&ElementContent, Option<&str>)>,
        after: Option<(&ElementContent, Option<&str>)>,
    ) -> Result<()> {
        let project_id = self.project_of_scene(scene_id)?;
        let to_value = |(c, character): (&ElementContent, Option<&str>)| {
            let mut v = element_value(&c.kind, &c.speaker, &c.body);
            if character.is_some() {
                v["character_id"] = json!(character);
            }
            v
        };
        self.record(Change {
            project_id: &project_id,
            scene_id: Some(scene_id),
            target_type: TARGET_ELEMENT,
            target_id: element_id,
            action,
            before: before.map(to_value),
            after: after.map(to_value),
        })
    }

    /// 場面の変更（題名・概要・削除・並べ替え）を記録する。
    pub fn record_scene(&self, project_id: &str, scene: &Scene, action: &str, before: Option<Value>, after: Option<Value>) -> Result<()> {
        self.record(Change {
            project_id,
            scene_id: Some(&scene.id),
            target_type: TARGET_SCENE,
            target_id: &scene.id,
            action,
            before,
            after,
        })
    }

    // ---- 編集履歴 -----------------------------------------------------

    /// 履歴を新しい順に返す。`scene_id` を指定するとその場面の履歴だけを返す。
    pub fn list_history(&self, project_id: &str, scene_id: Option<&str>, before_seq: Option<i64>, limit: i64) -> Result<Vec<HistoryEntry>> {
        self.get_project(project_id)?;
        let rows: Vec<EntryRow> = self.db.all(
            "SELECT * FROM history WHERE project_id = ? AND (? IS NULL OR scene_id = ?)
               AND (? IS NULL OR seq < ?)
             ORDER BY seq DESC LIMIT ?",
            p![project_id, scene_id, scene_id, before_seq, before_seq, limit.clamp(1, 500)],
        )?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn history_limit(&self) -> Result<i64> {
        Ok(self
            .db
            .opt::<ValueRow>("SELECT value FROM settings WHERE key = ?", p![KEY_HISTORY_LIMIT])?
            .and_then(|r| r.value.parse().ok())
            .filter(|n: &i64| *n > 0)
            .unwrap_or(DEFAULT_HISTORY_LIMIT))
    }

    pub fn set_history_limit(&self, limit: i64) -> Result<()> {
        if !(100..=1_000_000).contains(&limit) {
            return Err(AppError::Invalid("履歴の保持件数は 100〜1,000,000 件で指定してください".into()));
        }
        self.db.exec(
            "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            p![KEY_HISTORY_LIMIT, limit.to_string()],
        )
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
                    summary: scene.summary.clone(),
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
                            character_id: e.character_id,
                        })
                        .collect(),
                });
            }
            chapters.push(SnapshotChapter { id: node.chapter.id.clone(), title: node.chapter.title.clone(), scenes });
        }
        let files = self
            .list_files(project_id)?
            .into_iter()
            .map(|f| SnapshotFile { id: f.id, name: f.name, content: f.content, pinned: f.pinned })
            .collect();
        let mut characters = Vec::new();
        for c in self.list_characters(project_id)? {
            characters.push(SnapshotCharacter { facts: self.list_facts(&c.id)?, id: c.id, name: c.name, aliases: c.aliases });
        }
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
            characters,
            character_relations: self.list_relations(project_id)?,
        })
    }

    pub fn create_snapshot(&self, project_id: &str, note: &str, kind: &str) -> Result<SnapshotInfo> {
        let note = clean_note(note).map_err(AppError::Invalid)?;
        let data = self.snapshot_data(project_id)?;
        let id = new_id();
        let text = serde_json::to_string(&data).map_err(|e| AppError::Internal(e.to_string()))?;
        self.db.exec(
            &format!("INSERT INTO snapshots (id, project_id, note, kind, data, created_at) VALUES (?, ?, ?, ?, ?, {NOW})"),
            p![id.as_str(), project_id, note.as_str(), kind, text],
        )?;
        let created_at: String = self.db.scalar("SELECT created_at AS v FROM snapshots WHERE id = ?", p![id.as_str()])?;
        Ok(snapshot_info(&id, project_id, &note, kind, &created_at, &data))
    }

    fn load_snapshot(&self, id: &str) -> Result<(SnapshotInfo, SnapshotData)> {
        let row: SnapshotRow = self
            .db
            .opt("SELECT project_id, note, kind, data, created_at FROM snapshots WHERE id = ?", p![id])?
            .ok_or(AppError::NotFound("スナップショット"))?;
        let data: SnapshotData = serde_json::from_str(&row.data)
            .map_err(|_| AppError::Invalid("スナップショットの内容を読み込めませんでした".into()))?;
        let info = snapshot_info(id, &row.project_id, &row.note, &row.kind, &row.created_at, &data);
        Ok((info, data))
    }

    pub fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotInfo>> {
        self.get_project(project_id)?;
        #[derive(Deserialize)]
        struct IdRow {
            id: String,
        }
        let ids: Vec<IdRow> = self.db.all(
            "SELECT id FROM snapshots WHERE project_id = ? ORDER BY created_at DESC, rowid DESC",
            p![project_id],
        )?;
        ids.iter().map(|r| self.load_snapshot(&r.id).map(|(info, _)| info)).collect()
    }

    pub fn delete_snapshot(&self, id: &str) -> Result<()> {
        self.load_snapshot(id)?;
        self.db.exec("DELETE FROM snapshots WHERE id = ?", p![id])
    }

    /// スナップショットと現在の状態を比較する（場面・参照ファイル単位）。
    pub fn compare_snapshot(&self, id: &str) -> Result<Vec<CompareItem>> {
        let (info, past) = self.load_snapshot(id)?;
        let now = self.snapshot_data(&info.project_id)?;
        Ok(compare(&past, &now))
    }

    fn taken(&self, table: &str, id: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct One {
            #[allow(dead_code)]
            v: i64,
        }
        Ok(self.db.opt::<One>(&format!("SELECT 1 AS v FROM {table} WHERE id = ?"), p![id])?.is_some())
    }

    /// プロジェクトの本文・参照ファイル・キャラクターを削除する（復元とプロジェクトの削除で使う）。
    pub fn clear_project_content(&self, project_id: &str) -> Result<()> {
        self.db.exec("UPDATE elements SET character_id = NULL WHERE character_id IN (SELECT id FROM characters WHERE project_id = ?)", p![project_id])?;
        self.db.exec(
            "DELETE FROM elements WHERE scene_id IN
               (SELECT s.id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE c.project_id = ?)",
            p![project_id],
        )?;
        self.db.exec("DELETE FROM scenes WHERE chapter_id IN (SELECT id FROM chapters WHERE project_id = ?)", p![project_id])?;
        self.db.exec("DELETE FROM chapters WHERE project_id = ?", p![project_id])?;
        self.db.exec("DELETE FROM reference_files WHERE project_id = ?", p![project_id])?;
        self.db.exec("DELETE FROM character_relations WHERE project_id = ?", p![project_id])?;
        self.db.exec("DELETE FROM character_facts WHERE character_id IN (SELECT id FROM characters WHERE project_id = ?)", p![project_id])?;
        self.db.exec("DELETE FROM characters WHERE project_id = ?", p![project_id])
    }

    /// スナップショットの状態へ復元する。応答は、復元の直前に自動保存したスナップショット。
    pub fn restore_snapshot(&self, id: &str) -> Result<SnapshotInfo> {
        let (info, data) = self.load_snapshot(id)?;
        let project_id = info.project_id.clone();
        let backup = self.create_snapshot(
            &project_id,
            &format!("「{}」へ復元する前の状態（自動保存）", if info.note.is_empty() { &info.created_at } else { &info.note }),
            SNAPSHOT_BEFORE_RESTORE,
        )?;
        let current_scenes: usize = self.snapshot_data(&project_id)?.chapters.iter().map(|c| c.scenes.len()).sum();
        // 復元後のリビジョンは、復元前のどの値よりも大きくする（古い画面からの上書きを競合として検出するため）。
        let max_revision: i64 = self.db.scalar(
            "SELECT MAX(r) AS v FROM (
                SELECT COALESCE(MAX(e.revision), 0) AS r FROM elements e JOIN scenes s ON s.id = e.scene_id
                  JOIN chapters c ON c.id = s.chapter_id WHERE c.project_id = ?
                UNION ALL SELECT COALESCE(MAX(s.revision), 0) FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE c.project_id = ?
                UNION ALL SELECT COALESCE(MAX(revision), 0) FROM reference_files WHERE project_id = ?)",
            p![project_id.as_str(), project_id.as_str(), project_id.as_str()],
        )?;
        let revision = max_revision + 1;
        self.db.exec(
            &format!("UPDATE projects SET name = ?, description = ?, templates = ?, updated_at = {NOW} WHERE id = ?"),
            p![
                data.project.name.as_str(),
                data.project.description.as_str(),
                serde_json::to_string(&data.project.templates).unwrap_or_default(),
                project_id.as_str()
            ],
        )?;
        self.clear_project_content(&project_id)?;
        // ID が他で使われている場合（スナップショット後に移動したなど）は新しい ID を振り直す。
        let fresh = |table: &str, id: &str| -> Result<String> { Ok(if self.taken(table, id)? { new_id() } else { id.to_string() }) };
        let mut character_ids = std::collections::HashMap::new();
        for c in &data.characters {
            let cid = fresh("characters", &c.id)?;
            self.db.exec(
                &format!("INSERT INTO characters (id, project_id, name, aliases, created_at, updated_at) VALUES (?, ?, ?, ?, {NOW}, {NOW})"),
                p![cid.as_str(), project_id.as_str(), c.name.as_str(), json!(c.aliases).to_string()],
            )?;
            for f in &c.facts {
                let fid = fresh("character_facts", &f.id)?;
                self.db.exec(
                    &format!(
                        "INSERT INTO character_facts (id, character_id, category, text, source, confidence, evidence, status, created_at, updated_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
                    ),
                    p![fid, cid.as_str(), f.category.as_str(), f.text.as_str(), f.source.as_str(), f.confidence, json!(f.evidence).to_string(), f.status.as_str()],
                )?;
            }
            character_ids.insert(c.id.clone(), cid);
        }
        for r in &data.character_relations {
            let (Some(from), Some(to)) = (character_ids.get(&r.from_character_id), character_ids.get(&r.to_character_id)) else {
                continue;
            };
            let rid = fresh("character_relations", &r.id)?;
            self.db.exec(
                &format!(
                    "INSERT INTO character_relations (id, project_id, from_character_id, to_character_id, kind, text, source,
                                                      confidence, evidence, status, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
                ),
                p![rid, project_id.as_str(), from.as_str(), to.as_str(), r.kind.as_str(), r.text.as_str(), r.source.as_str(), r.confidence, json!(r.evidence).to_string(), r.status.as_str()],
            )?;
        }
        for (ci, chapter) in data.chapters.iter().enumerate() {
            let chapter_id = fresh("chapters", &chapter.id)?;
            self.db.exec(
                &format!("INSERT INTO chapters (id, project_id, title, position, created_at, updated_at) VALUES (?, ?, ?, ?, {NOW}, {NOW})"),
                p![chapter_id.as_str(), project_id.as_str(), chapter.title.as_str(), ci as i64],
            )?;
            for (si, scene) in chapter.scenes.iter().enumerate() {
                let scene_id = fresh("scenes", &scene.id)?;
                self.db.exec(
                    &format!(
                        "INSERT INTO scenes (id, chapter_id, title, summary, position, revision, created_at, updated_at)
                         VALUES (?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
                    ),
                    p![scene_id.as_str(), chapter_id.as_str(), scene.title.as_str(), scene.summary.as_str(), si as i64, revision],
                )?;
                for (ei, e) in scene.elements.iter().enumerate() {
                    let element_id = fresh("elements", &e.id)?;
                    let attrs = if e.attrs.is_object() { e.attrs.to_string() } else { "{}".to_string() };
                    let character = e.character_id.as_ref().and_then(|c| character_ids.get(c)).cloned();
                    self.db.exec(
                        &format!(
                            "INSERT INTO elements (id, scene_id, kind, body, speaker, attrs, position, origin, revision,
                                                   character_id, created_at, updated_at)
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
                        ),
                        p![element_id, scene_id.as_str(), e.kind.as_str(), e.body.as_str(), e.speaker.as_str(), attrs, ei as i64, e.origin.as_str(), revision, character],
                    )?;
                }
            }
        }
        for f in &data.files {
            let file_id = fresh("reference_files", &f.id)?;
            self.db.exec(
                &format!(
                    "INSERT INTO reference_files (id, project_id, name, content, pinned, revision, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
                ),
                p![file_id, project_id.as_str(), f.name.as_str(), f.content.as_str(), f.pinned as i64, revision],
            )?;
        }
        // 復元は主体を「復元」として記録する。
        self.db.exec(
            &format!(
                "INSERT INTO history (project_id, scene_id, target_type, target_id, action, actor, before, after, proposal_id, created_at, updated_at)
                 VALUES (?, NULL, 'project', ?, ?, ?, ?, ?, NULL, {NOW}, {NOW})"
            ),
            p![
                project_id.as_str(),
                project_id.as_str(),
                ACTION_RESTORE,
                ACTOR_RESTORE,
                json!({ "snapshot_id": backup.id, "scenes": current_scenes }).to_string(),
                json!({ "snapshot_id": info.id, "note": info.note }).to_string()
            ],
        )?;
        Ok(backup)
    }
}
