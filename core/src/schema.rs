//! DB スキーマの段階的な更新（マイグレーション）。PC 版と Cloudflare 版で同じ SQL を使う。
//!
//! `MIGRATIONS[i]` はバージョン `i + 1` へ上げる SQL。適用済みのバージョンの管理は各版が行う
//! （PC 版は `PRAGMA user_version`、Cloudflare 版は管理用の表）。

pub const MIGRATIONS: [&str; 8] = [
    include_str!("schema/schema_v1.sql"),
    include_str!("schema/schema_v2.sql"),
    include_str!("schema/schema_v3.sql"),
    include_str!("schema/schema_v4.sql"),
    include_str!("schema/schema_v5.sql"),
    include_str!("schema/schema_v6.sql"),
    include_str!("schema/schema_v7.sql"),
    include_str!("schema/schema_v8.sql"),
];

#[cfg(test)]
mod tests {
    #[test]
    fn migrations_match_schema_version() {
        assert_eq!(super::MIGRATIONS.len() as i64, crate::model::SCHEMA_VERSION);
    }
}
