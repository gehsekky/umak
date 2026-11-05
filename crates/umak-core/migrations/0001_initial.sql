-- Initial schema. Conventions:
-- - id columns are internal join keys; public_id columns are what leaves the core.
-- - Timestamps are Unix milliseconds.
-- - rel_path is relative to its library root and uses forward slashes.
-- - Data rebuilt from music files cascades when a track is deleted. Per-user data
--   restricts, so removing a track that holds it must be an explicit decision.

-- Location

CREATE TABLE library_roots (
    id        INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    name      TEXT NOT NULL,
    path      TEXT NOT NULL UNIQUE,
    added_at  INTEGER NOT NULL
) STRICT;

-- Catalog

CREATE TABLE artists (
    id             INTEGER PRIMARY KEY,
    public_id      TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL UNIQUE,
    sort_name      TEXT NOT NULL,
    musicbrainz_id TEXT
) STRICT;

CREATE INDEX idx_artists_sort ON artists (sort_name, id);

-- group_key is the identity computed by the scanner: the MusicBrainz album ID when
-- present, otherwise album artist, title and folder.
CREATE TABLE albums (
    id                   INTEGER PRIMARY KEY,
    public_id            TEXT NOT NULL UNIQUE,
    group_key            TEXT NOT NULL UNIQUE,
    title                TEXT NOT NULL,
    sort_title           TEXT NOT NULL,
    album_artist_display TEXT NOT NULL,
    album_artist_sort    TEXT NOT NULL,
    year                 INTEGER,
    compilation          TEXT NOT NULL DEFAULT 'none'
                         CHECK (compilation IN ('none', 'tagged', 'inferred')),
    art_ref              TEXT,
    musicbrainz_id       TEXT
) STRICT;

CREATE INDEX idx_albums_browse ON albums (album_artist_sort, sort_title, id);

CREATE TABLE genres (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
) STRICT;

-- Files

-- The sort and display columns copy values from the catalog so that browsing and
-- drawing a row need no joins. The scanner writes them; the link tables stay the
-- source of truth. missing_since is set when the file disappears.
CREATE TABLE tracks (
    id                       INTEGER PRIMARY KEY,
    public_id                TEXT NOT NULL UNIQUE,
    root_id                  INTEGER NOT NULL REFERENCES library_roots (id) ON DELETE RESTRICT,
    rel_path                 TEXT NOT NULL,
    size                     INTEGER NOT NULL,
    mtime                    INTEGER NOT NULL,
    fingerprint              BLOB,
    missing_since            INTEGER,
    duration_ms              INTEGER,
    codec                    TEXT,
    sample_rate              INTEGER,
    bit_depth                INTEGER,
    channels                 INTEGER,
    bitrate                  INTEGER,
    title                    TEXT NOT NULL,
    album_id                 INTEGER NOT NULL REFERENCES albums (id) ON DELETE RESTRICT,
    disc_no                  INTEGER NOT NULL DEFAULT 0,
    track_no                 INTEGER NOT NULL DEFAULT 0,
    year                     INTEGER,
    original_date            TEXT,
    rg_track_gain            REAL,
    rg_track_peak            REAL,
    rg_album_gain            REAL,
    rg_album_peak            REAL,
    musicbrainz_recording_id TEXT,
    artist_display           TEXT NOT NULL DEFAULT '',
    album_display            TEXT NOT NULL DEFAULT '',
    title_sort               TEXT NOT NULL,
    artist_sort              TEXT NOT NULL,
    album_sort               TEXT NOT NULL,
    album_artist_sort        TEXT NOT NULL,
    added_at                 INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL,
    UNIQUE (root_id, rel_path)
) STRICT;

CREATE INDEX idx_tracks_browse ON tracks
    (album_artist_sort, album_sort, album_id, disc_no, track_no, id);
CREATE INDEX idx_tracks_artist ON tracks
    (artist_sort, album_sort, album_id, disc_no, track_no, id);
CREATE INDEX idx_tracks_title ON tracks (title_sort, id);
CREATE INDEX idx_tracks_added ON tracks (added_at, id);
CREATE INDEX idx_tracks_album ON tracks (album_id, disc_no, track_no, id);
CREATE INDEX idx_tracks_fingerprint ON tracks (fingerprint) WHERE fingerprint IS NOT NULL;
CREATE INDEX idx_tracks_missing ON tracks (missing_since) WHERE missing_since IS NOT NULL;

-- Multi-value links

CREATE TABLE track_artists (
    track_id  INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    artist_id INTEGER NOT NULL REFERENCES artists (id) ON DELETE RESTRICT,
    role      TEXT NOT NULL CHECK (role IN ('artist', 'composer')),
    position  INTEGER NOT NULL,
    PRIMARY KEY (track_id, role, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_track_artists_artist ON track_artists (artist_id, role, track_id);

CREATE TABLE album_artists (
    album_id  INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    artist_id INTEGER NOT NULL REFERENCES artists (id) ON DELETE RESTRICT,
    position  INTEGER NOT NULL,
    PRIMARY KEY (album_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_album_artists_artist ON album_artists (artist_id, album_id);

CREATE TABLE track_genres (
    track_id INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    genre_id INTEGER NOT NULL REFERENCES genres (id) ON DELETE RESTRICT,
    position INTEGER NOT NULL,
    PRIMARY KEY (track_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_track_genres_genre ON track_genres (genre_id, track_id);

-- Tags with no column of their own, kept so nothing is lost on write

CREATE TABLE raw_tags (
    track_id INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    key      TEXT NOT NULL,
    position INTEGER NOT NULL,
    value    TEXT NOT NULL,
    PRIMARY KEY (track_id, key, position)
) STRICT, WITHOUT ROWID;

-- Search. The rowid is the track id; the scanner writes rows in its batches.

CREATE VIRTUAL TABLE tracks_fts USING fts5 (
    title,
    artists,
    album,
    genres,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

CREATE TRIGGER tracks_fts_delete AFTER DELETE ON tracks BEGIN
    DELETE FROM tracks_fts WHERE rowid = old.id;
END;

-- Per-user data

CREATE TABLE users (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
) STRICT;

-- Ratings run from 0 to 10 and display as half-stars. NULL means unrated.
CREATE TABLE track_user_data (
    user_id        INTEGER NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    track_id       INTEGER NOT NULL REFERENCES tracks (id) ON DELETE RESTRICT,
    rating         INTEGER CHECK (rating BETWEEN 0 AND 10),
    play_count     INTEGER NOT NULL DEFAULT 0,
    last_played_at INTEGER,
    PRIMARY KEY (user_id, track_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_track_user_data_track ON track_user_data (track_id);

CREATE TABLE play_history (
    id        INTEGER PRIMARY KEY,
    user_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    track_id  INTEGER NOT NULL REFERENCES tracks (id) ON DELETE RESTRICT,
    played_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_play_history_user ON play_history (user_id, played_at);
CREATE INDEX idx_play_history_track ON play_history (track_id);

-- Each user has exactly one playlist of kind 'player': the player playlist.
CREATE TABLE playlists (
    id               INTEGER PRIMARY KEY,
    public_id        TEXT NOT NULL UNIQUE,
    user_id          INTEGER NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    name             TEXT NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('manual', 'smart', 'player')),
    rule             TEXT,
    current_position INTEGER,
    current_ms       INTEGER,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    CHECK ((kind = 'smart') = (rule IS NOT NULL))
) STRICT;

CREATE INDEX idx_playlists_user ON playlists (user_id, name);
CREATE UNIQUE INDEX idx_playlists_one_player ON playlists (user_id) WHERE kind = 'player';

-- Positions leave gaps so an insert rarely renumbers the list. A track may appear
-- more than once in a playlist.
CREATE TABLE playlist_items (
    playlist_id INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    track_id    INTEGER NOT NULL REFERENCES tracks (id) ON DELETE RESTRICT,
    PRIMARY KEY (playlist_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_playlist_items_track ON playlist_items (track_id);

-- System

CREATE TABLE jobs (
    id             INTEGER PRIMARY KEY,
    public_id      TEXT NOT NULL UNIQUE,
    kind           TEXT NOT NULL,
    status         TEXT NOT NULL
                   CHECK (status IN ('queued', 'running', 'completed', 'failed', 'canceled')),
    progress_done  INTEGER NOT NULL DEFAULT 0,
    progress_total INTEGER,
    payload        TEXT,
    error          TEXT,
    created_at     INTEGER NOT NULL,
    started_at     INTEGER,
    finished_at    INTEGER
) STRICT;

CREATE INDEX idx_jobs_status ON jobs (status, created_at);

-- Values are JSON. Typed access lives in Rust.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Reserved rows. The public IDs in this block match the constants in db.rs.

INSERT INTO users (id, name) VALUES (1, 'default');

INSERT INTO artists (public_id, name, sort_name)
VALUES ('00000000-0000-4000-8000-000000000001', 'Various Artists', 'various artists');

INSERT INTO playlists (public_id, user_id, name, kind, created_at, updated_at)
VALUES ('00000000-0000-4000-8000-000000000002', 1, 'Player playlist', 'player', 0, 0);
