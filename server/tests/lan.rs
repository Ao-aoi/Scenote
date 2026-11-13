//! LAN の端末（iPad など）からの接続と認証の結合テスト。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use scenote::api::{router, AppState};
use scenote::llm::GeminiProvider;
use scenote::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const PC_HOST: &str = "192.168.1.10:8787";

fn app() -> Router {
    let state = AppState::new(Store::open_in_memory().unwrap(), Arc::new(GeminiProvider::new()), None)
        .with_lan_urls(vec![format!("http://{PC_HOST}")]);
    router(state, None)
}

#[derive(Clone, Copy)]
enum From {
    /// PC 本体のブラウザ。
    Pc,
    /// 同じ LAN の端末。
    Ipad,
}

struct Req<'a> {
    from: From,
    method: &'a str,
    path: &'a str,
    body: Option<Value>,
    cookie: Option<&'a str>,
    csrf: bool,
    host: Option<&'a str>,
}

impl<'a> Req<'a> {
    fn new(from: From, method: &'a str, path: &'a str) -> Self {
        Req { from, method, path, body: None, cookie: None, csrf: true, host: None }
    }
    fn body(mut self, v: Value) -> Self {
        self.body = Some(v);
        self
    }
    fn cookie(mut self, c: &'a str) -> Self {
        self.cookie = Some(c);
        self
    }
}

async fn send(app: &Router, r: Req<'_>) -> (StatusCode, Value, Option<String>) {
    let (peer, default_host) = match r.from {
        From::Pc => ("127.0.0.1:50000", "localhost:8787"),
        From::Ipad => ("192.168.1.20:50000", PC_HOST),
    };
    let mut b = Request::builder()
        .method(r.method)
        .uri(r.path)
        .header("host", r.host.unwrap_or(default_host))
        .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    if r.csrf {
        b = b.header("x-scenote", "1");
    }
    if let Some(c) = r.cookie {
        b = b.header("cookie", c);
    }
    let request = match r.body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, value, cookie)
}

async fn pairing_code(app: &Router) -> String {
    let (status, body, _) = send(app, Req::new(From::Pc, "POST", "/api/devices/pairing")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["urls"][0], format!("http://{PC_HOST}"));
    body["code"].as_str().unwrap().to_string()
}

async fn pair(app: &Router) -> String {
    let code = pairing_code(app).await;
    let (status, body, cookie) = send(app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": code, "device_name": "iPad" }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cookie = cookie.expect("cookie");
    assert!(cookie.starts_with("scenote_session="));
    cookie
}

#[tokio::test]
async fn unpaired_devices_are_rejected() {
    let app = app();
    // PC 本体はそのまま使える。
    let (status, _, _) = send(&app, Req::new(From::Pc, "GET", "/api/projects")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, s, _) = send(&app, Req::new(From::Pc, "GET", "/api/auth/status")).await;
    assert_eq!((s["authenticated"].as_bool(), s["local"].as_bool(), s["lan_enabled"].as_bool()), (Some(true), Some(true), Some(true)));

    // 未登録の端末は、状態の確認とペアリング以外はすべて拒否する。
    for (method, path) in [("GET", "/api/projects"), ("POST", "/api/projects"), ("GET", "/api/settings/llm"), ("GET", "/api/events"), ("GET", "/api/unknown")] {
        let (status, body, _) = send(&app, Req::new(From::Ipad, method, path).body(json!({ "name": "x" }))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(body["error"]["code"], "unauthenticated");
    }
    let (status, s, _) = send(&app, Req::new(From::Ipad, "GET", "/api/auth/status")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["authenticated"], false);
    // 偽のトークンも拒否する。
    let (status, _, _) = send(&app, Req::new(From::Ipad, "GET", "/api/projects").cookie("scenote_session=forged")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 外部サイトの名前を 127.0.0.1 に向けた要求（DNS リバインディング）は PC 本体として扱わない。
    let mut r = Req::new(From::Pc, "GET", "/api/projects");
    r.host = Some("evil.example.com:8787");
    assert_eq!(send(&app, r).await.0, StatusCode::UNAUTHORIZED);

    // 独自ヘッダーのない状態変更（他サイトからのフォーム送信など）は拒否する。
    let mut r = Req::new(From::Pc, "POST", "/api/projects").body(json!({ "name": "x" }));
    r.csrf = false;
    assert_eq!(send(&app, r).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn pairing_grants_access_until_revoked() {
    let app = app();
    // ペアリングコードを発行していない状態では登録できない。
    let (status, _, _) = send(&app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": "00000000" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // コードの発行は PC 本体のみ。
    let (status, _, _) = send(&app, Req::new(From::Ipad, "POST", "/api/devices/pairing")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = pair(&app).await;
    let (status, _, _) = send(&app, Req::new(From::Ipad, "POST", "/api/projects").body(json!({ "name": "iPad で作成" })).cookie(&cookie)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, projects, _) = send(&app, Req::new(From::Pc, "GET", "/api/projects")).await;
    assert_eq!(projects[0]["name"], "iPad で作成");
    let (_, s, _) = send(&app, Req::new(From::Ipad, "GET", "/api/auth/status").cookie(&cookie)).await;
    assert_eq!((s["authenticated"].as_bool(), s["local"].as_bool(), s["device_name"].as_str()), (Some(true), Some(false), Some("iPad")));

    // 登録済みの端末でも、端末の管理と API キーの変更はできない。
    let (status, _, _) = send(&app, Req::new(From::Ipad, "GET", "/api/devices").cookie(&cookie)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body, _) = send(&app, Req::new(From::Ipad, "PUT", "/api/settings/llm").body(json!({ "api_key": "leak" })).cookie(&cookie)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, settings, _) = send(&app, Req::new(From::Ipad, "PUT", "/api/settings/llm").body(json!({ "warn_percent": 70 })).cookie(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["has_api_key"], false);

    // PC 側で登録を解除すると、その端末は使えなくなる。
    let (_, devices, _) = send(&app, Req::new(From::Pc, "GET", "/api/devices")).await;
    assert_eq!(devices.as_array().unwrap().len(), 1);
    assert!(devices[0].get("token_hash").is_none());
    let id = devices[0]["id"].as_str().unwrap();
    let (status, _, _) = send(&app, Req::new(From::Pc, "DELETE", &format!("/api/devices/{id}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(&app, Req::new(From::Ipad, "GET", "/api/projects").cookie(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pairing_codes_are_single_use_and_lock_after_failures() {
    let app = app();
    let code = pairing_code(&app).await;
    let wrong = if code == "12345678" { "87654321" } else { "12345678" };
    for left in [4, 3, 2, 1] {
        let (status, body, _) = send(&app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": wrong }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"]["message"].as_str().unwrap().contains(&format!("あと {left} 回")));
    }
    // 5 回目の誤りでコードは無効になり、正しいコードでも登録できない。
    send(&app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": wrong }))).await;
    let (status, _, cookie) = send(&app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": code }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(cookie.is_none());

    // 正しいコードは一度だけ使える（区切りの空白やハイフンは無視する）。
    let code = pairing_code(&app).await;
    let spaced = format!("{} {}", &code[..4], &code[4..]);
    let (status, _, _) = send(&app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": spaced }))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, Req::new(From::Ipad, "POST", "/api/auth/pair").body(json!({ "code": code }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 端末側からの登録解除（ログアウト）。
    let cookie = pair(&app).await;
    let (status, _, cleared) = send(&app, Req::new(From::Ipad, "POST", "/api/auth/logout").cookie(&cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(cleared.as_deref(), Some("scenote_session="));
    let (status, _, _) = send(&app, Req::new(From::Ipad, "GET", "/api/projects").cookie(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pairing_requires_lan_mode() {
    let app = router(AppState::new(Store::open_in_memory().unwrap(), Arc::new(GeminiProvider::new()), None), None);
    let (status, body, _) = send(&app, Req::new(From::Pc, "POST", "/api/devices/pairing")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("SCENOTE_LAN=1"));
}
