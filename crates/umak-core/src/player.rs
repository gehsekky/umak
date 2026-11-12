//! The player client. The core owns the player playlist in the database and talks to
//! an engine through `PlayerBackend`, so where the engine runs can change without
//! changing callers. Every way of playing something replaces that one list.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;
use umak_audio::protocol::{
    EQ_BANDS, EQ_FREQUENCIES, PlaybackState, PlayerCommand, PlayerEvent, ReplayGainMode,
    TrackEntry, VisualFrame,
};

use crate::api::{ApiError, CoreEvent, ErrorCode, EventSink};
use crate::db::{Database, DbError};
use crate::library::{TRACK_COLUMNS, TrackRow};
use crate::playlists::{self, PlayerEdit, PlaylistPage};
use crate::settings;
use crate::user_data::{self, PlayRule, PlayTracker};

/// How much playback can be lost if the app ends without warning.
const SAVE_POSITION_EVERY_MS: u64 = 5000;
/// Written next to the database when playback has a problem, so a stutter leaves
/// evidence behind. Each line is tab-separated.
const HEALTH_LOG_NAME: &str = "player-health.log";
const MAX_HEALTH_LOG_BYTES: u64 = 256 * 1024;
/// A device normally asks for audio every 10 ms or so. This is well past that.
const STALL_US: u64 = 100_000;
/// Every setting the player follows starts with this.
const SETTINGS_PREFIX: &str = "player.";
pub(crate) const VOLUME_KEY: &str = "player.volume";
pub(crate) const MUTED_KEY: &str = "player.muted";
pub(crate) const REPLAY_GAIN_KEY: &str = "player.replayGain";
pub(crate) const PREAMP_KEY: &str = "player.preampDb";
/// The preamp's range in decibels.
pub(crate) const MAX_PREAMP_DB: f32 = 12.0;
pub(crate) const EQ_KEY: &str = "player.eq";
/// A device's name, or empty for the system's default.
pub(crate) const OUTPUT_DEVICE_KEY: &str = "player.outputDevice";
/// How far Skip forward and Skip back move. The system's media controls read it too.
pub const SKIP_KEY: &str = "player.skipSeconds";
pub(crate) const PREVIOUS_RESTART_KEY: &str = "player.previousRestartSeconds";
pub(crate) const PLAY_SHARE_KEY: &str = "player.playCountPercent";
pub(crate) const PLAY_MINUTES_KEY: &str = "player.playCountMinutes";
pub(crate) const RESUME_KEY: &str = "player.resumeAtStart";
/// An output failing again this soon after the last was replaced leaves playback stopped,
/// rather than starting engine after engine.
const REPLACE_AGAIN_AFTER: Duration = Duration::from_secs(10);
/// Saved presets are the player's own data, kept beside the settings but not one of them.
const EQ_PRESETS_KEY: &str = "player.eqPresets";
/// The range of an equalizer band in decibels.
const MAX_BAND_DB: f32 = 12.0;
/// Presets that come with the app, which cannot be changed or removed.
const BUILT_IN_PRESETS: [(&str, [f32; EQ_BANDS]); 5] = [
    ("Flat", [0.0; EQ_BANDS]),
    (
        "Bass boost",
        [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Treble boost",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 4.0, 5.0, 6.0],
    ),
    (
        "Vocal",
        [-2.0, -2.0, -1.0, 0.0, 2.0, 3.0, 3.0, 2.0, 0.0, -1.0],
    ),
    (
        "Smile",
        [5.0, 4.0, 2.0, 0.0, -1.0, -1.0, 0.0, 2.0, 4.0, 5.0],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PlayerAction {
    /// Resumes, or restarts the last session's list if nothing is loaded.
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlayerStatus {
    pub state: PlaybackState,
    pub track: Option<TrackRow>,
    pub index: Option<u32>,
    pub count: u32,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub device: Option<String>,
}

impl Default for PlayerStatus {
    fn default() -> Self {
        Self {
            state: PlaybackState::Stopped,
            track: None,
            index: None,
            count: 0,
            position_ms: 0,
            duration_ms: None,
            device: None,
        }
    }
}

/// How playback is leveled. These are saved, and each new engine starts with them.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSettings {
    /// Output gain from 0 to 1.
    pub volume: f32,
    /// Silent, while keeping the volume to come back to.
    pub muted: bool,
    pub replay_gain: ReplayGainMode,
    /// Decibels on top of the ReplayGain value. Applies unless ReplayGain is off.
    pub preamp_db: f32,
    pub eq: EqSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct EqSettings {
    pub enabled: bool,
    /// Decibels per band, in the order of the frequencies in `EqPresets`.
    pub bands: Vec<f32>,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bands: vec![0.0; EQ_BANDS],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EqPreset {
    pub name: String,
    pub bands: Vec<f32>,
    /// Comes with the app, so it cannot be changed or removed.
    #[serde(default)]
    pub built_in: bool,
}

/// The equalizer's bands, and the presets that can fill them.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct EqPresets {
    pub frequencies: Vec<u32>,
    pub presets: Vec<EqPreset>,
}

/// Checks a set of bands and brings each into range.
fn clean_bands(bands: &[f32]) -> Result<[f32; EQ_BANDS], ApiError> {
    let bands: [f32; EQ_BANDS] = bands.try_into().map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidArgs,
            format!("the equalizer has {EQ_BANDS} bands"),
        )
    })?;
    if bands.iter().any(|db| !db.is_finite()) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgs,
            "an equalizer band needs a number",
        ));
    }
    Ok(bands.map(|db| db.clamp(-MAX_BAND_DB, MAX_BAND_DB)))
}

pub(crate) fn default_eq() -> Value {
    serde_json::to_value(EqSettings::default()).unwrap_or_default()
}

/// Checks the equalizer setting, bringing each band into range.
pub(crate) fn check_eq(value: &Value) -> Result<Value, String> {
    let eq = serde_json::from_value::<EqSettings>(value.clone())
        .map_err(|_| "needs to say whether it is on and give a level for each band".to_owned())?;
    let bands = clean_bands(&eq.bands).map_err(|error| error.message)?;
    serde_json::to_value(EqSettings {
        enabled: eq.enabled,
        bands: bands.to_vec(),
    })
    .map_err(|error| error.to_string())
}

/// Whether the player follows a setting.
pub(crate) fn is_player_setting(key: &str) -> bool {
    key.starts_with(SETTINGS_PREFIX) && key != EQ_PRESETS_KEY
}

/// A number as it was given, without the digits a wider type would add.
fn json_number(number: f32) -> Value {
    json!(number.to_string().parse::<f64>().unwrap_or_default())
}

fn saved_presets(conn: &Connection) -> Result<Vec<EqPreset>, ApiError> {
    Ok(saved_setting(conn, EQ_PRESETS_KEY)?
        .and_then(|value| serde_json::from_str::<Vec<EqPreset>>(&value).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|preset| clean_bands(&preset.bands).is_ok())
        .map(|preset| EqPreset {
            built_in: false,
            ..preset
        })
        .collect())
}

fn write_presets(conn: &Connection, presets: &[EqPreset]) -> Result<(), ApiError> {
    let value = serde_json::to_string(presets)
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
    save_setting(conn, EQ_PRESETS_KEY, &value)
}

fn all_presets(conn: &Connection) -> Result<EqPresets, ApiError> {
    let mut presets: Vec<EqPreset> = BUILT_IN_PRESETS
        .iter()
        .map(|(name, bands)| EqPreset {
            name: (*name).to_owned(),
            bands: bands.to_vec(),
            built_in: true,
        })
        .collect();
    let mut saved = saved_presets(conn)?;
    saved.sort_by_key(|preset| preset.name.to_lowercase());
    presets.extend(saved);
    Ok(EqPresets {
        frequencies: EQ_FREQUENCIES.to_vec(),
        presets,
    })
}

fn is_built_in(name: &str) -> bool {
    BUILT_IN_PRESETS
        .iter()
        .any(|(built_in, _)| built_in.eq_ignore_ascii_case(name))
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            replay_gain: ReplayGainMode::Off,
            preamp_db: 0.0,
            eq: EqSettings::default(),
        }
    }
}

/// How much of the library has ReplayGain tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GainCoverage {
    pub tracks: u32,
    pub with_track_gain: u32,
    pub with_album_gain: u32,
}

fn saved_setting(conn: &Connection, key: &str) -> Result<Option<String>, ApiError> {
    Ok(conn
        .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
        .query_row([key], |row| row.get(0))
        .optional()?)
}

fn save_setting(conn: &Connection, key: &str, value: &str) -> Result<(), ApiError> {
    conn.prepare_cached(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )?
    .execute([key, value])?;
    Ok(())
}

/// What the player does, as against how it sounds. Views read these from the settings store.
#[derive(Debug, Clone, PartialEq)]
struct Behavior {
    /// None for the system's default.
    device: Option<String>,
    restart_threshold_ms: u64,
    play_rule: PlayRule,
}

fn load_behavior(conn: &Connection) -> Result<Behavior, ApiError> {
    load_behavior_with(conn, &[])
}

fn load_behavior_with(conn: &Connection, changed: &[(&str, &Value)]) -> Result<Behavior, ApiError> {
    let value = |key: &str| settings::value_with(conn, key, changed);
    let number =
        |key: &str| -> Result<f64, ApiError> { Ok(value(key)?.as_f64().unwrap_or_default()) };
    Ok(Behavior {
        device: value(OUTPUT_DEVICE_KEY)?
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        restart_threshold_ms: (number(PREVIOUS_RESTART_KEY)? * 1000.0) as u64,
        play_rule: PlayRule {
            share: number(PLAY_SHARE_KEY)? / 100.0,
            enough_ms: (number(PLAY_MINUTES_KEY)? * 60_000.0) as u64,
        },
    })
}

/// Reads the settings from the store, which has already checked them.
fn load_settings(conn: &Connection) -> Result<PlayerSettings, ApiError> {
    load_settings_with(conn, &[])
}

/// Reads the settings, with some taken as already changed.
fn load_settings_with(
    conn: &Connection,
    changed: &[(&str, &Value)],
) -> Result<PlayerSettings, ApiError> {
    let value = |key: &str| settings::value_with(conn, key, changed);
    let number = |key: &str| -> Result<f32, ApiError> {
        Ok(value(key)?.as_f64().unwrap_or_default() as f32)
    };
    let replay_gain = match value(REPLAY_GAIN_KEY)?.as_str() {
        Some("track") => ReplayGainMode::Track,
        Some("album") => ReplayGainMode::Album,
        _ => ReplayGainMode::Off,
    };
    Ok(PlayerSettings {
        volume: number(VOLUME_KEY)?,
        muted: value(MUTED_KEY)?.as_bool().unwrap_or(false),
        replay_gain,
        preamp_db: number(PREAMP_KEY)?,
        eq: serde_json::from_value(value(EQ_KEY)?).unwrap_or_default(),
    })
}

fn clamp_preamp(preamp_db: f32) -> f32 {
    preamp_db.clamp(-MAX_PREAMP_DB, MAX_PREAMP_DB)
}

/// What the engine should play at.
fn audible_volume(settings: &PlayerSettings) -> f32 {
    if settings.muted { 0.0 } else { settings.volume }
}

fn gain_command(settings: &PlayerSettings) -> PlayerCommand {
    PlayerCommand::SetGain {
        replay_gain: settings.replay_gain,
        preamp_db: settings.preamp_db,
    }
}

fn eq_command(eq: &EqSettings) -> Result<PlayerCommand, ApiError> {
    Ok(PlayerCommand::SetEq {
        enabled: eq.enabled,
        bands_db: clean_bands(&eq.bands)?,
    })
}

/// Every setting an engine follows, as commands, which a new engine gets before anything else.
fn engine_commands(
    settings: &PlayerSettings,
    behavior: &Behavior,
) -> Result<Vec<PlayerCommand>, ApiError> {
    Ok(vec![
        PlayerCommand::SetVolume {
            volume: audible_volume(settings),
        },
        gain_command(settings),
        eq_command(&settings.eq)?,
        PlayerCommand::SetRestartThreshold {
            ms: behavior.restart_threshold_ms,
        },
    ])
}

type Backend = Arc<Mutex<Option<Box<dyn PlayerBackend>>>>;

/// Has a running engine take up every setting. A later engine starts with them.
fn tell_running_engine_all(backend: &Backend, settings: &PlayerSettings, behavior: &Behavior) {
    if let Some(running) = lock(backend).as_mut()
        && let Ok(commands) = engine_commands(settings, behavior)
    {
        for command in &commands {
            let _ = running.send(command);
        }
    }
}

/// An engine that takes commands. Its events go to the callback it was created with.
pub trait PlayerBackend: Send {
    fn send(&mut self, command: &PlayerCommand) -> Result<(), String>;

    /// Starts or stops visual frames. A backend without them ignores this.
    fn set_visuals(&mut self, _enabled: bool) {}
}

pub type BackendEvents = Arc<dyn Fn(BackendSignal) + Send + Sync>;
/// Starts an engine on the output device named, or on the system's default for none or for
/// a device that is not there. Its `Ready` event names the device it opened.
pub type BackendFactory =
    Box<dyn FnMut(BackendEvents, Option<&str>) -> Result<Box<dyn PlayerBackend>, String> + Send>;

pub enum BackendSignal {
    Event(PlayerEvent),
    /// The engine ended without being asked to.
    Exited,
    /// What the output sounds like now, while visuals are on.
    Visuals(VisualFrame),
}

/// For a core that does not play, such as a headless tool.
pub fn no_engine(
    _events: BackendEvents,
    _device: Option<&str>,
) -> Result<Box<dyn PlayerBackend>, String> {
    Err("this build has no audio engine".to_owned())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// State the backend's callback shares with the player.
struct Shared {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    status: Mutex<PlayerStatus>,
    /// Whether the engine holds a list. If not, Play restores the saved one.
    loaded: AtomicBool,
    saved_position_ms: Mutex<u64>,
    seen_underruns: Mutex<u64>,
    plays: Mutex<PlayTracker>,
    /// Whether a visualizer is open. Not saved: each opens and closes its own.
    visuals: AtomicBool,
    /// The device the running engine was asked for, None for the system's default.
    wanted_device: Mutex<Option<String>>,
    /// What went wrong with the last output, until the engine replacing it is ready.
    lost_output: Mutex<Option<String>>,
    /// When an engine was last replaced because its output failed.
    last_replaced: Mutex<Option<Instant>>,
}

impl Shared {
    /// Records an underrun or a device stall. Failing to log must never affect playback.
    fn log_health(&self, underruns: u64, queued_ms: u64, longest_gap_us: u64) {
        let new_underruns = {
            let mut seen = lock(&self.seen_underruns);
            let new = underruns.saturating_sub(*seen);
            *seen = underruns;
            new
        };
        if new_underruns == 0 && longest_gap_us < STALL_US {
            return;
        }
        let path = self.db.path().with_file_name(HEALTH_LOG_NAME);
        if fs::metadata(&path).is_ok_and(|file| file.len() > MAX_HEALTH_LOG_BYTES) {
            let _ = fs::remove_file(&path);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let status = self.status_snapshot();
        let track = status.track.map_or_else(String::new, |track| track.title);
        let line = format!(
            "{now}\tposition_ms={}\tnew_underruns={new_underruns}\tqueued_ms={queued_ms}\tlongest_gap_us={longest_gap_us}\t{track}\n",
            status.position_ms
        );
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn status_snapshot(&self) -> PlayerStatus {
        lock(&self.status).clone()
    }

    fn track_row(&self, public_id: &str) -> Option<TrackRow> {
        self.db
            .read(|conn| {
                let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks AS t WHERE t.public_id = ?1");
                conn.prepare_cached(&sql)?
                    .query_row([public_id], |row| TrackRow::from_row(row, 0))
                    .optional()
                    .map_err(DbError::from)
            })
            .ok()
            .flatten()
    }

    /// Reads the current track's details again, in case they changed.
    fn refresh_track(&self) {
        let Some(current) = self.status_snapshot().track else {
            return;
        };
        let Some(track) = self.track_row(&current.id) else {
            return;
        };
        if track == current {
            return;
        }
        let status = {
            let mut status = lock(&self.status);
            // The engine may have moved on while this was being read.
            if status.track.as_ref().is_none_or(|now| now.id != track.id) {
                return;
            }
            status.track = Some(track);
            status.clone()
        };
        self.events.emit(&CoreEvent::PlayerChanged {
            status: Box::new(status),
        });
    }

    /// Counts a play. Failing to must never affect playback.
    fn record_play(&self, track_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64);
        let recorded = self.db.write(|conn| {
            let tx = conn.transaction()?;
            user_data::record_play(&tx, track_id, now)?;
            tx.commit()?;
            Ok::<_, DbError>(())
        });
        if let Err(error) = recorded {
            eprintln!("failed to record a play: {error}");
            return;
        }
        self.events.emit(&CoreEvent::LibraryChanged);
        self.refresh_track();
    }

    fn save_position(&self, index: Option<u32>, position_ms: u64) {
        *lock(&self.saved_position_ms) = position_ms;
        let saved = self.db.write(|conn| {
            playlists::save_player_position(conn, index, position_ms)?;
            Ok::<_, ApiError>(())
        });
        if let Err(error) = saved {
            eprintln!("failed to save the player position: {error}");
        }
    }

    /// Takes in what an engine says, returning true if its output failed and it needs replacing.
    fn handle(&self, signal: BackendSignal) -> bool {
        if let BackendSignal::Event(PlayerEvent::OutputFailed { message }) = signal {
            *lock(&self.lost_output) = Some(message);
            return true;
        }
        match signal {
            BackendSignal::Exited => {
                self.loaded.store(false, Ordering::SeqCst);
                let status = {
                    let mut status = lock(&self.status);
                    status.state = PlaybackState::Stopped;
                    status.clone()
                };
                self.events.emit(&CoreEvent::PlayerError {
                    message: "the player stopped unexpectedly".to_owned(),
                });
                self.events.emit(&CoreEvent::PlayerChanged {
                    status: Box::new(status),
                });
            }
            BackendSignal::Event(event) => self.handle_event(event),
            BackendSignal::Visuals(frame) => {
                self.events.emit(&CoreEvent::VisualFrame { frame });
            }
        }
        false
    }

    /// Notes the device an engine opened, saying why if it is not the one asked for.
    fn opened(&self, device: String) {
        let wanted = lock(&self.wanted_device).clone();
        let lost = lock(&self.lost_output).take();
        let (status, message) = {
            let mut status = lock(&self.status);
            let before = status.device.replace(device.clone());
            let message = match (lost, wanted) {
                (Some(_), _) if before.as_deref() == Some(device.as_str()) => Some(format!(
                    "{device} stopped working for a moment and was opened again."
                )),
                (Some(lost), _) => Some(format!("{lost}, so playback moved to {device}.")),
                (None, Some(wanted)) if wanted != device => Some(format!(
                    "{wanted} was not found, so {device} is used until it is back and chosen again."
                )),
                _ => None,
            };
            (status.clone(), message)
        };
        if let Some(message) = message {
            self.events.emit(&CoreEvent::PlayerError { message });
        }
        self.events.emit(&CoreEvent::PlayerChanged {
            status: Box::new(status),
        });
    }

    fn handle_event(&self, event: PlayerEvent) {
        match event {
            PlayerEvent::Ready { device, .. } => self.opened(device),
            PlayerEvent::State {
                state,
                index,
                track_id,
                position_ms,
                duration_ms,
            } => {
                lock(&self.plays).on_state(state, track_id.as_deref(), position_ms, duration_ms);
                let track = track_id.as_deref().and_then(|id| self.track_row(id));
                let index = index.map(|index| index as u32);
                let status = {
                    let mut status = lock(&self.status);
                    status.state = state;
                    status.track = track;
                    status.index = index;
                    status.position_ms = position_ms as i64;
                    status.duration_ms = duration_ms.map(|ms| ms as i64);
                    status.clone()
                };
                self.save_position(index, position_ms);
                self.events.emit(&CoreEvent::PlayerChanged {
                    status: Box::new(status),
                });
            }
            PlayerEvent::Position { position_ms } => {
                let index = {
                    let mut status = lock(&self.status);
                    status.position_ms = position_ms as i64;
                    status.index
                };
                let saved = *lock(&self.saved_position_ms);
                if position_ms.abs_diff(saved) >= SAVE_POSITION_EVERY_MS {
                    self.save_position(index, position_ms);
                }
                self.events.emit(&CoreEvent::PlayerPosition {
                    position_ms: position_ms as i64,
                });
                let played = lock(&self.plays).on_position(position_ms);
                if let Some(track) = played {
                    self.record_play(&track);
                }
            }
            PlayerEvent::Health {
                underruns,
                queued_ms,
                longest_gap_us,
            } => self.log_health(underruns, queued_ms, longest_gap_us),
            PlayerEvent::TrackFailed { id, message } => {
                let title = self
                    .track_row(&id)
                    .map_or_else(|| "a track".to_owned(), |track| track.title);
                self.events.emit(&CoreEvent::PlayerError {
                    message: format!("Could not play {title}: {message}"),
                });
            }
            PlayerEvent::Error { message } => {
                self.events.emit(&CoreEvent::PlayerError { message });
            }
            // Taken by `handle`, which has the engine replaced.
            PlayerEvent::OutputFailed { .. } => {}
        }
    }

    /// Shows playback as stopped, as when no engine holds the list.
    fn show_stopped(&self) {
        self.loaded.store(false, Ordering::SeqCst);
        let status = {
            let mut status = lock(&self.status);
            status.state = PlaybackState::Stopped;
            status.clone()
        };
        self.events.emit(&CoreEvent::PlayerChanged {
            status: Box::new(status),
        });
    }
}

/// What starting and replacing an engine takes. The engine's own events hold it weakly, so a
/// failed output can be replaced from there without keeping the player alive.
#[derive(Clone)]
struct Engines {
    shared: Arc<Shared>,
    backend: Backend,
    factory: Arc<Mutex<BackendFactory>>,
    /// Held across a change to the list and its message to the engine, because an edit
    /// only makes sense applied to the list before it.
    list: Arc<Mutex<()>>,
}

struct WeakEngines {
    shared: Weak<Shared>,
    backend: Weak<Mutex<Option<Box<dyn PlayerBackend>>>>,
    factory: Weak<Mutex<BackendFactory>>,
    list: Weak<Mutex<()>>,
}

impl WeakEngines {
    fn upgrade(&self) -> Option<Engines> {
        Some(Engines {
            shared: self.shared.upgrade()?,
            backend: self.backend.upgrade()?,
            factory: self.factory.upgrade()?,
            list: self.list.upgrade()?,
        })
    }
}

impl Engines {
    fn downgrade(&self) -> WeakEngines {
        WeakEngines {
            shared: Arc::downgrade(&self.shared),
            backend: Arc::downgrade(&self.backend),
            factory: Arc::downgrade(&self.factory),
            list: Arc::downgrade(&self.list),
        }
    }

    /// Starts an engine on the chosen device, with every setting, before anything plays.
    fn start(&self) -> Result<Box<dyn PlayerBackend>, ApiError> {
        let (settings, behavior) = self.shared.db.read(|conn: &Connection| {
            Ok::<_, ApiError>((load_settings(conn)?, load_behavior(conn)?))
        })?;
        lock(&self.shared.wanted_device).clone_from(&behavior.device);
        let shared = Arc::clone(&self.shared);
        let engines = self.downgrade();
        let callback: BackendEvents = Arc::new(move |signal| {
            if shared.handle(signal)
                && let Some(engines) = engines.upgrade()
            {
                // Not on this thread, which belongs to the engine being replaced.
                thread::spawn(move || engines.replace_failed());
            }
        });
        let mut started =
            (lock(&self.factory))(callback, behavior.device.as_deref()).map_err(Player::failure)?;
        for command in engine_commands(&settings, &behavior)? {
            started.send(&command).map_err(Player::failure)?;
        }
        started.set_visuals(self.shared.visuals.load(Ordering::SeqCst));
        Ok(started)
    }

    /// Sends a command, starting the engine if needed. If the engine turns out to have
    /// died, it starts a new one, which holds no list, so the caller must say so.
    fn send(&self, command: &PlayerCommand) -> Result<(), ApiError> {
        let mut backend = lock(&self.backend);
        if let Some(running) = backend.as_mut() {
            if running.send(command).is_ok() {
                return Ok(());
            }
            *backend = None;
            self.shared.loaded.store(false, Ordering::SeqCst);
        }
        let mut started = self.start()?;
        started.send(command).map_err(Player::failure)?;
        *backend = Some(started);
        Ok(())
    }

    fn load(
        &self,
        tracks: Vec<TrackEntry>,
        start: usize,
        start_ms: u64,
        paused: bool,
    ) -> Result<(), ApiError> {
        lock(&self.shared.status).count = tracks.len() as u32;
        self.send(&PlayerCommand::Load {
            tracks,
            start_index: start,
            start_ms,
            paused,
        })?;
        self.shared.loaded.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Loads the last session's list and position. The caller holds the list.
    fn restore(&self, paused: bool) -> Result<(), ApiError> {
        let (tracks, index, position_ms) = self.shared.db.read(|conn: &Connection| {
            let tracks = playlists::player_entries(conn)?;
            let (index, position_ms) = playlists::player_position(conn)?;
            Ok::<_, ApiError>((tracks, index, position_ms))
        })?;
        if tracks.is_empty() {
            return Ok(());
        }
        let (start, start_ms) = match index {
            Some(index) if (index as usize) < tracks.len() => (index as usize, position_ms),
            _ => (0, 0),
        };
        self.load(tracks, start, start_ms, paused)
    }

    /// Replaces the engine, as when the output device changes, carrying on from where
    /// playback was, paused if it was paused. With nothing playing, a running engine is
    /// replaced at once, so a device that is not there is reported now.
    fn replace(&self) {
        let _list = lock(&self.list);
        let old = lock(&self.backend).take();
        let had_engine = old.is_some();
        // Ends the old engine, which can take a moment, before another opens the device.
        drop(old);
        let status = self.shared.status_snapshot();
        let was_loaded = self.shared.loaded.swap(false, Ordering::SeqCst);
        let result = if was_loaded && status.state != PlaybackState::Stopped {
            // The position saved as it played is where to carry on from.
            self.shared
                .save_position(status.index, u64::try_from(status.position_ms).unwrap_or(0));
            self.restore(status.state == PlaybackState::Paused)
        } else if had_engine {
            self.start()
                .map(|started| *lock(&self.backend) = Some(started))
        } else {
            Ok(())
        };
        if let Err(error) = result {
            self.shared.events.emit(&CoreEvent::PlayerError {
                message: error.message,
            });
            self.shared.show_stopped();
        }
    }

    /// Replaces an engine whose output failed, unless the last one failed only just now.
    fn replace_failed(&self) {
        let again = {
            let mut last = lock(&self.shared.last_replaced);
            let again = last.is_some_and(|at| at.elapsed() < REPLACE_AGAIN_AFTER);
            *last = Some(Instant::now());
            again
        };
        if !again {
            self.replace();
            return;
        }
        let lost = lock(&self.shared.lost_output).take();
        let _list = lock(&self.list);
        drop(lock(&self.backend).take());
        self.shared.events.emit(&CoreEvent::PlayerError {
            message: format!(
                "{}, again, so playback stopped. Play tries again.",
                lost.unwrap_or_else(|| "The output stopped working".to_owned())
            ),
        });
        self.shared.show_stopped();
    }
}

/// Where a saved place goes after an edit. If its entry was removed, the place moves to
/// the start of the entry that followed it, which is what playback would do.
fn moved_place(
    origin: &[Option<usize>],
    index: Option<u32>,
    position_ms: u64,
) -> (Option<u32>, u64) {
    let Some(index) = index.map(|index| index as usize) else {
        return (None, 0);
    };
    if let Some(new) = origin.iter().position(|from| *from == Some(index)) {
        return (Some(new as u32), position_ms);
    }
    let following = origin
        .iter()
        .enumerate()
        .filter(|(_, from)| from.is_some_and(|from| from > index))
        .min_by_key(|(_, from)| **from)
        .map(|(new, _)| new as u32);
    (following, 0)
}

pub struct Player {
    shared: Arc<Shared>,
    backend: Backend,
    factory: Arc<Mutex<BackendFactory>>,
    /// See `Engines`.
    list: Arc<Mutex<()>>,
}

impl Player {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, factory: BackendFactory) -> Self {
        let mut plays = PlayTracker::default();
        if let Ok(behavior) = db.read(load_behavior) {
            plays.set_rule(behavior.play_rule);
        }
        Self {
            shared: Arc::new(Shared {
                db,
                events,
                status: Mutex::default(),
                loaded: AtomicBool::new(false),
                saved_position_ms: Mutex::new(0),
                plays: Mutex::new(plays),
                seen_underruns: Mutex::new(0),
                visuals: AtomicBool::new(false),
                wanted_device: Mutex::default(),
                lost_output: Mutex::default(),
                last_replaced: Mutex::default(),
            }),
            backend: Arc::default(),
            factory: Arc::new(Mutex::new(factory)),
            list: Arc::default(),
        }
    }

    fn engines(&self) -> Engines {
        Engines {
            shared: Arc::clone(&self.shared),
            backend: Arc::clone(&self.backend),
            factory: Arc::clone(&self.factory),
            list: Arc::clone(&self.list),
        }
    }

    pub fn status(&self) -> PlayerStatus {
        lock(&self.shared.status).clone()
    }

    /// Something a background job can call when tracks have changed, so that the details shown
    /// for the current track do not go stale.
    pub fn track_refresher(&self) -> impl Fn() + Send + 'static + use<> {
        let shared = Arc::clone(&self.shared);
        move || shared.refresh_track()
    }

    fn failure(message: String) -> ApiError {
        ApiError::new(ErrorCode::Internal, message)
    }

    fn send(&self, command: &PlayerCommand) -> Result<(), ApiError> {
        self.engines().send(command)
    }

    fn load(&self, tracks: Vec<TrackEntry>, start: usize, start_ms: u64) -> Result<(), ApiError> {
        self.engines().load(tracks, start, start_ms, false)
    }

    fn checked_start(count: usize, start: u32) -> Result<usize, ApiError> {
        if count == 0 {
            return Err(ApiError::new(ErrorCode::InvalidArgs, "nothing to play"));
        }
        let start = start as usize;
        if start >= count {
            return Err(ApiError::new(
                ErrorCode::InvalidArgs,
                "the start position is past the end of the list",
            ));
        }
        Ok(start)
    }

    /// Replaces the player playlist with these tracks and plays from `start`.
    pub fn play_tracks(&self, track_ids: &[String], start: u32) -> Result<(), ApiError> {
        let _list = lock(&self.list);
        let start = Self::checked_start(track_ids.len(), start)?;
        let tracks = self.shared.db.write(|conn| {
            let tx = conn.transaction()?;
            playlists::set_player_tracks(&tx, track_ids)?;
            let tracks = playlists::player_entries(&tx)?;
            tx.commit()?;
            Ok::<_, ApiError>(tracks)
        })?;
        self.load(tracks, start, 0)?;
        self.shared
            .events
            .emit(&CoreEvent::QueueChanged { replaced: true });
        Ok(())
    }

    /// Copies a saved playlist into the player playlist and plays from `start`. Later
    /// edits to the player playlist do not change the saved one.
    pub fn play_playlist(&self, id: &str, start: u32) -> Result<(), ApiError> {
        let _list = lock(&self.list);
        let tracks = self.shared.db.write(|conn| {
            let tx = conn.transaction()?;
            playlists::copy_to_player(&tx, id)?;
            let tracks = playlists::player_entries(&tx)?;
            tx.commit()?;
            Ok::<_, ApiError>(tracks)
        })?;
        let start = Self::checked_start(tracks.len(), start)?;
        self.load(tracks, start, 0)?;
        self.shared
            .events
            .emit(&CoreEvent::QueueChanged { replaced: true });
        Ok(())
    }

    /// Replaces the player playlist with the album a track is on, from its start.
    pub fn play_album(&self, track_id: &str) -> Result<(), ApiError> {
        let tracks = self
            .shared
            .db
            .read(|conn| playlists::album_track_ids(conn, track_id))?;
        self.play_tracks(&tracks, 0)
    }

    /// Plays an entry of the player playlist as it stands.
    pub fn play_index(&self, index: u32) -> Result<(), ApiError> {
        let _list = lock(&self.list);
        let tracks = self.shared.db.read(playlists::player_entries)?;
        let start = Self::checked_start(tracks.len(), index)?;
        self.load(tracks, start, 0)
    }

    /// Where the current track is in the list, whether or not an engine holds it.
    fn current_index(&self, conn: &Connection) -> Result<Option<u32>, ApiError> {
        if self.shared.loaded.load(Ordering::SeqCst) {
            Ok(lock(&self.shared.status).index)
        } else {
            Ok(playlists::player_position(conn)?.0)
        }
    }

    /// Changes the player playlist while what is playing carries on.
    fn edit(
        &self,
        change: impl FnOnce(&Connection) -> Result<PlayerEdit, ApiError>,
    ) -> Result<(), ApiError> {
        let _list = lock(&self.list);
        let loaded = self.shared.loaded.load(Ordering::SeqCst);
        let edit = self.shared.db.write(|conn| {
            let tx = conn.transaction()?;
            let edit = change(&tx)?;
            if !loaded {
                // No engine holds the list, so the saved place has to follow the edit.
                let (index, position_ms) = playlists::player_position(&tx)?;
                let (index, position_ms) = moved_place(&edit.origin, index, position_ms);
                playlists::save_player_position(&tx, index, position_ms)?;
            }
            tx.commit()?;
            Ok::<_, ApiError>(edit)
        })?;
        let status = {
            let mut status = lock(&self.shared.status);
            status.count = edit.tracks.len() as u32;
            // The engine confirms this, but another edit may come first.
            status.index = moved_place(&edit.origin, status.index, 0).0;
            status.clone()
        };
        if loaded {
            self.send(&PlayerCommand::EditList {
                tracks: edit.tracks,
                origin: edit.origin,
            })?;
        }
        self.shared
            .events
            .emit(&CoreEvent::QueueChanged { replaced: false });
        self.shared.events.emit(&CoreEvent::PlayerChanged {
            status: Box::new(status),
        });
        Ok(())
    }

    pub fn queue_items(&self, offset: u32, limit: u32) -> Result<PlaylistPage, ApiError> {
        self.shared
            .db
            .read(|conn| playlists::player_items(conn, offset, limit))
    }

    /// Adds tracks to the end of the player playlist.
    pub fn add_to_queue(&self, track_ids: &[String]) -> Result<(), ApiError> {
        self.edit(|conn| playlists::insert_into_player(conn, track_ids, None))
    }

    /// Inserts tracks right after the current one.
    pub fn play_next(&self, track_ids: &[String]) -> Result<(), ApiError> {
        self.edit(|conn| {
            let at = self
                .current_index(conn)?
                .map_or(0, |index| index as usize + 1);
            playlists::insert_into_player(conn, track_ids, Some(at))
        })
    }

    pub fn remove_from_queue(&self, positions: &[i64]) -> Result<(), ApiError> {
        self.edit(|conn| playlists::remove_from_player(conn, positions))
    }

    pub fn move_in_queue(&self, positions: &[i64], before: Option<i64>) -> Result<(), ApiError> {
        self.edit(|conn| playlists::move_in_player(conn, positions, before))
    }

    pub fn clear_queue(&self) -> Result<(), ApiError> {
        self.edit(playlists::clear_player)
    }

    pub fn control(&self, action: PlayerAction) -> Result<(), ApiError> {
        let loaded = self.shared.loaded.load(Ordering::SeqCst);
        match action {
            PlayerAction::Play if !loaded => self.restore(false),
            // With nothing loaded there is nothing for the rest to act on.
            _ if !loaded => Ok(()),
            PlayerAction::Play => self.send(&PlayerCommand::Play),
            PlayerAction::Pause => self.send(&PlayerCommand::Pause),
            PlayerAction::Stop => self.send(&PlayerCommand::Stop),
            PlayerAction::Next => self.send(&PlayerCommand::Next),
            PlayerAction::Previous => self.send(&PlayerCommand::Previous),
        }
    }

    pub fn seek(&self, ms: u64) -> Result<(), ApiError> {
        if self.shared.loaded.load(Ordering::SeqCst) {
            self.send(&PlayerCommand::Seek { ms })?;
        }
        Ok(())
    }

    pub fn settings(&self) -> Result<PlayerSettings, ApiError> {
        self.shared.db.read(load_settings)
    }

    /// Applies a command to a running engine. A later engine gets it from the settings.
    fn tell_running_engine(&self, command: &PlayerCommand) {
        if let Some(running) = lock(&self.backend).as_mut() {
            let _ = running.send(command);
        }
    }

    /// Starts or stops visual frames, for a visualizer opening or closing. They come only
    /// while something plays, and a later engine starts with them as they were left.
    pub fn set_visuals(&self, enabled: bool) {
        self.shared.visuals.store(enabled, Ordering::SeqCst);
        if let Some(running) = lock(&self.backend).as_mut() {
            running.set_visuals(enabled);
        }
    }

    /// Tells views the settings as they now are.
    pub(crate) fn settings_changed(&self) -> Result<PlayerSettings, ApiError> {
        let settings = self.settings()?;
        self.shared.events.emit(&CoreEvent::PlayerSettingsChanged {
            settings: settings.clone(),
        });
        Ok(settings)
    }

    /// Has a running engine take up settings about to be saved, given as they will be stored.
    /// The engine comes first, because the database may be waiting on a scan.
    pub(crate) fn apply(&self, changed: &[(&str, &Value)]) -> Result<(), ApiError> {
        let (settings, behavior) = self.shared.db.read(|conn: &Connection| {
            Ok::<_, ApiError>((
                load_settings_with(conn, changed)?,
                load_behavior_with(conn, changed)?,
            ))
        })?;
        lock(&self.shared.plays).set_rule(behavior.play_rule);
        tell_running_engine_all(&self.backend, &settings, &behavior);
        Ok(())
    }

    /// Moves playback to the output device now saved, carrying on from where it was.
    pub(crate) fn output_changed(&self) {
        self.engines().replace();
    }

    /// Loads the last session's list at its place, paused, if the settings ask for that and
    /// nothing has been played yet.
    pub fn resume_at_start(&self) -> Result<(), ApiError> {
        let wanted = self
            .shared
            .db
            .read(|conn| settings::value(conn, RESUME_KEY))?;
        if wanted != json!(true) || self.shared.loaded.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.restore(true)
    }

    /// Something a background job can call when it has changed settings in the database, such
    /// as an import, so that playback and views follow them.
    pub fn settings_applier(&self) -> impl Fn() + Send + 'static + use<> {
        let engines = self.engines();
        move || {
            let shared = &engines.shared;
            let Ok((settings, behavior)) = shared.db.read(|conn: &Connection| {
                Ok::<_, ApiError>((load_settings(conn)?, load_behavior(conn)?))
            }) else {
                return;
            };
            lock(&shared.plays).set_rule(behavior.play_rule);
            tell_running_engine_all(&engines.backend, &settings, &behavior);
            let moved = *lock(&shared.wanted_device) != behavior.device;
            if moved && lock(&engines.backend).is_some() {
                engines.replace();
            }
            shared
                .events
                .emit(&CoreEvent::PlayerSettingsChanged { settings });
        }
    }

    /// Saves settings, which the engine already has, and tells views about them.
    fn save(&self, values: &[(&str, Value)]) -> Result<PlayerSettings, ApiError> {
        let saved = self.shared.db.write(|conn| {
            values
                .iter()
                .map(|(key, value)| settings::write(conn, key, value))
                .collect::<Result<Vec<_>, ApiError>>()
        })?;
        self.shared
            .events
            .emit(&CoreEvent::SettingsChanged { settings: saved });
        self.settings_changed()
    }

    /// Sets the output gain, from 0 to 1. Choosing a volume ends any mute.
    pub fn set_volume(&self, volume: f32) -> Result<PlayerSettings, ApiError> {
        let volume = volume.clamp(0.0, 1.0);
        // The engine first, because the database may be waiting on a scan.
        self.tell_running_engine(&PlayerCommand::SetVolume { volume });
        self.save(&[(VOLUME_KEY, json_number(volume)), (MUTED_KEY, json!(false))])
    }

    pub fn set_muted(&self, muted: bool) -> Result<PlayerSettings, ApiError> {
        let settings = PlayerSettings {
            muted,
            ..self.settings()?
        };
        self.tell_running_engine(&PlayerCommand::SetVolume {
            volume: audible_volume(&settings),
        });
        self.save(&[(MUTED_KEY, json!(muted))])
    }

    pub fn set_gain(
        &self,
        replay_gain: ReplayGainMode,
        preamp_db: f32,
    ) -> Result<PlayerSettings, ApiError> {
        let preamp_db = clamp_preamp(preamp_db);
        self.tell_running_engine(&PlayerCommand::SetGain {
            replay_gain,
            preamp_db,
        });
        let mode = match replay_gain {
            ReplayGainMode::Off => "off",
            ReplayGainMode::Track => "track",
            ReplayGainMode::Album => "album",
        };
        self.save(&[
            (REPLAY_GAIN_KEY, json!(mode)),
            (PREAMP_KEY, json_number(preamp_db)),
        ])
    }

    /// Sets the equalizer. It applies at once and is saved.
    pub fn set_eq(&self, enabled: bool, bands: &[f32]) -> Result<PlayerSettings, ApiError> {
        let eq = EqSettings {
            enabled,
            bands: clean_bands(bands)?.to_vec(),
        };
        self.tell_running_engine(&eq_command(&eq)?);
        let value = serde_json::to_value(&eq)
            .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
        self.save(&[(EQ_KEY, value)])
    }

    pub fn eq_presets(&self) -> Result<EqPresets, ApiError> {
        self.shared.db.read(all_presets)
    }

    /// Saves a preset, replacing one of that name unless it comes with the app.
    pub fn save_eq_preset(&self, name: &str, bands: &[f32]) -> Result<EqPresets, ApiError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgs,
                "a preset needs a name",
            ));
        }
        if is_built_in(name) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!("{name} comes with the app, so choose another name"),
            ));
        }
        let bands = clean_bands(bands)?.to_vec();
        let presets = self.shared.db.write(|conn| {
            let tx = conn.transaction()?;
            let mut saved = saved_presets(&tx)?;
            saved.retain(|preset| !preset.name.eq_ignore_ascii_case(name));
            saved.push(EqPreset {
                name: name.to_owned(),
                bands,
                built_in: false,
            });
            write_presets(&tx, &saved)?;
            let presets = all_presets(&tx)?;
            tx.commit()?;
            Ok::<_, ApiError>(presets)
        })?;
        self.shared.events.emit(&CoreEvent::EqPresetsChanged);
        Ok(presets)
    }

    pub fn delete_eq_preset(&self, name: &str) -> Result<EqPresets, ApiError> {
        if is_built_in(name) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!("{name} comes with the app, so it cannot be removed"),
            ));
        }
        let presets = self.shared.db.write(|conn| {
            let tx = conn.transaction()?;
            let mut saved = saved_presets(&tx)?;
            saved.retain(|preset| !preset.name.eq_ignore_ascii_case(name));
            write_presets(&tx, &saved)?;
            let presets = all_presets(&tx)?;
            tx.commit()?;
            Ok::<_, ApiError>(presets)
        })?;
        self.shared.events.emit(&CoreEvent::EqPresetsChanged);
        Ok(presets)
    }

    pub fn gain_coverage(&self) -> Result<GainCoverage, ApiError> {
        self.shared.db.read(|conn: &Connection| {
            Ok(conn
                .prepare_cached(
                    "SELECT count(*), count(rg_track_gain), count(rg_album_gain) FROM tracks",
                )?
                .query_row([], |row| {
                    Ok(GainCoverage {
                        tracks: row.get(0)?,
                        with_track_gain: row.get(1)?,
                        with_album_gain: row.get(2)?,
                    })
                })?)
        })
    }

    /// Resumes the last session's list and position, playing or paused.
    fn restore(&self, paused: bool) -> Result<(), ApiError> {
        let _list = lock(&self.list);
        self.engines().restore(paused)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use rusqlite::params;

    use super::*;
    use crate::db::{DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID, new_public_id};

    #[derive(Clone, Default)]
    struct Recording(Arc<StdMutex<Vec<CoreEvent>>>);

    impl EventSink for Recording {
        fn emit(&self, event: &CoreEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    /// Records commands, and lets a test act as the engine through `signal`.
    #[derive(Clone, Default)]
    struct FakeEngine {
        commands: Arc<StdMutex<Vec<PlayerCommand>>>,
        signal: Arc<StdMutex<Option<BackendEvents>>>,
        starts: Arc<StdMutex<u32>>,
        broken: Arc<StdMutex<bool>>,
        visuals: Arc<StdMutex<Vec<bool>>>,
        /// The device each engine was started on.
        devices: Arc<StdMutex<Vec<Option<String>>>>,
    }

    struct FakeBackend(FakeEngine);

    impl PlayerBackend for FakeBackend {
        fn send(&mut self, command: &PlayerCommand) -> Result<(), String> {
            if *self.0.broken.lock().unwrap() {
                return Err("the pipe is closed".to_owned());
            }
            self.0.commands.lock().unwrap().push(command.clone());
            Ok(())
        }

        fn set_visuals(&mut self, enabled: bool) {
            self.0.visuals.lock().unwrap().push(enabled);
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        db: Arc<Database>,
        events: Recording,
        engine: FakeEngine,
        player: Player,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(dir.path().join("umak.db")).unwrap());
        db.write(|conn| {
            conn.execute_batch(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                     VALUES (1, 'r', 'Music', 'M:/music', 0);
                 INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                     VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x');",
            )?;
            for name in ["a", "b", "c"] {
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                         title_sort, artist_sort, album_sort, album_artist_sort, added_at, updated_at)
                     VALUES (?1, 1, ?2, 1, 1, ?1, 1, ?1, 'x', 'x', 'x', 0, 0)",
                    params![name, format!("dir/{name}.flac")],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        let events = Recording::default();
        let engine = FakeEngine::default();
        let factory_engine = engine.clone();
        let factory: BackendFactory = Box::new(move |callback, device: Option<&str>| {
            *factory_engine.signal.lock().unwrap() = Some(callback);
            *factory_engine.starts.lock().unwrap() += 1;
            *factory_engine.broken.lock().unwrap() = false;
            factory_engine
                .devices
                .lock()
                .unwrap()
                .push(device.map(str::to_owned));
            Ok(Box::new(FakeBackend(factory_engine.clone())))
        });
        let player = Player::new(Arc::clone(&db), Arc::new(events.clone()), factory);
        Fixture {
            _dir: dir,
            db,
            events,
            engine,
            player,
        }
    }

    impl Fixture {
        fn all_commands(&self) -> Vec<PlayerCommand> {
            self.engine.commands.lock().unwrap().clone()
        }

        /// The commands that drive playback, without the settings a new engine starts with.
        fn commands(&self) -> Vec<PlayerCommand> {
            self.all_commands()
                .into_iter()
                .filter(|command| {
                    !matches!(
                        command,
                        PlayerCommand::SetVolume { .. }
                            | PlayerCommand::SetGain { .. }
                            | PlayerCommand::SetEq { .. }
                            | PlayerCommand::SetRestartThreshold { .. }
                    )
                })
                .collect()
        }

        fn signal(&self, event: PlayerEvent) {
            let callback = self.engine.signal.lock().unwrap().clone().unwrap();
            callback(BackendSignal::Event(event));
        }

        fn player_titles(&self) -> Vec<String> {
            self.db
                .read(|conn| {
                    let mut statement = conn.prepare(
                        "SELECT t.title FROM playlist_items i
                         JOIN playlists p ON p.id = i.playlist_id
                         JOIN tracks t ON t.id = i.track_id
                         WHERE p.public_id = ?1 ORDER BY i.position",
                    )?;
                    let rows = statement
                        .query_map([DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID], |row| row.get(0))?;
                    rows.collect::<Result<Vec<String>, _>>()
                        .map_err(DbError::from)
                })
                .unwrap()
        }

        fn saved_list(&self, tracks: &[&str]) -> String {
            let ids: Vec<String> = tracks.iter().map(|id| (*id).to_owned()).collect();
            self.db
                .write(|conn| {
                    let list = playlists::create(conn, "Saved", 1)?;
                    playlists::add_tracks(conn, &list.id, &ids, 1)?;
                    Ok::<_, ApiError>(list.id)
                })
                .unwrap()
        }
    }

    fn state(state: PlaybackState, index: usize, track: &str, position_ms: u64) -> PlayerEvent {
        PlayerEvent::State {
            state,
            index: Some(index),
            track_id: Some(track.to_owned()),
            position_ms,
            duration_ms: Some(9000),
        }
    }

    #[test]
    fn playing_a_song_replaces_the_list_and_sends_full_paths_to_the_engine() {
        let fixture = fixture();
        fixture
            .player
            .play_tracks(&["a".to_owned(), "b".to_owned()], 0)
            .unwrap();
        fixture.player.play_tracks(&["c".to_owned()], 0).unwrap();
        assert_eq!(fixture.player_titles(), ["c"]);

        let commands = fixture.commands();
        let PlayerCommand::Load {
            tracks,
            start_index,
            start_ms,
            ..
        } = &commands[1]
        else {
            panic!("expected a load");
        };
        assert_eq!((*start_index, *start_ms), (0, 0));
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "c");
        assert!(tracks[0].path.ends_with("dir/c.flac"), "{}", tracks[0].path);
        assert!(tracks[0].path.starts_with("M:/music"), "{}", tracks[0].path);
    }

    #[test]
    fn playing_a_playlist_copies_it_so_the_saved_one_is_separate() {
        let fixture = fixture();
        let saved = fixture.saved_list(&["b", "a", "b"]);
        fixture.player.play_playlist(&saved, 2).unwrap();
        assert_eq!(fixture.player_titles(), ["b", "a", "b"]);
        let PlayerCommand::Load { start_index, .. } = fixture.commands()[0].clone() else {
            panic!("expected a load");
        };
        assert_eq!(start_index, 2);

        fixture.player.play_tracks(&["c".to_owned()], 0).unwrap();
        let kept = fixture
            .db
            .read(|conn| playlists::items(conn, &saved, 0, 10))
            .unwrap();
        assert_eq!(kept.total, 3);
    }

    #[test]
    fn empty_lists_and_bad_starts_are_refused_and_change_nothing() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        let error = fixture.player.play_tracks(&[], 0).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
        let error = fixture
            .player
            .play_tracks(&["b".to_owned()], 5)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
        assert_eq!(fixture.player_titles(), ["a"]);

        let empty = fixture.saved_list(&[]);
        assert!(fixture.player.play_playlist(&empty, 0).is_err());

        let error = fixture
            .player
            .play_tracks(&["no-such-track".to_owned()], 0)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
    }

    #[test]
    fn controls_reach_the_engine_only_once_something_is_loaded() {
        let fixture = fixture();
        fixture.player.control(PlayerAction::Pause).unwrap();
        fixture.player.control(PlayerAction::Next).unwrap();
        fixture.player.seek(100).unwrap();
        assert!(fixture.commands().is_empty());
        assert_eq!(
            *fixture.engine.starts.lock().unwrap(),
            0,
            "no engine started"
        );

        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.player.control(PlayerAction::Pause).unwrap();
        fixture.player.control(PlayerAction::Play).unwrap();
        fixture.player.control(PlayerAction::Next).unwrap();
        fixture.player.control(PlayerAction::Previous).unwrap();
        fixture.player.control(PlayerAction::Stop).unwrap();
        fixture.player.seek(1500).unwrap();
        assert_eq!(
            fixture.commands()[1..],
            [
                PlayerCommand::Pause,
                PlayerCommand::Play,
                PlayerCommand::Next,
                PlayerCommand::Previous,
                PlayerCommand::Stop,
                PlayerCommand::Seek { ms: 1500 },
            ]
        );
    }

    #[test]
    fn the_current_track_shows_new_details_after_its_tags_change() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.signal(state(PlaybackState::Playing, 0, "a", 250));
        let refresh = fixture.player.track_refresher();
        refresh();
        let events = |fixture: &Fixture| {
            fixture
                .events
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, CoreEvent::PlayerChanged { .. }))
                .count()
        };
        let before = events(&fixture);
        refresh();
        assert_eq!(
            events(&fixture),
            before,
            "nothing changed, so nothing to say"
        );

        fixture
            .player
            .shared
            .db
            .write(|conn| {
                conn.execute("UPDATE tracks SET title = 'New' WHERE public_id = 'a'", [])?;
                Ok::<_, DbError>(())
            })
            .unwrap();
        refresh();
        assert_eq!(fixture.player.status().track.unwrap().title, "New");
        assert_eq!(fixture.player.status().position_ms, 250);
        assert_eq!(events(&fixture), before + 1);
    }

    #[test]
    fn engine_events_become_a_status_with_the_track_details() {
        let fixture = fixture();
        fixture
            .player
            .play_tracks(&["a".to_owned(), "b".to_owned()], 0)
            .unwrap();
        fixture.signal(PlayerEvent::Ready {
            device: "Speakers".to_owned(),
            sample_rate: 48_000,
            channels: 2,
        });
        fixture.signal(state(PlaybackState::Playing, 1, "b", 250));

        let status = fixture.player.status();
        assert_eq!(status.state, PlaybackState::Playing);
        assert_eq!(status.track.as_ref().unwrap().title, "b");
        assert_eq!(status.index, Some(1));
        assert_eq!(status.count, 2);
        assert_eq!(status.duration_ms, Some(9000));
        assert_eq!(status.device.as_deref(), Some("Speakers"));
        assert!(fixture.events.0.lock().unwrap().iter().any(
            |event| matches!(event, CoreEvent::PlayerChanged { status } if status.position_ms == 250)
        ));

        fixture.signal(PlayerEvent::Position { position_ms: 900 });
        assert_eq!(fixture.player.status().position_ms, 900);
        assert!(
            fixture
                .events
                .0
                .lock()
                .unwrap()
                .contains(&CoreEvent::PlayerPosition { position_ms: 900 })
        );
    }

    #[test]
    fn a_failed_track_is_reported_by_its_title() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.signal(PlayerEvent::TrackFailed {
            id: "a".to_owned(),
            message: "file not found".to_owned(),
        });
        let events = fixture.events.0.lock().unwrap();
        assert!(events.contains(&CoreEvent::PlayerError {
            message: "Could not play a: file not found".to_owned()
        }));
    }

    #[test]
    fn play_with_nothing_loaded_resumes_the_saved_list_and_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("first.db");
        {
            let fixture = fixture();
            fixture
                .player
                .play_tracks(&["a".to_owned(), "b".to_owned()], 0)
                .unwrap();
            fixture.signal(state(PlaybackState::Paused, 1, "b", 4200));
            fixture
                .db
                .write(|conn| {
                    conn.execute("VACUUM INTO ?1", [path.to_str().unwrap()])?;
                    Ok::<_, DbError>(())
                })
                .unwrap();
        }

        let db = Arc::new(Database::open(&path).unwrap());
        let engine = FakeEngine::default();
        let factory_engine = engine.clone();
        let factory: BackendFactory = Box::new(move |callback, _device| {
            *factory_engine.signal.lock().unwrap() = Some(callback);
            Ok(Box::new(FakeBackend(factory_engine.clone())))
        });
        let player = Player::new(db, Arc::new(Recording::default()), factory);
        player.control(PlayerAction::Play).unwrap();

        let commands = engine.commands.lock().unwrap();
        let PlayerCommand::Load {
            tracks,
            start_index,
            start_ms,
            ..
        } = commands.last().unwrap()
        else {
            panic!("expected a load, got {commands:?}");
        };
        assert_eq!(tracks.len(), 2);
        assert_eq!((*start_index, *start_ms), (1, 4200));
    }

    #[test]
    fn play_with_no_saved_list_does_nothing() {
        let fixture = fixture();
        fixture.player.control(PlayerAction::Play).unwrap();
        assert!(fixture.commands().is_empty());
        assert_eq!(*fixture.engine.starts.lock().unwrap(), 0);
    }

    fn set(fixture: &Fixture, key: &str, value: Value) {
        fixture
            .db
            .write(|conn| settings::write(conn, key, &value))
            .unwrap();
    }

    fn errors(fixture: &Fixture) -> Vec<String> {
        fixture
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                CoreEvent::PlayerError { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn ready(fixture: &Fixture, device: &str) {
        fixture.signal(PlayerEvent::Ready {
            device: device.to_owned(),
            sample_rate: 48_000,
            channels: 2,
        });
    }

    fn last_load(fixture: &Fixture) -> (usize, u64, bool) {
        let commands = fixture.commands();
        let Some(PlayerCommand::Load {
            start_index,
            start_ms,
            paused,
            ..
        }) = commands.last()
        else {
            panic!("expected a load, got {commands:?}");
        };
        (*start_index, *start_ms, *paused)
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let began = Instant::now();
        while !done() {
            assert!(began.elapsed() < Duration::from_secs(5), "{what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn changing_the_output_carries_on_from_the_same_place_on_the_new_device() {
        let fixture = fixture();
        let tracks = ["a".to_owned(), "b".to_owned()];
        fixture.player.play_tracks(&tracks, 0).unwrap();
        fixture.signal(state(PlaybackState::Paused, 1, "b", 7000));

        set(&fixture, OUTPUT_DEVICE_KEY, json!("Headphones"));
        fixture.player.output_changed();
        assert_eq!(
            *fixture.engine.devices.lock().unwrap(),
            [None, Some("Headphones".to_owned())]
        );
        assert_eq!(last_load(&fixture), (1, 7000, true));
    }

    #[test]
    fn changing_the_output_with_nothing_playing_opens_it_at_once_if_an_engine_runs() {
        let fixture = fixture();
        set(&fixture, OUTPUT_DEVICE_KEY, json!("Headphones"));
        fixture.player.output_changed();
        assert_eq!(*fixture.engine.starts.lock().unwrap(), 0);

        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.player.control(PlayerAction::Stop).unwrap();
        fixture.signal(state(PlaybackState::Stopped, 0, "a", 0));
        set(&fixture, OUTPUT_DEVICE_KEY, json!(""));
        fixture.player.output_changed();
        assert_eq!(
            *fixture.engine.devices.lock().unwrap(),
            [Some("Headphones".to_owned()), None]
        );
    }

    #[test]
    fn a_device_that_is_not_there_is_reported_with_the_one_used() {
        let fixture = fixture();
        set(&fixture, OUTPUT_DEVICE_KEY, json!("Headphones"));
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        ready(&fixture, "Speakers");
        assert_eq!(fixture.player.status().device.as_deref(), Some("Speakers"));
        assert_eq!(
            errors(&fixture),
            ["Headphones was not found, so Speakers is used until it is back and chosen again."]
        );
        set(&fixture, OUTPUT_DEVICE_KEY, json!("Speakers"));
        fixture.player.output_changed();
        ready(&fixture, "Speakers");
        assert_eq!(errors(&fixture).len(), 1);
    }

    #[test]
    fn a_failed_output_is_replaced_and_playback_carries_on_unless_it_fails_again_at_once() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        ready(&fixture, "Headphones");
        fixture.signal(state(PlaybackState::Playing, 0, "a", 5000));

        fixture.signal(PlayerEvent::OutputFailed {
            message: "Headphones stopped working".to_owned(),
        });
        wait_until("no new engine", || {
            *fixture.engine.starts.lock().unwrap() == 2
                && matches!(fixture.commands().last(), Some(PlayerCommand::Load { .. }))
        });
        assert_eq!(last_load(&fixture), (0, 5000, false));
        ready(&fixture, "Speakers");
        assert_eq!(
            errors(&fixture),
            ["Headphones stopped working, so playback moved to Speakers."]
        );

        fixture.signal(PlayerEvent::OutputFailed {
            message: "Speakers stopped working".to_owned(),
        });
        wait_until("still playing", || {
            fixture.player.status().state == PlaybackState::Stopped
        });
        assert_eq!(*fixture.engine.starts.lock().unwrap(), 2);
        assert_eq!(
            errors(&fixture).last().map(String::as_str),
            Some("Speakers stopped working, again, so playback stopped. Play tries again.")
        );
    }

    #[test]
    fn the_last_queue_is_loaded_paused_at_start_only_if_asked_for() {
        let fixture = fixture();
        let tracks = ["a".to_owned(), "b".to_owned()];
        fixture.player.play_tracks(&tracks, 1).unwrap();
        fixture.signal(state(PlaybackState::Playing, 1, "b", 6000));
        let events: Arc<dyn EventSink> = Arc::new(fixture.events.clone());
        let restarted = Player::new(
            Arc::clone(&fixture.db),
            Arc::clone(&events),
            Box::new(no_engine),
        );
        restarted.resume_at_start().unwrap();
        assert!(!restarted.shared.loaded.load(Ordering::SeqCst));

        set(&fixture, RESUME_KEY, json!(true));
        let engine = fixture.engine.clone();
        let restarted = Player::new(
            Arc::clone(&fixture.db),
            events,
            Box::new(move |_, _| Ok(Box::new(FakeBackend(engine.clone())))),
        );
        restarted.resume_at_start().unwrap();
        assert_eq!(last_load(&fixture), (1, 6000, true));
    }

    #[test]
    fn previous_and_play_counting_follow_the_settings() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        let threshold = json!(0.0);
        let share = json!(10.0);
        fixture
            .player
            .apply(&[(PREVIOUS_RESTART_KEY, &threshold), (PLAY_SHARE_KEY, &share)])
            .unwrap();
        assert!(
            fixture
                .all_commands()
                .contains(&PlayerCommand::SetRestartThreshold { ms: 0 })
        );
        // A tenth of a 100-second track counts.
        fixture.signal(PlayerEvent::State {
            state: PlaybackState::Playing,
            index: Some(0),
            track_id: Some("a".to_owned()),
            position_ms: 0,
            duration_ms: Some(100_000),
        });
        for position_ms in (250..=10_250).step_by(250) {
            fixture.signal(PlayerEvent::Position { position_ms });
        }
        let plays: i64 = fixture
            .db
            .read(|conn| {
                conn.query_row("SELECT count(*) FROM play_history", [], |row| row.get(0))
                    .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(plays, 1);
    }

    #[test]
    fn a_dead_engine_is_replaced_and_the_list_is_loaded_again() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.signal(state(PlaybackState::Playing, 0, "a", 1000));

        let callback = fixture.engine.signal.lock().unwrap().clone().unwrap();
        callback(BackendSignal::Exited);
        *fixture.engine.broken.lock().unwrap() = true;
        assert_eq!(fixture.player.status().state, PlaybackState::Stopped);
        assert!(
            fixture
                .events
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, CoreEvent::PlayerError { .. }))
        );

        fixture.player.control(PlayerAction::Play).unwrap();
        assert_eq!(*fixture.engine.starts.lock().unwrap(), 2);
        let commands = fixture.commands();
        let PlayerCommand::Load { start_ms, .. } = commands.last().unwrap() else {
            panic!("expected a load, got {commands:?}");
        };
        assert_eq!(*start_ms, 1000);
    }

    #[test]
    fn visuals_reach_a_running_engine_and_a_later_one_and_their_frames_go_out() {
        let fixture = fixture();
        fixture.player.set_visuals(true);
        assert_eq!(
            *fixture.engine.starts.lock().unwrap(),
            0,
            "no engine just for visuals"
        );
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.player.set_visuals(false);
        assert_eq!(*fixture.engine.visuals.lock().unwrap(), [true, false]);

        let frame = VisualFrame {
            bands: vec![-6.0],
            wave: vec![0.5],
        };
        let callback = fixture.engine.signal.lock().unwrap().clone().unwrap();
        callback(BackendSignal::Visuals(frame.clone()));
        assert!(
            fixture
                .events
                .0
                .lock()
                .unwrap()
                .contains(&CoreEvent::VisualFrame { frame })
        );

        callback(BackendSignal::Exited);
        *fixture.engine.broken.lock().unwrap() = true;
        fixture.player.control(PlayerAction::Play).unwrap();
        assert_eq!(*fixture.engine.starts.lock().unwrap(), 2);
        assert_eq!(
            *fixture.engine.visuals.lock().unwrap(),
            [true, false, false]
        );
    }

    fn queue_titles(fixture: &Fixture) -> Vec<String> {
        fixture.player_titles()
    }

    fn queue_events(fixture: &Fixture) -> usize {
        fixture
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, CoreEvent::QueueChanged { .. }))
            .count()
    }

    #[test]
    fn edits_while_playing_reach_the_engine_with_where_each_entry_came_from() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.signal(state(PlaybackState::Playing, 0, "a", 1000));
        let before = fixture.commands().len();

        fixture.player.add_to_queue(&["b".to_owned()]).unwrap();
        fixture.player.play_next(&["c".to_owned()]).unwrap();
        assert_eq!(queue_titles(&fixture), ["a", "c", "b"]);
        assert_eq!(fixture.player.status().count, 3);

        let commands = fixture.commands();
        assert_eq!(commands.len(), before + 2, "no reload: {commands:?}");
        let PlayerCommand::EditList { tracks, origin } = commands.last().unwrap() else {
            panic!("expected an edit, got {commands:?}");
        };
        assert_eq!(tracks.len(), 3);
        assert_eq!(*origin, [Some(0), None, Some(1)]);
        assert_eq!(queue_events(&fixture), 3);
    }

    #[test]
    fn play_next_goes_after_the_current_track_not_the_first() {
        let fixture = fixture();
        let ids = ["a".to_owned(), "b".to_owned()];
        fixture.player.play_tracks(&ids, 1).unwrap();
        fixture.signal(state(PlaybackState::Playing, 1, "b", 0));
        fixture.player.play_next(&["c".to_owned()]).unwrap();
        assert_eq!(queue_titles(&fixture), ["a", "b", "c"]);
    }

    #[test]
    fn a_second_edit_uses_the_place_the_first_one_moved_the_current_track_to() {
        let fixture = fixture();
        let ids = ["a".to_owned(), "b".to_owned()];
        fixture.player.play_tracks(&ids, 1).unwrap();
        fixture.signal(state(PlaybackState::Playing, 1, "b", 0));
        // The engine has not reported anything between these two.
        let items = fixture.player.queue_items(0, 10).unwrap().items;
        fixture
            .player
            .move_in_queue(&[items[1].position], Some(items[0].position))
            .unwrap();
        fixture.player.play_next(&["c".to_owned()]).unwrap();
        assert_eq!(queue_titles(&fixture), ["b", "c", "a"]);
    }

    #[test]
    fn with_no_engine_edits_change_the_list_and_keep_the_saved_place() {
        let fixture = fixture();
        fixture
            .db
            .write(|conn| {
                let ids = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
                playlists::set_player_tracks(conn, &ids)?;
                playlists::save_player_position(conn, Some(1), 7000)?;
                Ok::<_, ApiError>(())
            })
            .unwrap();

        fixture.player.play_next(&["c".to_owned()]).unwrap();
        assert_eq!(queue_titles(&fixture), ["a", "b", "c", "c"]);
        assert!(fixture.commands().is_empty(), "no engine was started");

        let items = fixture.player.queue_items(0, 10).unwrap().items;
        fixture
            .player
            .move_in_queue(&[items[1].position], None)
            .unwrap();
        assert_eq!(queue_titles(&fixture), ["a", "c", "c", "b"]);
        fixture.player.control(PlayerAction::Play).unwrap();
        let commands = fixture.commands();
        let PlayerCommand::Load {
            start_index,
            start_ms,
            ..
        } = commands.last().unwrap()
        else {
            panic!("expected a load, got {commands:?}");
        };
        assert_eq!(
            (*start_index, *start_ms),
            (3, 7000),
            "the same track, same spot"
        );
    }

    #[test]
    fn a_saved_place_follows_removal_to_the_next_entry_or_nowhere() {
        let origin = [Some(0), Some(2), Some(3)];
        assert_eq!(moved_place(&origin, Some(2), 500), (Some(1), 500));
        assert_eq!(moved_place(&origin, Some(1), 500), (Some(1), 0));
        assert_eq!(moved_place(&[Some(0)], Some(1), 500), (None, 0));
        assert_eq!(moved_place(&origin, None, 500), (None, 0));
    }

    #[test]
    fn playing_an_entry_keeps_the_list() {
        let fixture = fixture();
        let ids = ["a".to_owned(), "b".to_owned()];
        fixture.player.play_tracks(&ids, 0).unwrap();
        fixture.player.play_index(1).unwrap();
        assert_eq!(queue_titles(&fixture), ["a", "b"]);
        let commands = fixture.commands();
        let PlayerCommand::Load { start_index, .. } = commands.last().unwrap() else {
            panic!("expected a load, got {commands:?}");
        };
        assert_eq!(*start_index, 1);
        assert_eq!(
            fixture.player.play_index(5).unwrap_err().code,
            ErrorCode::InvalidArgs
        );
    }

    #[test]
    fn clearing_while_playing_sends_an_empty_list() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.player.clear_queue().unwrap();
        assert!(queue_titles(&fixture).is_empty());
        assert_eq!(fixture.player.status().count, 0);
        let commands = fixture.commands();
        let PlayerCommand::EditList { tracks, origin } = commands.last().unwrap() else {
            panic!("expected an edit, got {commands:?}");
        };
        assert!(tracks.is_empty() && origin.is_empty());
    }

    #[test]
    fn settings_default_to_no_processing_and_are_saved() {
        let fixture = fixture();
        assert_eq!(
            fixture.player.settings().unwrap(),
            PlayerSettings::default()
        );

        fixture.player.set_volume(0.25).unwrap();
        fixture
            .player
            .set_gain(ReplayGainMode::Album, -3.5)
            .unwrap();
        assert_eq!(
            fixture.player.settings().unwrap(),
            PlayerSettings {
                volume: 0.25,
                muted: false,
                replay_gain: ReplayGainMode::Album,
                preamp_db: -3.5,
                eq: EqSettings::default(),
            }
        );
        assert!(fixture.all_commands().is_empty(), "no engine was started");
    }

    #[test]
    fn settings_are_kept_in_range_and_bad_saved_values_fall_back() {
        let fixture = fixture();
        assert_eq!(fixture.player.set_volume(7.0).unwrap().volume, 1.0);
        assert_eq!(fixture.player.set_volume(-1.0).unwrap().volume, 0.0);
        let settings = fixture
            .player
            .set_gain(ReplayGainMode::Track, 40.0)
            .unwrap();
        assert_eq!(settings.preamp_db, MAX_PREAMP_DB);

        fixture
            .db
            .write(|conn| {
                save_setting(conn, VOLUME_KEY, "loud")?;
                save_setting(conn, REPLAY_GAIN_KEY, "maybe")?;
                save_setting(conn, PREAMP_KEY, "NaN")
            })
            .unwrap();
        assert_eq!(
            fixture.player.settings().unwrap(),
            PlayerSettings::default()
        );
    }

    #[test]
    fn a_new_engine_starts_with_the_saved_settings_before_anything_plays() {
        let fixture = fixture();
        fixture.player.set_volume(0.5).unwrap();
        fixture.player.set_gain(ReplayGainMode::Track, 2.0).unwrap();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        let commands = fixture.all_commands();
        assert_eq!(commands[0], PlayerCommand::SetVolume { volume: 0.5 });
        assert_eq!(
            commands[1],
            PlayerCommand::SetGain {
                replay_gain: ReplayGainMode::Track,
                preamp_db: 2.0,
            }
        );
        assert!(matches!(commands[2], PlayerCommand::SetEq { .. }));
        assert_eq!(commands[3], PlayerCommand::SetRestartThreshold { ms: 3000 });
        assert!(matches!(commands[4], PlayerCommand::Load { .. }));
    }

    #[test]
    fn settings_changed_elsewhere_reach_a_running_engine_before_they_are_saved() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        let mode = json!("track");
        let preamp = json!(-2.5);
        fixture
            .player
            .apply(&[(REPLAY_GAIN_KEY, &mode), (PREAMP_KEY, &preamp)])
            .unwrap();
        assert!(fixture.all_commands().contains(&PlayerCommand::SetGain {
            replay_gain: ReplayGainMode::Track,
            preamp_db: -2.5,
        }));
        assert_eq!(
            fixture.player.settings().unwrap(),
            PlayerSettings::default(),
            "saving is left to the store"
        );

        // As an import does it: saved first, then taken up.
        fixture
            .db
            .write(|conn| settings::write(conn, VOLUME_KEY, &json!(0.5)))
            .unwrap();
        (fixture.player.settings_applier())();
        assert!(fixture.all_commands().ends_with(&[
            PlayerCommand::SetVolume { volume: 0.5 },
            PlayerCommand::SetGain {
                replay_gain: ReplayGainMode::Off,
                preamp_db: 0.0,
            },
            PlayerCommand::SetEq {
                enabled: false,
                bands_db: [0.0; EQ_BANDS],
            },
            PlayerCommand::SetRestartThreshold { ms: 3000 },
        ]));
    }

    #[test]
    fn changes_from_the_player_are_announced_as_settings_too() {
        let fixture = fixture();
        fixture.player.set_volume(0.5).unwrap();
        let events = fixture.events.0.lock().unwrap();
        let Some(CoreEvent::SettingsChanged { settings }) = events
            .iter()
            .find(|event| matches!(event, CoreEvent::SettingsChanged { .. }))
        else {
            panic!("no settings event in {events:?}");
        };
        let keys: Vec<&str> = settings
            .iter()
            .map(|setting| setting.key.as_str())
            .collect();
        assert_eq!(keys, [VOLUME_KEY, MUTED_KEY]);
        assert_eq!(settings[0].value, json!(0.5));
        assert!(settings[1].is_default);
    }

    #[test]
    fn a_running_engine_gets_changes_at_once_and_views_are_told() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        let settings = fixture.player.set_volume(0.5).unwrap();
        assert_eq!(
            fixture.all_commands().last(),
            Some(&PlayerCommand::SetVolume { volume: 0.5 })
        );
        assert!(
            fixture
                .events
                .0
                .lock()
                .unwrap()
                .contains(&CoreEvent::PlayerSettingsChanged { settings })
        );
    }

    #[test]
    fn muting_silences_the_engine_and_keeps_the_volume_to_come_back_to() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.player.set_volume(0.5).unwrap();

        let settings = fixture.player.set_muted(true).unwrap();
        assert!(settings.muted);
        assert_eq!(settings.volume, 0.5);
        assert_eq!(
            fixture.all_commands().last(),
            Some(&PlayerCommand::SetVolume { volume: 0.0 })
        );

        fixture.player.set_muted(false).unwrap();
        assert_eq!(
            fixture.all_commands().last(),
            Some(&PlayerCommand::SetVolume { volume: 0.5 })
        );
    }

    #[test]
    fn a_new_engine_starts_silent_if_muted() {
        let fixture = fixture();
        fixture.player.set_volume(0.5).unwrap();
        fixture.player.set_muted(true).unwrap();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        assert_eq!(
            fixture.all_commands()[0],
            PlayerCommand::SetVolume { volume: 0.0 }
        );
    }

    #[test]
    fn choosing_a_volume_ends_a_mute() {
        let fixture = fixture();
        fixture.player.set_muted(true).unwrap();
        let settings = fixture.player.set_volume(0.5).unwrap();
        assert!(!settings.muted);
        assert_eq!(fixture.player.settings().unwrap().volume, 0.5);
    }

    #[test]
    fn playing_an_album_replaces_the_list_with_it() {
        let fixture = fixture();
        fixture.player.play_tracks(&["b".to_owned()], 0).unwrap();
        fixture.player.play_album("b").unwrap();
        assert_eq!(queue_titles(&fixture), ["a", "b", "c"]);
        let commands = fixture.commands();
        let PlayerCommand::Load { start_index, .. } = commands.last().unwrap() else {
            panic!("expected a load, got {commands:?}");
        };
        assert_eq!(*start_index, 0);
    }

    #[test]
    fn an_album_with_nothing_playable_is_reported() {
        let fixture = fixture();
        fixture
            .db
            .write(|conn| {
                conn.execute("UPDATE tracks SET missing_since = 1", [])?;
                Ok::<_, DbError>(())
            })
            .unwrap();
        let error = fixture.player.play_album("a").unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
    }

    fn bands(first: f32) -> Vec<f32> {
        let mut bands = vec![0.0; EQ_BANDS];
        bands[0] = first;
        bands
    }

    #[test]
    fn the_equalizer_is_saved_kept_in_range_and_reaches_a_new_engine() {
        let fixture = fixture();
        assert!(!fixture.player.settings().unwrap().eq.enabled);

        let settings = fixture.player.set_eq(true, &bands(40.0)).unwrap();
        assert_eq!(settings.eq.bands[0], MAX_BAND_DB);
        assert_eq!(fixture.player.settings().unwrap().eq, settings.eq);

        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        let mut expected = [0.0; EQ_BANDS];
        expected[0] = MAX_BAND_DB;
        assert!(fixture.all_commands().contains(&PlayerCommand::SetEq {
            enabled: true,
            bands_db: expected,
        }));
    }

    #[test]
    fn a_running_engine_gets_equalizer_changes_at_once() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.player.set_eq(false, &bands(3.0)).unwrap();
        let mut expected = [0.0; EQ_BANDS];
        expected[0] = 3.0;
        assert_eq!(
            fixture.all_commands().last(),
            Some(&PlayerCommand::SetEq {
                enabled: false,
                bands_db: expected,
            })
        );
    }

    #[test]
    fn a_wrong_number_of_bands_is_refused_and_changes_nothing() {
        let fixture = fixture();
        fixture.player.set_eq(true, &bands(3.0)).unwrap();
        let error = fixture.player.set_eq(true, &[1.0, 2.0]).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
        assert_eq!(fixture.player.settings().unwrap().eq.bands[0], 3.0);
    }

    #[test]
    fn a_damaged_saved_equalizer_falls_back_to_off() {
        let fixture = fixture();
        fixture
            .db
            .write(|conn| save_setting(conn, EQ_KEY, r#"{"enabled":true,"bands":[1,2]}"#))
            .unwrap();
        assert_eq!(fixture.player.settings().unwrap().eq, EqSettings::default());
        fixture
            .db
            .write(|conn| save_setting(conn, EQ_KEY, "not json"))
            .unwrap();
        assert_eq!(fixture.player.settings().unwrap().eq, EqSettings::default());
    }

    #[test]
    fn presets_list_the_built_in_ones_first_and_saved_ones_by_name() {
        let fixture = fixture();
        let listed = fixture.player.eq_presets().unwrap();
        assert_eq!(listed.frequencies.len(), EQ_BANDS);
        assert_eq!(listed.presets.len(), BUILT_IN_PRESETS.len());
        assert!(listed.presets.iter().all(|preset| preset.built_in));

        fixture
            .player
            .save_eq_preset(" zebra ", &bands(1.0))
            .unwrap();
        let listed = fixture.player.save_eq_preset("Apple", &bands(2.0)).unwrap();
        let names: Vec<&str> = listed
            .presets
            .iter()
            .map(|preset| preset.name.as_str())
            .collect();
        assert_eq!(names[BUILT_IN_PRESETS.len()..], ["Apple", "zebra"]);
        assert!(!listed.presets.last().unwrap().built_in);
    }

    #[test]
    fn saving_a_preset_under_its_own_name_replaces_it() {
        let fixture = fixture();
        fixture.player.save_eq_preset("Mine", &bands(1.0)).unwrap();
        let listed = fixture.player.save_eq_preset("MINE", &bands(2.0)).unwrap();
        let mine: Vec<&EqPreset> = listed
            .presets
            .iter()
            .filter(|preset| !preset.built_in)
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].bands[0], 2.0);
    }

    #[test]
    fn built_in_presets_cannot_be_replaced_or_removed() {
        let fixture = fixture();
        let replace = fixture
            .player
            .save_eq_preset("flat", &bands(1.0))
            .unwrap_err();
        assert_eq!(replace.code, ErrorCode::Conflict);
        let remove = fixture.player.delete_eq_preset("Flat").unwrap_err();
        assert_eq!(remove.code, ErrorCode::Conflict);
        assert_eq!(
            fixture.player.eq_presets().unwrap().presets[0].bands,
            vec![0.0; EQ_BANDS]
        );
    }

    #[test]
    fn a_preset_can_be_deleted_and_a_blank_name_is_refused() {
        let fixture = fixture();
        fixture.player.save_eq_preset("Mine", &bands(1.0)).unwrap();
        let listed = fixture.player.delete_eq_preset("Mine").unwrap();
        assert_eq!(listed.presets.len(), BUILT_IN_PRESETS.len());
        let blank = fixture
            .player
            .save_eq_preset("  ", &bands(1.0))
            .unwrap_err();
        assert_eq!(blank.code, ErrorCode::InvalidArgs);
    }

    #[test]
    fn coverage_counts_the_tags_in_the_library() {
        let fixture = fixture();
        fixture
            .db
            .write(|conn| {
                conn.execute(
                    "UPDATE tracks SET rg_track_gain = -1.0 WHERE public_id IN ('a', 'b')",
                    [],
                )?;
                conn.execute(
                    "UPDATE tracks SET rg_album_gain = -1.0 WHERE public_id = 'a'",
                    [],
                )?;
                Ok::<_, DbError>(())
            })
            .unwrap();
        assert_eq!(
            fixture.player.gain_coverage().unwrap(),
            GainCoverage {
                tracks: 3,
                with_track_gain: 2,
                with_album_gain: 1,
            }
        );
    }

    #[test]
    fn playback_problems_are_logged_and_healthy_playback_is_not() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.signal(state(PlaybackState::Playing, 0, "a", 1000));
        let log = fixture.db.path().with_file_name(HEALTH_LOG_NAME);
        let health = |underruns, longest_gap_us| PlayerEvent::Health {
            underruns,
            queued_ms: 1990,
            longest_gap_us,
        };

        fixture.signal(health(0, 10_000));
        assert!(!log.exists(), "healthy playback writes nothing");

        fixture.signal(health(2, 10_000));
        fixture.signal(health(2, 10_000));
        fixture.signal(health(2, STALL_US + 1));
        let text = fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("new_underruns=2"), "{text}");
        assert!(lines[0].contains("position_ms=1000"), "{text}");
        assert!(lines[0].ends_with('a'), "{text}");
        assert!(lines[1].contains("new_underruns=0"), "{text}");
        assert!(
            lines[1].contains(&format!("longest_gap_us={}", STALL_US + 1)),
            "{text}"
        );
    }

    #[test]
    fn a_track_id_that_left_the_library_does_not_break_the_status() {
        let fixture = fixture();
        fixture.player.play_tracks(&["a".to_owned()], 0).unwrap();
        fixture.signal(state(PlaybackState::Playing, 0, &new_public_id(), 0));
        assert_eq!(fixture.player.status().track, None);
    }
}
