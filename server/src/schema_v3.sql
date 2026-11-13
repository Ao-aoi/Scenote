-- Scenote スキーマ v3（Phase 3: 履歴・スナップショット・嗜好学習）

-- 編集履歴。要素・参照ファイル単位で、変更前後の内容と由来を記録する。
CREATE TABLE history (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    scene_id    TEXT,                 -- 要素の変更の場合の場面（削除済みでも残す）
    target_type TEXT NOT NULL,        -- element / file / scene
    target_id   TEXT NOT NULL,
    action      TEXT NOT NULL,        -- create / update / delete / reorder / restore
    actor       TEXT NOT NULL,        -- human / ai_approved / ai_modified / restore
    before      TEXT,                 -- 変更前の内容（JSON）
    after       TEXT,                 -- 変更後の内容（JSON）
    proposal_id TEXT,                 -- AI 提案の承認による変更の場合
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL         -- 連続した入力をまとめた場合の最終更新
);
CREATE INDEX idx_history_project ON history(project_id, seq);
CREATE INDEX idx_history_scene ON history(scene_id, seq);
CREATE INDEX idx_history_target ON history(target_id, seq);

-- 手動スナップショット。プロジェクト全体（章・場面・要素・参照ファイル）を JSON で保持する。
CREATE TABLE snapshots (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    note        TEXT NOT NULL DEFAULT '',
    kind        TEXT NOT NULL DEFAULT 'manual', -- manual / before_restore
    data        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_snapshots_project ON snapshots(project_id, created_at);

-- 提案を人間が修正して採用した場合の、実際に反映した内容。
ALTER TABLE proposals ADD COLUMN final TEXT;
-- 嗜好の分析に使用済みか。
ALTER TABLE proposals ADD COLUMN analyzed_at TEXT;

-- 執筆嗜好のルール。project_id が NULL のものは全プロジェクト共通。
CREATE TABLE preference_rules (
    id          TEXT PRIMARY KEY,
    project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    category    TEXT NOT NULL DEFAULT 'other',
    confidence  REAL NOT NULL DEFAULT 0.5,   -- 0〜1。根拠が少ないうちは低く保つ
    evidence    TEXT NOT NULL DEFAULT '[]',  -- 根拠となった提案 ID の JSON 配列
    source      TEXT NOT NULL DEFAULT 'user', -- user / ai / copied
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_preference_rules_project ON preference_rules(project_id);

-- 分析結果のうち、まだプロファイルへ適用していない更新案（自動更新 OFF のとき）。
CREATE TABLE preference_suggestions (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    op          TEXT NOT NULL,     -- add / update / remove
    rule_id     TEXT,              -- update / remove の対象
    text        TEXT NOT NULL DEFAULT '',
    category    TEXT NOT NULL DEFAULT 'other',
    confidence  REAL NOT NULL DEFAULT 0.5,
    evidence    TEXT NOT NULL DEFAULT '[]',
    reason      TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'pending', -- pending / approved / rejected / stale
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_preference_suggestions_project ON preference_suggestions(project_id, status);
