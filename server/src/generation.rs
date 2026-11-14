//! キャラクター・世界観・ストーリーを AI で段階的に生成する（#6）。
//!
//! 生成した内容は「案」として返すだけで、保存はしない。ユーザーが確認・編集して採用したものだけを、
//! 既存の API（キャラクター・参照ファイル・章と場面の追加）で登録する。
//! 各段階は独立して呼び出せるため、どの段階でもやり直せる。
//! ストーリー案は、登録済みのキャラクター（ユーザーが確認したもの）と世界観のファイルを参照して作る。

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::assistant::extract_json;
use crate::error::{AppError, Result};
use crate::llm::{LlmMessage, LlmProvider, LlmRequest, Role};
use crate::store::Store;

const MAX_PREMISE_CHARS: usize = 4000;
const MAX_CHARACTERS: usize = 12;
const MAX_FACTS: usize = 12;
const MAX_CHAPTERS: usize = 12;
const MAX_SCENES: usize = 20;
/// ストーリー案に含める世界観のファイル名の候補。
pub const WORLD_FILE_NAME: &str = "世界観";

#[derive(Debug, Default, Deserialize)]
pub struct GenerateRequest {
    /// 作品の概要・ジャンル・雰囲気など。
    #[serde(default)]
    pub premise: String,
    /// この段階への追加の指示（「もっと明るく」など）。
    #[serde(default)]
    pub instructions: String,
    /// キャラクター案の人数（目安）。
    pub count: Option<usize>,
    /// 世界観の案で参考にするキャラクター（採用前の案を含めて渡せる）。
    #[serde(default)]
    pub characters: Vec<CharacterDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FactDraft {
    pub category: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharacterDraft {
    pub name: String,
    #[serde(default)]
    pub facts: Vec<FactDraft>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorldDraft {
    pub title: String,
    /// Markdown の本文。
    pub content: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SceneDraft {
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChapterDraft {
    pub title: String,
    pub scenes: Vec<SceneDraft>,
}

#[derive(Debug, Serialize)]
pub struct Drafts<T> {
    pub drafts: T,
    pub warnings: Vec<String>,
}

const COMMON: &str = "あなたはゲームシナリオの制作を手伝う補助機能です。出力は案であり、ユーザーが確認・編集してから採用します。\
出力は指定した形式の JSON オブジェクト 1 つだけとし、前後に文章やコードフェンスを付けないでください。";

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

fn clean(s: &str, max: usize) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.chars().count() <= max).then(|| s.to_string())
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// LLM を呼び出し、使用量を記録して JSON を返す。
async fn call(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    project_id: &str,
    system: String,
    prompt: String,
) -> Result<Value> {
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
    let request = LlmRequest { system, messages: vec![LlmMessage { role: Role::User, text: prompt }] };
    let response = provider.generate(&model, &key, &request).await.map_err(|e| AppError::Llm(e.0))?;
    lock(store).record_usage(provider.name(), &model, "generate", response.input_tokens, response.output_tokens)?;
    extract_json(&response.text).ok_or_else(|| AppError::Llm("AI の応答を解釈できませんでした（JSON ではありません）".into()))
}

fn premise_of(req: &GenerateRequest, project_description: &str) -> Result<String> {
    let premise = if req.premise.trim().is_empty() { project_description.trim() } else { req.premise.trim() };
    if premise.is_empty() {
        return Err(AppError::Invalid("作品の概要（ジャンル・雰囲気など）を入力してください".into()));
    }
    if premise.chars().count() > MAX_PREMISE_CHARS || req.instructions.chars().count() > MAX_PREMISE_CHARS {
        return Err(AppError::Invalid(format!("概要と指示は{MAX_PREMISE_CHARS}文字以内にしてください")));
    }
    Ok(premise.to_string())
}

fn characters_text(chars: &[CharacterDraft]) -> String {
    let mut out = String::new();
    for c in chars {
        out.push_str(&format!("## {}\n", c.name));
        for f in &c.facts {
            out.push_str(&format!("- {}: {}\n", f.category, f.text));
        }
    }
    out
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
        (premise_of(&req, &p.description)?, existing)
    };
    let count = req.count.unwrap_or(4).clamp(1, MAX_CHARACTERS);
    let system = format!(
        "{COMMON}\n\n作品の概要から、主要な登場人物のプロフィール案を作ります。\n\
         出力形式: {{\"characters\": [{{\"name\": \"名前\", \"facts\": [{{\"category\": \"役割\", \"text\": \"…\"}}, \
         {{\"category\": \"性格\", \"text\": \"…\"}}, {{\"category\": \"口調\", \"text\": \"…\"}}, {{\"category\": \"背景\", \"text\": \"…\"}}]}}]}}\n\
         - category は短い見出し（役割・年齢・外見・性格・価値観・口調・背景・目的・秘密など）とし、text は 1〜2 文にします。\n\
         - 既存の登場人物と同じ名前は使わないでください。"
    );
    let mut prompt = format!("# 作品の概要\n{premise}\n\n# 人数の目安\n{count} 人\n");
    if !existing.is_empty() {
        prompt.push_str(&format!("\n# 既存の登場人物（重複させない）\n{}\n", existing.join("、")));
    }
    if !req.instructions.trim().is_empty() {
        prompt.push_str(&format!("\n# 追加の指示\n{}\n", req.instructions.trim()));
    }
    let value = call(store, provider, env_api_key, project_id, system, prompt).await?;
    let mut warnings = Vec::new();
    let mut drafts: Vec<CharacterDraft> = Vec::new();
    for (i, c) in value.get("characters").and_then(Value::as_array).cloned().unwrap_or_default().iter().enumerate() {
        let Some(name) = clean(str_of(c, "name"), 100).filter(|n| !n.chars().any(char::is_control)) else {
            warnings.push(format!("案 {} は名前が不正なため除外しました", i + 1));
            continue;
        };
        if existing.contains(&name) || drafts.iter().any(|d| d.name == name) {
            warnings.push(format!("「{name}」は既存の人物と重なるため除外しました"));
            continue;
        }
        let facts: Vec<FactDraft> = c
            .get("facts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|f| Some(FactDraft { category: clean(str_of(f, "category"), 50)?, text: clean(str_of(f, "text"), 2000)? }))
            .take(MAX_FACTS)
            .collect();
        drafts.push(CharacterDraft { name, facts });
        if drafts.len() >= MAX_CHARACTERS {
            break;
        }
    }
    if drafts.is_empty() {
        return Err(AppError::Llm("キャラクター案を作れませんでした。概要を具体的にして、もう一度試してください".into()));
    }
    Ok(Drafts { drafts, warnings })
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
        (premise_of(&req, &p.description)?, saved_characters(&s, project_id)?)
    };
    // 採用前のキャラクター案も参考にできる。
    for c in &req.characters {
        if !chars.iter().any(|x| x.name == c.name) {
            chars.push(c.clone());
        }
    }
    let system = format!(
        "{COMMON}\n\n作品の概要と登場人物から、世界観・設定の案を作ります。\n\
         出力形式: {{\"title\": \"世界観\", \"content\": \"Markdown の本文\"}}\n\
         - content は見出し（## 舞台、## 時代・社会、## 用語、## ルール・制約、## 物語の鍵 など）を使って整理し、各項目は簡潔に書きます。\n\
         - 登場人物の設定と矛盾しないようにします。"
    );
    let mut prompt = format!("# 作品の概要\n{premise}\n");
    if !chars.is_empty() {
        prompt.push_str(&format!("\n# 登場人物\n{}", characters_text(&chars)));
    }
    if !req.instructions.trim().is_empty() {
        prompt.push_str(&format!("\n# 追加の指示\n{}\n", req.instructions.trim()));
    }
    let value = call(store, provider, env_api_key, project_id, system, prompt).await?;
    let content = clean(str_of(&value, "content"), 50_000)
        .ok_or_else(|| AppError::Llm("世界観の案を作れませんでした。もう一度試してください".into()))?;
    let title = clean(str_of(&value, "title"), 100).unwrap_or_else(|| WORLD_FILE_NAME.to_string());
    Ok(Drafts { drafts: WorldDraft { title, content }, warnings: vec![] })
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
            return Err(AppError::Invalid(
                "ストーリー案の前に、キャラクターを登録してください（キャラクター案を採用するか、手動で追加します）".into(),
            ));
        }
        let files = s.list_files(project_id)?;
        let world: Vec<String> = files
            .iter()
            .filter(|f| f.pinned || f.name.contains(WORLD_FILE_NAME))
            .map(|f| format!("## {}\n{}", f.name, f.content))
            .collect();
        let tree = s.project_tree(project_id)?;
        let outline: Vec<String> = tree
            .chapters
            .iter()
            .flat_map(|c| c.scenes.iter().map(move |sc| format!("- {} / {}: {}", c.chapter.title, sc.title, sc.summary)))
            .collect();
        (premise_of(&req, &p.description)?, chars, world, outline)
    };
    let system = format!(
        "{COMMON}\n\n作品の概要・登場人物・世界観から、ストーリーの構成案（章と場面の概要）を作ります。\n\
         出力形式: {{\"chapters\": [{{\"title\": \"章の題名\", \"scenes\": [{{\"title\": \"場面の題名\", \"summary\": \"その場面で起きること（1〜3 文）\"}}]}}]}}\n\
         - 登場人物の設定（性格・口調・目的）と世界観に一貫して従います。\n\
         - 既存の構成がある場合は、その続きとして新しい章を作ります。"
    );
    let mut prompt = format!("# 作品の概要\n{premise}\n\n# 登場人物\n{}", characters_text(&chars));
    if !world.is_empty() {
        prompt.push_str(&format!("\n# 世界観・設定\n{}\n", world.join("\n\n")));
    }
    if !outline.is_empty() {
        prompt.push_str(&format!("\n# 既存の構成\n{}\n", outline.join("\n")));
    }
    if !req.instructions.trim().is_empty() {
        prompt.push_str(&format!("\n# 追加の指示\n{}\n", req.instructions.trim()));
    }
    let value = call(store, provider, env_api_key, project_id, system, prompt).await?;
    let mut warnings = Vec::new();
    let mut drafts = Vec::new();
    for (i, c) in value.get("chapters").and_then(Value::as_array).cloned().unwrap_or_default().iter().enumerate() {
        let Some(title) = clean(str_of(c, "title"), 200).filter(|t| !t.chars().any(char::is_control)) else {
            warnings.push(format!("章 {} は題名が不正なため除外しました", i + 1));
            continue;
        };
        let scenes: Vec<SceneDraft> = c
            .get("scenes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|sc| {
                let title = clean(str_of(sc, "title"), 200).filter(|t| !t.chars().any(char::is_control))?;
                let summary = str_of(sc, "summary").trim().chars().take(2000).collect();
                Some(SceneDraft { title, summary })
            })
            .take(MAX_SCENES)
            .collect();
        if scenes.is_empty() {
            warnings.push(format!("章「{title}」には場面がないため除外しました"));
            continue;
        }
        drafts.push(ChapterDraft { title, scenes });
        if drafts.len() >= MAX_CHAPTERS {
            break;
        }
    }
    if drafts.is_empty() {
        return Err(AppError::Llm("ストーリー案を作れませんでした。もう一度試してください".into()));
    }
    Ok(Drafts { drafts, warnings })
}
