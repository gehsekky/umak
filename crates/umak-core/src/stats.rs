//! Library statistics, including how albums were grouped and which tags need fixing.

use rusqlite::Connection;
use serde::Serialize;

use crate::db::{Database, DbError};
use crate::scan::normalize::{UNKNOWN_ALBUM, UNKNOWN_ARTIST, album_folder};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootStats {
    pub path: String,
    pub tracks: i64,
    pub missing: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecStats {
    pub codec: String,
    pub tracks: i64,
    pub bytes: i64,
    pub min_sample_rate: Option<i64>,
    pub max_sample_rate: Option<i64>,
    pub max_bit_depth: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupingStats {
    pub rule: i64,
    pub albums: i64,
    pub tracks: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumNote {
    pub title: String,
    pub artist: String,
    pub folder: String,
    pub tracks: i64,
    pub track_artists: i64,
}

/// How many tracks lack each tag. A track with no title tag is shown by its file name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagGaps {
    pub no_title: i64,
    pub no_artist: i64,
    pub no_album: i64,
    pub no_track_number: i64,
    pub no_year: i64,
    pub no_replay_gain: i64,
    pub no_musicbrainz_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameCount {
    pub name: String,
    pub tracks: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStats {
    pub tracks: i64,
    pub missing_tracks: i64,
    pub albums: i64,
    pub artists: i64,
    pub genres: i64,
    pub total_bytes: i64,
    pub total_duration_ms: i64,
    pub roots: Vec<RootStats>,
    pub codecs: Vec<CodecStats>,
    /// Albums by the grouping rule that formed them, in rule order.
    pub grouping: Vec<GroupingStats>,
    pub tagged_compilations: i64,
    /// Albums with no album artist tag, whose artist was judged from their tracks.
    pub albums_without_album_artist: Vec<AlbumNote>,
    /// The judged albums with no leading artist, placed under Various Artists.
    pub inferred_compilations: Vec<AlbumNote>,
    pub tag_gaps: TagGaps,
    /// Artist values holding a separator. Each is one artist, as tagged. Some are real
    /// names like AC/DC; the rest need several values in the file.
    pub names_with_separators: Vec<NameCount>,
}

pub fn library_stats(db: &Database) -> Result<LibraryStats, DbError> {
    db.read(|conn| Ok(collect(conn)?))
}

fn collect(conn: &Connection) -> rusqlite::Result<LibraryStats> {
    let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
    Ok(LibraryStats {
        tracks: count("SELECT count(*) FROM tracks")?,
        missing_tracks: count("SELECT count(*) FROM tracks WHERE missing_since IS NOT NULL")?,
        albums: count("SELECT count(*) FROM albums")?,
        // The reserved Various Artists row exists in every library.
        artists: count(
            "SELECT count(*) FROM artists a
             WHERE EXISTS (SELECT 1 FROM track_artists WHERE artist_id = a.id)
                OR EXISTS (SELECT 1 FROM album_artists WHERE artist_id = a.id)",
        )?,
        genres: count("SELECT count(*) FROM genres")?,
        total_bytes: count("SELECT coalesce(sum(size), 0) FROM tracks")?,
        total_duration_ms: count("SELECT coalesce(sum(duration_ms), 0) FROM tracks")?,
        roots: conn
            .prepare(
                "SELECT r.path, count(t.id), coalesce(sum(t.missing_since IS NOT NULL), 0)
                 FROM library_roots r LEFT JOIN tracks t ON t.root_id = r.id
                 GROUP BY r.id ORDER BY r.path",
            )?
            .query_map([], |row| {
                Ok(RootStats {
                    path: row.get(0)?,
                    tracks: row.get(1)?,
                    missing: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?,
        codecs: conn
            .prepare(
                "SELECT coalesce(codec, 'unknown'), count(*), sum(size), min(sample_rate),
                        max(sample_rate), max(bit_depth)
                 FROM tracks GROUP BY 1 ORDER BY 2 DESC, 1",
            )?
            .query_map([], |row| {
                Ok(CodecStats {
                    codec: row.get(0)?,
                    tracks: row.get(1)?,
                    bytes: row.get(2)?,
                    min_sample_rate: row.get(3)?,
                    max_sample_rate: row.get(4)?,
                    max_bit_depth: row.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?,
        grouping: conn
            .prepare(
                "SELECT a.grouping_rule, count(DISTINCT a.id), count(t.id)
                 FROM albums a LEFT JOIN tracks t ON t.album_id = a.id
                 GROUP BY a.grouping_rule ORDER BY a.grouping_rule",
            )?
            .query_map([], |row| {
                Ok(GroupingStats {
                    rule: row.get(0)?,
                    albums: row.get(1)?,
                    tracks: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?,
        tagged_compilations: count("SELECT count(*) FROM albums WHERE compilation = 'tagged'")?,
        albums_without_album_artist: album_notes(conn, "a.artist_source = 'judged'")?,
        inferred_compilations: album_notes(conn, "a.compilation = 'inferred'")?,
        tag_gaps: tag_gaps(conn)?,
        names_with_separators: conn
            .prepare(
                "SELECT a.name, count(DISTINCT ta.track_id) AS tracks
                 FROM artists a JOIN track_artists ta ON ta.artist_id = a.id
                 WHERE a.name LIKE '%/%' OR a.name LIKE '%;%'
                 GROUP BY a.id ORDER BY tracks DESC, a.sort_name",
            )?
            .query_map([], |row| {
                Ok(NameCount {
                    name: row.get(0)?,
                    tracks: row.get(1)?,
                })
            })?
            .collect::<Result<_, _>>()?,
    })
}

fn album_notes(conn: &Connection, condition: &str) -> rusqlite::Result<Vec<AlbumNote>> {
    let sql = format!(
        "SELECT a.title, a.album_artist_display, min(t.rel_path), count(DISTINCT t.id),
                count(DISTINCT ta.artist_id)
         FROM albums a
         JOIN tracks t ON t.album_id = a.id
         LEFT JOIN track_artists ta ON ta.track_id = t.id AND ta.role = 'artist'
         WHERE {condition}
         GROUP BY a.id ORDER BY a.album_artist_sort, a.sort_title, a.id"
    );
    conn.prepare(&sql)?
        .query_map([], |row| {
            let first_path: String = row.get(2)?;
            Ok(AlbumNote {
                title: row.get(0)?,
                artist: row.get(1)?,
                folder: album_folder(&first_path).to_owned(),
                tracks: row.get(3)?,
                track_artists: row.get(4)?,
            })
        })?
        .collect()
}

fn tag_gaps(conn: &Connection) -> rusqlite::Result<TagGaps> {
    let mut gaps = conn.query_row(
        "SELECT coalesce(sum(artist_display = ?1), 0), coalesce(sum(album_display = ?2), 0),
                coalesce(sum(track_no = 0), 0), coalesce(sum(year IS NULL), 0),
                coalesce(sum(rg_track_gain IS NULL), 0),
                coalesce(sum(musicbrainz_recording_id IS NULL), 0)
         FROM tracks",
        [UNKNOWN_ARTIST, UNKNOWN_ALBUM],
        |row| {
            Ok(TagGaps {
                no_title: 0,
                no_artist: row.get(0)?,
                no_album: row.get(1)?,
                no_track_number: row.get(2)?,
                no_year: row.get(3)?,
                no_replay_gain: row.get(4)?,
                no_musicbrainz_id: row.get(5)?,
            })
        },
    )?;
    let mut titles = conn.prepare("SELECT title, rel_path FROM tracks")?;
    let mut rows = titles.query([])?;
    while let Some(row) = rows.next()? {
        let title = row.get_ref(0)?.as_str()?;
        let path = row.get_ref(1)?.as_str()?;
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.rsplit_once('.').map_or(name, |(stem, _)| stem) == title {
            gaps.no_title += 1;
        }
    }
    Ok(gaps)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::db::new_public_id;

    struct Fixture {
        _dir: tempfile::TempDir,
        db: Database,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        Fixture { _dir: dir, db }
    }

    struct Album<'a> {
        key: &'a str,
        rule: i64,
        source: &'a str,
        compilation: &'a str,
        artist: &'a str,
    }

    impl Fixture {
        fn write(&self, sql: &str) {
            self.db
                .write(|conn| {
                    conn.execute_batch(sql)?;
                    Ok::<_, DbError>(())
                })
                .unwrap();
        }

        fn album(&self, album: Album<'_>) -> i64 {
            self.db
                .write(|conn| {
                    conn.execute(
                        "INSERT INTO albums (public_id, group_key, title, sort_title,
                             album_artist_display, album_artist_sort, compilation,
                             grouping_rule, artist_source)
                         VALUES (?1, ?2, ?2, ?2, ?3, ?3, ?4, ?5, ?6)",
                        params![
                            new_public_id(),
                            album.key,
                            album.artist,
                            album.compilation,
                            album.rule,
                            album.source
                        ],
                    )?;
                    Ok::<_, DbError>(conn.last_insert_rowid())
                })
                .unwrap()
        }

        fn track(&self, album: i64, path: &str, title: &str, artist: Option<&str>) {
            self.db
                .write(|conn| {
                    conn.execute(
                        "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title,
                             album_id, duration_ms, codec, artist_display, album_display,
                             title_sort, artist_sort, album_sort, album_artist_sort,
                             added_at, updated_at)
                         VALUES (?1, 1, ?2, 100, 1, ?3, ?4, 1000, 'flac', ?5, 'A', 't', 'a', 'a',
                                 'a', 0, 0)",
                        params![
                            new_public_id(),
                            path,
                            title,
                            album,
                            artist.unwrap_or(UNKNOWN_ARTIST)
                        ],
                    )?;
                    let track = conn.last_insert_rowid();
                    if let Some(name) = artist {
                        conn.execute(
                            "INSERT OR IGNORE INTO artists (public_id, name, sort_name, norm_key)
                             VALUES (?1, ?2, ?2, ?2)",
                            params![new_public_id(), name],
                        )?;
                        conn.execute(
                            "INSERT INTO track_artists (track_id, artist_id, role, position)
                             SELECT ?1, id, 'artist', 0 FROM artists WHERE name = ?2",
                            params![track, name],
                        )?;
                    }
                    Ok::<_, DbError>(())
                })
                .unwrap();
        }
    }

    #[test]
    fn an_empty_library_reports_zeros() {
        let stats = library_stats(&fixture().db).unwrap();
        assert_eq!(stats.tracks, 0);
        assert_eq!(stats.artists, 0, "the reserved row is not counted");
        assert_eq!(stats.total_bytes, 0);
        assert!(stats.codecs.is_empty());
        assert_eq!(stats.tag_gaps, TagGaps::default());
    }

    #[test]
    fn albums_are_counted_by_rule_and_judged_albums_are_listed_with_their_folder() {
        let fixture = fixture();
        fixture.write(
            "INSERT INTO library_roots (id, public_id, name, path, added_at)
             VALUES (1, 'r', 'Music', 'M:/', 0)",
        );
        let tagged = fixture.album(Album {
            key: "tagged",
            rule: 2,
            source: "tag",
            compilation: "none",
            artist: "Band",
        });
        let leader = fixture.album(Album {
            key: "leader",
            rule: 4,
            source: "judged",
            compilation: "none",
            artist: "Main",
        });
        let mix = fixture.album(Album {
            key: "mix",
            rule: 4,
            source: "judged",
            compilation: "inferred",
            artist: "Various Artists",
        });
        let flagged = fixture.album(Album {
            key: "flagged",
            rule: 3,
            source: "flag",
            compilation: "tagged",
            artist: "Various Artists",
        });

        fixture.track(tagged, "t/1.flac", "One", Some("Band"));
        fixture.track(tagged, "t/2.flac", "Two", Some("Band"));
        fixture.track(leader, "l/CD1/1.flac", "One", Some("Main"));
        fixture.track(mix, "m/1.flac", "One", Some("A"));
        fixture.track(mix, "m/2.flac", "Two", Some("B"));
        fixture.track(flagged, "f/1.flac", "One", Some("C"));

        let stats = library_stats(&fixture.db).unwrap();
        let rules: Vec<_> = stats
            .grouping
            .iter()
            .map(|group| (group.rule, group.albums, group.tracks))
            .collect();
        assert_eq!(rules, [(2, 1, 2), (3, 1, 1), (4, 2, 3)]);
        assert_eq!(stats.tagged_compilations, 1);

        assert_eq!(stats.albums_without_album_artist.len(), 2);
        assert_eq!(stats.albums_without_album_artist[0].title, "leader");
        assert_eq!(stats.albums_without_album_artist[0].folder, "l");
        assert_eq!(
            stats.inferred_compilations,
            [AlbumNote {
                title: "mix".to_owned(),
                artist: "Various Artists".to_owned(),
                folder: "m".to_owned(),
                tracks: 2,
                track_artists: 2,
            }]
        );
    }

    #[test]
    fn totals_gaps_and_separator_names_are_reported() {
        let fixture = fixture();
        fixture.write(
            "INSERT INTO library_roots (id, public_id, name, path, added_at)
             VALUES (1, 'r', 'Music', 'M:/', 0)",
        );
        let album = fixture.album(Album {
            key: UNKNOWN_ALBUM,
            rule: 4,
            source: "judged",
            compilation: "none",
            artist: "X",
        });
        fixture.track(album, "a/from name.flac", "from name", None);
        fixture.track(album, "a/2.flac", "Real Title", Some("Dr. Dre/Eminem"));
        fixture.track(album, "a/3.flac", "Other Title", Some("Dr. Dre/Eminem"));
        fixture.track(album, "a/4.flac", "Fourth", Some("One; Two"));
        fixture.track(album, "a/5.flac", "Fifth", Some("Plain"));
        fixture.write("UPDATE tracks SET missing_since = 1 WHERE rel_path = 'a/2.flac'");

        let stats = library_stats(&fixture.db).unwrap();
        assert_eq!(stats.tracks, 5);
        assert_eq!(stats.missing_tracks, 1);
        assert_eq!(stats.total_bytes, 500);
        assert_eq!(stats.total_duration_ms, 5000);
        assert_eq!(stats.roots[0].tracks, 5);
        assert_eq!(stats.roots[0].missing, 1);
        assert_eq!(stats.codecs[0].codec, "flac");
        assert_eq!(stats.tag_gaps.no_title, 1);
        assert_eq!(stats.tag_gaps.no_artist, 1);
        assert_eq!(stats.tag_gaps.no_track_number, 5);
        assert_eq!(
            stats.names_with_separators,
            [
                NameCount {
                    name: "Dr. Dre/Eminem".to_owned(),
                    tracks: 2
                },
                NameCount {
                    name: "One; Two".to_owned(),
                    tracks: 1
                },
            ]
        );
    }
}
