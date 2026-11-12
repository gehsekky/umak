//! The operating system's media controls: on Windows the media overlay, the lock screen and
//! the media keys, through the System Media Transport Controls.
//!
//! A `MediaSession` follows the core's player events to show what is playing, with its cover
//! art, and turns presses of the keys and buttons into ordinary core commands, which the app
//! runs. The controls live on a thread of their own, since they may not be shared between
//! threads.
//!
//! This crate must never depend on Tauri or UI code.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use umak_audio::protocol::PlaybackState;
use umak_core::api::{CoreEvent, EventSink};
use umak_core::player::PlayerStatus;

/// A position this far from where playback should be is a seek, which the controls are told.
const DRIFT_MS: u64 = 1500;

/// Runs a core command by name with JSON arguments.
pub type RunCommand = Arc<dyn Fn(&'static str, Value) + Send + Sync>;

/// How far the seek buttons move, read at each press: the app's own skip length.
pub type SkipLength = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The picture file to show for a track. The controls keep the last picture they were given,
/// so a track with no art needs a placeholder. Called again when `artReady` names the track.
pub type FindCover = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// What the controls show. Nothing is shown with no track.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub state: PlaybackState,
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
}

pub fn now_playing(status: &PlayerStatus) -> Option<NowPlaying> {
    let track = status.track.as_ref()?;
    if status.state == PlaybackState::Stopped {
        return None;
    }
    Some(NowPlaying {
        state: status.state,
        track_id: track.id.clone(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration_ms: status
            .duration_ms
            .or(track.duration_ms)
            .and_then(|ms| u64::try_from(ms).ok()),
    })
}

/// Where playback is, as last reported.
#[derive(Debug, Clone)]
struct Position {
    playing: bool,
    position_ms: u64,
    at: Instant,
}

impl Position {
    /// Where playback should be by now.
    fn now_ms(&self) -> u64 {
        if self.playing {
            self.position_ms + self.at.elapsed().as_millis() as u64
        } else {
            self.position_ms
        }
    }
}

/// The core command for a press, given where playback is.
fn command_for(
    event: &MediaControlEvent,
    playing: bool,
    position_ms: u64,
    skip_ms: u64,
) -> Option<(&'static str, Value)> {
    let control = |action: &str| Some(("player.control", json!({ "action": action })));
    let seek = |ms: u64| Some(("player.seek", json!({ "ms": ms })));
    let moved = |direction: &SeekDirection, by: u64| match direction {
        SeekDirection::Forward => position_ms.saturating_add(by),
        SeekDirection::Backward => position_ms.saturating_sub(by),
    };
    match event {
        MediaControlEvent::Play => control("play"),
        MediaControlEvent::Pause => control("pause"),
        MediaControlEvent::Toggle => control(if playing { "pause" } else { "play" }),
        MediaControlEvent::Next => control("next"),
        MediaControlEvent::Previous => control("previous"),
        MediaControlEvent::Stop => control("stop"),
        MediaControlEvent::Seek(direction) => seek(moved(direction, skip_ms)),
        MediaControlEvent::SeekBy(direction, by) => seek(moved(direction, by.as_millis() as u64)),
        MediaControlEvent::SetPosition(MediaPosition(at)) => seek(at.as_millis() as u64),
        _ => None,
    }
}

/// The URL the controls take for a picture file. On Windows they want the file's own path,
/// with backslashes.
fn cover_url(file: &Path) -> String {
    let path = file.to_string_lossy();
    if cfg!(windows) {
        format!("file://{}", path.replace('/', "\\"))
    } else {
        format!("file://{path}")
    }
}

enum Message {
    Show(Option<NowPlaying>, u64),
    /// The cover of the track shown may have been found, with where playback is now.
    Cover(u64),
    Quit,
}

struct State {
    shown: Option<NowPlaying>,
    position: Position,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The media controls of one window. Dropping it removes them.
pub struct MediaSession {
    sender: Sender<Message>,
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
}

impl MediaSession {
    /// Starts the controls for the window `hwnd`, which Windows needs. Presses go to `run`.
    pub fn start(
        hwnd: Option<*mut c_void>,
        run: RunCommand,
        find_cover: FindCover,
        skip_length: SkipLength,
    ) -> Result<Self, String> {
        let state = Arc::new(Mutex::new(State {
            shown: None,
            position: Position {
                playing: false,
                position_ms: 0,
                at: Instant::now(),
            },
        }));
        let (sender, receiver) = mpsc::channel();
        let (started, started_result) = mpsc::channel();
        // A window handle is only a number, but raw pointers may not cross threads.
        let window = hwnd.map(|hwnd| hwnd as usize);
        let handler_state = Arc::clone(&state);
        let thread = thread::Builder::new()
            .name("umak-media".to_owned())
            .spawn(move || {
                let config = PlatformConfig {
                    display_name: "Umak",
                    dbus_name: "umak",
                    hwnd: window.map(|hwnd| hwnd as *mut c_void),
                };
                let mut controls = match MediaControls::new(config) {
                    Ok(controls) => controls,
                    Err(error) => {
                        let _ = started.send(Err(format!("{error:?}")));
                        return;
                    }
                };
                let attached = controls.attach(move |event| {
                    let position = lock(&handler_state).position.clone();
                    let skip_ms = skip_length();
                    if let Some((name, args)) =
                        command_for(&event, position.playing, position.now_ms(), skip_ms)
                    {
                        run(name, args);
                    }
                });
                if let Err(error) = attached {
                    let _ = started.send(Err(format!("{error:?}")));
                    return;
                }
                let _ = started.send(Ok(()));
                show(&mut controls, &receiver, &find_cover);
                let _ = controls.detach();
            })
            .map_err(|error| error.to_string())?;
        started_result
            .recv()
            .map_err(|_| "the media controls stopped".to_owned())??;
        Ok(Self {
            sender,
            state,
            thread: Some(thread),
        })
    }

    fn follow(&self, status: &PlayerStatus) {
        let now = now_playing(status);
        let position_ms = u64::try_from(status.position_ms).unwrap_or(0);
        let mut state = lock(&self.state);
        state.position = Position {
            playing: status.state == PlaybackState::Playing,
            position_ms,
            at: Instant::now(),
        };
        // Every change of state, of track and every seek comes this way, so each is shown.
        state.shown.clone_from(&now);
        let _ = self.sender.send(Message::Show(now, position_ms));
    }

    fn moved_to(&self, position_ms: u64) {
        let mut state = lock(&self.state);
        let expected = state.position.now_ms();
        state.position.position_ms = position_ms;
        state.position.at = Instant::now();
        // The controls keep time themselves; they need telling only when playback jumps.
        if expected.abs_diff(position_ms) > DRIFT_MS && state.shown.is_some() {
            let _ = self
                .sender
                .send(Message::Show(state.shown.clone(), position_ms));
        }
    }

    fn art_ready(&self, track_ids: &[String]) {
        let state = lock(&self.state);
        if let Some(shown) = &state.shown
            && track_ids.contains(&shown.track_id)
        {
            let _ = self.sender.send(Message::Cover(state.position.now_ms()));
        }
    }
}

impl EventSink for MediaSession {
    fn emit(&self, event: &CoreEvent) {
        match event {
            CoreEvent::PlayerChanged { status } => self.follow(status),
            CoreEvent::PlayerPosition { position_ms } => {
                self.moved_to(u64::try_from(*position_ms).unwrap_or(0));
            }
            CoreEvent::ArtReady { track_ids } => self.art_ready(track_ids),
            _ => {}
        }
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Describes the track, then where playback is, since describing it resets the timeline.
fn describe(
    controls: &mut MediaControls,
    now: &NowPlaying,
    cover: Option<&Path>,
    position_ms: u64,
) -> Result<(), souvlaki::Error> {
    let cover_url = cover.map(cover_url);
    // Every field is given, since one left out keeps the last track's value.
    controls.set_metadata(MediaMetadata {
        title: Some(&now.title),
        artist: Some(&now.artist),
        album: Some(&now.album),
        cover_url: cover_url.as_deref(),
        duration: now.duration_ms.map(Duration::from_millis),
    })?;
    set_position(controls, now, position_ms)
}

fn set_position(
    controls: &mut MediaControls,
    now: &NowPlaying,
    position_ms: u64,
) -> Result<(), souvlaki::Error> {
    let progress = Some(MediaPosition(Duration::from_millis(position_ms)));
    controls.set_playback(match now.state {
        PlaybackState::Playing => MediaPlayback::Playing { progress },
        _ => MediaPlayback::Paused { progress },
    })
}

/// Shows what each message says until told to quit.
fn show(controls: &mut MediaControls, receiver: &mpsc::Receiver<Message>, find_cover: &FindCover) {
    let mut metadata: Option<NowPlaying> = None;
    let mut cover: Option<PathBuf> = None;
    for message in receiver {
        let result = match message {
            Message::Quit => return,
            Message::Cover(position_ms) => {
                let Some(now) = &metadata else { continue };
                let found = find_cover(&now.track_id);
                if found == cover {
                    continue;
                }
                cover = found;
                describe(controls, now, cover.as_deref(), position_ms)
            }
            Message::Show(None, _) => {
                metadata = None;
                controls.set_playback(MediaPlayback::Stopped)
            }
            Message::Show(Some(now), position_ms) => {
                let same_track = metadata.as_ref().is_some_and(|shown| {
                    (
                        &shown.track_id,
                        &shown.title,
                        &shown.artist,
                        &shown.album,
                        shown.duration_ms,
                    ) == (
                        &now.track_id,
                        &now.title,
                        &now.artist,
                        &now.album,
                        now.duration_ms,
                    )
                });
                let result = if same_track {
                    set_position(controls, &now, position_ms)
                } else {
                    cover = find_cover(&now.track_id);
                    describe(controls, &now, cover.as_deref(), position_ms)
                };
                metadata = Some(now);
                result
            }
        };
        if let Err(error) = result {
            eprintln!("failed to update the media controls: {error:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use umak_core::library::TrackRow;

    use super::*;

    fn status(state: PlaybackState, track: bool) -> PlayerStatus {
        PlayerStatus {
            state,
            track: track.then(|| TrackRow {
                id: "t".to_owned(),
                title: "Title".to_owned(),
                artist: "Artist".to_owned(),
                album: "Album".to_owned(),
                duration_ms: Some(1000),
                track_no: 1,
                disc_no: 1,
                year: None,
                codec: None,
                sample_rate: None,
                bit_depth: None,
                missing: false,
                rating: None,
                play_count: 0,
                last_played_at: None,
            }),
            index: Some(0),
            count: 1,
            position_ms: 0,
            duration_ms: Some(2000),
            device: None,
        }
    }

    #[test]
    fn a_track_playing_or_paused_is_shown_and_nothing_else() {
        let shown = now_playing(&status(PlaybackState::Paused, true)).unwrap();
        assert_eq!(shown.title, "Title");
        assert_eq!(shown.track_id, "t");
        assert_eq!(shown.state, PlaybackState::Paused);
        // The player's duration, from the decoder, before the library's.
        assert_eq!(shown.duration_ms, Some(2000));
        assert_eq!(now_playing(&status(PlaybackState::Stopped, true)), None);
        assert_eq!(now_playing(&status(PlaybackState::Playing, false)), None);
    }

    #[test]
    fn presses_become_player_commands() {
        let command = |event, playing| command_for(&event, playing, 10_000, 5000);
        assert_eq!(
            command(MediaControlEvent::Toggle, true),
            Some(("player.control", json!({ "action": "pause" })))
        );
        assert_eq!(
            command(MediaControlEvent::Toggle, false),
            Some(("player.control", json!({ "action": "play" })))
        );
        assert_eq!(
            command(MediaControlEvent::Next, true),
            Some(("player.control", json!({ "action": "next" })))
        );
        assert_eq!(
            command(MediaControlEvent::Seek(SeekDirection::Backward), true),
            Some(("player.seek", json!({ "ms": 5000 })))
        );
        // The skip length the app is set to.
        assert_eq!(
            command_for(
                &MediaControlEvent::Seek(SeekDirection::Forward),
                true,
                10_000,
                15_000
            ),
            Some(("player.seek", json!({ "ms": 25_000 })))
        );
        assert_eq!(
            command(
                MediaControlEvent::SeekBy(SeekDirection::Backward, Duration::from_secs(60)),
                true
            ),
            Some(("player.seek", json!({ "ms": 0 })))
        );
        assert_eq!(
            command(
                MediaControlEvent::SetPosition(MediaPosition(Duration::from_millis(1234))),
                true
            ),
            Some(("player.seek", json!({ "ms": 1234 })))
        );
        assert_eq!(command(MediaControlEvent::Raise, true), None);
        assert_eq!(command(MediaControlEvent::SetVolume(0.5), true), None);
    }

    #[test]
    fn covers_are_given_as_file_urls() {
        let url = cover_url(Path::new("C:/data/art/ab/abc-large.jpg"));
        if cfg!(windows) {
            assert_eq!(url, "file://C:\\data\\art\\ab\\abc-large.jpg");
        } else {
            assert_eq!(url, "file://C:/data/art/ab/abc-large.jpg");
        }
    }

    #[test]
    fn a_playing_position_moves_on_by_itself() {
        let position = Position {
            playing: true,
            position_ms: 1000,
            at: Instant::now() - Duration::from_millis(500),
        };
        assert!((1500..1600).contains(&position.now_ms()));
        let paused = Position {
            playing: false,
            ..position
        };
        assert_eq!(paused.now_ms(), 1000);
    }
}
