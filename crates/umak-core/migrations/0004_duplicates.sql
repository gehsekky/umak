-- Lets the duplicate finder group a whole library from indexes alone, without reading every
-- track. Finding moved files uses the fingerprint index too, and only gains a column.

DROP INDEX idx_tracks_fingerprint;
CREATE INDEX idx_tracks_fingerprint ON tracks (fingerprint, missing_since)
    WHERE fingerprint IS NOT NULL;

CREATE INDEX idx_tracks_tag_match ON tracks (title, artist_display, album_display, track_no)
    WHERE missing_since IS NULL;
