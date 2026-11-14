//! Browsing artists, for the artist list and an artist's page.
//!
//! A track belongs to its album's artists when the album was tagged with an album artist,
//! and otherwise to its own artists. So an album with no album artist tag lists under each
//! artist on it, rather than under the artist, or Various Artists, that the scanner judged
//! for the album as a whole. Artists named only as composers, or as guests on an album
//! tagged with another album artist, do not list.
//!
//! A page of the list counts every artist's albums and tracks in two passes, one over the
//! tagged albums' artists and one over the tracks of the other albums, and walks the artists
//! by sort name keeping those with any. That costs the same wherever the page starts, where
//! counting each artist on the page alone costs as much as their tracks.

use std::collections::HashMap;
use std::rc::Rc;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use serde::Serialize;
use ts_rs::TS;

use crate::albums::{ALBUM_COLUMNS, AlbumRow};
use crate::library::MAX_PAGE;

/// The tracks that belong to artist `?1`, as `(id, album_id)`. The two halves never meet,
/// since an album either has an album artist tag or not, and each names an artist once.
const ARTIST_TRACKS: &str = "
    SELECT t.id, t.album_id FROM album_artists aa
    JOIN albums a ON a.id = aa.album_id AND a.artist_source = 'tag'
    JOIN tracks t INDEXED BY idx_tracks_album ON t.album_id = a.id
    WHERE aa.artist_id = ?1
    UNION ALL
    SELECT t.id, t.album_id FROM track_artists ta
    JOIN tracks t INDEXED BY idx_tracks_album_of ON t.id = ta.track_id
    JOIN albums a ON a.id = t.album_id AND a.artist_source <> 'tag'
    WHERE ta.artist_id = ?1 AND ta.role = 'artist'";

/// `(artist, albums, tracks)` for every artist of an album tagged with album artists.
const TAGGED_COUNTS: &str = "
    SELECT aa.artist_id, count(*),
           sum((SELECT count(*) FROM tracks t INDEXED BY idx_tracks_album
                WHERE t.album_id = a.id))
    FROM album_artists aa
    JOIN albums a ON a.id = aa.album_id AND a.artist_source = 'tag'
    GROUP BY aa.artist_id";

/// `(artist, albums, tracks)` for every artist on a track of the other albums. Starting
/// from the albums reads only their tracks, rather than every track to find them.
const UNTAGGED_COUNTS: &str = "
    SELECT ta.artist_id, count(DISTINCT t.album_id), count(*)
    FROM albums a
    CROSS JOIN tracks t INDEXED BY idx_tracks_album ON t.album_id = a.id
    JOIN track_artists ta ON ta.track_id = t.id AND ta.role = 'artist'
    WHERE a.artist_source <> 'tag'
    GROUP BY ta.artist_id";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
    pub album_count: i64,
    pub track_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ArtistPage {
    pub artists: Vec<ArtistRow>,
    /// How many artists the whole list holds.
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ArtistAlbums {
    pub artist: ArtistRow,
    /// Each album holding a track of the artist's, oldest first.
    pub albums: Vec<AlbumRow>,
}

type Found = (i64, String, String);

fn found(row: &Row<'_>) -> rusqlite::Result<Found> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

/// An artist by their public ID, as `(row ID, public ID, name)`.
fn find(conn: &Connection, id: &str) -> rusqlite::Result<Option<Found>> {
    conn.prepare_cached("SELECT id, public_id, name FROM artists WHERE public_id = ?1")?
        .query_row([id], found)
        .optional()
}

/// Every listed artist's `(albums, tracks)`, by row ID.
fn all_counts(conn: &Connection) -> rusqlite::Result<HashMap<i64, (i64, i64)>> {
    let mut counts: HashMap<i64, (i64, i64)> = HashMap::new();
    for sql in [TAGGED_COUNTS, UNTAGGED_COUNTS] {
        let mut statement = conn.prepare_cached(sql)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let entry = counts.entry(row.get(0)?).or_default();
            entry.0 += row.get::<_, i64>(1)?;
            entry.1 += row.get::<_, i64>(2)?;
        }
    }
    Ok(counts)
}

/// `text` as LIKE patterns, one per word, each matching anywhere in a name.
fn name_patterns(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            let escaped = word
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        })
        .collect()
}

/// Returns a page of artists by sort name, limited to those whose name holds every word of
/// `text` if any.
pub fn artist_page(
    conn: &Connection,
    text: Option<&str>,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<ArtistPage> {
    let counts = all_counts(conn)?;
    let patterns = text.map(name_patterns).unwrap_or_default();
    let filter = (1..=patterns.len())
        .map(|number| format!("name LIKE ?{number} ESCAPE '\\'"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let filter = if filter.is_empty() {
        filter
    } else {
        format!("WHERE {filter}")
    };
    let bound: Vec<&dyn ToSql> = patterns
        .iter()
        .map(|pattern| pattern as &dyn ToSql)
        .collect();

    let wanted = offset as usize..offset as usize + limit.min(MAX_PAGE) as usize;
    let mut artists = Vec::new();
    let mut total = 0;
    let mut statement = conn.prepare_cached(&format!(
        "SELECT id, public_id, name FROM artists INDEXED BY idx_artists_sort {filter}
         ORDER BY sort_name, id"
    ))?;
    let mut rows = statement.query(bound.as_slice())?;
    while let Some(row) = rows.next()? {
        let key: i64 = row.get(0)?;
        let Some(&(album_count, track_count)) = counts.get(&key) else {
            continue;
        };
        if wanted.contains(&total) {
            artists.push(ArtistRow {
                id: row.get(1)?,
                name: row.get(2)?,
                album_count,
                track_count,
            });
        }
        total += 1;
    }
    Ok(ArtistPage {
        artists,
        total: total as u32,
    })
}

/// An artist and the albums holding their tracks, or None if there is no such artist.
pub fn artist_albums(conn: &Connection, id: &str) -> rusqlite::Result<Option<ArtistAlbums>> {
    let Some((key, id, name)) = find(conn, id)? else {
        return Ok(None);
    };
    let (track_count, album_count) = conn
        .prepare_cached(&format!(
            "SELECT count(*), count(DISTINCT album_id) FROM ({ARTIST_TRACKS})"
        ))?
        .query_row([key], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let albums = conn
        .prepare_cached(&format!(
            "SELECT {ALBUM_COLUMNS} FROM albums a
             WHERE a.id IN (SELECT album_id FROM ({ARTIST_TRACKS}))
             ORDER BY a.year IS NULL, a.year, a.sort_title, a.id"
        ))?
        .query_map(params![key], |row| AlbumRow::from_row(row, 0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|album| album.check_pending(conn))
        .collect::<Result<_, _>>()?;
    Ok(Some(ArtistAlbums {
        artist: ArtistRow {
            id,
            name,
            album_count,
            track_count,
        },
        albums,
    }))
}

/// The tracks belonging to an artist, for limiting a track page to them. Empty if there is
/// no such artist.
pub fn artist_track_ids(conn: &Connection, id: &str) -> rusqlite::Result<Rc<Vec<Value>>> {
    let Some((key, ..)) = find(conn, id)? else {
        return Ok(Rc::new(Vec::new()));
    };
    let ids = conn
        .prepare_cached(&format!("SELECT id FROM ({ARTIST_TRACKS})"))?
        .query_map([key], |row| row.get(0).map(Value::Integer))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Rc::new(ids))
}
