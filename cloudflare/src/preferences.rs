//! 執筆嗜好の学習（Cloudflare 版）。規則は PC 版の `server/src/preferences.rs` と同じで、型・検証・
//! 分析のプロンプトと結果の解釈は `scenote_core::preferences` を共有する。
//!
//! 分析の材料は次のとおり（いずれもまだ分析していないものだけ）。
//! 1. AI の提案への操作（承認・修正して採用・却下）。PC 版と同じ。
//! 2. MCP で接続した AI が書いた本文を、ユーザーが後から直した・消した操作（編集履歴から集める）。
//!    Cloudflare 版では外部の AI が直接書き込むため、その文章への手直しを「修正して採用」「却下」に当たる反応として扱う。
//! 3. 設定で有効な場合のみ、ユーザーが直接入力した本文。

use serde::Deserialize;
use serde_json::Value;

use scenote_core::preferences::*;

use crate::db::NOW;
use crate::error::{AppError, Result};
use crate::p;
use crate::store::{new_id, Store};

/// MCP の AI が書いた本文への反応を、どの履歴まで分析したか（プロジェクトごと）。
const ANALYZED_SEQ_PREFIX: &str = "preferences.analyzed_seq.";

#[derive(Deserialize)]
struct RuleRow {
    id: String,
    project_id: Option<String>,
    text: String,
    category: String,
    confidence: f64,
    evidence: String,
    source: String,
    created_at: String,
    updated_at: String,
}

impl From<RuleRow> for Rule {
    fn from(r: RuleRow) -> Self {
        Rule {
            id: r.id,
            project_id: r.project_id,
            text: r.text,
            category: r.category,
            confidence: r.confidence,
            evidence: serde_json::from_str(&r.evidence).unwrap_or_default(),
            source: r.source,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct SuggestionRow {
    id: String,
    project_id: String,
    op: String,
    rule_id: Option<String>,
    text: String,
    category: String,
    confidence: f64,
    evidence: String,
    reason: String,
    status: String,
    created_at: String,
}

#[derive(Deserialize)]
struct ProposalRow {
    id: String,
    op: String,
    before: Option<String>,
    after: Option<String>,
    #[serde(rename = "final")]
    final_content: Option<String>,
    status: String,
    reject_reason: String,
    reason: String,
}

#[derive(Deserialize)]
struct FeedbackRow {
    seq: i64,
    action: String,
    before: Option<String>,
    after: Option<String>,
}

#[derive(Deserialize)]
struct EditRow {
    before: Option<String>,
    after: Option<String>,
}

#[derive(Deserialize)]
struct Count {
    v: i64,
}

fn parse(t: Option<String>) -> Option<Value> {
    t.and_then(|t| serde_json::from_str::<Value>(&t).ok())
}

/// MCP の AI が先に書いた要素に対する、人間の更新・削除（`seq` が `after` より新しいもの）。
const FEEDBACK_SQL: &str = "FROM history h
    WHERE h.project_id = ? AND h.seq > ? AND h.target_type = 'element' AND h.actor = 'human'
      AND h.action IN ('update', 'delete')
      AND EXISTS (SELECT 1 FROM history a WHERE a.project_id = h.project_id AND a.target_id = h.target_id
                  AND a.actor = 'ai_direct' AND a.seq < h.seq)";

pub struct Collected {
    pub decisions: Vec<Decision>,
    /// 分析に使った提案。
    pub proposal_ids: Vec<String>,
    /// 分析に使った履歴の最大の番号（MCP の AI の本文への反応）。
    pub max_seq: Option<i64>,
}

impl Store {
    fn bool_setting(&self, key: &str) -> Result<bool> {
        Ok(self.setting(key)?.is_some_and(|v| v == "1"))
    }

    pub fn preference_settings(&self) -> Result<PreferenceSettings> {
        Ok(PreferenceSettings {
            auto_update: self.bool_setting(KEY_AUTO_UPDATE)?,
            include_direct_edits: self.bool_setting(KEY_INCLUDE_DIRECT)?,
            auto_analyze: self.bool_setting(KEY_AUTO_ANALYZE)?,
        })
    }

    pub fn update_preference_settings(&self, auto_update: Option<bool>, include_direct_edits: Option<bool>, auto_analyze: Option<bool>) -> Result<()> {
        for (key, value) in [(KEY_AUTO_ANALYZE, auto_analyze), (KEY_AUTO_UPDATE, auto_update), (KEY_INCLUDE_DIRECT, include_direct_edits)] {
            if let Some(v) = value {
                self.set_setting(key, Some(if v { "1" } else { "0" }))?;
            }
        }
        Ok(())
    }

    /// 分析の実行状況。`running` は分析中のプロジェクト（Durable Object が保持する）。
    pub fn analysis_status(&self, project_id: &str, running: Option<&str>) -> Result<AnalysisStatus> {
        let mut status: AnalysisStatus = self
            .setting(&format!("{STATUS_PREFIX}{project_id}"))?
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        status.state = if running == Some(project_id) {
            "running".into()
        } else if status.last_error_at.is_some() && status.last_error_at > status.last_success_at {
            "error".into()
        } else {
            "idle".into()
        };
        Ok(status)
    }

    pub fn save_analysis_status(&self, project_id: &str, trigger: &str, error: Option<&str>) -> Result<()> {
        let mut status = self.analysis_status(project_id, None)?;
        let now: String = self.db.scalar(&format!("SELECT {NOW} AS v"), p![])?;
        status.last_trigger = Some(trigger.to_string());
        match error {
            None => status.last_success_at = Some(now),
            Some(e) => {
                status.last_error_at = Some(now);
                status.last_error = Some(e.chars().take(300).collect());
            }
        }
        status.state = String::new();
        self.set_setting(&format!("{STATUS_PREFIX}{project_id}"), Some(&serde_json::to_string(&status).unwrap_or_default()))
    }

    fn analyzed_seq(&self, project_id: &str) -> Result<i64> {
        Ok(self.setting(&format!("{ANALYZED_SEQ_PREFIX}{project_id}"))?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn unanalyzed_count(&self, project_id: &str) -> Result<i64> {
        let proposals: i64 = self.db.scalar(
            "SELECT COUNT(*) AS v FROM proposals WHERE project_id = ? AND status IN ('approved', 'rejected') AND analyzed_at IS NULL",
            p![project_id],
        )?;
        let feedback: i64 = self.db.scalar(&format!("SELECT COUNT(*) AS v {FEEDBACK_SQL}"), p![project_id, self.analyzed_seq(project_id)?])?;
        Ok(proposals + feedback)
    }

    /// 自動分析の条件を満たすか（件数・最後の操作からの経過・前回からの間隔）。
    pub fn auto_analysis_due(&self, project_id: &str) -> Result<bool> {
        if self.unanalyzed_count(project_id)? < AUTO_MIN_DECISIONS {
            return Ok(false);
        }
        let idle: Count = self.db.one(
            &format!(
                "SELECT COALESCE((julianday({NOW}) - julianday(MAX(t))) * 86400 >= ?, 1) AS v FROM (
                   SELECT MAX(decided_at) AS t FROM proposals WHERE project_id = ? AND status IN ('approved', 'rejected') AND analyzed_at IS NULL
                   UNION ALL SELECT MAX(updated_at) FROM history WHERE project_id = ? AND actor = 'human')"
            ),
            p![AUTO_IDLE_SECONDS, project_id, project_id],
        )?;
        if idle.v == 0 {
            return Ok(false);
        }
        let status = self.analysis_status(project_id, None)?;
        if let Some(last) = [status.last_success_at, status.last_error_at].into_iter().flatten().max() {
            let elapsed: f64 = self.db.scalar(&format!("SELECT (julianday({NOW}) - julianday(?)) * 86400 AS v"), p![last])?;
            if elapsed < AUTO_MIN_INTERVAL_SECONDS as f64 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // ---- ルール -------------------------------------------------------

    /// `project_id` が `None` なら共通ルール。
    pub fn list_rules(&self, project_id: Option<&str>) -> Result<Vec<Rule>> {
        let rows: Vec<RuleRow> = self.db.all(
            "SELECT * FROM preference_rules WHERE project_id IS ? ORDER BY confidence DESC, created_at",
            p![project_id],
        )?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_rule(&self, id: &str) -> Result<Rule> {
        self.db
            .opt::<RuleRow>("SELECT * FROM preference_rules WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("ルール"))
    }

    fn insert_rule(&self, project_id: Option<&str>, text: &str, category: &str, confidence: f64, evidence: &[String], source: &str) -> Result<String> {
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO preference_rules (id, project_id, text, category, confidence, evidence, source, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), project_id, text, category, confidence, serde_json::to_string(evidence).unwrap_or_default(), source],
        )?;
        Ok(id)
    }

    pub fn create_rule(&self, project_id: Option<&str>, text: &str, category: &str) -> Result<Rule> {
        if let Some(p) = project_id {
            self.get_project(p)?;
        }
        let text = clean_rule_text(text).map_err(AppError::Invalid)?;
        let category = clean_category(category).map_err(AppError::Invalid)?;
        let id = self.insert_rule(project_id, &text, &category, 1.0, &[], SOURCE_USER)?;
        self.get_rule(&id)
    }

    pub fn update_rule(&self, id: &str, text: Option<&str>, category: Option<&str>, confidence: Option<f64>) -> Result<Rule> {
        let rule = self.get_rule(id)?;
        let text = match text {
            Some(t) => clean_rule_text(t).map_err(AppError::Invalid)?,
            None => rule.text,
        };
        let category = match category {
            Some(c) => clean_category(c).map_err(AppError::Invalid)?,
            None => rule.category,
        };
        let confidence = match confidence {
            Some(c) => clean_confidence(c).map_err(AppError::Invalid)?,
            None => rule.confidence,
        };
        self.db.exec(
            &format!("UPDATE preference_rules SET text = ?, category = ?, confidence = ?, updated_at = {NOW} WHERE id = ?"),
            p![text, category, confidence, id],
        )?;
        self.get_rule(id)
    }

    pub fn delete_rule(&self, id: &str) -> Result<()> {
        self.get_rule(id)?;
        self.db.exec("DELETE FROM preference_rules WHERE id = ?", p![id])
    }

    /// ルールを別の範囲（共通、または別のプロジェクト）へ複製する。同じ文面のルールがあれば複製しない。
    pub fn copy_rules(&self, rule_ids: &[String], target_project: Option<&str>) -> Result<usize> {
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
    pub fn import_project_rules(&self, source_project: &str, target_project: &str) -> Result<usize> {
        self.get_project(source_project)?;
        if source_project == target_project {
            return Err(AppError::Invalid("同じプロジェクトからは取り込めません".into()));
        }
        let ids: Vec<String> = self.list_rules(Some(source_project))?.into_iter().map(|r| r.id).collect();
        self.copy_rules(&ids, Some(target_project))
    }

    // ---- 更新案 -------------------------------------------------------

    fn to_suggestion(&self, r: SuggestionRow) -> Result<Suggestion> {
        let rule_text = match &r.rule_id {
            Some(id) => self.db.opt::<RuleRow>("SELECT * FROM preference_rules WHERE id = ?", p![id.as_str()])?.map(|r| r.text),
            None => None,
        };
        Ok(Suggestion {
            id: r.id,
            project_id: r.project_id,
            op: r.op,
            rule_id: r.rule_id,
            rule_text,
            text: r.text,
            category: r.category,
            confidence: r.confidence,
            evidence: serde_json::from_str(&r.evidence).unwrap_or_default(),
            reason: r.reason,
            status: r.status,
            created_at: r.created_at,
        })
    }

    pub fn list_suggestions(&self, project_id: &str) -> Result<Vec<Suggestion>> {
        let rows: Vec<SuggestionRow> = self.db.all(
            "SELECT * FROM preference_suggestions WHERE project_id = ? AND status = 'pending' ORDER BY created_at, rowid",
            p![project_id],
        )?;
        rows.into_iter().map(|r| self.to_suggestion(r)).collect()
    }

    pub fn get_suggestion(&self, id: &str) -> Result<Suggestion> {
        let row: SuggestionRow = self
            .db
            .opt("SELECT * FROM preference_suggestions WHERE id = ?", p![id])?
            .ok_or(AppError::NotFound("更新案"))?;
        self.to_suggestion(row)
    }

    /// 更新案をプロファイルへ適用する。対象のルールが削除済みなら `stale`。
    pub fn approve_suggestion(&self, id: &str) -> Result<Suggestion> {
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
                self.db.exec(
                    &format!("UPDATE preference_rules SET text = ?, category = ?, confidence = ?, evidence = ?, updated_at = {NOW} WHERE id = ?"),
                    p![s.text.as_str(), s.category.as_str(), s.confidence, serde_json::to_string(&evidence).unwrap_or_default(), rule.id.as_str()],
                )?;
                "approved"
            }
            ("remove", Some(rule)) => {
                self.db.exec("DELETE FROM preference_rules WHERE id = ?", p![rule.id.as_str()])?;
                "approved"
            }
            _ => "stale",
        };
        self.db.exec("UPDATE preference_suggestions SET status = ? WHERE id = ?", p![status, id])?;
        self.get_suggestion(id)
    }

    pub fn reject_suggestion(&self, id: &str) -> Result<Suggestion> {
        let s = self.get_suggestion(id)?;
        if s.status != "pending" {
            return Err(AppError::Invalid("この更新案はすでに処理されています".into()));
        }
        self.db.exec("UPDATE preference_suggestions SET status = 'rejected' WHERE id = ?", p![id])?;
        self.get_suggestion(id)
    }

    pub fn profile(&self, project_id: &str, running: Option<&str>) -> Result<Profile> {
        self.get_project(project_id)?;
        Ok(Profile {
            global_rules: self.list_rules(None)?,
            project_rules: self.list_rules(Some(project_id))?,
            suggestions: self.list_suggestions(project_id)?,
            settings: self.preference_settings()?,
            unanalyzed: self.unanalyzed_count(project_id)?,
            status: self.analysis_status(project_id, running)?,
        })
    }

    /// 依頼のプロンプト（MCP の AI へ渡す説明など）に含める嗜好の説明。ルールがなければ空文字。
    pub fn preference_prompt(&self, project_id: &str) -> Result<String> {
        Ok(preference_prompt_text(&self.list_rules(Some(project_id))?, self.list_rules(None)?))
    }

    // ---- 分析 ---------------------------------------------------------

    /// 分析の対象となる操作を集める。
    pub fn collect_decisions(&self, project_id: &str, include_direct: bool) -> Result<Collected> {
        let mut decisions = Vec::new();
        let mut proposal_ids = Vec::new();
        let rows: Vec<ProposalRow> = self.db.all(
            "SELECT id, op, before, after, final, status, reject_reason, reason FROM proposals
             WHERE project_id = ? AND status IN ('approved', 'rejected') AND analyzed_at IS NULL
             ORDER BY decided_at DESC LIMIT ?",
            p![project_id, MAX_DECISIONS as i64],
        )?;
        for r in rows {
            let (before, after, final_content) = (parse(r.before), parse(r.after), parse(r.final_content));
            let outcome = match (r.status.as_str(), &final_content) {
                ("rejected", _) => format!(
                    "却下{}",
                    if r.reject_reason.is_empty() { String::new() } else { format!("（理由: {}）", r.reject_reason) }
                ),
                (_, Some(_)) => "修正して採用".to_string(),
                _ => "そのまま承認".to_string(),
            };
            let mut text = format!(
                "操作: {} / 結果: {outcome}\nAI の意図: {}\n変更前: {}\nAI の提案: {}",
                r.op,
                truncate(&r.reason, 200),
                truncate(&describe(&before), 600),
                truncate(&describe(&after), 600)
            );
            if final_content.is_some() {
                text.push_str(&format!("\nユーザーが修正した最終内容: {}", truncate(&describe(&final_content), 600)));
            }
            proposal_ids.push(r.id.clone());
            decisions.push(Decision { proposal_id: Some(r.id), text });
        }
        // MCP で接続した AI が書いた本文への、ユーザーの手直し・削除。
        let room = MAX_DECISIONS.saturating_sub(decisions.len()) as i64;
        let feedback: Vec<FeedbackRow> = self.db.all(
            &format!("SELECT h.seq, h.action, h.before, h.after {FEEDBACK_SQL} ORDER BY h.seq LIMIT ?"),
            p![project_id, self.analyzed_seq(project_id)?, room],
        )?;
        let max_seq = feedback.iter().map(|f| f.seq).max();
        for f in feedback {
            let (before, after) = (parse(f.before), parse(f.after));
            let text = if f.action == "delete" {
                format!("操作: AI（MCP）が書いた本文をユーザーが削除\n削除された内容: {}", truncate(&describe(&before), 600))
            } else {
                format!(
                    "操作: AI（MCP）が書いた本文をユーザーが修正\nAI が書いた内容: {}\nユーザーが修正した最終内容: {}",
                    truncate(&describe(&before), 600),
                    truncate(&describe(&after), 600)
                )
            };
            decisions.push(Decision { proposal_id: Some(format!("h{}", f.seq)), text });
        }
        if include_direct {
            let rows: Vec<EditRow> = self.db.all(
                "SELECT before, after FROM history WHERE project_id = ? AND target_type = 'element'
                   AND actor = 'human' AND action IN ('create', 'update') AND proposal_id IS NULL
                 ORDER BY seq DESC LIMIT 15",
                p![project_id],
            )?;
            for r in rows {
                decisions.push(Decision {
                    proposal_id: None,
                    text: format!(
                        "操作: ユーザーによる直接の編集\n変更前: {}\n変更後: {}",
                        truncate(&describe(&parse(r.before)), 600),
                        truncate(&describe(&parse(r.after)), 600)
                    ),
                });
            }
        }
        Ok(Collected { decisions, proposal_ids, max_seq })
    }

    /// 分析結果（検証済みの更新案）を保存し、使った操作を次回以降の対象から外す。自動更新なら適用する。
    pub fn save_analysis(&self, project_id: &str, collected: &Collected, changes: &[PlannedChange]) -> Result<Vec<Suggestion>> {
        let mut ids = Vec::new();
        for c in changes {
            let id = new_id();
            self.db.exec(
                &format!(
                    "INSERT INTO preference_suggestions (id, project_id, op, rule_id, text, category, confidence, evidence, reason, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, {NOW})"
                ),
                p![
                    id.as_str(),
                    project_id,
                    c.op,
                    c.rule_id.clone(),
                    c.text.as_str(),
                    c.category.as_str(),
                    c.confidence,
                    serde_json::to_string(&c.evidence).unwrap_or_default(),
                    c.reason.as_str()
                ],
            )?;
            ids.push(id);
        }
        for id in &collected.proposal_ids {
            self.db.exec(&format!("UPDATE proposals SET analyzed_at = {NOW} WHERE id = ?"), p![id.as_str()])?;
        }
        if let Some(seq) = collected.max_seq {
            self.set_setting(&format!("{ANALYZED_SEQ_PREFIX}{project_id}"), Some(&seq.to_string()))?;
        }
        let auto = self.preference_settings()?.auto_update;
        ids.iter().map(|id| if auto { self.approve_suggestion(id) } else { self.get_suggestion(id) }).collect()
    }
}
