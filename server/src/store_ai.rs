//! 参照ファイル・チャット・AI 提案・設定・使用量の永続化。
//!
//! AI の提案は `proposals` に保存するだけで、承認されるまでシナリオや参照ファイルへ反映しない。

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};

use crate::diff::{diff_text, Segment};
use crate::history::{
    self as hist, file_value, record_element, ACTION_CREATE, ACTION_DELETE, ACTION_UPDATE, ACTOR_AI_APPROVED,
    ACTOR_AI_MODIFIED, ACTOR_HUMAN, TARGET_FILE,
};
use crate::error::{AppError, Result};
use crate::model::*;
use crate::store::{
    clean_content, clean_title, content_of, element_from_row, insert_element, new_id, renumber,
    touch_project, touch_scene, update_element_content, Store, NOW, clean_summary, record_summary};

pub const OP_UPDATE_ELEMENT: &str = "update_element";
pub const OP_INSERT_ELEMENT: &str = "insert_element";
pub const OP_DELETE_ELEMENT: &str = "delete_element";
pub const OP_UPDATE_FILE: &str = "update_file";
pub const OP_CREATE_FILE: &str = "create_file";
/// 場面の概要（プロット）の変更。
pub const OP_UPDATE_SUMMARY: &str = "update_summary";
/// 概要つきの場面の追加（指定した場面の直後）。
pub const OP_INSERT_SCENE: &str = "insert_scene";

pub const ANCHOR_START: &str = "@start";
pub const ANCHOR_END: &str = "@end";

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_APPROVED: &str = "approved";
pub const STATUS_REJECTED: &str = "rejected";
/// 提案の生成後に対象が変更・削除され、そのままでは適用できなくなった。
pub const STATUS_STALE: &str = "stale";

const KEY_API_KEY: &str = "llm.api_key";
const KEY_MODEL: &str = "llm.model";
const KEY_TOKEN_LIMIT: &str = "usage.monthly_token_limit";
const KEY_WARN_PERCENT: &str = "usage.warn_percent";
const KEY_STOPPED: &str = "usage.stopped";
const KEY_OVERRIDE_MONTH: &str = "usage.override_month";

/// 既定のモデル。Gemini の「最新の Flash」を指す別名で、設定から変更できる。
pub const DEFAULT_MODEL: &str = "gemini-flash-latest";
/// 月あたりのトークン上限の初期値（0 は無制限）。
pub const DEFAULT_TOKEN_LIMIT: i64 = 2_000_000;
pub const DEFAULT_WARN_PERCENT: i64 = 80;

const MAX_FILE_CHARS: usize = 200_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReferenceFile {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub content: String,
    pub pinned: bool,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Default)]
pub struct FilePatch {
    pub name: Option<String>,
    pub content: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Chat {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub instructions: String,
    pub auto_select: bool,
    /// このチャットで参照するよう指定したファイル。
    pub file_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Default)]
pub struct ChatPatch {
    pub title: Option<String>,
    pub instructions: Option<String>,
    pub auto_select: Option<bool>,
    pub file_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: String,
    pub message_id: String,
    pub op: String,
    pub target_id: Option<String>,
    pub scene_id: Option<String>,
    pub anchor: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub reason: String,
    pub status: String,
    pub reject_reason: String,
    pub result_id: Option<String>,
    /// 人間が修正して採用した場合の、実際に反映した内容。
    pub final_content: Option<Value>,
    /// 本文（要素の `body`、ファイルの `content`）の変更前後の差分。
    pub diff: Vec<Segment>,
    #[serde(skip)]
    pub project_id: String,
    #[serde(skip)]
    pub ordinal: i64,
    #[serde(skip)]
    pub base_revision: Option<i64>,
}

/// 検証済みの提案（保存前）。
#[derive(Debug, Clone)]
pub struct NewProposal {
    pub op: &'static str,
    pub target_id: Option<String>,
    pub scene_id: Option<String>,
    pub anchor: Option<String>,
    pub base_revision: Option<i64>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub chat_id: String,
    pub role: String,
    pub content: String,
    pub scene_id: Option<String>,
    pub referenced_files: Value,
    pub warnings: Value,
    pub created_at: String,
    pub proposals: Vec<Proposal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatDetail {
    #[serde(flatten)]
    pub chat: Chat,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmSettings {
    pub provider: &'static str,
    pub model: String,
    /// API キーが設定済みか。キーそのものは決して返さない。
    pub has_api_key: bool,
    pub monthly_token_limit: i64,
    pub warn_percent: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageStatus {
    /// 集計対象の月（UTC、`YYYY-MM`）。
    pub month: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub monthly_token_limit: i64,
    pub warn_percent: i64,
    /// ok / warning / stopped
    pub state: &'static str,
    pub by_model: Vec<ModelUsage>,
}

fn file_from_row(row: &Row) -> rusqlite::Result<ReferenceFile> {
    Ok(ReferenceFile {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        content: row.get("content")?,
        pinned: row.get::<_, i64>("pinned")? != 0,
        revision: row.get("revision")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn parse_json(text: Option<String>) -> Option<Value> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

fn text_of<'a>(value: &'a Option<Value>, key: &str) -> &'a str {
    value
        .as_ref()
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn proposal_from_row(row: &Row) -> rusqlite::Result<Proposal> {
    let op: String = row.get("op")?;
    let before = parse_json(row.get("before")?);
    let after = parse_json(row.get("after")?);
    let key = if op == OP_UPDATE_FILE || op == OP_CREATE_FILE {
        "content"
    } else {
        "body"
    };
    // 修正して採用した場合は、実際に反映した内容との差分を示す。
    let final_content = parse_json(row.get("final")?);
    let shown = if final_content.is_some() { &final_content } else { &after };
    let diff = diff_text(text_of(&before, key), text_of(shown, key));
    Ok(Proposal {
        id: row.get("id")?,
        message_id: row.get("message_id")?,
        op,
        target_id: row.get("target_id")?,
        scene_id: row.get("scene_id")?,
        anchor: row.get("anchor")?,
        before,
        after,
        reason: row.get("reason")?,
        status: row.get("status")?,
        reject_reason: row.get("reject_reason")?,
        result_id: row.get("result_id")?,
        final_content,
        diff,
        project_id: row.get("project_id")?,
        ordinal: row.get("ordinal")?,
        base_revision: row.get("base_revision")?,
    })
}

fn clean_file_content(content: String) -> Result<String> {
    if content.chars().count() > MAX_FILE_CHARS {
        return Err(AppError::Invalid(format!(
            "参照ファイルは{MAX_FILE_CHARS}文字以内にしてください"
        )));
    }
    Ok(content.replace("\r\n", "\n").replace('\r', "\n"))
}

fn element_content_from(value: &Option<Value>) -> Result<ElementContent> {
    let value = value
        .clone()
        .ok_or_else(|| AppError::Internal("proposal has no content".into()))?;
    Ok(serde_json::from_value(value)?)
}

impl Store {
    // ---- 参照ファイル -------------------------------------------------

    pub fn list_files(&self, project_id: &str) -> Result<Vec<ReferenceFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM reference_files WHERE project_id = ?1 ORDER BY name, created_at",
        )?;
        let rows = stmt.query_map([project_id], file_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_file(&self, id: &str) -> Result<ReferenceFile> {
        self.conn
            .query_row("SELECT * FROM reference_files WHERE id = ?1", [id], file_from_row)
            .optional()?
            .ok_or(AppError::NotFound("参照ファイル"))
    }

    pub fn create_file(&mut self, project_id: &str, name: &str, content: &str) -> Result<ReferenceFile> {
        self.get_project(project_id)?;
        let name = clean_title("ファイル名", name)?;
        let content = clean_file_content(content.to_string())?;
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO reference_files (id, project_id, name, content, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
            ),
            params![id, project_id, name, content],
        )?;
        let file = self.get_file(&id)?;
        let tx = self.conn.transaction()?;
        record_file(&tx, &file, ACTION_CREATE, ACTOR_HUMAN, None, Some(file_value(&file)), None)?;
        tx.commit()?;
        Ok(file)
    }

    pub fn update_file(&mut self, id: &str, base_revision: i64, patch: FilePatch) -> Result<ReferenceFile> {
        let current = self.get_file(id)?;
        if current.revision != base_revision {
            return Err(AppError::Conflict {
                current: current.revision,
            });
        }
        let name = match patch.name {
            Some(n) => clean_title("ファイル名", &n)?,
            None => current.name.clone(),
        };
        let content = match patch.content {
            Some(c) => clean_file_content(c)?,
            None => current.content.clone(),
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "UPDATE reference_files SET name = ?2, content = ?3, pinned = ?4,
                        revision = revision + 1, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![id, name, content, patch.pinned.unwrap_or(current.pinned) as i64],
        )?;
        let updated = tx.query_row("SELECT * FROM reference_files WHERE id = ?1", [id], file_from_row)?;
        record_file(&tx, &updated, ACTION_UPDATE, ACTOR_HUMAN, Some(file_value(&current)), Some(file_value(&updated)), None)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_file(&mut self, id: &str) -> Result<()> {
        let file = self.get_file(id)?;
        let tx = self.conn.transaction()?;
        record_file(&tx, &file, ACTION_DELETE, ACTOR_HUMAN, Some(file_value(&file)), None, None)?;
        tx.execute("DELETE FROM reference_files WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    // ---- チャット -----------------------------------------------------

    fn chat_from_row(&self, row: &Row) -> rusqlite::Result<Chat> {
        Ok(Chat {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            title: row.get("title")?,
            instructions: row.get("instructions")?,
            auto_select: row.get::<_, i64>("auto_select")? != 0,
            file_ids: Vec::new(),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    fn chat_file_ids(&self, chat_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_id FROM chat_files WHERE chat_id = ?1 ORDER BY file_id")?;
        let rows = stmt.query_map([chat_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_chats(&self, project_id: &str) -> Result<Vec<Chat>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM chats WHERE project_id = ?1 ORDER BY created_at, id")?;
        let rows = stmt.query_map([project_id], |r| self.chat_from_row(r))?;
        let mut chats: Vec<Chat> = rows.collect::<rusqlite::Result<_>>()?;
        for chat in &mut chats {
            chat.file_ids = self.chat_file_ids(&chat.id)?;
        }
        Ok(chats)
    }

    pub fn get_chat(&self, id: &str) -> Result<Chat> {
        let mut chat = self
            .conn
            .query_row("SELECT * FROM chats WHERE id = ?1", [id], |r| self.chat_from_row(r))
            .optional()?
            .ok_or(AppError::NotFound("チャット"))?;
        chat.file_ids = self.chat_file_ids(id)?;
        Ok(chat)
    }

    pub fn create_chat(&mut self, project_id: &str, title: &str) -> Result<Chat> {
        self.get_project(project_id)?;
        let title = clean_title("チャット名", title)?;
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO chats (id, project_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, {NOW}, {NOW})"
            ),
            params![id, project_id, title],
        )?;
        self.get_chat(&id)
    }

    pub fn update_chat(&mut self, id: &str, patch: ChatPatch) -> Result<Chat> {
        let current = self.get_chat(id)?;
        let title = match patch.title {
            Some(t) => clean_title("チャット名", &t)?,
            None => current.title,
        };
        if let Some(ids) = &patch.file_ids {
            for file_id in ids {
                if self.get_file(file_id)?.project_id != current.project_id {
                    return Err(AppError::Invalid("別のプロジェクトのファイルは指定できません".into()));
                }
            }
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "UPDATE chats SET title = ?2, instructions = ?3, auto_select = ?4, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![
                id,
                title,
                patch.instructions.unwrap_or(current.instructions),
                patch.auto_select.unwrap_or(current.auto_select) as i64
            ],
        )?;
        if let Some(ids) = patch.file_ids {
            tx.execute("DELETE FROM chat_files WHERE chat_id = ?1", [id])?;
            for file_id in ids {
                tx.execute(
                    "INSERT OR IGNORE INTO chat_files (chat_id, file_id) VALUES (?1, ?2)",
                    params![id, file_id],
                )?;
            }
        }
        tx.commit()?;
        self.get_chat(id)
    }

    pub fn delete_chat(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM chats WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("チャット"));
        }
        Ok(())
    }

    fn proposals_for_message(&self, message_id: &str) -> Result<Vec<Proposal>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM proposals WHERE message_id = ?1 ORDER BY ordinal")?;
        let rows = stmt.query_map([message_id], proposal_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_messages(&self, chat_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM messages WHERE chat_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map([chat_id], |row| {
            Ok(Message {
                id: row.get("id")?,
                chat_id: row.get("chat_id")?,
                role: row.get("role")?,
                content: row.get("content")?,
                scene_id: row.get("scene_id")?,
                referenced_files: parse_json(row.get("referenced_files")?).unwrap_or_else(|| json!([])),
                warnings: parse_json(row.get("warnings")?).unwrap_or_else(|| json!([])),
                created_at: row.get("created_at")?,
                proposals: Vec::new(),
            })
        })?;
        let mut messages: Vec<Message> = rows.collect::<rusqlite::Result<_>>()?;
        for message in &mut messages {
            message.proposals = self.proposals_for_message(&message.id)?;
        }
        Ok(messages)
    }

    pub fn chat_detail(&self, id: &str) -> Result<ChatDetail> {
        Ok(ChatDetail {
            chat: self.get_chat(id)?,
            messages: self.list_messages(id)?,
        })
    }

    /// ユーザーの依頼と AI の応答（および提案）を 1 つのトランザクションで保存する。
    ///
    /// 提案はここでは保存するだけで、シナリオや参照ファイルには一切反映しない。
    #[allow(clippy::too_many_arguments)]
    pub fn add_exchange(
        &mut self,
        chat_id: &str,
        scene_id: Option<&str>,
        user_content: &str,
        assistant_content: &str,
        referenced_files: &Value,
        warnings: &[String],
        proposals: &[NewProposal],
    ) -> Result<Vec<Message>> {
        let chat = self.get_chat(chat_id)?;
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE chat_id = ?1",
            [chat_id],
            |r| r.get(0),
        )?;
        let user_id = new_id();
        let assistant_id = new_id();
        let insert = format!(
            "INSERT INTO messages (id, chat_id, seq, role, content, scene_id, referenced_files, warnings, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {NOW})"
        );
        tx.execute(
            &insert,
            params![user_id, chat_id, seq + 1, "user", user_content, scene_id, "[]", "[]"],
        )?;
        tx.execute(
            &insert,
            params![
                assistant_id,
                chat_id,
                seq + 2,
                "assistant",
                assistant_content,
                scene_id,
                referenced_files.to_string(),
                serde_json::to_string(warnings)?
            ],
        )?;
        for (ordinal, p) in proposals.iter().enumerate() {
            tx.execute(
                &format!(
                    "INSERT INTO proposals (id, message_id, project_id, ordinal, op, target_id, scene_id,
                                            anchor, base_revision, before, after, reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, {NOW})"
                ),
                params![
                    new_id(),
                    assistant_id,
                    chat.project_id,
                    ordinal as i64,
                    p.op,
                    p.target_id,
                    p.scene_id,
                    p.anchor,
                    p.base_revision,
                    p.before.as_ref().map(Value::to_string),
                    p.after.as_ref().map(Value::to_string),
                    p.reason
                ],
            )?;
        }
        tx.execute(
            &format!("UPDATE chats SET updated_at = {NOW} WHERE id = ?1"),
            [chat_id],
        )?;
        tx.commit()?;
        let messages = self.list_messages(chat_id)?;
        Ok(messages.into_iter().rev().take(2).rev().collect())
    }

    // ---- 提案の承認・却下 ---------------------------------------------

    pub fn get_proposal(&self, id: &str) -> Result<Proposal> {
        self.conn
            .query_row("SELECT * FROM proposals WHERE id = ?1", [id], proposal_from_row)
            .optional()?
            .ok_or(AppError::NotFound("提案"))
    }

    fn pending_proposal(&self, id: &str) -> Result<Proposal> {
        let proposal = self.get_proposal(id)?;
        if proposal.status != STATUS_PENDING {
            return Err(AppError::Invalid("この提案はすでに処理されています".into()));
        }
        Ok(proposal)
    }

    /// 提案を却下する。却下した提案が後から適用されることはない。
    pub fn reject_proposal(&mut self, id: &str, reason: &str) -> Result<Proposal> {
        self.pending_proposal(id)?;
        self.conn.execute(
            &format!(
                "UPDATE proposals SET status = ?2, reject_reason = ?3, decided_at = {NOW} WHERE id = ?1"
            ),
            params![id, STATUS_REJECTED, reason.trim()],
        )?;
        self.get_proposal(id)
    }

    /// 提案を承認して反映する。
    ///
    /// 提案の生成後に対象が変更・削除されていた場合は反映せず、状態を `stale` にして返す。
    pub fn approve_proposal(&mut self, id: &str) -> Result<Proposal> {
        self.approve_proposal_with(id, None)
    }

    /// 提案を承認して反映する。`edited` を渡すと、提案の内容を人間が修正したものとして反映する
    /// （由来は「AI 提案を人間が修正」）。修正できるのは変更・追加・ファイル変更・ファイル作成の提案。
    pub fn approve_proposal_with(&mut self, id: &str, edited: Option<Value>) -> Result<Proposal> {
        let mut proposal = self.pending_proposal(id)?;
        let edited = match edited {
            None => None,
            Some(value) => Some(self.validate_edit(&proposal, value)?),
        };
        if let Some(value) = &edited {
            if Some(value) == proposal.after.as_ref() {
                // 内容が変わっていなければ、そのまま承認したものとして扱う。
                return self.approve_proposal_with(id, None);
            }
            proposal.after = Some(value.clone());
        }
        let modified = edited.is_some();
        let actor = if modified { ACTOR_AI_MODIFIED } else { ACTOR_AI_APPROVED };
        let element_origin = if modified { ORIGIN_AI_MODIFIED } else { ORIGIN_AI_APPROVED };
        let tx = self.conn.transaction()?;
        let outcome: Option<Option<String>> = match proposal.op.as_str() {
            OP_UPDATE_ELEMENT | OP_DELETE_ELEMENT => {
                let target = proposal.target_id.as_deref().unwrap_or("");
                let current = tx
                    .query_row("SELECT * FROM elements WHERE id = ?1", [target], element_from_row)
                    .optional()?;
                match current {
                    Some(e) if Some(e.revision) == proposal.base_revision => {
                        let before = content_of(&e);
                        if proposal.op == OP_UPDATE_ELEMENT {
                            let content = clean_content(element_content_from(&proposal.after)?)?;
                            update_element_content(&tx, &e.id, &content, element_origin)?;
                            record_element(&tx, &e.scene_id, &e.id, ACTION_UPDATE, actor, Some(&before), Some(&content), Some(id))?;
                        } else {
                            tx.execute("DELETE FROM elements WHERE id = ?1", [&e.id])?;
                            record_element(&tx, &e.scene_id, &e.id, ACTION_DELETE, actor, Some(&before), None, Some(id))?;
                            renumber(&tx, "elements", "scene_id", &e.scene_id)?;
                        }
                        touch_scene(&tx, &e.scene_id)?;
                        Some(None)
                    }
                    _ => None,
                }
            }
            OP_INSERT_ELEMENT => {
                let scene_id = proposal.scene_id.as_deref().unwrap_or("");
                let content = clean_content(element_content_from(&proposal.after)?)?;
                match insert_position(&tx, &proposal, scene_id)? {
                    Some(position) => {
                        tx.execute(
                            "UPDATE elements SET position = position + 1 WHERE scene_id = ?1 AND position >= ?2",
                            params![scene_id, position],
                        )?;
                        let new_id = insert_element(&tx, scene_id, &content, position, element_origin)?;
                        record_element(&tx, scene_id, &new_id, ACTION_CREATE, actor, None, Some(&content), Some(id))?;
                        touch_scene(&tx, scene_id)?;
                        Some(Some(new_id))
                    }
                    None => None,
                }
            }
            OP_UPDATE_FILE => {
                let target = proposal.target_id.as_deref().unwrap_or("");
                let current = tx
                    .query_row("SELECT * FROM reference_files WHERE id = ?1", [target], file_from_row)
                    .optional()?;
                match current {
                    Some(f) if Some(f.revision) == proposal.base_revision => {
                        let content = clean_file_content(text_of(&proposal.after, "content").to_string())?;
                        tx.execute(
                            &format!(
                                "UPDATE reference_files SET content = ?2, revision = revision + 1, updated_at = {NOW}
                                 WHERE id = ?1"
                            ),
                            params![f.id, content],
                        )?;
                        let updated = tx.query_row("SELECT * FROM reference_files WHERE id = ?1", [&f.id], file_from_row)?;
                        record_file(&tx, &updated, ACTION_UPDATE, actor, Some(file_value(&f)), Some(file_value(&updated)), Some(id))?;
                        Some(None)
                    }
                    _ => None,
                }
            }
            OP_UPDATE_SUMMARY => {
                // 提案の生成後に概要が書き換えられていたら反映しない。
                let target = proposal.target_id.as_deref().unwrap_or("");
                let current = tx
                    .query_row("SELECT * FROM scenes WHERE id = ?1", [target], crate::store::scene_from_row)
                    .optional()?;
                match current {
                    Some(sc) if sc.summary == text_of(&proposal.before, "body") => {
                        let summary = clean_summary(text_of(&proposal.after, "body"))?;
                        tx.execute("UPDATE scenes SET summary = ?2 WHERE id = ?1", params![sc.id, summary])?;
                        record_summary(&tx, &proposal.project_id, &sc, &summary, actor, Some(id))?;
                        touch_scene(&tx, &sc.id)?;
                        Some(None)
                    }
                    _ => None,
                }
            }
            OP_INSERT_SCENE => {
                let anchor = proposal.anchor.as_deref().unwrap_or("");
                let current = tx
                    .query_row("SELECT * FROM scenes WHERE id = ?1", [anchor], crate::store::scene_from_row)
                    .optional()?;
                match current {
                    Some(sc) => {
                        let title = clean_title("場面の題名", text_of(&proposal.after, "title"))?;
                        let summary = clean_summary(text_of(&proposal.after, "body"))?;
                        let new_id = new_id();
                        tx.execute(
                            "UPDATE scenes SET position = position + 1 WHERE chapter_id = ?1 AND position > ?2",
                            params![sc.chapter_id, sc.position],
                        )?;
                        tx.execute(
                            &format!(
                                "INSERT INTO scenes (id, chapter_id, title, summary, position, created_at, updated_at)
                                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW}, {NOW})"
                            ),
                            params![new_id, sc.chapter_id, title, summary, sc.position + 1],
                        )?;
                        Some(Some(new_id))
                    }
                    None => None,
                }
            }
            OP_CREATE_FILE => {
                let name = clean_title("ファイル名", text_of(&proposal.after, "name"))?;
                let content = clean_file_content(text_of(&proposal.after, "content").to_string())?;
                let new_id = new_id();
                tx.execute(
                    &format!(
                        "INSERT INTO reference_files (id, project_id, name, content, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
                    ),
                    params![new_id, proposal.project_id, name, content],
                )?;
                let created = tx.query_row("SELECT * FROM reference_files WHERE id = ?1", [&new_id], file_from_row)?;
                record_file(&tx, &created, ACTION_CREATE, actor, None, Some(file_value(&created)), Some(id))?;
                Some(Some(new_id))
            }
            other => return Err(AppError::Internal(format!("unknown proposal op: {other}"))),
        };
        match outcome {
            Some(result_id) => {
                tx.execute(
                    &format!(
                        "UPDATE proposals SET status = ?2, result_id = ?3, final = ?4, decided_at = {NOW} WHERE id = ?1"
                    ),
                    params![id, STATUS_APPROVED, result_id, edited.as_ref().map(Value::to_string)],
                )?;
                touch_project(&tx, &proposal.project_id)?;
            }
            None => {
                tx.execute(
                    &format!("UPDATE proposals SET status = ?2, decided_at = {NOW} WHERE id = ?1"),
                    params![id, STATUS_STALE],
                )?;
            }
        }
        tx.commit()?;
        self.get_proposal(id)
    }

    /// 人間による修正内容を検証し、提案の `after` と同じ形の JSON にする。
    fn validate_edit(&self, proposal: &Proposal, value: Value) -> Result<Value> {
        match proposal.op.as_str() {
            OP_UPDATE_ELEMENT | OP_INSERT_ELEMENT => {
                let content: ElementContent = serde_json::from_value(value)
                    .map_err(|_| AppError::Invalid("修正内容の形式が正しくありません".into()))?;
                let content = clean_content(content)?;
                Ok(serde_json::to_value(content)?)
            }
            OP_UPDATE_FILE | OP_CREATE_FILE => {
                let content = value
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AppError::Invalid("修正内容の形式が正しくありません".into()))?;
                let content = clean_file_content(content.to_string())?;
                let name = match proposal.op.as_str() {
                    OP_CREATE_FILE => clean_title(
                        "ファイル名",
                        value.get("name").and_then(Value::as_str).unwrap_or(text_of(&proposal.after, "name")),
                    )?,
                    _ => text_of(&proposal.after, "name").to_string(),
                };
                Ok(json!({ "name": name, "content": content }))
            }
            OP_UPDATE_SUMMARY | OP_INSERT_SCENE => {
                let body = value
                    .get("body")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AppError::Invalid("修正内容の形式が正しくありません".into()))?;
                let title = match proposal.op.as_str() {
                    OP_INSERT_SCENE => clean_title(
                        "場面の題名",
                        value.get("title").and_then(Value::as_str).unwrap_or(text_of(&proposal.after, "title")),
                    )?,
                    _ => text_of(&proposal.after, "title").to_string(),
                };
                Ok(json!({ "title": title, "body": clean_summary(body)? }))
            }
            _ => Err(AppError::Invalid("削除の提案は修正できません".into())),
        }
    }

    // ---- 設定 ---------------------------------------------------------

    fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, v],
            )?,
            None => self.conn.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    fn int_setting(&self, key: &str, default: i64) -> Result<i64> {
        Ok(self
            .setting(key)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(default))
    }

    /// 保存されている API キー。サーバー内部でのみ使い、API 応答やログには出さない。
    pub fn stored_api_key(&self) -> Result<Option<String>> {
        Ok(self.setting(KEY_API_KEY)?.filter(|k| !k.is_empty()))
    }

    pub fn llm_model(&self) -> Result<String> {
        Ok(self
            .setting(KEY_MODEL)?
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()))
    }

    pub fn llm_settings(&self, env_key_present: bool) -> Result<LlmSettings> {
        Ok(LlmSettings {
            provider: "gemini",
            model: self.llm_model()?,
            has_api_key: env_key_present || self.stored_api_key()?.is_some(),
            monthly_token_limit: self.int_setting(KEY_TOKEN_LIMIT, DEFAULT_TOKEN_LIMIT)?,
            warn_percent: self.int_setting(KEY_WARN_PERCENT, DEFAULT_WARN_PERCENT)?,
        })
    }

    /// `api_key`: `None` は変更なし、空文字は削除。
    pub fn update_llm_settings(
        &mut self,
        api_key: Option<&str>,
        model: Option<&str>,
        monthly_token_limit: Option<i64>,
        warn_percent: Option<i64>,
    ) -> Result<()> {
        if let Some(key) = api_key {
            let key = key.trim();
            if key.chars().any(|c| c.is_control() || c.is_whitespace()) || key.len() > 500 {
                return Err(AppError::Invalid("API キーの形式が正しくありません".into()));
            }
            self.set_setting(KEY_API_KEY, if key.is_empty() { None } else { Some(key) })?;
        }
        if let Some(model) = model {
            let model = model.trim();
            let ok = !model.is_empty()
                && model.len() <= 100
                && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
            if !ok {
                return Err(AppError::Invalid(
                    "モデル名は英数字と - . _ で指定してください（例: gemini-flash-latest）".into(),
                ));
            }
            self.set_setting(KEY_MODEL, Some(model))?;
        }
        if let Some(percent) = warn_percent {
            if !(1..=100).contains(&percent) {
                return Err(AppError::Invalid("警告のしきい値は 1〜100 % で指定してください".into()));
            }
            self.set_setting(KEY_WARN_PERCENT, Some(&percent.to_string()))?;
        }
        if let Some(limit) = monthly_token_limit {
            if limit < 0 {
                return Err(AppError::Invalid("上限は 0 以上で指定してください（0 は無制限）".into()));
            }
            self.set_setting(KEY_TOKEN_LIMIT, Some(&limit.to_string()))?;
            // 上限の変更はユーザーの明示操作なので、新しい上限で停止状態を判定し直す。
            self.set_setting(KEY_OVERRIDE_MONTH, None)?;
            let (_, total) = self.month_total()?;
            let stopped = limit > 0 && total >= limit;
            self.set_setting(KEY_STOPPED, if stopped { Some("1") } else { None })?;
        }
        Ok(())
    }

    // ---- 使用量 -------------------------------------------------------

    fn month_total(&self) -> Result<(String, i64)> {
        Ok(self.conn.query_row(
            "SELECT strftime('%Y-%m','now'),
                    COALESCE((SELECT SUM(input_tokens + output_tokens) FROM llm_usage
                              WHERE substr(created_at, 1, 7) = strftime('%Y-%m','now')), 0)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    fn is_stopped(&self) -> Result<bool> {
        Ok(self.setting(KEY_STOPPED)?.is_some())
    }

    /// LLM を呼び出してよいか確認する。停止中なら `LlmStopped`。
    pub fn ensure_llm_allowed(&self) -> Result<()> {
        if self.is_stopped()? {
            return Err(AppError::LlmStopped);
        }
        Ok(())
    }

    /// 1 回の呼び出しの使用量を記録し、上限に達していれば停止状態にする。
    pub fn record_usage(
        &mut self,
        provider: &str,
        model: &str,
        purpose: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO llm_usage (provider, model, purpose, input_tokens, output_tokens, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
            ),
            params![provider, model, purpose, input_tokens.max(0), output_tokens.max(0)],
        )?;
        let limit = self.int_setting(KEY_TOKEN_LIMIT, DEFAULT_TOKEN_LIMIT)?;
        let (month, total) = self.month_total()?;
        let overridden = self.setting(KEY_OVERRIDE_MONTH)?.as_deref() == Some(month.as_str());
        if limit > 0 && total >= limit && !overridden {
            self.set_setting(KEY_STOPPED, Some("1"))?;
        }
        Ok(())
    }

    /// ユーザーの明示操作で停止を解除する。当月は上限を超えていても自動停止しない。
    pub fn resume_llm(&mut self) -> Result<()> {
        let (month, _) = self.month_total()?;
        self.set_setting(KEY_STOPPED, None)?;
        self.set_setting(KEY_OVERRIDE_MONTH, Some(&month))
    }

    pub fn usage_status(&self) -> Result<UsageStatus> {
        let (month, total) = self.month_total()?;
        let mut stmt = self.conn.prepare(
            "SELECT provider, model, COUNT(*), SUM(input_tokens), SUM(output_tokens) FROM llm_usage
             WHERE substr(created_at, 1, 7) = ?1 GROUP BY provider, model ORDER BY provider, model",
        )?;
        let by_model: Vec<ModelUsage> = stmt
            .query_map([&month], |r| {
                Ok(ModelUsage {
                    provider: r.get(0)?,
                    model: r.get(1)?,
                    requests: r.get(2)?,
                    input_tokens: r.get(3)?,
                    output_tokens: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let limit = self.int_setting(KEY_TOKEN_LIMIT, DEFAULT_TOKEN_LIMIT)?;
        let warn_percent = self.int_setting(KEY_WARN_PERCENT, DEFAULT_WARN_PERCENT)?;
        let state = if self.is_stopped()? {
            "stopped"
        } else if limit > 0 && total.saturating_mul(100) >= limit.saturating_mul(warn_percent) {
            "warning"
        } else {
            "ok"
        };
        Ok(UsageStatus {
            month,
            requests: by_model.iter().map(|m| m.requests).sum(),
            input_tokens: by_model.iter().map(|m| m.input_tokens).sum(),
            output_tokens: by_model.iter().map(|m| m.output_tokens).sum(),
            total_tokens: total,
            monthly_token_limit: limit,
            warn_percent,
            state,
            by_model,
        })
    }
}

/// 挿入提案の反映位置を決める。基準の要素が失われていれば `None`。
///
/// 同じ応答に含まれる同じ位置への挿入提案は、承認の順序にかかわらず提案時の並びを保つ。
fn insert_position(
    tx: &rusqlite::Transaction,
    proposal: &Proposal,
    scene_id: &str,
) -> Result<Option<i64>> {
    let scene_exists: bool = tx
        .query_row("SELECT 1 FROM scenes WHERE id = ?1", [scene_id], |_| Ok(true))
        .optional()?
        .unwrap_or(false);
    if !scene_exists {
        return Ok(None);
    }
    let anchor = proposal.anchor.as_deref().unwrap_or(ANCHOR_END);
    let sibling = |later: bool| -> Result<Option<i64>> {
        let (cmp, agg) = if later { (">", "MIN") } else { ("<", "MAX") };
        Ok(tx.query_row(
            &format!(
                "SELECT {agg}(e.position) FROM proposals p JOIN elements e ON e.id = p.result_id
                 WHERE p.message_id = ?1 AND p.op = ?2 AND p.anchor IS ?3 AND p.status = ?4
                   AND p.ordinal {cmp} ?5 AND e.scene_id = ?6"
            ),
            params![
                proposal.message_id,
                OP_INSERT_ELEMENT,
                proposal.anchor,
                STATUS_APPROVED,
                proposal.ordinal,
                scene_id
            ],
            |r| r.get(0),
        )?)
    };
    if let Some(position) = sibling(true)? {
        return Ok(Some(position));
    }
    if let Some(position) = sibling(false)? {
        return Ok(Some(position + 1));
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM elements WHERE scene_id = ?1",
        [scene_id],
        |r| r.get(0),
    )?;
    Ok(match anchor {
        ANCHOR_START => Some(0),
        ANCHOR_END => Some(count),
        element_id => tx
            .query_row(
                "SELECT position + 1 FROM elements WHERE id = ?1 AND scene_id = ?2",
                params![element_id, scene_id],
                |r| r.get(0),
            )
            .optional()?,
    })
}

/// 要素の内容を提案用の JSON にする。
pub fn element_json(e: &Element) -> Value {
    serde_json::to_value(content_of(e)).unwrap_or(Value::Null)
}

/// 参照ファイルの変更を履歴に記録する。
fn record_file(
    tx: &rusqlite::Transaction,
    file: &ReferenceFile,
    action: &str,
    actor: &str,
    before: Option<Value>,
    after: Option<Value>,
    proposal_id: Option<&str>,
) -> Result<()> {
    hist::record(
        tx,
        hist::Change {
            project_id: &file.project_id,
            scene_id: None,
            target_type: TARGET_FILE,
            target_id: &file.id,
            action,
            actor,
            before,
            after,
            proposal_id,
        },
    )
}
