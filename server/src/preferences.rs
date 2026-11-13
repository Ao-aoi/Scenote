//! 執筆嗜好の学習。
//!
//! AI の提案に対するユーザーの操作（承認・修正して採用・却下と理由）を LLM に分析させ、
//! 確認可能な「嗜好ルール」として保存する。分析とプロファイルへの適用は分けて扱い、
//! 自動更新が OFF（初期設定）の場合は、更新案をユーザーが承認したときだけ適用する。
//!
//! 少数の操作から普遍的な好みと断定しないよう、確信度は根拠の件数からプログラムが決め、
//! 分析で作られるルールは常にプロジェクト固有とする（共通プロファイルへはユーザーが明示的に移す）。

use std::sync::Mutex;

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;

use crate::assistant::extract_json;
use crate::error::{AppError, Result};
use crate::llm::{LlmMessage, LlmProvider, LlmRequest, Role};
use crate::store::{new_id, Store, NOW};

pub const CATEGORIES: [&str; 9] = [
    "style", "vocabulary", "length", "avoid", "balance", "tempo", "character", "density", "other",
];

pub const SOURCE_USER: &str = "user";
pub const SOURCE_AI: &str = "ai";
pub const SOURCE_COPIED: &str = "copied";

const KEY_AUTO_UPDATE: &str = "preferences.auto_update";
const KEY_INCLUDE_DIRECT: &str = "preferences.include_direct_edits";

/// 1 回の分析で扱う操作の最大件数。
const MAX_DECISIONS: usize = 40;
/// プロンプトに含める嗜好ルールの最大件数（共通・プロジェクトそれぞれ）。
const MAX_RULES_IN_PROMPT: usize = 30;
const MAX_RULE_CHARS: usize = 300;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rule {
    pub id: String,
    /// `None` は全プロジェクト共通。
    pub project_id: Option<String>,
    pub text: String,
    pub category: String,
    pub confidence: f64,
    /// 根拠となった提案の ID。
    pub evidence: Vec<String>,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub id: String,
    pub project_id: String,
    /// add / update / remove
    pub op: String,
    pub rule_id: Option<String>,
    /// update / remove の対象ルールの、提案時点の文面。
    pub rule_text: Option<String>,
    pub text: String,
    pub category: String,
    pub confidence: f64,
    pub evidence: Vec<String>,
    pub reason: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreferenceSettings {
    pub auto_update: bool,
    pub include_direct_edits: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub global_rules: Vec<Rule>,
    pub project_rules: Vec<Rule>,
    pub suggestions: Vec<Suggestion>,
    pub settings: PreferenceSettings,
    /// まだ分析していない、承認・却下済みの提案の件数。
    pub unanalyzed: i64,
}

#[derive(Debug, Serialize)]
pub struct AnalysisResult {
    pub summary: String,
    /// 自動更新 ON のときは適用済み、OFF のときは承認待ちの更新案。
    pub suggestions: Vec<Suggestion>,
    pub applied: bool,
    pub warnings: Vec<String>,
    pub profile: Profile,
}

/// 根拠の件数から確信度を決める。1 件では 0.35 にとどめ、件数が増えるほど上がる（上限 0.9）。
pub fn confidence_for(evidence_count: usize) -> f64 {
    let c = 0.2 + 0.15 * evidence_count as f64;
    (c.min(0.9) * 100.0).round() / 100.0
}

fn rule_from_row(row: &Row) -> rusqlite::Result<Rule> {
    let evidence: String = row.get("evidence")?;
    Ok(Rule {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        text: row.get("text")?,
        category: row.get("category")?,
        confidence: row.get("confidence")?,
        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
        source: row.get("source")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn clean_rule_text(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::Invalid("ルールの内容を入力してください".into()));
    }
    if text.chars().count() > MAX_RULE_CHARS {
        return Err(AppError::Invalid(format!("ルールは{MAX_RULE_CHARS}文字以内で入力してください")));
    }
    Ok(text.replace(['\r', '\n'], " "))
}

fn clean_category(category: &str) -> Result<String> {
    if CATEGORIES.contains(&category) {
        Ok(category.to_string())
    } else {
        Err(AppError::Invalid(format!("未知の分類です: {category}")))
    }
}

fn clean_confidence(c: f64) -> Result<f64> {
    if !(0.0..=1.0).contains(&c) || c.is_nan() {
        return Err(AppError::Invalid("確信度は 0〜1 で指定してください".into()));
    }
    Ok(c)
}

impl Store {
    fn bool_setting(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0))
            .optional()?
            .is_some_and(|v| v == "1"))
    }

    fn set_bool_setting(&self, key: &str, value: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, if value { "1" } else { "0" }],
        )?;
        Ok(())
    }

    pub fn preference_settings(&self) -> Result<PreferenceSettings> {
        Ok(PreferenceSettings {
            auto_update: self.bool_setting(KEY_AUTO_UPDATE)?,
            include_direct_edits: self.bool_setting(KEY_INCLUDE_DIRECT)?,
        })
    }

    pub fn update_preference_settings(&mut self, auto_update: Option<bool>, include_direct_edits: Option<bool>) -> Result<()> {
        if let Some(v) = auto_update {
            self.set_bool_setting(KEY_AUTO_UPDATE, v)?;
        }
        if let Some(v) = include_direct_edits {
            self.set_bool_setting(KEY_INCLUDE_DIRECT, v)?;
        }
        Ok(())
    }

    /// `project_id` が `None` なら共通ルール。
    pub fn list_rules(&self, project_id: Option<&str>) -> Result<Vec<Rule>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM preference_rules WHERE project_id IS ?1 ORDER BY confidence DESC, created_at",
        )?;
        let rows = stmt.query_map([project_id], rule_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_rule(&self, id: &str) -> Result<Rule> {
        self.conn
            .query_row("SELECT * FROM preference_rules WHERE id = ?1", [id], rule_from_row)
            .optional()?
            .ok_or(AppError::NotFound("ルール"))
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_rule(
        &self,
        project_id: Option<&str>,
        text: &str,
        category: &str,
        confidence: f64,
        evidence: &[String],
        source: &str,
    ) -> Result<String> {
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO preference_rules (id, project_id, text, category, confidence, evidence, source, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {NOW}, {NOW})"
            ),
            params![id, project_id, text, category, confidence, serde_json::to_string(evidence)?, source],
        )?;
        Ok(id)
    }

    /// ユーザーがルールを手動で追加する。
    pub fn create_rule(&mut self, project_id: Option<&str>, text: &str, category: &str) -> Result<Rule> {
        if let Some(p) = project_id {
            self.get_project(p)?;
        }
        let id = self.insert_rule(project_id, &clean_rule_text(text)?, &clean_category(category)?, 1.0, &[], SOURCE_USER)?;
        self.get_rule(&id)
    }

    pub fn update_rule(
        &mut self,
        id: &str,
        text: Option<&str>,
        category: Option<&str>,
        confidence: Option<f64>,
    ) -> Result<Rule> {
        let rule = self.get_rule(id)?;
        let text = match text {
            Some(t) => clean_rule_text(t)?,
            None => rule.text,
        };
        let category = match category {
            Some(c) => clean_category(c)?,
            None => rule.category,
        };
        let confidence = match confidence {
            Some(c) => clean_confidence(c)?,
            None => rule.confidence,
        };
        self.conn.execute(
            &format!(
                "UPDATE preference_rules SET text = ?2, category = ?3, confidence = ?4, updated_at = {NOW} WHERE id = ?1"
            ),
            params![id, text, category, confidence],
        )?;
        self.get_rule(id)
    }

    pub fn delete_rule(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM preference_rules WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("ルール"));
        }
        Ok(())
    }

    /// ルールを別の範囲（共通、または別のプロジェクト）へ複製する。同じ文面のルールがあれば複製しない。
    pub fn copy_rules(&mut self, rule_ids: &[String], target_project: Option<&str>) -> Result<usize> {
        if let Some(p) = target_project {
            self.get_project(p)?;
        }
        let existing: Vec<String> = self.list_rules(target_project)?.into_iter().map(|r| r.text).collect();
        let mut copied = 0;
        for id in rule_ids {
            let rule = self.get_rule(id)?;
            if rule.project_id.as_deref() == target_project || existing.contains(&rule.text) {
                continue;
            }
            self.insert_rule(target_project, &rule.text, &rule.category, rule.confidence, &rule.evidence, SOURCE_COPIED)?;
            copied += 1;
        }
        Ok(copied)
    }

    /// 別のプロジェクトのルールをすべて取り込む（継承・統合）。
    pub fn import_project_rules(&mut self, source_project: &str, target_project: &str) -> Result<usize> {
        self.get_project(source_project)?;
        if source_project == target_project {
            return Err(AppError::Invalid("同じプロジェクトからは取り込めません".into()));
        }
        let ids: Vec<String> = self.list_rules(Some(source_project))?.into_iter().map(|r| r.id).collect();
        self.copy_rules(&ids, Some(target_project))
    }

    fn suggestion_from_row(&self, row: &Row) -> rusqlite::Result<Suggestion> {
        let evidence: String = row.get("evidence")?;
        let rule_id: Option<String> = row.get("rule_id")?;
        Ok(Suggestion {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            op: row.get("op")?,
            rule_text: None,
            rule_id,
            text: row.get("text")?,
            category: row.get("category")?,
            confidence: row.get("confidence")?,
            evidence: serde_json::from_str(&evidence).unwrap_or_default(),
            reason: row.get("reason")?,
            status: row.get("status")?,
            created_at: row.get("created_at")?,
        })
    }

    fn fill_rule_text(&self, mut s: Suggestion) -> Result<Suggestion> {
        if let Some(rule_id) = &s.rule_id {
            s.rule_text = self
                .conn
                .query_row("SELECT text FROM preference_rules WHERE id = ?1", [rule_id], |r| r.get(0))
                .optional()?;
        }
        Ok(s)
    }

    pub fn list_suggestions(&self, project_id: &str) -> Result<Vec<Suggestion>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM preference_suggestions WHERE project_id = ?1 AND status = 'pending' ORDER BY created_at, rowid",
        )?;
        let rows: Vec<Suggestion> = stmt
            .query_map([project_id], |r| self.suggestion_from_row(r))?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter().map(|s| self.fill_rule_text(s)).collect()
    }

    fn get_suggestion(&self, id: &str) -> Result<Suggestion> {
        let s = self
            .conn
            .query_row("SELECT * FROM preference_suggestions WHERE id = ?1", [id], |r| self.suggestion_from_row(r))
            .optional()?
            .ok_or(AppError::NotFound("更新案"))?;
        self.fill_rule_text(s)
    }

    /// 更新案をプロファイルへ適用する。対象のルールが削除済みなら `stale`。
    pub fn approve_suggestion(&mut self, id: &str) -> Result<Suggestion> {
        let s = self.get_suggestion(id)?;
        if s.status != "pending" {
            return Err(AppError::Invalid("この更新案はすでに処理されています".into()));
        }
        let target = match &s.rule_id {
            Some(rule_id) => self.get_rule(rule_id).ok().filter(|r| r.project_id.as_deref() == Some(s.project_id.as_str())),
            None => None,
        };
        let status = match (s.op.as_str(), target) {
            ("add", _) => {
                self.insert_rule(Some(&s.project_id), &s.text, &s.category, s.confidence, &s.evidence, SOURCE_AI)?;
                "approved"
            }
            ("update", Some(rule)) => {
                let mut evidence = rule.evidence.clone();
                for e in &s.evidence {
                    if !evidence.contains(e) {
                        evidence.push(e.clone());
                    }
                }
                self.conn.execute(
                    &format!(
                        "UPDATE preference_rules SET text = ?2, category = ?3, confidence = ?4, evidence = ?5, updated_at = {NOW}
                         WHERE id = ?1"
                    ),
                    params![rule.id, s.text, s.category, s.confidence, serde_json::to_string(&evidence)?],
                )?;
                "approved"
            }
            ("remove", Some(rule)) => {
                self.conn.execute("DELETE FROM preference_rules WHERE id = ?1", [&rule.id])?;
                "approved"
            }
            _ => "stale",
        };
        self.conn.execute(
            "UPDATE preference_suggestions SET status = ?2 WHERE id = ?1",
            params![id, status],
        )?;
        self.get_suggestion(id)
    }

    pub fn reject_suggestion(&mut self, id: &str) -> Result<Suggestion> {
        let s = self.get_suggestion(id)?;
        if s.status != "pending" {
            return Err(AppError::Invalid("この更新案はすでに処理されています".into()));
        }
        self.conn.execute("UPDATE preference_suggestions SET status = 'rejected' WHERE id = ?1", [id])?;
        self.get_suggestion(id)
    }

    fn unanalyzed_count(&self, project_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM proposals WHERE project_id = ?1 AND status IN ('approved', 'rejected') AND analyzed_at IS NULL",
            [project_id],
            |r| r.get(0),
        )?)
    }

    pub fn profile(&self, project_id: &str) -> Result<Profile> {
        self.get_project(project_id)?;
        Ok(Profile {
            global_rules: self.list_rules(None)?,
            project_rules: self.list_rules(Some(project_id))?,
            suggestions: self.list_suggestions(project_id)?,
            settings: self.preference_settings()?,
            unanalyzed: self.unanalyzed_count(project_id)?,
        })
    }

    /// 依頼のプロンプトに含める嗜好の説明。ルールがなければ空文字。
    pub fn preference_prompt(&self, project_id: &str) -> Result<String> {
        let section = |title: &str, rules: &[Rule]| -> String {
            if rules.is_empty() {
                return String::new();
            }
            let mut out = format!("## {title}\n");
            for r in rules.iter().take(MAX_RULES_IN_PROMPT) {
                let strength = if r.confidence >= 0.7 {
                    "強い傾向"
                } else if r.confidence >= 0.45 {
                    "傾向"
                } else {
                    "弱い傾向（参考程度）"
                };
                out.push_str(&format!("- {}（{strength}）\n", r.text));
            }
            out
        };
        let project_rules = self.list_rules(Some(project_id))?;
        // 共通へ複製したルールなど、プロジェクトのルールと同じ文面のものは重ねて伝えない。
        let global_rules: Vec<Rule> = self
            .list_rules(None)?
            .into_iter()
            .filter(|g| !project_rules.iter().any(|p| p.text == g.text))
            .collect();
        let global = section("共通の嗜好", &global_rules);
        let project = section("このプロジェクトでの嗜好", &project_rules);
        if global.is_empty() && project.is_empty() {
            return Ok(String::new());
        }
        Ok(format!(
            "# ユーザーの執筆嗜好\n過去の提案への反応から学習した傾向です。共通の嗜好よりプロジェクトの嗜好を、\
             それよりチャットの追加指示とユーザーの依頼を優先してください。\n{global}{project}"
        ))
    }
}

/// 分析に渡す 1 件の操作。
struct Decision {
    proposal_id: Option<String>,
    text: String,
}

const ANALYSIS_PROMPT: &str = r#"あなたはゲームシナリオ制作アプリの補助機能です。ユーザーが AI の提案をどう扱ったか（承認・修正して採用・却下）から、ユーザーの執筆上の好みを読み取り、嗜好プロファイルの更新案を作ります。

## 原則
- 少数の操作から普遍的な好みと断定しないでください。1 件だけの却下を「禁止事項」にしないでください。
- 根拠のない推測でルールを作らないでください。各案には根拠となる操作の参照名（d1 など）を必ず挙げます。
- 既存のルールと重複する案は作らず、補強する場合は update を使います。矛盾する根拠が出た場合は update で表現を弱めるか、remove を提案します。
- 修正して採用した操作では、AI の提案と最終的な内容の違いが最も重要な手がかりです。
- ルールは、文章生成への具体的な指示として使える短い一文にします（例: 「台詞の語尾に『〜わ』を多用しない」）。
- 分類（category）は style（文体）, vocabulary（語彙）, length（文章の長さ）, avoid（避けたい表現）, balance（台詞と地の文の配分）, tempo（展開のテンポ）, character（キャラクターの話し方）, density（演出・説明の密度）, other のいずれかです。
- 読み取れる傾向がなければ changes は空にします。

## 出力形式
次の JSON オブジェクトだけを出力します。
{
  "summary": "分析の要約（日本語、2〜3 文）",
  "changes": [
    {"op": "add", "text": "ルール", "category": "style", "evidence": ["d1", "d3"], "reason": "理由"},
    {"op": "update", "target": "r2", "text": "更新後のルール", "category": "tempo", "evidence": ["d4"], "reason": "理由"},
    {"op": "remove", "target": "r3", "evidence": ["d5"], "reason": "理由"}
  ]
}"#;

fn describe(value: &Option<Value>) -> String {
    match value {
        None => "（なし）".to_string(),
        Some(v) => {
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
            if v.get("kind").is_some() {
                let speaker = text("speaker");
                let head = if speaker.is_empty() { text("kind").to_string() } else { format!("{}・{speaker}", text("kind")) };
                format!("[{head}] {}", text("body"))
            } else {
                format!("[ファイル「{}」] {}", text("name"), text("content"))
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 分析の対象となる操作を集める。
fn collect_decisions(store: &Store, project_id: &str, include_direct: bool) -> Result<Vec<Decision>> {
    let mut stmt = store.conn.prepare(
        "SELECT id, op, before, after, final, status, reject_reason, reason FROM proposals
         WHERE project_id = ?1 AND status IN ('approved', 'rejected') AND analyzed_at IS NULL
         ORDER BY decided_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project_id, MAX_DECISIONS as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    let parse = |t: Option<String>| t.and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let mut out = Vec::new();
    for row in rows {
        let (id, op, before, after, final_content, status, reject_reason, reason) = row?;
        let (before, after, final_content) = (parse(before), parse(after), parse(final_content));
        let outcome = match (status.as_str(), &final_content) {
            ("rejected", _) => format!(
                "却下{}",
                if reject_reason.is_empty() { String::new() } else { format!("（理由: {reject_reason}）") }
            ),
            (_, Some(_)) => "修正して採用".to_string(),
            _ => "そのまま承認".to_string(),
        };
        let mut text = format!(
            "操作: {op} / 結果: {outcome}\nAI の意図: {}\n変更前: {}\nAI の提案: {}",
            truncate(&reason, 200),
            truncate(&describe(&before), 600),
            truncate(&describe(&after), 600)
        );
        if final_content.is_some() {
            text.push_str(&format!("\nユーザーが修正した最終内容: {}", truncate(&describe(&final_content), 600)));
        }
        out.push(Decision { proposal_id: Some(id), text });
    }
    if include_direct {
        // 人間が直接編集した要素（AI 提案と無関係なもの）も参考にする（設定で有効な場合のみ）。
        let mut stmt = store.conn.prepare(
            "SELECT before, after FROM history WHERE project_id = ?1 AND target_type = 'element'
               AND actor = 'human' AND action IN ('create', 'update') AND proposal_id IS NULL
             ORDER BY seq DESC LIMIT 15",
        )?;
        let rows = stmt.query_map([project_id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?;
        for row in rows {
            let (before, after) = row?;
            out.push(Decision {
                proposal_id: None,
                text: format!(
                    "操作: ユーザーによる直接の編集\n変更前: {}\n変更後: {}",
                    truncate(&describe(&parse(before)), 600),
                    truncate(&describe(&parse(after)), 600)
                ),
            });
        }
    }
    Ok(out)
}

/// 検証済みの更新案（保存前）。
#[derive(Debug, Clone, PartialEq)]
struct PlannedChange {
    op: &'static str,
    rule_id: Option<String>,
    text: String,
    category: String,
    evidence: Vec<String>,
    reason: String,
    confidence: f64,
}

/// 分析結果を検証し、更新案にする。不正な案は除外して注意に残す。
fn interpret_analysis(
    text: &str,
    decisions: &[Decision],
    rules: &[Rule],
) -> std::result::Result<(String, Vec<PlannedChange>, Vec<String>), String> {
    let value = extract_json(text).ok_or("分析結果を解釈できませんでした（JSON ではありません）")?;
    let summary = value.get("summary").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let mut warnings = Vec::new();
    let mut out = Vec::new();
    let changes = value.get("changes").and_then(Value::as_array).cloned().unwrap_or_default();
    for (i, c) in changes.iter().enumerate() {
        let n = i + 1;
        let get = |k: &str| c.get(k).and_then(Value::as_str).map(str::trim);
        let op = get("op").unwrap_or("");
        // 根拠は提案に由来するもの（d 参照）のみを記録する。直接編集は根拠の件数にだけ数える。
        let mut evidence_count = 0;
        let mut evidence = Vec::new();
        for r in c.get("evidence").and_then(Value::as_array).into_iter().flatten() {
            let Some(index) = r.as_str().and_then(|s| s.strip_prefix('d')).and_then(|s| s.parse::<usize>().ok()) else {
                continue;
            };
            if let Some(d) = index.checked_sub(1).and_then(|i| decisions.get(i)) {
                evidence_count += 1;
                if let Some(id) = &d.proposal_id {
                    if !evidence.contains(id) {
                        evidence.push(id.clone());
                    }
                }
            }
        }
        if evidence_count == 0 {
            warnings.push(format!("案 {n} は根拠となる操作が示されていないため除外しました"));
            continue;
        }
        let target = match get("target") {
            Some(t) => match t.strip_prefix('r').and_then(|s| s.parse::<usize>().ok()).and_then(|i| i.checked_sub(1)).and_then(|i| rules.get(i)) {
                Some(rule) => Some(rule),
                None => {
                    warnings.push(format!("案 {n} は存在しないルールを指しているため除外しました"));
                    continue;
                }
            },
            None => None,
        };
        let reason = get("reason").unwrap_or("").to_string();
        match op {
            "add" | "update" => {
                let Some(text) = get("text").and_then(|t| clean_rule_text(t).ok()) else {
                    warnings.push(format!("案 {n} はルールの文面が不正なため除外しました"));
                    continue;
                };
                let category = get("category").filter(|c| CATEGORIES.contains(c)).unwrap_or("other").to_string();
                if op == "update" {
                    let Some(rule) = target else {
                        warnings.push(format!("案 {n} は更新対象のルールがないため除外しました"));
                        continue;
                    };
                    let total = rule.evidence.len().max(1) + evidence_count;
                    out.push(PlannedChange { op: "update", rule_id: Some(rule.id.clone()), text, category, evidence, reason, confidence: confidence_for(total) });
                } else {
                    if rules.iter().any(|r| r.text == text) {
                        warnings.push(format!("案 {n} は既存のルールと同じため除外しました"));
                        continue;
                    }
                    out.push(PlannedChange { op: "add", rule_id: None, text, category, evidence, reason, confidence: confidence_for(evidence_count) });
                }
            }
            "remove" => {
                let Some(rule) = target else {
                    warnings.push(format!("案 {n} は削除対象のルールがないため除外しました"));
                    continue;
                };
                out.push(PlannedChange { op: "remove", rule_id: Some(rule.id.clone()), text: rule.text.clone(), category: rule.category.clone(), evidence, reason, confidence: rule.confidence });
            }
            other => warnings.push(format!("案 {n} は未対応の操作（{other}）のため除外しました")),
        }
    }
    Ok((summary, out, warnings))
}

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// 承認・却下・修正の操作を分析し、嗜好プロファイルの更新案を作る。
pub async fn analyze(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    project_id: &str,
) -> Result<AnalysisResult> {
    let (decisions, rules, global_rules, model, api_key) = {
        let s = lock(store);
        s.get_project(project_id)?;
        s.ensure_llm_allowed()?;
        let api_key = match env_api_key {
            Some(k) => Some(k.to_string()),
            None => s.stored_api_key()?,
        }
        .ok_or_else(|| AppError::Invalid("API キーが設定されていません。「AI 設定」から登録してください".into()))?;
        let settings = s.preference_settings()?;
        let decisions = collect_decisions(&s, project_id, settings.include_direct_edits)?;
        if !decisions.iter().any(|d| d.proposal_id.is_some()) {
            return Err(AppError::Invalid(
                "分析できる新しい操作がありません。AI の提案を承認・修正・却下すると分析できるようになります".into(),
            ));
        }
        (decisions, s.list_rules(Some(project_id))?, s.list_rules(None)?, s.llm_model()?, api_key)
    };

    let mut prompt = String::from("# 既存のルール（このプロジェクト）\n");
    if rules.is_empty() {
        prompt.push_str("（なし）\n");
    }
    for (i, r) in rules.iter().enumerate() {
        prompt.push_str(&format!("- [r{}] {}（分類: {}、確信度: {:.2}）\n", i + 1, r.text, r.category, r.confidence));
    }
    if !global_rules.is_empty() {
        prompt.push_str("\n# 共通のルール（参考。変更対象にはしない）\n");
        for r in &global_rules {
            prompt.push_str(&format!("- {}\n", r.text));
        }
    }
    prompt.push_str("\n# ユーザーの操作\n");
    for (i, d) in decisions.iter().enumerate() {
        prompt.push_str(&format!("## [d{}]\n{}\n\n", i + 1, d.text));
    }
    let request = LlmRequest {
        system: ANALYSIS_PROMPT.to_string(),
        messages: vec![LlmMessage { role: Role::User, text: prompt }],
    };
    let response = provider
        .generate(&model, &api_key, &request)
        .await
        .map_err(|e| AppError::Llm(e.0))?;

    let mut s = lock(store);
    s.record_usage(provider.name(), &model, "preferences", response.input_tokens, response.output_tokens)?;
    let (summary, changes, warnings) =
        interpret_analysis(&response.text, &decisions, &rules).map_err(AppError::Llm)?;
    let auto = s.preference_settings()?.auto_update;
    let tx = s.conn.transaction()?;
    let mut ids = Vec::new();
    for c in &changes {
        let id = new_id();
        tx.execute(
            &format!(
                "INSERT INTO preference_suggestions (id, project_id, op, rule_id, text, category, confidence, evidence, reason, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, {NOW})"
            ),
            params![
                id,
                project_id,
                c.op,
                c.rule_id,
                c.text,
                c.category,
                c.confidence,
                serde_json::to_string(&c.evidence)?,
                c.reason
            ],
        )?;
        ids.push(id);
    }
    // 分析に使った提案は、次回以降の分析の対象から外す。
    for d in &decisions {
        if let Some(id) = &d.proposal_id {
            tx.execute(&format!("UPDATE proposals SET analyzed_at = {NOW} WHERE id = ?1"), [id])?;
        }
    }
    tx.commit()?;
    let mut suggestions = Vec::new();
    for id in &ids {
        suggestions.push(if auto { s.approve_suggestion(id)? } else { s.get_suggestion(id)? });
    }
    Ok(AnalysisResult {
        summary,
        suggestions,
        applied: auto,
        warnings,
        profile: s.profile(project_id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, text: &str, evidence: usize) -> Rule {
        Rule {
            id: id.into(),
            project_id: Some("p".into()),
            text: text.into(),
            category: "style".into(),
            confidence: confidence_for(evidence),
            evidence: (0..evidence).map(|i| format!("old{i}")).collect(),
            source: SOURCE_AI.into(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn analysis_output_is_validated() {
        let decisions = vec![
            Decision { proposal_id: Some("p1".into()), text: String::new() },
            Decision { proposal_id: Some("p2".into()), text: String::new() },
            Decision { proposal_id: None, text: String::new() },
        ];
        let rules = vec![rule("r-a", "地の文は短くする", 2), rule("r-b", "敬語を避ける", 1)];
        let text = serde_json::json!({ "summary": "要約", "changes": [
            { "op": "add", "text": "台詞の語尾を伸ばさない", "category": "character", "evidence": ["d1", "d2", "d1"], "reason": "2 件で修正" },
            { "op": "add", "text": "根拠なし", "category": "style", "evidence": [] },
            { "op": "add", "text": "存在しない根拠", "category": "style", "evidence": ["d9"] },
            { "op": "add", "text": "地の文は短くする", "category": "length", "evidence": ["d1"] },
            { "op": "update", "target": "r1", "text": "地の文は一文を短くする", "category": "bogus", "evidence": ["d3"] },
            { "op": "update", "target": "r7", "text": "x", "evidence": ["d1"] },
            { "op": "remove", "target": "r2", "evidence": ["d2"], "reason": "矛盾" },
            { "op": "explode", "evidence": ["d1"] },
            { "op": "add", "text": "", "evidence": ["d1"] }
        ] }).to_string();
        let (summary, changes, warnings) = interpret_analysis(&format!("```json\n{text}\n```"), &decisions, &rules).unwrap();
        assert_eq!(summary, "要約");
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].op, "add");
        assert_eq!(changes[0].evidence, ["p1", "p2"]);
        assert_eq!(changes[0].confidence, confidence_for(3));
        // 直接編集（proposal_id なし）は根拠の件数には数えるが、ID としては残さない。
        assert_eq!(changes[1].op, "update");
        assert_eq!(changes[1].rule_id.as_deref(), Some("r-a"));
        assert_eq!(changes[1].category, "other");
        assert!(changes[1].evidence.is_empty());
        assert_eq!(changes[1].confidence, confidence_for(3));
        assert_eq!(changes[2].op, "remove");
        assert_eq!(changes[2].text, "敬語を避ける");
        assert_eq!(warnings.len(), 6);
        assert!(interpret_analysis("ごめんなさい", &decisions, &rules).is_err());
    }

    #[test]
    fn confidence_grows_with_evidence_but_stays_bounded() {
        assert_eq!(confidence_for(1), 0.35);
        assert_eq!(confidence_for(2), 0.5);
        assert!(confidence_for(3) > confidence_for(2));
        assert_eq!(confidence_for(100), 0.9);
    }
}
