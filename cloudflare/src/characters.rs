//! キャラクター・人物の情報・関係（Cloudflare 版）。規則は PC 版の `server/src/characters.rs` と同じ。
//! 型と入力の検証は `scenote_core::characters` を共有する。

use serde::Deserialize;
use serde_json::{json, Value};

use scenote_core::characters::*;
use scenote_core::validate::clean_title;

use crate::db::NOW;
use crate::error::{AppError, Result};
use crate::p;
use crate::store::{new_id, Store};

fn invalid(msg: String) -> AppError {
    AppError::Invalid(msg)
}

#[derive(Deserialize)]
struct CharacterRow {
    id: String,
    project_id: String,
    name: String,
    aliases: String,
    created_at: String,
    updated_at: String,
}

impl From<CharacterRow> for Character {
    fn from(r: CharacterRow) -> Self {
        Character { id: r.id, project_id: r.project_id, name: r.name, aliases: json_list(r.aliases), created_at: r.created_at, updated_at: r.updated_at }
    }
}

#[derive(Deserialize)]
struct FactRow {
    id: String,
    character_id: String,
    category: String,
    text: String,
    source: String,
    confidence: Option<f64>,
    evidence: String,
    status: String,
    created_at: String,
    updated_at: String,
}

impl From<FactRow> for CharacterFact {
    fn from(r: FactRow) -> Self {
        CharacterFact {
            id: r.id,
            character_id: r.character_id,
            category: r.category,
            text: r.text,
            source: r.source,
            confidence: r.confidence,
            evidence: json_list(r.evidence),
            status: r.status,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct RelationRow {
    id: String,
    project_id: String,
    from_character_id: String,
    to_character_id: String,
    kind: String,
    text: String,
    source: String,
    confidence: Option<f64>,
    evidence: String,
    status: String,
    created_at: String,
    updated_at: String,
}

impl From<RelationRow> for CharacterRelation {
    fn from(r: RelationRow) -> Self {
        CharacterRelation {
            id: r.id,
            project_id: r.project_id,
            from_character_id: r.from_character_id,
            to_character_id: r.to_character_id,
            kind: r.kind,
            text: r.text,
            source: r.source,
            confidence: r.confidence,
            evidence: json_list(r.evidence),
            status: r.status,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct One {
    #[allow(dead_code)]
    v: i64,
}

impl Store {
    // ---- キャラクター -------------------------------------------------

    pub fn list_characters(&self, project_id: &str) -> Result<Vec<Character>> {
        self.get_project(project_id)?;
        let rows: Vec<CharacterRow> = self.db.all("SELECT * FROM characters WHERE project_id = ? ORDER BY name", p![project_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_character(&self, id: &str) -> Result<Character> {
        self.db
            .opt::<CharacterRow>("SELECT * FROM characters WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("キャラクター"))
    }

    fn check_unique_name(&self, project_id: &str, name: &str, except: Option<&str>) -> Result<()> {
        let dup: Option<One> = self.db.opt(
            "SELECT 1 AS v FROM characters WHERE project_id = ? AND name = ? AND id != ?",
            p![project_id, name, except.unwrap_or("")],
        )?;
        if dup.is_some() {
            return Err(AppError::Invalid("同じ名前のキャラクターがすでにいます".into()));
        }
        Ok(())
    }

    pub fn create_character(&self, project_id: &str, req: CreateCharacter) -> Result<Character> {
        self.get_project(project_id)?;
        let name = clean_title("キャラクター名", &req.name).map_err(invalid)?;
        let aliases = clean_aliases(req.aliases, &name).map_err(invalid)?;
        self.check_unique_name(project_id, &name, None)?;
        let id = new_id();
        self.db.exec(
            &format!("INSERT INTO characters (id, project_id, name, aliases, created_at, updated_at) VALUES (?, ?, ?, ?, {NOW}, {NOW})"),
            p![id.as_str(), project_id, name.as_str(), json!(aliases).to_string()],
        )?;
        self.get_character(&id)
    }

    pub fn update_character(&self, id: &str, req: UpdateCharacter) -> Result<Character> {
        let current = self.get_character(id)?;
        let name = match req.name {
            Some(v) => clean_title("キャラクター名", &v).map_err(invalid)?,
            None => current.name,
        };
        let aliases = clean_aliases(req.aliases.unwrap_or(current.aliases), &name).map_err(invalid)?;
        self.check_unique_name(&current.project_id, &name, Some(id))?;
        self.db.exec(
            &format!("UPDATE characters SET name = ?, aliases = ?, updated_at = {NOW} WHERE id = ?"),
            p![name.as_str(), json!(aliases).to_string(), id],
        )?;
        self.get_character(id)
    }

    /// キャラクターを削除する。情報・関係も削除し、要素の `character_id` は空に戻す（`speaker` と本文は残す）。
    pub fn delete_character(&self, id: &str) -> Result<()> {
        self.get_character(id)?;
        self.db.exec("UPDATE elements SET character_id = NULL WHERE character_id = ?", p![id])?;
        self.db.exec("DELETE FROM character_facts WHERE character_id = ?", p![id])?;
        self.db.exec("DELETE FROM character_relations WHERE from_character_id = ? OR to_character_id = ?", p![id, id])?;
        self.db.exec("DELETE FROM characters WHERE id = ?", p![id])
    }

    /// 要素に結びつけるキャラクターが、その場面と同じプロジェクトにいるか確かめる。
    pub fn check_character_for_scene(&self, scene_id: &str, character_id: &str) -> Result<()> {
        let ok: Option<One> = self.db.opt(
            "SELECT 1 AS v FROM characters ch JOIN chapters c ON c.project_id = ch.project_id
               JOIN scenes s ON s.chapter_id = c.id
             WHERE ch.id = ? AND s.id = ?",
            p![character_id, scene_id],
        )?;
        ok.map(|_| ()).ok_or_else(|| AppError::Invalid("同じプロジェクトのキャラクターを指定してください".into()))
    }

    /// 根拠の要素がすべて同じプロジェクトにあるか確かめる。
    fn clean_evidence(&self, project_id: &str, evidence: Vec<String>) -> Result<Vec<String>> {
        if evidence.len() > MAX_EVIDENCE {
            return Err(AppError::Invalid(format!("根拠は{MAX_EVIDENCE}件までです")));
        }
        let mut out = Vec::new();
        for id in evidence {
            let found: Option<One> = self.db.opt(
                "SELECT 1 AS v FROM elements e JOIN scenes s ON s.id = e.scene_id JOIN chapters c ON c.id = s.chapter_id
                 WHERE e.id = ? AND c.project_id = ?",
                p![id.as_str(), project_id],
            )?;
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
        let rows: Vec<FactRow> =
            self.db.all("SELECT * FROM character_facts WHERE character_id = ? ORDER BY category, created_at", p![character_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_fact(&self, id: &str) -> Result<CharacterFact> {
        self.db
            .opt::<FactRow>("SELECT * FROM character_facts WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("キャラクターの情報"))
    }

    pub fn create_fact(&self, character_id: &str, req: InfoInput) -> Result<CharacterFact> {
        let character = self.get_character(character_id)?;
        let category = clean_label("分類", req.category.as_deref().unwrap_or("")).map_err(invalid)?;
        let text = clean_text(req.text.as_deref().unwrap_or(""), true).map_err(invalid)?;
        let source = clean_choice("由来", req.source.as_deref().unwrap_or("user"), &SOURCES).map_err(invalid)?;
        let status = clean_choice("状態", req.status.as_deref().unwrap_or("active"), &STATUSES).map_err(invalid)?;
        let confidence = req.confidence.flatten();
        check_confidence(&source, confidence).map_err(invalid)?;
        let evidence = self.clean_evidence(&character.project_id, req.evidence.unwrap_or_default())?;
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO character_facts (id, character_id, category, text, source, confidence, evidence, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), character_id, category, text, source, confidence, json!(evidence).to_string(), status],
        )?;
        self.get_fact(&id)
    }

    pub fn update_fact(&self, id: &str, req: InfoInput) -> Result<CharacterFact> {
        let f = self.get_fact(id)?;
        let character = self.get_character(&f.character_id)?;
        let category = match req.category {
            Some(c) => clean_label("分類", &c).map_err(invalid)?,
            None => f.category,
        };
        let text = match req.text {
            Some(t) => clean_text(&t, true).map_err(invalid)?,
            None => f.text,
        };
        let source = match req.source {
            Some(s) => clean_choice("由来", &s, &SOURCES).map_err(invalid)?,
            None => f.source,
        };
        let status = match req.status {
            Some(s) => clean_choice("状態", &s, &STATUSES).map_err(invalid)?,
            None => f.status,
        };
        let confidence = req.confidence.unwrap_or(f.confidence);
        check_confidence(&source, confidence).map_err(invalid)?;
        let evidence = match req.evidence {
            Some(e) => self.clean_evidence(&character.project_id, e)?,
            None => f.evidence,
        };
        self.db.exec(
            &format!(
                "UPDATE character_facts SET category = ?, text = ?, source = ?, confidence = ?, evidence = ?, status = ?, updated_at = {NOW}
                 WHERE id = ?"
            ),
            p![category, text, source, confidence, json!(evidence).to_string(), status, id],
        )?;
        self.get_fact(id)
    }

    pub fn delete_fact(&self, id: &str) -> Result<()> {
        self.get_fact(id)?;
        self.db.exec("DELETE FROM character_facts WHERE id = ?", p![id])
    }

    // ---- 関係 ---------------------------------------------------------

    pub fn list_relations(&self, project_id: &str) -> Result<Vec<CharacterRelation>> {
        self.get_project(project_id)?;
        let rows: Vec<RelationRow> =
            self.db.all("SELECT * FROM character_relations WHERE project_id = ? ORDER BY created_at", p![project_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_relation(&self, id: &str) -> Result<CharacterRelation> {
        self.db
            .opt::<RelationRow>("SELECT * FROM character_relations WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("関係"))
    }

    pub fn create_relation(&self, project_id: &str, req: InfoInput) -> Result<CharacterRelation> {
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
        let kind = clean_label("関係の種類", req.kind.as_deref().unwrap_or("")).map_err(invalid)?;
        let text = clean_text(req.text.as_deref().unwrap_or(""), false).map_err(invalid)?;
        let source = clean_choice("由来", req.source.as_deref().unwrap_or("user"), &SOURCES).map_err(invalid)?;
        let status = clean_choice("状態", req.status.as_deref().unwrap_or("active"), &STATUSES).map_err(invalid)?;
        let confidence = req.confidence.flatten();
        check_confidence(&source, confidence).map_err(invalid)?;
        let evidence = self.clean_evidence(project_id, req.evidence.unwrap_or_default())?;
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO character_relations (id, project_id, from_character_id, to_character_id, kind, text, source,
                                                  confidence, evidence, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), project_id, from.id, to.id, kind, text, source, confidence, json!(evidence).to_string(), status],
        )?;
        self.get_relation(&id)
    }

    /// 関係を更新する。2 人の組み合わせは変えられない（変える場合は作り直す）。
    pub fn update_relation(&self, id: &str, req: InfoInput) -> Result<CharacterRelation> {
        let r = self.get_relation(id)?;
        if req.from_character_id.is_some() || req.to_character_id.is_some() {
            return Err(AppError::Invalid("関係の 2 人は変更できません。新しく作り直してください".into()));
        }
        let kind = match req.kind {
            Some(k) => clean_label("関係の種類", &k).map_err(invalid)?,
            None => r.kind,
        };
        let text = match req.text {
            Some(t) => clean_text(&t, false).map_err(invalid)?,
            None => r.text,
        };
        let source = match req.source {
            Some(s) => clean_choice("由来", &s, &SOURCES).map_err(invalid)?,
            None => r.source,
        };
        let status = match req.status {
            Some(s) => clean_choice("状態", &s, &STATUSES).map_err(invalid)?,
            None => r.status,
        };
        let confidence = req.confidence.unwrap_or(r.confidence);
        check_confidence(&source, confidence).map_err(invalid)?;
        let evidence = match req.evidence {
            Some(e) => self.clean_evidence(&r.project_id, e)?,
            None => r.evidence,
        };
        self.db.exec(
            &format!(
                "UPDATE character_relations SET kind = ?, text = ?, source = ?, confidence = ?, evidence = ?, status = ?, updated_at = {NOW}
                 WHERE id = ?"
            ),
            p![kind, text, source, confidence, json!(evidence).to_string(), status, id],
        )?;
        self.get_relation(id)
    }

    pub fn delete_relation(&self, id: &str) -> Result<()> {
        self.get_relation(id)?;
        self.db.exec("DELETE FROM character_relations WHERE id = ?", p![id])
    }

    /// JSON 書き出し用: キャラクター（情報つき）と関係。
    pub fn characters_json(&self, project_id: &str) -> Result<(Value, Value)> {
        let mut list = Vec::new();
        for c in self.list_characters(project_id)? {
            let facts = self.list_facts(&c.id)?;
            let mut v = json!(c);
            v["facts"] = json!(facts);
            list.push(v);
        }
        Ok((Value::Array(list), json!(self.list_relations(project_id)?)))
    }
}
