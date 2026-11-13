//! プロジェクトに属するキャラクターの永続化。
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::error::{AppError, Result};
use crate::store::{clean_title, new_id, Store, NOW};

#[derive(Debug, Clone, Serialize)]
pub struct Character {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateCharacter {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCharacter {
    pub name: Option<String>,
    pub aliases: Option<Vec<String>>,
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Character> {
    let aliases: String = row.get("aliases")?;
    Ok(Character {
        id: row.get("id")?, project_id: row.get("project_id")?, name: row.get("name")?,
        aliases: serde_json::from_str(&aliases).unwrap_or_default(),
        created_at: row.get("created_at")?, updated_at: row.get("updated_at")?,
    })
}

fn clean_aliases(aliases: Vec<String>, name: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for alias in aliases {
        let alias = clean_title("別名", &alias)?;
        if alias == name { continue; }
        if !result.contains(&alias) { result.push(alias); }
    }
    Ok(result)
}

pub fn list(store: &Store, project_id: &str) -> Result<Vec<Character>> {
    store.get_project(project_id)?;
    let mut stmt = store.conn.prepare("SELECT * FROM characters WHERE project_id = ?1 ORDER BY name")?;
    let rows = stmt.query_map([project_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn get(store: &Store, id: &str) -> Result<Character> {
    store.conn.query_row("SELECT * FROM characters WHERE id = ?1", [id], from_row)
        .optional()?.ok_or(AppError::NotFound("キャラクター"))
}

pub fn create(store: &mut Store, project_id: &str, req: CreateCharacter) -> Result<Character> {
    store.get_project(project_id)?;
    let name = clean_title("キャラクター名", &req.name)?;
    let aliases = clean_aliases(req.aliases, &name)?;
    let id = new_id();
    store.conn.execute(
        &format!("INSERT INTO characters (id, project_id, name, aliases, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"),
        params![id, project_id, name, serde_json::to_string(&aliases)?],
    ).map_err(|error| if error.to_string().contains("UNIQUE") { AppError::Invalid("同じ名前のキャラクターが既に存在します".into()) } else { error.into() })?;
    get(store, &id)
}

pub fn update(store: &mut Store, id: &str, req: UpdateCharacter) -> Result<Character> {
    let current = get(store, id)?;
    let name = match req.name { Some(value) => clean_title("キャラクター名", &value)?, None => current.name };
    let aliases = clean_aliases(req.aliases.unwrap_or(current.aliases), &name)?;
    store.conn.execute(
        &format!("UPDATE characters SET name = ?2, aliases = ?3, updated_at = {NOW} WHERE id = ?1"),
        params![id, name, serde_json::to_string(&aliases)?],
    ).map_err(|error| if error.to_string().contains("UNIQUE") { AppError::Invalid("同じ名前のキャラクターが既に存在します".into()) } else { error.into() })?;
    get(store, id)
}

pub fn delete(store: &mut Store, id: &str) -> Result<()> {
    let changed = store.conn.execute("DELETE FROM characters WHERE id = ?1", [id])?;
    if changed == 0 { return Err(AppError::NotFound("キャラクター")); }
    Ok(())
}
