//! LLM の呼び出し（Gemini）と、設定・使用量の管理（Cloudflare 版）。
//!
//! - 要求の組み立てと応答の解釈は `scenote_core::llm` を共有する。送受信は Workers の `fetch` で行う。
//! - API キーは Cloudflare の秘密情報 `GEMINI_API_KEY`（`wrangler secret put`）から読む。DB には保存せず、
//!   画面へも返さない（設定済みかどうかだけを返す）。
//! - 使用量の上限と自動停止の規則は PC 版（`server/src/store_ai.rs`）と同じ。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::{Env, Fetch, Headers, Method, Request, RequestInit};

use scenote_core::llm::*;

use crate::db::NOW;
use crate::error::{AppError, Result};
use crate::p;
use crate::store::Store;

const KEY_MODEL: &str = "llm.model";
const KEY_TOKEN_LIMIT: &str = "usage.monthly_token_limit";
const KEY_WARN_PERCENT: &str = "usage.warn_percent";
const KEY_STOPPED: &str = "usage.stopped";
const KEY_OVERRIDE_MONTH: &str = "usage.override_month";
pub const DEFAULT_MODEL: &str = "gemini-flash-latest";
pub const DEFAULT_TOKEN_LIMIT: i64 = 2_000_000;
pub const DEFAULT_WARN_PERCENT: i64 = 80;
pub const PROVIDER: &str = "gemini";
pub const API_KEY_SECRET: &str = "GEMINI_API_KEY";

#[derive(Debug, Clone, Serialize)]
pub struct LlmSettings {
    pub provider: &'static str,
    pub model: String,
    pub has_api_key: bool,
    pub monthly_token_limit: i64,
    pub warn_percent: i64,
    /// API キーを画面から登録できるか（Cloudflare 版は秘密情報で設定するため false）。
    pub api_key_editable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageStatus {
    pub month: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub monthly_token_limit: i64,
    pub warn_percent: i64,
    pub state: &'static str,
    pub by_model: Vec<ModelUsage>,
}

#[derive(Deserialize)]
pub struct UpdateLlmSettings {
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub monthly_token_limit: Option<i64>,
    pub warn_percent: Option<i64>,
}

#[derive(Deserialize)]
struct ValueRow {
    value: String,
}

#[derive(Deserialize)]
struct MonthRow {
    month: String,
    total: i64,
}

/// 秘密情報の API キー。なければ `None`。
pub fn api_key(env: &Env) -> Option<String> {
    env.secret(API_KEY_SECRET).ok().map(|s| s.to_string()).filter(|k| !k.trim().is_empty())
}

pub fn require_api_key(env: &Env) -> Result<String> {
    api_key(env).ok_or_else(|| {
        AppError::Invalid(format!(
            "Gemini の API キーが設定されていません。Cloudflare の秘密情報 {API_KEY_SECRET} に登録してください（手順は docs/deploy-cloudflare.md）"
        ))
    })
}

impl Store {
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.opt::<ValueRow>("SELECT value FROM settings WHERE key = ?", p![key])?.map(|r| r.value))
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.db.exec(
                "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                p![key, v],
            ),
            None => self.db.exec("DELETE FROM settings WHERE key = ?", p![key]),
        }
    }

    fn int_setting(&self, key: &str, default: i64) -> Result<i64> {
        Ok(self.setting(key)?.and_then(|v| v.parse().ok()).unwrap_or(default))
    }

    pub fn llm_model(&self) -> Result<String> {
        Ok(self.setting(KEY_MODEL)?.unwrap_or_else(|| DEFAULT_MODEL.to_string()))
    }

    pub fn llm_settings(&self, has_api_key: bool) -> Result<LlmSettings> {
        Ok(LlmSettings {
            provider: PROVIDER,
            model: self.llm_model()?,
            has_api_key,
            monthly_token_limit: self.int_setting(KEY_TOKEN_LIMIT, DEFAULT_TOKEN_LIMIT)?,
            warn_percent: self.int_setting(KEY_WARN_PERCENT, DEFAULT_WARN_PERCENT)?,
            api_key_editable: false,
        })
    }

    pub fn update_llm_settings(&self, req: UpdateLlmSettings) -> Result<()> {
        if req.api_key.is_some() {
            return Err(AppError::Forbidden(format!(
                "Cloudflare 版では API キーを画面から登録できません。Cloudflare の秘密情報 {API_KEY_SECRET} に登録してください"
            )));
        }
        if let Some(model) = req.model {
            let model = model.trim();
            let ok = !model.is_empty()
                && model.len() <= 100
                && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
            if !ok {
                return Err(AppError::Invalid("モデル名は英数字と - . _ で指定してください（例: gemini-flash-latest）".into()));
            }
            self.set_setting(KEY_MODEL, Some(model))?;
        }
        if let Some(percent) = req.warn_percent {
            if !(1..=100).contains(&percent) {
                return Err(AppError::Invalid("警告のしきい値は 1〜100 % で指定してください".into()));
            }
            self.set_setting(KEY_WARN_PERCENT, Some(&percent.to_string()))?;
        }
        if let Some(limit) = req.monthly_token_limit {
            if limit < 0 {
                return Err(AppError::Invalid("上限は 0 以上で指定してください（0 は無制限）".into()));
            }
            self.set_setting(KEY_TOKEN_LIMIT, Some(&limit.to_string()))?;
            // 上限の変更はユーザーの明示操作なので、新しい上限で停止状態を判定し直す。
            self.set_setting(KEY_OVERRIDE_MONTH, None)?;
            let (_, total) = self.month_total()?;
            let stopped = limit > 0 && total >= limit;
            self.set_setting(KEY_STOPPED, if stopped { Some("1") } else { None })?;
        }
        Ok(())
    }

    fn month_total(&self) -> Result<(String, i64)> {
        let r: MonthRow = self.db.one(
            "SELECT strftime('%Y-%m','now') AS month,
                    COALESCE((SELECT SUM(input_tokens + output_tokens) FROM llm_usage
                              WHERE substr(created_at, 1, 7) = strftime('%Y-%m','now')), 0) AS total",
            p![],
        )?;
        Ok((r.month, r.total))
    }

    /// LLM を呼び出してよいか確認する。停止中なら呼び出さない。
    pub fn ensure_llm_allowed(&self) -> Result<()> {
        if self.setting(KEY_STOPPED)?.is_some() {
            return Err(AppError::LlmStopped);
        }
        Ok(())
    }

    /// 1 回の呼び出しの使用量を記録し、上限に達していれば停止状態にする。
    pub fn record_usage(&self, model: &str, purpose: &str, input_tokens: i64, output_tokens: i64) -> Result<()> {
        self.db.exec(
            &format!(
                "INSERT INTO llm_usage (provider, model, purpose, input_tokens, output_tokens, created_at)
                 VALUES (?, ?, ?, ?, ?, {NOW})"
            ),
            p![PROVIDER, model, purpose, input_tokens.max(0), output_tokens.max(0)],
        )?;
        let limit = self.int_setting(KEY_TOKEN_LIMIT, DEFAULT_TOKEN_LIMIT)?;
        let (month, total) = self.month_total()?;
        let overridden = self.setting(KEY_OVERRIDE_MONTH)?.as_deref() == Some(month.as_str());
        if limit > 0 && total >= limit && !overridden {
            self.set_setting(KEY_STOPPED, Some("1"))?;
        }
        Ok(())
    }

    /// ユーザーの明示操作で停止を解除する。当月は上限を超えていても自動停止しない。
    pub fn resume_llm(&self) -> Result<()> {
        let (month, _) = self.month_total()?;
        self.set_setting(KEY_STOPPED, None)?;
        self.set_setting(KEY_OVERRIDE_MONTH, Some(&month))
    }

    pub fn usage_status(&self) -> Result<UsageStatus> {
        let (month, total) = self.month_total()?;
        let by_model: Vec<ModelUsage> = self.db.all(
            "SELECT provider, model, COUNT(*) AS requests, SUM(input_tokens) AS input_tokens, SUM(output_tokens) AS output_tokens
             FROM llm_usage WHERE substr(created_at, 1, 7) = ? GROUP BY provider, model ORDER BY provider, model",
            p![month.as_str()],
        )?;
        let limit = self.int_setting(KEY_TOKEN_LIMIT, DEFAULT_TOKEN_LIMIT)?;
        let warn_percent = self.int_setting(KEY_WARN_PERCENT, DEFAULT_WARN_PERCENT)?;
        let state = if self.setting(KEY_STOPPED)?.is_some() {
            "stopped"
        } else if limit > 0 && total.saturating_mul(100) >= limit.saturating_mul(warn_percent) {
            "warning"
        } else {
            "ok"
        };
        Ok(UsageStatus {
            month,
            requests: by_model.iter().map(|m| m.requests).sum(),
            input_tokens: by_model.iter().map(|m| m.input_tokens).sum(),
            output_tokens: by_model.iter().map(|m| m.output_tokens).sum(),
            total_tokens: total,
            monthly_token_limit: limit,
            warn_percent,
            state,
            by_model,
        })
    }
}

/// Gemini の API の接続先。開発時の確認用に `SCENOTE_GEMINI_BASE_URL` で差し替えられる。
fn base_url(env: &Env) -> String {
    env.var("SCENOTE_GEMINI_BASE_URL")
        .ok()
        .map(|v| v.to_string())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| GEMINI_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

async fn send(env: &Env, method: Method, url: &str, api_key: &str, body: Option<&Value>) -> std::result::Result<(u16, Value), LlmError> {
    let headers = Headers::new();
    let _ = headers.set("x-goog-api-key", api_key);
    let _ = headers.set("content-type", "application/json");
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(b) = body {
        init.with_body(Some(b.to_string().into()));
    }
    let request = Request::new_with_init(url, &init).map_err(|_| LlmError("Gemini API への要求を作れませんでした".into()))?;
    let _ = env;
    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|_| LlmError("Gemini API に接続できません。時間をおいて試してください".into()))?;
    let status = response.status_code();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    Ok((status, value))
}

/// 文章を生成する。API キーは URL ではなくヘッダーで送る（ログやエラーメッセージに残さないため）。
pub async fn generate(env: &Env, model: &str, api_key: &str, request: &LlmRequest) -> std::result::Result<LlmResponse, LlmError> {
    let url = format!("{}/models/{}:generateContent", base_url(env), model);
    let (status, body) = send(env, Method::Post, &url, api_key, Some(&gemini_request_body(request))).await?;
    if !(200..300).contains(&status) {
        return Err(LlmError(gemini_error_message(status, &body)));
    }
    parse_gemini_response(&body)
}

/// API キーで使える、文章生成に向いたモデルの一覧。
pub async fn list_models(env: &Env, api_key: &str) -> std::result::Result<Vec<LlmModel>, LlmError> {
    let mut models = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..20 {
        let mut url = url::Url::parse(&format!("{}/models", base_url(env))).map_err(|_| LlmError("接続先が正しくありません".into()))?;
        url.query_pairs_mut().append_pair("pageSize", "1000");
        if let Some(t) = &page_token {
            url.query_pairs_mut().append_pair("pageToken", t);
        }
        let (status, body) = send(env, Method::Get, url.as_str(), api_key, None).await?;
        if !(200..300).contains(&status) {
            return Err(LlmError(gemini_error_message(status, &body)));
        }
        collect_models(&body, &mut models)?;
        match body["nextPageToken"].as_str().filter(|s| !s.is_empty()) {
            Some(t) if page_token.as_deref() != Some(t) => page_token = Some(t.to_string()),
            _ => return Ok(models),
        }
    }
    Err(LlmError("Gemini API のモデル一覧がページ数の上限を超えました".into()))
}
