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
use serde::{Deserialize, Serialize};
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
    /// 分析結果（更新案）を承認なしでプロファイルに適用する。
    pub auto_update: bool,
    pub include_direct_edits: bool,
    /// 条件を満たしたら、分析そのものを自動で実行する（#10）。
    pub auto_analyze: bool,
}

/// 分析の実行状況（プロジェクトごと）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnalysisStatus {
    /// idle / running / error
    pub state: String,
    /// 最後に成功した分析の日時（UTC）。
    pub last_success_at: Option<String>,
    /// 最後に失敗した分析の日時と理由。失敗しても以前のルールは残る。
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    /// 最後の実行のきっかけ（manual / auto）。
    pub last_trigger: Option<String>,
}

/// 自動分析を始める、未分析の操作の件数。
pub const AUTO_MIN_DECISIONS: i64 = 5;
/// 最後の操作からこの時間（秒）が経つまでは自動分析しない（操作が続いている間にまとめるため）。
pub const AUTO_IDLE_SECONDS: i64 = 10 * 60;
/// 自動分析どうしの最小間隔（秒）。
pub const AUTO_MIN_INTERVAL_SECONDS: i64 = 60 * 60;
const KEY_AUTO_ANALYZE: &str = "preferences.auto_analyze";
const STATUS_PREFIX: &str = "preferences.status.";

/// 分析を同時に複数実行しないための目印（手動・自動の重複を防ぐ）。
static ANALYZING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub global_rules: Vec<Rule>,
    pub project_rules: Vec<Rule>,
    pub suggestions: Vec<Suggestion>,
    pub settings: PreferenceSettings,
    /// まだ分析していない、承認・却下済みの提案の件数。
    pub unanalyzed: i64,
    pub status: AnalysisStatus,
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
            auto_analyze: self.bool_setting(KEY_AUTO_ANALYZE)?,
        })
    }

    pub fn analysis_status(&self, project_id: &str) -> Result<AnalysisStatus> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [format!("{STATUS_PREFIX}{project_id}")], |r| r.get(0))
            .optional()?;
        let mut status: AnalysisStatus = raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
        let running = ANALYZING.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(project_id);
        status.state = if running {
            "running".into()
        } else if status.last_error_at.is_some() && status.last_error_at > status.last_success_at {
            "error".into()
        } else {
            "idle".into()
        };
        Ok(status)
    }

    fn save_analysis_status(&self, project_id: &str, trigger: &str, error: Option<&str>) -> Result<()> {
        let mut status = self.analysis_status(project_id)?;
        let now: String = self.conn.query_row(&format!("SELECT {NOW}"), [], |r| r.get(0))?;
        status.last_trigger = Some(trigger.to_string());
        match error {
            None => status.last_success_at = Some(now),
            Some(e) => {
                status.last_error_at = Some(now);
                status.last_error = Some(e.chars().take(300).collect());
            }
        }
        status.state = String::new();
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("{STATUS_PREFIX}{project_id}"), serde_json::to_string(&status)?],
        )?;
        Ok(())
    }

    /// 自動分析の条件を満たすか（件数・最後の操作からの経過・前回からの間隔）。
    pub fn auto_analysis_due(&self, project_id: &str) -> Result<bool> {
        if self.unanalyzed_count(project_id)? < AUTO_MIN_DECISIONS {
            return Ok(false);
        }
        let idle: bool = self.conn.query_row(
            &format!(
                "SELECT COALESCE((julianday({NOW}) - julianday(MAX(decided_at))) * 86400 >= ?2, 1) FROM proposals
                 WHERE project_id = ?1 AND status IN ('approved', 'rejected') AND analyzed_at IS NULL"
            ),
            params![project_id, AUTO_IDLE_SECONDS],
            |r| r.get(0),
        )?;
        if !idle {
            return Ok(false);
        }
        let status = self.analysis_status(project_id)?;
        let last = [status.last_success_at.clone(), status.last_error_at.clone()].into_iter().flatten().max();
        if let Some(last) = last {
            let elapsed: f64 = self.conn.query_row(
                &format!("SELECT (julianday({NOW}) - julianday(?1)) * 86400"),
                [last],
                |r| r.get(0),
            )?;
            if elapsed < AUTO_MIN_INTERVAL_SECONDS as f64 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn update_preference_settings(
        &mut self,
        auto_update: Option<bool>,
        include_direct_edits: Option<bool>,
        auto_analyze: Option<bool>,
    ) -> Result<()> {
        if let Some(v) = auto_analyze {
            self.set_bool_setting(KEY_AUTO_ANALYZE, v)?;
        }
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
            status: self.analysis_status(project_id)?,
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
            if v.get("title").is_some() && v.get("body").is_some() {
                format!("[場面「{}」の概要] {}", text("title"), text("body"))
            } else if v.get("kind").is_some() {
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
    analyze_with_trigger(store, provider, env_api_key, project_id, "manual").await
}

/// 分析を実行し、結果（成功・失敗）を状況として記録する。同時に実行できるのは 1 件だけ。
pub async fn analyze_with_trigger(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    project_id: &str,
    trigger: &str,
) -> Result<AnalysisResult> {
    {
        let mut running = ANALYZING.lock().unwrap_or_else(|e| e.into_inner());
        if running.is_some() {
            return Err(AppError::Invalid("別の分析を実行中です。終わってからもう一度試してください".into()));
        }
        *running = Some(project_id.to_string());
    }
    let result = analyze_inner(store, provider, env_api_key, project_id).await;
    *ANALYZING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let s = lock(store);
    // 「分析できる操作がない」は失敗として残さない。
    let error = match &result {
        Err(AppError::Invalid(_)) => return result,
        Err(e) => Some(e.to_string()),
        Ok(_) => None,
    };
    s.save_analysis_status(project_id, trigger, error.as_deref())?;
    drop(s);
    match result {
        Ok(mut r) => {
            r.profile = lock(store).profile(project_id)?;
            Ok(r)
        }
        Err(e) => Err(e),
    }
}

/// 自動分析: 設定が有効なら、条件を満たすプロジェクトを分析する。分析したプロジェクトの ID を返す。
pub async fn run_auto_analysis(store: &Mutex<Store>, provider: &dyn LlmProvider, env_api_key: Option<&str>) -> Vec<String> {
    let due: Vec<String> = {
        let s = lock(store);
        if !s.preference_settings().map(|p| p.auto_analyze).unwrap_or(false) || s.ensure_llm_allowed().is_err() {
            return Vec::new();
        }
        s.list_projects()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| s.auto_analysis_due(&p.id).unwrap_or(false))
            .map(|p| p.id)
            .collect()
    };
    let mut done = Vec::new();
    for project_id in due {
        // 失敗は状況として記録済み。以前のルールは失われない。
        let _ = analyze_with_trigger(store, provider, env_api_key, &project_id, "auto").await;
        done.push(project_id);
    }
    done
}

async fn analyze_inner(
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

    struct ScriptedLlm(std::sync::Mutex<Vec<String>>);

    impl LlmProvider for ScriptedLlm {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn generate<'a>(&'a self, _m: &'a str, _k: &'a str, _r: &'a LlmRequest) -> crate::llm::LlmFuture<'a> {
            let next = self.0.lock().unwrap().pop();
            Box::pin(async move {
                next.map(|text| crate::llm::LlmResponse { text, input_tokens: 1, output_tokens: 1 })
                    .ok_or_else(|| crate::llm::LlmError("台本切れ".into()))
            })
        }
    }

    /// 承認・却下済みの提案を n 件作る（決定日時は `ago_seconds` 秒前）。
    fn decided(store: &Store, project_id: &str, n: usize, ago_seconds: i64) {
        let chat = new_id();
        store.conn.execute(
            &format!("INSERT INTO chats (id, project_id, title, created_at, updated_at) VALUES (?1, ?2, 'c', {NOW}, {NOW})"),
            params![chat, project_id],
        ).unwrap();
        let msg = new_id();
        store.conn.execute(
            &format!("INSERT INTO messages (id, chat_id, seq, role, content, created_at) VALUES (?1, ?2, 1, 'assistant', 'x', {NOW})"),
            params![msg, chat],
        ).unwrap();
        for i in 0..n {
            store.conn.execute(
                &format!(
                    "INSERT INTO proposals (id, message_id, project_id, ordinal, op, after, status, decided_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'update_element', '{{\"kind\":\"Narration\",\"speaker\":\"\",\"body\":\"x\"}}', 'approved',
                             strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?5), {NOW})"
                ),
                params![new_id(), msg, project_id, i as i64, format!("-{ago_seconds} seconds")],
            ).unwrap();
        }
    }

    #[tokio::test]
    async fn auto_analysis_runs_only_when_enabled_and_due() {
        let mut store = Store::open_in_memory().unwrap();
        let p = store.create_project("自動", "", &[]).unwrap();
        store.update_llm_settings(Some("key"), None, None, None).unwrap();
        let answer = serde_json::json!({ "summary": "s", "changes": [] }).to_string();
        let llm = ScriptedLlm(std::sync::Mutex::new(vec![answer.clone(), answer.clone()]));
        decided(&store, &p.id, 5, 3600);
        let store = Mutex::new(store);

        // 無効なら何もしない。
        assert!(run_auto_analysis(&store, &llm, None).await.is_empty());
        lock(&store).update_preference_settings(None, None, Some(true)).unwrap();
        assert!(lock(&store).preference_settings().unwrap().auto_analyze);

        // 件数・経過時間を満たすと実行し、状況を記録する。
        assert_eq!(run_auto_analysis(&store, &llm, None).await, vec![p.id.clone()]);
        let status = lock(&store).analysis_status(&p.id).unwrap();
        assert_eq!((status.state.as_str(), status.last_trigger.as_deref()), ("idle", Some("auto")));
        assert!(status.last_success_at.is_some());
        assert_eq!(lock(&store).unanalyzed_count(&p.id).unwrap(), 0);

        // 直後は、件数を満たしても前回からの間隔が足りないので実行しない。
        decided(&lock(&store), &p.id, 5, 3600);
        assert!(!lock(&store).auto_analysis_due(&p.id).unwrap());
        assert!(run_auto_analysis(&store, &llm, None).await.is_empty());

        // 操作が続いている（最後の操作から時間が経っていない）間は実行しない。
        let q = lock(&store).create_project("編集中", "", &[]).unwrap();
        decided(&lock(&store), &q.id, 6, 30);
        assert!(!lock(&store).auto_analysis_due(&q.id).unwrap());
        // 件数が足りない場合も実行しない。
        let r = lock(&store).create_project("少ない", "", &[]).unwrap();
        decided(&lock(&store), &r.id, 2, 3600);
        assert!(!lock(&store).auto_analysis_due(&r.id).unwrap());

        // 失敗は状況として記録し、以前のルールは残る。
        let t = lock(&store).create_project("失敗", "", &[]).unwrap();
        let rule = lock(&store).create_rule(Some(&t.id), "地の文は短く", "length").unwrap();
        decided(&lock(&store), &t.id, 5, 3600);
        let empty = ScriptedLlm(std::sync::Mutex::new(vec![]));
        assert_eq!(run_auto_analysis(&store, &empty, None).await, vec![t.id.clone()]);
        let status = lock(&store).analysis_status(&t.id).unwrap();
        assert_eq!(status.state, "error");
        assert_eq!(status.last_error.as_deref(), Some("台本切れ"));
        assert_eq!(lock(&store).list_rules(Some(&t.id)).unwrap()[0].id, rule.id);
        // 利用停止中は自動分析しない。
        lock(&store).update_llm_settings(None, None, Some(1), None).unwrap();
        assert!(run_auto_analysis(&store, &llm, None).await.is_empty());
    }

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
