//! Scenote の共有ライブラリ。DB・通信に依存しない処理をまとめ、PC 版サーバー（`server/`）と
//! Cloudflare 版（`cloudflare/`）の両方から使う。
//!
//! - `model`    : データモデル
//! - `markdown` : Markdown との相互変換
//! - `diff`     : テキスト差分
//! - `logic`    : 条件式・変更式
//! - `validate` : 入力の検証・正規化と、Markdown 保存時の要素の対応づけ

pub mod characters;
pub mod diff;
pub mod events;
pub mod files;
pub mod history;
pub mod logic;
pub mod markdown;
pub mod model;
pub mod schema;
pub mod validate;
