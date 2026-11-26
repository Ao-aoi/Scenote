//! Cloudflare Access の署名つきトークン（JWT）の検証。
//!
//! Access を通過した要求には `Cf-Access-Jwt-Assertion` ヘッダーが付く。署名（RS256）を
//! チームの公開鍵で確かめ、発行元・対象（AUD）・有効期限を照合する。Access を経由しない経路
//! （設定漏れなど）からの要求はここで拒否する。

use std::cell::RefCell;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use serde::Deserialize;
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

pub const JWT_HEADER: &str = "Cf-Access-Jwt-Assertion";

/// 公開鍵の一覧を読み直す間隔（ミリ秒）。
const CERTS_TTL_MS: f64 = 60.0 * 60.0 * 1000.0;

thread_local! {
    static CERTS: RefCell<Option<(f64, Vec<Value>)>> = const { RefCell::new(None) };
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Aud,
    exp: f64,
    #[serde(default)]
    nbf: Option<f64>,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Aud {
    One(String),
    Many(Vec<String>),
}

impl Aud {
    fn contains(&self, aud: &str) -> bool {
        match self {
            Aud::One(a) => a == aud,
            Aud::Many(list) => list.iter().any(|a| a == aud),
        }
    }
}

pub struct AccessConfig {
    /// 例: `myteam.cloudflareaccess.com`
    pub team_domain: String,
    /// Access のアプリケーションの「Application Audience (AUD) Tag」
    pub aud: String,
}

/// トークンを検証し、利用者のメールアドレスを返す。失敗の理由はログ用（利用者には出さない）。
pub async fn verify(token: &str, config: &AccessConfig) -> Result<String, String> {
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("malformed token".into());
    };
    let header: Header = serde_json::from_slice(&b64(h)?).map_err(|_| "bad header")?;
    if header.alg != "RS256" {
        return Err("unexpected alg".into());
    }
    let claims: Claims = serde_json::from_slice(&b64(c)?).map_err(|_| "bad claims")?;
    let signature = b64(s)?;

    let issuer = format!("https://{}", config.team_domain);
    if claims.iss != issuer {
        return Err("unexpected issuer".into());
    }
    if !claims.aud.contains(&config.aud) {
        return Err("unexpected audience".into());
    }
    let now = js_sys::Date::now() / 1000.0;
    if claims.exp < now - 30.0 || claims.nbf.is_some_and(|nbf| nbf > now + 30.0) {
        return Err("token expired or not yet valid".into());
    }

    let jwk = find_key(&config.team_domain, &header.kid).await?;
    let signed = format!("{h}.{c}");
    if !verify_rs256(&jwk, signed.as_bytes(), &signature).await? {
        return Err("bad signature".into());
    }
    claims.email.ok_or_else(|| "no email in token".into())
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| "bad base64".into())
}

/// `kid` に一致する公開鍵を返す。見つからなければ一覧を取り直す（鍵の更新に追従するため）。
async fn find_key(team_domain: &str, kid: &str) -> Result<Value, String> {
    let now = js_sys::Date::now();
    let lookup = |keys: &[Value]| keys.iter().find(|k| k.get("kid").and_then(Value::as_str) == Some(kid)).cloned();
    let cached = CERTS.with(|c| c.borrow().as_ref().filter(|(t, _)| now - t < CERTS_TTL_MS).and_then(|(_, keys)| lookup(keys)));
    if let Some(k) = cached {
        return Ok(k);
    }
    let url = format!("https://{team_domain}/cdn-cgi/access/certs");
    let mut resp = worker::Fetch::Url(url.parse().map_err(|_| "bad team domain")?)
        .send()
        .await
        .map_err(|e| format!("fetch certs: {e}"))?;
    let body: Value = resp.json().await.map_err(|e| format!("certs json: {e}"))?;
    let keys = body.get("keys").and_then(Value::as_array).cloned().unwrap_or_default();
    let found = lookup(&keys);
    CERTS.with(|c| *c.borrow_mut() = Some((now, keys)));
    found.ok_or_else(|| "unknown kid".into())
}

fn js_err(e: JsValue) -> String {
    format!("{e:?}")
}

/// WebCrypto で RS256（RSASSA-PKCS1-v1_5 / SHA-256）の署名を検証する。
async fn verify_rs256(jwk: &Value, data: &[u8], signature: &[u8]) -> Result<bool, String> {
    let global = js_sys::global();
    let subtle = Reflect::get(&global, &"crypto".into())
        .and_then(|c| Reflect::get(&c, &"subtle".into()))
        .map_err(js_err)?;

    let algorithm = Object::new();
    Reflect::set(&algorithm, &"name".into(), &"RSASSA-PKCS1-v1_5".into()).map_err(js_err)?;
    let hash = Object::new();
    Reflect::set(&hash, &"name".into(), &"SHA-256".into()).map_err(js_err)?;
    Reflect::set(&algorithm, &"hash".into(), &hash).map_err(js_err)?;

    // 公開鍵として必要な項目だけを渡す。
    let key_json = serde_json::json!({
        "kty": jwk.get("kty"), "n": jwk.get("n"), "e": jwk.get("e"), "alg": "RS256", "ext": true,
    });
    let key_obj = js_sys::JSON::parse(&key_json.to_string()).map_err(js_err)?;
    let usages = Array::of1(&"verify".into());

    let import: js_sys::Function = Reflect::get(&subtle, &"importKey".into()).map_err(js_err)?.dyn_into().map_err(js_err)?;
    let args = Array::of5(&"jwk".into(), &key_obj, &algorithm, &JsValue::FALSE, &usages);
    let key = JsFuture::from(import.apply(&subtle, &args).map_err(js_err)?.dyn_into::<Promise>().map_err(js_err)?)
        .await
        .map_err(js_err)?;

    let verify: js_sys::Function = Reflect::get(&subtle, &"verify".into()).map_err(js_err)?.dyn_into().map_err(js_err)?;
    let args = Array::of4(&algorithm, &key, &Uint8Array::from(signature), &Uint8Array::from(data));
    let ok = JsFuture::from(verify.apply(&subtle, &args).map_err(js_err)?.dyn_into::<Promise>().map_err(js_err)?)
        .await
        .map_err(js_err)?;
    Ok(ok.as_bool().unwrap_or(false))
}
