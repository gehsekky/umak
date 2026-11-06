//! The player client. The core owns the player playlist in the database and talks to
//! an engine through `PlayerBackend`, so where the engine runs can change without
//! changing callers. Every way of playing something replaces that one list.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use umak_audio::protocol::{PlaybackState, PlayerCommand, PlayerEvent, TrackEntry};

use crate::api::{ApiError, CoreEvent, ErrorCode, EventSink};
use crate::db::{Database, DbError};
use crate::library::{TRACK_COLUMNS, TrackRow};
use crate::playlists::{self, PlayerEdit, PlaylistPage};

/// How much playback can be lost if the app ends without warning.
const SAVE_POSITION_EVERY_MS: u64 = 5000;
/// Written next to the database when playback has a problem, so a stutter leaves
/// evidence behind. Each line is tab-separated.
const HEALTH_LOG_NAME: &str = "player-health.log";
const MAX_HEALTH_LOG_BYTES: u64 = 256 * 1024;
/// A device normally asks for audio every 10 ms or so. This is well past that.
const STALL_US: u64 = 100_000;

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

/// An engine that takes commands. Its events go to the callback it was created with.
pub trait PlayerBackend: Send {
    fn send(&mut self, command: &PlayerCommand) -> Result<(), String>;
}

pub type BackendEvents = Arc<dyn Fn(BackendSignal) + Send + Sync>;
pub type BackendFactory =
    Box<dyn FnMut(BackendEvents) -> Result<Box<dyn PlayerBackend>, String> + Send>;

pub enum BackendSignal {
    Event(PlayerEvent),
    /// The engine ended without being asked to.
    Exited,
}

/// For a core that does not play, such as a headless tool.
pub fn no_engine(_events: BackendEvents) -> Result<Box<dyn PlayerBackend>, String> {
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

    fn handle(&self, signal: BackendSignal) {
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
        }
    }

    fn handle_event(&self, event: PlayerEvent) {
        match event {
            PlayerEvent::Ready { device, .. } => {
                lock(&self.status).device = Some(device);
            }
            PlayerEvent::State {
                state,
                index,
                track_id,
                position_ms,
                duration_ms,
            } => {
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
        }
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
    backend: Mutex<Option<Box<dyn PlayerBackend>>>,
    factory: Mutex<BackendFactory>,
    /// Held across a change to the list and its message to the engine, because an edit
    /// only makes sense applied to the list before it.
    list: Mutex<()>,
}

impl Player {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, factory: BackendFactory) -> Self {
        Self {
            shared: Arc::new(Shared {
                db,
                events,
                status: Mutex::default(),
                loaded: AtomicBool::new(false),
                saved_position_ms: Mutex::new(0),
                seen_underruns: Mutex::new(0),
            }),
            backend: Mutex::new(None),
            factory: Mutex::new(factory),
            list: Mutex::new(()),
        }
    }

    pub fn status(&self) -> PlayerStatus {
        lock(&self.shared.status).clone()
    }

    fn failure(message: String) -> ApiError {
        ApiError::new(ErrorCode::Internal, message)
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
        let shared = Arc::clone(&self.shared);
        let callback: BackendEvents = Arc::new(move |signal| shared.handle(signal));
        let mut started = (lock(&self.factory))(callback).map_err(Self::failure)?;
        started.send(command).map_err(Self::failure)?;
        *backend = Some(started);
        Ok(())
    }

    fn load(&self, tracks: Vec<TrackEntry>, start: usize, start_ms: u64) -> Result<(), ApiError> {
        lock(&self.shared.status).count = tracks.len() as u32;
        self.send(&PlayerCommand::Load {
            tracks,
            start_index: start,
            start_ms,
        })?;
        self.shared.loaded.store(true, Ordering::SeqCst);
        Ok(())
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
            PlayerAction::Play if !loaded => self.restore(),
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

    /// Resumes the last session's list and position.
    fn restore(&self) -> Result<(), ApiError> {
        let _list = lock(&self.list);
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
        self.load(tracks, start, start_ms)
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
        let factory: BackendFactory = Box::new(move |callback| {
            *factory_engine.signal.lock().unwrap() = Some(callback);
            *factory_engine.starts.lock().unwrap() += 1;
            *factory_engine.broken.lock().unwrap() = false;
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
        fn commands(&self) -> Vec<PlayerCommand> {
            self.engine.commands.lock().unwrap().clone()
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
        let factory: BackendFactory = Box::new(move |callback| {
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
        } = &commands[0]
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
