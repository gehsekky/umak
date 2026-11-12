//! The settings store. Each setting is declared once here, with its type, its default and
//! the values it allows. Values are kept as JSON in the `settings` table. A value that is
//! missing reads as the default, and so does one that makes no sense, along with what was
//! wrong with it. Keys nobody declares, such as those from a newer app's export, are kept as
//! they are.

use std::collections::HashMap;
use std::sync::LazyLock;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};
use ts_rs::TS;

use crate::api::{ApiError, ErrorCode};
use crate::player;

/// What a setting holds, which also says how to show it.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SettingKind {
    Toggle,
    /// A value in range, which is brought into range when it is not.
    Number {
        min: f64,
        max: f64,
        step: f64,
        /// Shown after the value, such as "dB".
        unit: Option<String>,
    },
    Choice {
        options: Vec<SettingOption>,
    },
    /// Has a control of its own, such as the equalizer's bands.
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct SettingOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SettingDefinition {
    pub key: String,
    /// The heading it is listed under. Resetting a section resets the settings under it.
    pub section: String,
    pub label: String,
    pub help: String,
    pub kind: SettingKind,
    #[ts(type = "unknown")]
    pub default: Value,
    /// Set somewhere else, such as the volume on the player bar, so not listed with the rest.
    pub hidden: bool,
}

/// A setting's value as the app uses it.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SettingValue {
    pub key: String,
    #[ts(type = "unknown")]
    pub value: Value,
    pub is_default: bool,
    /// Why the saved value is not used, when it made no sense.
    pub problem: Option<String>,
}

/// Checks a value for a custom setting, bringing it into shape where it can.
type Check = fn(&Value) -> Result<Value, String>;

struct Declared {
    definition: SettingDefinition,
    check: Option<Check>,
}

impl Declared {
    fn new(
        key: &str,
        section: &str,
        label: &str,
        help: &str,
        kind: SettingKind,
        default: Value,
    ) -> Self {
        Self {
            definition: SettingDefinition {
                key: key.to_owned(),
                section: section.to_owned(),
                label: label.to_owned(),
                help: help.to_owned(),
                kind,
                default,
                hidden: false,
            },
            check: None,
        }
    }

    fn hidden(mut self) -> Self {
        self.definition.hidden = true;
        self
    }

    fn checked_by(mut self, check: Check) -> Self {
        self.check = Some(check);
        self
    }

    /// The value in the form it is stored and used, or what is wrong with it.
    fn check(&self, value: &Value) -> Result<Value, String> {
        match &self.definition.kind {
            SettingKind::Toggle => value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| "needs to be on or off".to_owned()),
            SettingKind::Number { min, max, .. } => value
                .as_f64()
                .filter(|number| number.is_finite())
                .map(|number| json!(number.clamp(*min, *max)))
                .ok_or_else(|| "needs a number".to_owned()),
            SettingKind::Choice { options } => value
                .as_str()
                .filter(|chosen| options.iter().any(|option| option.value == *chosen))
                .map(|chosen| Value::String(chosen.to_owned()))
                .ok_or_else(|| {
                    let names: Vec<&str> =
                        options.iter().map(|option| option.value.as_str()).collect();
                    format!("needs one of {}", names.join(", "))
                }),
            SettingKind::Custom => match self.check {
                Some(check) => check(value),
                None => Ok(value.clone()),
            },
        }
    }
}

fn number(min: f64, max: f64, step: f64, unit: Option<&str>) -> SettingKind {
    SettingKind::Number {
        min,
        max,
        step,
        unit: unit.map(str::to_owned),
    }
}

fn choice(options: &[(&str, &str)]) -> SettingKind {
    SettingKind::Choice {
        options: options
            .iter()
            .map(|(value, label)| SettingOption {
                value: (*value).to_owned(),
                label: (*label).to_owned(),
            })
            .collect(),
    }
}

const AUDIO: &str = "Audio";

/// Every setting, in the order it is listed.
static DECLARED: LazyLock<Vec<Declared>> = LazyLock::new(|| {
    vec![
        Declared::new(
            player::VOLUME_KEY,
            AUDIO,
            "Volume",
            "The output level, from silent to full.",
            number(0.0, 1.0, 0.01, None),
            json!(1.0),
        )
        .hidden(),
        Declared::new(
            player::MUTED_KEY,
            AUDIO,
            "Muted",
            "Silent, keeping the volume to come back to.",
            SettingKind::Toggle,
            json!(false),
        )
        .hidden(),
        Declared::new(
            player::REPLAY_GAIN_KEY,
            AUDIO,
            "ReplayGain",
            "Evens out loudness using the values in your files' tags. Album keeps the \
             differences between songs on an album, and Track evens out every song. Off plays \
             files exactly as they are.",
            choice(&[("off", "Off"), ("album", "Album"), ("track", "Track")]),
            json!("off"),
        ),
        Declared::new(
            player::PREAMP_KEY,
            AUDIO,
            "Preamp",
            "Added on top of ReplayGain, including on tracks without values. It does not apply \
             when ReplayGain is off.",
            number(
                -f64::from(player::MAX_PREAMP_DB),
                f64::from(player::MAX_PREAMP_DB),
                0.5,
                Some("dB"),
            ),
            json!(0.0),
        ),
        Declared::new(
            player::EQ_KEY,
            AUDIO,
            "Equalizer",
            "Boosting a band lowers everything by the same amount to make room, and a limiter \
             catches any peaks left, so nothing clips. When the equalizer is off and ReplayGain \
             is off, files play exactly as they are.",
            SettingKind::Custom,
            player::default_eq(),
        )
        .checked_by(player::check_eq),
    ]
});

fn declared(key: &str) -> Result<&'static Declared, ApiError> {
    DECLARED
        .iter()
        .find(|declared| declared.definition.key == key)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("there is no setting {key}")))
}

pub fn definitions() -> Vec<SettingDefinition> {
    DECLARED
        .iter()
        .map(|declared| declared.definition.clone())
        .collect()
}

/// A stored value as JSON. Values from before the store were sometimes plain text.
fn parse_stored(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// The value to use for a setting, given what is stored for it.
fn resolve(declared: &Declared, stored: Option<&str>) -> SettingValue {
    let definition = &declared.definition;
    let (value, problem) = match stored.map(|raw| (raw, declared.check(&parse_stored(raw)))) {
        None => (definition.default.clone(), None),
        Some((_, Ok(value))) => (value, None),
        Some((raw, Err(reason))) => {
            let shown: String = raw.chars().take(40).collect();
            let shown = if shown.len() < raw.len() {
                format!("{shown}...")
            } else {
                shown
            };
            (
                definition.default.clone(),
                Some(format!(
                    "The saved value {shown} {reason}, so the default is used."
                )),
            )
        }
    };
    SettingValue {
        key: definition.key.clone(),
        is_default: value == definition.default,
        value,
        problem,
    }
}

fn stored(conn: &Connection, key: &str) -> Result<Option<String>, ApiError> {
    Ok(conn
        .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
        .query_row([key], |row| row.get(0))
        .optional()?)
}

pub fn read(conn: &Connection, key: &str) -> Result<SettingValue, ApiError> {
    let declared = declared(key)?;
    Ok(resolve(declared, stored(conn, key)?.as_deref()))
}

/// The value to use for a setting.
pub fn value(conn: &Connection, key: &str) -> Result<Value, ApiError> {
    read(conn, key).map(|setting| setting.value)
}

/// The value to use for a setting, with some values taken as already changed.
pub fn value_with(
    conn: &Connection,
    key: &str,
    changed: &[(&str, &Value)],
) -> Result<Value, ApiError> {
    match changed.iter().find(|(changed, _)| *changed == key) {
        Some((_, value)) => Ok((*value).clone()),
        None => value(conn, key),
    }
}

/// Every declared setting's value, in the order they are declared.
pub fn all(conn: &Connection) -> Result<Vec<SettingValue>, ApiError> {
    let mut saved: HashMap<String, String> = conn
        .prepare("SELECT key, value FROM settings")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(DECLARED
        .iter()
        .map(|declared| resolve(declared, saved.remove(&declared.definition.key).as_deref()))
        .collect())
}

/// Checks a value for a setting, giving it in the form it is stored and used.
pub fn check(key: &str, value: &Value) -> Result<Value, ApiError> {
    let declared = declared(key)?;
    declared.check(value).map_err(|reason| {
        ApiError::new(
            ErrorCode::InvalidArgs,
            format!("{} {reason}", declared.definition.label),
        )
    })
}

/// Checks and saves a value.
pub fn write(conn: &Connection, key: &str, value: &Value) -> Result<SettingValue, ApiError> {
    let value = check(key, value)?;
    conn.prepare_cached(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )?
    .execute([key, &value.to_string()])?;
    read(conn, key)
}

/// Puts a setting back to its default.
pub fn reset(conn: &Connection, key: &str) -> Result<SettingValue, ApiError> {
    declared(key)?;
    conn.prepare_cached("DELETE FROM settings WHERE key = ?1")?
        .execute([key])?;
    read(conn, key)
}

/// Settings that are not declared, but kept for a version of the app that knows them.
#[cfg(test)]
fn undeclared(conn: &Connection) -> Vec<String> {
    let mut keys: Vec<String> = conn
        .prepare("SELECT key FROM settings ORDER BY key")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    keys.retain(|key| declared(key).is_err());
    keys
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::db::{Database, DbError};

    fn open() -> (TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        (dir, db)
    }

    fn save_raw(db: &Database, key: &str, raw: &str) {
        db.write(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [key, raw],
            )
            .map_err(DbError::from)
        })
        .unwrap();
    }

    #[test]
    fn every_declared_default_passes_its_own_check() {
        let mut keys = Vec::new();
        for declared in DECLARED.iter() {
            let definition = &declared.definition;
            assert_eq!(
                declared.check(&definition.default).as_ref(),
                Ok(&definition.default),
                "{}",
                definition.key
            );
            assert!(!keys.contains(&&definition.key), "{} twice", definition.key);
            keys.push(&definition.key);
        }
    }

    #[test]
    fn missing_values_read_as_the_default() {
        let (_dir, db) = open();
        let values = db.read(all).unwrap();
        assert_eq!(values.len(), DECLARED.len());
        assert!(
            values
                .iter()
                .all(|setting| setting.is_default && setting.problem.is_none())
        );
    }

    #[test]
    fn values_are_checked_brought_into_range_and_saved_as_json() {
        let (_dir, db) = open();
        let saved = db
            .write(|conn| write(conn, player::PREAMP_KEY, &json!(40)))
            .unwrap();
        assert_eq!(saved.value, json!(12.0));
        assert!(!saved.is_default);
        let raw: String = db
            .read(|conn| {
                conn.query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [player::PREAMP_KEY],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(raw, "12.0");

        let refused =
            |key: &str, value: Value| db.write(|conn| write(conn, key, &value)).unwrap_err().code;
        assert_eq!(
            refused(player::REPLAY_GAIN_KEY, json!("loud")),
            ErrorCode::InvalidArgs
        );
        assert_eq!(refused(player::MUTED_KEY, json!(1)), ErrorCode::InvalidArgs);
        assert_eq!(refused(player::EQ_KEY, json!({})), ErrorCode::InvalidArgs);
        assert_eq!(refused("no.such", json!(1)), ErrorCode::NotFound);
        assert_eq!(
            db.read(|conn| value(conn, player::REPLAY_GAIN_KEY))
                .unwrap(),
            json!("off")
        );
    }

    #[test]
    fn bad_saved_values_fall_back_to_the_default_and_say_why() {
        let (_dir, db) = open();
        save_raw(&db, player::REPLAY_GAIN_KEY, "maybe");
        save_raw(&db, player::VOLUME_KEY, "{not json");
        let replay_gain = db.read(|conn| read(conn, player::REPLAY_GAIN_KEY)).unwrap();
        assert_eq!(replay_gain.value, json!("off"));
        assert!(replay_gain.is_default);
        assert_eq!(
            replay_gain.problem.as_deref(),
            Some("The saved value maybe needs one of off, album, track, so the default is used.")
        );
        let volume = db.read(|conn| read(conn, player::VOLUME_KEY)).unwrap();
        assert_eq!(volume.value, json!(1.0));
        assert!(volume.problem.unwrap().contains("needs a number"));
    }

    #[test]
    fn values_saved_before_the_store_keep_their_meaning() {
        let (_dir, db) = open();
        // How the player saved them: numbers and flags as text, the mode as a bare word.
        save_raw(&db, player::VOLUME_KEY, "0.25");
        save_raw(&db, player::MUTED_KEY, "true");
        save_raw(&db, player::REPLAY_GAIN_KEY, "album");
        save_raw(&db, player::PREAMP_KEY, "1");
        let values = db.read(all).unwrap();
        let value = |key: &str| {
            values
                .iter()
                .find(|setting| setting.key == key)
                .unwrap()
                .clone()
        };
        assert_eq!(value(player::VOLUME_KEY).value, json!(0.25));
        assert_eq!(value(player::MUTED_KEY).value, json!(true));
        assert_eq!(value(player::REPLAY_GAIN_KEY).value, json!("album"));
        assert_eq!(value(player::PREAMP_KEY).value, json!(1.0));
        assert!(values.iter().all(|setting| setting.problem.is_none()));
    }

    #[test]
    fn resetting_removes_the_value_and_keeps_settings_nobody_declares() {
        let (_dir, db) = open();
        save_raw(&db, "newer.app.setting", "[1,2]");
        db.write(|conn| write(conn, player::REPLAY_GAIN_KEY, &json!("track")))
            .unwrap();
        let back = db
            .write(|conn| reset(conn, player::REPLAY_GAIN_KEY))
            .unwrap();
        assert!(back.is_default);
        assert_eq!(
            db.read(|conn| Ok::<_, DbError>(undeclared(conn))).unwrap(),
            ["newer.app.setting"]
        );
        assert_eq!(
            db.write(|conn| reset(conn, "newer.app.setting"))
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
}
