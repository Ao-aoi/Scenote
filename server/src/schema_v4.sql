-- Scenote スキーマ v4（Phase 4: iPad からの利用）

-- ペアリング済みの端末。トークンは SHA-256 のハッシュだけを保存する。
CREATE TABLE devices (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);
