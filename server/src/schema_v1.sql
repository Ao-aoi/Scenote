-- Scenote スキーマ v1（Phase 1: 最小のシナリオエディタ）
CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    templates   TEXT NOT NULL DEFAULT '[]',   -- 選択中テンプレートの JSON 配列
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE chapters (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title      TEXT NOT NULL,
    position   INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_chapters_project ON chapters(project_id, position);

CREATE TABLE scenes (
    id         TEXT PRIMARY KEY,
    chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    title      TEXT NOT NULL,
    position   INTEGER NOT NULL,
    revision   INTEGER NOT NULL DEFAULT 1,    -- 場面と配下要素の変更ごとに増加（競合検出用）
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_scenes_chapter ON scenes(chapter_id, position);

CREATE TABLE elements (
    id         TEXT PRIMARY KEY,
    scene_id   TEXT NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,                 -- Dialogue / Narration / ... （拡張可能な文字列）
    body       TEXT NOT NULL DEFAULT '',
    speaker    TEXT NOT NULL DEFAULT '',      -- 関連キャラクター名
    attrs      TEXT NOT NULL DEFAULT '{}',    -- 種類ごとの追加情報（JSON オブジェクト）
    position   INTEGER NOT NULL,
    origin     TEXT NOT NULL DEFAULT 'human', -- 編集の由来
    revision   INTEGER NOT NULL DEFAULT 1,    -- 内容の変更ごとに増加（競合検出用）
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_elements_scene ON elements(scene_id, position);
