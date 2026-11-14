//! キャラクターを独立したデータとして保存する基盤（`docs/character-model.md` の段階 1）。
//!
//! - キャラクター本体（名前・別名）、人物についての情報（facts）、2 人の関係（relations）を持つ。
//! - 要素の `character_id` は `speaker` と併存する。`speaker` は書き換えない。
//! - 参照はすべて同じプロジェクト内に限り、入力値と由来（source）・状態（status）・確信度・根拠をサーバーで検証する。
//!
//! 話者名との自動照合・候補の取り込み・AI による推定は、この段階では扱わない。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::store::{clean_title, new_id, Store, NOW};

/// 情報の由来。
pub const SOURCES: [&str; 3] = ["user", "confirmed", "inferred"];
/// 情報の状態。
pub const STATUSES: [&str; 3] = ["active", "proposed", "rejected"];

const MAX_TEXT_CHARS: usize = 2000;
const MAX_CATEGORY_CHARS: usize = 50;
const MAX_ALIASES: usize = 20;
const MAX_EVIDENCE: usize = 50;

#[derive(Debug, Clone, Serialize, PartialEq)]
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

/// 人物についての 1 件の情報（プロフィール・性格・口調など）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharacterFact {
    pub id: String,
    pub character_id: String,
    pub category: String,
    pub text: String,
    pub source: String,
    pub confidence: Option<f64>,
    /// 根拠となる要素の ID。
    pub evidence: Vec<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 2 人の関係（方向つき）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharacterRelation {
    pub id: String,
    pub project_id: String,
    pub from_character_id: String,
    pub to_character_id: String,
    pub kind: String,
    pub text: String,
    pub source: String,
    pub confidence: Option<f64>,
    pub evidence: Vec<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 情報・関係の作成と更新で共通の入力。更新では省略した項目を変えない。
#[derive(Debug, Default, Deserialize)]
pub struct InfoInput {
    pub category: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub source: Option<String>,
    /// `null` で確信度を消す。
    #[serde(default, deserialize_with = "double_option")]
    pub confidence: Option<Option<f64>>,
    pub evidence: Option<Vec<String>>,
    pub status: Option<String>,
    /// 関係の作成時のみ。
    pub from_character_id: Option<String>,
    pub to_character_id: Option<String>,
}

/// 「省略」と「null」を区別して受け取る。
fn double_option<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn json_list(text: String) -> Vec<String> {
    serde_json::from_str(&text).unwrap_or_default()
}

fn character_from_row(row: &Row) -> rusqlite::Result<Character> {
    Ok(Character {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        aliases: json_list(row.get("aliases")?),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn fact_from_row(row: &Row) -> rusqlite::Result<CharacterFact> {
    Ok(CharacterFact {
        id: row.get("id")?,
        character_id: row.get("character_id")?,
        category: row.get("category")?,
        text: row.get("text")?,
        source: row.get("source")?,
        confidence: row.get("confidence")?,
        evidence: json_list(row.get("evidence")?),
        status: row.get("status")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn relation_from_row(row: &Row) -> rusqlite::Result<CharacterRelation> {
    Ok(CharacterRelation {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        from_character_id: row.get("from_character_id")?,
        to_character_id: row.get("to_character_id")?,
        kind: row.get("kind")?,
        text: row.get("text")?,
        source: row.get("source")?,
        confidence: row.get("confidence")?,
        evidence: json_list(row.get("evidence")?),
        status: row.get("status")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn clean_aliases(aliases: Vec<String>, name: &str) -> Result<Vec<String>> {
    if aliases.len() > MAX_ALIASES {
        return Err(AppError::Invalid(format!("別名は{MAX_ALIASES}個までです")));
    }
    let mut result = Vec::new();
    for alias in aliases {
        let alias = clean_title("別名", &alias)?;
        if alias != name && !result.contains(&alias) {
            result.push(alias);
        }
    }
    Ok(result)
}

fn clean_label(what: &str, value: &str) -> Result<String> {
    let v = value.trim();
    if v.is_empty() {
        return Err(AppError::Invalid(format!("{what}を入力してください")));
    }
    if v.chars().count() > MAX_CATEGORY_CHARS || v.chars().any(char::is_control) {
        return Err(AppError::Invalid(format!("{what}は改行を含まない{MAX_CATEGORY_CHARS}文字以内にしてください")));
    }
    Ok(v.to_string())
}

fn clean_text(value: &str, required: bool) -> Result<String> {
    let v = value.trim();
    if required && v.is_empty() {
        return Err(AppError::Invalid("内容を入力してください".into()));
    }
    if v.chars().count() > MAX_TEXT_CHARS {
        return Err(AppError::Invalid(format!("内容は{MAX_TEXT_CHARS}文字以内にしてください")));
    }
    Ok(v.replace("\r\n", "\n"))
}

fn clean_choice(what: &str, value: &str, allowed: &[&str]) -> Result<String> {
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(AppError::Invalid(format!("{what}は {} のいずれかで指定してください", allowed.join("・"))))
    }
}

/// 由来と確信度の組み合わせを検証する。AI の推定（inferred）には確信度が必要。
fn check_confidence(source: &str, confidence: Option<f64>) -> Result<()> {
    if let Some(c) = confidence {
        if !(0.0..=1.0).contains(&c) || c.is_nan() {
            return Err(AppError::Invalid("確信度は 0〜1 で指定してください".into()));
        }
    }
    if source == "inferred" && confidence.is_none() {
        return Err(AppError::Invalid("AI の推定（inferred）には確信度を指定してください".into()));
    }
    Ok(())
}

fn unique_error(e: rusqlite::Error) -> AppError {
    if e.to_string().contains("UNIQUE") {
        AppError::Invalid("同じ名前のキャラクターがすでにいます".into())
    } else {
        e.into()
    }
}

impl Store {
    // ---- キャラクター -------------------------------------------------

    pub fn list_characters(&self, project_id: &str) -> Result<Vec<Character>> {
        self.get_project(project_id)?;
        let mut stmt = self.conn.prepare("SELECT * FROM characters WHERE project_id = ?1 ORDER BY name")?;
        let rows = stmt.query_map([project_id], character_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_character(&self, id: &str) -> Result<Character> {
        self.conn
            .query_row("SELECT * FROM characters WHERE id = ?1", [id], character_from_row)
            .optional()?
            .ok_or(AppError::NotFound("キャラクター"))
    }

    pub fn create_character(&mut self, project_id: &str, req: CreateCharacter) -> Result<Character> {
        self.get_project(project_id)?;
        let name = clean_title("キャラクター名", &req.name)?;
        let aliases = clean_aliases(req.aliases, &name)?;
        let id = new_id();
        self.conn
            .execute(
                &format!(
                    "INSERT INTO characters (id, project_id, name, aliases, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
                ),
                params![id, project_id, name, serde_json::to_string(&aliases)?],
            )
            .map_err(unique_error)?;
        self.get_character(&id)
    }

    pub fn update_character(&mut self, id: &str, req: UpdateCharacter) -> Result<Character> {
        let current = self.get_character(id)?;
        let name = match req.name {
            Some(v) => clean_title("キャラクター名", &v)?,
            None => current.name,
        };
        let aliases = clean_aliases(req.aliases.unwrap_or(current.aliases), &name)?;
        self.conn
            .execute(
                &format!("UPDATE characters SET name = ?2, aliases = ?3, updated_at = {NOW} WHERE id = ?1"),
                params![id, name, serde_json::to_string(&aliases)?],
            )
            .map_err(unique_error)?;
        self.get_character(id)
    }

    /// キャラクターを削除する。情報・関係も削除し、要素の `character_id` は空に戻す（`speaker` と本文は残す）。
    pub fn delete_character(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE elements SET character_id = NULL WHERE character_id = ?1", [id])?;
        let n = tx.execute("DELETE FROM characters WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("キャラクター"));
        }
        tx.commit()?;
        Ok(())
    }

    /// 要素に結びつけるキャラクターが、その場面と同じプロジェクトにいるか確かめる。
    pub(crate) fn check_character_for_scene(&self, scene_id: &str, character_id: &str) -> Result<()> {
        let ok: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM characters ch JOIN chapters c ON c.project_id = ch.project_id
                   JOIN scenes s ON s.chapter_id = c.id
                 WHERE ch.id = ?1 AND s.id = ?2",
                params![character_id, scene_id],
                |r| r.get(0),
            )
            .optional()?;
        ok.map(|_| ()).ok_or_else(|| AppError::Invalid("同じプロジェクトのキャラクターを指定してください".into()))
    }

    /// 根拠の要素がすべて同じプロジェクトにあるか確かめる。
    fn clean_evidence(&self, project_id: &str, evidence: Vec<String>) -> Result<Vec<String>> {
        if evidence.len() > MAX_EVIDENCE {
            return Err(AppError::Invalid(format!("根拠は{MAX_EVIDENCE}件までです")));
        }
        let mut out = Vec::new();
        for id in evidence {
            let found: Option<i64> = self
                .conn
                .query_row(
                    "SELECT 1 FROM elements e JOIN scenes s ON s.id = e.scene_id JOIN chapters c ON c.id = s.chapter_id
                     WHERE e.id = ?1 AND c.project_id = ?2",
                    params![id, project_id],
                    |r| r.get(0),
                )
                .optional()?;
            if found.is_none() {
                return Err(AppError::Invalid("根拠には同じプロジェクトの要素を指定してください".into()));
            }
            if !out.contains(&id) {
                out.push(id);
            }
        }
        Ok(out)
    }

    // ---- 人物の情報 ---------------------------------------------------

    pub fn list_facts(&self, character_id: &str) -> Result<Vec<CharacterFact>> {
        self.get_character(character_id)?;
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM character_facts WHERE character_id = ?1 ORDER BY category, created_at")?;
        let rows = stmt.query_map([character_id], fact_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_fact(&self, id: &str) -> Result<CharacterFact> {
        self.conn
            .query_row("SELECT * FROM character_facts WHERE id = ?1", [id], fact_from_row)
            .optional()?
            .ok_or(AppError::NotFound("キャラクターの情報"))
    }

    pub fn create_fact(&mut self, character_id: &str, req: InfoInput) -> Result<CharacterFact> {
        let character = self.get_character(character_id)?;
        let category = clean_label("分類", req.category.as_deref().unwrap_or(""))?;
        let text = clean_text(req.text.as_deref().unwrap_or(""), true)?;
        let source = clean_choice("由来", req.source.as_deref().unwrap_or("user"), &SOURCES)?;
        let status = clean_choice("状態", req.status.as_deref().unwrap_or("active"), &STATUSES)?;
        let confidence = req.confidence.flatten();
        check_confidence(&source, confidence)?;
        let evidence = self.clean_evidence(&character.project_id, req.evidence.unwrap_or_default())?;
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO character_facts (id, character_id, category, text, source, confidence, evidence, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {NOW}, {NOW})"
            ),
            params![id, character_id, category, text, source, confidence, serde_json::to_string(&evidence)?, status],
        )?;
        self.get_fact(&id)
    }

    pub fn update_fact(&mut self, id: &str, req: InfoInput) -> Result<CharacterFact> {
        let f = self.get_fact(id)?;
        let character = self.get_character(&f.character_id)?;
        let category = match req.category {
            Some(c) => clean_label("分類", &c)?,
            None => f.category,
        };
        let text = match req.text {
            Some(t) => clean_text(&t, true)?,
            None => f.text,
        };
        let source = match req.source {
            Some(s) => clean_choice("由来", &s, &SOURCES)?,
            None => f.source,
        };
        let status = match req.status {
            Some(s) => clean_choice("状態", &s, &STATUSES)?,
            None => f.status,
        };
        let confidence = req.confidence.unwrap_or(f.confidence);
        check_confidence(&source, confidence)?;
        let evidence = match req.evidence {
            Some(e) => self.clean_evidence(&character.project_id, e)?,
            None => f.evidence,
        };
        self.conn.execute(
            &format!(
                "UPDATE character_facts SET category = ?2, text = ?3, source = ?4, confidence = ?5, evidence = ?6,
                        status = ?7, updated_at = {NOW} WHERE id = ?1"
            ),
            params![id, category, text, source, confidence, serde_json::to_string(&evidence)?, status],
        )?;
        self.get_fact(id)
    }

    pub fn delete_fact(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM character_facts WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("キャラクターの情報"));
        }
        Ok(())
    }

    // ---- 関係 ---------------------------------------------------------

    pub fn list_relations(&self, project_id: &str) -> Result<Vec<CharacterRelation>> {
        self.get_project(project_id)?;
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM character_relations WHERE project_id = ?1 ORDER BY created_at")?;
        let rows = stmt.query_map([project_id], relation_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_relation(&self, id: &str) -> Result<CharacterRelation> {
        self.conn
            .query_row("SELECT * FROM character_relations WHERE id = ?1", [id], relation_from_row)
            .optional()?
            .ok_or(AppError::NotFound("関係"))
    }

    pub fn create_relation(&mut self, project_id: &str, req: InfoInput) -> Result<CharacterRelation> {
        self.get_project(project_id)?;
        let pick = |id: Option<&str>| -> Result<Character> {
            let id = id.ok_or_else(|| AppError::Invalid("関係の 2 人を指定してください".into()))?;
            let c = self.get_character(id).map_err(|_| AppError::Invalid("関係の人物が見つかりません".into()))?;
            if c.project_id != project_id {
                return Err(AppError::Invalid("同じプロジェクトのキャラクターを指定してください".into()));
            }
            Ok(c)
        };
        let from = pick(req.from_character_id.as_deref())?;
        let to = pick(req.to_character_id.as_deref())?;
        if from.id == to.id {
            return Err(AppError::Invalid("関係は異なる 2 人の間に設定してください".into()));
        }
        let kind = clean_label("関係の種類", req.kind.as_deref().unwrap_or(""))?;
        let text = clean_text(req.text.as_deref().unwrap_or(""), false)?;
        let source = clean_choice("由来", req.source.as_deref().unwrap_or("user"), &SOURCES)?;
        let status = clean_choice("状態", req.status.as_deref().unwrap_or("active"), &STATUSES)?;
        let confidence = req.confidence.flatten();
        check_confidence(&source, confidence)?;
        let evidence = self.clean_evidence(project_id, req.evidence.unwrap_or_default())?;
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO character_relations (id, project_id, from_character_id, to_character_id, kind, text, source,
                                                  confidence, evidence, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, {NOW}, {NOW})"
            ),
            params![id, project_id, from.id, to.id, kind, text, source, confidence, serde_json::to_string(&evidence)?, status],
        )?;
        self.get_relation(&id)
    }

    /// 関係を更新する。2 人の組み合わせは変えられない（変える場合は作り直す）。
    pub fn update_relation(&mut self, id: &str, req: InfoInput) -> Result<CharacterRelation> {
        let r = self.get_relation(id)?;
        if req.from_character_id.is_some() || req.to_character_id.is_some() {
            return Err(AppError::Invalid("関係の 2 人は変更できません。新しく作り直してください".into()));
        }
        let kind = match req.kind {
            Some(k) => clean_label("関係の種類", &k)?,
            None => r.kind,
        };
        let text = match req.text {
            Some(t) => clean_text(&t, false)?,
            None => r.text,
        };
        let source = match req.source {
            Some(s) => clean_choice("由来", &s, &SOURCES)?,
            None => r.source,
        };
        let status = match req.status {
            Some(s) => clean_choice("状態", &s, &STATUSES)?,
            None => r.status,
        };
        let confidence = req.confidence.unwrap_or(r.confidence);
        check_confidence(&source, confidence)?;
        let evidence = match req.evidence {
            Some(e) => self.clean_evidence(&r.project_id, e)?,
            None => r.evidence,
        };
        self.conn.execute(
            &format!(
                "UPDATE character_relations SET kind = ?2, text = ?3, source = ?4, confidence = ?5, evidence = ?6,
                        status = ?7, updated_at = {NOW} WHERE id = ?1"
            ),
            params![id, kind, text, source, confidence, serde_json::to_string(&evidence)?, status],
        )?;
        self.get_relation(id)
    }

    pub fn delete_relation(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM character_relations WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("関係"));
        }
        Ok(())
    }

    /// JSON 書き出し用: キャラクター（情報つき）と関係。
    pub fn characters_json(&self, project_id: &str) -> Result<(Value, Value)> {
        let mut list = Vec::new();
        for c in self.list_characters(project_id)? {
            let facts = self.list_facts(&c.id)?;
            let mut v = serde_json::to_value(&c)?;
            v["facts"] = serde_json::to_value(facts)?;
            list.push(v);
        }
        Ok((Value::Array(list), serde_json::to_value(self.list_relations(project_id)?)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ElementContent;
    use rusqlite::Connection;
    use serde_json::json;

    #[test]
    fn migrates_existing_v5_database() {
        let dir = std::env::temp_dir().join(format!("scenote-v5-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v5.db");
        {
            // v5 までのスキーマで作った既存の DB を再現する。
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for sql in [
                include_str!("schema_v1.sql"),
                include_str!("schema_v2.sql"),
                include_str!("schema_v3.sql"),
                include_str!("schema_v4.sql"),
                include_str!("schema_v5.sql"),
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 5).unwrap();
            conn.execute_batch(
                "INSERT INTO projects VALUES ('p', '旧作', '', '[]', 't', 't');
                 INSERT INTO chapters VALUES ('c', 'p', '第1章', 0, 't', 't');
                 INSERT INTO scenes VALUES ('s', 'c', '場面1', 0, 1, 't', 't');
                 INSERT INTO elements (id, scene_id, kind, body, speaker, position, created_at, updated_at)
                   VALUES ('e', 's', 'Dialogue', 'こんにちは', 'アリス', 0, 't', 't');",
            )
            .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        let version: i64 = store.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, crate::model::SCHEMA_VERSION);
        let e = store.get_element("e").unwrap();
        assert_eq!((e.speaker.as_str(), e.body.as_str(), e.character_id), ("アリス", "こんにちは", None));
        // 移行後にキャラクターを作り、既存の要素に結びつけられる。
        let alice = store.create_character("p", CreateCharacter { name: "アリス".into(), aliases: vec![] }).unwrap();
        let patch = crate::store::ElementPatch { character_id: Some(Some(alice.id.clone())), ..Default::default() };
        let e = store.update_element("e", e.revision, patch).unwrap();
        assert_eq!(e.character_id.as_deref(), Some(alice.id.as_str()));
        assert_eq!(e.speaker, "アリス");
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    fn input(v: serde_json::Value) -> InfoInput {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn validates_facts_and_relations() {
        let mut store = Store::open_in_memory().unwrap();
        let p = store.create_project("作品", "", &[]).unwrap();
        let scene = store.project_tree(&p.id).unwrap().chapters[0].scenes[0].id.clone();
        let el = store
            .create_element(&scene, ElementContent { kind: "Dialogue".into(), speaker: "アリス".into(), body: "…".into() }, None)
            .unwrap();
        let a = store
            .create_character(&p.id, CreateCharacter { name: " アリス ".into(), aliases: vec!["ありす".into(), "アリス".into(), "ありす".into()] })
            .unwrap();
        assert_eq!((a.name.as_str(), a.aliases.clone()), ("アリス", vec!["ありす".to_string()]));
        assert!(store.create_character(&p.id, CreateCharacter { name: "アリス".into(), aliases: vec![] }).is_err());
        let b = store.create_character(&p.id, CreateCharacter { name: "ボブ".into(), aliases: vec![] }).unwrap();

        // 情報: 由来・状態・確信度・根拠の検証。
        let f = store.create_fact(&a.id, input(json!({ "category": "口調", "text": "丁寧語で話す" }))).unwrap();
        assert_eq!((f.source.as_str(), f.status.as_str(), f.confidence), ("user", "active", None));
        assert!(store.create_fact(&a.id, input(json!({ "category": "", "text": "x" }))).is_err());
        assert!(store.create_fact(&a.id, input(json!({ "category": "性格", "text": "" }))).is_err());
        assert!(store.create_fact(&a.id, input(json!({ "category": "性格", "text": "x", "source": "guess" }))).is_err());
        assert!(store.create_fact(&a.id, input(json!({ "category": "性格", "text": "x", "status": "maybe" }))).is_err());
        assert!(store.create_fact(&a.id, input(json!({ "category": "性格", "text": "x", "source": "inferred" }))).is_err());
        assert!(store.create_fact(&a.id, input(json!({ "category": "性格", "text": "x", "confidence": 1.5 }))).is_err());
        assert!(store.create_fact(&a.id, input(json!({ "category": "性格", "text": "x", "evidence": ["missing"] }))).is_err());
        let inferred = store
            .create_fact(&a.id, input(json!({ "category": "性格", "text": "慎重", "source": "inferred", "confidence": 0.4,
                                              "status": "proposed", "evidence": [el.id, el.id] })))
            .unwrap();
        assert_eq!(inferred.evidence, vec![el.id.clone()]);
        // 更新: 省略した項目は保ち、null で確信度を消せる（inferred のままでは消せない）。
        assert!(store.update_fact(&inferred.id, input(json!({ "confidence": null }))).is_err());
        let confirmed = store.update_fact(&inferred.id, input(json!({ "source": "confirmed", "status": "active", "confidence": null }))).unwrap();
        assert_eq!((confirmed.text.as_str(), confirmed.confidence, confirmed.evidence.len()), ("慎重", None, 1));
        assert_eq!(store.list_facts(&a.id).unwrap().len(), 2);

        // 関係: 同じプロジェクトの異なる 2 人に限る。
        let r = store
            .create_relation(&p.id, input(json!({ "from_character_id": a.id, "to_character_id": b.id, "kind": "信頼", "text": "幼なじみ" })))
            .unwrap();
        assert_eq!((r.kind.as_str(), r.from_character_id.as_str()), ("信頼", a.id.as_str()));
        assert!(store.create_relation(&p.id, input(json!({ "from_character_id": a.id, "to_character_id": a.id, "kind": "x" }))).is_err());
        assert!(store.create_relation(&p.id, input(json!({ "from_character_id": a.id, "kind": "x" }))).is_err());
        assert!(store.update_relation(&r.id, input(json!({ "to_character_id": a.id }))).is_err());
        let r = store.update_relation(&r.id, input(json!({ "kind": "依存", "source": "inferred", "confidence": 0.5 }))).unwrap();
        assert_eq!((r.kind.as_str(), r.text.as_str()), ("依存", "幼なじみ"));

        // 他プロジェクトのキャラクター・要素は参照できない。
        let other = store.create_project("別作品", "", &[]).unwrap();
        let other_scene = store.project_tree(&other.id).unwrap().chapters[0].scenes[0].id.clone();
        let c = store.create_character(&other.id, CreateCharacter { name: "アリス".into(), aliases: vec![] }).unwrap();
        assert!(store.create_relation(&p.id, input(json!({ "from_character_id": a.id, "to_character_id": c.id, "kind": "x" }))).is_err());
        assert!(store.create_fact(&c.id, input(json!({ "category": "x", "text": "x", "evidence": [el.id] }))).is_err());
        assert!(store.check_character_for_scene(&other_scene, &a.id).is_err());
        assert!(store
            .create_element_with_character(&other_scene, ElementContent { kind: "Narration".into(), speaker: "".into(), body: "".into() }, None, Some(&a.id))
            .is_err());

        // 削除すると、要素の character_id は空に戻り、speaker は残る。情報・関係も消える。
        let patch = crate::store::ElementPatch { character_id: Some(Some(b.id.clone())), ..Default::default() };
        store.update_element(&el.id, el.revision, patch).unwrap();
        store.delete_character(&b.id).unwrap();
        let e = store.get_element(&el.id).unwrap();
        assert_eq!((e.character_id, e.speaker.as_str()), (None, "アリス"));
        assert!(store.list_relations(&p.id).unwrap().is_empty());
        store.delete_fact(&f.id).unwrap();
        assert!(store.delete_fact(&f.id).is_err());
    }

    #[test]
    fn restores_snapshot_without_character_data() {
        let mut store = Store::open_in_memory().unwrap();
        let p = store.create_project("旧", "", &[]).unwrap();
        let tree = store.project_tree(&p.id).unwrap();
        let (chapter, scene) = (tree.chapters[0].chapter.id.clone(), tree.chapters[0].scenes[0].id.clone());
        let a = store.create_character(&p.id, CreateCharacter { name: "アリス".into(), aliases: vec![] }).unwrap();
        // v5 までの形式（characters・character_id がない）のスナップショット。
        let old = json!({
            "format": "scenote-snapshot", "schema_version": 5,
            "project": { "name": "旧", "description": "", "templates": [] },
            "chapters": [{ "id": chapter, "title": "第1章", "scenes": [{ "id": scene, "title": "場面1", "elements": [
                { "id": "old-e", "kind": "Dialogue", "speaker": "アリス", "body": "昔の台詞", "attrs": {}, "origin": "human" }
            ] }] }],
            "files": []
        });
        store.conn.execute(
            "INSERT INTO snapshots (id, project_id, note, kind, data, created_at) VALUES ('old', ?1, '旧形式', 'manual', ?2, 't')",
            params![p.id, old.to_string()],
        ).unwrap();
        store.restore_snapshot("old").unwrap();
        let e = store.get_element("old-e").unwrap();
        assert_eq!((e.speaker.as_str(), e.body.as_str(), e.character_id), ("アリス", "昔の台詞", None));
        // 旧形式にはキャラクターがないため、復元後は空になる（復元前の状態は自動保存から戻せる）。
        assert!(store.list_characters(&p.id).unwrap().is_empty());
        let snaps = store.list_snapshots(&p.id).unwrap();
        let backup = snaps.iter().find(|s| s.kind == "before_restore").unwrap();
        store.restore_snapshot(&backup.id).unwrap();
        assert_eq!(store.list_characters(&p.id).unwrap()[0].id, a.id);
    }
}
