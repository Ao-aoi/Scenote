//! キャラクター基盤の HTTP API・書き出し・スナップショットの結合テスト。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use scenote::api::{router, AppState};
use scenote::llm::GeminiProvider;
use scenote::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> Router {
    router(AppState::new(Store::open_in_memory().unwrap(), Arc::new(GeminiProvider::new()), None), None)
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:8787")
        .header("x-scenote", "1")
        .extension(axum::extract::ConnectInfo("127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap()));
    let req = match body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn characters_round_trip_through_api_export_and_snapshots() {
    let app = app();
    let (_, p) = call(&app, "POST", "/api/projects", Some(json!({ "name": "月夜の館" }))).await;
    let pid = p["id"].as_str().unwrap();
    let (_, tree) = call(&app, "GET", &format!("/api/projects/{pid}"), None).await;
    let sid = tree["chapters"][0]["scenes"][0]["id"].as_str().unwrap().to_string();

    // キャラクター・情報・関係の作成。
    let (status, alice) = call(&app, "POST", &format!("/api/projects/{pid}/characters"), Some(json!({ "name": "アリス", "aliases": ["ありす"] }))).await;
    assert_eq!(status, StatusCode::CREATED, "{alice}");
    let aid = alice["id"].as_str().unwrap().to_string();
    let (_, bob) = call(&app, "POST", &format!("/api/projects/{pid}/characters"), Some(json!({ "name": "ボブ" }))).await;
    let bid = bob["id"].as_str().unwrap().to_string();
    let (status, fact) = call(&app, "POST", &format!("/api/characters/{aid}/facts"), Some(json!({ "category": "口調", "text": "丁寧語" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{fact}");
    let (status, rel) = call(&app, "POST", &format!("/api/projects/{pid}/character-relations"),
        Some(json!({ "from_character_id": aid, "to_character_id": bid, "kind": "信頼" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{rel}");

    // 要素: speaker を保ったまま character_id を保存・取得できる。
    let (status, created) = call(&app, "POST", &format!("/api/scenes/{sid}/elements"),
        Some(json!({ "kind": "Dialogue", "speaker": "アリス", "body": "ようこそ", "character_id": aid }))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let eid = created["element"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["element"]["character_id"], aid.as_str());
    assert_eq!(created["element"]["speaker"], "アリス");
    // 本文だけの更新では character_id は変わらない。null で外せる。
    let (_, upd) = call(&app, "PATCH", &format!("/api/elements/{eid}"), Some(json!({ "base_revision": 1, "body": "いらっしゃい" }))).await;
    assert_eq!(upd["element"]["character_id"], aid.as_str());
    let (_, upd) = call(&app, "PATCH", &format!("/api/elements/{eid}"), Some(json!({ "base_revision": 2, "character_id": null }))).await;
    assert_eq!(upd["element"]["character_id"], Value::Null);
    let (_, upd) = call(&app, "PATCH", &format!("/api/elements/{eid}"), Some(json!({ "base_revision": 3, "character_id": aid }))).await;
    assert_eq!(upd["element"]["character_id"], aid.as_str());
    // 他プロジェクトのキャラクターは結びつけられない。
    let (_, other) = call(&app, "POST", "/api/projects", Some(json!({ "name": "別作品" }))).await;
    let (_, foreign) = call(&app, "POST", &format!("/api/projects/{}/characters", other["id"].as_str().unwrap()), Some(json!({ "name": "他人" }))).await;
    let (status, _) = call(&app, "PATCH", &format!("/api/elements/{eid}"), Some(json!({ "base_revision": 4, "character_id": foreign["id"] }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, "POST", &format!("/api/projects/{pid}/character-relations"),
        Some(json!({ "from_character_id": aid, "to_character_id": foreign["id"], "kind": "x" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 一覧・取得・更新・削除。
    let (_, list) = call(&app, "GET", &format!("/api/projects/{pid}/characters"), None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let (status, _) = call(&app, "PATCH", &format!("/api/characters/{bid}"), Some(json!({ "name": "アリス" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "同名は不可");
    let (_, f2) = call(&app, "PATCH", &format!("/api/character-facts/{}", fact["id"].as_str().unwrap()), Some(json!({ "text": "丁寧語で話す" }))).await;
    assert_eq!(f2["text"], "丁寧語で話す");

    // JSON 書き出しに含まれる。
    let (_, exported) = call(&app, "GET", &format!("/api/projects/{pid}/export/json"), None).await;
    assert_eq!(exported["characters"][0]["name"], "アリス");
    assert_eq!(exported["characters"][0]["facts"][0]["text"], "丁寧語で話す");
    assert_eq!(exported["character_relations"][0]["kind"], "信頼");
    assert_eq!(exported["chapters"][0]["scenes"][0]["elements"][0]["character_id"], aid.as_str());

    // スナップショット → 変更 → 復元で、キャラクターと結びつきが戻る。
    let (_, snap) = call(&app, "POST", &format!("/api/projects/{pid}/snapshots"), Some(json!({ "note": "前" }))).await;
    call(&app, "DELETE", &format!("/api/characters/{aid}"), None).await;
    let (_, scene) = call(&app, "GET", &format!("/api/scenes/{sid}"), None).await;
    assert_eq!(scene["elements"][0]["character_id"], Value::Null);
    assert_eq!(scene["elements"][0]["speaker"], "アリス");
    let (status, _) = call(&app, "POST", &format!("/api/snapshots/{}/restore", snap["id"].as_str().unwrap()), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(&app, "GET", &format!("/api/projects/{pid}/characters"), None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let (_, facts) = call(&app, "GET", &format!("/api/characters/{aid}/facts"), None).await;
    assert_eq!(facts[0]["text"], "丁寧語で話す");
    let (_, rels) = call(&app, "GET", &format!("/api/projects/{pid}/character-relations"), None).await;
    assert_eq!(rels.as_array().unwrap().len(), 1);
    let (_, scene) = call(&app, "GET", &format!("/api/scenes/{sid}"), None).await;
    assert_eq!(scene["elements"][0]["character_id"], aid.as_str());

    // 削除。
    let (status, _) = call(&app, "DELETE", &format!("/api/character-relations/{}", rels[0]["id"].as_str().unwrap()), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, "GET", &format!("/api/characters/{}", "nope"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
