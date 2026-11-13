//! SQLite による永続化層。
//!
//! ID の発行、順序の採番、参照整合性、競合検出はすべてここで行う（LLM には任せない）。

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::markdown::{self, ChapterDoc, ProjectDoc, SceneDoc};
use crate::model::*;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
const MAX_TITLE_CHARS: usize = 200;

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Default)]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub templates: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct ElementPatch {
    pub kind: Option<String>,
    pub speaker: Option<String>,
    pub body: Option<String>,
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn clean_title(what: &str, title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::Invalid(format!("{what}を入力してください")));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(AppError::Invalid(format!(
            "{what}は{MAX_TITLE_CHARS}文字以内で入力してください"
        )));
    }
    if title.chars().any(|c| c.is_control()) {
        return Err(AppError::Invalid(format!("{what}に改行などの制御文字は使えません")));
    }
    Ok(title.to_string())
}

fn clean_templates(templates: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in templates {
        if !TEMPLATES.contains(&t.as_str()) {
            return Err(AppError::Invalid(format!("未知のテンプレートです: {t}")));
        }
        if !out.contains(t) {
            out.push(t.clone());
        }
    }
    Ok(out)
}

fn clean_content(content: ElementContent) -> Result<ElementContent> {
    if !is_valid_kind(&content.kind) {
        return Err(AppError::Invalid(
            "要素の種類は英字で始まる英数字で指定してください".into(),
        ));
    }
    if !is_valid_speaker(&content.speaker) {
        return Err(AppError::Invalid(
            "キャラクター名に [ ] : * と改行は使えません（100文字以内）".into(),
        ));
    }
    Ok(ElementContent {
        body: content.body.replace("\r\n", "\n").replace('\r', "\n"),
        ..content
    })
}

fn project_from_row(row: &Row) -> rusqlite::Result<Project> {
    let templates: String = row.get("templates")?;
    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        templates: serde_json::from_str(&templates).unwrap_or_default(),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn chapter_from_row(row: &Row) -> rusqlite::Result<Chapter> {
    Ok(Chapter {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        title: row.get("title")?,
        position: row.get("position")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn scene_from_row(row: &Row) -> rusqlite::Result<Scene> {
    Ok(Scene {
        id: row.get("id")?,
        chapter_id: row.get("chapter_id")?,
        title: row.get("title")?,
        position: row.get("position")?,
        revision: row.get("revision")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn element_from_row(row: &Row) -> rusqlite::Result<Element> {
    let attrs: String = row.get("attrs")?;
    Ok(Element {
        id: row.get("id")?,
        scene_id: row.get("scene_id")?,
        kind: row.get("kind")?,
        body: row.get("body")?,
        speaker: row.get("speaker")?,
        attrs: serde_json::from_str(&attrs).unwrap_or_else(|_| json!({})),
        position: row.get("position")?,
        origin: row.get("origin")?,
        revision: row.get("revision")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| AppError::Internal(format!("create data dir: {e}")))?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// `PRAGMA user_version` に基づいてスキーマを段階的に更新する。
    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(AppError::Internal(format!(
                "database schema v{version} is newer than this build (v{SCHEMA_VERSION})"
            )));
        }
        if version < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(include_str!("schema_v1.sql"))?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    // ---- プロジェクト -------------------------------------------------

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM projects ORDER BY updated_at DESC, name")?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        self.conn
            .query_row("SELECT * FROM projects WHERE id = ?1", [id], project_from_row)
            .optional()?
            .ok_or(AppError::NotFound("プロジェクト"))
    }

    /// プロジェクトを作成し、すぐ書き始められるよう最初の章と場面を用意する。
    pub fn create_project(
        &mut self,
        name: &str,
        description: &str,
        templates: &[String],
    ) -> Result<Project> {
        let name = clean_title("プロジェクト名", name)?;
        let templates = clean_templates(templates)?;
        let id = new_id();
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO projects (id, name, description, templates, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
            ),
            params![id, name, description, serde_json::to_string(&templates)?],
        )?;
        let chapter_id = insert_chapter(&tx, &id, "第1章")?;
        insert_scene(&tx, &chapter_id, "場面1")?;
        tx.commit()?;
        self.get_project(&id)
    }

    /// テンプレートの変更は選択状態の更新のみで、既存の章・場面・要素には触れない。
    pub fn update_project(&mut self, id: &str, patch: ProjectPatch) -> Result<Project> {
        let current = self.get_project(id)?;
        let name = match patch.name {
            Some(n) => clean_title("プロジェクト名", &n)?,
            None => current.name,
        };
        let templates = match patch.templates {
            Some(t) => clean_templates(&t)?,
            None => current.templates,
        };
        self.conn.execute(
            &format!(
                "UPDATE projects SET name = ?2, description = ?3, templates = ?4, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![
                id,
                name,
                patch.description.unwrap_or(current.description),
                serde_json::to_string(&templates)?
            ],
        )?;
        self.get_project(id)
    }

    pub fn delete_project(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM projects WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("プロジェクト"));
        }
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
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM chapters WHERE project_id = ?1 ORDER BY position")?;
        let rows = stmt.query_map([project_id], chapter_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_chapter(&self, id: &str) -> Result<Chapter> {
        self.conn
            .query_row("SELECT * FROM chapters WHERE id = ?1", [id], chapter_from_row)
            .optional()?
            .ok_or(AppError::NotFound("章"))
    }

    pub fn create_chapter(&mut self, project_id: &str, title: &str) -> Result<Chapter> {
        self.get_project(project_id)?;
        let title = clean_title("章の題名", title)?;
        let tx = self.conn.transaction()?;
        let id = insert_chapter(&tx, project_id, &title)?;
        touch_project(&tx, project_id)?;
        tx.commit()?;
        self.get_chapter(&id)
    }

    pub fn rename_chapter(&mut self, id: &str, title: &str) -> Result<Chapter> {
        let chapter = self.get_chapter(id)?;
        let title = clean_title("章の題名", title)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("UPDATE chapters SET title = ?2, updated_at = {NOW} WHERE id = ?1"),
            params![id, title],
        )?;
        touch_project(&tx, &chapter.project_id)?;
        tx.commit()?;
        self.get_chapter(id)
    }

    pub fn delete_chapter(&mut self, id: &str) -> Result<()> {
        let chapter = self.get_chapter(id)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM chapters WHERE id = ?1", [id])?;
        renumber(&tx, "chapters", "project_id", &chapter.project_id)?;
        touch_project(&tx, &chapter.project_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_chapters(&mut self, project_id: &str, ids: &[String]) -> Result<()> {
        self.get_project(project_id)?;
        let tx = self.conn.transaction()?;
        apply_order(&tx, "chapters", "project_id", project_id, ids)?;
        touch_project(&tx, project_id)?;
        tx.commit()?;
        Ok(())
    }

    // ---- 場面 ---------------------------------------------------------

    fn list_scenes(&self, chapter_id: &str) -> Result<Vec<Scene>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM scenes WHERE chapter_id = ?1 ORDER BY position")?;
        let rows = stmt.query_map([chapter_id], scene_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_scene(&self, id: &str) -> Result<Scene> {
        self.conn
            .query_row("SELECT * FROM scenes WHERE id = ?1", [id], scene_from_row)
            .optional()?
            .ok_or(AppError::NotFound("場面"))
    }

    pub fn scene_detail(&self, id: &str) -> Result<SceneDetail> {
        let scene = self.get_scene(id)?;
        let elements = self.list_elements(id)?;
        Ok(SceneDetail { scene, elements })
    }

    pub fn create_scene(&mut self, chapter_id: &str, title: &str) -> Result<Scene> {
        let chapter = self.get_chapter(chapter_id)?;
        let title = clean_title("場面の題名", title)?;
        let tx = self.conn.transaction()?;
        let id = insert_scene(&tx, chapter_id, &title)?;
        touch_project(&tx, &chapter.project_id)?;
        tx.commit()?;
        self.get_scene(&id)
    }

    pub fn rename_scene(&mut self, id: &str, title: &str) -> Result<Scene> {
        self.get_scene(id)?;
        let title = clean_title("場面の題名", title)?;
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE scenes SET title = ?2 WHERE id = ?1", params![id, title])?;
        touch_scene(&tx, id)?;
        tx.commit()?;
        self.get_scene(id)
    }

    pub fn delete_scene(&mut self, id: &str) -> Result<()> {
        let scene = self.get_scene(id)?;
        let chapter = self.get_chapter(&scene.chapter_id)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM scenes WHERE id = ?1", [id])?;
        renumber(&tx, "scenes", "chapter_id", &scene.chapter_id)?;
        touch_project(&tx, &chapter.project_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_scenes(&mut self, chapter_id: &str, ids: &[String]) -> Result<()> {
        let chapter = self.get_chapter(chapter_id)?;
        let tx = self.conn.transaction()?;
        apply_order(&tx, "scenes", "chapter_id", chapter_id, ids)?;
        touch_project(&tx, &chapter.project_id)?;
        tx.commit()?;
        Ok(())
    }

    // ---- 要素 ---------------------------------------------------------

    fn list_elements(&self, scene_id: &str) -> Result<Vec<Element>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM elements WHERE scene_id = ?1 ORDER BY position")?;
        let rows = stmt.query_map([scene_id], element_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_element(&self, id: &str) -> Result<Element> {
        self.conn
            .query_row("SELECT * FROM elements WHERE id = ?1", [id], element_from_row)
            .optional()?
            .ok_or(AppError::NotFound("要素"))
    }

    /// 要素を追加する。`position` 省略時は末尾、指定時はその位置へ挿入する。
    pub fn create_element(
        &mut self,
        scene_id: &str,
        content: ElementContent,
        position: Option<i64>,
    ) -> Result<Element> {
        self.get_scene(scene_id)?;
        let content = clean_content(content)?;
        let tx = self.conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM elements WHERE scene_id = ?1",
            [scene_id],
            |r| r.get(0),
        )?;
        let position = position.unwrap_or(count).clamp(0, count);
        tx.execute(
            "UPDATE elements SET position = position + 1 WHERE scene_id = ?1 AND position >= ?2",
            params![scene_id, position],
        )?;
        let id = insert_element(&tx, scene_id, &content, position)?;
        touch_scene(&tx, scene_id)?;
        tx.commit()?;
        self.get_element(&id)
    }

    /// 要素を更新する。`base_revision` が現在のリビジョンと異なる場合は競合として拒否する。
    pub fn update_element(
        &mut self,
        id: &str,
        base_revision: i64,
        patch: ElementPatch,
    ) -> Result<Element> {
        let current = self.get_element(id)?;
        if current.revision != base_revision {
            return Err(AppError::Conflict {
                current: current.revision,
            });
        }
        let content = clean_content(ElementContent {
            kind: patch.kind.unwrap_or(current.kind),
            speaker: patch.speaker.unwrap_or(current.speaker),
            body: patch.body.unwrap_or(current.body),
        })?;
        let tx = self.conn.transaction()?;
        update_element_content(&tx, id, &content)?;
        touch_scene(&tx, &current.scene_id)?;
        tx.commit()?;
        self.get_element(id)
    }

    pub fn delete_element(&mut self, id: &str) -> Result<()> {
        let element = self.get_element(id)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM elements WHERE id = ?1", [id])?;
        renumber(&tx, "elements", "scene_id", &element.scene_id)?;
        touch_scene(&tx, &element.scene_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_elements(&mut self, scene_id: &str, ids: &[String]) -> Result<()> {
        self.get_scene(scene_id)?;
        let tx = self.conn.transaction()?;
        apply_order(&tx, "elements", "scene_id", scene_id, ids)?;
        touch_scene(&tx, scene_id)?;
        tx.commit()?;
        Ok(())
    }

    // ---- Markdown / JSON ----------------------------------------------

    pub fn scene_markdown(&self, scene_id: &str) -> Result<(Scene, String)> {
        let detail = self.scene_detail(scene_id)?;
        let contents: Vec<ElementContent> = detail.elements.iter().map(content_of).collect();
        Ok((detail.scene, markdown::serialize_elements(&contents)))
    }

    /// 場面の Markdown ソースを保存する。
    ///
    /// 既存の要素と突き合わせ、内容が変わっていない要素は ID・リビジョンをそのまま保つ。
    pub fn replace_scene_markdown(
        &mut self,
        scene_id: &str,
        base_revision: i64,
        source: &str,
    ) -> Result<SceneDetail> {
        let scene = self.get_scene(scene_id)?;
        if scene.revision != base_revision {
            return Err(AppError::Conflict {
                current: scene.revision,
            });
        }
        let new: Vec<ElementContent> = markdown::parse_elements(source)
            .into_iter()
            .map(clean_content)
            .collect::<Result<_>>()?;
        let old = self.list_elements(scene_id)?;
        let old_contents: Vec<ElementContent> = old.iter().map(content_of).collect();
        let plan = align(&old_contents, &new);

        let tx = self.conn.transaction()?;
        let mut used: HashSet<&str> = HashSet::new();
        for (new_index, old_index) in plan.iter().enumerate() {
            let position = new_index as i64;
            match old_index {
                Some(i) => {
                    let existing = &old[*i];
                    used.insert(existing.id.as_str());
                    if old_contents[*i] != new[new_index] {
                        update_element_content(&tx, &existing.id, &new[new_index])?;
                    }
                    tx.execute(
                        "UPDATE elements SET position = ?2 WHERE id = ?1",
                        params![existing.id, position],
                    )?;
                }
                None => {
                    insert_element(&tx, scene_id, &new[new_index], position)?;
                }
            }
        }
        for existing in old.iter().filter(|e| !used.contains(e.id.as_str())) {
            tx.execute("DELETE FROM elements WHERE id = ?1", [&existing.id])?;
        }
        touch_scene(&tx, scene_id)?;
        tx.commit()?;
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
            chapters.push(ChapterDoc {
                title: node.chapter.title.clone(),
                scenes,
            });
        }
        Ok(ProjectDoc {
            title: tree.project.name,
            chapters,
        })
    }

    pub fn export_markdown(&self, project_id: &str) -> Result<String> {
        Ok(markdown::serialize_project(&self.project_doc(project_id)?))
    }

    /// ID・順序・由来情報を含む完全な構造を JSON で出力する。
    pub fn export_json(&self, project_id: &str) -> Result<Value> {
        let tree = self.project_tree(project_id)?;
        let mut chapters = Vec::new();
        for node in &tree.chapters {
            let mut scenes = Vec::new();
            for scene in &node.scenes {
                scenes.push(serde_json::to_value(self.scene_detail(&scene.id)?)?);
            }
            let mut chapter = serde_json::to_value(&node.chapter)?;
            chapter["scenes"] = Value::Array(scenes);
            chapters.push(chapter);
        }
        let exported_at: String = self
            .conn
            .query_row(&format!("SELECT {NOW}"), [], |r| r.get(0))?;
        Ok(json!({
            "format": "scenote-project",
            "schema_version": SCHEMA_VERSION,
            "exported_at": exported_at,
            "project": tree.project,
            "chapters": chapters,
        }))
    }
}

fn content_of(e: &Element) -> ElementContent {
    ElementContent {
        kind: e.kind.clone(),
        speaker: e.speaker.clone(),
        body: e.body.clone(),
    }
}

fn next_position(tx: &Transaction, table: &str, parent_col: &str, parent_id: &str) -> Result<i64> {
    Ok(tx.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {parent_col} = ?1"),
        [parent_id],
        |r| r.get(0),
    )?)
}

fn insert_chapter(tx: &Transaction, project_id: &str, title: &str) -> Result<String> {
    let id = new_id();
    let position = next_position(tx, "chapters", "project_id", project_id)?;
    tx.execute(
        &format!(
            "INSERT INTO chapters (id, project_id, title, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
        ),
        params![id, project_id, title, position],
    )?;
    Ok(id)
}

fn insert_scene(tx: &Transaction, chapter_id: &str, title: &str) -> Result<String> {
    let id = new_id();
    let position = next_position(tx, "scenes", "chapter_id", chapter_id)?;
    tx.execute(
        &format!(
            "INSERT INTO scenes (id, chapter_id, title, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
        ),
        params![id, chapter_id, title, position],
    )?;
    Ok(id)
}

fn insert_element(
    tx: &Transaction,
    scene_id: &str,
    content: &ElementContent,
    position: i64,
) -> Result<String> {
    let id = new_id();
    tx.execute(
        &format!(
            "INSERT INTO elements (id, scene_id, kind, body, speaker, position, origin, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {NOW}, {NOW})"
        ),
        params![id, scene_id, content.kind, content.body, content.speaker, position, ORIGIN_HUMAN],
    )?;
    Ok(id)
}

fn update_element_content(tx: &Transaction, id: &str, content: &ElementContent) -> Result<()> {
    tx.execute(
        &format!(
            "UPDATE elements SET kind = ?2, speaker = ?3, body = ?4,
                    revision = revision + 1, updated_at = {NOW}
             WHERE id = ?1"
        ),
        params![id, content.kind, content.speaker, content.body],
    )?;
    Ok(())
}

fn touch_project(tx: &Transaction, project_id: &str) -> Result<()> {
    tx.execute(
        &format!("UPDATE projects SET updated_at = {NOW} WHERE id = ?1"),
        [project_id],
    )?;
    Ok(())
}

/// 場面のリビジョンを進め、所属プロジェクトの更新日時も更新する。
fn touch_scene(tx: &Transaction, scene_id: &str) -> Result<()> {
    tx.execute(
        &format!("UPDATE scenes SET revision = revision + 1, updated_at = {NOW} WHERE id = ?1"),
        [scene_id],
    )?;
    tx.execute(
        &format!(
            "UPDATE projects SET updated_at = {NOW} WHERE id =
               (SELECT c.project_id FROM chapters c JOIN scenes s ON s.chapter_id = c.id WHERE s.id = ?1)"
        ),
        [scene_id],
    )?;
    Ok(())
}

fn child_ids(tx: &Transaction, table: &str, parent_col: &str, parent_id: &str) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT id FROM {table} WHERE {parent_col} = ?1 ORDER BY position"
    ))?;
    let rows = stmt.query_map([parent_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn write_positions(tx: &Transaction, table: &str, ids: &[String]) -> Result<()> {
    let mut stmt = tx.prepare(&format!("UPDATE {table} SET position = ?2 WHERE id = ?1"))?;
    for (i, id) in ids.iter().enumerate() {
        stmt.execute(params![id, i as i64])?;
    }
    Ok(())
}

/// 削除後に順序を 0 からの連番へ詰め直す。
fn renumber(tx: &Transaction, table: &str, parent_col: &str, parent_id: &str) -> Result<()> {
    let ids = child_ids(tx, table, parent_col, parent_id)?;
    write_positions(tx, table, &ids)
}

/// 指定された順序を適用する。`ids` は現在の子要素の並べ替えと完全に一致していなければならない。
fn apply_order(
    tx: &Transaction,
    table: &str,
    parent_col: &str,
    parent_id: &str,
    ids: &[String],
) -> Result<()> {
    let existing = child_ids(tx, table, parent_col, parent_id)?;
    let requested: HashSet<&String> = ids.iter().collect();
    if ids.len() != existing.len()
        || requested.len() != ids.len()
        || !existing.iter().all(|id| requested.contains(id))
    {
        return Err(AppError::Invalid(
            "並べ替えの対象が最新の状態と一致しません。再読み込みしてからやり直してください".into(),
        ));
    }
    write_positions(tx, table, ids)
}

/// 新しい要素列の各要素を、既存要素のどれに対応させるか決める。
///
/// 内容が完全に一致する要素を最長共通部分列で対応づけ、その間に挟まれた区間では
/// 先頭から順に「同じ要素の編集」とみなして対応づける。対応のない新要素は `None`（新規作成）。
fn align(old: &[ElementContent], new: &[ElementContent]) -> Vec<Option<usize>> {
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut plan = vec![None; m];
    let (mut i, mut j) = (0, 0);
    let (mut gap_i, mut gap_j) = (0, 0);
    let pair_gap = |plan: &mut Vec<Option<usize>>, from_i: usize, to_i: usize, from_j: usize, to_j: usize| {
        for (oi, nj) in (from_i..to_i).zip(from_j..to_j) {
            plan[nj] = Some(oi);
        }
    };
    while i < n && j < m {
        if old[i] == new[j] {
            pair_gap(&mut plan, gap_i, i, gap_j, j);
            plan[j] = Some(i);
            i += 1;
            j += 1;
            gap_i = i;
            gap_j = j;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pair_gap(&mut plan, gap_i, n, gap_j, m);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(kind: &str, speaker: &str, body: &str) -> ElementContent {
        ElementContent {
            kind: kind.into(),
            speaker: speaker.into(),
            body: body.into(),
        }
    }

    fn narration(body: &str) -> ElementContent {
        content(KIND_NARRATION, "", body)
    }

    /// プロジェクトを作り、最初の場面の ID を返す。
    fn setup() -> (Store, String, String) {
        let mut store = Store::open_in_memory().unwrap();
        let project = store.create_project("試作", "", &[]).unwrap();
        let tree = store.project_tree(&project.id).unwrap();
        let scene_id = tree.chapters[0].scenes[0].id.clone();
        (store, project.id, scene_id)
    }

    fn bodies(store: &Store, scene_id: &str) -> Vec<String> {
        store
            .scene_detail(scene_id)
            .unwrap()
            .elements
            .into_iter()
            .map(|e| e.body)
            .collect()
    }

    #[test]
    fn creates_project_with_initial_structure() {
        let (store, project_id, _) = setup();
        let tree = store.project_tree(&project_id).unwrap();
        assert_eq!(tree.chapters.len(), 1);
        assert_eq!(tree.chapters[0].scenes.len(), 1);
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn validates_input() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(matches!(store.create_project("  ", "", &[]), Err(AppError::Invalid(_))));
        assert!(matches!(
            store.create_project("a", "", &["unknown".into()]),
            Err(AppError::Invalid(_))
        ));
        let (mut store, _, scene_id) = setup();
        assert!(matches!(
            store.create_element(&scene_id, content("bad kind", "", ""), None),
            Err(AppError::Invalid(_))
        ));
        assert!(matches!(
            store.create_element(&scene_id, content("Dialogue", "a]b", ""), None),
            Err(AppError::Invalid(_))
        ));
        assert!(matches!(
            store.create_element("missing", narration(""), None),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn element_crud_and_ordering() {
        let (mut store, _, scene_id) = setup();
        let a = store.create_element(&scene_id, narration("a"), None).unwrap();
        let b = store.create_element(&scene_id, narration("b"), None).unwrap();
        let c = store.create_element(&scene_id, narration("c"), Some(1)).unwrap();
        assert_eq!(bodies(&store, &scene_id), ["a", "c", "b"]);

        store
            .reorder_elements(&scene_id, &[b.id.clone(), a.id.clone(), c.id.clone()])
            .unwrap();
        assert_eq!(bodies(&store, &scene_id), ["b", "a", "c"]);

        store.delete_element(&a.id).unwrap();
        let detail = store.scene_detail(&scene_id).unwrap();
        let positions: Vec<i64> = detail.elements.iter().map(|e| e.position).collect();
        assert_eq!(positions, [0, 1]);
        assert_eq!(detail.elements[0].id, b.id);
        assert_eq!(detail.elements[1].id, c.id);

        // 不完全・重複・他場面の ID を含む並べ替えは拒否し、順序を変えない。
        for bad in [
            vec![b.id.clone()],
            vec![b.id.clone(), b.id.clone()],
            vec![b.id.clone(), a.id.clone()],
        ] {
            assert!(matches!(
                store.reorder_elements(&scene_id, &bad),
                Err(AppError::Invalid(_))
            ));
        }
        assert_eq!(bodies(&store, &scene_id), ["b", "c"]);
    }

    #[test]
    fn update_detects_conflicts() {
        let (mut store, _, scene_id) = setup();
        let e = store.create_element(&scene_id, narration("初稿"), None).unwrap();
        let patch = |body: &str| ElementPatch {
            body: Some(body.into()),
            ..Default::default()
        };
        let updated = store.update_element(&e.id, e.revision, patch("PCで編集")).unwrap();
        assert_eq!(updated.revision, e.revision + 1);
        assert_eq!(updated.kind, KIND_NARRATION);

        // 古いリビジョンに基づく更新（別端末からの同時編集）は上書きしない。
        let err = store.update_element(&e.id, e.revision, patch("iPadで編集")).unwrap_err();
        assert!(matches!(err, AppError::Conflict { current } if current == updated.revision));
        assert_eq!(store.get_element(&e.id).unwrap().body, "PCで編集");
    }

    #[test]
    fn chapters_and_scenes_crud() {
        let (mut store, project_id, scene_id) = setup();
        let first = store.project_tree(&project_id).unwrap().chapters[0].chapter.clone();
        let second = store.create_chapter(&project_id, "第2章").unwrap();
        let s2 = store.create_scene(&first.id, "場面2").unwrap();
        assert_eq!(s2.position, 1);
        store.reorder_scenes(&first.id, &[s2.id.clone(), scene_id.clone()]).unwrap();
        store.reorder_chapters(&project_id, &[second.id.clone(), first.id.clone()]).unwrap();
        store.rename_chapter(&second.id, " 序章 ").unwrap();
        store.rename_scene(&s2.id, "再会").unwrap();

        let tree = store.project_tree(&project_id).unwrap();
        assert_eq!(tree.chapters[0].chapter.title, "序章");
        assert_eq!(tree.chapters[1].scenes[0].title, "再会");
        assert_eq!(tree.chapters[1].scenes[1].id, scene_id);

        // 章を削除すると配下の場面・要素も消え、他の章には影響しない。
        store.create_element(&scene_id, narration("x"), None).unwrap();
        store.delete_chapter(&first.id).unwrap();
        assert!(matches!(store.get_scene(&scene_id), Err(AppError::NotFound(_))));
        let tree = store.project_tree(&project_id).unwrap();
        assert_eq!(tree.chapters.len(), 1);
        assert_eq!(tree.chapters[0].chapter.position, 0);
        let orphans: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM elements", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn template_change_keeps_existing_data() {
        let (mut store, project_id, scene_id) = setup();
        store.create_element(&scene_id, narration("本文"), None).unwrap();
        let before = store.export_markdown(&project_id).unwrap();
        let updated = store
            .update_project(
                &project_id,
                ProjectPatch {
                    templates: Some(vec!["choice_adv".into(), "status_adv".into(), "choice_adv".into()]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.templates, ["choice_adv", "status_adv"]);
        assert_eq!(store.export_markdown(&project_id).unwrap(), before);
    }

    #[test]
    fn markdown_source_edit_keeps_ids_of_unchanged_elements() {
        let (mut store, _, scene_id) = setup();
        let a = store.create_element(&scene_id, narration("冒頭"), None).unwrap();
        let b = store.create_element(&scene_id, content(KIND_DIALOGUE, "アリス", "やあ"), None).unwrap();
        let c = store.create_element(&scene_id, narration("結び"), None).unwrap();

        let (scene, md) = store.scene_markdown(&scene_id).unwrap();
        assert_eq!(md, "冒頭\n\n**アリス**: やあ\n\n結び\n");

        // 台詞を書き換え、その後ろに演出を挿入し、結びはそのまま。
        let edited = "冒頭\n\n**アリス**: こんにちは\n\n[演出] 暗転\n\n結び\n";
        let detail = store.replace_scene_markdown(&scene_id, scene.revision, edited).unwrap();
        let ids: Vec<&str> = detail.elements.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), 4);
        assert_eq!(ids[0], a.id);
        assert_eq!(ids[1], b.id);
        assert_eq!(ids[3], c.id);
        assert_eq!(detail.elements[0].revision, a.revision);
        assert_eq!(detail.elements[1].revision, b.revision + 1);
        assert_eq!(detail.elements[2].kind, KIND_DIRECTION);
        let positions: Vec<i64> = detail.elements.iter().map(|e| e.position).collect();
        assert_eq!(positions, [0, 1, 2, 3]);
        assert_eq!(store.scene_markdown(&scene_id).unwrap().1, edited);

        // 古いリビジョンでの保存は拒否する。
        assert!(matches!(
            store.replace_scene_markdown(&scene_id, scene.revision, "上書き"),
            Err(AppError::Conflict { .. })
        ));

        // 削除と全消去。
        let rev = store.get_scene(&scene_id).unwrap().revision;
        let detail = store.replace_scene_markdown(&scene_id, rev, "結び\n").unwrap();
        assert_eq!(detail.elements.len(), 1);
        assert_eq!(detail.elements[0].id, c.id);
        let detail = store.replace_scene_markdown(&scene_id, detail.scene.revision, "").unwrap();
        assert!(detail.elements.is_empty());
    }

    #[test]
    fn exports_round_trip_and_include_structure() {
        let (mut store, project_id, scene_id) = setup();
        store.create_element(&scene_id, narration("一行目\n\n空行のあと"), None).unwrap();
        store.create_element(&scene_id, content(KIND_CHOICE, "", "- 進む\n- 戻る"), None).unwrap();

        let md = store.export_markdown(&project_id).unwrap();
        let doc = markdown::parse_project(&md);
        assert_eq!(doc, store.project_doc(&project_id).unwrap());
        assert_eq!(doc.title, "試作");

        let json = store.export_json(&project_id).unwrap();
        assert_eq!(json["format"], "scenote-project");
        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        let elements = json["chapters"][0]["scenes"][0]["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[1]["kind"], "Choice");
        assert_eq!(elements[1]["origin"], "human");
        assert!(elements[1]["id"].as_str().unwrap().len() > 10);
    }

    #[test]
    fn data_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("scenote-test-{}", Uuid::new_v4()));
        let path = dir.join("nested").join("scenote.db");
        let (project_id, scene_id) = {
            let mut store = Store::open(&path).unwrap();
            let project = store.create_project("永続化", "説明", &["linear_adv".into()]).unwrap();
            let scene_id = store.project_tree(&project.id).unwrap().chapters[0].scenes[0].id.clone();
            store.create_element(&scene_id, narration("残る文章"), None).unwrap();
            (project.id, scene_id)
        };
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_project(&project_id).unwrap().templates, ["linear_adv"]);
        assert_eq!(bodies(&store, &scene_id), ["残る文章"]);
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn delete_project_removes_everything() {
        let (mut store, project_id, scene_id) = setup();
        store.create_element(&scene_id, narration("x"), None).unwrap();
        store.delete_project(&project_id).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
        assert!(matches!(store.get_scene(&scene_id), Err(AppError::NotFound(_))));
    }
}
