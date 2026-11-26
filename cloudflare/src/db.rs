//! Durable Object 内蔵の SQLite への薄い窓口。

use serde::de::{DeserializeOwned, IgnoredAny};
use worker::{SqlStorage, SqlStorageValue};

use crate::error::{AppError, Result};

pub type Param = SqlStorageValue;

/// SQL の引数を並べる。`p![id, title, 3i64]`
#[macro_export]
macro_rules! p {
    ($($v:expr),* $(,)?) => { vec![$($crate::db::Param::from($v)),*] };
}

pub const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

#[derive(Clone)]
pub struct Db {
    sql: SqlStorage,
}

impl Db {
    pub fn new(sql: SqlStorage) -> Self {
        Self { sql }
    }

    /// 結果を使わない文を実行する（結果を読み切って実行を完了させる）。
    pub fn exec(&self, query: &str, params: Vec<Param>) -> Result<()> {
        self.sql.exec(query, Some(params))?.to_array::<IgnoredAny>()?;
        Ok(())
    }

    pub fn all<T: DeserializeOwned>(&self, query: &str, params: Vec<Param>) -> Result<Vec<T>> {
        Ok(self.sql.exec(query, Some(params))?.to_array::<T>()?)
    }

    pub fn opt<T: DeserializeOwned>(&self, query: &str, params: Vec<Param>) -> Result<Option<T>> {
        Ok(self.all(query, params)?.into_iter().next())
    }

    pub fn one<T: DeserializeOwned>(&self, query: &str, params: Vec<Param>) -> Result<T> {
        self.opt(query, params)?
            .ok_or_else(|| AppError::Internal(format!("no row: {query}")))
    }

    /// 1列だけの結果を返す。
    pub fn scalar<T: DeserializeOwned>(&self, query: &str, params: Vec<Param>) -> Result<T> {
        #[derive(serde::Deserialize)]
        struct V<T> {
            v: T,
        }
        Ok(self.one::<V<T>>(query, params)?.v)
    }
}
