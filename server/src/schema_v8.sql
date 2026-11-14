-- Scenote スキーマ v8: かんたん執筆で自動で反映した提案の印
-- 本人が書いた文章を AI が要素に整えて反映したもの。嗜好の分析には使わない。
ALTER TABLE proposals ADD COLUMN auto_applied INTEGER NOT NULL DEFAULT 0;
