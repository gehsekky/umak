//! Browsing albums, for the album grid.
//!
//! Pages work like the track list's: by position, walking an index for each order, with
//! search limited to albums holding a matching track. Those albums are found once and passed
//! to the page as a list, since finding them is most of the work when a search matches most
//! of the library. An album's cover is the cached art of its first track, by disc and track,
//! that has some. Until every track has been looked at, a view showing the album can ask for
//! the next one with `unchecked_track`, so covers on screen need not wait for the whole cache.

use std::rc::Rc;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::library::{MAX_PAGE, TRACK_COLUMNS, TrackRow, search_query};

/// The columns `AlbumRow::from_row` reads, for a query that names `albums` as `a`. Art is
/// current only while the track's size and mtime match those it was read from.
const ALBUM_COLUMNS: &str = "a.public_id, a.title, a.album_artist_display, a.year,
    (SELECT count(*) FROM tracks c WHERE c.album_id = a.id),
    (SELECT ca.art FROM tracks c JOIN track_art ca
        ON ca.track_id = c.id AND ca.size = c.size AND ca.mtime = c.mtime
     WHERE c.album_id = a.id AND ca.art IS NOT NULL
     ORDER BY c.disc_no, c.track_no, c.id LIMIT 1)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AlbumSort {
    /// Album artist, then title.
    Artist,
    /// Title, then album artist.
    Title,
    /// Year, then album artist and title. Albums with no year come first.
    Year,
}

impl AlbumSort {
    fn index(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Artist => (
                "idx_albums_browse",
                &["album_artist_sort", "sort_title", "id"],
            ),
            Self::Title => (
                "idx_albums_title",
                &["sort_title", "album_artist_sort", "id"],
            ),
            Self::Year => (
                "idx_albums_year",
                &["year", "album_artist_sort", "sort_title", "id"],
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AlbumRow {
    pub id: String,
    pub title: String,
    /// The album artist as tagged, or as the scanner judged it.
    pub artist: String,
    pub year: Option<i64>,
    pub track_count: i64,
    /// The cover's picture, for `<art>-small` or `<art>-large`. None when no track's art is
    /// cached, which may be because it has not been looked for yet.
    pub art: Option<String>,
    /// There is no cover yet, but some tracks have not been looked at since they last changed.
    pub art_pending: bool,
}

impl AlbumRow {
    /// Reads `ALBUM_COLUMNS`, which start at column `first`.
    fn from_row(row: &Row<'_>, first: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(first)?,
            title: row.get(first + 1)?,
            artist: row.get(first + 2)?,
            year: row.get(first + 3)?,
            track_count: row.get(first + 4)?,
            art: row.get(first + 5)?,
            art_pending: false,
        })
    }

    /// Sets `art_pending`, which only albums with no cover need.
    fn check_pending(mut self, conn: &Connection) -> rusqlite::Result<Self> {
        if self.art.is_none() {
            self.art_pending = unchecked_track(conn, &self.id)?.is_some();
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct AlbumPage {
    pub albums: Vec<AlbumRow>,
    /// How many albums the whole list holds.
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct AlbumTracks {
    pub album: AlbumRow,
    /// By disc and track.
    pub tracks: Vec<TrackRow>,
}

const MATCHING_ALBUMS: &str = "SELECT DISTINCT t.album_id FROM tracks t
    INDEXED BY idx_tracks_album_of
    WHERE t.id IN (SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ?1)";

/// Returns a page of albums in the given order, limited to albums with a track matching
/// `text` if any.
pub fn album_page(
    conn: &Connection,
    text: Option<&str>,
    sort: AlbumSort,
    descending: bool,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<AlbumPage> {
    let (index, columns) = sort.index();
    let direction = if descending { " DESC" } else { "" };
    let order = columns
        .iter()
        .map(|column| format!("a.{column}{direction}"))
        .collect::<Vec<_>>()
        .join(", ");
    let matching = match text.and_then(search_query) {
        Some(search) => Some(Rc::new(
            conn.prepare_cached(MATCHING_ALBUMS)?
                .query_map(params![search], |row| row.get(0).map(Value::Integer))?
                .collect::<Result<Vec<_>, _>>()?,
        )),
        None => None,
    };
    let filter = if matching.is_some() {
        "WHERE a.id IN rarray(?3)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {ALBUM_COLUMNS} FROM albums AS a INDEXED BY {index} {filter}
         ORDER BY {order} LIMIT ?1 OFFSET ?2"
    );
    let limit = limit.min(MAX_PAGE);
    let mut bound: Vec<&dyn ToSql> = vec![&limit, &offset];
    if let Some(matching) = &matching {
        bound.push(matching);
    }
    let albums = conn
        .prepare_cached(&sql)?
        .query_map(bound.as_slice(), |row| AlbumRow::from_row(row, 0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|album| album.check_pending(conn))
        .collect::<Result<_, _>>()?;

    let total = match &matching {
        Some(matching) => matching.len() as u32,
        None => conn
            .prepare_cached("SELECT count(*) FROM albums")?
            .query_row([], |row| row.get(0))?,
    };
    Ok(AlbumPage { albums, total })
}

/// An album and its tracks, or None if there is no such album.
pub fn album_tracks(conn: &Connection, id: &str) -> rusqlite::Result<Option<AlbumTracks>> {
    let found = conn
        .prepare_cached(&format!(
            "SELECT a.id, {ALBUM_COLUMNS} FROM albums a WHERE a.public_id = ?1"
        ))?
        .query_row(params![id], |row| {
            let key: i64 = row.get(0)?;
            Ok((key, AlbumRow::from_row(row, 1)?))
        })
        .optional()?;
    let Some((key, album)) = found else {
        return Ok(None);
    };
    let album = album.check_pending(conn)?;
    let tracks = conn
        .prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks AS t INDEXED BY idx_tracks_album
             WHERE t.album_id = ?1 ORDER BY t.disc_no, t.track_no, t.id"
        ))?
        .query_map(params![key], |row| TrackRow::from_row(row, 0))?
        .collect::<Result<_, _>>()?;
    Ok(Some(AlbumTracks { album, tracks }))
}

/// The album's first track, by disc and track, whose art has not been looked for since the
/// file last changed. Missing files are passed over, since they cannot be read.
pub fn unchecked_track(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT c.public_id FROM albums a
         JOIN tracks c INDEXED BY idx_tracks_album ON c.album_id = a.id
         LEFT JOIN track_art ca ON ca.track_id = c.id AND ca.size = c.size AND ca.mtime = c.mtime
         WHERE a.public_id = ?1 AND c.missing_since IS NULL AND ca.track_id IS NULL
         ORDER BY c.disc_no, c.track_no, c.id LIMIT 1",
    )?
    .query_row(params![album_id], |row| row.get(0))
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbError};

    /// Title, album artist and year of each album. Tracks are added by each test.
    const ALBUMS: &[(&str, &str, Option<i64>)] = &[
        ("Charlie", "Yan", Some(1999)),
        ("Alpha", "Zed", Some(2001)),
        ("Bravo", "Xia", None),
    ];

    fn library() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                 VALUES (1, 'r', 'Music', 'M:/', 0)",
                [],
            )?;
            for (number, (title, artist, year)) in ALBUMS.iter().enumerate() {
                conn.execute(
                    "INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                         album_artist_display, album_artist_sort, year)
                     VALUES (?1, ?2, ?2, ?3, lower(?3), ?4, lower(?4), ?5)",
                    params![
                        number as i64 + 1,
                        format!("album-{title}"),
                        title,
                        artist,
                        year
                    ],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        (dir, db)
    }

    /// Adds a track to an album, numbered from 1 in `ALBUMS` order, and returns its row ID.
    fn add_track(db: &Database, album: i64, title: &str, disc: i64, track: i64) -> i64 {
        db.write(|conn| {
            conn.execute(
                "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                     disc_no, track_no, artist_display, album_display, title_sort, artist_sort,
                     album_sort, album_artist_sort, added_at, updated_at)
                 VALUES (?1, 1, ?1, 10, 20, ?2, ?3, ?4, ?5, 'A', '', lower(?2), 'a', '', '', 0, 0)",
                params![format!("{album}-{title}"), title, album, disc, track],
            )?;
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO tracks_fts (rowid, title, artists, album, genres)
                 VALUES (?1, ?2, 'A', '', '')",
                params![id, title],
            )?;
            Ok::<_, DbError>(id)
        })
        .unwrap()
    }

    fn set_art(db: &Database, track: i64, art: Option<&str>, mtime: i64) {
        db.write(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO track_art (track_id, size, mtime, art, source)
                 VALUES (?1, 10, ?2, ?3, iif(?3 IS NULL, 'none', 'embedded'))",
                params![track, mtime, art],
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();
    }

    fn page(db: &Database, text: Option<&str>, sort: AlbumSort, descending: bool) -> AlbumPage {
        db.read(|conn| album_page(conn, text, sort, descending, 0, 10).map_err(DbError::from))
            .unwrap()
    }

    fn titles(page: &AlbumPage) -> Vec<&str> {
        page.albums
            .iter()
            .map(|album| album.title.as_str())
            .collect()
    }

    #[test]
    fn each_sort_orders_the_albums_in_both_directions() {
        let (_dir, db) = library();
        let cases = [
            (AlbumSort::Artist, ["Bravo", "Charlie", "Alpha"]),
            (AlbumSort::Title, ["Alpha", "Bravo", "Charlie"]),
            (AlbumSort::Year, ["Bravo", "Charlie", "Alpha"]),
        ];
        for (sort, expected) in cases {
            let found = page(&db, None, sort, false);
            assert_eq!(titles(&found), expected, "{sort:?}");
            assert_eq!(found.total, 3);
            let mut reversed = expected;
            reversed.reverse();
            assert_eq!(titles(&page(&db, None, sort, true)), reversed, "{sort:?}");
        }
        let one = db
            .read(|conn| {
                album_page(conn, None, AlbumSort::Title, false, 1, 1).map_err(DbError::from)
            })
            .unwrap();
        assert_eq!((titles(&one), one.total), (vec!["Bravo"], 3));
    }

    #[test]
    fn search_lists_each_album_holding_a_match_once() {
        let (_dir, db) = library();
        add_track(&db, 1, "Tweezer", 1, 1);
        add_track(&db, 1, "Tweezer Reprise", 1, 2);
        add_track(&db, 2, "Tweed", 1, 1);
        add_track(&db, 3, "Other", 1, 1);
        let found = page(&db, Some("twee"), AlbumSort::Title, false);
        assert_eq!((titles(&found), found.total), (vec!["Alpha", "Charlie"], 2));
        let found = page(&db, Some("tweez rep"), AlbumSort::Title, false);
        assert_eq!((titles(&found), found.total), (vec!["Charlie"], 1));
        assert_eq!(page(&db, Some("zzz"), AlbumSort::Title, false).total, 0);
        assert_eq!(page(&db, Some("  "), AlbumSort::Title, false).total, 3);
    }

    #[test]
    fn the_cover_is_the_first_track_with_current_art() {
        let (_dir, db) = library();
        let first = add_track(&db, 1, "One", 1, 1);
        let second = add_track(&db, 1, "Two", 1, 2);
        let later_disc = add_track(&db, 1, "Three", 2, 1);
        set_art(&db, later_disc, Some("disc2"), 20);
        set_art(&db, second, Some("track2"), 20);
        set_art(&db, first, None, 20);
        let charlie = |db: &Database| {
            page(db, None, AlbumSort::Title, false)
                .albums
                .into_iter()
                .find(|album| album.title == "Charlie")
                .unwrap()
        };
        let album = charlie(&db);
        assert_eq!(album.art.as_deref(), Some("track2"));
        assert_eq!(album.track_count, 3);

        // Art read before the file last changed no longer counts.
        set_art(&db, second, Some("track2"), 19);
        assert_eq!(charlie(&db).art.as_deref(), Some("disc2"));
        set_art(&db, first, Some("track1"), 20);
        assert_eq!(charlie(&db).art.as_deref(), Some("track1"));

        let bare = page(&db, None, AlbumSort::Title, false);
        assert_eq!(bare.albums[0].art, None);
        assert_eq!(bare.albums[0].track_count, 0);
        assert!(!bare.albums[0].art_pending);
    }

    #[test]
    fn an_album_without_a_cover_is_pending_until_each_track_is_looked_at_in_order() {
        let (_dir, db) = library();
        let first = add_track(&db, 2, "One", 1, 1);
        let gone = add_track(&db, 2, "Gone", 1, 2);
        let later = add_track(&db, 2, "Three", 1, 3);
        db.write(|conn| {
            conn.execute(
                "UPDATE tracks SET missing_since = 1 WHERE id = ?1",
                params![gone],
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();
        let alpha = |db: &Database| {
            let page = page(db, None, AlbumSort::Title, false);
            let album = page.albums.into_iter().next().unwrap();
            let next = db
                .read(|conn| unchecked_track(conn, &album.id).map_err(DbError::from))
                .unwrap();
            (album.art_pending, next)
        };
        assert_eq!(alpha(&db), (true, Some("2-One".to_owned())));
        set_art(&db, first, None, 20);
        assert_eq!(alpha(&db), (true, Some("2-Three".to_owned())));
        set_art(&db, later, None, 19);
        assert_eq!(
            alpha(&db),
            (true, Some("2-Three".to_owned())),
            "stale art counts as unchecked"
        );
        set_art(&db, later, None, 20);
        assert_eq!(alpha(&db), (false, None));
        set_art(&db, first, Some("found"), 20);
        let page = page(&db, None, AlbumSort::Title, false);
        assert_eq!(page.albums[0].art.as_deref(), Some("found"));
        assert!(!page.albums[0].art_pending);
    }

    #[test]
    fn an_album_lists_its_tracks_by_disc_and_track() {
        let (_dir, db) = library();
        add_track(&db, 1, "Disc two", 2, 1);
        add_track(&db, 1, "Second", 1, 2);
        add_track(&db, 1, "First", 1, 1);
        add_track(&db, 2, "Elsewhere", 1, 1);
        let found = db
            .read(|conn| album_tracks(conn, "album-Charlie").map_err(DbError::from))
            .unwrap()
            .unwrap();
        assert_eq!(found.album.title, "Charlie");
        assert_eq!(found.album.year, Some(1999));
        let tracks: Vec<&str> = found
            .tracks
            .iter()
            .map(|track| track.title.as_str())
            .collect();
        assert_eq!(tracks, ["First", "Second", "Disc two"]);
        let missing = db
            .read(|conn| album_tracks(conn, "nothing").map_err(DbError::from))
            .unwrap();
        assert_eq!(missing, None);
    }

    #[test]
    fn browse_and_search_walk_their_index_without_sorting() {
        let (_dir, db) = library();
        for sort in [AlbumSort::Artist, AlbumSort::Title, AlbumSort::Year] {
            for descending in [false, true] {
                let (index, columns) = sort.index();
                let direction = if descending { " DESC" } else { "" };
                let order = columns
                    .iter()
                    .map(|column| format!("a.{column}{direction}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let plan = query_plan(
                    &db,
                    &format!(
                        "SELECT {ALBUM_COLUMNS} FROM albums AS a INDEXED BY {index}
                         WHERE a.id IN rarray(?3) ORDER BY {order} LIMIT ?1 OFFSET ?2"
                    ),
                );
                assert!(plan.contains(index), "{plan}");
                assert!(!plan.contains("TEMP B-TREE"), "{plan}");
            }
        }
        let plan = query_plan(&db, MATCHING_ALBUMS);
        assert!(
            plan.contains("COVERING INDEX idx_tracks_album_of"),
            "{plan}"
        );
    }

    fn query_plan(db: &Database, sql: &str) -> String {
        db.read(|conn| {
            let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
            // Planning needs the parameters bound, but not to anything in particular.
            let unbound = std::iter::repeat_n(Value::Null, statement.parameter_count());
            let rows = statement.query_map(rusqlite::params_from_iter(unbound), |row| {
                row.get::<_, String>(3)
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        })
        .unwrap()
        .join(" | ")
    }
}
