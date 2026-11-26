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
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::llm::{LlmMessage, LlmProvider, LlmRequest, Role};
use crate::store::{new_id, Store, NOW};

pub use scenote_core::preferences::*;

/// 分析を同時に複数実行しないための目印（手動・自動の重複を防ぐ）。
static ANALYZING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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

fn checked_rule_text(text: &str) -> Result<String> {
    scenote_core::preferences::clean_rule_text(text).map_err(AppError::Invalid)
}

fn checked_category(category: &str) -> Result<String> {
    scenote_core::preferences::clean_category(category).map_err(AppError::Invalid)
}

fn checked_confidence(c: f64) -> Result<f64> {
    scenote_core::preferences::clean_confidence(c).map_err(AppError::Invalid)
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
        let id = self.insert_rule(project_id, &checked_rule_text(text)?, &checked_category(category)?, 1.0, &[], SOURCE_USER)?;
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
            Some(t) => checked_rule_text(t)?,
            None => rule.text,
        };
        let category = match category {
            Some(c) => checked_category(c)?,
            None => rule.category,
        };
        let confidence = match confidence {
            Some(c) => checked_confidence(c)?,
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
        Ok(preference_prompt_text(&self.list_rules(Some(project_id))?, self.list_rules(None)?))
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

    let prompt = analysis_prompt(&rules, &global_rules, &decisions);
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
