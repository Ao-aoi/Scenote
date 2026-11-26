//! MCP クライアント（Claude・ChatGPT など）のための OAuth 2.1 認可サーバー。
//!
//! - クライアントの動的登録（RFC 7591）: `POST /oauth/register`
//! - 認可（認可コード + PKCE S256 のみ）: `GET/POST /oauth/authorize`
//!   この URL は Cloudflare Access で保護し、本人の確認は Access に任せる。利用者が画面で許可したときだけコードを発行する。
//! - トークン: `POST /oauth/token`（`authorization_code`・`refresh_token`）
//!
//! コードとトークンは SHA-256 のハッシュだけを保存する。リフレッシュトークンは使うたびに作り直す。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::error::AppError;
use crate::p;

pub const ACCESS_TOKEN_TTL_SECS: f64 = 60.0 * 60.0;
pub const REFRESH_TOKEN_TTL_SECS: f64 = 60.0 * 60.0 * 24.0 * 30.0;
const CODE_TTL_SECS: f64 = 5.0 * 60.0;
const MAX_CLIENTS: i64 = 200;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS oauth_clients (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    redirect_uris TEXT NOT NULL,
    created_at    REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS oauth_codes (
    code_hash    TEXT PRIMARY KEY,
    client_id    TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    challenge    TEXT NOT NULL,
    email        TEXT NOT NULL,
    expires_at   REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS oauth_tokens (
    token_hash TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    client_id  TEXT NOT NULL,
    email      TEXT NOT NULL,
    expires_at REAL NOT NULL
);
";

pub fn now() -> f64 {
    js_sys::Date::now() / 1000.0
}

pub fn hash(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

/// 推測できない乱数の文字列（32 バイト、`crypto.getRandomValues`）。
pub fn random_token() -> String {
    let bytes = js_sys::Uint8Array::new_with_length(32);
    let global = js_sys::global();
    let ok = js_sys::Reflect::get(&global, &"crypto".into()).and_then(|crypto| {
        let f: js_sys::Function = js_sys::Reflect::get(&crypto, &"getRandomValues".into())?.into();
        f.call1(&crypto, &bytes)
    });
    assert!(ok.is_ok(), "crypto.getRandomValues is available in Workers");
    URL_SAFE_NO_PAD.encode(bytes.to_vec())
}

/// OAuth の誤り（RFC 6749 の形式で返す）。
pub struct OAuthError {
    pub status: u16,
    pub error: &'static str,
    pub description: String,
}

impl OAuthError {
    fn new(status: u16, error: &'static str, description: impl Into<String>) -> Self {
        Self { status, error, description: description.into() }
    }

    pub fn body(&self) -> Value {
        json!({ "error": self.error, "error_description": self.description })
    }
}

impl From<AppError> for OAuthError {
    fn from(e: AppError) -> Self {
        if let AppError::Internal(d) = &e {
            worker::console_error!("oauth internal error: {d}");
        }
        OAuthError::new(500, "server_error", "サーバー内部でエラーが発生しました")
    }
}

type OResult<T> = std::result::Result<T, OAuthError>;

/// 転送先として認める URI か（https、または手元のアプリ用の http://localhost）。
fn acceptable_redirect(uri: &str) -> bool {
    let Ok(u) = url::Url::parse(uri) else { return false };
    if u.fragment().is_some() {
        return false;
    }
    match u.scheme() {
        "https" => true,
        "http" => matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

#[derive(Deserialize)]
struct RegisterBody {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
}

pub fn register(db: &Db, body: &str) -> OResult<Value> {
    let b: RegisterBody = serde_json::from_str(body)
        .map_err(|_| OAuthError::new(400, "invalid_client_metadata", "登録内容の形式が正しくありません"))?;
    if b.redirect_uris.is_empty() || b.redirect_uris.len() > 10 || !b.redirect_uris.iter().all(|u| acceptable_redirect(u)) {
        return Err(OAuthError::new(400, "invalid_redirect_uri", "redirect_uris が正しくありません"));
    }
    let count: i64 = db.scalar("SELECT COUNT(*) AS v FROM oauth_clients", p![])?;
    if count >= MAX_CLIENTS {
        return Err(OAuthError::new(400, "invalid_client_metadata", "登録できるクライアントの数の上限に達しました"));
    }
    let name: String = b.client_name.unwrap_or_default().chars().filter(|c| !c.is_control()).take(100).collect();
    let name = if name.trim().is_empty() { "MCP クライアント".to_string() } else { name };
    let id = random_token();
    db.exec(
        "INSERT INTO oauth_clients (id, name, redirect_uris, created_at) VALUES (?, ?, ?, ?)",
        p![id.as_str(), name.as_str(), json!(b.redirect_uris).to_string(), now()],
    )?;
    Ok(json!({
        "client_id": id,
        "client_name": name,
        "redirect_uris": b.redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "client_id_issued_at": now() as i64,
    }))
}

#[derive(Deserialize)]
struct ClientRow {
    name: String,
    redirect_uris: String,
}

/// 認可の要求（`/oauth/authorize` の引数）。
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeParams {
    #[serde(default)]
    pub response_type: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub code_challenge: String,
    #[serde(default)]
    pub code_challenge_method: String,
}

/// 認可の要求を検証し、クライアントの表示名を返す。
///
/// クライアントか転送先が不正な場合は、転送先へ戻さずに画面で知らせる（`Err`）。
pub fn check_authorize(db: &Db, p: &AuthorizeParams) -> OResult<String> {
    let client: ClientRow = db
        .opt("SELECT name, redirect_uris FROM oauth_clients WHERE id = ?", p![p.client_id.as_str()])?
        .ok_or_else(|| OAuthError::new(400, "invalid_client", "登録されていないクライアントです"))?;
    let uris: Vec<String> = serde_json::from_str(&client.redirect_uris).unwrap_or_default();
    if !uris.iter().any(|u| u == &p.redirect_uri) {
        return Err(OAuthError::new(400, "invalid_request", "転送先（redirect_uri）が登録と一致しません"));
    }
    if p.response_type != "code" {
        return Err(OAuthError::new(400, "unsupported_response_type", "response_type は code のみ使えます"));
    }
    if p.code_challenge_method != "S256" || p.code_challenge.len() < 43 || p.code_challenge.len() > 128 {
        return Err(OAuthError::new(400, "invalid_request", "PKCE（S256）が必要です"));
    }
    Ok(client.name)
}

/// 利用者が許可した要求に認可コードを発行し、転送先の URL を返す。
pub fn grant(db: &Db, p: &AuthorizeParams, email: &str) -> OResult<String> {
    check_authorize(db, p)?;
    let code = random_token();
    db.exec("DELETE FROM oauth_codes WHERE expires_at < ?", p![now()])?;
    db.exec(
        "INSERT INTO oauth_codes (code_hash, client_id, redirect_uri, challenge, email, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        p![hash(&code), p.client_id.as_str(), p.redirect_uri.as_str(), p.code_challenge.as_str(), email, now() + CODE_TTL_SECS],
    )?;
    let mut target = url::Url::parse(&p.redirect_uri).map_err(|_| OAuthError::new(400, "invalid_request", "転送先が正しくありません"))?;
    {
        let mut q = target.query_pairs_mut();
        q.append_pair("code", &code);
        if let Some(s) = &p.state {
            q.append_pair("state", s);
        }
    }
    Ok(target.to_string())
}

/// 利用者が拒否した場合の転送先。
pub fn deny_redirect(p: &AuthorizeParams) -> Option<String> {
    let mut target = url::Url::parse(&p.redirect_uri).ok()?;
    {
        let mut q = target.query_pairs_mut();
        q.append_pair("error", "access_denied");
        if let Some(s) = &p.state {
            q.append_pair("state", s);
        }
    }
    Some(target.to_string())
}

#[derive(Deserialize)]
struct CodeRow {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    email: String,
    expires_at: f64,
}

#[derive(Deserialize)]
struct TokenRow {
    client_id: String,
    email: String,
    expires_at: f64,
}

fn issue(db: &Db, client_id: &str, email: &str) -> OResult<Value> {
    let access = random_token();
    let refresh = random_token();
    let t = now();
    db.exec("DELETE FROM oauth_tokens WHERE expires_at < ?", p![t])?;
    db.exec(
        "INSERT INTO oauth_tokens (token_hash, kind, client_id, email, expires_at) VALUES (?, 'access', ?, ?, ?)",
        p![hash(&access), client_id, email, t + ACCESS_TOKEN_TTL_SECS],
    )?;
    db.exec(
        "INSERT INTO oauth_tokens (token_hash, kind, client_id, email, expires_at) VALUES (?, 'refresh', ?, ?, ?)",
        p![hash(&refresh), client_id, email, t + REFRESH_TOKEN_TTL_SECS],
    )?;
    Ok(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TOKEN_TTL_SECS as i64,
        "refresh_token": refresh,
    }))
}

/// トークンの発行（`application/x-www-form-urlencoded` の本文）。
pub fn token(db: &Db, body: &str) -> OResult<Value> {
    let form: std::collections::HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    let get = |k: &str| form.get(k).map(String::as_str).unwrap_or("");
    match get("grant_type") {
        "authorization_code" => {
            let code_hash = hash(get("code"));
            let row: Option<CodeRow> = db.opt(
                "SELECT client_id, redirect_uri, challenge, email, expires_at FROM oauth_codes WHERE code_hash = ?",
                p![code_hash.as_str()],
            )?;
            // コードは一度だけ使える（失敗しても無効にする）。
            db.exec("DELETE FROM oauth_codes WHERE code_hash = ?", p![code_hash.as_str()])?;
            let row = row.ok_or_else(|| OAuthError::new(400, "invalid_grant", "認可コードが無効です"))?;
            if row.expires_at < now() || row.client_id != get("client_id") || row.redirect_uri != get("redirect_uri") {
                return Err(OAuthError::new(400, "invalid_grant", "認可コードが無効です"));
            }
            if hash(get("code_verifier")) != row.challenge {
                return Err(OAuthError::new(400, "invalid_grant", "PKCE の検証に失敗しました"));
            }
            issue(db, &row.client_id, &row.email)
        }
        "refresh_token" => {
            let token_hash = hash(get("refresh_token"));
            let row: Option<TokenRow> = db.opt(
                "SELECT client_id, email, expires_at FROM oauth_tokens WHERE token_hash = ? AND kind = 'refresh'",
                p![token_hash.as_str()],
            )?;
            let row = row.ok_or_else(|| OAuthError::new(400, "invalid_grant", "リフレッシュトークンが無効です"))?;
            if row.expires_at < now() || (!get("client_id").is_empty() && row.client_id != get("client_id")) {
                return Err(OAuthError::new(400, "invalid_grant", "リフレッシュトークンが無効です"));
            }
            db.exec("DELETE FROM oauth_tokens WHERE token_hash = ?", p![token_hash.as_str()])?;
            issue(db, &row.client_id, &row.email)
        }
        _ => Err(OAuthError::new(400, "unsupported_grant_type", "grant_type が対応していません")),
    }
}

/// アクセストークンを確かめ、利用者のメールアドレスを返す。
pub fn verify_bearer(db: &Db, token: &str) -> crate::error::Result<Option<String>> {
    let row: Option<TokenRow> = db.opt(
        "SELECT client_id, email, expires_at FROM oauth_tokens WHERE token_hash = ? AND kind = 'access'",
        p![hash(token)],
    )?;
    Ok(row.filter(|r| r.expires_at >= now()).map(|r| r.email))
}

/// 接続中のクライアントの一覧（設定画面で解除するため）。
pub fn list_clients(db: &Db) -> crate::error::Result<Value> {
    #[derive(Deserialize, serde::Serialize)]
    struct Row {
        id: String,
        name: String,
        created_at: f64,
        active: i64,
    }
    let rows: Vec<Row> = db.all(
        "SELECT c.id, c.name, c.created_at,
                (SELECT COUNT(*) FROM oauth_tokens t WHERE t.client_id = c.id AND t.expires_at >= ?) AS active
         FROM oauth_clients c ORDER BY c.created_at DESC",
        p![now()],
    )?;
    Ok(json!(rows))
}

/// クライアントの接続を解除する（発行済みのトークンも無効にする）。
pub fn revoke_client(db: &Db, id: &str) -> crate::error::Result<()> {
    db.exec("DELETE FROM oauth_tokens WHERE client_id = ?", p![id])?;
    db.exec("DELETE FROM oauth_codes WHERE client_id = ?", p![id])?;
    db.exec("DELETE FROM oauth_clients WHERE id = ?", p![id])?;
    Ok(())
}

/// 認可の画面（利用者に許可を求める）。値はすべてエスケープして埋め込む。
pub fn consent_page(client_name: &str, email: &str, p: &AuthorizeParams) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
    };
    let hidden = |name: &str, value: &str| format!(r#"<input type="hidden" name="{}" value="{}">"#, esc(name), esc(value));
    let host = url::Url::parse(&p.redirect_uri).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Scenote への接続の許可</title>
<style>
body{{font-family:system-ui,sans-serif;background:#f6f5f1;color:#22201c;margin:0;padding:24px 16px}}
main{{max-width:480px;margin:0 auto;background:#fff;border:1px solid #dcd9d1;border-radius:12px;padding:24px}}
h1{{font-size:1.25rem;margin:0 0 12px}} p{{line-height:1.7}} .muted{{color:#6d6a63;font-size:.9rem}}
.actions{{display:flex;gap:12px;margin-top:20px}} button{{font-size:1rem;padding:10px 18px;border-radius:8px;border:1px solid #dcd9d1;background:#fff}}
button.primary{{background:#2f6f9f;color:#fff;border-color:#2f6f9f}}
@media (prefers-color-scheme: dark){{body{{background:#1b1c1e;color:#e8e6e1}}main{{background:#25272a;border-color:#3a3c40}}button{{background:#25272a;color:#e8e6e1;border-color:#3a3c40}}}}
</style></head>
<body><main>
<h1>Scenote への接続の許可</h1>
<p><strong>{name}</strong> が、あなた（{email}）の Scenote に接続しようとしています。</p>
<p>許可すると、この AI はすべてのプロジェクトの本文・構成を<strong>読み取り、直接書き換え</strong>られます。書き換えた本文には「AI（MCP）」の由来が記録されます。</p>
<p class="muted">転送先: {host}<br>接続は Scenote の設定からいつでも解除できます。</p>
<form method="post" action="/oauth/authorize">
{fields}
<div class="actions"><button class="primary" name="decision" value="allow" type="submit">許可する</button>
<button name="decision" value="deny" type="submit">拒否する</button></div>
</form>
</main></body></html>"#,
        name = esc(client_name),
        email = esc(email),
        host = esc(&host),
        fields = [
            hidden("response_type", &p.response_type),
            hidden("client_id", &p.client_id),
            hidden("redirect_uri", &p.redirect_uri),
            hidden("state", p.state.as_deref().unwrap_or("")),
            hidden("code_challenge", &p.code_challenge),
            hidden("code_challenge_method", &p.code_challenge_method),
        ]
        .join("\n"),
    )
}
