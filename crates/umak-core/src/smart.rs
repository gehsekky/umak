//! Rule-based playlists. A rule is stored instead of a list of tracks and worked out each time,
//! so the list follows the library, ratings and plays as they change. Files that are missing are
//! left out, because they could not be played.
//!
//! A rule is checked before use, and turned into SQL only from fixed pieces: anything the person
//! typed reaches the query as a bound value.

use rusqlite::types::Value;
use rusqlite::{Connection, ToSql, params_from_iter};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::{ApiError, ErrorCode};
use crate::db::DEFAULT_USER_ID;
use crate::library::{MAX_PAGE, TRACK_COLUMNS, TrackRow};
use crate::user_data::MAX_RATING;

const MAX_CONDITIONS: usize = 50;
const MAX_LIMIT: u32 = 1_000_000;
/// About 270 years, far more than anyone needs and far from overflowing when turned to time.
const MAX_DAYS: i64 = 100_000;
const MAX_NUMBER: i64 = 1_000_000_000;
const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
const MS_PER_MINUTE: i64 = 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SmartField {
    Title,
    Artist,
    Album,
    Genre,
    /// The codec, such as flac.
    Format,
    Year,
    /// From 0 to 10, in half-stars.
    Rating,
    Plays,
    /// In whole minutes.
    Length,
    /// In kilobits per second.
    Bitrate,
    LastPlayed,
    Added,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SmartOp {
    Contains,
    NotContains,
    /// Text ignores case, for the letters A to Z.
    Is,
    IsNot,
    AtLeast,
    AtMost,
    /// The field has no value: not rated, never played, or no year.
    Empty,
    WithinDays,
    /// Older than this many days. A track never played counts as not played in that time.
    NotWithinDays,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SmartCondition {
    pub field: SmartField,
    pub op: SmartOp,
    /// The value of a text field or a format.
    pub text: Option<String>,
    /// The value of a number field, or a number of days.
    pub number: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SmartSort {
    Album,
    Artist,
    Title,
    Added,
    LastPlayed,
    Rating,
    Plays,
    Year,
    Length,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SmartRule {
    /// Whether a track must meet every condition, or any one of them.
    pub match_all: bool,
    /// With none, every track matches, for lists like the 50 most played.
    pub conditions: Vec<SmartCondition>,
    pub sort: SmartSort,
    pub descending: bool,
    /// Keeps only this many tracks from the top of the sorted list.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Format,
    Number,
    Rating,
    Date,
}

fn kind_of(field: SmartField) -> Kind {
    match field {
        SmartField::Title | SmartField::Artist | SmartField::Album | SmartField::Genre => {
            Kind::Text
        }
        SmartField::Format => Kind::Format,
        SmartField::Year | SmartField::Plays | SmartField::Length | SmartField::Bitrate => {
            Kind::Number
        }
        SmartField::Rating => Kind::Rating,
        SmartField::LastPlayed | SmartField::Added => Kind::Date,
    }
}

fn op_fits(field: SmartField, op: SmartOp) -> bool {
    use SmartOp::{
        AtLeast, AtMost, Contains, Empty, Is, IsNot, NotContains, NotWithinDays, WithinDays,
    };
    match kind_of(field) {
        Kind::Text => matches!(op, Contains | NotContains | Is | IsNot),
        Kind::Format => matches!(op, Is | IsNot),
        Kind::Number => {
            matches!(op, AtLeast | AtMost | Is) || (op == Empty && field == SmartField::Year)
        }
        Kind::Rating => matches!(op, AtLeast | AtMost | Is | Empty),
        Kind::Date => {
            matches!(op, WithinDays | NotWithinDays)
                || (op == Empty && field == SmartField::LastPlayed)
        }
    }
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgs, message)
}

impl SmartCondition {
    fn check(&self) -> Result<(), ApiError> {
        if !op_fits(self.field, self.op) {
            return Err(invalid("that comparison does not apply to that field"));
        }
        if self.op == SmartOp::Empty {
            return Ok(());
        }
        match kind_of(self.field) {
            Kind::Text | Kind::Format => {
                if self
                    .text
                    .as_deref()
                    .is_none_or(|text| text.trim().is_empty())
                {
                    return Err(invalid("a rule is missing its text"));
                }
            }
            Kind::Number | Kind::Rating | Kind::Date => {
                let (lowest, highest, name) = match kind_of(self.field) {
                    Kind::Rating => (0, MAX_RATING, "a rating"),
                    Kind::Date => (1, MAX_DAYS, "a number of days"),
                    _ => (0, MAX_NUMBER, "a number"),
                };
                let number = self
                    .number
                    .ok_or_else(|| invalid(format!("a rule is missing {name}")))?;
                if !(lowest..=highest).contains(&number) {
                    return Err(invalid(format!(
                        "{name} in a rule must be from {lowest} to {highest}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// The SQL for this condition, adding its values to `values`.
    fn sql(&self, now: i64, values: &mut Vec<Value>) -> String {
        match kind_of(self.field) {
            Kind::Text | Kind::Format => self.text_sql(values),
            Kind::Number | Kind::Rating => self.number_sql(values),
            Kind::Date => self.date_sql(now, values),
        }
    }

    fn text_sql(&self, values: &mut Vec<Value>) -> String {
        let text = self.text.clone().unwrap_or_default();
        let (test, value) = match self.op {
            SmartOp::Contains | SmartOp::NotContains => {
                let escaped = text
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                ("LIKE ? ESCAPE '\\'", format!("%{escaped}%"))
            }
            _ => ("= ? COLLATE NOCASE", text),
        };
        values.push(Value::Text(value));
        let negative = matches!(self.op, SmartOp::NotContains | SmartOp::IsNot);
        if self.field == SmartField::Genre {
            let exists = format!(
                "EXISTS (SELECT 1 FROM track_genres AS tg JOIN genres AS g ON g.id = tg.genre_id
                         WHERE tg.track_id = t.id AND g.name {test})"
            );
            return if negative {
                format!("NOT {exists}")
            } else {
                exists
            };
        }
        let column = match self.field {
            SmartField::Title => "t.title",
            SmartField::Artist => "t.artist_display",
            SmartField::Album => "t.album_display",
            _ => "coalesce(t.codec, '')",
        };
        if negative {
            format!("NOT ({column} {test})")
        } else {
            format!("{column} {test}")
        }
    }

    fn number_sql(&self, values: &mut Vec<Value>) -> String {
        let column = match self.field {
            SmartField::Year => "t.year",
            SmartField::Rating => "d.rating",
            SmartField::Plays => "coalesce(d.play_count, 0)",
            SmartField::Length => "t.duration_ms / 60000",
            _ => "t.bitrate",
        };
        if self.op == SmartOp::Empty {
            return format!("{column} IS NULL");
        }
        let number = self.number.unwrap_or_default();
        // A length in minutes is compared in full, so 3:30 is not "at most 3 minutes".
        if self.field == SmartField::Length && self.op == SmartOp::AtMost {
            values.push(Value::Integer(number * MS_PER_MINUTE));
            return "t.duration_ms <= ?".to_owned();
        }
        values.push(Value::Integer(number));
        let test = match self.op {
            SmartOp::AtLeast => ">=",
            SmartOp::AtMost => "<=",
            _ => "=",
        };
        format!("{column} {test} ?")
    }

    fn date_sql(&self, now: i64, values: &mut Vec<Value>) -> String {
        let column = if self.field == SmartField::Added {
            "t.added_at"
        } else {
            "d.last_played_at"
        };
        if self.op == SmartOp::Empty {
            return format!("{column} IS NULL");
        }
        values.push(Value::Integer(
            now - self.number.unwrap_or_default() * MS_PER_DAY,
        ));
        if self.op == SmartOp::WithinDays {
            format!("{column} >= ?")
        } else {
            format!("({column} IS NULL OR {column} < ?)")
        }
    }
}

impl SmartRule {
    pub fn check(&self) -> Result<(), ApiError> {
        if self.conditions.len() > MAX_CONDITIONS {
            return Err(invalid(format!(
                "a smart playlist can have at most {MAX_CONDITIONS} rules"
            )));
        }
        if self
            .limit
            .is_some_and(|limit| !(1..=MAX_LIMIT).contains(&limit))
        {
            return Err(invalid(format!(
                "a smart playlist's limit must be from 1 to {MAX_LIMIT}"
            )));
        }
        self.conditions.iter().try_for_each(SmartCondition::check)
    }

    /// The tracks and joins to select from, and the values that go with them.
    fn from_sql(&self, now: i64) -> (String, Vec<Value>) {
        let mut values = Vec::new();
        let tests: Vec<String> = self
            .conditions
            .iter()
            .map(|condition| condition.sql(now, &mut values))
            .collect();
        let matching = if tests.is_empty() {
            String::new()
        } else {
            let joiner = if self.match_all { " AND " } else { " OR " };
            format!("AND ({})", tests.join(joiner))
        };
        let sql = format!(
            "FROM tracks AS t
             LEFT JOIN track_user_data AS d ON d.user_id = {DEFAULT_USER_ID} AND d.track_id = t.id
             WHERE t.missing_since IS NULL {matching}"
        );
        (sql, values)
    }

    fn order_sql(&self) -> String {
        let keys: &[&str] = match self.sort {
            SmartSort::Album => &[
                "t.album_artist_sort",
                "t.album_sort",
                "t.album_id",
                "t.disc_no",
                "t.track_no",
            ],
            SmartSort::Artist => &[
                "t.artist_sort",
                "t.album_sort",
                "t.album_id",
                "t.disc_no",
                "t.track_no",
            ],
            SmartSort::Title => &["t.title_sort"],
            SmartSort::Added => &["t.added_at"],
            SmartSort::LastPlayed => &["d.last_played_at"],
            SmartSort::Rating => &["d.rating"],
            SmartSort::Plays => &["coalesce(d.play_count, 0)"],
            SmartSort::Year => &["t.year"],
            SmartSort::Length => &["t.duration_ms"],
        };
        let direction = if self.descending { " DESC" } else { "" };
        let mut order: Vec<String> = keys.iter().map(|key| format!("{key}{direction}")).collect();
        // Ties keep a steady order, so pages of a list always fit together.
        order.push(format!("t.id{direction}"));
        order.join(", ")
    }
}

/// How many tracks a rule gives, and how long they play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmartTotals {
    pub total: u32,
    pub duration_ms: i64,
}

/// Works out the totals. With no limit there is no need to sort anything first.
pub fn totals(conn: &Connection, rule: &SmartRule, now: i64) -> rusqlite::Result<SmartTotals> {
    let (from, values) = rule.from_sql(now);
    let sql = match rule.limit {
        None => format!("SELECT count(*), coalesce(sum(t.duration_ms), 0) {from}"),
        Some(limit) => format!(
            "SELECT count(*), coalesce(sum(duration_ms), 0) FROM (
                 SELECT t.duration_ms AS duration_ms {from} ORDER BY {} LIMIT {limit}
             )",
            rule.order_sql()
        ),
    };
    conn.prepare_cached(&sql)?
        .query_row(params_from_iter(values.iter()), |row| {
            Ok(SmartTotals {
                total: row.get(0)?,
                duration_ms: row.get(1)?,
            })
        })
}

/// A page of the list, with each row's place in it counted from 1.
pub fn page(
    conn: &Connection,
    rule: &SmartRule,
    now: i64,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<Vec<(i64, TrackRow)>> {
    let end = u64::from(rule.limit.unwrap_or(u32::MAX));
    let wanted = u64::from(limit.min(MAX_PAGE)).min(end.saturating_sub(u64::from(offset)));
    let (from, mut values) = rule.from_sql(now);
    values.push(Value::Integer(wanted as i64));
    values.push(Value::Integer(i64::from(offset)));
    let sql = format!(
        "SELECT {TRACK_COLUMNS} {from} ORDER BY {} LIMIT ? OFFSET ?",
        rule.order_sql()
    );
    let bound: Vec<&dyn ToSql> = values.iter().map(|value| value as &dyn ToSql).collect();
    conn.prepare_cached(&sql)?
        .query_map(bound.as_slice(), |row| TrackRow::from_row(row, 0))?
        .enumerate()
        .map(|(index, row)| Ok((i64::from(offset) + index as i64 + 1, row?)))
        .collect()
}

/// Fills a playlist with the rule's tracks in order, with positions spaced by `gap`.
pub fn insert_into(
    conn: &Connection,
    playlist: i64,
    rule: &SmartRule,
    now: i64,
    gap: i64,
) -> rusqlite::Result<()> {
    let (from, values) = rule.from_sql(now);
    let limit = rule
        .limit
        .map_or(String::new(), |limit| format!("LIMIT {limit}"));
    let order = rule.order_sql();
    let sql = format!(
        "INSERT INTO playlist_items (playlist_id, position, track_id)
         SELECT {playlist}, row_number() OVER () * {gap}, id FROM (
             SELECT t.id AS id {from} ORDER BY {order} {limit}
         )"
    );
    conn.prepare_cached(&sql)?
        .execute(params_from_iter(values.iter()))?;
    Ok(())
}

/// The formats present in the library, to choose from in a rule.
pub fn formats(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(
        "SELECT DISTINCT codec FROM tracks
         WHERE codec IS NOT NULL AND missing_since IS NULL ORDER BY codec",
    )?
    .query_map([], |row| row.get(0))?
    .collect()
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::db::{Database, DbError, new_public_id};

    const NOW: i64 = 1000 * MS_PER_DAY;

    #[derive(Clone, Copy)]
    struct Track<'a> {
        title: &'a str,
        artist: &'a str,
        album: &'a str,
        codec: &'a str,
        year: Option<i64>,
        minutes: i64,
        bitrate: i64,
        added_days_ago: i64,
        rating: Option<i64>,
        plays: i64,
        played_days_ago: Option<i64>,
        genre: Option<&'a str>,
        missing: bool,
    }

    const TRACK: Track<'static> = Track {
        title: "Song",
        artist: "Artist",
        album: "Album",
        codec: "mp3",
        year: None,
        minutes: 3,
        bitrate: 320,
        added_days_ago: 10,
        rating: None,
        plays: 0,
        played_days_ago: None,
        genre: None,
        missing: false,
    };

    fn library(tracks: &[Track<'_>]) -> (tempfile::TempDir, Database) {
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
            for (index, track) in tracks.iter().enumerate() {
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, missing_since,
                         duration_ms, codec, bitrate, title, album_id, year, artist_display,
                         album_display, title_sort, artist_sort, album_sort, album_artist_sort,
                         added_at, updated_at)
                     VALUES (?1, 1, ?2, 1, 1, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?10, ?11, ?12, ?13,
                             ?12, ?14, 0)",
                    params![
                        new_public_id(),
                        format!("{index}.mp3"),
                        track.missing.then_some(1),
                        track.minutes * MS_PER_MINUTE,
                        track.codec,
                        track.bitrate,
                        track.title,
                        track.year,
                        track.artist,
                        track.album,
                        track.title.to_lowercase(),
                        track.artist.to_lowercase(),
                        track.album.to_lowercase(),
                        NOW - track.added_days_ago * MS_PER_DAY,
                    ],
                )?;
                let id = conn.last_insert_rowid();
                if track.rating.is_some() || track.plays > 0 {
                    conn.execute(
                        "INSERT INTO track_user_data (user_id, track_id, rating, play_count,
                                                      last_played_at)
                         VALUES (1, ?1, ?2, ?3, ?4)",
                        params![
                            id,
                            track.rating,
                            track.plays,
                            track.played_days_ago.map(|days| NOW - days * MS_PER_DAY)
                        ],
                    )?;
                }
                if let Some(genre) = track.genre {
                    conn.execute(
                        "INSERT OR IGNORE INTO genres (name, norm_key) VALUES (?1, ?2)",
                        params![genre, genre.to_lowercase()],
                    )?;
                    conn.execute(
                        "INSERT INTO track_genres (track_id, genre_id, position)
                         VALUES (?1, (SELECT id FROM genres WHERE name = ?2), 0)",
                        params![id, genre],
                    )?;
                }
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        (dir, db)
    }

    fn rule(conditions: Vec<SmartCondition>) -> SmartRule {
        SmartRule {
            match_all: true,
            conditions,
            sort: SmartSort::Title,
            descending: false,
            limit: None,
        }
    }

    fn text(field: SmartField, op: SmartOp, value: &str) -> SmartCondition {
        SmartCondition {
            field,
            op,
            text: Some(value.to_owned()),
            number: None,
        }
    }

    fn number(field: SmartField, op: SmartOp, value: i64) -> SmartCondition {
        SmartCondition {
            field,
            op,
            text: None,
            number: Some(value),
        }
    }

    fn empty(field: SmartField) -> SmartCondition {
        SmartCondition {
            field,
            op: SmartOp::Empty,
            text: None,
            number: None,
        }
    }

    fn titles(db: &Database, rule: &SmartRule) -> Vec<String> {
        rule.check().unwrap();
        db.read(|conn| {
            let rows = page(conn, rule, NOW, 0, 1000)?;
            let found: Vec<String> = rows.into_iter().map(|(_, row)| row.title).collect();
            let counted = totals(conn, rule, NOW)?;
            assert_eq!(counted.total as usize, found.len());
            Ok::<_, DbError>(found)
        })
        .unwrap()
    }

    #[test]
    fn the_rule_from_the_plan_flac_rated_highly_and_not_played_lately() {
        let good = |title| Track {
            title,
            codec: "flac",
            rating: Some(8),
            ..TRACK
        };
        let (_dir, db) = library(&[
            Track {
                plays: 1,
                played_days_ago: Some(200),
                ..good("Long ago")
            },
            good("Never played"),
            Track {
                plays: 1,
                played_days_ago: Some(5),
                ..good("Just played")
            },
            Track {
                codec: "mp3",
                ..good("An MP3")
            },
            Track {
                rating: Some(7),
                ..good("Rated lower")
            },
            Track {
                rating: None,
                ..good("Not rated")
            },
        ]);
        let found = titles(
            &db,
            &rule(vec![
                text(SmartField::Format, SmartOp::Is, "flac"),
                number(SmartField::Rating, SmartOp::AtLeast, 8),
                number(SmartField::LastPlayed, SmartOp::NotWithinDays, 180),
            ]),
        );
        assert_eq!(found, ["Long ago", "Never played"]);
    }

    #[test]
    fn any_needs_one_condition_and_all_needs_every_one() {
        let (_dir, db) = library(&[
            Track {
                title: "Both",
                artist: "Xa",
                year: Some(1999),
                ..TRACK
            },
            Track {
                title: "Artist",
                artist: "Xa",
                ..TRACK
            },
            Track {
                title: "Year",
                year: Some(1999),
                ..TRACK
            },
            Track {
                title: "Neither",
                ..TRACK
            },
        ]);
        let conditions = vec![
            text(SmartField::Artist, SmartOp::Is, "Xa"),
            number(SmartField::Year, SmartOp::Is, 1999),
        ];
        assert_eq!(titles(&db, &rule(conditions.clone())), ["Both"]);
        let any = SmartRule {
            match_all: false,
            ..rule(conditions)
        };
        assert_eq!(titles(&db, &any), ["Artist", "Both", "Year"]);
    }

    #[test]
    fn no_conditions_means_every_track_but_a_missing_one() {
        let (_dir, db) = library(&[
            Track {
                title: "Here",
                ..TRACK
            },
            Track {
                title: "Gone",
                missing: true,
                ..TRACK
            },
        ]);
        assert_eq!(titles(&db, &rule(vec![])), ["Here"]);
    }

    #[test]
    fn text_matches_ignoring_case_and_wildcards_mean_nothing() {
        let (_dir, db) = library(&[
            Track {
                title: "100% Pure",
                ..TRACK
            },
            Track {
                title: "100 Pure",
                ..TRACK
            },
            Track {
                title: "under_score",
                ..TRACK
            },
            Track {
                title: "underXscore",
                ..TRACK
            },
        ]);
        let with = |op, value| titles(&db, &rule(vec![text(SmartField::Title, op, value)]));
        assert_eq!(with(SmartOp::Contains, "%"), ["100% Pure"]);
        assert_eq!(with(SmartOp::Contains, "_"), ["under_score"]);
        assert_eq!(with(SmartOp::Contains, "PURE"), ["100 Pure", "100% Pure"]);
        assert_eq!(with(SmartOp::Is, "100 PURE"), ["100 Pure"]);
        assert_eq!(with(SmartOp::IsNot, "100 PURE").len(), 3);
        assert_eq!(
            with(SmartOp::NotContains, "pure"),
            ["under_score", "underXscore"]
        );
    }

    #[test]
    fn typed_text_cannot_change_the_query() {
        let (_dir, db) = library(&[Track {
            title: "Safe",
            ..TRACK
        }]);
        let hostile = "x' OR 1=1; DROP TABLE tracks; --";
        assert!(
            titles(
                &db,
                &rule(vec![text(SmartField::Title, SmartOp::Contains, hostile)])
            )
            .is_empty()
        );
        assert!(
            titles(
                &db,
                &rule(vec![text(SmartField::Genre, SmartOp::Is, hostile)])
            )
            .is_empty()
        );
        assert_eq!(titles(&db, &rule(vec![])), ["Safe"]);
    }

    #[test]
    fn genres_match_any_of_a_tracks_genres() {
        let (_dir, db) = library(&[
            Track {
                title: "Rock",
                genre: Some("Rock"),
                ..TRACK
            },
            Track {
                title: "Folk",
                genre: Some("Folk Rock"),
                ..TRACK
            },
            Track {
                title: "None",
                ..TRACK
            },
        ]);
        let with = |op, value| titles(&db, &rule(vec![text(SmartField::Genre, op, value)]));
        assert_eq!(with(SmartOp::Contains, "rock"), ["Folk", "Rock"]);
        assert_eq!(with(SmartOp::Is, "rock"), ["Rock"]);
        assert_eq!(with(SmartOp::NotContains, "folk"), ["None", "Rock"]);
        assert_eq!(with(SmartOp::IsNot, "rock"), ["Folk", "None"]);
    }

    #[test]
    fn numbers_compare_and_empty_finds_the_missing_ones() {
        let (_dir, db) = library(&[
            Track {
                title: "Old",
                year: Some(1970),
                rating: Some(2),
                plays: 5,
                ..TRACK
            },
            Track {
                title: "New",
                year: Some(2020),
                rating: Some(9),
                bitrate: 128,
                ..TRACK
            },
            Track {
                title: "Bare",
                ..TRACK
            },
        ]);
        let with = |condition| titles(&db, &rule(vec![condition]));
        assert_eq!(
            with(number(SmartField::Year, SmartOp::AtMost, 1999)),
            ["Old"]
        );
        assert_eq!(with(empty(SmartField::Year)), ["Bare"]);
        // An unrated track is not "rated at most" anything; it has its own test.
        assert_eq!(
            with(number(SmartField::Rating, SmartOp::AtMost, 5)),
            ["Old"]
        );
        assert_eq!(with(empty(SmartField::Rating)), ["Bare"]);
        // No plays recorded is zero plays.
        assert_eq!(
            with(number(SmartField::Plays, SmartOp::Is, 0)),
            ["Bare", "New"]
        );
        assert_eq!(
            with(number(SmartField::Plays, SmartOp::AtLeast, 1)),
            ["Old"]
        );
        assert_eq!(
            with(number(SmartField::Bitrate, SmartOp::AtMost, 128)),
            ["New"]
        );
    }

    #[test]
    fn a_length_at_most_some_minutes_means_all_of_it() {
        let (_dir, db) = library(&[
            Track {
                title: "Three",
                minutes: 3,
                ..TRACK
            },
            Track {
                title: "Four",
                minutes: 4,
                ..TRACK
            },
        ]);
        let with = |condition| titles(&db, &rule(vec![condition]));
        assert_eq!(
            with(number(SmartField::Length, SmartOp::AtMost, 3)),
            ["Three"]
        );
        assert_eq!(
            with(number(SmartField::Length, SmartOp::AtLeast, 4)),
            ["Four"]
        );
        assert_eq!(with(number(SmartField::Length, SmartOp::Is, 4)), ["Four"]);
    }

    #[test]
    fn dates_count_back_from_now_and_never_played_is_its_own_answer() {
        let (_dir, db) = library(&[
            Track {
                title: "Recent",
                added_days_ago: 1,
                plays: 1,
                played_days_ago: Some(2),
                ..TRACK
            },
            Track {
                title: "Old",
                added_days_ago: 400,
                plays: 1,
                played_days_ago: Some(300),
                ..TRACK
            },
            Track {
                title: "Never",
                added_days_ago: 30,
                ..TRACK
            },
        ]);
        let with = |condition| titles(&db, &rule(vec![condition]));
        assert_eq!(
            with(number(SmartField::LastPlayed, SmartOp::WithinDays, 30)),
            ["Recent"]
        );
        assert_eq!(
            with(number(SmartField::LastPlayed, SmartOp::NotWithinDays, 30)),
            ["Never", "Old"]
        );
        assert_eq!(with(empty(SmartField::LastPlayed)), ["Never"]);
        assert_eq!(
            with(number(SmartField::Added, SmartOp::WithinDays, 60)),
            ["Never", "Recent"]
        );
        assert_eq!(
            with(number(SmartField::Added, SmartOp::NotWithinDays, 60)),
            ["Old"]
        );
    }

    #[test]
    fn sorting_and_a_limit_keep_the_top_of_the_list_page_by_page() {
        let (_dir, db) = library(&[
            Track {
                title: "A",
                plays: 1,
                played_days_ago: Some(1),
                ..TRACK
            },
            Track {
                title: "B",
                plays: 9,
                played_days_ago: Some(1),
                ..TRACK
            },
            Track {
                title: "C",
                plays: 5,
                played_days_ago: Some(1),
                ..TRACK
            },
            Track {
                title: "D",
                ..TRACK
            },
        ]);
        let most_played = SmartRule {
            sort: SmartSort::Plays,
            descending: true,
            limit: Some(2),
            ..rule(vec![])
        };
        assert_eq!(titles(&db, &most_played), ["B", "C"]);

        db.read(|conn| {
            let first = page(conn, &most_played, NOW, 0, 1)?;
            assert_eq!((first[0].0, first[0].1.title.as_str()), (1, "B"));
            let second = page(conn, &most_played, NOW, 1, 10)?;
            assert_eq!((second[0].0, second[0].1.title.as_str()), (2, "C"));
            assert_eq!(second.len(), 1);
            assert!(page(conn, &most_played, NOW, 2, 10)?.is_empty());
            assert!(page(conn, &most_played, NOW, 5, 10)?.is_empty());
            Ok::<_, DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn totals_with_a_limit_add_up_only_the_kept_tracks() {
        let (_dir, db) = library(&[
            Track {
                title: "A",
                minutes: 1,
                ..TRACK
            },
            Track {
                title: "B",
                minutes: 2,
                ..TRACK
            },
            Track {
                title: "C",
                minutes: 4,
                ..TRACK
            },
        ]);
        let limited = SmartRule {
            limit: Some(2),
            ..rule(vec![])
        };
        let all = rule(vec![]);
        db.read(|conn| {
            assert_eq!(
                totals(conn, &limited, NOW)?,
                SmartTotals {
                    total: 2,
                    duration_ms: 3 * MS_PER_MINUTE
                }
            );
            assert_eq!(
                totals(conn, &all, NOW)?,
                SmartTotals {
                    total: 3,
                    duration_ms: 7 * MS_PER_MINUTE
                }
            );
            Ok::<_, DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn every_sort_works_in_both_directions() {
        let (_dir, db) = library(&[
            Track {
                title: "A",
                ..TRACK
            },
            Track {
                title: "B",
                year: Some(2000),
                rating: Some(5),
                plays: 2,
                played_days_ago: Some(1),
                ..TRACK
            },
        ]);
        for sort in [
            SmartSort::Album,
            SmartSort::Artist,
            SmartSort::Title,
            SmartSort::Added,
            SmartSort::LastPlayed,
            SmartSort::Rating,
            SmartSort::Plays,
            SmartSort::Year,
            SmartSort::Length,
        ] {
            for descending in [false, true] {
                let sorted = SmartRule {
                    sort,
                    descending,
                    ..rule(vec![])
                };
                assert_eq!(titles(&db, &sorted).len(), 2);
            }
        }
    }

    #[test]
    fn a_rule_that_makes_no_sense_is_refused() {
        let refused = |condition| {
            let error = rule(vec![condition]).check().unwrap_err();
            assert!(matches!(error.code, ErrorCode::InvalidArgs));
        };
        refused(number(SmartField::Title, SmartOp::AtLeast, 1));
        refused(text(SmartField::Format, SmartOp::Contains, "fl"));
        refused(text(SmartField::Title, SmartOp::Contains, "  "));
        refused(number(SmartField::Rating, SmartOp::AtLeast, 11));
        refused(number(SmartField::Plays, SmartOp::AtLeast, -1));
        refused(number(SmartField::LastPlayed, SmartOp::WithinDays, 0));
        refused(number(
            SmartField::LastPlayed,
            SmartOp::WithinDays,
            MAX_DAYS + 1,
        ));
        refused(empty(SmartField::Plays));
        refused(empty(SmartField::Added));
        refused(SmartCondition {
            field: SmartField::Year,
            op: SmartOp::Is,
            text: None,
            number: None,
        });

        let no_limit = SmartRule {
            limit: Some(0),
            ..rule(vec![])
        };
        assert!(no_limit.check().is_err());
        let too_many = rule(vec![empty(SmartField::Year); MAX_CONDITIONS + 1]);
        assert!(too_many.check().is_err());
    }

    #[test]
    fn a_rule_is_stored_as_plain_json_and_reads_back_the_same() {
        let original = SmartRule {
            limit: Some(5),
            sort: SmartSort::LastPlayed,
            ..rule(vec![
                text(SmartField::Format, SmartOp::Is, "flac"),
                empty(SmartField::Rating),
            ])
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"matchAll\":true"));
        assert_eq!(serde_json::from_str::<SmartRule>(&json).unwrap(), original);
    }

    #[test]
    fn the_formats_in_the_library_can_be_listed() {
        let (_dir, db) = library(&[
            Track {
                codec: "mp3",
                ..TRACK
            },
            Track {
                codec: "flac",
                ..TRACK
            },
            Track {
                codec: "mp3",
                ..TRACK
            },
            Track {
                codec: "aac",
                missing: true,
                ..TRACK
            },
        ]);
        let found = db.read(|conn| Ok::<_, DbError>(formats(conn)?)).unwrap();
        assert_eq!(found, ["flac", "mp3"]);
    }

    #[test]
    fn a_playlist_can_be_filled_in_the_rules_order() {
        let (_dir, db) = library(&[
            Track {
                title: "B",
                ..TRACK
            },
            Track {
                title: "A",
                ..TRACK
            },
            Track {
                title: "C",
                ..TRACK
            },
        ]);
        let two = SmartRule {
            limit: Some(2),
            ..rule(vec![])
        };
        let filled: Vec<(i64, String)> = db
            .write(|conn| {
                conn.execute_batch(
                    "INSERT INTO playlists (id, public_id, user_id, name, kind, created_at, updated_at)
                     VALUES (9, 'p', 1, 'P', 'manual', 0, 0);",
                )?;
                insert_into(conn, 9, &two, NOW, 1024)?;
                let mut statement = conn.prepare(
                    "SELECT i.position, t.title FROM playlist_items i
                     JOIN tracks t ON t.id = i.track_id ORDER BY i.position",
                )?;
                let rows = statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .unwrap();
        assert_eq!(filled, [(1024, "A".to_owned()), (2048, "B".to_owned())]);
    }
}
