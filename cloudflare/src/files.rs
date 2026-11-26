//! 参照ファイル（世界観・キャラクター設定・制作方針などの資料。Cloudflare 版）。
//! 規則は PC 版の `server/src/store_ai.rs` と同じ。変更は履歴に記録する。

use serde::Deserialize;
use serde_json::{json, Value};

use scenote_core::files::{clean_file_content, ReferenceFile};
use scenote_core::history::{ACTION_CREATE, ACTION_DELETE, ACTION_UPDATE, TARGET_FILE};
use scenote_core::validate::clean_title;

use crate::db::NOW;
use crate::error::{AppError, Result};
use crate::history::Change;
use crate::p;
use crate::store::{new_id, Store};

#[derive(Debug, Default)]
pub struct FilePatch {
    pub name: Option<String>,
    pub content: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Deserialize)]
struct FileRow {
    id: String,
    project_id: String,
    name: String,
    content: String,
    pinned: i64,
    revision: i64,
    created_at: String,
    updated_at: String,
}

impl From<FileRow> for ReferenceFile {
    fn from(r: FileRow) -> Self {
        ReferenceFile {
            id: r.id,
            project_id: r.project_id,
            name: r.name,
            content: r.content,
            pinned: r.pinned != 0,
            revision: r.revision,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

fn file_value(f: &ReferenceFile) -> Value {
    json!({ "name": f.name, "content": f.content, "pinned": f.pinned })
}

impl Store {
    pub fn list_files(&self, project_id: &str) -> Result<Vec<ReferenceFile>> {
        let rows: Vec<FileRow> =
            self.db.all("SELECT * FROM reference_files WHERE project_id = ? ORDER BY name, created_at", p![project_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_file(&self, id: &str) -> Result<ReferenceFile> {
        self.db
            .opt::<FileRow>("SELECT * FROM reference_files WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("参照ファイル"))
    }

    fn record_file(&self, file: &ReferenceFile, action: &str, before: Option<Value>, after: Option<Value>) -> Result<()> {
        self.record(Change {
            project_id: &file.project_id,
            scene_id: None,
            target_type: TARGET_FILE,
            target_id: &file.id,
            action,
            before,
            after,
        })
    }

    pub fn create_file(&self, project_id: &str, name: &str, content: &str, pinned: bool) -> Result<ReferenceFile> {
        self.get_project(project_id)?;
        let name = clean_title("ファイル名", name).map_err(AppError::Invalid)?;
        let content = clean_file_content(content.to_string()).map_err(AppError::Invalid)?;
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO reference_files (id, project_id, name, content, pinned, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), project_id, name, content, pinned as i64],
        )?;
        let file = self.get_file(&id)?;
        self.record_file(&file, ACTION_CREATE, None, Some(file_value(&file)))?;
        Ok(file)
    }

    pub fn update_file(&self, id: &str, base_revision: i64, patch: FilePatch) -> Result<ReferenceFile> {
        let current = self.get_file(id)?;
        if current.revision != base_revision {
            return Err(AppError::Conflict { current: current.revision });
        }
        let name = match patch.name {
            Some(n) => clean_title("ファイル名", &n).map_err(AppError::Invalid)?,
            None => current.name.clone(),
        };
        let content = match patch.content {
            Some(c) => clean_file_content(c).map_err(AppError::Invalid)?,
            None => current.content.clone(),
        };
        self.db.exec(
            &format!(
                "UPDATE reference_files SET name = ?, content = ?, pinned = ?, revision = revision + 1, updated_at = {NOW}
                 WHERE id = ?"
            ),
            p![name, content, patch.pinned.unwrap_or(current.pinned) as i64, id],
        )?;
        let updated = self.get_file(id)?;
        self.record_file(&updated, ACTION_UPDATE, Some(file_value(&current)), Some(file_value(&updated)))?;
        Ok(updated)
    }

    pub fn delete_file(&self, id: &str) -> Result<()> {
        let file = self.get_file(id)?;
        self.record_file(&file, ACTION_DELETE, Some(file_value(&file)), None)?;
        self.db.exec("DELETE FROM reference_files WHERE id = ?", p![id])
    }
}
