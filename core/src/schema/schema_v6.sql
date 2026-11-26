-- Scenote スキーマ v6: キャラクターの独立データ管理
CREATE TABLE characters (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    aliases TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, name)
);
CREATE INDEX idx_characters_project ON characters(project_id, name);

CREATE TABLE character_facts (
    id TEXT PRIMARY KEY,
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    category TEXT NOT NULL,
    text TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('user','confirmed','inferred')),
    confidence REAL,
    evidence TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','proposed','rejected')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_character_facts_character ON character_facts(character_id, status);

CREATE TABLE character_relations (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    to_character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL CHECK(source IN ('user','confirmed','inferred')),
    confidence REAL,
    evidence TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','proposed','rejected')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(from_character_id <> to_character_id)
);
CREATE INDEX idx_character_relations_project ON character_relations(project_id, from_character_id, to_character_id);
ALTER TABLE elements ADD COLUMN character_id TEXT REFERENCES characters(id) ON DELETE SET NULL;
