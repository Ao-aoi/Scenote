//! エラー型と JSON のエラー応答。形式は PC 版（`server/src/error.rs`）に合わせる。

use std::fmt;

#[derive(Debug)]
pub enum AppError {
    NotFound(&'static str),
    Invalid(String),
    Conflict { current: i64 },
    Forbidden(String),
    /// Cloudflare 版ではまだ使えない機能。
    Unavailable,
    /// LLM の呼び出しに失敗した。メッセージはユーザーへ表示する。
    Llm(String),
    /// 使用量の上限に達し、API 利用を停止している。
    LlmStopped,
    /// DB など内部エラー。詳細はクライアントへ返さない。
    Internal(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::NotFound(what) => write!(f, "{what}が見つかりません"),
            AppError::Invalid(msg) | AppError::Forbidden(msg) => write!(f, "{msg}"),
            AppError::Conflict { .. } => write!(
                f,
                "他の画面または端末で先に変更されています。最新の内容を読み込んでから編集し直してください"
            ),
            AppError::Unavailable => write!(f, "この機能は Cloudflare 版ではまだ使えません"),
            AppError::Llm(msg) => write!(f, "{msg}"),
            AppError::LlmStopped => write!(
                f,
                "API 使用量が上限に達したため、AI の利用を停止しています。設定で上限を変更するか、利用を再開してください"
            ),
            AppError::Internal(_) => write!(f, "サーバー内部でエラーが発生しました"),
        }
    }
}

impl AppError {
    pub fn status(&self) -> u16 {
        match self {
            AppError::NotFound(_) => 404,
            AppError::Invalid(_) => 400,
            AppError::Conflict { .. } => 409,
            AppError::Forbidden(_) => 403,
            AppError::Unavailable => 501,
            AppError::Llm(_) => 502,
            AppError::LlmStopped => 429,
            AppError::Internal(_) => 500,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::Invalid(_) => "invalid",
            AppError::Conflict { .. } => "conflict",
            AppError::Forbidden(_) => "forbidden",
            AppError::Unavailable => "unavailable",
            AppError::Llm(_) => "llm",
            AppError::LlmStopped => "llm_stopped",
            AppError::Internal(_) => "internal",
        }
    }

    pub fn body(&self) -> serde_json::Value {
        let mut error = serde_json::json!({ "code": self.code(), "message": self.to_string() });
        if let AppError::Conflict { current } = self {
            error["current_revision"] = serde_json::json!(current);
        }
        serde_json::json!({ "error": error })
    }
}

impl From<worker::Error> for AppError {
    fn from(e: worker::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Invalid(format!("要求の形式が正しくありません: {e}"))
    }
}
