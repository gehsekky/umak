//! Finding files that look like copies of each other. This only reports them: which copy to
//! keep is the person's call, and nothing here changes a file or the library.
//!
//! Files that are missing are left out, because there is nothing left to compare or clean up.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::library::{MAX_PAGE, TRACK_COLUMNS, TrackRow};
use crate::scan::normalize::{UNKNOWN_ALBUM, UNKNOWN_ARTIST};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DuplicateMatch {
    /// The same audio fingerprint, whatever the tags say.
    Audio,
    /// The same title, artist, album and track number, exactly as they are in the library.
    ///
    /// The track number matters: the same song played twice in a concert has the same title,
    /// artist and album, and in a real library such recordings far outnumber actual copies.
    /// Tracks with no artist or album have nothing to go on, so they are left out.
    Tags,
}

impl DuplicateMatch {
    /// Which rows of `tracks`, named `t`, to group, and the columns to group them by.
    fn query(self) -> (String, &'static [&'static str]) {
        match self {
            Self::Audio => (
                "t.fingerprint IS NOT NULL AND t.missing_since IS NULL".to_owned(),
                &["fingerprint"],
            ),
            Self::Tags => (
                format!(
                    "t.missing_since IS NULL
                     AND t.artist_display <> '{UNKNOWN_ARTIST}'
                     AND t.album_display <> '{UNKNOWN_ALBUM}'"
                ),
                &["title", "artist_display", "album_display", "track_no"],
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateTrack {
    pub track: TrackRow,
    pub root_id: String,
    pub rel_path: String,
    pub size: i64,
    pub bitrate: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct DuplicateGroup {
    pub tracks: Vec<DuplicateTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DuplicatePage {
    pub groups: Vec<DuplicateGroup>,
    /// How many groups there are in all.
    pub total_groups: u32,
    /// How many tracks those groups hold.
    pub total_tracks: u32,
}

/// Returns a page of groups, ordered by artist, album and title. A limit of zero gives only
/// the totals.
pub fn duplicate_page(
    conn: &Connection,
    by: DuplicateMatch,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<DuplicatePage> {
    let (filter, keys) = by.query();
    let key_list = keys.join(", ");
    // Grouped from an index alone. Only the tracks in a group are then read, to put it in order.
    let found = format!(
        "WITH found AS (
             SELECT {key_list}, count(*) AS copies
             FROM tracks AS t WHERE {filter} GROUP BY {key_list} HAVING copies > 1
         )"
    );

    let (total_groups, total_tracks) = conn
        .prepare_cached(&format!(
            "{found} SELECT count(*), coalesce(sum(copies), 0) FROM found"
        ))?
        .query_row([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if limit == 0 {
        return Ok(DuplicatePage {
            groups: Vec::new(),
            total_groups,
            total_tracks,
        });
    }

    let qualified = |table: &str| {
        keys.iter()
            .map(|key| format!("{table}.{key}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let same_as = |table: &str| {
        keys.iter()
            .map(|key| format!("t.{key} = {table}.{key}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let found_keys = qualified("found");
    let in_found = same_as("found");
    let in_shown = same_as("shown");
    let sql = format!(
        "{found}, shown AS (
             SELECT {found_keys}, row_number() OVER (
                 ORDER BY min(t.artist_sort), min(t.album_sort), min(t.title_sort), {found_keys}
             ) AS place
             FROM found CROSS JOIN tracks AS t ON {in_found}
             WHERE {filter}
             GROUP BY {found_keys} ORDER BY place LIMIT ?1 OFFSET ?2
         )
         SELECT shown.place, r.public_id, t.rel_path, t.size, t.bitrate, {TRACK_COLUMNS}
         FROM shown
         CROSS JOIN tracks AS t ON {in_shown}
         JOIN library_roots AS r ON r.id = t.root_id
         WHERE {filter}
         ORDER BY shown.place, r.id, t.rel_path"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query(params![limit.min(MAX_PAGE), offset])?;
    let mut page: Vec<DuplicateGroup> = Vec::new();
    let mut current_place = None;
    while let Some(row) = rows.next()? {
        let place: i64 = row.get(0)?;
        if current_place != Some(place) {
            current_place = Some(place);
            page.push(DuplicateGroup { tracks: Vec::new() });
        }
        if let Some(group) = page.last_mut() {
            group.tracks.push(DuplicateTrack {
                root_id: row.get(1)?,
                rel_path: row.get(2)?,
                size: row.get(3)?,
                bitrate: row.get(4)?,
                track: TrackRow::from_row(row, 5)?,
            });
        }
    }
    Ok(DuplicatePage {
        groups: page,
        total_groups,
        total_tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbError, new_public_id};

    #[derive(Clone, Copy)]
    struct Track<'a> {
        path: &'a str,
        fingerprint: Option<&'a [u8]>,
        title: &'a str,
        artist: &'a str,
        album: &'a str,
        number: i64,
        missing: bool,
    }

    const TRACK: Track<'static> = Track {
        path: "",
        fingerprint: None,
        title: "Song",
        artist: "Artist",
        album: "Album",
        number: 1,
        missing: false,
    };

    fn library(tracks: &[Track<'_>]) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                 VALUES (1, 'root', 'Music', 'M:/', 0)",
                [],
            )?;
            conn.execute(
                "INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                 VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x')",
                [],
            )?;
            for track in tracks {
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, fingerprint,
                         missing_since, title, album_id, track_no, bitrate, artist_display,
                         album_display, title_sort, artist_sort, album_sort, album_artist_sort,
                         added_at, updated_at)
                     VALUES (?1, 1, ?2, 100, 1, ?3, ?4, ?5, 1, ?6, 320, ?7, ?8, ?9, ?10, ?11, ?10,
                             0, 0)",
                    params![
                        new_public_id(),
                        track.path,
                        track.fingerprint,
                        track.missing.then_some(1),
                        track.title,
                        track.number,
                        track.artist,
                        track.album,
                        track.title.to_lowercase(),
                        track.artist.to_lowercase(),
                        track.album.to_lowercase(),
                    ],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        (dir, db)
    }

    fn find(db: &Database, by: DuplicateMatch, offset: u32, limit: u32) -> DuplicatePage {
        db.read(|conn| Ok::<_, DbError>(duplicate_page(conn, by, offset, limit)?))
            .unwrap()
    }

    fn paths(page: &DuplicatePage) -> Vec<Vec<&str>> {
        page.groups
            .iter()
            .map(|group| group.tracks.iter().map(|t| t.rel_path.as_str()).collect())
            .collect()
    }

    #[test]
    fn the_same_audio_groups_files_whatever_their_tags_say() {
        let (_dir, db) = library(&[
            Track {
                path: "b.mp3",
                fingerprint: Some(b"one"),
                title: "Other",
                ..TRACK
            },
            Track {
                path: "a.mp3",
                fingerprint: Some(b"one"),
                ..TRACK
            },
            Track {
                path: "c.mp3",
                fingerprint: Some(b"two"),
                ..TRACK
            },
            Track {
                path: "d.mp3",
                ..TRACK
            },
            Track {
                path: "e.mp3",
                ..TRACK
            },
        ]);
        let page = find(&db, DuplicateMatch::Audio, 0, 100);
        assert_eq!(paths(&page), [["a.mp3", "b.mp3"]]);
        assert_eq!((page.total_groups, page.total_tracks), (1, 2));
        let first = &page.groups[0].tracks[0];
        assert_eq!(first.root_id, "root");
        assert_eq!((first.size, first.bitrate), (100, Some(320)));
        assert_eq!(first.track.title, "Song");
    }

    #[test]
    fn a_missing_file_is_not_a_copy() {
        let (_dir, db) = library(&[
            Track {
                path: "a.mp3",
                fingerprint: Some(b"one"),
                ..TRACK
            },
            Track {
                path: "b.mp3",
                fingerprint: Some(b"one"),
                missing: true,
                ..TRACK
            },
            Track {
                path: "c.mp3",
                fingerprint: Some(b"two"),
                title: "T",
                ..TRACK
            },
            Track {
                path: "d.mp3",
                fingerprint: Some(b"two"),
                title: "T",
                ..TRACK
            },
            Track {
                path: "e.mp3",
                fingerprint: Some(b"two"),
                title: "T",
                missing: true,
                ..TRACK
            },
        ]);
        let page = find(&db, DuplicateMatch::Audio, 0, 100);
        assert_eq!(paths(&page), [["c.mp3", "d.mp3"]]);
        assert_eq!((page.total_groups, page.total_tracks), (1, 2));
        assert_eq!(
            paths(&find(&db, DuplicateMatch::Tags, 0, 100)),
            [["c.mp3", "d.mp3"]]
        );
    }

    #[test]
    fn matching_tags_must_be_the_same_as_written() {
        let (_dir, db) = library(&[
            Track {
                path: "a.flac",
                fingerprint: Some(b"one"),
                ..TRACK
            },
            Track {
                path: "a.mp3",
                fingerprint: Some(b"two"),
                ..TRACK
            },
            Track {
                path: "case.mp3",
                title: "SONG",
                ..TRACK
            },
            Track {
                path: "artist.mp3",
                artist: "Artist/Other",
                ..TRACK
            },
            Track {
                path: "album.mp3",
                album: "Album (Live)",
                ..TRACK
            },
        ]);
        assert_eq!(
            paths(&find(&db, DuplicateMatch::Tags, 0, 100)),
            [["a.flac", "a.mp3"]]
        );
        assert!(find(&db, DuplicateMatch::Audio, 0, 100).groups.is_empty());
    }

    #[test]
    fn a_song_played_twice_on_an_album_is_not_a_copy() {
        let (_dir, db) = library(&[
            Track {
                path: "set1.mp3",
                number: 3,
                ..TRACK
            },
            Track {
                path: "set2.mp3",
                number: 9,
                ..TRACK
            },
        ]);
        assert!(find(&db, DuplicateMatch::Tags, 0, 100).groups.is_empty());
    }

    #[test]
    fn tracks_with_nothing_to_compare_do_not_match_by_tags() {
        let (_dir, db) = library(&[
            Track {
                path: "a.mp3",
                artist: UNKNOWN_ARTIST,
                ..TRACK
            },
            Track {
                path: "b.mp3",
                artist: UNKNOWN_ARTIST,
                ..TRACK
            },
            Track {
                path: "c.mp3",
                album: UNKNOWN_ALBUM,
                ..TRACK
            },
            Track {
                path: "d.mp3",
                album: UNKNOWN_ALBUM,
                ..TRACK
            },
        ]);
        assert!(find(&db, DuplicateMatch::Tags, 0, 100).groups.is_empty());
    }

    #[test]
    fn groups_come_in_order_a_page_at_a_time_with_the_same_totals() {
        let (_dir, db) = library(&[
            Track {
                path: "c1",
                fingerprint: Some(b"c"),
                artist: "Cee",
                ..TRACK
            },
            Track {
                path: "c2",
                fingerprint: Some(b"c"),
                artist: "Cee",
                ..TRACK
            },
            Track {
                path: "c3",
                fingerprint: Some(b"c"),
                artist: "Cee",
                ..TRACK
            },
            Track {
                path: "a1",
                fingerprint: Some(b"a"),
                artist: "Ay",
                ..TRACK
            },
            Track {
                path: "a2",
                fingerprint: Some(b"a"),
                artist: "Ay",
                ..TRACK
            },
            Track {
                path: "b1",
                fingerprint: Some(b"b"),
                artist: "Bee",
                ..TRACK
            },
            Track {
                path: "b2",
                fingerprint: Some(b"b"),
                artist: "Bee",
                ..TRACK
            },
        ]);
        let first = find(&db, DuplicateMatch::Audio, 0, 2);
        assert_eq!(first.groups.len(), 2);
        assert_eq!(paths(&first)[0], ["a1", "a2"]);
        assert_eq!(paths(&first)[1], ["b1", "b2"]);
        let rest = find(&db, DuplicateMatch::Audio, 2, 2);
        assert_eq!(paths(&rest), [["c1", "c2", "c3"]]);
        for page in [&first, &rest] {
            assert_eq!((page.total_groups, page.total_tracks), (3, 7));
        }
        let totals = find(&db, DuplicateMatch::Audio, 0, 0);
        assert!(totals.groups.is_empty());
        assert_eq!((totals.total_groups, totals.total_tracks), (3, 7));
    }

    #[test]
    fn an_empty_library_has_no_duplicates() {
        let (_dir, db) = library(&[]);
        for by in [DuplicateMatch::Audio, DuplicateMatch::Tags] {
            let page = find(&db, by, 0, 100);
            assert!(page.groups.is_empty());
            assert_eq!((page.total_groups, page.total_tracks), (0, 0));
        }
    }
}
