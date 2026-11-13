//! LLM プロバイダーの抽象化レイヤー。
//!
//! プロバイダー固有の API 仕様はこのモジュールの中に閉じ込め、アプリの他の部分は
//! `LlmProvider` だけを通して LLM を呼び出す。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system: String,
    pub messages: Vec<LlmMessage>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LlmModel {
    pub id: String,
    pub display_name: String,
}

pub type LlmModelsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<LlmModel>, LlmError>> + Send + 'a>>;

/// 呼び出しの失敗。メッセージはユーザーへ表示するため、API キーなどの秘密を含めない。
#[derive(Debug, Clone)]
pub struct LlmError(pub String);

pub type LlmFuture<'a> = Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>>;

pub trait LlmProvider: Send + Sync {
    /// 使用量の集計に使うプロバイダー名。
    fn name(&self) -> &'static str;

    fn generate<'a>(&'a self, model: &'a str, api_key: &'a str, request: &'a LlmRequest) -> LlmFuture<'a>;

    /// API キーで利用可能な generateContent 対応モデルを取得する。
    fn list_models<'a>(&'a self, _api_key: &'a str) -> LlmModelsFuture<'a> {
        Box::pin(async { Err(LlmError("このプロバイダーはモデル一覧の取得に対応していません".into())) })
    }
}

/// Gemini API（generateContent）。
///
/// 構造化出力の指定方法はモデルの世代によって異なるため使用せず、JSON の形式は
/// プロンプトで指示し、応答はアプリ側で検証する。
pub struct GeminiProvider {
    client: reqwest::Client,
    base_url: String,
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self::with_base_url("https://generativelanguage.googleapis.com/v1beta")
    }

    pub fn with_base_url(base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .unwrap_or_default();
        GeminiProvider {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn gemini_request_body(request: &LlmRequest) -> Value {
    let contents: Vec<Value> = request
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": if m.role == Role::User { "user" } else { "model" },
                "parts": [{ "text": m.text }],
            })
        })
        .collect();
    json!({
        "systemInstruction": { "parts": [{ "text": request.system }] },
        "contents": contents,
    })
}

pub(crate) fn parse_gemini_response(body: &Value) -> Result<LlmResponse, LlmError> {
    let candidate = &body["candidates"][0];
    let text: String = candidate["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                // 思考過程（thought）のパートは本文に含めない。
                .filter(|p| p["thought"].as_bool() != Some(true))
                .filter_map(|p| p["text"].as_str())
                .collect()
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        let reason = candidate["finishReason"]
            .as_str()
            .or_else(|| body["promptFeedback"]["blockReason"].as_str())
            .unwrap_or("不明");
        return Err(LlmError(format!(
            "AI から本文のない応答が返されました（理由: {reason}）。依頼の内容を変えて試してください"
        )));
    }
    let usage = &body["usageMetadata"];
    let count = |key: &str| usage[key].as_i64().unwrap_or(0);
    Ok(LlmResponse {
        text,
        input_tokens: count("promptTokenCount"),
        output_tokens: count("candidatesTokenCount") + count("thoughtsTokenCount"),
    })
}

pub(crate) fn gemini_error_message(status: u16, body: &Value) -> String {
    let detail = body["error"]["message"].as_str().unwrap_or("").trim();
    let detail: String = detail.chars().take(300).collect();
    let summary = match status {
        400 => "Gemini API が要求を受け付けませんでした。モデル名や API キーを確認してください",
        401 | 403 => "Gemini API の認証に失敗しました。API キーを確認してください",
        404 => "指定したモデルが見つかりません。設定のモデル名を確認してください",
        429 => "Gemini API の利用制限（レート制限または無料枠の上限）に達しました。時間をおいて試してください",
        500..=599 => "Gemini API 側でエラーが発生しました。時間をおいて試してください",
        _ => "Gemini API の呼び出しに失敗しました",
    };
    if detail.is_empty() {
        format!("{summary} (HTTP {status})")
    } else {
        format!("{summary} (HTTP {status}: {detail})")
    }
}

impl LlmProvider for GeminiProvider {
    fn list_models<'a>(&'a self, api_key: &'a str) -> LlmModelsFuture<'a> {
        Box::pin(async move {
            let mut page_token: Option<String> = None;
            let mut seen_tokens = std::collections::HashSet::new();
            let mut models: Vec<LlmModel> = Vec::new();
            for _ in 0..20 {
                let mut request = self.client
                    .get(format!("{}/models", self.base_url))
                    .header("x-goog-api-key", api_key)
                    .query(&[("pageSize", "1000")]);
                if let Some(token) = page_token.as_deref() {
                    request = request.query(&[("pageToken", token)]);
                }
                let response = request.send().await.map_err(|_| {
                    LlmError("Gemini API のモデル一覧を取得できません。ネットワーク接続を確認してください".into())
                })?;
                let status = response.status().as_u16();
                let body: Value = response.json().await.unwrap_or(Value::Null);
                if !(200..300).contains(&status) {
                    return Err(LlmError(gemini_error_message(status, &body)));
                }
                let entries = body["models"].as_array().ok_or_else(|| {
                    LlmError("Gemini API から不正なモデル一覧が返されました".into())
                })?;
                for entry in entries {
                    let supports_generation = entry["supportedGenerationMethods"]
                        .as_array()
                        .is_some_and(|methods| methods.iter().any(|m| m.as_str() == Some("generateContent")));
                    let Some(name) = entry["name"].as_str().and_then(|n| n.strip_prefix("models/")) else {
                        continue;
                    };
                    if !supports_generation
                        || name.is_empty()
                        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
                        || models.iter().any(|m| m.id == name)
                    {
                        continue;
                    }
                    let display_name = entry["displayName"].as_str().filter(|s| !s.trim().is_empty())
                        .unwrap_or(name).to_string();
                    models.push(LlmModel { id: name.to_string(), display_name });
                }
                let next = body["nextPageToken"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
                match next {
                    Some(token) if seen_tokens.insert(token.clone()) => page_token = Some(token),
                    _ => return Ok(models),
                }
            }
            Err(LlmError("Gemini API のモデル一覧がページ数の上限を超えました".into()))
        })
    }

    fn name(&self) -> &'static str {
        "gemini"
    }

    fn generate<'a>(&'a self, model: &'a str, api_key: &'a str, request: &'a LlmRequest) -> LlmFuture<'a> {
        Box::pin(async move {
            let url = format!("{}/models/{}:generateContent", self.base_url, model);
            // API キーは URL ではなくヘッダーで送る（ログやエラーメッセージに残さないため）。
            let response = self
                .client
                .post(url)
                .header("x-goog-api-key", api_key)
                .json(&gemini_request_body(request))
                .send()
                .await
                .map_err(|e| {
                    LlmError(if e.is_timeout() {
                        "Gemini API の応答が時間内に返りませんでした。時間をおいて試してください".into()
                    } else {
                        "Gemini API に接続できません。インターネット接続を確認してください".into()
                    })
                })?;
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            if !(200..300).contains(&status) {
                return Err(LlmError(gemini_error_message(status, &body)));
            }
            parse_gemini_response(&body)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_request_body() {
        let body = gemini_request_body(&LlmRequest {
            system: "指示".into(),
            messages: vec![
                LlmMessage { role: Role::User, text: "こんにちは".into() },
                LlmMessage { role: Role::Assistant, text: "はい".into() },
            ],
        });
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "指示");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(body["contents"][1]["parts"][0]["text"], "はい");
    }

    #[test]
    fn parses_response_and_usage() {
        let body = json!({
            "candidates": [{ "content": { "parts": [
                { "text": "考え中", "thought": true }, { "text": "前半" }, { "text": "後半" }
            ] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 120, "candidatesTokenCount": 30, "thoughtsTokenCount": 12 }
        });
        let r = parse_gemini_response(&body).unwrap();
        assert_eq!(r.text, "前半後半");
        assert_eq!((r.input_tokens, r.output_tokens), (120, 42));
    }

    #[test]
    fn reports_empty_and_error_responses() {
        let blocked = json!({ "candidates": [{ "finishReason": "SAFETY" }] });
        assert!(parse_gemini_response(&blocked).unwrap_err().0.contains("SAFETY"));
        let message = gemini_error_message(429, &json!({ "error": { "message": "Quota exceeded" } }));
        assert!(message.contains("利用制限") && message.contains("Quota exceeded"));
        assert!(gemini_error_message(403, &Value::Null).contains("API キー"));
    }
}
