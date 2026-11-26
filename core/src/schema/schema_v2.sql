-- Scenote スキーマ v2（Phase 2: AI チャットと提案レビュー）
CREATE TABLE reference_files (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    content    TEXT NOT NULL DEFAULT '',
    pinned     INTEGER NOT NULL DEFAULT 0,   -- 1: プロジェクト共通で常に参照する
    revision   INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_reference_files_project ON reference_files(project_id, name);

CREATE TABLE chats (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    instructions TEXT NOT NULL DEFAULT '',   -- チャットごとの追加指示
    auto_select  INTEGER NOT NULL DEFAULT 1, -- 1: AI による参照ファイル自動選択を使う
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX idx_chats_project ON chats(project_id, updated_at);

-- チャットごとに指定した参照ファイル
CREATE TABLE chat_files (
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL REFERENCES reference_files(id) ON DELETE CASCADE,
    PRIMARY KEY (chat_id, file_id)
);

CREATE TABLE messages (
    id               TEXT PRIMARY KEY,
    chat_id          TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    seq              INTEGER NOT NULL,
    role             TEXT NOT NULL,              -- user / assistant
    content          TEXT NOT NULL,
    scene_id         TEXT,                       -- 依頼時に開いていた場面
    referenced_files TEXT NOT NULL DEFAULT '[]', -- 参照したファイル [{id,name,source}]
    warnings         TEXT NOT NULL DEFAULT '[]', -- 検証で除外した提案などの注意
    created_at       TEXT NOT NULL
);
CREATE INDEX idx_messages_chat ON messages(chat_id, seq);

CREATE TABLE proposals (
    id            TEXT PRIMARY KEY,
    message_id    TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    ordinal       INTEGER NOT NULL,
    op            TEXT NOT NULL,   -- update_element / insert_element / delete_element / update_file / create_file
    target_id     TEXT,            -- 対象の要素・ファイル
    scene_id      TEXT,            -- 要素を対象とする場合の場面
    anchor        TEXT,            -- 挿入位置: 要素 ID / @start / @end
    base_revision INTEGER,         -- 提案生成時の対象のリビジョン
    before        TEXT,            -- 変更前の内容（JSON）
    after         TEXT,            -- 変更後の内容（JSON）
    reason        TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'pending', -- pending / approved / rejected / stale
    reject_reason TEXT NOT NULL DEFAULT '',
    result_id     TEXT,            -- 承認により作成された要素・ファイル
    decided_at    TEXT,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_proposals_message ON proposals(message_id, ordinal);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE llm_usage (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    provider      TEXT NOT NULL,
    model         TEXT NOT NULL,
    purpose       TEXT NOT NULL,   -- chat / select_files
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_llm_usage_created ON llm_usage(created_at);
