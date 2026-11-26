//! Scenote の Cloudflare 版。
//!
//! - Worker（`fetch`）: 画面のファイルは静的アセットとして配信し、`/api/` への要求は
//!   Cloudflare Access のトークンを検証してから Durable Object へ渡す。
//! - Durable Object（`ScenoteData`）: 内蔵の SQLite にすべてのデータを保存する。個人利用のため
//!   インスタンスは1つ（名前 `main`）とし、要求は1つずつ順に処理される。
//!   状態を変える要求はトランザクションの中で処理し、失敗した場合は途中の書き込みを取り消す。
//!   成功したら、接続中の端末へ変更を WebSocket で通知する（形式は PC 版と同じ）。

pub mod access;
pub mod api;
pub mod db;
pub mod error;
pub mod mcp;
pub mod oauth;
pub mod store;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Function, Reflect};
use serde_json::json;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use worker::*;

use crate::api::{ApiRequest, ApiResponse, Body, Caller};
use crate::db::Db;
use crate::error::AppError;
use crate::store::Store;

/// Worker から Durable Object へ、確認済みの利用者を伝えるヘッダー。
const USER_HEADER: &str = "x-scenote-user";
/// 要求を送った画面の識別子（自分の変更による通知を無視するため）。
const CLIENT_HEADER: &str = "x-scenote-client";
/// 状態を変える要求に必須の独自ヘッダー（CSRF 対策。PC 版と同じ）。
const CSRF_HEADER: &str = "x-scenote";

fn json_error(status: u16, code: &str, message: &str) -> Result<Response> {
    Ok(Response::from_json(&json!({ "error": { "code": code, "message": message } }))?.with_status(status))
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).filter(|v| !v.trim().is_empty())
}

fn is_localhost(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// Origin がある場合は、要求先と同じ配信元かを確かめる。
fn same_origin(req: &Request, url: &Url) -> bool {
    match req.headers().get("Origin").ok().flatten() {
        None => true,
        Some(origin) => Url::parse(&origin).is_ok_and(|o| o.origin() == url.origin()),
    }
}

/// 要求者を確かめ、メールアドレスを返す。確かめられなければエラー応答を返す。
async fn authenticate(req: &Request, env: &Env, url: &Url) -> std::result::Result<String, Result<Response>> {
    // 開発用: `wrangler dev` を localhost で動かすときだけ、Access なしで通す。
    if let Some(user) = var(env, "SCENOTE_DEV_USER") {
        if is_localhost(url) {
            return Ok(user);
        }
    }
    let (Some(team_domain), Some(aud)) = (var(env, "ACCESS_TEAM_DOMAIN"), var(env, "ACCESS_AUD")) else {
        return Err(json_error(500, "config", "サーバーの設定が不足しています（ACCESS_TEAM_DOMAIN・ACCESS_AUD）"));
    };
    let Some(token) = req.headers().get(access::JWT_HEADER).ok().flatten() else {
        return Err(json_error(401, "unauthenticated", "Cloudflare Access でログインしてから開いてください"));
    };
    let email = match access::verify(&token, &access::AccessConfig { team_domain, aud }).await {
        Ok(email) => email,
        Err(reason) => {
            console_warn!("access token rejected: {reason}");
            return Err(json_error(401, "unauthenticated", "ログインの確認に失敗しました。ページを読み込み直してください"));
        }
    };
    // 任意: Access のポリシーに加えて、許可するアドレスをこちらでも限定する。
    if let Some(allowed) = var(env, "ALLOWED_EMAILS") {
        if !allowed.split(',').any(|a| a.trim().eq_ignore_ascii_case(&email)) {
            return Err(json_error(403, "forbidden", "このアカウントには利用が許可されていません"));
        }
    }
    Ok(email)
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    if !url.path().starts_with("/api/") {
        // 画面のファイル（通常は静的アセットとして Worker を通らずに配信される）。
        return match url.path() {
            p if p.starts_with("/.well-known/oauth-protected-resource") => protected_resource_metadata(&url),
            "/.well-known/oauth-authorization-server" => authorization_server_metadata(&url),
            // MCP とトークンの発行は Access を通らない（クライアントは Access にログインできない）。
            // 認証は Durable Object がアクセストークンで行う。
            "/mcp" | "/oauth/register" | "/oauth/token" => forward(&env, &mut req, &url, None).await,
            // 認可の画面は Access で保護し、ログインした本人に許可を求める。
            "/oauth/authorize" => {
                let email = match authenticate(&req, &env, &url).await {
                    Ok(e) => e,
                    Err(resp) => return resp,
                };
                if req.method() == Method::Post && !same_origin(&req, &url) {
                    return json_error(403, "forbidden", "不正な送信元からの要求のため拒否しました");
                }
                forward(&env, &mut req, &url, Some(&email)).await
            }
            _ => env.assets("ASSETS")?.fetch_request(req).await,
        };
    }
    let email = match authenticate(&req, &env, &url).await {
        Ok(e) => e,
        Err(resp) => return resp,
    };
    let method = req.method();
    let mutating = api::is_mutating(method.as_ref());
    let is_events = url.path() == "/api/events";
    if (mutating || is_events) && !same_origin(&req, &url) {
        return json_error(403, "forbidden", "不正な送信元からの要求のため拒否しました");
    }
    if mutating && req.headers().get(CSRF_HEADER)?.is_none() {
        return json_error(403, "forbidden", "不正な送信元からの要求のため拒否しました");
    }

    forward(&env, &mut req, &url, Some(&email)).await
}

/// 確認済みの利用者（あれば）を付けて Durable Object へ渡す（外から付けられた同名のヘッダーは使わない）。
async fn forward(env: &Env, req: &mut Request, url: &Url, email: Option<&str>) -> Result<Response> {
    let method = req.method();
    let headers = Headers::new();
    for name in ["content-type", "upgrade", "authorization", "mcp-protocol-version", CLIENT_HEADER] {
        if let Some(v) = req.headers().get(name)? {
            headers.set(name, &v)?;
        }
    }
    if let Some(email) = email {
        headers.set(USER_HEADER, email)?;
    }
    let mut init = RequestInit::new();
    // Durable Object の転送の応答（OAuth の 302 など）を、ここで追わずにそのまま返す。
    init.with_method(method.clone()).with_headers(headers).with_redirect(RequestRedirect::Manual);
    if api::is_mutating(method.as_ref()) {
        init.with_body(Some(JsValue::from_str(&req.text().await?)));
    }
    let forwarded = Request::new_with_init(url.as_str(), &init)?;
    let stub = env.durable_object("SCENOTE")?.id_from_name("main")?.get_stub()?;
    stub.fetch_with_request(forwarded).await
}

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// 保護されたリソースのメタデータ（RFC 9728）。MCP クライアントはここから認可サーバーを知る。
fn protected_resource_metadata(url: &Url) -> Result<Response> {
    let origin = origin_of(url);
    Response::from_json(&json!({
        "resource": format!("{origin}/mcp"),
        "authorization_servers": [origin],
        "bearer_methods_supported": ["header"],
        "resource_name": "Scenote",
    }))
}

/// 認可サーバーのメタデータ（RFC 8414）。
fn authorization_server_metadata(url: &Url) -> Result<Response> {
    let origin = origin_of(url);
    Response::from_json(&json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/oauth/authorize"),
        "token_endpoint": format!("{origin}/oauth/token"),
        "registration_endpoint": format!("{origin}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

#[durable_object]
pub struct ScenoteData {
    state: State,
    /// `ctx.storage`（`transactionSync` を呼ぶため）。
    storage: JsValue,
    store: Store,
    migrated: Cell<bool>,
}

impl ScenoteData {
    /// `f` をトランザクションの中で実行する。`f` が `false` を返したら書き込みを取り消す。
    fn run_tx<T: 'static>(&self, f: impl FnOnce() -> (T, bool) + 'static) -> std::result::Result<T, AppError> {
        let slot: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
        let out = slot.clone();
        let callback = Closure::once(move || -> std::result::Result<JsValue, JsValue> {
            let (value, commit) = f();
            *out.borrow_mut() = Some(value);
            if commit {
                Ok(JsValue::UNDEFINED)
            } else {
                // 例外を投げると、transactionSync がそれまでの書き込みを取り消す。
                Err(JsValue::from_str("scenote: rollback"))
            }
        });
        let result = Reflect::get(&self.storage, &"transactionSync".into())
            .and_then(|f| f.dyn_into::<Function>().map_err(JsValue::from))
            .and_then(|f| f.call1(&self.storage, callback.as_ref().unchecked_ref()));
        let value = slot.borrow_mut().take();
        match (value, result) {
            (Some(v), _) => Ok(v),
            (None, Err(e)) => Err(AppError::Internal(format!("transaction failed: {e:?}"))),
            (None, Ok(_)) => Err(AppError::Internal("transaction returned no value".into())),
        }
    }

    fn ensure_migrated(&self) -> std::result::Result<(), AppError> {
        if self.migrated.get() {
            return Ok(());
        }
        let store = self.store.clone();
        self.run_tx(move || {
            let r = store.migrate().and_then(|_| store.db.exec(oauth::SCHEMA, crate::p![]));
            let ok = r.is_ok();
            (r, ok)
        })??;
        self.migrated.set(true);
        Ok(())
    }

    /// API の要求を処理する。状態を変える要求はトランザクションの中で処理し、成功したら変更を通知する。
    fn execute(&self, store: Store, method: String, api_path: String, body: String, email: String, client: Option<String>) -> ApiResponse {
        let mutating = api::is_mutating(&method);
        let path = format!("/api/{api_path}");
        let response = if mutating {
            self.run_tx(move || {
                let caller = Caller { email };
                let r = api::handle(&store, &ApiRequest { method: &method, path: &api_path, body: &body, caller: &caller });
                let commit = r.is_success();
                (r, commit)
            })
            .unwrap_or_else(|e| {
                console_error!("{e:?}");
                ApiResponse::error(&e)
            })
        } else {
            let caller = Caller { email };
            api::handle(&store, &ApiRequest { method: &method, path: &api_path, body: &body, caller: &caller })
        };
        if mutating && response.is_success() {
            let scopes = scenote_core::events::scopes_for(&path);
            if !scopes.is_empty() {
                self.broadcast(scopes, client);
            }
        }
        response
    }

    async fn handle_oauth(&self, mut req: Request, url: &Url) -> Result<Response> {
        let method = req.method();
        let path = url.path().to_string();
        let body = if method == Method::Post { req.text().await? } else { String::new() };
        let oauth_json = |r: std::result::Result<serde_json::Value, oauth::OAuthError>, ok: u16| -> Result<Response> {
            let resp = match r {
                Ok(v) => Response::from_json(&v)?.with_status(ok),
                Err(e) => Response::from_json(&e.body())?.with_status(e.status),
            };
            resp.headers().set("cache-control", "no-store")?;
            Ok(resp)
        };
        match (method, path.as_str()) {
            (Method::Post, "/oauth/register") => {
                let db = self.store.db.clone();
                let r = self.run_tx(move || {
                    let r = oauth::register(&db, &body);
                    let ok = r.is_ok();
                    (r, ok)
                });
                oauth_json(r.unwrap_or_else(|e| Err(e.into())), 201)
            }
            (Method::Post, "/oauth/token") => {
                let db = self.store.db.clone();
                // 認可コードは失敗しても無効にするため、誤りの場合も書き込みを確定する。
                let r = self.run_tx(move || (oauth::token(&db, &body), true));
                oauth_json(r.unwrap_or_else(|e| Err(e.into())), 200)
            }
            (m @ (Method::Get | Method::Post), "/oauth/authorize") => {
                let email = req.headers().get(USER_HEADER)?.unwrap_or_default();
                if email.is_empty() {
                    return json_error(401, "unauthenticated", "ログインが必要です");
                }
                let source = if m == Method::Get { url.query().unwrap_or("").to_string() } else { body };
                let mut params: oauth::AuthorizeParams = serde_json::to_value(
                    url::form_urlencoded::parse(source.as_bytes()).into_owned().collect::<std::collections::HashMap<String, String>>(),
                )
                .ok()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or(oauth::AuthorizeParams {
                    response_type: String::new(), client_id: String::new(), redirect_uri: String::new(),
                    state: None, code_challenge: String::new(), code_challenge_method: String::new(),
                });
                if params.state.as_deref() == Some("") {
                    params.state = None;
                }
                let error_page = |e: oauth::OAuthError| -> Result<Response> {
                    Ok(Response::from_html(format!(
                        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>Scenote</title><p>接続を許可できません: {}</p>",
                        e.description.replace('<', "&lt;")
                    ))?.with_status(400))
                };
                if m == Method::Get {
                    return match oauth::check_authorize(&self.store.db, &params) {
                        Ok(name) => {
                            let resp = Response::from_html(oauth::consent_page(&name, &email, &params))?;
                            resp.headers().set("cache-control", "no-store")?;
                            resp.headers().set("x-frame-options", "DENY")?;
                            resp.headers().set("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'")?;
                            Ok(resp)
                        }
                        Err(e) => error_page(e),
                    };
                }
                let decision = url::form_urlencoded::parse(source.as_bytes())
                    .find(|(k, _)| k == "decision")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                if decision != "allow" {
                    if let Err(e) = oauth::check_authorize(&self.store.db, &params) {
                        return error_page(e);
                    }
                    return match oauth::deny_redirect(&params) {
                        Some(to) => Response::redirect(Url::parse(&to)?),
                        None => error_page(oauth::OAuthError { status: 400, error: "invalid_request", description: "転送先が正しくありません".into() }),
                    };
                }
                let db = self.store.db.clone();
                let r = self.run_tx(move || {
                    let r = oauth::grant(&db, &params, &email);
                    let ok = r.is_ok();
                    (r, ok)
                });
                match r.unwrap_or_else(|e| Err(e.into())) {
                    Ok(to) => Response::redirect(Url::parse(&to)?),
                    Err(e) => error_page(e),
                }
            }
            _ => json_error(405, "method_not_allowed", "対応していない要求です"),
        }
    }

    async fn handle_mcp(&self, mut req: Request, url: &Url) -> Result<Response> {
        let unauthorized = || -> Result<Response> {
            let resp = json_error(401, "unauthenticated", "アクセストークンが必要です")?;
            resp.headers().set(
                "www-authenticate",
                &format!(
                    "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
                    url.origin().ascii_serialization()
                ),
            )?;
            Ok(resp)
        };
        let token = req
            .headers()
            .get("authorization")?
            .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
        let Some(token) = token else { return unauthorized() };
        let email = match oauth::verify_bearer(&self.store.db, &token) {
            Ok(Some(e)) => e,
            Ok(None) => return unauthorized(),
            Err(e) => return to_response(ApiResponse::error(&e)),
        };
        if req.method() != Method::Post {
            // サーバーからの通知の配信（SSE）は使わない。
            return Ok(Response::empty()?.with_status(405));
        }
        let body = req.text().await?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&body) else {
            return Response::from_json(&mcp::rpc_error(&serde_json::Value::Null, -32700, "JSON を解釈できません"));
        };
        if message.is_array() {
            return Response::from_json(&mcp::rpc_error(&serde_json::Value::Null, -32600, "一括の要求には対応していません"));
        }
        let Some(id) = message.get("id").cloned() else {
            // 通知（initialized など）には応答を返さない。
            return Ok(Response::empty()?.with_status(202));
        };
        let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(serde_json::Value::Null);
        let result = match method {
            "initialize" => mcp::rpc_result(&id, mcp::initialize_result(params.get("protocolVersion").and_then(|v| v.as_str()))),
            "ping" => mcp::rpc_result(&id, json!({})),
            "tools/list" => mcp::rpc_result(&id, json!({ "tools": mcp::tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                mcp::rpc_result(&id, self.call_tool(name, &args, email))
            }
            _ => mcp::rpc_error(&id, -32601, "対応していないメソッドです"),
        };
        Response::from_json(&result)
    }

    /// MCP のツールを実行する。書き込みの由来は `ai_direct` になる。
    fn call_tool(&self, name: &str, args: &serde_json::Value, email: String) -> serde_json::Value {
        let mut store = self.store.clone();
        store.origin = scenote_core::model::ORIGIN_AI_DIRECT;
        let response = match mcp::to_call(name, args) {
            Err(message) => return mcp::tool_result(message, true),
            Ok(mcp::Call::Api { method, path, body }) => {
                let body = if body.is_null() { String::new() } else { body.to_string() };
                self.execute(store, method.to_string(), path, body, email, Some("mcp".into()))
            }
            Ok(mcp::Call::Append { scene_id, markdown }) => {
                let path = format!("/api/scenes/{scene_id}/markdown");
                let r = self.run_tx(move || {
                    let r = store.append_markdown(&scene_id, &markdown);
                    let ok = r.is_ok();
                    (r, ok)
                });
                let response = match r.and_then(|r| r) {
                    Ok(detail) => match serde_json::to_value(detail) {
                        Ok(v) => ApiResponse { status: 200, body: Body::Json(v) },
                        Err(e) => ApiResponse::error(&AppError::Internal(e.to_string())),
                    },
                    Err(e) => ApiResponse::error(&e),
                };
                if response.is_success() {
                    self.broadcast(scenote_core::events::scopes_for(&path), Some("mcp".into()));
                }
                response
            }
        };
        let is_error = !response.is_success();
        let text = match response.body {
            Body::Json(v) => serde_json::to_string_pretty(&v).unwrap_or_default(),
            Body::Download { text, .. } => text,
            Body::Empty => "完了しました".into(),
        };
        mcp::tool_result(text, is_error)
    }

    /// 変更を、接続中のすべての端末へ通知する。
    fn broadcast(&self, scopes: Vec<&'static str>, client: Option<String>) {
        let message = json!({ "type": "change", "scopes": scopes, "client": client, "device_id": null }).to_string();
        for ws in self.state.get_websockets() {
            // 切断済みの接続への送信の失敗は無視する。
            let _ = ws.send_with_str(&message);
        }
    }
}

fn to_response(r: ApiResponse) -> Result<Response> {
    let response = match r.body {
        Body::Json(v) => Response::from_json(&v)?.with_status(r.status),
        Body::Empty => Response::empty()?.with_status(r.status),
        Body::Download { content_type, disposition, text } => {
            let resp = Response::ok(text)?;
            resp.headers().set("content-type", content_type)?;
            resp.headers().set("content-disposition", &disposition)?;
            resp
        }
    };
    response.headers().set("cache-control", "no-store")?;
    Ok(response)
}

impl DurableObject for ScenoteData {
    fn new(state: State, _env: Env) -> Self {
        let inner = state._inner();
        let raw: JsValue = inner.clone().into();
        let storage = Reflect::get(&raw, &"storage".into()).unwrap_or(JsValue::UNDEFINED);
        let state = State::from(inner);
        let store = Store::new(Db::new(state.storage().sql()));
        Self { state, storage, store, migrated: Cell::new(false) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        if let Err(e) = self.ensure_migrated() {
            if let AppError::Internal(detail) = &e {
                console_error!("migration failed: {detail}");
            }
            return to_response(ApiResponse::error(&e));
        }
        let url = req.url()?;
        let path = url.path().to_string();
        if path.starts_with("/oauth/") {
            return self.handle_oauth(req, &url).await;
        }
        if path == "/mcp" {
            return self.handle_mcp(req, &url).await;
        }
        if path == "/api/events" {
            let pair = WebSocketPair::new()?;
            self.state.accept_web_socket(&pair.server);
            return Response::from_websocket(pair.client);
        }
        let method = req.method().to_string();
        let body = if api::is_mutating(&method) { req.text().await? } else { String::new() };
        let email = req.headers().get(USER_HEADER)?.unwrap_or_default();
        let client = req.headers().get(CLIENT_HEADER)?.filter(|v| v.len() <= 64);
        let api_path = path.strip_prefix("/api/").unwrap_or("").to_string();
        let response = self.execute(self.store.clone(), method, api_path, body, email, client);
        to_response(response)
    }

    async fn websocket_message(&self, _ws: WebSocket, _message: WebSocketIncomingMessage) -> Result<()> {
        // 端末からの送信は使わない（通知は一方向）。
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _was_clean: bool) -> Result<()> {
        let _ = ws.close(Some(1000), Some("closed"));
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        let _ = ws.close(Some(1011), Some("error"));
        Ok(())
    }
}
