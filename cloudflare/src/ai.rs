//! AI を使う要求（作品づくり・嗜好の分析・モデル一覧・AI 設定）。Durable Object の中で処理する。
//!
//! LLM の呼び出しは時間がかかるため、トランザクションの外で行う。読み出し → 呼び出し → 結果の書き込み
//! （使用量の記録を含む）の順に進め、書き込みだけをトランザクションの中で行う。

use serde_json::Value;

use scenote_core::generation::*;
use scenote_core::llm::{extract_json, LlmMessage, LlmRequest, Role};
use scenote_core::preferences::{analysis_prompt, interpret_analysis, AnalysisResult, ANALYSIS_PROMPT};

use crate::api::{ApiResponse, Body};
use crate::error::{AppError, Result};
use crate::llm::{self, UpdateLlmSettings};
use crate::ScenoteData;

fn ok(value: impl serde::Serialize) -> ApiResponse {
    match serde_json::to_value(value) {
        Ok(v) => ApiResponse { status: 200, body: Body::Json(v) },
        Err(e) => ApiResponse::error(&AppError::Internal(e.to_string())),
    }
}

fn respond<T: serde::Serialize>(r: Result<T>) -> ApiResponse {
    match r {
        Ok(v) => ok(v),
        Err(e) => {
            if let AppError::Internal(d) = &e {
                worker::console_error!("internal error: {d}");
            }
            ApiResponse::error(&e)
        }
    }
}

impl ScenoteData {
    /// AI を使う要求なら処理して応答を返す。それ以外は `None`。
    pub(crate) async fn handle_ai(&self, method: &str, api_path: &str, body: &str, client: Option<String>) -> Option<ApiResponse> {
        let path = api_path.split('?').next().unwrap_or("");
        let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        let response = match (method, segments.as_slice()) {
            ("GET", ["settings", "llm"]) => respond(self.store.llm_settings(llm::api_key(&self.env).is_some())),
            ("PUT", ["settings", "llm"]) => {
                let store = self.store.clone();
                let has_key = llm::api_key(&self.env).is_some();
                let req: Result<UpdateLlmSettings> = serde_json::from_str(body).map_err(AppError::from);
                respond(req.and_then(|req| {
                    self.run_tx(move || {
                        let r = store.update_llm_settings(req).and_then(|_| store.llm_settings(has_key));
                        let ok = r.is_ok();
                        (r, ok)
                    })?
                }))
            }
            ("GET", ["settings", "llm", "models"]) => respond(self.models().await),
            ("POST", ["projects", id, "generate", stage]) => {
                let req: Result<GenerateRequest> = serde_json::from_str(if body.trim().is_empty() { "{}" } else { body }).map_err(AppError::from);
                match req {
                    Err(e) => ApiResponse::error(&e),
                    Ok(req) => match *stage {
                        "characters" => respond(self.generate_characters(id, req).await),
                        "world" => respond(self.generate_world(id, req).await),
                        "story" => respond(self.generate_story(id, req).await),
                        _ => ApiResponse::error(&AppError::NotFound("API")),
                    },
                }
            }
            ("POST", ["projects", id, "preferences", "analyze"]) => respond(self.analyze(id, "manual").await),
            _ => return None,
        };
        if method != "GET" && response.is_success() {
            let scopes = scenote_core::events::scopes_for(&format!("/api/{path}"));
            if !scopes.is_empty() {
                self.broadcast(scopes, client);
            }
        }
        Some(response)
    }

    async fn models(&self) -> Result<Vec<scenote_core::llm::LlmModel>> {
        let key = llm::require_api_key(&self.env)?;
        llm::list_models(&self.env, &key).await.map_err(|e| AppError::Llm(e.0))
    }

    /// LLM を呼び出し、使用量を記録して応答の文章を返す。
    async fn call_llm(&self, purpose: &'static str, system: String, prompt: String) -> Result<String> {
        self.store.ensure_llm_allowed()?;
        let key = llm::require_api_key(&self.env)?;
        let model = self.store.llm_model()?;
        let request = LlmRequest { system, messages: vec![LlmMessage { role: Role::User, text: prompt }] };
        let response = llm::generate(&self.env, &model, &key, &request).await.map_err(|e| AppError::Llm(e.0))?;
        let store = self.store.clone();
        let (input, output) = (response.input_tokens, response.output_tokens);
        self.run_tx(move || {
            let r = store.record_usage(&model, purpose, input, output);
            let ok = r.is_ok();
            (r, ok)
        })??;
        Ok(response.text)
    }

    async fn call_json(&self, p: Prompt) -> Result<Value> {
        let text = self.call_llm("generate", p.system, p.prompt).await?;
        extract_json(&text).ok_or_else(|| AppError::Llm("AI の応答を解釈できませんでした（JSON ではありません）".into()))
    }

    fn saved_characters(&self, project_id: &str) -> Result<Vec<CharacterDraft>> {
        let mut out = Vec::new();
        for c in self.store.list_characters(project_id)? {
            let facts = self
                .store
                .list_facts(&c.id)?
                .into_iter()
                // 承認待ち・却下した推定は使わない。
                .filter(|f| f.status == "active")
                .map(|f| FactDraft { category: f.category, text: f.text })
                .collect();
            out.push(CharacterDraft { name: c.name, facts });
        }
        Ok(out)
    }

    async fn generate_characters(&self, project_id: &str, req: GenerateRequest) -> Result<Drafts<Vec<CharacterDraft>>> {
        let p = self.store.get_project(project_id)?;
        let premise = premise_of(&req, &p.description).map_err(AppError::Invalid)?;
        let existing: Vec<String> = self.store.list_characters(project_id)?.into_iter().map(|c| c.name).collect();
        let value = self.call_json(characters_prompt(&premise, &existing, req.count, &req.instructions)).await?;
        parse_characters(&value, &existing).map_err(AppError::Llm)
    }

    async fn generate_world(&self, project_id: &str, req: GenerateRequest) -> Result<Drafts<WorldDraft>> {
        let p = self.store.get_project(project_id)?;
        let premise = premise_of(&req, &p.description).map_err(AppError::Invalid)?;
        let mut chars = self.saved_characters(project_id)?;
        for c in &req.characters {
            if !chars.iter().any(|x| x.name == c.name) {
                chars.push(c.clone());
            }
        }
        let value = self.call_json(world_prompt(&premise, &chars, &req.instructions)).await?;
        parse_world(&value).map_err(AppError::Llm)
    }

    async fn generate_story(&self, project_id: &str, req: GenerateRequest) -> Result<Drafts<Vec<ChapterDraft>>> {
        let p = self.store.get_project(project_id)?;
        let chars = self.saved_characters(project_id)?;
        if chars.is_empty() {
            return Err(AppError::Invalid(NEED_CHARACTERS.into()));
        }
        let world: Vec<String> = self
            .store
            .list_files(project_id)?
            .iter()
            .filter(|f| is_world_file(&f.name, f.pinned))
            .map(|f| format!("## {}\n{}", f.name, f.content))
            .collect();
        let tree = self.store.project_tree(project_id)?;
        let outline: Vec<String> = tree
            .chapters
            .iter()
            .flat_map(|c| c.scenes.iter().map(move |sc| format!("- {} / {}: {}", c.chapter.title, sc.title, sc.summary)))
            .collect();
        let premise = premise_of(&req, &p.description).map_err(AppError::Invalid)?;
        let value = self.call_json(story_prompt(&premise, &chars, &world, &outline, &req.instructions)).await?;
        parse_story(&value).map_err(AppError::Llm)
    }

    /// 嗜好を分析し、結果（成功・失敗）を状況として記録する。同時に実行できるのは 1 件だけ。
    pub(crate) async fn analyze(&self, project_id: &str, trigger: &'static str) -> Result<AnalysisResult> {
        {
            let mut running = self.analyzing.borrow_mut();
            if running.is_some() {
                return Err(AppError::Invalid("別の分析を実行中です。終わってからもう一度試してください".into()));
            }
            *running = Some(project_id.to_string());
        }
        let result = self.analyze_inner(project_id).await;
        *self.analyzing.borrow_mut() = None;
        // 「分析できる操作がない」は失敗として残さない。
        let error = match &result {
            Err(AppError::Invalid(_)) => return result,
            Err(e) => Some(e.to_string()),
            Ok(_) => None,
        };
        let store = self.store.clone();
        let pid = project_id.to_string();
        self.run_tx(move || {
            let r = store.save_analysis_status(&pid, trigger, error.as_deref());
            let ok = r.is_ok();
            (r, ok)
        })??;
        let mut r = result?;
        r.profile = self.store.profile(project_id, None)?;
        Ok(r)
    }

    async fn analyze_inner(&self, project_id: &str) -> Result<AnalysisResult> {
        self.store.get_project(project_id)?;
        self.store.ensure_llm_allowed()?;
        llm::require_api_key(&self.env)?;
        let settings = self.store.preference_settings()?;
        let collected = self.store.collect_decisions(project_id, settings.include_direct_edits)?;
        if !collected.decisions.iter().any(|d| d.proposal_id.is_some()) {
            return Err(AppError::Invalid(
                "分析できる新しい操作がありません。MCP で接続した AI が書いた本文を直したり消したりすると、分析できるようになります".into(),
            ));
        }
        let rules = self.store.list_rules(Some(project_id))?;
        let global_rules = self.store.list_rules(None)?;
        let prompt = analysis_prompt(&rules, &global_rules, &collected.decisions);
        let text = self.call_llm("preferences", ANALYSIS_PROMPT.to_string(), prompt).await?;
        let (summary, changes, warnings) = interpret_analysis(&text, &collected.decisions, &rules).map_err(AppError::Llm)?;
        let store = self.store.clone();
        let pid = project_id.to_string();
        let suggestions = self.run_tx(move || {
            let r = store.save_analysis(&pid, &collected, &changes);
            let ok = r.is_ok();
            (r, ok)
        })??;
        let applied = self.store.preference_settings()?.auto_update;
        Ok(AnalysisResult { summary, suggestions, applied, warnings, profile: self.store.profile(project_id, None)? })
    }

    /// 自動分析: 設定が有効なら、条件を満たすプロジェクトを分析する。分析したプロジェクトの ID を返す。
    pub(crate) async fn run_auto_analysis(&self) -> Vec<String> {
        let enabled = self.store.preference_settings().map(|p| p.auto_analyze).unwrap_or(false);
        if !enabled || self.store.ensure_llm_allowed().is_err() || llm::api_key(&self.env).is_none() {
            return Vec::new();
        }
        let due: Vec<String> = self
            .store
            .list_projects()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| self.store.auto_analysis_due(&p.id).unwrap_or(false))
            .map(|p| p.id)
            .collect();
        let mut done = Vec::new();
        for project_id in due {
            // 失敗は状況として記録済み。以前のルールは失われない。
            let _ = self.analyze(&project_id, "auto").await;
            self.broadcast(vec!["prefs"], None);
            done.push(project_id);
        }
        done
    }
}

