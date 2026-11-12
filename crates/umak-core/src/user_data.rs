//! Ratings and what has been played, which belong to the person and not to the files. They
//! live only in the database, so nothing here touches a music file.

use rusqlite::{Connection, OptionalExtension, params};
use umak_audio::protocol::PlaybackState;

use crate::api::{ApiError, ErrorCode};
use crate::db::DEFAULT_USER_ID;

pub const MAX_RATING: i64 = 10;

/// However long a track is, this much listening makes it a play, unless the user says otherwise.
const ENOUGH_LISTENING_MS: u64 = 4 * 60 * 1000;
/// Position reports come a few times a second. A bigger step than this is a seek, and what it
/// skipped was not listened to.
const LONGEST_STEP_MS: u64 = 2000;

/// Sets or clears the rating of these tracks. A rating runs from 0 to 10, in half-stars.
pub fn set_rating(
    conn: &Connection,
    track_ids: &[String],
    rating: Option<i64>,
) -> Result<(), ApiError> {
    if rating.is_some_and(|rating| !(0..=MAX_RATING).contains(&rating)) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgs,
            "a rating runs from 0 to 10",
        ));
    }
    let mut internal = conn.prepare_cached("SELECT id FROM tracks WHERE public_id = ?1")?;
    let mut update = conn.prepare_cached(
        "INSERT INTO track_user_data (user_id, track_id, rating) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id, track_id) DO UPDATE SET rating = excluded.rating",
    )?;
    for public_id in track_ids {
        let track: i64 = internal
            .query_row([public_id], |row| row.get(0))
            .optional()?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "that track no longer exists"))?;
        update.execute(params![DEFAULT_USER_ID, track, rating])?;
    }
    Ok(())
}

/// Counts a play. A track that has gone from the library is simply not counted.
pub fn record_play(conn: &Connection, track_id: &str, now: i64) -> rusqlite::Result<()> {
    let Some(track): Option<i64> = conn
        .prepare_cached("SELECT id FROM tracks WHERE public_id = ?1")?
        .query_row([track_id], |row| row.get(0))
        .optional()?
    else {
        return Ok(());
    };
    conn.prepare_cached(
        "INSERT INTO track_user_data (user_id, track_id, play_count, last_played_at)
         VALUES (?1, ?2, 1, ?3)
         ON CONFLICT (user_id, track_id) DO UPDATE SET
             play_count = play_count + 1, last_played_at = excluded.last_played_at",
    )?
    .execute(params![DEFAULT_USER_ID, track, now])?;
    conn.prepare_cached(
        "INSERT INTO play_history (user_id, track_id, played_at) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![DEFAULT_USER_ID, track, now])?;
    Ok(())
}

/// How much listening makes a play: a share of the track or a length of time, whichever comes
/// first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayRule {
    /// From 0 to 1.
    pub share: f64,
    pub enough_ms: u64,
}

impl Default for PlayRule {
    /// Half the track or four minutes, the rule scrobbling services use.
    fn default() -> Self {
        Self {
            share: 0.5,
            enough_ms: ENOUGH_LISTENING_MS,
        }
    }
}

/// Decides when listening has become a play, by its rule. It measures listening, not position,
/// so seeking to the end of a track does not count, and pausing makes no difference.
#[derive(Debug, Default)]
pub struct PlayTracker {
    rule: PlayRule,
    track: Option<String>,
    needed_ms: u64,
    listened_ms: u64,
    position_ms: u64,
    counted: bool,
    playing: bool,
}

impl PlayTracker {
    /// Takes effect from the next report of the player's state.
    pub fn set_rule(&mut self, rule: PlayRule) {
        self.rule = rule;
    }

    /// Takes in the player's state. A new track, or going back to the start of this one, starts
    /// a new listen. A reorder of the queue changes neither.
    pub fn on_state(
        &mut self,
        state: PlaybackState,
        track: Option<&str>,
        position_ms: u64,
        duration_ms: Option<u64>,
    ) {
        let restarted = position_ms <= LONGEST_STEP_MS && self.position_ms > LONGEST_STEP_MS;
        if self.track.as_deref() != track || restarted || state == PlaybackState::Stopped {
            self.track = track.map(str::to_owned);
            self.listened_ms = 0;
            self.counted = false;
        }
        let rule = self.rule;
        self.needed_ms = duration_ms.map_or(rule.enough_ms, |duration| {
            ((duration as f64 * rule.share) as u64).min(rule.enough_ms)
        });
        self.position_ms = position_ms;
        self.playing = state == PlaybackState::Playing;
    }

    /// Takes in a position report, and returns the track if this report made it a play.
    pub fn on_position(&mut self, position_ms: u64) -> Option<String> {
        let step = position_ms.saturating_sub(self.position_ms);
        let moved_on = position_ms > self.position_ms;
        self.position_ms = position_ms;
        if !self.playing || !moved_on || step > LONGEST_STEP_MS {
            return None;
        }
        self.listened_ms += step;
        if self.counted || self.listened_ms < self.needed_ms {
            return None;
        }
        self.counted = true;
        self.track.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbError};

    const MINUTE: u64 = 60 * 1000;

    /// Plays from one position to another in steps like the engine's, returning any plays.
    fn listen(tracker: &mut PlayTracker, from: u64, to: u64) -> Vec<String> {
        let mut plays = Vec::new();
        let mut position = from;
        while position < to {
            position = (position + 250).min(to);
            plays.extend(tracker.on_position(position));
        }
        plays
    }

    fn start(tracker: &mut PlayTracker, track: &str, duration: u64) {
        tracker.on_state(PlaybackState::Playing, Some(track), 0, Some(duration));
    }

    #[test]
    fn half_a_track_is_a_play_and_it_counts_once() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 3 * MINUTE);
        assert!(listen(&mut tracker, 0, 89 * 1000).is_empty());
        assert_eq!(listen(&mut tracker, 89 * 1000, 91 * 1000), ["a"]);
        assert!(listen(&mut tracker, 91 * 1000, 3 * MINUTE).is_empty());
    }

    #[test]
    fn four_minutes_of_a_long_track_is_a_play() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 30 * MINUTE);
        assert!(listen(&mut tracker, 0, 4 * MINUTE - 1000).is_empty());
        assert_eq!(listen(&mut tracker, 4 * MINUTE - 1000, 4 * MINUTE), ["a"]);
    }

    #[test]
    fn seeking_past_a_track_is_not_listening_to_it() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 3 * MINUTE);
        tracker.on_state(
            PlaybackState::Playing,
            Some("a"),
            170 * 1000,
            Some(3 * MINUTE),
        );
        assert!(listen(&mut tracker, 170 * 1000, 3 * MINUTE).is_empty());
    }

    #[test]
    fn a_jump_with_no_state_report_is_not_listening_either() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 3 * MINUTE);
        assert_eq!(tracker.on_position(170 * 1000), None);
    }

    #[test]
    fn listening_adds_up_across_a_pause_and_a_seek() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 3 * MINUTE);
        listen(&mut tracker, 0, 50 * 1000);
        tracker.on_state(
            PlaybackState::Paused,
            Some("a"),
            50 * 1000,
            Some(3 * MINUTE),
        );
        assert_eq!(tracker.on_position(50 * 1000), None);
        tracker.on_state(
            PlaybackState::Playing,
            Some("a"),
            100 * 1000,
            Some(3 * MINUTE),
        );
        assert!(listen(&mut tracker, 100 * 1000, 139 * 1000).is_empty());
        assert_eq!(listen(&mut tracker, 139 * 1000, 141 * 1000), ["a"]);
    }

    #[test]
    fn the_next_track_starts_again_even_if_it_is_the_same_one() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", MINUTE);
        assert_eq!(listen(&mut tracker, 0, MINUTE), ["a"]);
        start(&mut tracker, "b", MINUTE);
        assert_eq!(listen(&mut tracker, 0, MINUTE), ["b"]);
        start(&mut tracker, "b", MINUTE);
        assert_eq!(listen(&mut tracker, 0, MINUTE), ["b"]);
    }

    #[test]
    fn listening_does_not_carry_over_to_the_next_track() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 2 * MINUTE);
        listen(&mut tracker, 0, 50 * 1000);
        start(&mut tracker, "b", 2 * MINUTE);
        assert!(listen(&mut tracker, 0, 50 * 1000).is_empty());
    }

    #[test]
    fn stopping_ends_the_listen() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", 2 * MINUTE);
        listen(&mut tracker, 0, 50 * 1000);
        tracker.on_state(PlaybackState::Stopped, Some("a"), 0, Some(2 * MINUTE));
        assert_eq!(tracker.on_position(50 * 1000), None);
        start(&mut tracker, "a", 2 * MINUTE);
        assert!(listen(&mut tracker, 0, 50 * 1000).is_empty());
    }

    #[test]
    fn a_small_step_back_does_not_start_a_new_play() {
        let mut tracker = PlayTracker::default();
        start(&mut tracker, "a", MINUTE);
        assert_eq!(listen(&mut tracker, 0, 40 * 1000), ["a"]);
        tracker.on_state(PlaybackState::Playing, Some("a"), 30 * 1000, Some(MINUTE));
        assert!(listen(&mut tracker, 30 * 1000, MINUTE).is_empty());
    }

    fn library() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        db.write(|conn| {
            conn.execute_batch(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                     VALUES (1, 'r', 'Music', 'M:/', 0);
                 INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                     VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x');
                 INSERT INTO tracks (id, public_id, root_id, rel_path, size, mtime, title, album_id,
                     title_sort, artist_sort, album_sort, album_artist_sort, added_at, updated_at)
                 VALUES (1, 'one', 1, '1.mp3', 1, 1, 'One', 1, 'one', 'x', 'album', 'x', 0, 0),
                        (2, 'two', 1, '2.mp3', 1, 1, 'Two', 1, 'two', 'x', 'album', 'x', 0, 0);",
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();
        (dir, db)
    }

    /// What the list of tracks would show: rating, plays and when last played.
    fn shown(db: &Database, id: &str) -> (Option<i64>, i64, Option<i64>) {
        use crate::library::{TRACK_COLUMNS, TrackRow};
        db.read(|conn| {
            let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks AS t WHERE t.public_id = ?1");
            let row = conn.query_row(&sql, [id], |row| TrackRow::from_row(row, 0))?;
            Ok::<_, DbError>((row.rating, row.play_count, row.last_played_at))
        })
        .unwrap()
    }

    fn rate(db: &Database, ids: &[&str], rating: Option<i64>) -> Result<(), ApiError> {
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        db.write(|conn| set_rating(conn, &ids, rating))
    }

    #[test]
    fn tracks_start_unrated_and_unplayed() {
        let (_dir, db) = library();
        assert_eq!(shown(&db, "one"), (None, 0, None));
    }

    #[test]
    fn a_rating_can_be_set_changed_and_cleared_on_several_tracks() {
        let (_dir, db) = library();
        rate(&db, &["one", "two"], Some(7)).unwrap();
        assert_eq!(shown(&db, "one").0, Some(7));
        assert_eq!(shown(&db, "two").0, Some(7));
        rate(&db, &["one"], Some(10)).unwrap();
        assert_eq!(shown(&db, "one").0, Some(10));
        rate(&db, &["one"], None).unwrap();
        assert_eq!(shown(&db, "one").0, None);
        assert_eq!(shown(&db, "two").0, Some(7));
    }

    #[test]
    fn a_rating_out_of_range_or_for_no_track_is_refused() {
        let (_dir, db) = library();
        assert!(matches!(
            rate(&db, &["one"], Some(11)).unwrap_err().code,
            ErrorCode::InvalidArgs
        ));
        assert!(matches!(
            rate(&db, &["one"], Some(-1)).unwrap_err().code,
            ErrorCode::InvalidArgs
        ));
        assert!(matches!(
            rate(&db, &["gone"], Some(5)).unwrap_err().code,
            ErrorCode::NotFound
        ));
    }

    #[test]
    fn a_play_adds_to_the_count_and_the_history_and_keeps_the_rating() {
        let (_dir, db) = library();
        rate(&db, &["one"], Some(8)).unwrap();
        db.write(|conn| {
            record_play(conn, "one", 100)?;
            record_play(conn, "one", 200)?;
            record_play(conn, "gone", 300)?;
            Ok::<_, DbError>(())
        })
        .unwrap();
        assert_eq!(shown(&db, "one"), (Some(8), 2, Some(200)));
        assert_eq!(shown(&db, "two"), (None, 0, None));
        let history: Vec<i64> = db
            .read(|conn| {
                let mut statement =
                    conn.prepare("SELECT played_at FROM play_history ORDER BY played_at")?;
                let times = statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                Ok::<_, DbError>(times)
            })
            .unwrap();
        assert_eq!(history, [100, 200]);
    }
}
