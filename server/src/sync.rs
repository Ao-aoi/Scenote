//! 端末間の同期（変更通知）と、要求ごとの認証。
//!
//! 状態を変える要求が成功するたびに、影響する範囲（場面・構成・ファイル・チャットなど）を
//! WebSocket で全端末へ通知する。各端末は通知を受けて必要な部分だけを読み直す。
//! 同時編集の競合は、従来どおりリビジョンの照合で検出する（通知は表示を新しくするためのもの）。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;

use crate::api::AppState;
use crate::auth::{cookie_token, is_local_request, passes_csrf_check, Principal};

/// 要求を送った画面の識別子（自分の変更による通知を無視するため）。
pub const CLIENT_HEADER: &str = "x-scenote-client";

#[derive(Debug, Clone, Serialize)]
pub struct ChangeEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// 影響する範囲: tree / scene / files / chat / prefs / settings / devices
    pub scopes: Vec<&'static str>,
    /// 変更を行った画面。
    pub client: Option<String>,
    /// 登録を解除された端末（`kind` が `device_revoked` のとき）。
    pub device_id: Option<String>,
}

pub type EventSender = broadcast::Sender<ChangeEvent>;

pub fn channel() -> EventSender {
    broadcast::channel(256).0
}

/// 要求のパスから、変更の影響範囲を決める。
pub fn scopes_for(path: &str) -> Vec<&'static str> {
    let rest = path.strip_prefix("/api/").unwrap_or(path);
    let mut parts = rest.split('/');
    let first = parts.next().unwrap_or("");
    let third = parts.nth(1).unwrap_or("");
    match (first, third) {
        ("elements", _) => vec!["scene"],
        ("scenes", "markdown" | "elements") => vec!["scene"],
        ("scenes", _) | ("chapters", _) => vec!["tree", "scene"],
        ("projects", "files") | ("files", _) => vec!["files"],
        ("projects", "chats") | ("chats", _) => vec!["chat"],
        ("projects", "preferences") | ("preferences", _) => vec!["prefs"],
        ("projects", "snapshots") => vec!["snapshots"],
        ("projects", "variables") | ("variables", _) => vec!["structure"],
        ("snapshots", _) => vec!["snapshots", "tree", "scene", "files"],
        // 提案の承認は、場面・ファイル・チャットのすべてに影響しうる。
        ("proposals", _) => vec!["chat", "scene", "files", "history"],
        ("projects", _) => vec!["tree", "projects"],
        ("settings" | "usage", _) => vec!["settings"],
        _ => vec![],
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": { "code": "unauthenticated", "message": "この端末は接続が許可されていません。PC の画面でペアリングコードを発行して入力してください" } })),
    )
        .into_response()
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": { "code": "forbidden", "message": message } }))).into_response()
}

/// 認証・CSRF 対策を行い、状態を変える要求が成功したら変更を通知する。
pub async fn guard(State(s): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if !path.starts_with("/api/") {
        // 画面のファイル（HTML・JS・CSS）はデータを含まないため、誰でも取得できる。
        return next.run(req).await;
    }
    if !passes_csrf_check(&req) {
        return forbidden("不正な送信元からの要求のため拒否しました");
    }
    let principal = if is_local_request(&req) {
        Some(Principal::Local)
    } else {
        match cookie_token(req.headers()) {
            Some(token) => match s.store().device_by_token(&token) {
                Ok(Some(d)) => Some(Principal::Device { id: d.id, name: d.name }),
                Ok(None) => None,
                Err(e) => return e.into_response(),
            },
            None => None,
        }
    };
    let public = matches!(path.as_str(), "/api/auth/status" | "/api/auth/pair");
    match &principal {
        None if !public => return unauthorized(),
        Some(p) if !p.is_local() && path.starts_with("/api/devices") => {
            return forbidden("端末の管理は PC 本体の画面からのみ行えます");
        }
        _ => {}
    }
    if let Some(p) = principal {
        req.extensions_mut().insert(p);
    }
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let client = req
        .headers()
        .get(CLIENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 64)
        .map(str::to_string);
    let response = next.run(req).await;
    if mutating && response.status().is_success() {
        let scopes = scopes_for(&path);
        if !scopes.is_empty() {
            // 受信側がいなくても失敗扱いにしない。
            let _ = s.events.send(ChangeEvent { kind: "change", scopes, client, device_id: None });
        }
    }
    response
}

/// 変更通知の WebSocket。
pub async fn events(
    State(s): State<AppState>,
    principal: axum::Extension<Principal>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // 他サイトのページから WebSocket を開かれないよう、Origin が Host と一致することを確かめる。
    let origin_ok = match (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        (Some(origin), Some(host)) => origin
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://"))
            .is_some_and(|(_, rest)| Some(rest.trim_end_matches('/')) == host.to_str().ok()),
        (None, _) => true,
        _ => false,
    };
    if !origin_ok {
        return forbidden("不正な送信元からの接続のため拒否しました");
    }
    let device_id = match principal.0 {
        Principal::Device { id, .. } => Some(id),
        Principal::Local => None,
    };
    let rx = s.events.subscribe();
    ws.on_upgrade(move |socket| run_socket(socket, rx, device_id))
}

async fn run_socket(mut socket: WebSocket, mut rx: broadcast::Receiver<ChangeEvent>, device_id: Option<String>) {
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tokio::select! {
            event = rx.recv() => {
                let event = match event {
                    Ok(e) => e,
                    // 通知を取りこぼした場合は、全体を読み直してもらう。
                    Err(broadcast::error::RecvError::Lagged(_)) => ChangeEvent {
                        kind: "resync", scopes: vec![], client: None, device_id: None,
                    },
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if event.kind == "device_revoked" && event.device_id.is_some() && event.device_id == device_id {
                    let _ = socket.send(Message::Text(json!({ "type": "revoked" }).to_string().into())).await;
                    break;
                }
                if event.kind == "device_revoked" {
                    continue;
                }
                let Ok(text) = serde_json::to_string(&event) else { continue };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_follow_paths() {
        assert_eq!(scopes_for("/api/elements/x"), ["scene"]);
        assert_eq!(scopes_for("/api/scenes/x/markdown"), ["scene"]);
        assert_eq!(scopes_for("/api/scenes/x"), ["tree", "scene"]);
        assert_eq!(scopes_for("/api/projects/p/files"), ["files"]);
        assert_eq!(scopes_for("/api/chats/c/messages"), ["chat"]);
        assert_eq!(scopes_for("/api/proposals/p/approve"), ["chat", "scene", "files", "history"]);
        assert_eq!(scopes_for("/api/snapshots/s/restore"), ["snapshots", "tree", "scene", "files"]);
        assert_eq!(scopes_for("/api/projects"), ["tree", "projects"]);
        assert_eq!(scopes_for("/api/projects/p/variables"), ["structure"]);
        assert!(scopes_for("/api/auth/pair").is_empty());
    }
}
