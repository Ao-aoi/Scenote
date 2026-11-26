-- Scenote スキーマ v5（Phase 5: 条件・変数・分岐）

-- プロジェクトの変数（フラグ・好感度・所持金など）。
CREATE TABLE variables (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    var_type    TEXT NOT NULL,            -- number / bool / text
    initial     TEXT NOT NULL DEFAULT '', -- 初期値（式として解釈できる文字列）
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (project_id, name)
);
