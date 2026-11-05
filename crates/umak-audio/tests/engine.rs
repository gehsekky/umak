//! End-to-end engine tests. A test output captures what would reach the device, so
//! pitch, duration, ordering and seek position can be measured instead of listened to.

use std::any::Any;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use umak_audio::effects::{Effect, EffectChain, OutputTap};
use umak_audio::engine::{ChainParts, Engine, check_file};
use umak_audio::output::{AudioOutput, NullOutput, OutputFormat};
use umak_audio::protocol::{PlaybackState, PlayerCommand, PlayerEvent, TrackEntry};
use umak_audio::render::Renderer;

const OUTPUT_RATE: u32 = 48_000;
const WAIT: Duration = Duration::from_secs(20);

/// A 16-bit stereo WAV playing each `(frequency, seconds)` tone in turn.
fn wav(path: &Path, rate: u32, tones: &[(f32, f32)]) {
    let mut samples = Vec::new();
    for (frequency, seconds) in tones {
        let frames = (rate as f32 * seconds) as u32;
        for frame in 0..frames {
            let value = (frame as f32 * frequency * std::f32::consts::TAU / rate as f32).sin();
            let sample = (value * 0.5 * f32::from(i16::MAX)) as i16;
            samples.extend_from_slice(&sample.to_le_bytes());
            samples.extend_from_slice(&sample.to_le_bytes());
        }
    }
    let mut file = b"RIFF".to_vec();
    file.extend((36 + samples.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend(16_u32.to_le_bytes());
    file.extend(1_u16.to_le_bytes());
    file.extend(2_u16.to_le_bytes());
    file.extend(rate.to_le_bytes());
    file.extend((rate * 4).to_le_bytes());
    file.extend(4_u16.to_le_bytes());
    file.extend(16_u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend((samples.len() as u32).to_le_bytes());
    file.extend(samples);
    fs::write(path, file).unwrap();
}

/// Pulls samples as fast as the test needs, keeping only real audio.
struct CaptureOutput {
    captured: Arc<Mutex<Vec<f32>>>,
    stop: Arc<AtomicBool>,
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl AudioOutput for CaptureOutput {
    fn format(&self) -> OutputFormat {
        OutputFormat {
            device: "capture".to_owned(),
            sample_rate: OUTPUT_RATE,
            channels: 2,
        }
    }

    fn start(self: Box<Self>, mut renderer: Renderer) -> Result<Box<dyn Any>, String> {
        let (captured, stop) = (self.captured, self.stop);
        let guard = StopOnDrop(Arc::clone(&stop));
        thread::spawn(move || {
            let shared = Arc::clone(renderer.shared());
            let mut buffer = vec![0.0_f32; 2048];
            let mut last_played = 0;
            while !stop.load(Ordering::SeqCst) {
                renderer.render(&mut buffer);
                let played = shared.played_frames();
                let fresh = ((played - last_played) as usize * 2).min(buffer.len());
                last_played = played;
                if fresh > 0 {
                    captured.lock().unwrap().extend_from_slice(&buffer[..fresh]);
                }
                thread::sleep(Duration::from_micros(100));
            }
        });
        Ok(Box::new(guard))
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    engine: Engine,
    events: mpsc::Receiver<PlayerEvent>,
    captured: Arc<Mutex<Vec<f32>>>,
    seen: Vec<PlayerEvent>,
}

fn harness_with(chain: ChainParts) -> Harness {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let output = CaptureOutput {
        captured: Arc::clone(&captured),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let (sender, events) = mpsc::channel();
    let engine = Engine::spawn(
        move || Ok(Box::new(output) as Box<dyn AudioOutput>),
        chain,
        move |event| {
            let _ = sender.send(event);
        },
    );
    Harness {
        _dir: tempfile::tempdir().unwrap(),
        engine,
        events,
        captured,
        seen: Vec::new(),
    }
}

fn harness() -> Harness {
    harness_with(ChainParts::default())
}

impl Harness {
    fn file(&self, name: &str, rate: u32, tones: &[(f32, f32)]) -> TrackEntry {
        let path = self._dir.path().join(format!("{name}.wav"));
        wav(&path, rate, tones);
        TrackEntry {
            id: name.to_owned(),
            path: path.to_str().unwrap().to_owned(),
        }
    }

    fn load(&self, tracks: Vec<TrackEntry>, start_index: usize, start_ms: u64) {
        self.engine.send(PlayerCommand::Load {
            tracks,
            start_index,
            start_ms,
        });
    }

    /// Waits for an event that satisfies `wanted`, keeping every event seen.
    fn wait_for(&mut self, wanted: impl Fn(&PlayerEvent) -> bool) -> PlayerEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self
                .events
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timed out; saw {:#?}", self.seen));
            self.seen.push(event.clone());
            if wanted(&event) {
                return event;
            }
        }
    }

    fn wait_for_stop(&mut self) {
        self.wait_for(|event| {
            matches!(
                event,
                PlayerEvent::State {
                    state: PlaybackState::Stopped,
                    ..
                }
            )
        });
    }

    fn wait_for_track(&mut self, id: &str) -> PlayerEvent {
        self.wait_for(|event| {
            matches!(
                event,
                PlayerEvent::State { track_id: Some(track), state: PlaybackState::Playing, .. } if track == id
            )
        })
    }

    fn take_audio(&self) -> Vec<f32> {
        std::mem::take(&mut *self.captured.lock().unwrap())
    }

    fn wait_for_frames(&self, frames: usize) {
        let deadline = Instant::now() + WAIT;
        while self.captured.lock().unwrap().len() < frames * 2 {
            assert!(Instant::now() < deadline, "audio never arrived");
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Cycles per second in a stretch of interleaved stereo audio.
fn frequency(samples: &[f32]) -> f32 {
    let left: Vec<f32> = samples.chunks_exact(2).map(|frame| frame[0]).collect();
    let crossings = left
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    crossings as f32 * OUTPUT_RATE as f32 / left.len() as f32
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < expected * 0.02
}

#[test]
fn a_track_plays_at_the_right_pitch_and_length_through_the_resampler() {
    for rate in [44_100, 48_000, 96_000, 192_000] {
        let mut player = harness();
        let track = player.file("tone", rate, &[(1000.0, 1.0)]);
        player.load(vec![track], 0, 0);
        player.wait_for_stop();

        let audio = player.take_audio();
        let frames = audio.len() / 2;
        assert!(
            frames.abs_diff(OUTPUT_RATE as usize) < 64,
            "{rate}: {frames} frames"
        );
        let pitch = frequency(&audio);
        assert!(near(pitch, 1000.0), "{rate}: {pitch} Hz");
        assert!(
            audio.iter().any(|sample| sample.abs() > 0.4),
            "{rate}: silent"
        );
    }
}

#[test]
fn tracks_play_in_order_and_the_list_ends_stopped() {
    let mut player = harness();
    let tracks = vec![
        player.file("a", 48_000, &[(500.0, 0.2)]),
        player.file("b", 48_000, &[(2000.0, 0.2)]),
    ];
    player.load(tracks, 0, 0);
    player.wait_for_track("a");
    player.wait_for_track("b");
    player.wait_for_stop();

    let audio = player.take_audio();
    let middle = audio.len() / 2;
    assert!(near(frequency(&audio[..middle]), 500.0));
    assert!(near(frequency(&audio[middle..]), 2000.0));
    let last = player.seen.last().unwrap();
    assert!(matches!(
        last,
        PlayerEvent::State {
            index: None,
            track_id: None,
            ..
        }
    ));
}

#[test]
fn a_load_starts_at_the_chosen_track_and_position() {
    let mut player = harness();
    let tracks = vec![
        player.file("a", 48_000, &[(500.0, 0.2)]),
        player.file("b", 48_000, &[(600.0, 1.0), (3000.0, 1.0)]),
    ];
    player.load(tracks, 1, 1000);
    let started = player.wait_for_track("b");
    assert!(matches!(
        started,
        PlayerEvent::State {
            position_ms: 1000,
            ..
        }
    ));
    player.wait_for_stop();

    let audio = player.take_audio();
    let frames = audio.len() / 2;
    assert!(
        frames.abs_diff(OUTPUT_RATE as usize) < 64,
        "{frames} frames"
    );
    assert!(near(frequency(&audio), 3000.0));
}

#[test]
fn a_seek_lands_exactly_on_the_requested_audio_and_drops_what_was_queued() {
    let mut player = harness();
    // Each second has its own tone, so the audio itself says where playback is.
    let track = player.file(
        "steps",
        44_100,
        &[(400.0, 1.0), (800.0, 1.0), (1600.0, 1.0), (3200.0, 1.0)],
    );
    player.load(vec![track], 0, 0);
    player.wait_for_track("steps");
    player.wait_for_frames(4800);
    player.engine.send(PlayerCommand::Pause);
    player.wait_for(|event| {
        matches!(
            event,
            PlayerEvent::State {
                state: PlaybackState::Paused,
                ..
            }
        )
    });
    player.engine.send(PlayerCommand::Seek { ms: 2000 });
    player.wait_for(|event| {
        matches!(
            event,
            PlayerEvent::State {
                position_ms: 2000,
                ..
            }
        )
    });
    player.take_audio();

    player.engine.send(PlayerCommand::Play);
    player.wait_for_frames(4800);
    let audio = player.take_audio();
    let pitch = frequency(&audio[..9600.min(audio.len())]);
    assert!(near(pitch, 1600.0), "after the seek: {pitch} Hz");

    player.wait_for_stop();
}

#[test]
fn pause_holds_the_position_and_play_continues_it() {
    let mut player = harness();
    let track = player.file("tone", 48_000, &[(440.0, 3.0)]);
    player.load(vec![track], 0, 0);
    player.wait_for_track("tone");
    player.wait_for_frames(4800);
    player.engine.send(PlayerCommand::Pause);
    player.wait_for(|event| {
        matches!(
            event,
            PlayerEvent::State {
                state: PlaybackState::Paused,
                ..
            }
        )
    });

    thread::sleep(Duration::from_millis(30));
    let held = player.take_audio().len();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        player.take_audio().len(),
        0,
        "audio flowed while paused ({held} before)"
    );

    player.engine.send(PlayerCommand::Play);
    player.wait_for_frames(4800);
}

#[test]
fn stop_ends_playback_and_play_restarts_the_track() {
    let mut player = harness();
    let track = player.file("tone", 48_000, &[(440.0, 3.0)]);
    player.load(vec![track], 0, 0);
    player.wait_for_track("tone");
    player.wait_for_frames(4800);

    player.engine.send(PlayerCommand::Stop);
    player.wait_for_stop();
    thread::sleep(Duration::from_millis(30));
    player.take_audio();
    thread::sleep(Duration::from_millis(50));
    assert!(player.take_audio().is_empty());

    player.engine.send(PlayerCommand::Play);
    let started = player.wait_for_track("tone");
    assert!(matches!(started, PlayerEvent::State { position_ms: 0, .. }));
}

#[test]
fn next_and_previous_move_through_the_list() {
    let mut player = harness();
    let tracks = vec![
        player.file("a", 48_000, &[(440.0, 5.0)]),
        player.file("b", 48_000, &[(440.0, 5.0)]),
        player.file("c", 48_000, &[(440.0, 5.0)]),
    ];
    player.load(tracks, 0, 0);
    player.wait_for_track("a");
    player.engine.send(PlayerCommand::Next);
    player.wait_for_track("b");
    player.engine.send(PlayerCommand::Next);
    player.wait_for_track("c");
    player.engine.send(PlayerCommand::Previous);
    player.wait_for_track("b");
    player.engine.send(PlayerCommand::Previous);
    player.wait_for_track("a");
    // At the start of the list, Previous stays on the first track.
    player.engine.send(PlayerCommand::Previous);
    player.wait_for_track("a");
    player.engine.send(PlayerCommand::Next);
    player.wait_for_track("b");
    player.engine.send(PlayerCommand::Next);
    player.wait_for_track("c");
    // Past the end of the list, playback stops.
    player.engine.send(PlayerCommand::Next);
    player.wait_for_stop();
}

#[test]
fn previous_restarts_a_track_that_has_played_for_a_while() {
    let mut player = harness();
    let tracks = vec![
        player.file("a", 48_000, &[(440.0, 1.0)]),
        player.file("b", 48_000, &[(440.0, 8.0)]),
    ];
    player.load(tracks, 1, 4000);
    player.wait_for_track("b");
    player.engine.send(PlayerCommand::Previous);
    let event = player.wait_for(|event| matches!(event, PlayerEvent::State { position_ms: 0, .. }));
    assert!(matches!(event, PlayerEvent::State { track_id: Some(id), .. } if id == "b"));
}

#[test]
fn a_track_that_cannot_open_is_reported_and_skipped() {
    let mut player = harness();
    let good = player.file("good", 48_000, &[(440.0, 0.2)]);
    let gone = TrackEntry {
        id: "gone".to_owned(),
        path: player
            ._dir
            .path()
            .join("gone.flac")
            .to_str()
            .unwrap()
            .to_owned(),
    };
    let garbage_path = player._dir.path().join("garbage.mp3");
    fs::write(&garbage_path, b"this is not audio at all").unwrap();
    let garbage = TrackEntry {
        id: "garbage".to_owned(),
        path: garbage_path.to_str().unwrap().to_owned(),
    };
    player.load(vec![gone, garbage, good], 0, 0);
    player.wait_for_track("good");
    let failed: Vec<&str> = player
        .seen
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::TrackFailed { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(failed, ["gone", "garbage"]);
    player.wait_for_stop();
}

#[test]
fn a_list_of_only_bad_tracks_ends_stopped() {
    let mut player = harness();
    let gone = TrackEntry {
        id: "gone".to_owned(),
        path: "no/such/file.flac".to_owned(),
    };
    player.load(vec![gone], 0, 0);
    player.wait_for_stop();
}

#[test]
fn position_events_advance_while_playing() {
    // A real-time output, because position is reported a few times a second.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    wav(&path, 48_000, &[(440.0, 6.0)]);
    let (sender, events) = mpsc::channel();
    let engine = Engine::spawn(
        || Ok(Box::new(NullOutput::new(OUTPUT_RATE, 2)) as Box<dyn AudioOutput>),
        ChainParts::default(),
        move |event| {
            let _ = sender.send(event);
        },
    );
    engine.send(PlayerCommand::Load {
        tracks: vec![TrackEntry {
            id: "tone".to_owned(),
            path: path.to_str().unwrap().to_owned(),
        }],
        start_index: 0,
        start_ms: 0,
    });
    let started = Instant::now();
    let mut positions = Vec::new();
    while positions.len() < 4 {
        let event = events.recv_timeout(WAIT).unwrap();
        if let PlayerEvent::Position { position_ms } = event {
            positions.push(position_ms);
        }
    }
    let elapsed = started.elapsed().as_millis() as u64;
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{positions:?}"
    );
    // The position tracks real time, not how fast the file decodes.
    let last = *positions.last().unwrap();
    assert!(last.abs_diff(elapsed) < 250, "{last} ms after {elapsed} ms");
}

struct Invert;

impl Effect for Invert {
    fn process(&mut self, samples: &mut [f32], _channels: usize) {
        for sample in samples {
            *sample = -*sample;
        }
    }
}

struct Recording(Arc<Mutex<f32>>);

impl OutputTap for Recording {
    fn on_output(&mut self, samples: &[f32], _channels: usize) {
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        let mut stored = self.0.lock().unwrap();
        *stored = stored.max(peak);
    }
}

#[test]
fn the_effect_chain_changes_samples_and_the_tap_sees_the_result() {
    let peak = Arc::new(Mutex::new(0.0));
    let mut player = harness_with(ChainParts {
        effects: EffectChain::new(vec![Box::new(Invert)]),
        tap: Some(Box::new(Recording(Arc::clone(&peak)))),
    });
    let track = player.file("tone", 48_000, &[(1000.0, 0.2)]);
    player.load(vec![track], 0, 0);
    player.wait_for_stop();

    let audio = player.take_audio();
    // The WAV's first half-cycle is positive, so inversion makes it negative.
    let first = audio.iter().find(|sample| sample.abs() > 0.01).unwrap();
    assert!(*first < 0.0, "first sample {first}");
    assert!(*peak.lock().unwrap() > 0.4);
}

#[test]
fn volume_scales_the_output() {
    let mut player = harness();
    player.engine.send(PlayerCommand::SetVolume { volume: 0.5 });
    let track = player.file("tone", 48_000, &[(1000.0, 0.3)]);
    player.load(vec![track], 0, 0);
    player.wait_for_stop();
    let peak = player
        .take_audio()
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!((peak - 0.25).abs() < 0.02, "peak {peak}");
}

#[test]
fn a_full_decode_check_reports_the_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    wav(&path, 96_000, &[(1000.0, 0.5)]);
    let check = check_file(path.to_str().unwrap(), 48_000).unwrap();
    assert_eq!(check.sample_rate, 96_000);
    assert_eq!(check.channels, 2);
    assert_eq!(check.frames, 48_000);
    assert!(
        check.output_frames.abs_diff(24_000) < 8,
        "{}",
        check.output_frames
    );
    assert_eq!(check.decode_errors, 0);
    assert_eq!(check.duration_ms, Some(500));
    assert!(check_file("no/such/file.flac", 48_000).is_err());
}
