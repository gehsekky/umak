//! Writes scanned files to the database and applies the album grouping rules.
//!
//! Each album keeps the same `group_key` whichever way its artist is decided, so an
//! album judged from its tracks keeps its ID as tracks are added. The judgment itself
//! runs in `refresh_albums`, after the batches that touched the album.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::normalize::{
    UNKNOWN_ALBUM, UNKNOWN_ARTIST, VARIOUS_ARTISTS, album_folder, artist_key, identity_key,
    is_various, sort_key, sort_key_from_tag,
};
use super::tags::{FileInfo, TrackTags, positioned};
use crate::db::{VARIOUS_ARTISTS_PUBLIC_ID, new_public_id};
use crate::settings;

const ARTIST_SEPARATOR: &str = ", ";

/// One file ready to be stored.
pub struct FileRecord<'a> {
    pub root_id: i64,
    pub rel_path: &'a str,
    pub size: i64,
    pub mtime: i64,
    /// The track already at this path, if any.
    pub existing: Option<i64>,
    pub fingerprint: Option<&'a [u8]>,
    pub info: &'a FileInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Added,
    Updated,
    Moved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AlbumChoice {
    group_key: String,
    rule: i64,
    source: &'static str,
    title: String,
    musicbrainz_id: Option<String>,
}

/// Applies the grouping rules in order. Rule 4 is decided later, from all the tracks.
fn choose_album(tags: &TrackTags, root_id: i64, rel_path: &str) -> AlbumChoice {
    let title = tags
        .album
        .clone()
        .unwrap_or_else(|| UNKNOWN_ALBUM.to_owned());
    let title_key = identity_key(&title);
    let folder = album_folder(rel_path).to_lowercase();
    let first_album_artist = tags.album_artists.first();

    let source = match (first_album_artist, tags.compilation) {
        (Some(_), _) => "tag",
        (None, true) => "flag",
        (None, false) => "judged",
    };
    let by_artist = |artist: &str| format!("a:{artist}|{title_key}|{root_id}:{folder}");

    let musicbrainz_id = tags
        .musicbrainz_album_id
        .as_deref()
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty());
    let (group_key, rule) = if let Some(id) = &musicbrainz_id {
        (format!("mb:{id}"), 1)
    } else if let Some(artist) = first_album_artist {
        (by_artist(&artist_key(artist)), 2)
    } else if tags.compilation {
        (by_artist(&artist_key(VARIOUS_ARTISTS)), 3)
    } else {
        (format!("f:{root_id}:{folder}|{title_key}"), 4)
    };
    AlbumChoice {
        group_key,
        rule,
        source,
        title,
        musicbrainz_id,
    }
}

#[derive(Clone)]
struct ArtistRow {
    id: i64,
    sort_name: String,
}

/// Lookups for one scan, on the writer connection. Rows it created in a failed
/// transaction are unusable, so a failed batch ends the scan.
#[derive(Default)]
pub struct Catalog {
    /// Folded, as `sort_key` takes them.
    articles: Vec<String>,
    /// Whether stored artist sort values are worked out again, the articles having changed.
    pub resort: bool,
    artists: HashMap<String, ArtistRow>,
    genres: HashMap<String, i64>,
    albums: HashMap<String, i64>,
    /// Albums whose stored values may no longer match their tracks.
    pub touched_albums: HashSet<i64>,
    pub changed: bool,
}

impl Catalog {
    /// A catalog sorting names by the articles in the settings.
    pub fn new(conn: &Connection) -> Self {
        Self {
            articles: settings::sort_articles(conn),
            ..Self::default()
        }
    }

    fn sort_key(&self, name: &str) -> String {
        sort_key(name, &self.articles)
    }

    fn artist(
        &mut self,
        conn: &Connection,
        name: &str,
        sort_tag: Option<&str>,
    ) -> rusqlite::Result<ArtistRow> {
        let key = artist_key(name);
        if let Some(row) = self.artists.get(&key) {
            return Ok(row.clone());
        }
        let found = conn
            .prepare_cached("SELECT id, sort_name FROM artists WHERE norm_key = ?1")?
            .query_row([&key], |row| {
                Ok(ArtistRow {
                    id: row.get(0)?,
                    sort_name: row.get(1)?,
                })
            })
            .optional()?;
        let sort_name = || sort_tag.map_or_else(|| self.sort_key(name), sort_key_from_tag);
        let row = match found {
            Some(row) if self.resort => {
                let sort_name = sort_name();
                if sort_name != row.sort_name {
                    conn.prepare_cached("UPDATE artists SET sort_name = ?2 WHERE id = ?1")?
                        .execute(params![row.id, sort_name])?;
                }
                ArtistRow {
                    id: row.id,
                    sort_name,
                }
            }
            Some(row) => row,
            None => {
                let sort_name = sort_name();
                conn.prepare_cached(
                    "INSERT INTO artists (public_id, name, sort_name, norm_key)
                     VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![new_public_id(), name, sort_name, key])?;
                ArtistRow {
                    id: conn.last_insert_rowid(),
                    sort_name,
                }
            }
        };
        self.artists.insert(key, row.clone());
        Ok(row)
    }

    fn genre(&mut self, conn: &Connection, name: &str) -> rusqlite::Result<i64> {
        let key = identity_key(name);
        if let Some(id) = self.genres.get(&key) {
            return Ok(*id);
        }
        let found = conn
            .prepare_cached("SELECT id FROM genres WHERE norm_key = ?1")?
            .query_row([&key], |row| row.get(0))
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                conn.prepare_cached("INSERT INTO genres (name, norm_key) VALUES (?1, ?2)")?
                    .execute(params![name, key])?;
                conn.last_insert_rowid()
            }
        };
        self.genres.insert(key, id);
        Ok(id)
    }

    /// Returns the track's album. The first track seen in a scan refreshes the album's
    /// stored values, so a retag takes effect; later tracks can only raise the
    /// compilation flag.
    fn album(
        &mut self,
        conn: &Connection,
        choice: &AlbumChoice,
        tags: &TrackTags,
    ) -> rusqlite::Result<i64> {
        let flagged = tags.compilation || tags.album_artists.iter().any(|name| is_various(name));
        if let Some(id) = self.albums.get(&choice.group_key) {
            if flagged && choice.source != "judged" {
                conn.prepare_cached(
                    "UPDATE albums SET compilation = 'tagged' WHERE id = ?1 AND compilation = 'none'",
                )?
                .execute([id])?;
            }
            return Ok(*id);
        }

        let sort_title = tags
            .album_sort
            .as_deref()
            .map_or_else(|| self.sort_key(&choice.title), sort_key_from_tag);
        let compilation = if flagged { "tagged" } else { "none" };

        // Each album artist needs its row before the album's display text is built.
        let mut artists = Vec::new();
        match choice.source {
            "tag" => {
                for (index, name) in tags.album_artists.iter().enumerate() {
                    let sort = (index == 0)
                        .then_some(tags.album_artist_sort.as_deref())
                        .flatten();
                    artists.push((name.as_str(), self.artist(conn, name, sort)?));
                }
            }
            "flag" => artists.push((VARIOUS_ARTISTS, self.artist(conn, VARIOUS_ARTISTS, None)?)),
            _ => {}
        }
        let display = artists
            .iter()
            .map(|(name, _)| {
                if is_various(name) {
                    VARIOUS_ARTISTS
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(ARTIST_SEPARATOR);
        let artist_sort = match (&tags.album_artist_sort, artists.first()) {
            (Some(tag), _) if choice.source == "tag" => sort_key_from_tag(tag),
            (_, Some((_, row))) => row.sort_name.clone(),
            _ => sort_key_from_tag(UNKNOWN_ARTIST),
        };

        let existing: Option<i64> = conn
            .prepare_cached("SELECT id FROM albums WHERE group_key = ?1")?
            .query_row([&choice.group_key], |row| row.get(0))
            .optional()?;
        let id = match existing {
            Some(id) if choice.source == "judged" => {
                conn.prepare_cached(
                    "UPDATE albums SET title = ?2, sort_title = ?3, grouping_rule = ?4,
                                       artist_source = 'judged'
                     WHERE id = ?1",
                )?
                .execute(params![id, choice.title, sort_title, choice.rule])?;
                id
            }
            Some(id) => {
                conn.prepare_cached(
                    "UPDATE albums SET title = ?2, sort_title = ?3, album_artist_display = ?4,
                                       album_artist_sort = ?5, compilation = ?6, grouping_rule = ?7,
                                       artist_source = ?8
                     WHERE id = ?1",
                )?
                .execute(params![
                    id,
                    choice.title,
                    sort_title,
                    display,
                    artist_sort,
                    compilation,
                    choice.rule,
                    choice.source
                ])?;
                id
            }
            None => {
                let display = if display.is_empty() {
                    UNKNOWN_ARTIST
                } else {
                    &display
                };
                conn.prepare_cached(
                    "INSERT INTO albums (public_id, group_key, title, sort_title, album_artist_display,
                                         album_artist_sort, compilation, musicbrainz_id,
                                         grouping_rule, artist_source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )?
                .execute(params![
                    new_public_id(),
                    choice.group_key,
                    choice.title,
                    sort_title,
                    display,
                    artist_sort,
                    compilation,
                    choice.musicbrainz_id,
                    choice.rule,
                    choice.source
                ])?;
                conn.last_insert_rowid()
            }
        };
        if choice.source != "judged" {
            let ids: Vec<i64> = artists.iter().map(|(_, row)| row.id).collect();
            replace_album_artists(conn, id, &ids)?;
        }
        self.albums.insert(choice.group_key.clone(), id);
        self.touched_albums.insert(id);
        Ok(id)
    }
}

fn replace_album_artists(
    conn: &Connection,
    album_id: i64,
    artist_ids: &[i64],
) -> rusqlite::Result<()> {
    conn.prepare_cached("DELETE FROM album_artists WHERE album_id = ?1")?
        .execute([album_id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO album_artists (album_id, artist_id, position) VALUES (?1, ?2, ?3)",
    )?;
    for (position, artist_id) in artist_ids.iter().enumerate() {
        insert.execute(params![album_id, artist_id, position as i64])?;
    }
    Ok(())
}

/// Finds a missing track with the same audio. Identical copies are matched by file
/// name first, so each copy keeps its own history where that can be told.
fn find_moved(
    conn: &Connection,
    fingerprint: &[u8],
    rel_path: &str,
) -> rusqlite::Result<Option<i64>> {
    let file_name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    let wanted = file_name(rel_path);
    let mut candidates = conn.prepare_cached(
        "SELECT id, rel_path FROM tracks
         WHERE fingerprint = ?1 AND missing_since IS NOT NULL ORDER BY id",
    )?;
    let mut first = None;
    let mut rows = candidates.query([fingerprint])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        if file_name(&path) == wanted {
            return Ok(Some(id));
        }
        first.get_or_insert(id);
    }
    Ok(first)
}

/// Stores one file: a new track, a change to a known track, or a known track that
/// moved. Moving or changing a track keeps its ID and everything attached to it.
pub fn apply_file(
    conn: &Connection,
    catalog: &mut Catalog,
    file: &FileRecord<'_>,
    now: i64,
) -> rusqlite::Result<Applied> {
    let tags = &file.info.tags;
    let audio = &file.info.audio;
    let moved = match (file.existing, file.fingerprint) {
        (None, Some(fingerprint)) => find_moved(conn, fingerprint, file.rel_path)?,
        _ => None,
    };
    let known = file.existing.or(moved);

    let choice = choose_album(tags, file.root_id, file.rel_path);
    let album_id = catalog.album(conn, &choice, tags)?;
    catalog.touched_albums.insert(album_id);
    catalog.changed = true;

    let (album_artist_sort, album_title, album_sort): (String, String, String) = conn
        .prepare_cached("SELECT album_artist_sort, title, sort_title FROM albums WHERE id = ?1")?
        .query_row([album_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;

    let mut artists = Vec::with_capacity(tags.artists.len());
    for (index, name) in tags.artists.iter().enumerate() {
        let sort = (index == 0)
            .then_some(tags.artist_sort.as_deref())
            .flatten();
        artists.push(catalog.artist(conn, name, sort)?);
    }
    let artist_display = if tags.artists.is_empty() {
        UNKNOWN_ARTIST.to_owned()
    } else {
        tags.artists.join(ARTIST_SEPARATOR)
    };
    let artist_sort = match (&tags.artist_sort, artists.first()) {
        (Some(tag), _) => sort_key_from_tag(tag),
        (None, Some(row)) => row.sort_name.clone(),
        (None, None) => sort_key_from_tag(UNKNOWN_ARTIST),
    };
    let title = tags.title.clone().unwrap_or_else(|| {
        let name = file.rel_path.rsplit('/').next().unwrap_or(file.rel_path);
        name.rsplit_once('.')
            .map_or(name, |(stem, _)| stem)
            .to_owned()
    });
    let title_sort = tags
        .title_sort
        .as_deref()
        .map_or_else(|| catalog.sort_key(&title), sort_key_from_tag);

    let values = params![
        file.root_id,
        file.rel_path,
        file.size,
        file.mtime,
        file.fingerprint,
        audio.duration_ms,
        audio.codec,
        audio.sample_rate,
        audio.bit_depth,
        audio.channels,
        audio.bitrate,
        title,
        album_id,
        tags.disc_no.unwrap_or(0),
        tags.track_no.unwrap_or(0),
        tags.year,
        tags.original_date,
        tags.rg_track_gain,
        tags.rg_track_peak,
        tags.rg_album_gain,
        tags.rg_album_peak,
        tags.musicbrainz_recording_id,
        artist_display,
        album_title,
        title_sort,
        artist_sort,
        album_sort,
        album_artist_sort,
        now,
    ];

    let track_id = match known {
        Some(id) => {
            let old_album: i64 = conn
                .prepare_cached("SELECT album_id FROM tracks WHERE id = ?1")?
                .query_row([id], |row| row.get(0))?;
            catalog.touched_albums.insert(old_album);
            let mut update = conn.prepare_cached(
                "UPDATE tracks SET
                    root_id = ?1, rel_path = ?2, size = ?3, mtime = ?4, fingerprint = ?5,
                    missing_since = NULL, duration_ms = ?6, codec = ?7, sample_rate = ?8,
                    bit_depth = ?9, channels = ?10, bitrate = ?11, title = ?12, album_id = ?13,
                    disc_no = ?14, track_no = ?15, year = ?16, original_date = ?17,
                    rg_track_gain = ?18, rg_track_peak = ?19, rg_album_gain = ?20,
                    rg_album_peak = ?21, musicbrainz_recording_id = ?22, artist_display = ?23,
                    album_display = ?24, title_sort = ?25, artist_sort = ?26, album_sort = ?27,
                    album_artist_sort = ?28, updated_at = ?29
                 WHERE id = ?30",
            )?;
            let mut with_id: Vec<&dyn rusqlite::ToSql> = values.to_vec();
            with_id.push(&id);
            update.execute(with_id.as_slice())?;
            conn.prepare_cached("DELETE FROM track_artists WHERE track_id = ?1")?
                .execute([id])?;
            conn.prepare_cached("DELETE FROM track_genres WHERE track_id = ?1")?
                .execute([id])?;
            conn.prepare_cached("DELETE FROM raw_tags WHERE track_id = ?1")?
                .execute([id])?;
            conn.prepare_cached("DELETE FROM tracks_fts WHERE rowid = ?1")?
                .execute([id])?;
            id
        }
        None => {
            let mut insert = conn.prepare_cached(
                "INSERT INTO tracks (
                    root_id, rel_path, size, mtime, fingerprint, duration_ms, codec, sample_rate,
                    bit_depth, channels, bitrate, title, album_id, disc_no, track_no, year,
                    original_date, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak,
                    musicbrainz_recording_id, artist_display, album_display, title_sort,
                    artist_sort, album_sort, album_artist_sort, updated_at, public_id, added_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                           ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?29)",
            )?;
            let public_id = new_public_id();
            let mut with_id: Vec<&dyn rusqlite::ToSql> = values.to_vec();
            with_id.push(&public_id);
            insert.execute(with_id.as_slice())?;
            conn.last_insert_rowid()
        }
    };

    let mut link_artist = conn.prepare_cached(
        "INSERT INTO track_artists (track_id, artist_id, role, position) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (position, row) in artists.iter().enumerate() {
        link_artist.execute(params![track_id, row.id, "artist", position as i64])?;
    }
    for (position, name) in tags.composers.iter().enumerate() {
        let row = catalog.artist(conn, name, None)?;
        link_artist.execute(params![track_id, row.id, "composer", position as i64])?;
    }

    let mut link_genre = conn.prepare_cached(
        "INSERT INTO track_genres (track_id, genre_id, position) VALUES (?1, ?2, ?3)",
    )?;
    for (position, name) in tags.genres.iter().enumerate() {
        let genre = catalog.genre(conn, name)?;
        link_genre.execute(params![track_id, genre, position as i64])?;
    }

    let mut raw = conn.prepare_cached(
        "INSERT INTO raw_tags (track_id, key, position, value) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (key, position, value) in positioned(&tags.raw) {
        raw.execute(params![track_id, key, position, value])?;
    }

    conn.prepare_cached(
        "INSERT INTO tracks_fts (rowid, title, artists, album, genres) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        track_id,
        title,
        tags.artists.join(" "),
        tags.album.as_deref().unwrap_or(""),
        tags.genres.join(" "),
    ])?;

    Ok(match (file.existing, moved) {
        (Some(_), _) => Applied::Updated,
        (None, Some(_)) => Applied::Moved,
        (None, None) => Applied::Added,
    })
}

/// Recomputes what depends on all of an album's tracks: rule 4's album artist, the
/// year, and the copies of album values stored on each track.
pub fn refresh_albums(conn: &Connection, albums: &HashSet<i64>) -> rusqlite::Result<()> {
    for &album in albums {
        let total: i64 = conn
            .prepare_cached("SELECT count(*) FROM tracks WHERE album_id = ?1")?
            .query_row([album], |row| row.get(0))?;
        if total == 0 {
            continue;
        }
        let source: String = conn
            .prepare_cached("SELECT artist_source FROM albums WHERE id = ?1")?
            .query_row([album], |row| row.get(0))?;
        if source == "judged" {
            judge_album_artist(conn, album, total)?;
        }
        conn.prepare_cached(
            "UPDATE albums SET year = (
                SELECT year FROM tracks WHERE album_id = ?1 AND year IS NOT NULL
                GROUP BY year ORDER BY count(*) DESC, year LIMIT 1
             ) WHERE id = ?1",
        )?
        .execute([album])?;
        conn.prepare_cached(
            "UPDATE tracks SET album_artist_sort = a.album_artist_sort,
                               album_sort = a.sort_title,
                               album_display = a.title
             FROM albums a
             WHERE tracks.album_id = ?1 AND a.id = ?1
               AND (tracks.album_artist_sort <> a.album_artist_sort
                    OR tracks.album_sort <> a.sort_title
                    OR tracks.album_display <> a.title)",
        )?
        .execute([album])?;
    }
    Ok(())
}

/// Rule 4: an artist on at least half the tracks is the album artist. Otherwise the
/// album is an inferred compilation under Various Artists.
fn judge_album_artist(conn: &Connection, album: i64, total: i64) -> rusqlite::Result<()> {
    let leader: Option<(i64, String, String, i64)> = conn
        .prepare_cached(
            "SELECT ar.id, ar.name, ar.sort_name, count(DISTINCT t.id) AS tracks
             FROM tracks t
             JOIN track_artists ta ON ta.track_id = t.id AND ta.role = 'artist'
             JOIN artists ar ON ar.id = ta.artist_id
             WHERE t.album_id = ?1
             GROUP BY ar.id ORDER BY tracks DESC, ar.sort_name LIMIT 1",
        )?
        .query_row([album], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;

    let (artist, display, sort, compilation) = match leader {
        None => (
            None,
            UNKNOWN_ARTIST.to_owned(),
            sort_key_from_tag(UNKNOWN_ARTIST),
            "none",
        ),
        Some((id, name, sort, count)) if count * 2 >= total => {
            let various = is_various(&name);
            (
                Some(id),
                name,
                sort,
                if various { "inferred" } else { "none" },
            )
        }
        Some(_) => {
            let (id, sort): (i64, String) = conn
                .prepare_cached("SELECT id, sort_name FROM artists WHERE public_id = ?1")?
                .query_row([VARIOUS_ARTISTS_PUBLIC_ID], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
            (Some(id), VARIOUS_ARTISTS.to_owned(), sort, "inferred")
        }
    };
    conn.prepare_cached(
        "UPDATE albums SET album_artist_display = ?2, album_artist_sort = ?3, compilation = ?4
         WHERE id = ?1",
    )?
    .execute(params![album, display, sort, compilation])?;
    let ids: Vec<i64> = artist.into_iter().collect();
    replace_album_artists(conn, album, &ids)
}

/// Removes catalog rows no track refers to any more.
pub fn remove_orphans(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM albums WHERE NOT EXISTS (SELECT 1 FROM tracks WHERE album_id = albums.id)",
        [],
    )?;
    conn.execute(
        "DELETE FROM artists
         WHERE public_id <> ?1
           AND NOT EXISTS (SELECT 1 FROM track_artists WHERE artist_id = artists.id)
           AND NOT EXISTS (SELECT 1 FROM album_artists WHERE artist_id = artists.id)",
        [VARIOUS_ARTISTS_PUBLIC_ID],
    )?;
    conn.execute(
        "DELETE FROM genres
         WHERE NOT EXISTS (SELECT 1 FROM track_genres WHERE genre_id = genres.id)",
        [],
    )?;
    Ok(())
}

pub fn mark_missing(conn: &Connection, tracks: &[i64], now: i64) -> rusqlite::Result<()> {
    let mut update = conn.prepare_cached("UPDATE tracks SET missing_since = ?2 WHERE id = ?1")?;
    for track in tracks {
        update.execute(params![track, now])?;
    }
    Ok(())
}

pub fn mark_present(conn: &Connection, tracks: &[i64]) -> rusqlite::Result<()> {
    let mut update = conn.prepare_cached("UPDATE tracks SET missing_since = NULL WHERE id = ?1")?;
    for track in tracks {
        update.execute([track])?;
    }
    Ok(())
}

/// How many of these tracks are still missing.
pub fn count_missing(conn: &Connection, tracks: &[i64]) -> rusqlite::Result<u64> {
    let mut select =
        conn.prepare_cached("SELECT missing_since IS NOT NULL FROM tracks WHERE id = ?1")?;
    let mut count = 0;
    for track in tracks {
        if select.query_row([track], |row| row.get::<_, bool>(0))? {
            count += 1;
        }
    }
    Ok(count)
}
