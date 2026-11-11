-- Waveforms for the seek bar, computed from the audio once per file and rebuilt when it
-- changes. A row is current while size and mtime match the track's, or its fingerprint does,
-- so a tag edit that leaves the audio alone keeps it. format changes when the way waveforms
-- are computed does, which makes every row out of date.

CREATE TABLE waveforms (
    track_id    INTEGER PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    format      INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    mtime       INTEGER NOT NULL,
    fingerprint BLOB,
    -- NULL, with error set, when the file could not be decoded.
    peaks       BLOB,
    levels      BLOB,
    error       TEXT,
    created_at  INTEGER NOT NULL
) STRICT;
