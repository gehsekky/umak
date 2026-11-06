//! The real-time half of the engine. `Renderer::render` runs on the audio thread, so it
//! must not block, allocate or take locks: it talks to the engine thread only through
//! a lock-free ring buffer and atomics. `tests/realtime.rs` checks it never allocates.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::effects::{Effect, EffectChain, OutputTap};
use crate::eq::{EqControl, Equalizer};
use crate::limiter::Limiter;
use crate::protocol::{EQ_BANDS, ReplayGainMode, TrackGain};

/// How long a change of volume or gain takes, so it does not click. 10 ms at 48 kHz.
const RAMP_FRAMES: f32 = 480.0;
/// Pending track changes. Each takes at least a packet of audio, so this far exceeds
/// what a queue of seconds can hold.
const GAIN_QUEUE: usize = 256;

fn from_db(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[derive(Debug, Clone, Copy)]
struct Level {
    gain: f32,
    /// The most gain this track takes without its peak passing full scale.
    ceiling: f32,
}

impl Level {
    fn new(db: Option<f32>, peak: Option<f32>) -> Option<Self> {
        Some(Self {
            gain: from_db(db?),
            // A peak at or below zero is a bad tag, which says nothing about the audio.
            ceiling: peak
                .filter(|peak| *peak > 0.0)
                .map_or(f32::INFINITY, |peak| 1.0 / peak),
        })
    }
}

/// A track's levels, and the played frame where its audio starts.
#[derive(Debug, Clone, Copy)]
pub struct GainChange {
    at: u64,
    track: Option<Level>,
    album: Option<Level>,
}

impl GainChange {
    pub fn new(at: u64, gain: &TrackGain) -> Self {
        Self {
            at,
            track: Level::new(gain.track_db, gain.track_peak),
            album: Level::new(gain.album_db, gain.album_peak),
        }
    }
}

const MODE_OFF: u8 = 0;
const MODE_TRACK: u8 = 1;
const MODE_ALBUM: u8 = 2;

/// The gain for a track under the current settings.
fn gain_for(change: Option<&GainChange>, mode: u8, preamp: f32) -> f32 {
    if mode == MODE_OFF {
        return 1.0;
    }
    let level = change.and_then(|change| {
        if mode == MODE_TRACK {
            change.track.or(change.album)
        } else {
            change.album.or(change.track)
        }
    });
    match level {
        Some(level) => (preamp * level.gain).min(level.ceiling),
        None => preamp,
    }
}

/// A gain that moves to a new value over a short ramp.
struct Smoothed {
    value: f32,
    target: f32,
    step: f32,
}

impl Smoothed {
    fn new(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
        }
    }

    fn jump_to(&mut self, value: f32) {
        self.value = value;
        self.target = value;
    }

    fn aim_at(&mut self, target: f32) {
        if target != self.target {
            self.target = target;
            self.step = (target - self.value) / RAMP_FRAMES;
        }
    }

    fn apply(&mut self, samples: &mut [f32], channels: usize) {
        if self.value == self.target {
            if self.value != 1.0 {
                for sample in samples {
                    *sample *= self.value;
                }
            }
            return;
        }
        for frame in samples.chunks_exact_mut(channels) {
            self.value += self.step;
            let passed = (self.step > 0.0) == (self.value >= self.target);
            if passed {
                self.value = self.target;
            }
            for sample in frame {
                *sample *= self.value;
            }
        }
    }
}

/// State shared between the engine thread and the audio thread.
pub struct Shared {
    paused: AtomicBool,
    flush_request: AtomicU64,
    flush_ack: AtomicU64,
    played_frames: AtomicU64,
    underruns: AtomicU64,
    volume_bits: AtomicU32,
    output_failed: AtomicBool,
    longest_gap_us: AtomicU64,
    replay_gain: AtomicU8,
    preamp_bits: AtomicU32,
    eq: Arc<EqControl>,
}

impl Shared {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            flush_request: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            played_frames: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            volume_bits: AtomicU32::new(1.0_f32.to_bits()),
            output_failed: AtomicBool::new(false),
            longest_gap_us: AtomicU64::new(0),
            replay_gain: AtomicU8::new(MODE_OFF),
            preamp_bits: AtomicU32::new(1.0_f32.to_bits()),
            eq: Arc::default(),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume_bits
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_gain(&self, mode: ReplayGainMode, preamp_db: f32) {
        let mode = match mode {
            ReplayGainMode::Off => MODE_OFF,
            ReplayGainMode::Track => MODE_TRACK,
            ReplayGainMode::Album => MODE_ALBUM,
        };
        self.replay_gain.store(mode, Ordering::Relaxed);
        self.preamp_bits
            .store(from_db(preamp_db).to_bits(), Ordering::Relaxed);
    }

    pub fn set_eq(&self, enabled: bool, bands_db: &[f32; EQ_BANDS]) {
        self.eq.set(enabled, bands_db);
    }

    pub fn played_frames(&self) -> u64 {
        self.played_frames.load(Ordering::Acquire)
    }

    /// Times the output asked for samples while playing and had to fill in silence.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Asks the audio thread to discard everything queued. Returns the request number.
    pub fn request_flush(&self) -> u64 {
        self.flush_request.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn flush_done(&self, request: u64) -> bool {
        self.flush_ack.load(Ordering::Acquire) >= request
    }

    /// The longest wait between output callbacks since this was last asked. A long one
    /// means the device or the OS stalled, whatever the queue held.
    pub fn take_longest_gap_us(&self) -> u64 {
        self.longest_gap_us.swap(0, Ordering::Relaxed)
    }

    pub fn mark_output_failed(&self) {
        self.output_failed.store(true, Ordering::Release);
    }

    pub fn take_output_failed(&self) -> bool {
        self.output_failed.swap(false, Ordering::AcqRel)
    }
}

pub struct Renderer {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    equalizer: Equalizer,
    chain: EffectChain,
    limiter: Limiter,
    tap: Option<Box<dyn OutputTap>>,
    /// Whether samples have flowed since the last flush, so silence means an underrun.
    active: bool,
    last_call: Option<Instant>,
    gains: Consumer<GainChange>,
    /// The levels of the track now playing.
    level: Option<GainChange>,
    gain: Smoothed,
    volume: Smoothed,
}

/// The engine thread's end of the queues to the audio thread.
pub struct Feeds {
    pub samples: Producer<f32>,
    pub gains: Producer<GainChange>,
}

/// Creates the queues to the audio thread. The sample queue's capacity is in samples.
pub fn channel(
    capacity: usize,
    sample_rate: u32,
    channels: usize,
    chain: EffectChain,
    tap: Option<Box<dyn OutputTap>>,
) -> (Feeds, Renderer, Arc<Shared>) {
    let (samples, consumer) = RingBuffer::new(capacity);
    let (gains, gain_consumer) = RingBuffer::new(GAIN_QUEUE);
    let shared = Arc::new(Shared::new());
    let renderer = Renderer {
        consumer,
        channels,
        equalizer: Equalizer::new(sample_rate, channels, Arc::clone(&shared.eq)),
        limiter: Limiter::new(sample_rate),
        shared: Arc::clone(&shared),
        chain,
        tap,
        active: false,
        last_call: None,
        gains: gain_consumer,
        level: None,
        gain: Smoothed::new(1.0),
        volume: Smoothed::new(1.0),
    };
    (Feeds { samples, gains }, renderer, shared)
}

impl Renderer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Levels the audio, changing gain at the exact frame where a new track starts.
    fn apply_gain(&mut self, audio: &mut [f32], mut frame: u64, resuming: bool) {
        let mode = self.shared.replay_gain.load(Ordering::Relaxed);
        let preamp = f32::from_bits(self.shared.preamp_bits.load(Ordering::Relaxed));
        let mut jump = resuming;
        let mut rest = audio;
        while !rest.is_empty() {
            while let Ok(change) = self.gains.peek() {
                if change.at > frame {
                    break;
                }
                self.level = self.gains.pop().ok();
                jump = true;
            }
            let target = gain_for(self.level.as_ref(), mode, preamp);
            if jump {
                self.gain.jump_to(target);
                jump = false;
            } else {
                self.gain.aim_at(target);
            }
            let left = rest.len() / self.channels;
            let frames = match self.gains.peek() {
                Ok(change) => left.min((change.at - frame) as usize),
                Err(_) => left,
            };
            let (now, later) = rest.split_at_mut(frames * self.channels);
            self.gain.apply(now, self.channels);
            frame += frames as u64;
            rest = later;
        }
    }

    /// Fills an output buffer of interleaved samples. Called by the audio thread.
    pub fn render(&mut self, output: &mut [f32]) {
        let now = Instant::now();
        if let Some(last) = self.last_call {
            let gap = now.duration_since(last).as_micros() as u64;
            self.shared.longest_gap_us.fetch_max(gap, Ordering::Relaxed);
        }
        self.last_call = Some(now);

        let request = self.shared.flush_request.load(Ordering::Acquire);
        if request != self.shared.flush_ack.load(Ordering::Relaxed) {
            let queued = self.consumer.slots();
            if let Ok(chunk) = self.consumer.read_chunk(queued) {
                chunk.commit_all();
            }
            while self.gains.pop().is_ok() {}
            self.level = None;
            self.active = false;
            self.shared.flush_ack.store(request, Ordering::Release);
        }

        // Audio starting after silence takes its gains at once, since no ramp can click.
        let resuming = !self.active;
        let first_frame = self.shared.played_frames();
        let mut written = 0;
        if !self.shared.paused.load(Ordering::Acquire) {
            // Whole frames only, so channels can never swap.
            let wanted = output.len().min(self.consumer.slots());
            let wanted = wanted - wanted % self.channels;
            let (filled, _) = self.consumer.pop_partial_slice(&mut output[..wanted]);
            written = filled.len();
            if written > 0 {
                self.shared
                    .played_frames
                    .fetch_add((written / self.channels) as u64, Ordering::AcqRel);
                self.active = true;
            }
            if written < output.len() && self.active {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                self.active = false;
            }
        }

        // The fixed signal chain. Each stage runs once, in this order.
        let (audio, silence) = output.split_at_mut(written);
        if resuming {
            self.equalizer.restart();
            self.limiter.restart();
        }
        self.apply_gain(audio, first_frame, resuming);
        self.equalizer.process(audio, self.channels);
        self.chain.process(audio, self.channels);
        // Nothing has changed the level when these are all off, and then the samples are
        // left exactly as decoded, even any that decoding left over full scale.
        if self.shared.replay_gain.load(Ordering::Relaxed) != MODE_OFF
            || self.equalizer.is_active()
            || !self.chain.is_empty()
        {
            self.limiter.process(audio, self.channels);
        }
        let volume = f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed));
        if resuming {
            self.volume.jump_to(volume);
        } else {
            self.volume.aim_at(volume);
        }
        self.volume.apply(audio, self.channels);
        silence.fill(0.0);
        if let Some(tap) = &mut self.tap {
            tap.on_output(output, self.channels);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: f32 = 0.5;

    fn tagged(db: f32) -> TrackGain {
        TrackGain {
            track_db: Some(db),
            ..TrackGain::default()
        }
    }

    fn rig() -> (Feeds, Renderer, Arc<Shared>) {
        let mut parts = channel(96_000, 48_000, 2, EffectChain::default(), None);
        let block = vec![SAMPLE; 40_000];
        let _ = parts.0.samples.push_partial_slice(&block);
        parts
    }

    fn render(renderer: &mut Renderer, frames: usize) -> Vec<f32> {
        let mut output = vec![0.0; frames * 2];
        renderer.render(&mut output);
        output
    }

    #[test]
    fn gain_values_follow_the_mode_and_fall_back_to_the_other_tag() {
        let both = GainChange::new(
            0,
            &TrackGain {
                track_db: Some(-20.0),
                album_db: Some(-40.0),
                ..TrackGain::default()
            },
        );
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert_eq!(gain_for(Some(&both), MODE_OFF, 2.0), 1.0);
        assert!(close(gain_for(Some(&both), MODE_TRACK, 1.0), 0.1));
        assert!(close(gain_for(Some(&both), MODE_ALBUM, 1.0), 0.01));
        assert!(close(gain_for(Some(&both), MODE_ALBUM, 2.0), 0.02));

        let track_only = GainChange::new(0, &tagged(-20.0));
        assert!(close(gain_for(Some(&track_only), MODE_ALBUM, 1.0), 0.1));
        let untagged = GainChange::new(0, &TrackGain::default());
        assert_eq!(gain_for(Some(&untagged), MODE_TRACK, 1.0), 1.0);
        assert_eq!(gain_for(Some(&untagged), MODE_TRACK, 2.0), 2.0);
    }

    #[test]
    fn the_peak_tag_limits_gain_and_a_bad_peak_tag_is_ignored() {
        let loud = |peak| {
            GainChange::new(
                0,
                &TrackGain {
                    track_db: Some(12.0),
                    track_peak: Some(peak),
                    ..TrackGain::default()
                },
            )
        };
        assert_eq!(gain_for(Some(&loud(0.5)), MODE_TRACK, 1.0), 2.0);
        // 12 dB is about 3.98, and -1.0 is a real bad value from a real library.
        let ignored = gain_for(Some(&loud(-1.0)), MODE_TRACK, 1.0);
        assert!((ignored - 3.98).abs() < 0.01, "{ignored}");
        // A peak above full scale limits a cut as well.
        let quiet = GainChange::new(
            0,
            &TrackGain {
                track_db: Some(-6.0),
                track_peak: Some(1.5),
                ..TrackGain::default()
            },
        );
        let value = gain_for(Some(&quiet), MODE_TRACK, 1.0);
        assert!((value - 0.501).abs() < 0.001, "{value}");
    }

    #[test]
    fn the_gain_changes_at_the_exact_frame_a_track_starts() {
        let (mut feeds, mut renderer, shared) = rig();
        shared.set_gain(ReplayGainMode::Track, 0.0);
        feeds
            .gains
            .push(GainChange::new(0, &tagged(-20.0)))
            .unwrap();
        feeds
            .gains
            .push(GainChange::new(100, &tagged(0.0)))
            .unwrap();
        let output = render(&mut renderer, 200);
        for (frame, sample) in output.chunks_exact(2).map(|frame| frame[0]).enumerate() {
            let expected = if frame < 100 { SAMPLE * 0.1 } else { SAMPLE };
            assert!((sample - expected).abs() < 1e-4, "frame {frame}: {sample}");
        }
    }

    #[test]
    fn changing_a_setting_while_playing_ramps_instead_of_clicking() {
        let (mut feeds, mut renderer, shared) = rig();
        feeds
            .gains
            .push(GainChange::new(0, &tagged(-20.0)))
            .unwrap();
        render(&mut renderer, 100);

        let mut samples = Vec::new();
        shared.set_gain(ReplayGainMode::Track, 0.0);
        samples.extend(render(&mut renderer, 300));
        samples.extend(render(&mut renderer, 300));
        let left: Vec<f32> = samples.chunks_exact(2).map(|frame| frame[0]).collect();
        assert!((left[0] - SAMPLE).abs() < 0.01, "starts at the old level");
        assert!(
            (left[left.len() - 1] - SAMPLE * 0.1).abs() < 1e-4,
            "ends at the new one"
        );
        let largest_step = left
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        assert!(largest_step < 0.002, "largest step {largest_step}");
    }

    #[test]
    fn the_volume_ramps_while_playing_but_not_when_audio_starts() {
        let (_feeds, mut renderer, shared) = rig();
        shared.set_volume(0.25);
        let first = render(&mut renderer, 1);
        assert_eq!(first[0], SAMPLE * 0.25, "no ramp from the old value");

        shared.set_volume(1.0);
        let next = render(&mut renderer, 1);
        assert!(next[0] < SAMPLE * 0.3, "it moves gradually: {}", next[0]);
        let last = render(&mut renderer, 1000);
        assert_eq!(last[last.len() - 1], SAMPLE);
    }

    #[test]
    fn off_at_full_volume_leaves_samples_untouched() {
        let (mut feeds, mut renderer, shared) = rig();
        feeds
            .gains
            .push(GainChange::new(0, &tagged(-20.0)))
            .unwrap();
        shared.set_gain(ReplayGainMode::Off, 12.0);
        assert!(
            render(&mut renderer, 500)
                .iter()
                .all(|sample| *sample == SAMPLE)
        );
    }

    #[test]
    fn a_flush_forgets_changes_for_audio_that_was_discarded() {
        let (mut feeds, mut renderer, shared) = rig();
        shared.set_gain(ReplayGainMode::Track, 0.0);
        feeds
            .gains
            .push(GainChange::new(0, &tagged(-20.0)))
            .unwrap();
        feeds
            .gains
            .push(GainChange::new(50, &tagged(-40.0)))
            .unwrap();
        render(&mut renderer, 10);
        shared.request_flush();
        // The audio thread sees the flush with an empty queue behind it.
        render(&mut renderer, 10);
        assert!(feeds.gains.slots() == GAIN_QUEUE, "the queue was emptied");
        let block = vec![SAMPLE; 2000];
        let _ = feeds.samples.push_partial_slice(&block);
        let played = shared.played_frames();
        feeds
            .gains
            .push(GainChange::new(played, &tagged(0.0)))
            .unwrap();
        assert!(
            render(&mut renderer, 100)
                .iter()
                .all(|sample| *sample == SAMPLE)
        );
    }
}
