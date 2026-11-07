//! Browsing and searching tracks.
//!
//! Lists are paged by position, because a virtualized list must be able to jump to any
//! row when the scrollbar is dragged, which keyset paging cannot do. Each sort has an
//! index and tracks hold copies of their album values, so a page needs no sort and no
//! joins. Search walks the same index and stops once the page is full, instead of
//! sorting every match: a short term can match most of the library.

use rusqlite::{Connection, Row, ToSql, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MAX_PAGE: u32 = 1000;

/// The columns `TrackRow::from_row` reads, for a query that names `tracks` as `t`.
///
/// What belongs to the person comes from one lookup per row, for the default user, whose ID is 1.
pub const TRACK_COLUMNS: &str = "t.public_id, t.title, t.artist_display, t.album_display,
    t.duration_ms, t.track_no, t.disc_no, t.year, t.codec, t.sample_rate, t.bit_depth,
    t.missing_since IS NOT NULL,
    (SELECT d.rating FROM track_user_data d WHERE d.user_id = 1 AND d.track_id = t.id),
    coalesce((SELECT d.play_count FROM track_user_data d
              WHERE d.user_id = 1 AND d.track_id = t.id), 0),
    (SELECT d.last_played_at FROM track_user_data d WHERE d.user_id = 1 AND d.track_id = t.id)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum TrackSort {
    /// Album artist, then album, disc and track.
    Album,
    /// Track artist, then album, disc and track.
    Artist,
    Title,
    Added,
}

impl TrackSort {
    fn index(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Album => (
                "idx_tracks_browse",
                &[
                    "album_artist_sort",
                    "album_sort",
                    "album_id",
                    "disc_no",
                    "track_no",
                    "id",
                ],
            ),
            Self::Artist => (
                "idx_tracks_artist",
                &[
                    "artist_sort",
                    "album_sort",
                    "album_id",
                    "disc_no",
                    "track_no",
                    "id",
                ],
            ),
            Self::Title => ("idx_tracks_title", &["title_sort", "id"]),
            Self::Added => ("idx_tracks_added", &["added_at", "id"]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TrackRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<i64>,
    pub track_no: i64,
    pub disc_no: i64,
    pub year: Option<i64>,
    pub codec: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub missing: bool,
    /// From 0 to 10, in half-stars.
    pub rating: Option<i64>,
    pub play_count: i64,
    /// Milliseconds since the Unix epoch.
    pub last_played_at: Option<i64>,
}

impl TrackRow {
    /// Reads `TRACK_COLUMNS`, which start at column `first`.
    pub fn from_row(row: &Row<'_>, first: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(first)?,
            title: row.get(first + 1)?,
            artist: row.get(first + 2)?,
            album: row.get(first + 3)?,
            duration_ms: row.get(first + 4)?,
            track_no: row.get(first + 5)?,
            disc_no: row.get(first + 6)?,
            year: row.get(first + 7)?,
            codec: row.get(first + 8)?,
            sample_rate: row.get(first + 9)?,
            bit_depth: row.get(first + 10)?,
            missing: row.get(first + 11)?,
            rating: row.get(first + 12)?,
            play_count: row.get(first + 13)?,
            last_played_at: row.get(first + 14)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct TrackPage {
    pub tracks: Vec<TrackRow>,
    /// How many tracks the whole list holds.
    pub total: u32,
}

/// Turns what the user typed into an FTS5 query: every word must match as a prefix.
/// Quoting each word means FTS5 operators and punctuation in the text are harmless.
pub fn search_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// Returns a page of tracks in the given order, limited to matches of `text` if any.
/// Missing tracks are included, so the UI can show them as missing.
pub fn track_page(
    conn: &Connection,
    text: Option<&str>,
    sort: TrackSort,
    descending: bool,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<TrackPage> {
    let (index, columns) = sort.index();
    let direction = if descending { " DESC" } else { "" };
    let order = columns
        .iter()
        .map(|column| format!("t.{column}{direction}"))
        .collect::<Vec<_>>()
        .join(", ");
    let search = text.and_then(search_query);
    let filter = if search.is_some() {
        "WHERE t.id IN (SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ?3)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {TRACK_COLUMNS} FROM tracks AS t INDEXED BY {index} {filter}
         ORDER BY {order} LIMIT ?1 OFFSET ?2"
    );
    let limit = limit.min(MAX_PAGE);
    let mut bound: Vec<&dyn ToSql> = vec![&limit, &offset];
    if let Some(search) = &search {
        bound.push(search);
    }
    let tracks = conn
        .prepare_cached(&sql)?
        .query_map(bound.as_slice(), |row| TrackRow::from_row(row, 0))?
        .collect::<Result<_, _>>()?;

    let total = match &search {
        Some(search) => conn
            .prepare_cached("SELECT count(*) FROM tracks_fts WHERE tracks_fts MATCH ?1")?
            .query_row(params![search], |row| row.get(0))?,
        None => conn
            .prepare_cached("SELECT count(*) FROM tracks")?
            .query_row([], |row| row.get(0))?,
    };
    Ok(TrackPage { tracks, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbError, new_public_id};

    fn library(tracks: &[(&str, &str, &str, i64)]) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                 VALUES (1, 'r', 'Music', 'M:/', 0)",
                [],
            )?;
            conn.execute(
                "INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                 VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x')",
                [],
            )?;
            for (number, (title, artist, album_sort, added)) in tracks.iter().enumerate() {
                let sort_artist = artist.to_lowercase();
                let sort_title = title.to_lowercase();
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                         artist_display, album_display, title_sort, artist_sort, album_sort,
                         album_artist_sort, added_at, updated_at)
                     VALUES (?1, 1, ?2, 1, 1, ?3, 1, ?4, ?5, ?6, ?7, ?5, ?7, ?8, 0)",
                    params![
                        new_public_id(),
                        format!("{number}.flac"),
                        title,
                        artist,
                        album_sort,
                        sort_title,
                        sort_artist,
                        added
                    ],
                )?;
                conn.execute(
                    "INSERT INTO tracks_fts (rowid, title, artists, album, genres)
                     VALUES (?1, ?2, ?3, ?4, '')",
                    params![conn.last_insert_rowid(), title, artist, album_sort],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        (dir, db)
    }

    fn titles(
        db: &Database,
        text: Option<&str>,
        sort: TrackSort,
        descending: bool,
        offset: u32,
        limit: u32,
    ) -> (Vec<String>, u32) {
        db.read(|conn| {
            let page = track_page(conn, text, sort, descending, offset, limit)?;
            Ok::<_, DbError>((
                page.tracks.into_iter().map(|track| track.title).collect(),
                page.total,
            ))
        })
        .unwrap()
    }

    const TRACKS: &[(&str, &str, &str, i64)] = &[
        ("Charlie", "Zed", "b", 30),
        ("Alpha", "Yan", "c", 10),
        ("Bravo", "Xia", "a", 20),
    ];

    #[test]
    fn each_sort_orders_the_list_in_both_directions() {
        let (_dir, db) = library(TRACKS);
        let cases = [
            (TrackSort::Title, ["Alpha", "Bravo", "Charlie"]),
            (TrackSort::Artist, ["Bravo", "Alpha", "Charlie"]),
            (TrackSort::Album, ["Bravo", "Alpha", "Charlie"]),
            (TrackSort::Added, ["Alpha", "Bravo", "Charlie"]),
        ];
        for (sort, expected) in cases {
            assert_eq!(
                titles(&db, None, sort, false, 0, 10).0,
                expected,
                "{sort:?}"
            );
            let mut reversed = expected;
            reversed.reverse();
            assert_eq!(titles(&db, None, sort, true, 0, 10).0, reversed, "{sort:?}");
        }
    }

    #[test]
    fn a_page_can_start_anywhere_and_reports_the_whole_list() {
        let (_dir, db) = library(TRACKS);
        assert_eq!(
            titles(&db, None, TrackSort::Title, false, 1, 1),
            (vec!["Bravo".to_owned()], 3)
        );
        assert_eq!(
            titles(&db, None, TrackSort::Title, false, 9, 5),
            (Vec::new(), 3)
        );
    }

    #[test]
    fn search_matches_prefixes_of_every_word_in_sorted_order_and_counts_all_matches() {
        let (_dir, db) = library(&[
            ("Tweezer", "Phish", "z", 1),
            ("Tweezer Reprise", "Phish", "a", 2),
            ("Other", "Phish", "m", 3),
            ("Tweed", "Someone", "m", 4),
        ]);
        let sort = TrackSort::Title;
        let search = |text| titles(&db, Some(text), sort, false, 0, 10);
        assert_eq!(
            search("twe"),
            (
                vec![
                    "Tweed".to_owned(),
                    "Tweezer".to_owned(),
                    "Tweezer Reprise".to_owned()
                ],
                3
            )
        );
        assert_eq!(search("tweez ph").1, 2);
        assert_eq!(search("zzz"), (Vec::new(), 0));
        assert_eq!(titles(&db, Some("twe"), sort, false, 1, 1).0, ["Tweezer"]);
        assert_eq!(titles(&db, Some("twe"), sort, false, 0, 1).1, 3);
    }

    #[test]
    fn blank_search_text_means_the_whole_list() {
        let (_dir, db) = library(TRACKS);
        assert_eq!(
            titles(&db, Some("   "), TrackSort::Title, false, 0, 10).1,
            3
        );
    }

    #[test]
    fn search_text_cannot_inject_query_syntax_or_fail() {
        let (_dir, db) = library(TRACKS);
        for text in [
            "\"",
            "\"\"",
            "'",
            "*",
            "-",
            "+",
            "(",
            ")",
            "^",
            ":",
            "!!!",
            "a\"b",
            "NEAR(a b)",
            "title:alpha",
            "alpha OR bravo",
            "NOT",
            "AND",
            "alpha AND",
            "\\",
            "%",
            "{",
        ] {
            db.read(|conn| {
                track_page(conn, Some(text), TrackSort::Title, false, 0, 10).map_err(DbError::from)
            })
            .unwrap_or_else(|error| panic!("{text:?}: {error}"));
        }
        // OR is a word to look for, not an operator.
        assert_eq!(
            titles(&db, Some("alpha OR bravo"), TrackSort::Title, false, 0, 10).1,
            0
        );
    }

    #[test]
    fn the_limit_is_capped() {
        let tracks: Vec<(&str, &str, &str, i64)> = (0..MAX_PAGE + 5)
            .map(|index| ("Song", "Artist", "album", i64::from(index)))
            .collect();
        let (_dir, db) = library(&tracks);
        let (rows, total) = titles(&db, None, TrackSort::Added, false, 0, u32::MAX);
        assert_eq!(rows.len(), MAX_PAGE as usize);
        assert_eq!(total, MAX_PAGE + 5);
    }

    #[test]
    fn a_missing_track_is_listed_and_marked() {
        let (_dir, db) = library(TRACKS);
        db.write(|conn| {
            conn.execute(
                "UPDATE tracks SET missing_since = 1 WHERE title = 'Alpha'",
                [],
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();
        let page = db
            .read(|conn| {
                track_page(conn, None, TrackSort::Title, false, 0, 10).map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(page.total, 3);
        assert!(page.tracks[0].missing);
        assert!(!page.tracks[1].missing);
    }

    #[test]
    fn browse_and_search_walk_their_index_without_sorting() {
        let (_dir, db) = library(TRACKS);
        for sort in [
            TrackSort::Album,
            TrackSort::Artist,
            TrackSort::Title,
            TrackSort::Added,
        ] {
            let (index, columns) = sort.index();
            let order = columns
                .iter()
                .map(|column| format!("t.{column}"))
                .collect::<Vec<_>>()
                .join(", ");
            let plan = db
                .read(|conn| {
                    let sql = format!(
                        "EXPLAIN QUERY PLAN SELECT {TRACK_COLUMNS} FROM tracks AS t INDEXED BY {index}
                         WHERE t.id IN (SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH 'a')
                         ORDER BY {order} LIMIT 10"
                    );
                    let mut statement = conn.prepare(&sql)?;
                    let rows = statement.query_map([], |row| row.get::<_, String>(3))?;
                    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
                })
                .unwrap()
                .join(" | ");
            assert!(plan.contains(index), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        }
    }
}
