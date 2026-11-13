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
pub mod assistant;
pub mod error;
pub mod llm;
pub mod markdown;
pub mod model;
pub mod diff;
pub mod store;
pub mod store_ai;
