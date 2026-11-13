//! LAN 内の端末（iPad など）の認証。
//!
//! 方式:
//! - PC 本体（ループバック接続、かつ Host が localhost 系）からの要求は信頼する。
//! - それ以外の端末は、PC の画面で発行した使い捨てのペアリングコードを入力して登録する。
//!   登録すると端末ごとのランダムなトークンを HttpOnly Cookie として渡し、DB にはハッシュだけを残す。
//! - 端末の一覧表示・登録解除・ペアリングコードの発行・API キーの変更は PC 本体からのみ行える。
//! - 状態を変える要求には独自ヘッダー `X-Scenote` を必須にし、他サイトからの送信（CSRF）を防ぐ。
//!   Host が localhost 系でない限りループバックとして扱わないことで、DNS リバインディングも防ぐ。

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use axum::extract::connect_info::ConnectInfo;
use axum::http::{header, HeaderMap, Method, Request};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::store::{new_id, Store, NOW};

pub const COOKIE_NAME: &str = "scenote_session";
/// 状態を変える要求に必須のヘッダー（他サイトのフォーム送信などでは付けられない）。
pub const CSRF_HEADER: &str = "x-scenote";
/// ペアリングコードの有効期間。
pub const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);
/// ペアリングコードを無効にするまでの入力ミスの回数。
pub const PAIRING_MAX_FAILURES: u32 = 5;
/// Cookie の有効期間（秒）。端末の登録は PC 側で解除するまで有効。
const COOKIE_MAX_AGE: u64 = 400 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_seen_at: String,
}

/// 要求の送信元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// PC 本体。
    Local,
    /// ペアリング済みの端末。
    Device { id: String, name: String },
}

impl Principal {
    pub fn is_local(&self) -> bool {
        matches!(self, Principal::Local)
    }
}

/// 発行中のペアリングコード（メモリ上にのみ保持する）。
#[derive(Debug, Clone)]
pub struct Pairing {
    pub code: String,
    pub expires: Instant,
    pub failures: u32,
}

impl Pairing {
    pub fn new() -> Self {
        // 8 桁の数字。UUID v4（OS の乱数）から作る。
        let n = u128::from_le_bytes(*Uuid::new_v4().as_bytes()) % 100_000_000;
        Pairing {
            code: format!("{n:08}"),
            expires: Instant::now() + PAIRING_TTL,
            failures: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        Instant::now() < self.expires && self.failures < PAIRING_MAX_FAILURES
    }
}

impl Default for Pairing {
    fn default() -> Self {
        Self::new()
    }
}

/// 端末用のトークン（推測できない 244 ビットの乱数）。
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn session_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE}")
}

pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

pub fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value.to_string())
        .filter(|v| !v.is_empty())
}

/// Host ヘッダーがこの PC 自身を指しているか（DNS リバインディング対策）。
fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { host })
    };
    let name = name.to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// 要求が PC 本体からのものか。
pub fn is_local_request<B>(req: &Request<B>) -> bool {
    let from_loopback = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback() || addr.ip().to_canonical().is_loopback());
    from_loopback && host_is_loopback(req.headers())
}

/// 状態を変える要求が、このアプリの画面から送られたものか（CSRF 対策）。
pub fn passes_csrf_check<B>(req: &Request<B>) -> bool {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    if req.headers().get(CSRF_HEADER).is_none() {
        return false;
    }
    // Origin が付いている場合は、Host と一致することを確かめる。
    match (req.headers().get(header::ORIGIN), req.headers().get(header::HOST)) {
        (Some(origin), Some(host)) => {
            let origin = origin.to_str().unwrap_or("");
            let host = host.to_str().unwrap_or("");
            origin
                .split_once("://")
                .is_some_and(|(_, rest)| rest.trim_end_matches('/') == host)
        }
        _ => true,
    }
}

impl Store {
    pub fn register_device(&mut self, name: &str) -> Result<(Device, String)> {
        let name = name.trim();
        let name = if name.is_empty() { "名前のない端末" } else { name };
        if name.chars().count() > 100 || name.chars().any(char::is_control) {
            return Err(AppError::Invalid("端末名は改行を含まない100文字以内で入力してください".into()));
        }
        let token = new_token();
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO devices (id, name, token_hash, created_at, last_seen_at)
                 VALUES (?1, ?2, ?3, {NOW}, {NOW})"
            ),
            params![id, name, hash_token(&token)],
        )?;
        Ok((self.get_device(&id)?, token))
    }

    fn get_device(&self, id: &str) -> Result<Device> {
        self.conn
            .query_row("SELECT id, name, created_at, last_seen_at FROM devices WHERE id = ?1", [id], |r| {
                Ok(Device {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                    last_seen_at: r.get(3)?,
                })
            })
            .optional()?
            .ok_or(AppError::NotFound("端末"))
    }

    /// トークンに対応する端末。見つかれば最終接続日時を更新する（1 分に 1 回まで）。
    pub fn device_by_token(&self, token: &str) -> Result<Option<Device>> {
        let device: Option<String> = self
            .conn
            .query_row("SELECT id FROM devices WHERE token_hash = ?1", [hash_token(token)], |r| r.get(0))
            .optional()?;
        let Some(id) = device else { return Ok(None) };
        self.conn.execute(
            &format!(
                "UPDATE devices SET last_seen_at = {NOW}
                 WHERE id = ?1 AND (julianday({NOW}) - julianday(last_seen_at)) * 86400 > 60"
            ),
            [&id],
        )?;
        self.get_device(&id).map(Some)
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at, last_seen_at FROM devices ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            Ok(Device {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                last_seen_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_device(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM devices WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("端末"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn req(addr: &str, host: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri("/api/projects");
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        let mut r = b.body(Body::empty()).unwrap();
        r.extensions_mut().insert(ConnectInfo(addr.parse::<SocketAddr>().unwrap()));
        r
    }

    #[test]
    fn local_requires_loopback_peer_and_host() {
        assert!(is_local_request(&req("127.0.0.1:5000", Some("127.0.0.1:8787"))));
        assert!(is_local_request(&req("127.0.0.1:5000", Some("localhost:8787"))));
        assert!(is_local_request(&req("[::1]:5000", Some("[::1]:8787"))));
        assert!(is_local_request(&req("[::ffff:127.0.0.1]:5000", Some("localhost"))));
        // LAN の端末。
        assert!(!is_local_request(&req("192.168.1.20:5000", Some("192.168.1.10:8787"))));
        assert!(!is_local_request(&req("192.168.1.20:5000", Some("localhost:8787"))));
        // DNS リバインディング（外部サイトの名前が 127.0.0.1 を指す）。
        assert!(!is_local_request(&req("127.0.0.1:5000", Some("evil.example.com:8787"))));
        assert!(!is_local_request(&req("127.0.0.1:5000", None)));
        // 接続元が分からない場合は信頼しない。
        let r = Request::builder().header(header::HOST, "localhost").body(Body::empty()).unwrap();
        assert!(!is_local_request(&r));
    }

    #[test]
    fn csrf_check_requires_header_and_matching_origin() {
        let make = |method: Method, header_set: bool, origin: Option<&str>| {
            let mut b = Request::builder().method(method).uri("/api/x").header(header::HOST, "192.168.1.10:8787");
            if header_set {
                b = b.header(CSRF_HEADER, "1");
            }
            if let Some(o) = origin {
                b = b.header(header::ORIGIN, o);
            }
            b.body(Body::empty()).unwrap()
        };
        assert!(passes_csrf_check(&make(Method::GET, false, None)));
        assert!(!passes_csrf_check(&make(Method::POST, false, None)));
        assert!(passes_csrf_check(&make(Method::POST, true, None)));
        assert!(passes_csrf_check(&make(Method::POST, true, Some("http://192.168.1.10:8787"))));
        assert!(!passes_csrf_check(&make(Method::POST, true, Some("http://evil.example.com"))));
    }

    #[test]
    fn tokens_and_cookies() {
        let t = new_token();
        assert_eq!(t.len(), 64);
        assert_ne!(t, new_token());
        assert_eq!(hash_token("a").len(), 64);
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, format!("other=1; {COOKIE_NAME}={t}").parse().unwrap());
        assert_eq!(cookie_token(&h).as_deref(), Some(t.as_str()));
        let p = Pairing::new();
        assert_eq!(p.code.len(), 8);
        assert!(p.is_valid());
    }
}
