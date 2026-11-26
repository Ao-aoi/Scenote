//! LLM との通信で、通信手段に依存しない部分（要求の組み立て・応答の解釈・モデル一覧の整理）。
//! 実際の送受信は各版が行う（PC 版は reqwest、Cloudflare 版は Workers の fetch）。

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
    /// プロバイダーが返すバージョン表記（リリース日は API から取得できない）。
    pub version: Option<String>,
    pub input_token_limit: Option<i64>,
    pub output_token_limit: Option<i64>,
    /// 思考（thinking）に対応しているか。不明なら `None`。
    pub thinking: Option<bool>,
}

/// 呼び出しの失敗。メッセージはユーザーへ表示するため、API キーなどの秘密を含めない。
#[derive(Debug, Clone)]
pub struct LlmError(pub String);

pub fn gemini_request_body(request: &LlmRequest) -> Value {
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

pub fn parse_gemini_response(body: &Value) -> Result<LlmResponse, LlmError> {
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

pub fn gemini_error_message(status: u16, body: &Value) -> String {
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

/// `models.list` の 1 ページ分から、generateContent に対応したモデルを `models` へ追加する。
/// 文章生成に向かないモデルを示す語。`generateContent` に対応していても、画像・音声の生成や
/// リアルタイム対話などの専用モデルは、シナリオ執筆では使えないため一覧から外す。
pub const NON_TEXT_MARKERS: [&str; 14] = [
    "image", "imagen", "nano-banana", "nano banana", "tts", "audio", "speech", "live", "veo",
    "lyria", "embedding", "aqa", "robotics", "computer-use",
];

/// 文章の生成・推敲に使えるモデルか。ID と表示名から判定する。
pub fn is_text_model(id: &str, display_name: &str) -> bool {
    let haystack = format!("{} {}", id, display_name).to_lowercase();
    !NON_TEXT_MARKERS.iter().any(|m| haystack.contains(m))
}

pub fn collect_models(body: &Value, models: &mut Vec<LlmModel>) -> Result<(), LlmError> {
    let entries = body["models"]
        .as_array()
        .ok_or_else(|| LlmError("Gemini API から不正なモデル一覧が返されました".into()))?;
    for entry in entries {
        let supports_generation = entry["supportedGenerationMethods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|m| m.as_str() == Some("generateContent")));
        let Some(name) = entry["name"].as_str().and_then(|n| n.strip_prefix("models/")) else {
            continue;
        };
        if !supports_generation
            || name.is_empty()
            || name.len() > 100
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
            || models.iter().any(|m| m.id == name)
        {
            continue;
        }
        let display_name = entry["displayName"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(name)
            .to_string();
        if !is_text_model(name, &display_name) {
            continue;
        }
        let version = entry["version"]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 50)
            .map(str::to_string);
        models.push(LlmModel {
            id: name.to_string(),
            display_name,
            version,
            input_token_limit: entry["inputTokenLimit"].as_i64().filter(|n| *n > 0),
            output_token_limit: entry["outputTokenLimit"].as_i64().filter(|n| *n > 0),
            thinking: entry["thinking"].as_bool(),
        });
    }
    Ok(())
}


/// 応答の文章から JSON オブジェクトを取り出す（前後の文章やコードフェンスを許容する）。
pub fn extract_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end]).ok().filter(Value::is_object)
}

pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
