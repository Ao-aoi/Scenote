-- Scenote スキーマ v7: 場面の概要（プロット）
-- 場面で起きることを短く書いたもの。簡易版のプロット編集と、AI によるストーリー案の生成で使う。
ALTER TABLE scenes ADD COLUMN summary TEXT NOT NULL DEFAULT '';
