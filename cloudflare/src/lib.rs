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
        return env.assets("ASSETS")?.fetch_request(req).await;
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

    // 確認済みの利用者を付けて Durable Object へ渡す（外から付けられた同名のヘッダーは使わない）。
    let headers = Headers::new();
    for name in ["content-type", "upgrade", CLIENT_HEADER] {
        if let Some(v) = req.headers().get(name)? {
            headers.set(name, &v)?;
        }
    }
    headers.set(USER_HEADER, &email)?;
    let mut init = RequestInit::new();
    init.with_method(method.clone()).with_headers(headers);
    if mutating {
        init.with_body(Some(JsValue::from_str(&req.text().await?)));
    }
    let forwarded = Request::new_with_init(url.as_str(), &init)?;
    let stub = env.durable_object("SCENOTE")?.id_from_name("main")?.get_stub()?;
    stub.fetch_with_request(forwarded).await
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
            let r = store.migrate();
            let ok = r.is_ok();
            (r, ok)
        })??;
        self.migrated.set(true);
        Ok(())
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
        if path == "/api/events" {
            let pair = WebSocketPair::new()?;
            self.state.accept_web_socket(&pair.server);
            return Response::from_websocket(pair.client);
        }
        let method = req.method().to_string();
        let mutating = api::is_mutating(&method);
        let body = if mutating { req.text().await? } else { String::new() };
        let email = req.headers().get(USER_HEADER)?.unwrap_or_default();
        let client = req.headers().get(CLIENT_HEADER)?.filter(|v| v.len() <= 64);
        let api_path = path.strip_prefix("/api/").unwrap_or("").to_string();

        let response = if mutating {
            let store = self.store.clone();
            let (m, p) = (method.clone(), api_path.clone());
            self.run_tx(move || {
                let caller = Caller { email };
                let r = api::handle(&store, &ApiRequest { method: &m, path: &p, body: &body, caller: &caller });
                let commit = r.is_success();
                (r, commit)
            })
            .unwrap_or_else(|e| {
                console_error!("{e:?}");
                ApiResponse::error(&e)
            })
        } else {
            let caller = Caller { email };
            api::handle(&self.store, &ApiRequest { method: &method, path: &api_path, body: "", caller: &caller })
        };
        if mutating && response.is_success() {
            let scopes = scenote_core::events::scopes_for(&path);
            if !scopes.is_empty() {
                self.broadcast(scopes, client);
            }
        }
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
