//! The audio engine running inside this process, behind `PlayerBackend`.
//!
//! The player's event handlers read and write the database, which can wait on a scan.
//! They run on their own thread, so a slow handler can never hold up decoding.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use umak_audio::analysis::{self, Visualizer};
use umak_audio::engine::{ChainParts, Engine};
use umak_audio::output::{AudioOutput, CpalOutput, NullOutput};
use umak_audio::protocol::PlayerCommand;

use crate::player::{BackendEvents, BackendSignal, PlayerBackend};

/// For tests of the app: `null` uses no device at all, and `muted` runs the real device
/// at zero gain, so timing is real but nothing is audible.
const OUTPUT_VARIABLE: &str = "UMAK_PLAY_OUTPUT";
const NULL_RATE: u32 = 48_000;
const NULL_CHANNELS: u16 = 2;

pub struct EnginePlayer {
    engine: Engine,
    closing: Arc<AtomicBool>,
    /// Holds the gain at zero whatever volume is asked for, for tests of the app.
    muted: bool,
    /// Dropped after the engine, whose audio thread holds its tap.
    visualizer: Visualizer,
}

impl EnginePlayer {
    /// Starts an engine on the default output device.
    pub fn spawn(events: BackendEvents) -> Result<Box<dyn PlayerBackend>, String> {
        let output = std::env::var(OUTPUT_VARIABLE).unwrap_or_default();
        let null_output = output == "null";
        let mut player = Self::with_output(
            move || -> Result<Box<dyn AudioOutput>, String> {
                if null_output {
                    Ok(Box::new(NullOutput::new(NULL_RATE, NULL_CHANNELS)))
                } else {
                    Ok(Box::new(CpalOutput::default_device()?))
                }
            },
            events,
        );
        if output == "muted" {
            player.muted = true;
            player.engine.send(PlayerCommand::SetVolume { volume: 0.0 });
        }
        Ok(Box::new(player))
    }

    fn with_output(
        make_output: impl FnOnce() -> Result<Box<dyn AudioOutput>, String> + Send + 'static,
        events: BackendEvents,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        // Frames go straight out from the visualizer's thread, so a slow handler of the
        // other events cannot hold them up.
        let visual_events = Arc::clone(&events);
        let (tap, visualizer) =
            analysis::visualizer(move |frame| visual_events(BackendSignal::Visuals(frame)));
        let chain = ChainParts {
            tap: Some(Box::new(tap)),
            ..ChainParts::default()
        };
        let engine = Engine::spawn(make_output, chain, move |event| {
            let _ = sender.send(event);
        });
        let closing = Arc::new(AtomicBool::new(false));
        let handler_closing = Arc::clone(&closing);
        // The channel closes when the engine thread ends, however it ends.
        thread::spawn(move || {
            for event in receiver {
                events(BackendSignal::Event(event));
            }
            if !handler_closing.load(Ordering::SeqCst) {
                events(BackendSignal::Exited);
            }
        });
        Self {
            engine,
            closing,
            muted: false,
            visualizer,
        }
    }
}

impl PlayerBackend for EnginePlayer {
    fn send(&mut self, command: &PlayerCommand) -> Result<(), String> {
        if self.muted && matches!(command, PlayerCommand::SetVolume { .. }) {
            return Ok(());
        }
        self.engine.try_send(command.clone())
    }

    fn set_visuals(&mut self, enabled: bool) {
        self.visualizer.set_enabled(enabled);
    }
}

impl Drop for EnginePlayer {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use rusqlite::params;
    use umak_audio::output::OutputFormat;
    use umak_audio::protocol::{PlaybackState, PlayerEvent, TrackEntry, VisualFrame};
    use umak_audio::render::Renderer;

    use super::*;
    use crate::api::{CoreEvent, EventSink};
    use crate::db::{Database, DbError};
    use crate::player::Player;

    const RATE: u32 = 48_000;
    const WAIT: Duration = Duration::from_secs(20);

    /// One second of a 16-bit stereo tone.
    fn wav(path: &Path) {
        let mut samples = Vec::new();
        for frame in 0..RATE {
            let value = (frame as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin();
            let sample = (value * 0.5 * f32::from(i16::MAX)) as i16;
            samples.extend_from_slice(&sample.to_le_bytes());
            samples.extend_from_slice(&sample.to_le_bytes());
        }
        let mut file = b"RIFF".to_vec();
        file.extend((36 + samples.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        file.extend(16_u32.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(2_u16.to_le_bytes());
        file.extend(RATE.to_le_bytes());
        file.extend((RATE * 4).to_le_bytes());
        file.extend(4_u16.to_le_bytes());
        file.extend(16_u16.to_le_bytes());
        file.extend_from_slice(b"data");
        file.extend((samples.len() as u32).to_le_bytes());
        file.extend(samples);
        fs::write(path, file).unwrap();
    }

    fn wait_until(mut done: impl FnMut() -> bool, what: &str) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Pulls samples as fast as it can and counts the ones holding audio.
    struct CountingOutput {
        audio_samples: Arc<AtomicUsize>,
    }

    impl AudioOutput for CountingOutput {
        fn format(&self) -> OutputFormat {
            OutputFormat {
                device: "counting".to_owned(),
                sample_rate: RATE,
                channels: 2,
            }
        }

        fn start(self: Box<Self>, mut renderer: Renderer) -> Result<Box<dyn Any>, String> {
            let (stop, stopped) = mpsc::channel::<()>();
            thread::spawn(move || {
                let mut buffer = vec![0.0_f32; 2048];
                while let Err(mpsc::TryRecvError::Empty) = stopped.try_recv() {
                    renderer.render(&mut buffer);
                    let audio = buffer.iter().filter(|sample| **sample != 0.0).count();
                    self.audio_samples.fetch_add(audio, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(1));
                }
            });
            Ok(Box::new(stop))
        }
    }

    #[test]
    fn a_blocked_event_handler_does_not_stop_the_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        wav(&path);
        let audio_samples = Arc::new(AtomicUsize::new(0));
        let output = CountingOutput {
            audio_samples: Arc::clone(&audio_samples),
        };
        // The handler waits on its first event until the test ends.
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let events: BackendEvents = Arc::new(move |_| {
            let _ = released.lock().unwrap().recv();
        });
        let mut player = EnginePlayer::with_output(move || Ok(Box::new(output)), events);
        player
            .send(&PlayerCommand::Load {
                tracks: vec![TrackEntry {
                    id: "tone".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                    gain: Default::default(),
                }],
                start_index: 0,
                start_ms: 0,
            })
            .unwrap();

        // Nearly every sample of the tone is nonzero, so this is almost the whole track.
        let expected = (RATE as usize * 2) * 9 / 10;
        wait_until(
            || audio_samples.load(Ordering::SeqCst) >= expected,
            "the track to play with its handler blocked",
        );
        drop(release);
    }

    #[test]
    fn a_muted_player_ignores_the_saved_volume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        wav(&path);
        let audio_samples = Arc::new(AtomicUsize::new(0));
        let output = CountingOutput {
            audio_samples: Arc::clone(&audio_samples),
        };
        let mut player = EnginePlayer::with_output(move || Ok(Box::new(output)), Arc::new(|_| {}));
        player.muted = true;
        player.engine.send(PlayerCommand::SetVolume { volume: 0.0 });
        player
            .send(&PlayerCommand::SetVolume { volume: 1.0 })
            .unwrap();
        player
            .send(&PlayerCommand::Load {
                tracks: vec![TrackEntry {
                    id: "tone".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                    gain: Default::default(),
                }],
                start_index: 0,
                start_ms: 0,
            })
            .unwrap();
        thread::sleep(Duration::from_millis(200));
        assert_eq!(audio_samples.load(Ordering::SeqCst), 0);
    }

    fn signal_log() -> (BackendEvents, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&log);
        let events: BackendEvents = Arc::new(move |signal| {
            writer.lock().unwrap().push(match signal {
                BackendSignal::Event(PlayerEvent::Error { message }) => message,
                BackendSignal::Event(_) | BackendSignal::Visuals(_) => "event".to_owned(),
                BackendSignal::Exited => "exited".to_owned(),
            });
        });
        (events, log)
    }

    #[test]
    fn an_engine_that_ends_on_its_own_is_reported_and_refuses_commands() {
        let (events, log) = signal_log();
        let mut player =
            EnginePlayer::with_output(|| Err("no device for this test".to_owned()), events);
        wait_until(
            || {
                log.lock()
                    .unwrap()
                    .last()
                    .is_some_and(|last| last == "exited")
            },
            "the engine's end to be reported",
        );
        assert_eq!(log.lock().unwrap()[0], "no device for this test");
        assert!(player.send(&PlayerCommand::Play).is_err());
    }

    #[test]
    fn closing_the_player_is_not_reported_as_a_failure() {
        let (events, log) = signal_log();
        let player = EnginePlayer::with_output(|| Ok(Box::new(NullOutput::new(RATE, 2))), events);
        drop(player);
        thread::sleep(Duration::from_millis(100));
        assert!(!log.lock().unwrap().iter().any(|entry| entry == "exited"));
    }

    #[derive(Clone, Default)]
    struct Recording(Arc<Mutex<Vec<CoreEvent>>>);

    impl EventSink for Recording {
        fn emit(&self, event: &CoreEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    /// A library holding one track, `tone`: a second of 440 Hz.
    fn tone_library(dir: &Path) -> Arc<Database> {
        wav(&dir.join("tone.wav"));
        let db = Arc::new(Database::open(dir.join("umak.db")).unwrap());
        let root = dir.to_string_lossy().into_owned();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                 VALUES (1, 'r', 'Music', ?1, 0)",
                [&root],
            )?;
            conn.execute_batch(
                "INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                 VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x');",
            )?;
            conn.execute(
                "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                     title_sort, artist_sort, album_sort, album_artist_sort, added_at, updated_at)
                 VALUES (?1, 1, 'tone.wav', 1, 1, ?1, 1, ?1, 'x', 'x', 'x', 0, 0)",
                params!["tone"],
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();
        db
    }

    fn null_player(db: Arc<Database>, recording: &Recording) -> Player {
        Player::new(
            db,
            Arc::new(recording.clone()),
            Box::new(|events| {
                Ok(Box::new(EnginePlayer::with_output(
                    || Ok(Box::new(NullOutput::new(RATE, 2))),
                    events,
                )))
            }),
        )
    }

    #[test]
    fn the_player_plays_a_library_track_through_the_engine_to_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let recording = Recording::default();
        let player = null_player(tone_library(dir.path()), &recording);
        player.play_tracks(&["tone".to_owned()], 0).unwrap();

        let states = || -> Vec<PlaybackState> {
            let events = recording.0.lock().unwrap();
            events
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::PlayerChanged { status } => Some(status.state),
                    _ => None,
                })
                .collect()
        };
        wait_until(
            || states().last() == Some(&PlaybackState::Stopped),
            "the track to finish",
        );
        assert!(states().contains(&PlaybackState::Playing));
        let errors: Vec<CoreEvent> = recording
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, CoreEvent::PlayerError { .. }))
            .cloned()
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn frames_of_what_plays_come_while_visuals_are_on() {
        let dir = tempfile::tempdir().unwrap();
        let recording = Recording::default();
        let player = null_player(tone_library(dir.path()), &recording);
        let frames = || -> Vec<VisualFrame> {
            recording
                .0
                .lock()
                .unwrap()
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::VisualFrame { frame } => Some(frame.clone()),
                    _ => None,
                })
                .collect()
        };
        player.set_visuals(true);
        player.play_tracks(&["tone".to_owned()], 0).unwrap();
        // The tone is at half scale, so its band reads about 6 dB down.
        wait_until(
            || {
                frames()
                    .iter()
                    .any(|frame| frame.bands.iter().any(|band| *band > -10.0))
            },
            "a frame showing the tone",
        );

        player.set_visuals(false);
        thread::sleep(Duration::from_millis(50));
        let seen = frames().len();
        thread::sleep(Duration::from_millis(200));
        assert_eq!(frames().len(), seen, "frames after visuals were turned off");
    }
}
