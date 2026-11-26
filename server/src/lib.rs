//! Scenote サーバーのライブラリ部分。
//!
//! - `model`    : データモデル
//! - `store`    : SQLite 永続化・整合性管理
//! - `markdown` : Markdown との相互変換
//! - `store_ai` : 参照ファイル・チャット・提案・設定・使用量
//! - `diff`     : テキスト差分
//! - `llm`      : LLM プロバイダーの抽象化（Gemini）
//! - `assistant`: AI との共同編集（参照選択・プロンプト・応答検証）
//! - `api`      : HTTP API

pub mod api;
pub mod characters;
pub mod assistant;
pub mod auth;
pub mod error;
pub mod generation;
pub mod history;
pub mod llm;
pub mod preferences;
pub mod store;
pub mod store_ai;
pub mod structure;
pub mod sync;

pub use scenote_core::{diff, logic, markdown, model};
