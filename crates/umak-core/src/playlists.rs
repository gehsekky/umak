//! Saved playlists. Every function here acts on the default user's manual playlists,
//! so the reserved player playlist cannot be renamed, deleted or edited by accident.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use ts_rs::TS;

use crate::api::{ApiError, ErrorCode};
use crate::db::{DEFAULT_USER_ID, new_public_id};
use crate::library::{MAX_PAGE, TRACK_COLUMNS, TrackRow};

/// Item positions leave gaps, so playlists can later insert without renumbering.
const POSITION_GAP: i64 = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub track_count: u32,
    pub duration_ms: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One entry in a playlist. A track can appear more than once, so an entry is
/// identified by its position, which a reorder may change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct PlaylistItem {
    pub position: i64,
    pub track: TrackRow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct PlaylistPage {
    pub items: Vec<PlaylistItem>,
    pub total: u32,
}

fn not_found() -> ApiError {
    ApiError::new(ErrorCode::NotFound, "that playlist no longer exists")
}

fn clean_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgs,
            "a playlist needs a name",
        ));
    }
    Ok(name)
}

fn internal_id(conn: &Connection, public_id: &str) -> Result<i64, ApiError> {
    conn.prepare_cached(
        "SELECT id FROM playlists WHERE public_id = ?1 AND user_id = ?2 AND kind = 'manual'",
    )?
    .query_row(params![public_id, DEFAULT_USER_ID], |row| row.get(0))
    .optional()?
    .ok_or_else(not_found)
}

fn touch(conn: &Connection, playlist: i64, now: i64) -> rusqlite::Result<()> {
    conn.prepare_cached("UPDATE playlists SET updated_at = ?2 WHERE id = ?1")?
        .execute(params![playlist, now])?;
    Ok(())
}

const SELECT_PLAYLIST: &str = "SELECT p.public_id, p.name,
        (SELECT count(*) FROM playlist_items i WHERE i.playlist_id = p.id),
        (SELECT coalesce(sum(t.duration_ms), 0) FROM playlist_items i
         JOIN tracks t ON t.id = i.track_id WHERE i.playlist_id = p.id),
        p.created_at, p.updated_at
    FROM playlists p";

fn playlist_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: row.get(0)?,
        name: row.get(1)?,
        track_count: row.get(2)?,
        duration_ms: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Playlist>, ApiError> {
    let sql = format!(
        "{SELECT_PLAYLIST} WHERE p.user_id = ?1 AND p.kind = 'manual'
         ORDER BY p.name COLLATE NOCASE, p.id"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_map([DEFAULT_USER_ID], playlist_from_row)?
        .collect::<Result<_, _>>()?)
}

fn get(conn: &Connection, id: i64) -> Result<Playlist, ApiError> {
    let sql = format!("{SELECT_PLAYLIST} WHERE p.id = ?1");
    Ok(conn
        .prepare_cached(&sql)?
        .query_row([id], playlist_from_row)?)
}

pub fn create(conn: &Connection, name: &str, now: i64) -> Result<Playlist, ApiError> {
    let name = clean_name(name)?;
    conn.prepare_cached(
        "INSERT INTO playlists (public_id, user_id, name, kind, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'manual', ?4, ?4)",
    )?
    .execute(params![new_public_id(), DEFAULT_USER_ID, name, now])?;
    get(conn, conn.last_insert_rowid())
}

pub fn rename(conn: &Connection, id: &str, name: &str, now: i64) -> Result<Playlist, ApiError> {
    let name = clean_name(name)?;
    let playlist = internal_id(conn, id)?;
    conn.prepare_cached("UPDATE playlists SET name = ?2, updated_at = ?3 WHERE id = ?1")?
        .execute(params![playlist, name, now])?;
    get(conn, playlist)
}

pub fn delete(conn: &Connection, id: &str) -> Result<(), ApiError> {
    let playlist = internal_id(conn, id)?;
    conn.prepare_cached("DELETE FROM playlists WHERE id = ?1")?
        .execute([playlist])?;
    Ok(())
}

pub fn items(
    conn: &Connection,
    id: &str,
    offset: u32,
    limit: u32,
) -> Result<PlaylistPage, ApiError> {
    let playlist = internal_id(conn, id)?;
    let sql = format!(
        "SELECT i.position, {TRACK_COLUMNS}
         FROM playlist_items i JOIN tracks AS t ON t.id = i.track_id
         WHERE i.playlist_id = ?1 ORDER BY i.position LIMIT ?2 OFFSET ?3"
    );
    let items = conn
        .prepare_cached(&sql)?
        .query_map(params![playlist, limit.min(MAX_PAGE), offset], |row| {
            Ok(PlaylistItem {
                position: row.get(0)?,
                track: TrackRow::from_row(row, 1)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let total = conn
        .prepare_cached("SELECT count(*) FROM playlist_items WHERE playlist_id = ?1")?
        .query_row([playlist], |row| row.get(0))?;
    Ok(PlaylistPage { items, total })
}

/// Appends tracks in the order given. Nothing is added if any track is unknown.
pub fn add_tracks(
    conn: &Connection,
    id: &str,
    track_ids: &[String],
    now: i64,
) -> Result<Playlist, ApiError> {
    let playlist = internal_id(conn, id)?;
    let mut position: i64 = conn
        .prepare_cached(
            "SELECT coalesce(max(position), 0) FROM playlist_items WHERE playlist_id = ?1",
        )?
        .query_row([playlist], |row| row.get(0))?;
    let mut track_id = conn.prepare_cached("SELECT id FROM tracks WHERE public_id = ?1")?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO playlist_items (playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
    )?;
    for public_id in track_ids {
        let track: i64 = track_id
            .query_row([public_id], |row| row.get(0))
            .optional()?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "that track no longer exists"))?;
        position += POSITION_GAP;
        insert.execute(params![playlist, position, track])?;
    }
    touch(conn, playlist, now)?;
    get(conn, playlist)
}

pub fn remove_items(
    conn: &Connection,
    id: &str,
    positions: &[i64],
    now: i64,
) -> Result<Playlist, ApiError> {
    let playlist = internal_id(conn, id)?;
    let mut delete =
        conn.prepare_cached("DELETE FROM playlist_items WHERE playlist_id = ?1 AND position = ?2")?;
    for position in positions {
        delete.execute(params![playlist, position])?;
    }
    touch(conn, playlist, now)?;
    get(conn, playlist)
}

/// Moves entries so they sit together, in their current order, right before the entry
/// at `before`, or at the end when `before` is `None`. Positions are renumbered.
pub fn move_items(
    conn: &Connection,
    id: &str,
    positions: &[i64],
    before: Option<i64>,
    now: i64,
) -> Result<Playlist, ApiError> {
    let playlist = internal_id(conn, id)?;
    let current: Vec<(i64, i64)> = conn
        .prepare_cached(
            "SELECT position, track_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position",
        )?
        .query_map([playlist], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;

    let moving: HashSet<i64> = positions.iter().copied().collect();
    let known = |wanted: i64| current.iter().any(|(position, _)| *position == wanted);
    if positions.iter().any(|position| !known(*position))
        || before.is_some_and(|position| !known(position))
    {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "the playlist changed; reload it and try again",
        ));
    }

    let (moved, kept): (Vec<_>, Vec<_>) = current
        .iter()
        .copied()
        .partition(|(position, _)| moving.contains(position));
    // The target may itself be moving, so the insertion point is the first entry at or
    // after it that stays where it is.
    let insert_at = match before {
        None => kept.len(),
        Some(target) => {
            let from = current
                .iter()
                .position(|(position, _)| *position == target)
                .unwrap_or(current.len());
            current[from..]
                .iter()
                .find(|(position, _)| !moving.contains(position))
                .and_then(|(position, _)| kept.iter().position(|(kept, _)| kept == position))
                .unwrap_or(kept.len())
        }
    };
    let mut reordered = kept;
    reordered.splice(insert_at..insert_at, moved);

    conn.prepare_cached("DELETE FROM playlist_items WHERE playlist_id = ?1")?
        .execute([playlist])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO playlist_items (playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
    )?;
    for (index, (_, track)) in reordered.iter().enumerate() {
        insert.execute(params![playlist, (index as i64 + 1) * POSITION_GAP, track])?;
    }
    touch(conn, playlist, now)?;
    get(conn, playlist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID, Database, DbError};

    struct Fixture {
        _dir: tempfile::TempDir,
        db: Database,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        db.write(|conn| {
            conn.execute_batch(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                     VALUES (1, 'r', 'Music', 'M:/', 0);
                 INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                     VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x');",
            )?;
            for name in ["a", "b", "c", "d"] {
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                         duration_ms, title_sort, artist_sort, album_sort, album_artist_sort,
                         added_at, updated_at)
                     VALUES (?1, 1, ?1, 1, 1, ?1, 1, 1000, ?1, 'x', 'x', 'x', 0, 0)",
                    [name],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        Fixture { _dir: dir, db }
    }

    impl Fixture {
        fn run<T>(
            &self,
            work: impl FnOnce(&Connection) -> Result<T, ApiError>,
        ) -> Result<T, ApiError> {
            self.db.write(|conn| work(conn))
        }

        fn with_tracks(&self, tracks: &[&str]) -> Playlist {
            let list = self.run(|conn| create(conn, "List", 1)).unwrap();
            let ids: Vec<String> = tracks.iter().map(|id| (*id).to_owned()).collect();
            self.run(|conn| add_tracks(conn, &list.id, &ids, 2))
                .unwrap()
        }

        fn entries(&self, id: &str) -> Vec<(i64, String)> {
            self.run(|conn| items(conn, id, 0, MAX_PAGE))
                .unwrap()
                .items
                .into_iter()
                .map(|item| (item.position, item.track.title))
                .collect()
        }

        fn titles(&self, id: &str) -> Vec<String> {
            self.entries(id)
                .into_iter()
                .map(|(_, title)| title)
                .collect()
        }

        fn positions(&self, id: &str) -> Vec<i64> {
            self.entries(id)
                .into_iter()
                .map(|(position, _)| position)
                .collect()
        }
    }

    #[test]
    fn a_playlist_can_be_created_renamed_listed_and_deleted() {
        let fixture = fixture();
        let made = fixture.run(|conn| create(conn, "  Road Trip ", 5)).unwrap();
        assert_eq!(made.name, "Road Trip");
        assert_eq!(made.track_count, 0);
        assert!(uuid::Uuid::parse_str(&made.id).is_ok());

        let renamed = fixture
            .run(|conn| rename(conn, &made.id, "Long Drive", 9))
            .unwrap();
        assert_eq!(
            (renamed.name.as_str(), renamed.updated_at),
            ("Long Drive", 9)
        );
        assert_eq!(fixture.run(list).unwrap(), [renamed]);

        fixture.run(|conn| delete(conn, &made.id)).unwrap();
        assert!(fixture.run(list).unwrap().is_empty());
    }

    #[test]
    fn a_blank_name_is_rejected() {
        let fixture = fixture();
        let error = fixture.run(|conn| create(conn, "   ", 1)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
        let made = fixture.run(|conn| create(conn, "Keep", 1)).unwrap();
        let error = fixture
            .run(|conn| rename(conn, &made.id, "", 1))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
    }

    #[test]
    fn lists_are_sorted_by_name_ignoring_case() {
        let fixture = fixture();
        for name in ["beta", "Alpha", "Gamma"] {
            fixture.run(|conn| create(conn, name, 1)).unwrap();
        }
        let names: Vec<String> = fixture
            .run(list)
            .unwrap()
            .into_iter()
            .map(|playlist| playlist.name)
            .collect();
        assert_eq!(names, ["Alpha", "beta", "Gamma"]);
    }

    #[test]
    fn the_reserved_player_playlist_is_out_of_reach() {
        let fixture = fixture();
        let player = DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID;
        assert!(fixture.run(list).unwrap().is_empty());
        for error in [
            fixture
                .run(|conn| rename(conn, player, "Mine", 1))
                .unwrap_err(),
            fixture.run(|conn| delete(conn, player)).unwrap_err(),
            fixture.run(|conn| items(conn, player, 0, 10)).unwrap_err(),
            fixture
                .run(|conn| add_tracks(conn, player, &["a".to_owned()], 1))
                .unwrap_err(),
        ] {
            assert_eq!(error.code, ErrorCode::NotFound);
        }
    }

    #[test]
    fn an_unknown_playlist_is_reported() {
        let fixture = fixture();
        let error = fixture.run(|conn| delete(conn, "nope")).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
    }

    #[test]
    fn tracks_append_in_order_and_may_repeat_and_totals_update() {
        let fixture = fixture();
        let list = fixture.with_tracks(&["b", "a", "b"]);
        assert_eq!(list.track_count, 3);
        assert_eq!(list.duration_ms, 3000);
        assert_eq!(list.updated_at, 2);
        assert_eq!(fixture.titles(&list.id), ["b", "a", "b"]);

        fixture
            .run(|conn| add_tracks(conn, &list.id, &["c".to_owned()], 3))
            .unwrap();
        assert_eq!(fixture.titles(&list.id), ["b", "a", "b", "c"]);
    }

    #[test]
    fn adding_an_unknown_track_adds_nothing() {
        let fixture = fixture();
        let list = fixture.with_tracks(&["a"]);
        let ids = vec!["b".to_owned(), "missing".to_owned()];
        let result = fixture.db.write(|conn| {
            let tx = conn.transaction()?;
            add_tracks(&tx, &list.id, &ids, 3)?;
            tx.commit()?;
            Ok::<_, ApiError>(())
        });
        assert_eq!(result.unwrap_err().code, ErrorCode::NotFound);
        assert_eq!(fixture.titles(&list.id), ["a"]);
    }

    #[test]
    fn items_can_be_paged_and_report_the_whole_list() {
        let fixture = fixture();
        let list = fixture.with_tracks(&["a", "b", "c", "d"]);
        let page = fixture.run(|conn| items(conn, &list.id, 1, 2)).unwrap();
        assert_eq!(page.total, 4);
        let titles: Vec<_> = page
            .items
            .iter()
            .map(|item| item.track.title.as_str())
            .collect();
        assert_eq!(titles, ["b", "c"]);
    }

    #[test]
    fn removing_entries_removes_only_those_copies() {
        let fixture = fixture();
        let list = fixture.with_tracks(&["a", "b", "a", "c"]);
        let positions = fixture.positions(&list.id);
        let updated = fixture
            .run(|conn| remove_items(conn, &list.id, &[positions[0]], 5))
            .unwrap();
        assert_eq!(updated.track_count, 3);
        assert_eq!(fixture.titles(&list.id), ["b", "a", "c"]);
    }

    #[test]
    fn deleting_a_playlist_keeps_its_tracks() {
        let fixture = fixture();
        let list = fixture.with_tracks(&["a"]);
        fixture.run(|conn| delete(conn, &list.id)).unwrap();
        let count: i64 = fixture
            .run(|conn| Ok(conn.query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))?))
            .unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn entries_move_before_a_target_or_to_the_end_keeping_their_order() {
        // Indexes into [a, b, c, d]: what moves, the target, and the expected result.
        let cases: &[(&[usize], Option<usize>, [&str; 4])] = &[
            (&[3], Some(0), ["d", "a", "b", "c"]),
            (&[0], None, ["b", "c", "d", "a"]),
            (&[0], Some(2), ["b", "a", "c", "d"]),
            (&[1, 3], Some(0), ["b", "d", "a", "c"]),
            (&[3, 1], Some(0), ["b", "d", "a", "c"]),
            (&[0, 2], None, ["b", "d", "a", "c"]),
            // Moving a block onto itself leaves it in place.
            (&[1, 2], Some(1), ["a", "b", "c", "d"]),
            (&[1, 2], Some(2), ["a", "b", "c", "d"]),
            (&[0, 1], Some(1), ["a", "b", "c", "d"]),
            (&[2, 3], Some(3), ["a", "b", "c", "d"]),
            (&[], Some(0), ["a", "b", "c", "d"]),
        ];
        for (moving, before, expected) in cases {
            let fixture = fixture();
            let list = fixture.with_tracks(&["a", "b", "c", "d"]);
            let positions = fixture.positions(&list.id);
            let chosen: Vec<i64> = moving.iter().map(|index| positions[*index]).collect();
            let target = before.map(|index| positions[index]);
            fixture
                .run(|conn| move_items(conn, &list.id, &chosen, target, 5))
                .unwrap();
            assert_eq!(fixture.titles(&list.id), expected, "{moving:?} {before:?}");
        }
    }

    #[test]
    fn a_stale_position_is_refused_and_changes_nothing() {
        let fixture = fixture();
        let list = fixture.with_tracks(&["a", "b"]);
        let positions = fixture.positions(&list.id);
        for (moving, before) in [(vec![99_999], None), (vec![positions[0]], Some(99_999))] {
            let error = fixture
                .run(|conn| move_items(conn, &list.id, &moving, before, 5))
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);
        }
        assert_eq!(fixture.titles(&list.id), ["a", "b"]);
        assert_eq!(fixture.positions(&list.id), positions);
    }
}
