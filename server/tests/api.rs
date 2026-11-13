//! HTTP API の結合テスト（インメモリ DB を使用）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use scenote::api::{router, AppState};
use scenote::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> Router {
    let provider = std::sync::Arc::new(scenote::llm::GeminiProvider::new());
    router(AppState::new(Store::open_in_memory().unwrap(), provider, None), None)
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, String) {
    // PC 本体（ループバック）の画面からの要求として送る。
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:8787")
        .header("x-scenote", "1")
        .extension(axum::extract::ConnectInfo("127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap()));
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
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn call_json(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (status, text) = call(app, method, path, body).await;
    let value = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap() };
    (status, value)
}

#[tokio::test]
async fn full_editing_flow() {
    let app = app();

    let (status, project) = call_json(
        &app,
        "POST",
        "/api/projects",
        Some(json!({ "name": "月夜の館", "templates": ["choice_adv"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pid = project["id"].as_str().unwrap();

    let (_, tree) = call_json(&app, "GET", &format!("/api/projects/{pid}"), None).await;
    let sid = tree["chapters"][0]["scenes"][0]["id"].as_str().unwrap().to_string();

    // 要素を 2 つ追加し、2 つ目を先頭へ挿入する。
    let (status, first) = call_json(
        &app,
        "POST",
        &format!("/api/scenes/{sid}/elements"),
        Some(json!({ "kind": "Narration", "body": "館の扉が開いた。" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, second) = call_json(
        &app,
        "POST",
        &format!("/api/scenes/{sid}/elements"),
        Some(json!({ "kind": "Dialogue", "speaker": "執事", "body": "ようこそ。", "position": 0 })),
    )
    .await;
    let first_id = first["element"]["id"].as_str().unwrap();
    let second_id = second["element"]["id"].as_str().unwrap();

    // 更新と、古いリビジョンによる競合。
    let (status, updated) = call_json(
        &app,
        "PATCH",
        &format!("/api/elements/{first_id}"),
        Some(json!({ "base_revision": 1, "body": "館の扉が、ひとりでに開いた。" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["element"]["revision"], 2);
    let (status, conflict) = call_json(
        &app,
        "PATCH",
        &format!("/api/elements/{first_id}"),
        Some(json!({ "base_revision": 1, "body": "上書きされてはいけない" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "conflict");
    assert_eq!(conflict["error"]["current_revision"], 2);

    // 並べ替え。
    let (status, _) = call_json(
        &app,
        "PUT",
        &format!("/api/scenes/{sid}/elements/order"),
        Some(json!({ "ids": [first_id, second_id] })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, scene) = call_json(&app, "GET", &format!("/api/scenes/{sid}"), None).await;
    assert_eq!(scene["elements"][0]["body"], "館の扉が、ひとりでに開いた。");
    assert_eq!(scene["elements"][1]["speaker"], "執事");
    assert_eq!(updated["scene_revision"].as_i64().unwrap() + 1, scene["revision"].as_i64().unwrap());

    // 場面の Markdown ソース編集。
    let (_, md) = call_json(&app, "GET", &format!("/api/scenes/{sid}/markdown"), None).await;
    assert_eq!(md["markdown"], "館の扉が、ひとりでに開いた。\n\n**執事**: ようこそ。\n");
    let (status, replaced) = call_json(
        &app,
        "PUT",
        &format!("/api/scenes/{sid}/markdown"),
        Some(json!({ "base_revision": md["revision"], "markdown": format!("{}\n[選択肢] - 入る\n- 帰る\n", md["markdown"].as_str().unwrap()) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replaced["elements"].as_array().unwrap().len(), 3);
    assert_eq!(replaced["elements"][0]["id"], first_id);
    assert_eq!(replaced["elements"][2]["kind"], "Choice");

    // エクスポート。
    let (status, exported) = call(&app, "GET", &format!("/api/projects/{pid}/export/markdown"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(exported.contains("# 第1章\n\n## 場面1\n\n館の扉が、ひとりでに開いた。"));
    let (_, exported) = call_json(&app, "GET", &format!("/api/projects/{pid}/export/json"), None).await;
    assert_eq!(exported["project"]["name"], "月夜の館");
    assert_eq!(exported["chapters"][0]["scenes"][0]["elements"][2]["body"], "- 入る\n- 帰る");

    // 削除。
    let (status, _) = call(&app, "DELETE", &format!("/api/elements/{second_id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, "DELETE", &format!("/api/projects/{pid}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, "GET", &format!("/api/scenes/{sid}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn errors_are_reported_as_json() {
    let app = app();
    let (status, body) = call_json(&app, "GET", "/api/projects/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (status, body) = call_json(&app, "POST", "/api/projects", Some(json!({ "name": "" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid");

    let (status, body) = call_json(&app, "GET", "/api/unknown", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (status, meta) = call_json(&app, "GET", "/api/meta", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["element_kinds"].as_array().unwrap().len(), 5);
}
