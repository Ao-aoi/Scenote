//! キャラクター・世界観・ストーリーを AI で段階的に生成する（#6）。
//!
//! プロンプトの組み立てと応答の検証は `scenote_core::generation` を共有する。ここでは DB からの読み出しと
//! LLM の呼び出し・使用量の記録だけを行う。生成した内容は案として返すだけで、保存はしない。

use std::sync::Mutex;

use serde_json::Value;

pub use scenote_core::generation::*;

use crate::error::{AppError, Result};
use crate::llm::{extract_json, LlmMessage, LlmProvider, LlmRequest, Role};
use crate::store::Store;

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// LLM を呼び出し、使用量を記録して JSON を返す。
async fn call(store: &Mutex<Store>, provider: &dyn LlmProvider, env_api_key: Option<&str>, project_id: &str, p: Prompt) -> Result<Value> {
    let (model, key) = {
        let s = lock(store);
        s.get_project(project_id)?;
        s.ensure_llm_allowed()?;
        let key = match env_api_key {
            Some(k) => Some(k.to_string()),
            None => s.stored_api_key()?,
        }
        .ok_or_else(|| AppError::Invalid("API キーが設定されていません。「AI 設定」から登録してください".into()))?;
        (s.llm_model()?, key)
    };
    let request = LlmRequest { system: p.system, messages: vec![LlmMessage { role: Role::User, text: p.prompt }] };
    let response = provider.generate(&model, &key, &request).await.map_err(|e| AppError::Llm(e.0))?;
    lock(store).record_usage(provider.name(), &model, "generate", response.input_tokens, response.output_tokens)?;
    extract_json(&response.text).ok_or_else(|| AppError::Llm("AI の応答を解釈できませんでした（JSON ではありません）".into()))
}

/// 段階 1: キャラクターのプロフィール案。
pub async fn characters(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    project_id: &str,
    req: GenerateRequest,
) -> Result<Drafts<Vec<CharacterDraft>>> {
    let (premise, existing) = {
        let s = lock(store);
        let p = s.get_project(project_id)?;
        let existing: Vec<String> = s.list_characters(project_id)?.into_iter().map(|c| c.name).collect();
        (premise_of(&req, &p.description).map_err(AppError::Invalid)?, existing)
    };
    let prompt = characters_prompt(&premise, &existing, req.count, &req.instructions);
    let value = call(store, provider, env_api_key, project_id, prompt).await?;
    parse_characters(&value, &existing).map_err(AppError::Llm)
}

/// 段階 2: 世界観・設定の案（参照ファイルとして登録できる Markdown）。
pub async fn world(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    project_id: &str,
    req: GenerateRequest,
) -> Result<Drafts<WorldDraft>> {
    let (premise, mut chars) = {
        let s = lock(store);
        let p = s.get_project(project_id)?;
        (premise_of(&req, &p.description).map_err(AppError::Invalid)?, saved_characters(&s, project_id)?)
    };
    // 採用前のキャラクター案も参考にできる。
    for c in &req.characters {
        if !chars.iter().any(|x| x.name == c.name) {
            chars.push(c.clone());
        }
    }
    let value = call(store, provider, env_api_key, project_id, world_prompt(&premise, &chars, &req.instructions)).await?;
    parse_world(&value).map_err(AppError::Llm)
}

fn saved_characters(s: &Store, project_id: &str) -> Result<Vec<CharacterDraft>> {
    let mut out = Vec::new();
    for c in s.list_characters(project_id)? {
        let facts = s
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

/// 段階 3: 登録済みのキャラクターと世界観をもとに、ストーリー（章・場面の概要）の案を作る。
pub async fn story(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    project_id: &str,
    req: GenerateRequest,
) -> Result<Drafts<Vec<ChapterDraft>>> {
    let (premise, chars, world, outline) = {
        let s = lock(store);
        let p = s.get_project(project_id)?;
        let chars = saved_characters(&s, project_id)?;
        if chars.is_empty() {
            return Err(AppError::Invalid(NEED_CHARACTERS.into()));
        }
        let world: Vec<String> = s
            .list_files(project_id)?
            .iter()
            .filter(|f| is_world_file(&f.name, f.pinned))
            .map(|f| format!("## {}\n{}", f.name, f.content))
            .collect();
        let tree = s.project_tree(project_id)?;
        let outline: Vec<String> = tree
            .chapters
            .iter()
            .flat_map(|c| c.scenes.iter().map(move |sc| format!("- {} / {}: {}", c.chapter.title, sc.title, sc.summary)))
            .collect();
        (premise_of(&req, &p.description).map_err(AppError::Invalid)?, chars, world, outline)
    };
    let value = call(store, provider, env_api_key, project_id, story_prompt(&premise, &chars, &world, &outline, &req.instructions)).await?;
    parse_story(&value).map_err(AppError::Llm)
}
