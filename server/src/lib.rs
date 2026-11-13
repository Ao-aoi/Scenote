//! Scenote サーバーのライブラリ部分。
//!
//! - `model`    : データモデル
//! - `store`    : SQLite 永続化・整合性管理
//! - `markdown` : Markdown との相互変換
//! - `api`      : HTTP API

pub mod api;
pub mod error;
pub mod markdown;
pub mod model;
pub mod store;
