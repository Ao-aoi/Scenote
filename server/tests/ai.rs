//! AI チャット・提案レビュー・使用量制御の結合テスト。LLM は台本どおりに応答する模擬実装を使う。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use scenote::api::{router, AppState};
use scenote::llm::{LlmError, LlmFuture, LlmProvider, LlmRequest, LlmResponse};
use scenote::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET_KEY: &str = "test-secret-key-12345";

#[derive(Default)]
struct MockProvider {
    script: Mutex<VecDeque<Result<LlmResponse, LlmError>>>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl MockProvider {
    fn push(&self, text: &str, input: i64, output: i64) {
        self.script.lock().unwrap().push_back(Ok(LlmResponse {
            text: text.to_string(),
            input_tokens: input,
            output_tokens: output,
        }));
    }

    fn reply(&self, value: Value) {
        self.push(&value.to_string(), 100, 50);
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn last_prompt(&self) -> String {
        let seen = self.seen.lock().unwrap();
        seen.last().unwrap().messages.last().unwrap().text.clone()
    }
}

impl LlmProvider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn generate<'a>(&'a self, _model: &'a str, api_key: &'a str, request: &'a LlmRequest) -> LlmFuture<'a> {
        assert_eq!(api_key, SECRET_KEY);
        self.seen.lock().unwrap().push(request.clone());
        let next = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(LlmError("台本にない呼び出し".into())));
        Box::pin(async move { next })
    }
}

struct Env {
    app: Router,
    llm: Arc<MockProvider>,
    project: String,
    scene: String,
    chat: String,
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(path);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains(SECRET_KEY), "API キーが応答に含まれている: {path}");
    let value = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap() };
    (status, value)
}

/// プロジェクト・場面（要素 3 つ）・チャットを用意し、API キーを登録する。
async fn setup() -> Env {
    let llm = Arc::new(MockProvider::default());
    let app = router(AppState::new(Store::open_in_memory().unwrap(), llm.clone(), None), None);
    let (_, project) = call(&app, "POST", "/api/projects", Some(json!({ "name": "月夜の館" }))).await;
    let project = project["id"].as_str().unwrap().to_string();
    let (_, tree) = call(&app, "GET", &format!("/api/projects/{project}"), None).await;
    let scene = tree["chapters"][0]["scenes"][0]["id"].as_str().unwrap().to_string();
    for (kind, speaker, body) in [
        ("Narration", "", "館の扉が開いた。"),
        ("Dialogue", "執事", "ようこそ。"),
        ("Narration", "", "アリスは息をのんだ。"),
    ] {
        call(&app, "POST", &format!("/api/scenes/{scene}/elements"),
            Some(json!({ "kind": kind, "speaker": speaker, "body": body }))).await;
    }
    let (status, chat) = call(&app, "POST", &format!("/api/projects/{project}/chats"), Some(json!({ "title": "推敲" }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, settings) = call(&app, "PUT", "/api/settings/llm", Some(json!({ "api_key": SECRET_KEY }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["has_api_key"], true);
    Env { app, llm, project, scene, chat: chat["id"].as_str().unwrap().to_string() }
}

impl Env {
    async fn send(&self, content: &str) -> (StatusCode, Value) {
        call(&self.app, "POST", &format!("/api/chats/{}/messages", self.chat),
            Some(json!({ "content": content, "scene_id": self.scene }))).await
    }

    async fn bodies(&self) -> Vec<String> {
        let (_, scene) = call(&self.app, "GET", &format!("/api/scenes/{}", self.scene), None).await;
        scene["elements"].as_array().unwrap().iter().map(|e| e["body"].as_str().unwrap().to_string()).collect()
    }

    async fn decide(&self, proposal: &Value, action: &str) -> Value {
        let id = proposal["id"].as_str().unwrap();
        let (status, result) = call(&self.app, "POST", &format!("/api/proposals/{id}/{action}"), Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        result
    }
}

#[tokio::test]
async fn proposals_apply_only_after_approval() {
    let env = setup().await;
    env.llm.reply(json!({
        "reply": "台詞を丁寧にし、描写を足しました。",
        "proposals": [
            { "op": "update_element", "target": "e2", "body": "ようこそ、お待ちしておりました。", "reason": "丁寧に" },
            { "op": "insert_element", "after": "e2", "kind": "Direction", "body": "SE: 扉の閉まる音", "reason": "演出" },
            { "op": "insert_element", "after": "e2", "kind": "Narration", "body": "執事は深く一礼した。", "reason": "描写" },
            { "op": "delete_element", "target": "e3", "reason": "冗長" },
            { "op": "update_element", "target": "e1", "body": "館の扉が、音もなく開いた。", "reason": "雰囲気" }
        ]
    }));
    let (status, exchange) = env.send("執事の台詞を丁寧にして").await;
    assert_eq!(status, StatusCode::OK, "{exchange}");
    assert_eq!(exchange["messages"][0]["role"], "user");
    let proposals = exchange["messages"][1]["proposals"].as_array().unwrap().clone();
    assert_eq!(proposals.len(), 5);
    assert!(proposals.iter().all(|p| p["status"] == "pending"));

    // プロンプトには場面の要素が参照名つきで含まれる。
    let prompt = env.llm.last_prompt();
    assert!(prompt.contains(r#""ref":"e2""#) && prompt.contains("ようこそ。"));

    // 差分は変更箇所だけを示す。
    assert_eq!(proposals[0]["diff"], json!([
        { "op": "equal", "text": "ようこそ" },
        { "op": "insert", "text": "、お待ちしておりました" },
        { "op": "equal", "text": "。" }
    ]));
    assert_eq!(proposals[0]["before"]["speaker"], "執事");
    assert_eq!(proposals[0]["after"]["speaker"], "執事");

    // 承認するまで、既存データは一切変わらない。
    let original = vec!["館の扉が開いた。", "ようこそ。", "アリスは息をのんだ。"];
    assert_eq!(env.bodies().await, original);

    // 一部承認・一部却下。挿入は後ろの提案から先に承認しても、提案時の順序になる。
    assert_eq!(env.decide(&proposals[2], "approve").await["status"], "approved");
    assert_eq!(env.decide(&proposals[1], "approve").await["status"], "approved");
    assert_eq!(env.decide(&proposals[0], "approve").await["status"], "approved");
    let rejected = call(&env.app, "POST", &format!("/api/proposals/{}/reject", proposals[3]["id"].as_str().unwrap()),
        Some(json!({ "reason": "この一文は残したい" }))).await.1;
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(rejected["reject_reason"], "この一文は残したい");
    assert_eq!(env.bodies().await, vec![
        "館の扉が開いた。",
        "ようこそ、お待ちしておりました。",
        "SE: 扉の閉まる音",
        "執事は深く一礼した。",
        "アリスは息をのんだ。",
    ]);

    // 由来情報が記録される。
    let (_, scene) = call(&env.app, "GET", &format!("/api/scenes/{}", env.scene), None).await;
    let origins: Vec<&str> = scene["elements"].as_array().unwrap().iter().map(|e| e["origin"].as_str().unwrap()).collect();
    assert_eq!(origins, ["human", "ai_approved", "ai_approved", "ai_approved", "human"]);

    // 処理済みの提案は二度と適用できない（却下した提案を後から承認できない）。
    for p in [&proposals[3], &proposals[0]] {
        let (status, _) = call(&env.app, "POST", &format!("/api/proposals/{}/approve", p["id"].as_str().unwrap()), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // 提案後に人間が対象を編集していた場合は、上書きせず「適用不可」にする。
    let first = &scene["elements"][0];
    call(&env.app, "PATCH", &format!("/api/elements/{}", first["id"].as_str().unwrap()),
        Some(json!({ "base_revision": first["revision"], "body": "人間が先に直した。" }))).await;
    assert_eq!(env.decide(&proposals[4], "approve").await["status"], "stale");
    assert_eq!(env.bodies().await[0], "人間が先に直した。");

    // 履歴として保存されている。
    let (_, chat) = call(&env.app, "GET", &format!("/api/chats/{}", env.chat), None).await;
    let statuses: Vec<&str> = chat["messages"][1]["proposals"].as_array().unwrap().iter().map(|p| p["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, ["approved", "approved", "approved", "rejected", "stale"]);
}

#[tokio::test]
async fn invalid_llm_output_is_handled_safely() {
    let env = setup().await;

    // JSON として解釈できない応答: 何も保存しない。
    env.llm.push("すみません、うまく書けませんでした。", 10, 5);
    let (status, body) = env.send("書き直して").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "llm");
    let (_, chat) = call(&env.app, "GET", &format!("/api/chats/{}", env.chat), None).await;
    assert!(chat["messages"].as_array().unwrap().is_empty());

    // 不正な提案は除外し、正しい提案だけを残す。コードフェンスつきの応答も解釈する。
    let response = json!({
        "reply": "直しました。",
        "proposals": [
            { "op": "update_element", "target": "e9", "body": "存在しない要素" },
            { "op": "update_element", "target": "e1", "kind": "bad kind", "body": "種類が不正" },
            { "op": "update_element", "target": "e2", "body": "ようこそ。" },
            { "op": "update_element", "target": "e2", "speaker": "執事]", "body": "話者が不正" },
            { "op": "drop_table", "target": "e1" },
            { "op": "update_file", "target": "f1", "content": "参照していないファイル" },
            "文字列",
            { "op": "update_element", "target": "e3", "body": "アリスは小さく息をのんだ。" }
        ]
    });
    env.llm.push(&format!("```json\n{response}\n```"), 10, 5);
    let (status, exchange) = env.send("書き直して").await;
    assert_eq!(status, StatusCode::OK, "{exchange}");
    let message = &exchange["messages"][1];
    assert_eq!(message["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(message["proposals"][0]["after"]["body"], "アリスは小さく息をのんだ。");
    assert_eq!(message["warnings"].as_array().unwrap().len(), 7);
    assert_eq!(env.bodies().await[2], "アリスは息をのんだ。");

    // LLM の呼び出し自体が失敗した場合。
    let (status, body) = env.send("もう一度").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["message"], "台本にない呼び出し");
}

#[tokio::test]
async fn reference_files_follow_priority_rules() {
    let env = setup().await;
    let files_path = format!("/api/projects/{}/files", env.project);
    let mut ids = Vec::new();
    for (name, content) in [("世界観", "王国歴の設定"), ("アリス", "主人公の設定"), ("執事", "執事の設定"), ("没ネタ", "使わない案")] {
        let (status, file) = call(&env.app, "POST", &files_path, Some(json!({ "name": name, "content": content }))).await;
        assert_eq!(status, StatusCode::CREATED);
        ids.push(file["id"].as_str().unwrap().to_string());
    }
    // 世界観: プロジェクト共通で常に参照 / アリス: チャットで指定
    call(&env.app, "PATCH", &format!("/api/files/{}", ids[0]), Some(json!({ "base_revision": 1, "pinned": true }))).await;
    call(&env.app, "PATCH", &format!("/api/chats/{}", env.chat), Some(json!({ "file_ids": [ids[1]], "instructions": "敬体で書く" }))).await;

    // 候補が少量なので、選択のための呼び出しは行わず残りを自動で参照する。没ネタは依頼時に除外。
    env.llm.reply(json!({ "reply": "了解です。", "proposals": [] }));
    let (status, exchange) = call(&env.app, "POST", &format!("/api/chats/{}/messages", env.chat),
        Some(json!({ "content": "相談です", "exclude_file_ids": [ids[3]] }))).await;
    assert_eq!(status, StatusCode::OK, "{exchange}");
    assert_eq!(env.llm.calls(), 1);
    let referenced: Vec<(String, String)> = exchange["messages"][1]["referenced_files"].as_array().unwrap().iter()
        .map(|f| (f["name"].as_str().unwrap().to_string(), f["source"].as_str().unwrap().to_string())).collect();
    assert!(referenced.contains(&("世界観".into(), "project".into())));
    assert!(referenced.contains(&("アリス".into(), "chat".into())));
    assert!(referenced.contains(&("執事".into(), "auto".into())));
    assert_eq!(referenced.len(), 3);
    let prompt = env.llm.last_prompt();
    assert!(prompt.contains("王国歴の設定") && prompt.contains("敬体で書く"));
    assert!(!prompt.contains("使わない案"));
    assert!(prompt.contains("場面は開かれていません"));

    // 自動選択を無効にすると、指定したファイルだけを参照する。依頼時の追加が最優先。
    call(&env.app, "PATCH", &format!("/api/chats/{}", env.chat), Some(json!({ "auto_select": false }))).await;
    env.llm.reply(json!({ "reply": "了解です。", "proposals": [] }));
    let (_, exchange) = call(&env.app, "POST", &format!("/api/chats/{}/messages", env.chat),
        Some(json!({ "content": "続き", "include_file_ids": [ids[3]], "exclude_file_ids": [ids[0]] }))).await;
    let names: Vec<&str> = exchange["messages"][1]["referenced_files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["アリス", "没ネタ"]);
    // 過去のやり取りが履歴として渡される（依頼 2 件 + 応答 1 件）。
    assert_eq!(env.llm.seen.lock().unwrap().last().unwrap().messages.len(), 3);

    // 候補が大きい場合は、AI に選ばせてから本体を呼び出す。
    call(&env.app, "PATCH", &format!("/api/chats/{}", env.chat), Some(json!({ "auto_select": true, "file_ids": [] }))).await;
    let (_, big) = call(&env.app, "POST", &files_path, Some(json!({ "name": "年表", "content": "年表の本文。".repeat(2000) }))).await;
    let before = env.llm.calls();
    env.llm.reply(json!({ "files": ["f2", "f99"] }));
    let (status, _) = call(&env.app, "POST", &format!("/api/chats/{}/messages", env.chat), Some(json!({ "content": "年表を見て" }))).await;
    // 存在しない参照名を含む選択結果は採用しないが、依頼自体は失敗させない（台本切れで本体が失敗する）。
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    env.llm.reply(json!({ "files": ["f3"] }));
    env.llm.reply(json!({ "reply": "年表を確認しました。", "proposals": [
        { "op": "update_file", "target": "f2", "content": "年表の本文（改訂）。", "reason": "整理" },
        { "op": "create_file", "name": "用語集", "content": "- 王国歴: 暦", "reason": "新規" }
    ] }));
    let (status, exchange) = call(&env.app, "POST", &format!("/api/chats/{}/messages", env.chat), Some(json!({ "content": "年表を見て" }))).await;
    assert_eq!(status, StatusCode::OK, "{exchange}");
    assert_eq!(env.llm.calls(), before + 4);
    let message = &exchange["messages"][1];
    let names: Vec<&str> = message["referenced_files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["世界観", "年表"]);

    // 参照ファイルへの変更も提案扱いで、承認するまで反映しない。
    let big_id = big["id"].as_str().unwrap();
    let (_, file) = call(&env.app, "GET", &format!("/api/files/{big_id}"), None).await;
    assert!(file["content"].as_str().unwrap().len() > 1000);
    let proposals = message["proposals"].as_array().unwrap();
    assert_eq!(env.decide(&proposals[0], "approve").await["status"], "approved");
    let created = env.decide(&proposals[1], "approve").await;
    let (_, file) = call(&env.app, "GET", &format!("/api/files/{big_id}"), None).await;
    assert_eq!(file["content"], "年表の本文（改訂）。");
    let (_, file) = call(&env.app, "GET", &format!("/api/files/{}", created["result_id"].as_str().unwrap()), None).await;
    assert_eq!(file["name"], "用語集");
}

#[tokio::test]
async fn usage_is_counted_and_stops_at_limit() {
    let env = setup().await;
    let (_, settings) = call(&env.app, "PUT", "/api/settings/llm",
        Some(json!({ "monthly_token_limit": 1000, "warn_percent": 50, "model": "test-model" }))).await;
    assert_eq!(settings["monthly_token_limit"], 1000);

    let ok = json!({ "reply": "はい。", "proposals": [] }).to_string();
    env.llm.push(&ok, 300, 100);
    let (_, exchange) = env.send("1 回目").await;
    assert_eq!(exchange["usage"]["state"], "ok");
    assert_eq!(exchange["usage"]["total_tokens"], 400);

    env.llm.push(&ok, 200, 100);
    let (_, exchange) = env.send("2 回目").await;
    assert_eq!(exchange["usage"]["state"], "warning");

    env.llm.push(&ok, 250, 100);
    let (status, exchange) = env.send("3 回目").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exchange["usage"]["state"], "stopped");
    let (_, usage) = call(&env.app, "GET", "/api/usage", None).await;
    assert_eq!(usage["requests"], 3);
    assert_eq!((usage["input_tokens"].as_i64(), usage["output_tokens"].as_i64()), (Some(750), Some(300)));
    assert_eq!(usage["by_model"][0]["model"], "test-model");
    assert_eq!(usage["by_model"][0]["provider"], "mock");

    // 上限到達後は LLM を呼び出さない。明示的に再開するまで停止が続く。
    let calls = env.llm.calls();
    for _ in 0..2 {
        let (status, body) = env.send("4 回目").await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "llm_stopped");
    }
    assert_eq!(env.llm.calls(), calls);
    // 上限と無関係な設定変更では解除されない。
    call(&env.app, "PUT", "/api/settings/llm", Some(json!({ "model": "other-model" }))).await;
    assert_eq!(env.send("4 回目").await.0, StatusCode::TOO_MANY_REQUESTS);

    // 明示的な再開。
    let (_, usage) = call(&env.app, "POST", "/api/usage/resume", None).await;
    assert_eq!(usage["state"], "warning");
    env.llm.push(&ok, 10, 10);
    let (status, exchange) = env.send("再開後").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(exchange["usage"]["state"], "stopped");

    // 上限を引き上げると警告も消える。上限を使用量以下に下げると停止する。
    let (_, _) = call(&env.app, "PUT", "/api/settings/llm", Some(json!({ "monthly_token_limit": 100000 }))).await;
    assert_eq!(call(&env.app, "GET", "/api/usage", None).await.1["state"], "ok");
    call(&env.app, "PUT", "/api/settings/llm", Some(json!({ "monthly_token_limit": 500 }))).await;
    assert_eq!(call(&env.app, "GET", "/api/usage", None).await.1["state"], "stopped");
}

#[tokio::test]
async fn api_key_is_required_and_never_returned() {
    let env = setup().await;
    let (_, settings) = call(&env.app, "GET", "/api/settings/llm", None).await;
    assert_eq!(settings["has_api_key"], true);
    assert!(settings.get("api_key").is_none());
    // プロジェクトの書き出しにも含まれない（call() が全応答を検査している）。
    call(&env.app, "GET", &format!("/api/projects/{}/export/json", env.project), None).await;

    // キーを削除すると、LLM を呼び出さずに設定を促す。
    let (_, settings) = call(&env.app, "PUT", "/api/settings/llm", Some(json!({ "api_key": "" }))).await;
    assert_eq!(settings["has_api_key"], false);
    let (status, body) = env.send("こんにちは").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("API キー"));
    assert_eq!(env.llm.calls(), 0);
}

#[tokio::test]
async fn model_list_requires_key_and_reports_provider_errors() {
    let env = setup().await;
    // 模擬プロバイダーはモデル一覧に対応していないため、そのエラーを返す（キーは含まれない）。
    let (status, body) = call(&env.app, "GET", "/api/settings/llm/models", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "llm");
    call(&env.app, "PUT", "/api/settings/llm", Some(json!({ "api_key": "" }))).await;
    let (status, body) = call(&env.app, "GET", "/api/settings/llm/models", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("API キー"));
}
