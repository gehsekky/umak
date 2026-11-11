-- Which cover art each track shows. The images themselves are files in the art folder beside
-- the database, named by a hash of the original picture, so tracks sharing a picture share
-- the files. A row is current while size and mtime match the track's.
-- - art: the hash, or NULL when the track has none, neither in its tags nor in its folder.
-- - source: where the picture came from.

CREATE TABLE track_art (
    track_id INTEGER PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    size     INTEGER NOT NULL,
    mtime    INTEGER NOT NULL,
    art      TEXT,
    source   TEXT NOT NULL CHECK (source IN ('embedded', 'folder', 'none')),
    error    TEXT
) STRICT;

CREATE INDEX idx_track_art_art ON track_art (art) WHERE art IS NOT NULL;
