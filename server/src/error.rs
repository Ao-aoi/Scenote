//! アプリ共通のエラー型。HTTP 層では JSON のエラー応答へ変換する。

use std::fmt;

#[derive(Debug)]
pub enum AppError {
    /// 対象が存在しない。
    NotFound(&'static str),
    /// 入力が不正。メッセージはそのままユーザーへ表示できる内容にする。
    Invalid(String),
    /// 楽観ロックの競合。`current` はサーバー側の現在のリビジョン。
    Conflict { current: i64 },
    /// DB など内部エラー。詳細はクライアントへ返さない。
    Internal(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::NotFound(what) => write!(f, "{what}が見つかりません"),
            AppError::Invalid(msg) => write!(f, "{msg}"),
            AppError::Conflict { .. } => write!(
                f,
                "他の画面または端末で先に変更されています。最新の内容を読み込んでから編集し直してください"
            ),
            AppError::Internal(_) => write!(f, "サーバー内部でエラーが発生しました"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}
