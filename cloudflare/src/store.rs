//! Durable Object の SQLite に対するデータ操作。
//!
//! 規則（検証・採番・競合検出・Markdown 保存時の要素の対応づけ）は PC 版の `server/src/store.rs` と同じにする。
//! 検証は `scenote_core::validate` を共有する。書き込みはすべて呼び出し側のトランザクションの中で行う。
//!
//! 第1段階では編集履歴（`history`）を記録しない。表は PC 版と同じスキーマで用意してある。

use serde::Deserialize;
use serde_json::{json, Value};

use scenote_core::markdown::{self, ChapterDoc, ProjectDoc, SceneDoc};
use scenote_core::model::*;
use scenote_core::schema::MIGRATIONS;
use scenote_core::validate::{self, align, content_of, origin_after_human_edit};

use crate::db::{Db, NOW};
use crate::error::{AppError, Result};
use crate::p;

#[derive(Clone)]
pub struct Store {
    pub db: Db,
}

#[derive(Debug, Default)]
pub struct ElementPatch {
    pub kind: Option<String>,
    pub speaker: Option<String>,
    pub body: Option<String>,
    pub attrs: Option<Value>,
    pub character_id: Option<Option<String>>,
}

fn invalid(msg: String) -> AppError {
    AppError::Invalid(msg)
}

/// ID を発行する（Workers の `crypto.randomUUID()`）。
pub fn new_id() -> String {
    let global = js_sys::global();
    js_sys::Reflect::get(&global, &"crypto".into())
        .and_then(|crypto| {
            let f: js_sys::Function = js_sys::Reflect::get(&crypto, &"randomUUID".into())?.into();
            f.call0(&crypto)
        })
        .ok()
        .and_then(|v| v.as_string())
        .expect("crypto.randomUUID is available in Workers")
}

#[derive(Deserialize)]
struct ProjectRow {
    id: String,
    name: String,
    description: String,
    templates: String,
    created_at: String,
    updated_at: String,
}

impl From<ProjectRow> for Project {
    fn from(r: ProjectRow) -> Self {
        Project {
            id: r.id,
            name: r.name,
            description: r.description,
            templates: serde_json::from_str(&r.templates).unwrap_or_default(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct ChapterRow {
    id: String,
    project_id: String,
    title: String,
    position: i64,
    created_at: String,
    updated_at: String,
}

impl From<ChapterRow> for Chapter {
    fn from(r: ChapterRow) -> Self {
        Chapter { id: r.id, project_id: r.project_id, title: r.title, position: r.position, created_at: r.created_at, updated_at: r.updated_at }
    }
}

#[derive(Deserialize)]
struct SceneRow {
    id: String,
    chapter_id: String,
    title: String,
    summary: String,
    position: i64,
    revision: i64,
    created_at: String,
    updated_at: String,
}

impl From<SceneRow> for Scene {
    fn from(r: SceneRow) -> Self {
        Scene {
            id: r.id,
            chapter_id: r.chapter_id,
            title: r.title,
            summary: r.summary,
            position: r.position,
            revision: r.revision,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct ElementRow {
    id: String,
    scene_id: String,
    kind: String,
    body: String,
    speaker: String,
    character_id: Option<String>,
    attrs: String,
    position: i64,
    origin: String,
    revision: i64,
    created_at: String,
    updated_at: String,
}

impl From<ElementRow> for Element {
    fn from(r: ElementRow) -> Self {
        Element {
            id: r.id,
            scene_id: r.scene_id,
            kind: r.kind,
            body: r.body,
            speaker: r.speaker,
            character_id: r.character_id,
            attrs: serde_json::from_str(&r.attrs).unwrap_or_else(|_| json!({})),
            position: r.position,
            origin: r.origin,
            revision: r.revision,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

impl Store {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// スキーマを最新にする。PC 版と同じ SQL を順に適用し、適用済みの版は管理用の表に記録する。
    pub fn migrate(&self) -> Result<()> {
        self.db.exec("CREATE TABLE IF NOT EXISTS _scenote_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL)", p![])?;
        let version: i64 = self
            .db
            .opt::<VersionRow>("SELECT value AS v FROM _scenote_meta WHERE key = 'schema_version'", p![])?
            .map(|r| r.v)
            .unwrap_or(0);
        if version > SCHEMA_VERSION {
            return Err(AppError::Internal(format!("database schema v{version} is newer than this build (v{SCHEMA_VERSION})")));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            self.db.exec(sql, p![])?;
            self.db.exec(
                "INSERT INTO _scenote_meta (key, value) VALUES ('schema_version', ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                p![(i + 1) as i64],
            )?;
        }
        Ok(())
    }

    // ---- プロジェクト -------------------------------------------------

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let rows: Vec<ProjectRow> = self.db.all("SELECT * FROM projects ORDER BY updated_at DESC, name", p![])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        self.db
            .opt::<ProjectRow>("SELECT * FROM projects WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("プロジェクト"))
    }

    /// プロジェクトを作成し、すぐ書き始められるよう最初の章と場面を用意する。
    pub fn create_project(&self, name: &str, description: &str, templates: &[String]) -> Result<Project> {
        let name = validate::clean_title("プロジェクト名", name).map_err(invalid)?;
        let templates = validate::clean_templates(templates).map_err(invalid)?;
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO projects (id, name, description, templates, created_at, updated_at)
                 VALUES (?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), name, description, serde_json::to_string(&templates).unwrap_or_default()],
        )?;
        let chapter_id = self.insert_chapter(&id, "第1章")?;
        self.insert_scene(&chapter_id, "場面1")?;
        self.get_project(&id)
    }

    /// テンプレートの変更は選択状態の更新のみで、既存の章・場面・要素には触れない。
    pub fn update_project(&self, id: &str, name: Option<String>, description: Option<String>, templates: Option<Vec<String>>) -> Result<Project> {
        let current = self.get_project(id)?;
        let name = match name {
            Some(n) => validate::clean_title("プロジェクト名", &n).map_err(invalid)?,
            None => current.name,
        };
        let templates = match templates {
            Some(t) => validate::clean_templates(&t).map_err(invalid)?,
            None => current.templates,
        };
        self.db.exec(
            &format!("UPDATE projects SET name = ?, description = ?, templates = ?, updated_at = {NOW} WHERE id = ?"),
            p![name, description.unwrap_or(current.description), serde_json::to_string(&templates).unwrap_or_default(), id],
        )?;
        self.get_project(id)
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.get_project(id)?;
        // 外部キーの連鎖削除に加え、本文の表は明示的に消す。
        self.db.exec(
            "DELETE FROM elements WHERE scene_id IN
               (SELECT s.id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE c.project_id = ?)",
            p![id],
        )?;
        self.db.exec("DELETE FROM scenes WHERE chapter_id IN (SELECT id FROM chapters WHERE project_id = ?)", p![id])?;
        self.db.exec("DELETE FROM chapters WHERE project_id = ?", p![id])?;
        self.db.exec("DELETE FROM projects WHERE id = ?", p![id])?;
        Ok(())
    }

    pub fn project_tree(&self, id: &str) -> Result<ProjectTree> {
        let project = self.get_project(id)?;
        let mut chapters = Vec::new();
        for chapter in self.list_chapters(id)? {
            let scenes = self.list_scenes(&chapter.id)?;
            chapters.push(ChapterNode { chapter, scenes });
        }
        Ok(ProjectTree { project, chapters })
    }

    // ---- 章 -----------------------------------------------------------

    fn list_chapters(&self, project_id: &str) -> Result<Vec<Chapter>> {
        let rows: Vec<ChapterRow> = self.db.all("SELECT * FROM chapters WHERE project_id = ? ORDER BY position", p![project_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_chapter(&self, id: &str) -> Result<Chapter> {
        self.db
            .opt::<ChapterRow>("SELECT * FROM chapters WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("章"))
    }

    pub fn create_chapter(&self, project_id: &str, title: &str) -> Result<Chapter> {
        self.get_project(project_id)?;
        let title = validate::clean_title("章の題名", title).map_err(invalid)?;
        let id = self.insert_chapter(project_id, &title)?;
        self.touch_project(project_id)?;
        self.get_chapter(&id)
    }

    pub fn rename_chapter(&self, id: &str, title: &str) -> Result<Chapter> {
        let chapter = self.get_chapter(id)?;
        let title = validate::clean_title("章の題名", title).map_err(invalid)?;
        self.db.exec(&format!("UPDATE chapters SET title = ?, updated_at = {NOW} WHERE id = ?"), p![title, id])?;
        self.touch_project(&chapter.project_id)?;
        self.get_chapter(id)
    }

    pub fn delete_chapter(&self, id: &str) -> Result<()> {
        let chapter = self.get_chapter(id)?;
        self.db.exec("DELETE FROM elements WHERE scene_id IN (SELECT id FROM scenes WHERE chapter_id = ?)", p![id])?;
        self.db.exec("DELETE FROM scenes WHERE chapter_id = ?", p![id])?;
        self.db.exec("DELETE FROM chapters WHERE id = ?", p![id])?;
        self.renumber("chapters", "project_id", &chapter.project_id)?;
        self.touch_project(&chapter.project_id)
    }

    pub fn reorder_chapters(&self, project_id: &str, ids: &[String]) -> Result<()> {
        self.get_project(project_id)?;
        self.apply_order("chapters", "project_id", project_id, ids)?;
        self.touch_project(project_id)
    }

    // ---- 場面 ---------------------------------------------------------

    fn list_scenes(&self, chapter_id: &str) -> Result<Vec<Scene>> {
        let rows: Vec<SceneRow> = self.db.all("SELECT * FROM scenes WHERE chapter_id = ? ORDER BY position", p![chapter_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_scene(&self, id: &str) -> Result<Scene> {
        self.db
            .opt::<SceneRow>("SELECT * FROM scenes WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("場面"))
    }

    pub fn scene_detail(&self, id: &str) -> Result<SceneDetail> {
        let scene = self.get_scene(id)?;
        let elements = self.list_elements(id)?;
        Ok(SceneDetail { scene, elements })
    }

    /// 章の末尾、または指定した場面の直後に、（概要つきの）場面を追加する。
    pub fn create_scene(&self, chapter_id: &str, title: &str, summary: &str, after: Option<&str>) -> Result<Scene> {
        let chapter = self.get_chapter(chapter_id)?;
        let title = validate::clean_title("場面の題名", title).map_err(invalid)?;
        let summary = validate::clean_summary(summary).map_err(invalid)?;
        let position = match after {
            Some(a) => {
                let s = self.get_scene(a)?;
                if s.chapter_id != chapter_id {
                    return Err(AppError::Invalid("同じ章の場面を指定してください".into()));
                }
                Some(s.position + 1)
            }
            None => None,
        };
        let id = self.insert_scene(chapter_id, &title)?;
        if let Some(pos) = position {
            self.db.exec(
                "UPDATE scenes SET position = position + 1 WHERE chapter_id = ? AND position >= ? AND id != ?",
                p![chapter_id, pos, id.as_str()],
            )?;
            self.db.exec("UPDATE scenes SET position = ? WHERE id = ?", p![pos, id.as_str()])?;
        }
        self.db.exec("UPDATE scenes SET summary = ? WHERE id = ?", p![summary, id.as_str()])?;
        self.touch_project(&chapter.project_id)?;
        self.get_scene(&id)
    }

    pub fn rename_scene(&self, id: &str, title: &str) -> Result<Scene> {
        self.get_scene(id)?;
        let title = validate::clean_title("場面の題名", title).map_err(invalid)?;
        self.db.exec("UPDATE scenes SET title = ? WHERE id = ?", p![title, id])?;
        self.touch_scene(id)?;
        self.get_scene(id)
    }

    /// 場面の概要（プロット）を更新する。`base_revision` が古ければ競合として拒否する。
    pub fn update_scene_summary(&self, id: &str, base_revision: i64, summary: &str) -> Result<Scene> {
        let scene = self.get_scene(id)?;
        if scene.revision != base_revision {
            return Err(AppError::Conflict { current: scene.revision });
        }
        let summary = validate::clean_summary(summary).map_err(invalid)?;
        if summary == scene.summary {
            return Ok(scene);
        }
        self.db.exec("UPDATE scenes SET summary = ? WHERE id = ?", p![summary, id])?;
        self.touch_scene(id)?;
        self.get_scene(id)
    }

    pub fn delete_scene(&self, id: &str) -> Result<()> {
        let scene = self.get_scene(id)?;
        let chapter = self.get_chapter(&scene.chapter_id)?;
        self.db.exec("DELETE FROM elements WHERE scene_id = ?", p![id])?;
        self.db.exec("DELETE FROM scenes WHERE id = ?", p![id])?;
        self.renumber("scenes", "chapter_id", &scene.chapter_id)?;
        self.touch_project(&chapter.project_id)
    }

    pub fn reorder_scenes(&self, chapter_id: &str, ids: &[String]) -> Result<()> {
        let chapter = self.get_chapter(chapter_id)?;
        self.apply_order("scenes", "chapter_id", chapter_id, ids)?;
        self.touch_project(&chapter.project_id)
    }

    // ---- 要素 ---------------------------------------------------------

    fn list_elements(&self, scene_id: &str) -> Result<Vec<Element>> {
        let rows: Vec<ElementRow> = self.db.all("SELECT * FROM elements WHERE scene_id = ? ORDER BY position", p![scene_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_element(&self, id: &str) -> Result<Element> {
        self.db
            .opt::<ElementRow>("SELECT * FROM elements WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("要素"))
    }

    /// 要素を追加する。`position` 省略時は末尾、指定時はその位置へ挿入する。
    pub fn create_element(&self, scene_id: &str, content: ElementContent, position: Option<i64>, character_id: Option<&str>) -> Result<Element> {
        self.get_scene(scene_id)?;
        if character_id.is_some() {
            // キャラクターの管理は第1段階では未対応。
            return Err(AppError::Unavailable);
        }
        let content = validate::clean_content(content).map_err(invalid)?;
        let count: i64 = self.db.scalar("SELECT COUNT(*) AS v FROM elements WHERE scene_id = ?", p![scene_id])?;
        let position = position.unwrap_or(count).clamp(0, count);
        self.db.exec(
            "UPDATE elements SET position = position + 1 WHERE scene_id = ? AND position >= ?",
            p![scene_id, position],
        )?;
        let id = self.insert_element(scene_id, &content, position, ORIGIN_HUMAN)?;
        self.touch_scene(scene_id)?;
        self.get_element(&id)
    }

    /// 要素を更新する。`base_revision` が現在のリビジョンと異なる場合は競合として拒否する。
    pub fn update_element(&self, id: &str, base_revision: i64, patch: ElementPatch) -> Result<Element> {
        let current = self.get_element(id)?;
        if current.revision != base_revision {
            return Err(AppError::Conflict { current: current.revision });
        }
        if matches!(patch.character_id, Some(Some(_))) {
            return Err(AppError::Unavailable);
        }
        let origin = origin_after_human_edit(&current.origin);
        let content = validate::clean_content(ElementContent {
            kind: patch.kind.unwrap_or(current.kind),
            speaker: patch.speaker.unwrap_or(current.speaker),
            body: patch.body.unwrap_or(current.body),
        })
        .map_err(invalid)?;
        let attrs = match patch.attrs {
            Some(a) => Some(self.clean_attrs(&current.scene_id, &content.kind, a)?),
            None => None,
        };
        self.update_element_content(id, &content, origin)?;
        if let Some(a) = &attrs {
            self.db.exec("UPDATE elements SET attrs = ? WHERE id = ?", p![a.to_string(), id])?;
        }
        if let Some(None) = patch.character_id {
            self.db.exec("UPDATE elements SET character_id = NULL WHERE id = ?", p![id])?;
        }
        self.touch_scene(&current.scene_id)?;
        self.get_element(id)
    }

    fn clean_attrs(&self, scene_id: &str, kind: &str, attrs: Value) -> Result<Value> {
        let project_id: String = self.db.scalar(
            "SELECT c.project_id AS v FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?",
            p![scene_id],
        )?;
        let mut db_error = None;
        let result = validate::clean_attrs(kind, attrs, |target| {
            match self.db.opt::<IdRow>(
                "SELECT s.id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ? AND c.project_id = ?",
                p![target, project_id.as_str()],
            ) {
                Ok(found) => found.is_some(),
                Err(e) => {
                    db_error = Some(e);
                    false
                }
            }
        });
        if let Some(e) = db_error {
            return Err(e);
        }
        result.map_err(invalid)
    }

    pub fn delete_element(&self, id: &str) -> Result<()> {
        let element = self.get_element(id)?;
        self.db.exec("DELETE FROM elements WHERE id = ?", p![id])?;
        self.renumber("elements", "scene_id", &element.scene_id)?;
        self.touch_scene(&element.scene_id)
    }

    pub fn reorder_elements(&self, scene_id: &str, ids: &[String]) -> Result<()> {
        self.get_scene(scene_id)?;
        self.apply_order("elements", "scene_id", scene_id, ids)?;
        self.touch_scene(scene_id)
    }

    // ---- Markdown / JSON ----------------------------------------------

    pub fn scene_markdown(&self, scene_id: &str) -> Result<(Scene, String)> {
        let detail = self.scene_detail(scene_id)?;
        let contents: Vec<ElementContent> = detail.elements.iter().map(content_of).collect();
        Ok((detail.scene, markdown::serialize_elements(&contents)))
    }

    /// 場面の Markdown ソースを保存する。内容が変わっていない要素は ID・リビジョンをそのまま保つ。
    pub fn replace_scene_markdown(&self, scene_id: &str, base_revision: i64, source: &str) -> Result<SceneDetail> {
        let scene = self.get_scene(scene_id)?;
        if scene.revision != base_revision {
            return Err(AppError::Conflict { current: scene.revision });
        }
        let new: Vec<ElementContent> = markdown::parse_elements(source)
            .into_iter()
            .map(validate::clean_content)
            .collect::<std::result::Result<_, _>>()
            .map_err(invalid)?;
        let old = self.list_elements(scene_id)?;
        let old_contents: Vec<ElementContent> = old.iter().map(content_of).collect();
        let plan = align(&old_contents, &new);

        let mut used = std::collections::HashSet::new();
        for (new_index, old_index) in plan.iter().enumerate() {
            let position = new_index as i64;
            match old_index {
                Some(i) => {
                    let existing = &old[*i];
                    used.insert(existing.id.as_str());
                    if old_contents[*i] != new[new_index] {
                        self.update_element_content(&existing.id, &new[new_index], origin_after_human_edit(&existing.origin))?;
                    }
                    self.db.exec("UPDATE elements SET position = ? WHERE id = ?", p![position, existing.id.as_str()])?;
                }
                None => {
                    self.insert_element(scene_id, &new[new_index], position, ORIGIN_HUMAN)?;
                }
            }
        }
        for existing in old.iter().filter(|e| !used.contains(e.id.as_str())) {
            self.db.exec("DELETE FROM elements WHERE id = ?", p![existing.id.as_str()])?;
        }
        self.touch_scene(scene_id)?;
        self.scene_detail(scene_id)
    }

    fn project_doc(&self, project_id: &str) -> Result<ProjectDoc> {
        let tree = self.project_tree(project_id)?;
        let mut chapters = Vec::new();
        for node in &tree.chapters {
            let mut scenes = Vec::new();
            for scene in &node.scenes {
                scenes.push(SceneDoc {
                    title: scene.title.clone(),
                    elements: self.list_elements(&scene.id)?.iter().map(content_of).collect(),
                });
            }
            chapters.push(ChapterDoc { title: node.chapter.title.clone(), scenes });
        }
        Ok(ProjectDoc { title: tree.project.name, chapters })
    }

    pub fn export_markdown(&self, project_id: &str) -> Result<String> {
        Ok(markdown::serialize_project(&self.project_doc(project_id)?))
    }

    /// ID・順序・由来情報を含む構造を JSON で出力する（PC 版と同じ形式）。
    pub fn export_json(&self, project_id: &str) -> Result<Value> {
        let tree = self.project_tree(project_id)?;
        let mut chapters = Vec::new();
        for node in &tree.chapters {
            let mut scenes = Vec::new();
            for scene in &node.scenes {
                scenes.push(serde_json::to_value(self.scene_detail(&scene.id)?).map_err(|e| AppError::Internal(e.to_string()))?);
            }
            let mut chapter = serde_json::to_value(&node.chapter).map_err(|e| AppError::Internal(e.to_string()))?;
            chapter["scenes"] = Value::Array(scenes);
            chapters.push(chapter);
        }
        let exported_at: String = self.db.scalar(&format!("SELECT {NOW} AS v"), p![])?;
        Ok(json!({
            "format": "scenote-project",
            "schema_version": SCHEMA_VERSION,
            "exported_at": exported_at,
            "project": tree.project,
            "chapters": chapters,
            "reference_files": [],
            "characters": [],
            "character_relations": [],
        }))
    }

    // ---- 内部 ---------------------------------------------------------

    fn next_position(&self, table: &str, parent_col: &str, parent_id: &str) -> Result<i64> {
        self.db.scalar(&format!("SELECT COUNT(*) AS v FROM {table} WHERE {parent_col} = ?"), p![parent_id])
    }

    fn insert_chapter(&self, project_id: &str, title: &str) -> Result<String> {
        let id = new_id();
        let position = self.next_position("chapters", "project_id", project_id)?;
        self.db.exec(
            &format!("INSERT INTO chapters (id, project_id, title, position, created_at, updated_at) VALUES (?, ?, ?, ?, {NOW}, {NOW})"),
            p![id.as_str(), project_id, title, position],
        )?;
        Ok(id)
    }

    fn insert_scene(&self, chapter_id: &str, title: &str) -> Result<String> {
        let id = new_id();
        let position = self.next_position("scenes", "chapter_id", chapter_id)?;
        self.db.exec(
            &format!("INSERT INTO scenes (id, chapter_id, title, position, created_at, updated_at) VALUES (?, ?, ?, ?, {NOW}, {NOW})"),
            p![id.as_str(), chapter_id, title, position],
        )?;
        Ok(id)
    }

    fn insert_element(&self, scene_id: &str, content: &ElementContent, position: i64, origin: &str) -> Result<String> {
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO elements (id, scene_id, kind, body, speaker, position, origin, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), scene_id, content.kind.as_str(), content.body.as_str(), content.speaker.as_str(), position, origin],
        )?;
        Ok(id)
    }

    fn update_element_content(&self, id: &str, content: &ElementContent, origin: &str) -> Result<()> {
        self.db.exec(
            &format!(
                "UPDATE elements SET kind = ?, speaker = ?, body = ?, origin = ?, revision = revision + 1, updated_at = {NOW}
                 WHERE id = ?"
            ),
            p![content.kind.as_str(), content.speaker.as_str(), content.body.as_str(), origin, id],
        )
    }

    fn touch_project(&self, project_id: &str) -> Result<()> {
        self.db.exec(&format!("UPDATE projects SET updated_at = {NOW} WHERE id = ?"), p![project_id])
    }

    /// 場面のリビジョンを進め、所属プロジェクトの更新日時も更新する。
    fn touch_scene(&self, scene_id: &str) -> Result<()> {
        self.db.exec(&format!("UPDATE scenes SET revision = revision + 1, updated_at = {NOW} WHERE id = ?"), p![scene_id])?;
        self.db.exec(
            &format!(
                "UPDATE projects SET updated_at = {NOW} WHERE id =
                   (SELECT c.project_id FROM chapters c JOIN scenes s ON s.chapter_id = c.id WHERE s.id = ?)"
            ),
            p![scene_id],
        )
    }

    fn child_ids(&self, table: &str, parent_col: &str, parent_id: &str) -> Result<Vec<String>> {
        let rows: Vec<IdRow> = self.db.all(&format!("SELECT id FROM {table} WHERE {parent_col} = ? ORDER BY position"), p![parent_id])?;
        Ok(rows.into_iter().map(|r| r.id).collect())
    }

    fn write_positions(&self, table: &str, ids: &[String]) -> Result<()> {
        for (i, id) in ids.iter().enumerate() {
            self.db.exec(&format!("UPDATE {table} SET position = ? WHERE id = ?"), p![i as i64, id.as_str()])?;
        }
        Ok(())
    }

    /// 削除後に順序を 0 からの連番へ詰め直す。
    fn renumber(&self, table: &str, parent_col: &str, parent_id: &str) -> Result<()> {
        let ids = self.child_ids(table, parent_col, parent_id)?;
        self.write_positions(table, &ids)
    }

    /// 指定された順序を適用する。`ids` は現在の子の並べ替えと完全に一致していなければならない。
    fn apply_order(&self, table: &str, parent_col: &str, parent_id: &str, ids: &[String]) -> Result<()> {
        let existing = self.child_ids(table, parent_col, parent_id)?;
        if !validate::is_permutation(&existing, ids) {
            return Err(AppError::Invalid(validate::ORDER_MISMATCH.into()));
        }
        self.write_positions(table, ids)
    }
}

#[derive(Deserialize)]
struct VersionRow {
    v: i64,
}
