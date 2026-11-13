//! AI との共同編集の中核: 参照ファイルの選択、プロンプトの組み立て、応答の検証。
//!
//! LLM に任せるのは文章生成・内容理解・参照ファイルの選択だけである。ID の対応づけ、
//! 応答の検証、差分計算、保存はすべてプログラム側で行い、AI の出力は必ず「提案」として保存する。

use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{AppError, Result};
use crate::llm::{LlmMessage, LlmProvider, LlmRequest, Role};
use crate::model::*;
use crate::store::Store;
use crate::store_ai::*;

/// 自動選択の候補がこの文字数以下なら、選択のための LLM 呼び出しを省いてすべて参照する。
const AUTO_INCLUDE_ALL_CHARS: usize = 6000;
/// 参照ファイル選択用に LLM へ見せる、各ファイル冒頭の文字数。
const PREVIEW_CHARS: usize = 300;
/// プロンプトに含める過去のやり取りの最大件数。
const HISTORY_MESSAGES: usize = 20;
const MAX_REQUEST_CHARS: usize = 20_000;
const MAX_PROPOSALS: usize = 50;

pub const SOURCE_REQUEST: &str = "request";
pub const SOURCE_CHAT: &str = "chat";
pub const SOURCE_PROJECT: &str = "project";
pub const SOURCE_AUTO: &str = "auto";

#[derive(Debug, Default)]
pub struct SendRequest {
    pub content: String,
    /// 依頼時に開いている場面。要素への変更提案はこの場面が対象になる。
    pub scene_id: Option<String>,
    /// この依頼に限って追加で参照するファイル。
    pub include_file_ids: Vec<String>,
    /// この依頼に限って参照から外すファイル。
    pub exclude_file_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Exchange {
    /// 保存したユーザーの依頼と AI の応答。
    pub messages: Vec<Message>,
    pub usage: UsageStatus,
}

/// ユーザーが明示的に指定した参照ファイルと、AI による自動選択の候補を決める。
///
/// 優先順位: 依頼時の除外 > 依頼時の追加 > チャットの指定 > プロジェクト共通（常に参照）> AI の自動選択。
/// AI の自動選択は候補から追加するだけで、ユーザーの指定を外したり、除外したファイルを戻したりしない。
pub fn select_fixed(
    files: &[ReferenceFile],
    chat: &Chat,
    include: &[String],
    exclude: &[String],
) -> (Vec<(ReferenceFile, &'static str)>, Vec<ReferenceFile>) {
    let mut fixed = Vec::new();
    let mut candidates = Vec::new();
    for file in files {
        if exclude.contains(&file.id) {
            continue;
        }
        let source = if include.contains(&file.id) {
            Some(SOURCE_REQUEST)
        } else if chat.file_ids.contains(&file.id) {
            Some(SOURCE_CHAT)
        } else if file.pinned {
            Some(SOURCE_PROJECT)
        } else {
            None
        };
        match source {
            Some(source) => fixed.push((file.clone(), source)),
            None if chat.auto_select => candidates.push(file.clone()),
            None => {}
        }
    }
    (fixed, candidates)
}

/// LLM の応答から JSON オブジェクトを取り出す。コードフェンスや前後の文章があっても許容する。
pub fn extract_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end])
        .ok()
        .filter(Value::is_object)
}

fn selection_request(candidates: &[ReferenceFile], request: &str) -> LlmRequest {
    let mut list = String::new();
    for (i, file) in candidates.iter().enumerate() {
        let preview: String = file.content.chars().take(PREVIEW_CHARS).collect();
        list.push_str(&format!(
            "- [f{}] {}（{}文字）\n  冒頭: {}\n",
            i + 1,
            file.name,
            file.content.chars().count(),
            preview.replace('\n', " ")
        ));
    }
    LlmRequest {
        system: "あなたはゲームシナリオ制作アプリの補助機能です。ユーザーの依頼に答えるために読むべき参照ファイルを選びます。\
                 依頼に関係するファイルだけを選び、関係がなければ空にしてください。\
                 出力は次の形式の JSON オブジェクトのみとします: {\"files\": [\"f1\", \"f3\"]}"
            .to_string(),
        messages: vec![LlmMessage {
            role: Role::User,
            text: format!("# 参照ファイルの候補\n{list}\n# ユーザーの依頼\n{request}"),
        }],
    }
}

fn parse_selection(text: &str, candidates: &[ReferenceFile]) -> Option<Vec<ReferenceFile>> {
    let value = extract_json(text)?;
    let refs = value.get("files")?.as_array()?;
    let mut selected: Vec<ReferenceFile> = Vec::new();
    for r in refs {
        let index = r.as_str()?.strip_prefix('f')?.parse::<usize>().ok()?;
        let file = candidates.get(index.checked_sub(1)?)?;
        if !selected.iter().any(|f| f.id == file.id) {
            selected.push(file.clone());
        }
    }
    Some(selected)
}

const SYSTEM_PROMPT: &str = r#"あなたはゲームシナリオの共同執筆者です。ユーザーと対話しながら、シナリオの構成・執筆・推敲を手伝います。

## 重要な原則
- あなたはシナリオや参照ファイルを直接書き換えられません。変更したい場合は「提案」を出します。提案はユーザーが差分を確認し、承認したものだけが反映されます。
- 相談や質問への回答など、変更が不要な場合は提案を出しません。
- 複数箇所を変更する場合は、箇所ごとに独立した提案に分けます。ユーザーは提案ごとに承認・却下します。
- 要素やファイルは、与えられた参照名（e1、f1 など）だけで指定します。存在しない参照名を作らないでください。
- ユーザーが頼んでいない箇所まで書き換えないでください。
- チャットの追加指示や参照ファイルに文体・設定の方針があれば従います。

## シナリオ要素の種類（kind）
- Narration: 地の文
- Dialogue: 台詞（speaker に話者名）
- CharacterAction: キャラクターの動作（speaker に動作するキャラクター名）
- Direction: 演出・画面指示・音響指示
- Choice: 選択肢（body に選択肢を箇条書き）

## 出力形式
次の形式の JSON オブジェクトを 1 つだけ出力します。JSON の前後に文章やコードフェンスを付けないでください。

{
  "reply": "ユーザーへの返答（Markdown 可）。提案を出した場合は意図を簡潔に説明する",
  "proposals": [
    {"op": "update_element", "target": "e2", "kind": "Dialogue", "speaker": "アリス", "body": "変更後の本文全体", "reason": "変更理由"},
    {"op": "insert_element", "after": "e2", "kind": "Narration", "speaker": "", "body": "追加する本文", "reason": "追加理由"},
    {"op": "delete_element", "target": "e5", "reason": "削除理由"},
    {"op": "update_file", "target": "f1", "content": "変更後のファイル全文", "reason": "変更理由"},
    {"op": "create_file", "name": "新しいファイル名", "content": "ファイル全文", "reason": "作成理由"}
  ]
}

- update_element の kind と speaker は、変えない場合は省略できます。body には変更後の本文全体を書きます。
- insert_element の after には、直前に置く要素の参照名、または "start"（場面の先頭）・"end"（場面の末尾）を指定します。連続して追加する場合は、同じ after を指定して並べたい順に書きます。
- 要素への提案は「編集中の場面」が示されている場合だけ出せます。
- 提案がない場合は "proposals": [] とします。"#;

fn kind_label(kind: &str) -> &str {
    match kind {
        KIND_NARRATION => "地の文",
        KIND_DIALOGUE => "台詞",
        KIND_CHARACTER_ACTION => "動作",
        KIND_DIRECTION => "演出",
        KIND_CHOICE => "選択肢",
        other => other,
    }
}

/// 依頼に添える文脈（プロジェクト情報・構成・編集中の場面・参照ファイル）を組み立てる。
pub fn build_context(
    tree: &ProjectTree,
    scene: Option<&SceneDetail>,
    files: &[ReferenceFile],
    chat: &Chat,
    preferences: &str,
    request: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# プロジェクト\n名前: {}\n", tree.project.name));
    if !tree.project.description.trim().is_empty() {
        out.push_str(&format!("説明: {}\n", tree.project.description.trim()));
    }
    out.push_str("\n# 構成\n");
    for node in &tree.chapters {
        out.push_str(&format!("- {}\n", node.chapter.title));
        for s in &node.scenes {
            let mark = if scene.is_some_and(|open| open.scene.id == s.id) {
                "（編集中）"
            } else {
                ""
            };
            out.push_str(&format!("  - {}{mark}\n", s.title));
        }
    }
    match scene {
        Some(scene) => {
            out.push_str(&format!("\n# 編集中の場面「{}」の要素\n", scene.scene.title));
            if scene.elements.is_empty() {
                out.push_str("（まだ要素がありません）\n");
            }
            for (i, e) in scene.elements.iter().enumerate() {
                let line = json!({
                    "ref": format!("e{}", i + 1),
                    "kind": e.kind,
                    "種類": kind_label(&e.kind),
                    "speaker": e.speaker,
                    "body": e.body,
                });
                out.push_str(&line.to_string());
                out.push('\n');
            }
        }
        None => out.push_str("\n# 編集中の場面\n（場面は開かれていません。要素への提案は出せません）\n"),
    }
    out.push_str("\n# 参照ファイル\n");
    if files.is_empty() {
        out.push_str("（参照ファイルはありません）\n");
    }
    for (i, file) in files.iter().enumerate() {
        out.push_str(&format!("## [f{}] {}\n{}\n\n", i + 1, file.name, file.content));
    }
    if !preferences.is_empty() {
        out.push('\n');
        out.push_str(preferences);
    }
    if !chat.instructions.trim().is_empty() {
        out.push_str(&format!("\n# このチャットの追加指示\n{}\n", chat.instructions.trim()));
    }
    out.push_str(&format!("\n# ユーザーの依頼\n{request}"));
    out
}

fn index_of(reference: Option<&str>, prefix: char, len: usize) -> Option<usize> {
    let n: usize = reference?.trim().strip_prefix(prefix)?.parse().ok()?;
    let index = n.checked_sub(1)?;
    (index < len).then_some(index)
}

fn validate_one(
    raw: &Value,
    scene: Option<&SceneDetail>,
    files: &[ReferenceFile],
) -> std::result::Result<Option<NewProposal>, String> {
    let text = |key: &str| raw.get(key).and_then(Value::as_str);
    let op = text("op").ok_or("op がありません")?;
    let reason = text("reason").unwrap_or("").trim().to_string();
    let element = |key: &str| -> std::result::Result<(&SceneDetail, &Element), String> {
        let scene = scene.ok_or("場面が開かれていないため、要素への提案は扱えません")?;
        let index = index_of(text(key), 'e', scene.elements.len())
            .ok_or_else(|| format!("存在しない要素を指しています（{}）", text(key).unwrap_or("指定なし")))?;
        Ok((scene, &scene.elements[index]))
    };
    let content = |fallback: Option<&Element>| -> std::result::Result<ElementContent, String> {
        let kind = text("kind").map(str::to_string).or_else(|| fallback.map(|e| e.kind.clone()));
        let speaker = text("speaker")
            .map(|s| s.trim().to_string())
            .or_else(|| fallback.map(|e| e.speaker.clone()));
        let content = ElementContent {
            kind: kind.ok_or("kind がありません")?,
            speaker: speaker.unwrap_or_default(),
            body: text("body").ok_or("body がありません")?.to_string(),
        };
        if !is_valid_kind(&content.kind) {
            return Err(format!("要素の種類が不正です（{}）", content.kind));
        }
        if !is_valid_speaker(&content.speaker) {
            return Err("キャラクター名に使えない文字が含まれています".to_string());
        }
        Ok(content)
    };
    let file = || -> std::result::Result<&ReferenceFile, String> {
        let index = index_of(text("target"), 'f', files.len())
            .ok_or_else(|| format!("存在しない参照ファイルを指しています（{}）", text("target").unwrap_or("指定なし")))?;
        Ok(&files[index])
    };
    let to_json = |c: &ElementContent| serde_json::to_value(c).unwrap_or(Value::Null);

    match op {
        OP_UPDATE_ELEMENT => {
            let (scene, e) = element("target")?;
            let after = content(Some(e))?;
            let before = ElementContent {
                kind: e.kind.clone(),
                speaker: e.speaker.clone(),
                body: e.body.clone(),
            };
            if after == before {
                return Ok(None);
            }
            Ok(Some(NewProposal {
                op: OP_UPDATE_ELEMENT,
                target_id: Some(e.id.clone()),
                scene_id: Some(scene.scene.id.clone()),
                anchor: None,
                base_revision: Some(e.revision),
                before: Some(to_json(&before)),
                after: Some(to_json(&after)),
                reason,
            }))
        }
        OP_DELETE_ELEMENT => {
            let (scene, e) = element("target")?;
            Ok(Some(NewProposal {
                op: OP_DELETE_ELEMENT,
                target_id: Some(e.id.clone()),
                scene_id: Some(scene.scene.id.clone()),
                anchor: None,
                base_revision: Some(e.revision),
                before: Some(element_json(e)),
                after: None,
                reason,
            }))
        }
        OP_INSERT_ELEMENT => {
            let scene = scene.ok_or("場面が開かれていないため、要素への提案は扱えません")?;
            let anchor = match text("after").map(str::trim) {
                Some("start") => ANCHOR_START.to_string(),
                Some("end") | None => ANCHOR_END.to_string(),
                Some(_) => element("after")?.1.id.clone(),
            };
            let after = content(None)?;
            Ok(Some(NewProposal {
                op: OP_INSERT_ELEMENT,
                target_id: None,
                scene_id: Some(scene.scene.id.clone()),
                anchor: Some(anchor),
                base_revision: None,
                before: None,
                after: Some(to_json(&after)),
                reason,
            }))
        }
        OP_UPDATE_FILE => {
            let f = file()?;
            let content = text("content").ok_or("content がありません")?;
            if content == f.content {
                return Ok(None);
            }
            Ok(Some(NewProposal {
                op: OP_UPDATE_FILE,
                target_id: Some(f.id.clone()),
                scene_id: None,
                anchor: None,
                base_revision: Some(f.revision),
                before: Some(json!({ "name": f.name, "content": f.content })),
                after: Some(json!({ "name": f.name, "content": content })),
                reason,
            }))
        }
        OP_CREATE_FILE => {
            let name = text("name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name がありません")?;
            if name.chars().count() > 200 || name.chars().any(char::is_control) {
                return Err("ファイル名が不正です".to_string());
            }
            let content = text("content").ok_or("content がありません")?;
            Ok(Some(NewProposal {
                op: OP_CREATE_FILE,
                target_id: None,
                scene_id: None,
                anchor: None,
                base_revision: None,
                before: None,
                after: Some(json!({ "name": name, "content": content })),
                reason,
            }))
        }
        other => Err(format!("未対応の操作です（{other}）")),
    }
}

/// LLM の応答を検証する。返答文、検証を通った提案、除外した内容についての注意を返す。
///
/// 応答全体が解釈できない場合は `Err`（何も保存しない）。
pub fn interpret_response(
    text: &str,
    scene: Option<&SceneDetail>,
    files: &[ReferenceFile],
) -> std::result::Result<(String, Vec<NewProposal>, Vec<String>), String> {
    let value = extract_json(text).ok_or("AI の応答を解釈できませんでした（JSON ではありません）")?;
    let reply = value
        .get("reply")
        .and_then(Value::as_str)
        .ok_or("AI の応答を解釈できませんでした（reply がありません）")?
        .trim()
        .to_string();
    let mut proposals = Vec::new();
    let mut warnings = Vec::new();
    let raw = match value.get("proposals") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => {
            warnings.push("提案の形式が不正だったため、すべて除外しました".to_string());
            Vec::new()
        }
    };
    for (i, item) in raw.iter().enumerate() {
        if proposals.len() >= MAX_PROPOSALS {
            warnings.push(format!("提案が多すぎるため、{MAX_PROPOSALS} 件を超えた分を除外しました"));
            break;
        }
        match validate_one(item, scene, files) {
            Ok(Some(p)) => proposals.push(p),
            Ok(None) => warnings.push(format!("提案 {} は現在の内容と同じため除外しました", i + 1)),
            Err(why) => warnings.push(format!("提案 {} を除外しました: {why}", i + 1)),
        }
    }
    if reply.is_empty() && proposals.is_empty() {
        return Err("AI の応答が空でした".to_string());
    }
    Ok((reply, proposals, warnings))
}

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// チャットの依頼を処理する。
///
/// 成功した場合のみ依頼と応答を保存する。提案は保存するだけで、承認されるまで反映しない。
pub async fn send_message(
    store: &Mutex<Store>,
    provider: &dyn LlmProvider,
    env_api_key: Option<&str>,
    chat_id: &str,
    request: SendRequest,
) -> Result<Exchange> {
    let content = request.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::Invalid("依頼の内容を入力してください".into()));
    }
    if content.chars().count() > MAX_REQUEST_CHARS {
        return Err(AppError::Invalid(format!(
            "依頼は{MAX_REQUEST_CHARS}文字以内で入力してください"
        )));
    }

    // 1. 必要な情報をまとめて読み出す（LLM 呼び出し中はロックを保持しない）。
    let (chat, tree, scene, fixed, candidates, history, model, api_key, preferences) = {
        let s = lock(store);
        s.ensure_llm_allowed()?;
        let chat = s.get_chat(chat_id)?;
        let api_key = match env_api_key {
            Some(key) => Some(key.to_string()),
            None => s.stored_api_key()?,
        }
        .ok_or_else(|| AppError::Invalid("API キーが設定されていません。「AI 設定」から登録してください".into()))?;
        let tree = s.project_tree(&chat.project_id)?;
        let scene = match &request.scene_id {
            Some(id) => {
                let detail = s.scene_detail(id)?;
                let in_project = tree
                    .chapters
                    .iter()
                    .any(|c| c.scenes.iter().any(|sc| sc.id == detail.scene.id));
                if !in_project {
                    return Err(AppError::Invalid("別のプロジェクトの場面は指定できません".into()));
                }
                Some(detail)
            }
            None => None,
        };
        let files = s.list_files(&chat.project_id)?;
        let (fixed, candidates) = select_fixed(
            &files,
            &chat,
            &request.include_file_ids,
            &request.exclude_file_ids,
        );
        let history = s.list_messages(chat_id)?;
        let preferences = s.preference_prompt(&chat.project_id)?;
        (chat, tree, scene, fixed, candidates, history, s.llm_model()?, api_key, preferences)
    };

    // 2. 参照ファイルの自動選択。候補が少量なら LLM を呼ばずにすべて参照する。
    let mut warnings: Vec<String> = Vec::new();
    let candidate_chars: usize = candidates.iter().map(|f| f.content.chars().count()).sum();
    let auto: Vec<ReferenceFile> = if candidates.is_empty() || candidate_chars <= AUTO_INCLUDE_ALL_CHARS {
        candidates
    } else {
        let response = provider
            .generate(&model, &api_key, &selection_request(&candidates, &content))
            .await
            .map_err(|e| AppError::Llm(e.0))?;
        let mut s = lock(store);
        s.record_usage(provider.name(), &model, "select_files", response.input_tokens, response.output_tokens)?;
        // 選択の呼び出しで上限に達した場合は、本体の呼び出しを行わない。
        s.ensure_llm_allowed()?;
        drop(s);
        parse_selection(&response.text, &candidates).unwrap_or_else(|| {
            warnings.push("参照ファイルの自動選択に失敗したため、指定されたファイルのみ参照しました".to_string());
            Vec::new()
        })
    };
    let mut selected: Vec<(ReferenceFile, &'static str)> = fixed;
    selected.extend(auto.into_iter().map(|f| (f, SOURCE_AUTO)));
    let files: Vec<ReferenceFile> = selected.iter().map(|(f, _)| f.clone()).collect();

    // 3. 本体の呼び出し。
    let mut messages: Vec<LlmMessage> = history
        .iter()
        .rev()
        .take(HISTORY_MESSAGES)
        .rev()
        .map(|m| LlmMessage {
            role: if m.role == "user" { Role::User } else { Role::Assistant },
            text: m.content.clone(),
        })
        .collect();
    messages.push(LlmMessage {
        role: Role::User,
        text: build_context(&tree, scene.as_ref(), &files, &chat, &preferences, &content),
    });
    let llm_request = LlmRequest {
        system: SYSTEM_PROMPT.to_string(),
        messages,
    };
    let response = provider
        .generate(&model, &api_key, &llm_request)
        .await
        .map_err(|e| AppError::Llm(e.0))?;

    // 4. 使用量を記録し、応答を検証して保存する。
    let mut s = lock(store);
    s.record_usage(provider.name(), &model, "chat", response.input_tokens, response.output_tokens)?;
    let (reply, proposals, mut response_warnings) =
        interpret_response(&response.text, scene.as_ref(), &files).map_err(AppError::Llm)?;
    warnings.append(&mut response_warnings);
    let referenced: Vec<Value> = selected
        .iter()
        .map(|(f, source)| json!({ "id": f.id, "name": f.name, "source": source }))
        .collect();
    let messages = s.add_exchange(
        chat_id,
        scene.as_ref().map(|d| d.scene.id.as_str()),
        &content,
        &reply,
        &Value::Array(referenced),
        &warnings,
        &proposals,
    )?;
    Ok(Exchange {
        messages,
        usage: s.usage_status()?,
    })
}
