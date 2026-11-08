//! SQLite storage: connection setup and schema migrations.
//!
//! One writer connection and any number of read-only connections share a database
//! in WAL mode, so readers never wait for the scanner. The database must live on a
//! local disk, because WAL does not work across a network filesystem.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Migrations in order. A database's `user_version` counts how many have run.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_scanner.sql"),
    include_str!("../migrations/0003_tag_edits.sql"),
    include_str!("../migrations/0004_duplicates.sql"),
    include_str!("../migrations/0005_conversions.sql"),
];

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_IDLE_READERS: usize = 4;
const STATEMENT_CACHE: usize = 64;

pub const DEFAULT_USER_ID: i64 = 1;
pub const VARIOUS_ARTISTS_PUBLIC_ID: &str = "00000000-0000-4000-8000-000000000001";
pub const DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID: &str = "00000000-0000-4000-8000-000000000002";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this app supports ({supported})")]
    NewerSchema { found: i64, supported: i64 },
    #[error("WAL mode is unavailable (journal mode is {0}); the database must be on a local disk")]
    WalUnavailable(String),
}

/// Generates an opaque ID for a row that leaves the core.
pub fn new_public_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub struct Database {
    path: PathBuf,
    writer: Mutex<Connection>,
    idle_readers: Mutex<Vec<Connection>>,
}

impl Database {
    /// Opens or creates a database and brings its schema up to date.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        let mut writer = Connection::open(&path)?;
        configure_writer(&writer)?;
        migrate(&mut writer, MIGRATIONS)?;
        Ok(Self {
            path,
            writer: Mutex::new(writer),
            idle_readers: Mutex::new(Vec::new()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `work` on the single writer connection, waiting for any other writer.
    pub fn write<T, E: From<DbError>>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        work(&mut lock(&self.writer))
    }

    /// Runs `work` on a pooled read-only connection, which never waits for the writer.
    pub fn read<T, E: From<DbError>>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        let idle = lock(&self.idle_readers).pop();
        let reader = match idle {
            Some(reader) => reader,
            None => self.open_reader()?,
        };
        let result = work(&reader);
        let mut idle = lock(&self.idle_readers);
        if idle.len() < MAX_IDLE_READERS {
            idle.push(reader);
        }
        result
    }

    fn open_reader(&self) -> Result<Connection, DbError> {
        let reader = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        reader.busy_timeout(BUSY_TIMEOUT)?;
        Ok(reader)
    }
}

/// A panic mid-transaction rolls back when the transaction drops, so the connection
/// behind a poisoned lock is still sound.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn configure_writer(conn: &Connection) -> Result<(), DbError> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(DbError::WalUnavailable(mode));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Runs each pending migration in its own transaction.
fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<(), DbError> {
    let supported = migrations.len() as i64;
    let found: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > supported {
        return Err(DbError::NewerSchema { found, supported });
    }
    for (index, sql) in migrations.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::params;
    use tempfile::TempDir;

    use super::*;

    impl Database {
        fn writer(&mut self) -> &mut Connection {
            self.writer.get_mut().unwrap()
        }
    }

    fn open_temp() -> (TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        (dir, db)
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn insert_root(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO library_roots (public_id, name, path, added_at) VALUES (?1, 'Music', 'M:/', 0)",
            [new_public_id()],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_album(conn: &Connection, group_key: &str) -> i64 {
        conn.execute(
            "INSERT INTO albums (public_id, group_key, title, sort_title, album_artist_display, album_artist_sort)
             VALUES (?1, ?2, 'Album', 'album', 'Artist', 'artist')",
            params![new_public_id(), group_key],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_track(conn: &Connection, root: i64, album: i64, rel_path: &str) -> i64 {
        conn.execute(
            "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                                 title_sort, artist_sort, album_sort, album_artist_sort, added_at, updated_at)
             VALUES (?1, ?2, ?3, 1, 1, 'Title', ?4, 'title', 'artist', 'album', 'artist', 0, 0)",
            params![new_public_id(), root, rel_path, album],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn fixture_track(conn: &Connection) -> i64 {
        let root = insert_root(conn);
        let album = insert_album(conn, "album");
        insert_track(conn, root, album, "a.flac")
    }

    #[test]
    fn a_new_database_is_migrated_with_wal_and_foreign_keys() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        assert_eq!(user_version(conn), MIGRATIONS.len() as i64);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
    }

    #[test]
    fn reserved_rows_exist_and_match_the_constants() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let various: String = conn
            .query_row(
                "SELECT name FROM artists WHERE public_id = ?1",
                [VARIOUS_ARTISTS_PUBLIC_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(various, "Various Artists");
        let (user, kind): (i64, String) = conn
            .query_row(
                "SELECT user_id, kind FROM playlists WHERE public_id = ?1",
                [DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((user, kind.as_str()), (DEFAULT_USER_ID, "player"));
    }

    #[test]
    fn reopening_keeps_data_and_does_not_rerun_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("umak.db");
        {
            let mut db = Database::open(&path).unwrap();
            fixture_track(db.writer());
        }
        let mut db = Database::open(&path).unwrap();
        assert_eq!(count(db.writer(), "tracks"), 1);
        assert_eq!(count(db.writer(), "users"), 1);
    }

    #[test]
    fn a_database_from_a_newer_app_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("umak.db");
        let newer = MIGRATIONS.len() as i64 + 1;
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", newer)
            .unwrap();
        assert!(matches!(
            Database::open(&path),
            Err(DbError::NewerSchema { found, .. }) if found == newer
        ));
        assert_eq!(user_version(&Connection::open(&path).unwrap()), newer);
    }

    #[test]
    fn a_failed_migration_rolls_back_and_keeps_earlier_ones() {
        let mut conn = Connection::open_in_memory().unwrap();
        let result = migrate(
            &mut conn,
            &[
                "CREATE TABLE first (id INTEGER)",
                "CREATE TABLE second (id INTEGER); NOT VALID SQL",
            ],
        );
        assert!(result.is_err());
        assert_eq!(user_version(&conn), 1);
        assert_eq!(count(&conn, "first"), 0);
        assert!(conn.prepare("SELECT * FROM second").is_err());
    }

    #[test]
    fn a_reader_sees_committed_data_during_a_write_and_cannot_write() {
        let (_dir, mut db) = open_temp();
        fixture_track(db.writer());
        let reader = db.open_reader().unwrap();
        let tx = db.writer().transaction().unwrap();
        tx.execute("DELETE FROM raw_tags", []).unwrap();
        tx.execute("UPDATE tracks SET title = 'Changed'", [])
            .unwrap();
        let title: String = reader
            .query_row("SELECT title FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "Title");
        tx.commit().unwrap();
        assert!(reader.execute("DELETE FROM tracks", []).is_err());
    }

    #[test]
    fn per_user_data_blocks_deleting_a_track() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let track = fixture_track(conn);
        conn.execute(
            "INSERT INTO track_user_data (user_id, track_id, rating) VALUES (?1, ?2, 8)",
            params![DEFAULT_USER_ID, track],
        )
        .unwrap();
        assert!(
            conn.execute("DELETE FROM tracks WHERE id = ?1", [track])
                .is_err()
        );
        conn.execute("DELETE FROM track_user_data", []).unwrap();
        conn.execute("DELETE FROM tracks WHERE id = ?1", [track])
            .unwrap();
    }

    #[test]
    fn playlist_entries_block_deleting_a_track_and_may_repeat_it() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let track = fixture_track(conn);
        let insert =
            "INSERT INTO playlist_items (playlist_id, position, track_id) VALUES (1, ?1, ?2)";
        conn.execute(insert, params![1000, track]).unwrap();
        conn.execute(insert, params![2000, track]).unwrap();
        assert!(
            conn.execute("DELETE FROM tracks WHERE id = ?1", [track])
                .is_err()
        );
    }

    #[test]
    fn rebuildable_data_cascades_when_a_track_is_deleted() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let track = fixture_track(conn);
        conn.execute(
            "INSERT INTO raw_tags VALUES (?1, 'MOOD', 0, 'Calm')",
            [track],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track_artists VALUES (?1, 1, 'artist', 0)",
            [track],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tracks_fts (rowid, title, artists, album, genres) VALUES (?1, 'Title', '', '', '')",
            [track],
        )
        .unwrap();
        conn.execute("DELETE FROM tracks WHERE id = ?1", [track])
            .unwrap();
        assert_eq!(count(conn, "raw_tags"), 0);
        assert_eq!(count(conn, "track_artists"), 0);
        assert_eq!(count(conn, "tracks_fts"), 0);
    }

    #[test]
    fn constraints_reject_invalid_values() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let track = fixture_track(conn);
        let rating = "INSERT INTO track_user_data (user_id, track_id, rating) VALUES (1, ?1, ?2)";
        assert!(conn.execute(rating, params![track, 11]).is_err());
        assert!(conn.execute(rating, params![track, -1]).is_err());
        conn.execute(rating, params![track, 10]).unwrap();
        assert!(
            conn.execute("UPDATE albums SET compilation = 'maybe'", [])
                .is_err()
        );
        assert!(
            conn.execute("INSERT INTO tracks (public_id) VALUES ('incomplete')", [])
                .is_err()
        );
    }

    #[test]
    fn a_path_is_unique_within_a_root_only() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let root = insert_root(conn);
        let album = insert_album(conn, "album");
        insert_track(conn, root, album, "a.flac");
        let duplicate = "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                                 title_sort, artist_sort, album_sort, album_artist_sort, added_at, updated_at)
             VALUES ('other', ?1, 'a.flac', 1, 1, 'T', ?2, 't', 'a', 'a', 'a', 0, 0)";
        assert!(conn.execute(duplicate, params![root, album]).is_err());
        conn.execute(
            "INSERT INTO library_roots (public_id, name, path, added_at) VALUES ('r2', 'Other', 'N:/', 0)",
            [],
        )
        .unwrap();
        conn.execute(duplicate, params![conn.last_insert_rowid(), album])
            .unwrap();
    }

    #[test]
    fn each_user_can_have_only_one_player_playlist() {
        let (_dir, mut db) = open_temp();
        let result = db.writer().execute(
            "INSERT INTO playlists (public_id, user_id, name, kind, created_at, updated_at)
             VALUES ('second', 1, 'Second', 'player', 0, 0)",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_smart_playlist_requires_a_rule_and_others_forbid_one() {
        let (_dir, mut db) = open_temp();
        let insert =
            "INSERT INTO playlists (public_id, user_id, name, kind, rule, created_at, updated_at)
                      VALUES (?1, 1, 'P', ?2, ?3, 0, 0)";
        let conn = db.writer();
        assert!(
            conn.execute(insert, params!["a", "smart", None::<String>])
                .is_err()
        );
        assert!(conn.execute(insert, params!["b", "manual", "{}"]).is_err());
        conn.execute(insert, params!["c", "smart", "{}"]).unwrap();
    }

    fn insert_fts(conn: &Connection, rowid: i64, title: &str, artists: &str) {
        conn.execute(
            "INSERT INTO tracks_fts (rowid, title, artists, album, genres) VALUES (?1, ?2, ?3, '', '')",
            params![rowid, title, artists],
        )
        .unwrap();
    }

    fn search(conn: &Connection, query: &str) -> Vec<i64> {
        let mut statement = conn
            .prepare("SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ?1 ORDER BY rowid")
            .unwrap();
        statement
            .query_map([query], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn search_ignores_accents_and_case_and_matches_prefixes() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        insert_fts(conn, 1, "Symphonie Nr. 4", "Christoph von Dohnányi");
        insert_fts(conn, 2, "Tweezer", "Phish");
        assert_eq!(search(conn, "dohnanyi"), [1]);
        assert_eq!(search(conn, "DOHNÁNYI"), [1]);
        assert_eq!(search(conn, "do*"), [1]);
        assert_eq!(search(conn, "tweez*"), [2]);
        assert!(search(conn, "zeppelin").is_empty());
    }

    fn query_plan(conn: &Connection, sql: &str) -> String {
        let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let details: Vec<String> = statement
            .query_map([], |row| row.get(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        details.join("\n")
    }

    #[test]
    fn keyset_browse_queries_use_their_index_without_sorting_or_joins() {
        let (_dir, mut db) = open_temp();
        let conn = db.writer();
        let cases = [
            (
                "idx_tracks_browse",
                "(album_artist_sort, album_sort, album_id, disc_no, track_no, id)",
                "('a', 'b', 1, 1, 1, 1)",
            ),
            (
                "idx_tracks_artist",
                "(artist_sort, album_sort, album_id, disc_no, track_no, id)",
                "('a', 'b', 1, 1, 1, 1)",
            ),
            ("idx_tracks_title", "(title_sort, id)", "('a', 1)"),
            ("idx_tracks_added", "(added_at, id)", "(1, 1)"),
        ];
        for (index, columns, after) in cases {
            let order = columns.trim_matches(['(', ')']);
            let plan = query_plan(
                conn,
                &format!(
                    "SELECT public_id, title, artist_display, album_display, duration_ms FROM tracks
                     WHERE {columns} > {after} ORDER BY {order} LIMIT 200"
                ),
            );
            assert!(plan.contains(index), "{index} not used: {plan}");
            assert!(
                !plan.contains("TEMP B-TREE"),
                "{index} needs a sort: {plan}"
            );
        }
    }

    #[test]
    fn read_works_while_the_writer_is_held_and_write_is_visible_after() {
        let (_dir, db) = open_temp();
        db.write(|conn| {
            let root = insert_root(conn);
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE library_roots SET name = 'Changed' WHERE id = ?1",
                [root],
            )?;
            let name: String = db.read(|reader| {
                reader
                    .query_row("SELECT name FROM library_roots", [], |row| row.get(0))
                    .map_err(DbError::from)
            })?;
            assert_eq!(name, "Music");
            tx.commit()?;
            Ok::<_, DbError>(())
        })
        .unwrap();
        let name: String = db
            .read(|reader| {
                reader
                    .query_row("SELECT name FROM library_roots", [], |row| row.get(0))
                    .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(name, "Changed");
    }

    #[test]
    fn idle_readers_are_reused_and_capped() {
        let (_dir, db) = open_temp();
        fn nested(db: &Database, depth: usize) -> Result<(), DbError> {
            if depth == 0 {
                return Ok(());
            }
            db.read(|_| nested(db, depth - 1))
        }
        nested(&db, MAX_IDLE_READERS + 3).unwrap();
        assert_eq!(db.idle_readers.lock().unwrap().len(), MAX_IDLE_READERS);
        nested(&db, 1).unwrap();
        assert_eq!(db.idle_readers.lock().unwrap().len(), MAX_IDLE_READERS);
    }

    #[test]
    fn public_ids_are_unique() {
        assert_ne!(new_public_id(), new_public_id());
    }
}
