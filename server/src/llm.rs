//! LLM プロバイダーの抽象化レイヤー。
//!
//! プロバイダー固有の API 仕様はこのモジュールの中に閉じ込め、アプリの他の部分は
//! `LlmProvider` だけを通して LLM を呼び出す。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;

pub use scenote_core::llm::*;

pub type LlmModelsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<LlmModel>, LlmError>> + Send + 'a>>;

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
                    // 設定画面を長く待たせないよう、生成より短い時間で打ち切る。
                    .timeout(Duration::from_secs(20))
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
                collect_models(&body, &mut models)?;
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
    use serde_json::json;

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
    fn collects_generate_content_models_only() {
        let page = json!({ "models": [
            { "name": "models/gemini-a", "displayName": "Gemini A", "version": "3.8", "inputTokenLimit": 1048576,
              "outputTokenLimit": 65536, "thinking": true, "supportedGenerationMethods": ["generateContent", "countTokens"] },
            { "name": "models/gemini-3-pro-image-preview", "displayName": "Nano Banana Pro", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-x", "displayName": "Nano Banana", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-2.5-flash-preview-tts", "displayName": "Gemini TTS", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-live-2.5-flash", "displayName": "Live", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-2.5-flash-native-audio", "displayName": "Audio", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-robotics-er", "displayName": "Robotics", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemma-3-27b-it", "displayName": "Gemma 3 27B", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/embed-x", "displayName": "Embed", "supportedGenerationMethods": ["embedContent"] },
            { "name": "models/no-methods" },
            { "name": "tunedModels/foo", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/bad name", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-b", "displayName": "  ", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-a", "displayName": "重複", "supportedGenerationMethods": ["generateContent"] }
        ] });
        let mut models = Vec::new();
        collect_models(&page, &mut models).unwrap();
        let got: Vec<(&str, &str)> = models.iter().map(|m| (m.id.as_str(), m.display_name.as_str())).collect();
        assert_eq!(got, [("gemini-a", "Gemini A"), ("gemma-3-27b-it", "Gemma 3 27B"), ("gemini-b", "gemini-b")]);
        assert_eq!(models[0].version.as_deref(), Some("3.8"));
        assert_eq!((models[0].input_token_limit, models[0].output_token_limit, models[0].thinking), (Some(1048576), Some(65536), Some(true)));
        assert_eq!((models[1].version.as_deref(), models[1].thinking), (None, None));
        assert!(collect_models(&json!({ "error": {} }), &mut models).is_err());
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
