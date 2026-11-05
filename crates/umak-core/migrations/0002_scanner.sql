-- Columns the scanner needs.
-- - norm_key is the Unicode-aware identity of an artist or genre, so spellings that
--   differ only by case share one row. The reserved row's key is plain ASCII.
-- - grouping_rule records which album grouping rule (1 to 4) formed the album.
-- - artist_source says where the album artist came from: a tag, the compilation
--   flag, or judged from the album's tracks. Judged albums are recomputed when their
--   tracks change.

ALTER TABLE artists ADD COLUMN norm_key TEXT NOT NULL DEFAULT '';
UPDATE artists SET norm_key = lower(name);
CREATE UNIQUE INDEX idx_artists_norm_key ON artists (norm_key);

ALTER TABLE genres ADD COLUMN norm_key TEXT NOT NULL DEFAULT '';
UPDATE genres SET norm_key = lower(name);
CREATE UNIQUE INDEX idx_genres_norm_key ON genres (norm_key);

ALTER TABLE albums ADD COLUMN grouping_rule INTEGER NOT NULL DEFAULT 2
    CHECK (grouping_rule BETWEEN 1 AND 4);
ALTER TABLE albums ADD COLUMN artist_source TEXT NOT NULL DEFAULT 'tag'
    CHECK (artist_source IN ('tag', 'flag', 'judged'));
