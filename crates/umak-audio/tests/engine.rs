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
use umak_audio::protocol::{PlaybackState, PlayerCommand, PlayerEvent, ReplayGainMode, TrackEntry};
use umak_audio::render::Renderer;

const OUTPUT_RATE: u32 = 48_000;
const WAIT: Duration = Duration::from_secs(20);

/// A 16-bit stereo WAV playing each `(frequency, seconds)` tone in turn.
fn wav(path: &Path, rate: u32, tones: &[(f32, f32)]) {
    let mut samples = Vec::new();
    for (frequency, seconds) in tones {
        let frames = (rate as f32 * seconds) as u32;
        samples.extend(tone_bytes(rate, *frequency, 0..frames));
    }
    wav_bytes(path, rate, samples);
}

/// Stereo samples of a tone, so a range can be one piece of a longer tone.
fn tone_bytes(rate: u32, frequency: f32, frames: std::ops::Range<u32>) -> Vec<u8> {
    let mut samples = Vec::new();
    for frame in frames {
        let value =
            (frame as f64 * f64::from(frequency) * std::f64::consts::TAU / f64::from(rate)).sin();
        let sample = (value * 0.5 * f64::from(i16::MAX)) as i16;
        samples.extend_from_slice(&sample.to_le_bytes());
        samples.extend_from_slice(&sample.to_le_bytes());
    }
    samples
}

fn wav_bytes(path: &Path, rate: u32, samples: Vec<u8>) {
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
            gain: Default::default(),
        }
    }

    /// A file holding one piece of a tone, so pieces played in turn make the whole tone.
    fn piece(&self, name: &str, rate: u32, frames: std::ops::Range<u32>) -> TrackEntry {
        let path = self._dir.path().join(format!("{name}.wav"));
        wav_bytes(&path, rate, tone_bytes(rate, 997.0, frames));
        TrackEntry {
            id: name.to_owned(),
            path: path.to_str().unwrap().to_owned(),
            gain: Default::default(),
        }
    }

    /// Plays a list to its end and returns everything that came out.
    fn play_all(&mut self, tracks: Vec<TrackEntry>) -> Vec<f32> {
        self.load(tracks, 0, 0);
        self.wait_for_stop();
        thread::sleep(Duration::from_millis(30));
        self.take_audio()
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
        gain: Default::default(),
    };
    let garbage_path = player._dir.path().join("garbage.mp3");
    fs::write(&garbage_path, b"this is not audio at all").unwrap();
    let garbage = TrackEntry {
        id: "garbage".to_owned(),
        path: garbage_path.to_str().unwrap().to_owned(),
        gain: Default::default(),
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
        gain: Default::default(),
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
            gain: Default::default(),
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

/// The tracks announced as playing, in order, each once.
fn announced(seen: &[PlayerEvent]) -> Vec<String> {
    let mut tracks: Vec<String> = Vec::new();
    for event in seen {
        if let PlayerEvent::State {
            track_id: Some(track),
            state: PlaybackState::Playing,
            ..
        } = event
            && tracks.last() != Some(track)
        {
            tracks.push(track.clone());
        }
    }
    tracks
}

#[test]
fn a_join_at_the_output_rate_adds_and_loses_no_samples() {
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..60_000)]);
    assert_eq!(expected.len(), 60_000 * 2);

    let mut player = harness();
    let pieces = vec![
        player.piece("a", OUTPUT_RATE, 0..12_345),
        player.piece("b", OUTPUT_RATE, 12_345..30_001),
        player.piece("c", OUTPUT_RATE, 30_001..60_000),
    ];
    let joined = player.play_all(pieces);
    assert!(
        joined == expected,
        "the joined audio differs from the whole"
    );
    assert_eq!(announced(&player.seen), ["a", "b", "c"]);
}

#[test]
fn a_join_through_the_resampler_matches_the_whole_track() {
    let rate = 44_100;
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", rate, 0..60_000)]);

    let mut player = harness();
    let pieces = vec![
        player.piece("a", rate, 0..12_345),
        player.piece("b", rate, 12_345..30_001),
        player.piece("c", rate, 30_001..60_000),
    ];
    let joined = player.play_all(pieces);
    assert_eq!(joined.len(), expected.len());
    let worst = joined
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 0.001, "worst difference {worst}");
}

#[test]
fn a_join_between_sample_rates_keeps_each_length_and_leaves_no_gap() {
    let mut player = harness();
    let first = player.file("first", 44_100, &[(440.0, 0.5)]);
    let second = player.file("second", 96_000, &[(880.0, 0.5)]);
    let third = player.file("third", 44_100, &[(1760.0, 0.5)]);
    let audio = player.play_all(vec![first, second, third]);

    let half_second = OUTPUT_RATE as usize / 2;
    assert!(
        (audio.len() / 2).abs_diff(half_second * 3) <= 2,
        "{} frames",
        audio.len() / 2
    );
    let section = |number: usize| {
        let start = (half_second * number + 2000) * 2;
        &audio[start..start + 40_000]
    };
    for (number, tone) in [(0, 440.0), (1, 880.0), (2, 1760.0)] {
        let pitch = frequency(section(number));
        assert!(near(pitch, tone), "section {number}: {pitch} Hz");
    }
    // Sine tones cross zero, but never rest there.
    let mut silent = 0;
    let mut longest_silence = 0;
    for frame in audio.chunks_exact(2) {
        silent = if frame[0].abs() < 1e-4 { silent + 1 } else { 0 };
        longest_silence = longest_silence.max(silent);
    }
    assert!(longest_silence < 8, "{longest_silence} silent frames");
    assert_eq!(announced(&player.seen), ["first", "second", "third"]);
}

#[test]
fn tracks_shorter_than_the_queue_are_each_announced_and_keep_their_length() {
    let mut player = harness();
    let tracks: Vec<TrackEntry> = (0..6)
        .map(|index| {
            let start = index * 4800;
            player.piece(&format!("t{index}"), OUTPUT_RATE, start..start + 4800)
        })
        .collect();
    let audio = player.play_all(tracks);
    assert_eq!(audio.len(), 6 * 4800 * 2);
    assert_eq!(
        announced(&player.seen),
        ["t0", "t1", "t2", "t3", "t4", "t5"]
    );
}

#[test]
fn the_current_track_and_position_change_when_the_join_is_heard() {
    let dir = tempfile::tempdir().unwrap();
    let entry = |name: &str, seconds: f32| {
        let path = dir.path().join(format!("{name}.wav"));
        wav(&path, OUTPUT_RATE, &[(440.0, seconds)]);
        TrackEntry {
            id: name.to_owned(),
            path: path.to_str().unwrap().to_owned(),
            gain: Default::default(),
        }
    };
    let tracks = vec![entry("first", 0.5), entry("second", 1.0)];

    // Real-time output: both tracks fit in the queue, so both are decoded at once.
    let (sender, events) = mpsc::channel();
    let engine = Engine::spawn(
        || Ok(Box::new(NullOutput::new(OUTPUT_RATE, 2)) as Box<dyn AudioOutput>),
        ChainParts::default(),
        move |event| {
            let _ = sender.send((Instant::now(), event));
        },
    );
    engine.send(PlayerCommand::Load {
        tracks,
        start_index: 0,
        start_ms: 0,
    });
    let mut started = None;
    loop {
        let (when, event) = events.recv_timeout(WAIT).unwrap();
        let PlayerEvent::State {
            track_id: Some(track),
            index,
            position_ms,
            duration_ms,
            ..
        } = event
        else {
            continue;
        };
        if track == "first" {
            started.get_or_insert(when);
        } else {
            let elapsed = when.duration_since(started.unwrap()).as_millis();
            assert!((450..=650).contains(&elapsed), "after {elapsed} ms");
            assert_eq!(index, Some(1));
            assert_eq!(duration_ms, Some(1000));
            assert!(position_ms < 50, "position {position_ms} ms");
            return;
        }
    }
}

/// Loads a list and pauses, so the second track sits decoded in the queue.
fn paused_with_the_next_track_queued(player: &mut Harness) -> Vec<TrackEntry> {
    let first = player.file(
        "first",
        OUTPUT_RATE,
        &[(400.0, 0.1), (800.0, 0.1), (1600.0, 0.1)],
    );
    let second = player.file("second", OUTPUT_RATE, &[(3200.0, 0.3)]);
    let third = player.file("third", OUTPUT_RATE, &[(200.0, 0.3)]);
    let tracks = vec![first, second, third];
    player.load(tracks.clone(), 0, 0);
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
    thread::sleep(Duration::from_millis(100));
    player.take_audio();
    tracks
}

#[test]
fn a_seek_in_the_current_track_works_after_the_next_one_is_queued() {
    let mut player = harness();
    paused_with_the_next_track_queued(&mut player);
    player.engine.send(PlayerCommand::Seek { ms: 200 });
    player.wait_for(|event| {
        matches!(
            event,
            PlayerEvent::State {
                state: PlaybackState::Paused,
                position_ms: 200,
                track_id: Some(track),
                ..
            } if track == "first"
        )
    });
    player.take_audio();
    player.engine.send(PlayerCommand::Play);
    player.wait_for_stop();
    thread::sleep(Duration::from_millis(30));
    let audio = player.take_audio();

    let tenth = OUTPUT_RATE as usize / 10;
    assert_eq!(audio.len() / 2, tenth + 2 * 3 * tenth);
    let pitch = frequency(&audio[..tenth * 2]);
    assert!(near(pitch, 1600.0), "after the seek: {pitch} Hz");
    let pitch = frequency(&audio[tenth * 2..tenth * 8]);
    assert!(near(pitch, 3200.0), "the next track: {pitch} Hz");
}

#[test]
fn next_skips_to_the_right_track_after_the_next_one_is_queued() {
    let mut player = harness();
    paused_with_the_next_track_queued(&mut player);
    player.engine.send(PlayerCommand::Next);
    player.wait_for_track("second");
    player.wait_for_stop();
    thread::sleep(Duration::from_millis(30));
    let audio = player.take_audio();
    let tenth = OUTPUT_RATE as usize / 10;
    assert_eq!(audio.len() / 2, 6 * tenth);
    assert!(near(frequency(&audio[..tenth * 6]), 3200.0));
    assert!(near(frequency(&audio[tenth * 6..]), 200.0));
}

#[test]
fn a_bad_track_between_two_good_ones_is_reported_and_leaves_no_gap() {
    let mut player = harness();
    let first = player.piece("first", OUTPUT_RATE, 0..24_000);
    let gone = TrackEntry {
        id: "gone".to_owned(),
        path: player
            ._dir
            .path()
            .join("gone.flac")
            .to_str()
            .unwrap()
            .to_owned(),
        gain: Default::default(),
    };
    let last = player.piece("last", OUTPUT_RATE, 24_000..48_000);
    let joined = player.play_all(vec![first, gone, last]);

    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..48_000)]);
    assert!(
        joined == expected,
        "the joined audio differs from the whole"
    );
    assert_eq!(announced(&player.seen), ["first", "last"]);
    assert!(
        player
            .seen
            .iter()
            .any(|event| matches!(event, PlayerEvent::TrackFailed { id, .. } if id == "gone"))
    );
}

const MP3_FRAME_BYTES: usize = 417;
const MP3_FRAME_SAMPLES: u64 = 1152;

/// A silent MP3 of `frames` audio frames at 44.1 kHz. With `trim`, it starts with a LAME
/// tag stating the encoder's delay and padding fields.
fn silent_mp3(path: &Path, frames: u32, trim: Option<(u32, u32)>) {
    // MPEG 1 layer 3, 128 kbps, 44.1 kHz, stereo. All-zero side info means silence.
    let frame = |body: &[u8]| {
        let mut bytes = vec![0_u8; MP3_FRAME_BYTES];
        bytes[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        bytes[36..36 + body.len()].copy_from_slice(body);
        bytes
    };
    let mut file = Vec::new();
    if let Some((delay, padding)) = trim {
        let mut tag = b"Info".to_vec();
        tag.extend(1_u32.to_be_bytes()); // Only the frame count is present.
        tag.extend(frames.to_be_bytes());
        tag.extend_from_slice(b"LAME3.100");
        tag.extend([0; 2]); // Revision and lowpass.
        tag.extend([0; 8]); // Peak and two gains.
        tag.extend([0; 2]); // Encoding flags and bitrate.
        let packed = (delay << 12) | padding;
        tag.extend(&packed.to_be_bytes()[1..]);
        tag.extend([0; 12]); // The rest, with a zero CRC, which means "do not check".
        file.extend(frame(&tag));
    }
    for _ in 0..frames {
        file.extend(frame(&[]));
    }
    fs::write(path, file).unwrap();
}

#[test]
fn the_lame_tag_removes_encoder_delay_and_padding_from_an_mp3() {
    let dir = tempfile::tempdir().unwrap();
    let tagged = dir.path().join("tagged.mp3");
    silent_mp3(&tagged, 20, Some((576, 1000)));
    let check = check_file(tagged.to_str().unwrap(), 44_100).unwrap();
    // The decoder adds 529 frames of its own delay, which also comes off both fields.
    let expected = 20 * MP3_FRAME_SAMPLES - (576 + 529) - (1000 - 529);
    assert_eq!(check.frames, expected);

    let untagged = dir.path().join("untagged.mp3");
    silent_mp3(&untagged, 20, None);
    let check = check_file(untagged.to_str().unwrap(), 44_100).unwrap();
    assert_eq!(check.frames, 20 * MP3_FRAME_SAMPLES);
}

#[test]
fn mp3_tracks_join_at_their_trimmed_lengths() {
    let mut player = harness();
    let mut tracks = Vec::new();
    for name in ["a", "b"] {
        let path = player._dir.path().join(format!("{name}.mp3"));
        silent_mp3(&path, 40, Some((576, 1000)));
        tracks.push(TrackEntry {
            id: name.to_owned(),
            path: path.to_str().unwrap().to_owned(),
            gain: Default::default(),
        });
    }
    let audio = player.play_all(tracks);
    let trimmed = 40 * MP3_FRAME_SAMPLES - (576 + 529) - (1000 - 529);
    let expected = (2 * trimmed) as f64 * f64::from(OUTPUT_RATE) / 44_100.0;
    assert!(
        (audio.len() as f64 / 2.0 - expected).abs() <= 1.0,
        "{} frames, expected {expected}",
        audio.len() / 2
    );
    assert_eq!(announced(&player.seen), ["a", "b"]);
}

/// Measures real joins, for checking against music that runs across tracks. Set
/// `UMAK_JOIN_FILES` to file paths separated by `;`. Each neighboring pair plays from
/// three seconds before the join until three seconds after it.
#[test]
#[ignore = "needs real files; run with --ignored --nocapture"]
fn report_on_real_joins() {
    let files = std::env::var("UMAK_JOIN_FILES").expect("set UMAK_JOIN_FILES");
    let files: Vec<&str> = files.split(';').collect();
    for pair in files.windows(2) {
        let mut player = harness();
        let entry = |id: &str, path: &str| TrackEntry {
            id: id.to_owned(),
            path: path.to_owned(),
            gain: Default::default(),
        };
        let check = check_file(pair[0], OUTPUT_RATE).unwrap();
        let start_ms = check.output_frames * 1000 / u64::from(OUTPUT_RATE) - 3000;
        player.load(vec![entry("a", pair[0]), entry("b", pair[1])], 0, start_ms);
        player.wait_for_track("b");
        player.wait_for_frames(OUTPUT_RATE as usize * 6);
        player.engine.send(PlayerCommand::Stop);
        let audio = player.take_audio();

        let left: Vec<f32> = audio.chunks_exact(2).map(|frame| frame[0]).collect();
        let join = OUTPUT_RATE as usize * 3;
        let window = OUTPUT_RATE as usize / 2;
        let (from, to) = (join.saturating_sub(window), (join + window).min(left.len()));
        let (mut silent, mut longest) = (0, 0);
        for sample in &left[from..to] {
            silent = if sample.abs() < 1e-5 { silent + 1 } else { 0 };
            longest = longest.max(silent);
        }
        let mut steps: Vec<f32> = left[from..to]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .collect();
        let step_at_join = steps
            .iter()
            .skip(window.saturating_sub(40))
            .take(80)
            .fold(0.0, |worst: f32, step| worst.max(*step));
        steps.sort_by(f32::total_cmp);
        println!(
            "{} -> {}\n  silent frames near the join: {longest}; largest step at the join {step_at_join:.4}, near it p99 {:.4}, max {:.4}; a's output {} frames",
            pair[0],
            pair[1],
            steps[steps.len() * 99 / 100],
            steps[steps.len() - 1],
            check.output_frames,
        );
    }
}

impl Harness {
    fn edit(&self, tracks: Vec<TrackEntry>, origin: Vec<Option<usize>>) {
        self.engine.send(PlayerCommand::EditList { tracks, origin });
    }

    /// Waits for the engine to report the current track at this list index.
    fn wait_for_index(&mut self, wanted: Option<usize>) {
        self.wait_for(
            |event| matches!(event, PlayerEvent::State { index, .. } if *index == wanted),
        );
    }

    fn take_all_after_stop(&mut self) -> Vec<f32> {
        self.wait_for_stop();
        thread::sleep(Duration::from_millis(30));
        self.take_audio()
    }
}

#[test]
fn adding_to_the_list_while_playing_does_not_disturb_the_audio() {
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..144_000)]);

    let mut player = harness();
    let first = player.piece("a", OUTPUT_RATE, 0..48_000);
    let second = player.piece("b", OUTPUT_RATE, 48_000..96_000);
    let third = player.piece("c", OUTPUT_RATE, 96_000..144_000);
    player.load(vec![first.clone(), second.clone()], 0, 0);
    player.edit(vec![first, second, third], vec![Some(0), Some(1), None]);
    let audio = player.take_all_after_stop();
    assert!(audio == expected, "the audio differs from the whole");
    assert_eq!(announced(&player.seen), ["a", "b", "c"]);
}

#[test]
fn adding_after_the_last_track_finished_decoding_still_joins_without_a_gap() {
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..48_000)]);

    let mut player = harness();
    let first = player.piece("a", OUTPUT_RATE, 0..24_000);
    let second = player.piece("b", OUTPUT_RATE, 24_000..48_000);
    player.load(vec![first.clone()], 0, 0);
    // While paused, the whole track decodes into the queue and decoding reaches the end.
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
    thread::sleep(Duration::from_millis(100));
    player.edit(vec![first, second], vec![Some(0), None]);
    player.engine.send(PlayerCommand::Play);
    let audio = player.take_all_after_stop();
    assert!(audio == expected, "the audio differs from the whole");
    assert_eq!(announced(&player.seen), ["a", "b"]);
}

#[test]
fn inserting_before_a_queued_track_plays_the_new_one_next_and_resumes_in_place() {
    let mut player = harness();
    let tracks = paused_with_the_next_track_queued(&mut player);
    let new = player.file("new", OUTPUT_RATE, &[(600.0, 0.3)]);
    let edited = vec![tracks[0].clone(), new, tracks[1].clone(), tracks[2].clone()];
    player.edit(edited, vec![Some(0), None, Some(1), Some(2)]);
    player.wait_for(|event| {
        matches!(
            event,
            PlayerEvent::State {
                state: PlaybackState::Paused,
                track_id: Some(track),
                ..
            } if track == "first"
        )
    });
    player.take_audio();
    player.engine.send(PlayerCommand::Play);
    let audio = player.take_all_after_stop();
    assert_eq!(
        announced(&player.seen).last().map(String::as_str),
        Some("third")
    );
    assert!(announced(&player.seen).contains(&"new".to_owned()));
    // The last 0.9 seconds are the three later tracks, in their new order.
    let tenth = OUTPUT_RATE as usize / 10 * 2;
    let end = audio.len();
    assert!(near(
        frequency(&audio[end - 9 * tenth..end - 6 * tenth]),
        600.0
    ));
    assert!(near(
        frequency(&audio[end - 6 * tenth..end - 3 * tenth]),
        3200.0
    ));
    assert!(near(frequency(&audio[end - 3 * tenth..]), 200.0));
}

#[test]
fn moving_entries_keeps_the_current_track_and_changes_what_follows() {
    let mut player = harness();
    let tracks = paused_with_the_next_track_queued(&mut player);
    // The order becomes third, first, second, and "first" is still playing.
    let edited = vec![tracks[2].clone(), tracks[0].clone(), tracks[1].clone()];
    player.edit(edited, vec![Some(2), Some(0), Some(1)]);
    player.wait_for_index(Some(1));
    player.engine.send(PlayerCommand::Play);
    player.wait_for_stop();
    let names = announced(&player.seen);
    assert_eq!(names, ["first", "second"], "third moved before the current");
}

#[test]
fn removing_the_current_track_moves_to_the_one_that_followed_it() {
    let mut player = harness();
    let tracks = paused_with_the_next_track_queued(&mut player);
    let edited = vec![tracks[1].clone(), tracks[2].clone()];
    player.edit(edited, vec![Some(1), Some(2)]);
    // It stays paused, as it was.
    player.wait_for(|event| {
        matches!(
            event,
            PlayerEvent::State {
                state: PlaybackState::Paused,
                index: Some(0),
                track_id: Some(track),
                ..
            } if track == "second"
        )
    });
    player.engine.send(PlayerCommand::Play);
    player.wait_for_track("third");
    player.wait_for_stop();
}

#[test]
fn removing_a_track_that_was_queued_skips_it() {
    let mut player = harness();
    let tracks = paused_with_the_next_track_queued(&mut player);
    let edited = vec![tracks[0].clone(), tracks[2].clone()];
    player.edit(edited, vec![Some(0), Some(2)]);
    player.engine.send(PlayerCommand::Play);
    player.wait_for_track("third");
    player.wait_for_stop();
    assert!(!announced(&player.seen).contains(&"second".to_owned()));
}

#[test]
fn clearing_the_list_stops_playback() {
    let mut player = harness();
    paused_with_the_next_track_queued(&mut player);
    player.edit(Vec::new(), Vec::new());
    player.wait_for_stop();
}

fn with_track_gain(mut track: TrackEntry, db: f32) -> TrackEntry {
    track.gain.track_db = Some(db);
    track
}

fn set_track_mode(player: &Harness, preamp_db: f32) {
    player.engine.send(PlayerCommand::SetGain {
        replay_gain: ReplayGainMode::Track,
        preamp_db,
    });
}

fn peak_of(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn replay_gain_and_the_preamp_change_the_level_by_the_stated_amount() {
    let mut player = harness();
    let plain = player.piece("plain", OUTPUT_RATE, 0..24_000);
    let level = peak_of(&player.play_all(vec![plain.clone()]));

    set_track_mode(&player, 0.0);
    let cut = with_track_gain(plain.clone(), -6.0);
    let measured = peak_of(&player.play_all(vec![cut.clone()])) / level;
    assert!((measured - 0.501).abs() < 0.002, "-6 dB gave {measured}");

    set_track_mode(&player, 3.0);
    let measured = peak_of(&player.play_all(vec![cut])) / level;
    assert!(
        (measured - 0.708).abs() < 0.002,
        "-6 + 3 dB gave {measured}"
    );

    // A track with no tag gets the preamp alone.
    let measured = peak_of(&player.play_all(vec![plain])) / level;
    assert!((measured - 1.413).abs() < 0.003, "+3 dB gave {measured}");
}

#[test]
fn off_leaves_tagged_tracks_exactly_as_they_are() {
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..24_000)]);
    let mut player = harness();
    let tagged = with_track_gain(player.piece("tagged", OUTPUT_RATE, 0..24_000), -9.0);
    assert!(player.play_all(vec![tagged]) == expected);
}

#[test]
fn each_track_of_a_gapless_join_gets_its_own_gain_at_the_exact_sample() {
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..48_000)]);

    let mut player = harness();
    set_track_mode(&player, 0.0);
    let first = with_track_gain(player.piece("a", OUTPUT_RATE, 0..24_000), -6.0206);
    let second = with_track_gain(player.piece("b", OUTPUT_RATE, 24_000..48_000), 0.0);
    let audio = player.play_all(vec![first, second]);

    assert_eq!(audio.len(), expected.len());
    for (index, (actual, reference)) in audio.iter().zip(&expected).enumerate() {
        let factor = if index / 2 < 24_000 { 0.5 } else { 1.0 };
        assert!(
            (actual - reference * factor).abs() < 1e-3,
            "frame {}: {actual}, expected {}",
            index / 2,
            reference * factor
        );
    }
}

fn set_eq(player: &Harness, band: usize, db: f32) {
    let mut bands_db = [0.0; 10];
    bands_db[band] = db;
    player.engine.send(PlayerCommand::SetEq {
        enabled: true,
        bands_db,
    });
}

/// The level of a part of the audio, relative to a reference, in decibels.
fn level_db(samples: &[f32], reference: f32) -> f32 {
    20.0 * (peak_of(samples) / reference).log10()
}

#[test]
fn the_equalizer_changes_tones_by_the_stated_amount_through_the_whole_engine() {
    let mut plain = harness();
    let tones = [(1000.0, 0.5), (125.0, 0.5)];
    let track = plain.file("tones", OUTPUT_RATE, &tones);
    let reference = plain.play_all(vec![track.clone()]);
    let half = reference.len() / 2;
    let settled = half / 4;
    let reference_level = peak_of(&reference[settled..half]);

    let mut player = harness();
    // A cut, so the rest of the signal keeps its level.
    set_eq(&player, 5, -6.0);
    let track = player.file("tones", OUTPUT_RATE, &tones);
    let audio = player.play_all(vec![track]);
    let at_band = level_db(&audio[settled..half], reference_level);
    assert!((at_band + 6.0).abs() < 0.2, "at 1 kHz: {at_band} dB");
    let elsewhere = level_db(&audio[half + settled..], reference_level);
    assert!(elsewhere.abs() < 0.2, "at 125 Hz: {elsewhere} dB");
}

#[test]
fn turning_the_equalizer_off_leaves_samples_exactly_as_they_were() {
    let mut whole = harness();
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..24_000)]);
    let mut player = harness();
    player.engine.send(PlayerCommand::SetEq {
        enabled: false,
        bands_db: [9.0; 10],
    });
    let track = player.piece("track", OUTPUT_RATE, 0..24_000);
    assert!(player.play_all(vec![track]) == expected);
}

#[test]
fn the_limiter_holds_a_heavy_boost_under_full_scale_through_the_whole_engine() {
    // The tone's peak is 0.5, so 12 dB of preamp would reach about 2.
    let mut player = harness();
    set_track_mode(&player, 12.0);
    let track = player.piece("loud", OUTPUT_RATE, 0..24_000);
    let audio = player.play_all(vec![track]);
    let peak = peak_of(&audio);
    assert!(peak <= 1.0, "peak {peak}");
    assert!(peak > 0.9, "peak {peak}");
}

#[test]
fn a_gapless_join_stays_continuous_through_the_equalizer() {
    let mut whole = harness();
    set_eq(&whole, 5, -6.0);
    let expected = whole.play_all(vec![whole.piece("whole", OUTPUT_RATE, 0..48_000)]);

    let mut player = harness();
    set_eq(&player, 5, -6.0);
    let tracks = vec![
        player.piece("a", OUTPUT_RATE, 0..24_000),
        player.piece("b", OUTPUT_RATE, 24_000..48_000),
    ];
    let audio = player.play_all(tracks);
    assert_eq!(audio.len(), expected.len());
    let worst = audio
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-5, "worst difference {worst}");
}

/// How much of the audio thread's time the full chain takes. Run with --release.
#[test]
#[ignore = "a measurement; run with --release --ignored --nocapture"]
fn report_the_cost_of_the_signal_chain() {
    let (mut feeds, mut renderer, shared) = umak_audio::render::channel(
        OUTPUT_RATE as usize * 2 * 2,
        OUTPUT_RATE,
        2,
        EffectChain::default(),
        None,
    );
    shared.set_gain(ReplayGainMode::Album, 3.0);
    shared.set_eq(
        true,
        &[3.0, -3.0, 3.0, -3.0, 3.0, -3.0, 3.0, -3.0, 3.0, -3.0],
    );
    // The device's usual request: 10 ms.
    let block = OUTPUT_RATE as usize / 100;
    let source: Vec<f32> = (0..block * 2)
        .map(|index| (index as f32 * 0.03).sin() * 0.5)
        .collect();
    let mut output = vec![0.0; block * 2];
    let mut busy = Duration::ZERO;
    let rounds = 6000;
    for _ in 0..rounds {
        let _ = feeds.samples.push_partial_slice(&source);
        let started = Instant::now();
        renderer.render(&mut output);
        busy += started.elapsed();
    }
    let audio = Duration::from_millis(10) * rounds;
    println!(
        "{busy:?} of work for {audio:?} of audio: {:.3}% of one core",
        busy.as_secs_f64() / audio.as_secs_f64() * 100.0
    );
}

/// How the limiter treats real music. Set `UMAK_LIMIT_FILES` to paths separated by `;`.
#[test]
#[ignore = "needs real files; run with --release --ignored --nocapture"]
fn report_on_limiting_real_audio() {
    use umak_audio::decoder::Decoder;
    use umak_audio::effects::Effect;
    use umak_audio::limiter::Limiter;
    use umak_audio::source::LocalFile;

    let files = std::env::var("UMAK_LIMIT_FILES").expect("set UMAK_LIMIT_FILES");
    for path in files.split(';') {
        let source = LocalFile(path.into());
        let mut decoder = Decoder::open(&source).unwrap();
        let mut samples = Vec::new();
        while decoder.next_chunk(&mut samples).unwrap() {}
        let channels = decoder.channels();
        let source_peak = peak_of(&samples);
        println!("{path}\n  source peak {source_peak:.3}");
        for boost_db in [3.0_f32, 6.0, 12.0] {
            let boost = 10.0_f32.powf(boost_db / 20.0);
            let boosted: Vec<f32> = samples.iter().map(|sample| sample * boost).collect();
            let mut limited = boosted.clone();
            Limiter::new(decoder.sample_rate()).process(&mut limited, channels);
            let (mut touched, mut total_db, mut worst_db) = (0_u64, 0.0_f64, 0.0_f64);
            for (out, input) in limited
                .chunks_exact(channels)
                .zip(boosted.chunks_exact(channels))
            {
                if out[0] != input[0] && input[0] != 0.0 {
                    let db = 20.0 * f64::from((out[0] / input[0]).abs()).log10();
                    touched += 1;
                    total_db += db;
                    worst_db = worst_db.min(db);
                }
            }
            let frames = limited.len() / channels;
            println!(
                "  +{boost_db} dB: reducing {:.1}% of the time, by {:.2} dB on average then, {worst_db:.1} dB at most; output peak {:.4}",
                touched as f64 / frames as f64 * 100.0,
                if touched > 0 {
                    total_db / touched as f64
                } else {
                    0.0
                },
                peak_of(&limited),
            );
        }
    }
}
