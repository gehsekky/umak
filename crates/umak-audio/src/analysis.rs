//! Feeds visualizers. The tap copies what goes to the output into a ring buffer on the audio
//! thread, and an analysis thread reads it at a steady rate, runs an FFT and sends frames.
//!
//! Both sides do nothing while visualizers are closed: the tap returns at once and the
//! thread sleeps. While they are open, a full ring drops samples rather than wait, so a
//! stalled analysis thread can never hold up the audio.
//!
//! The tap sees samples as they are handed to the device, a device buffer before they are
//! heard, which is too little to see.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use realfft::{RealFftPlanner, RealToComplex};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::effects::OutputTap;
use crate::protocol::{
    VISUAL_BANDS, VISUAL_FLOOR_DB, VISUAL_FRAME_RATE, VISUAL_HIGH_HZ, VISUAL_LOW_HZ,
    VISUAL_WAVE_LEN, VisualFrame,
};

/// About 85 ms at 48 kHz, which resolves the lowest bands to within a few hertz.
const FFT_LEN: usize = 4096;
/// Over a third of a second at the highest rates, many frames' worth.
const RING_LEN: usize = 1 << 16;
/// With no samples for this long, the output has stopped, so it reads as silence.
const QUIET_AFTER: Duration = Duration::from_millis(100);

struct Shared {
    enabled: AtomicBool,
    closing: AtomicBool,
    sample_rate: AtomicU32,
}

/// The audio thread's end. It mixes the channels to one and copies them to the ring.
pub struct VisualTap {
    producer: Producer<f32>,
    shared: Arc<Shared>,
}

impl OutputTap for VisualTap {
    fn prepare(&mut self, sample_rate: u32, _channels: usize) {
        self.shared
            .sample_rate
            .store(sample_rate, Ordering::Release);
    }

    fn on_output(&mut self, samples: &[f32], channels: usize) {
        if channels == 0 || !self.shared.enabled.load(Ordering::Relaxed) {
            return;
        }
        let frames = (samples.len() / channels).min(self.producer.slots());
        if frames == 0 {
            return;
        }
        let scale = 1.0 / channels as f32;
        if let Ok(chunk) = self.producer.write_chunk_uninit(frames) {
            chunk.fill_from_iter(
                samples
                    .chunks_exact(channels)
                    .map(|frame| frame.iter().sum::<f32>() * scale),
            );
        }
    }
}

/// The analysis thread. Dropping it ends the thread.
pub struct Visualizer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// Creates the tap for the engine's signal chain and the thread that reads it. Frames go to
/// `send` on that thread.
pub fn visualizer(send: impl FnMut(VisualFrame) + Send + 'static) -> (VisualTap, Visualizer) {
    let (producer, consumer) = RingBuffer::new(RING_LEN);
    let shared = Arc::new(Shared {
        enabled: AtomicBool::new(false),
        closing: AtomicBool::new(false),
        sample_rate: AtomicU32::new(0),
    });
    let thread_shared = Arc::clone(&shared);
    let thread = thread::Builder::new()
        .name("umak-visualizer".to_owned())
        .spawn(move || run(&thread_shared, consumer, send))
        .expect("the visualizer thread starts");
    let tap = VisualTap {
        producer,
        shared: Arc::clone(&shared),
    };
    let visualizer = Visualizer {
        shared,
        thread: Some(thread),
    };
    (tap, visualizer)
}

impl Visualizer {
    /// Starts or stops the frames. Stopped, neither side does any work.
    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, mut consumer: Consumer<f32>, mut send: impl FnMut(VisualFrame)) {
    let period = Duration::from_secs(1) / VISUAL_FRAME_RATE;
    let mut analysis = Analysis::new();
    while !shared.closing.load(Ordering::Acquire) {
        if !shared.enabled.load(Ordering::Acquire) {
            thread::park();
            continue;
        }
        // Whatever the ring holds is from before, when nobody was looking.
        drain(&mut consumer, |_| {});
        analysis.clear();
        let mut last_audio = Instant::now();
        let mut sent_silence = false;
        let mut next = Instant::now();
        while shared.enabled.load(Ordering::Acquire) && !shared.closing.load(Ordering::Acquire) {
            let now = Instant::now();
            if now < next {
                // Not park_timeout, which on Windows waits in whole timer ticks of 15.6 ms.
                // Sleep uses a high-resolution timer there.
                thread::sleep(next - now);
                continue;
            }
            // Keep to the schedule, but after a stall start it again rather than catch up.
            next += period;
            if next < now {
                next = now + period;
            }

            if drain(&mut consumer, |samples| analysis.push(samples)) > 0 {
                last_audio = now;
            } else if now - last_audio >= QUIET_AFTER {
                analysis.clear();
            }
            let silent = analysis.is_silent();
            if silent && sent_silence {
                continue;
            }
            let rate = shared.sample_rate.load(Ordering::Acquire);
            send(analysis.frame(rate));
            sent_silence = silent;
        }
    }
}

/// Hands over everything in the ring, and says how many samples that was.
fn drain(consumer: &mut Consumer<f32>, mut take: impl FnMut(&[f32])) -> usize {
    let count = consumer.slots();
    let Ok(chunk) = consumer.read_chunk(count) else {
        return 0;
    };
    let (first, second) = chunk.as_slices();
    take(first);
    take(second);
    chunk.commit_all();
    count
}

/// The latest samples and what is needed to analyze them, allocated once.
struct Analysis {
    history: Vec<f32>,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<realfft::num_complex::Complex<f32>>,
    scratch: Vec<realfft::num_complex::Complex<f32>>,
    magnitudes: Vec<f32>,
    /// The bins each band reads, for the rate they were worked out for.
    bands: Vec<Band>,
    bands_rate: u32,
}

/// A band reads the loudest bin in `first..end`, or, narrower than a bin, the level at
/// `center` between the two bins around it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Band {
    Bins { first: usize, end: usize },
    Between { center: f32 },
    AboveNyquist,
}

impl Analysis {
    fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let window = (0..FFT_LEN)
            .map(|index| {
                let phase = std::f32::consts::TAU * index as f32 / FFT_LEN as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        Self {
            history: vec![0.0; FFT_LEN],
            window,
            fft,
            input,
            magnitudes: vec![0.0; output.len()],
            output,
            scratch,
            bands: Vec::new(),
            bands_rate: 0,
        }
    }

    fn clear(&mut self) {
        self.history.fill(0.0);
    }

    fn push(&mut self, samples: &[f32]) {
        let len = self.history.len();
        if samples.len() >= len {
            self.history
                .copy_from_slice(&samples[samples.len() - len..]);
        } else if !samples.is_empty() {
            self.history.copy_within(samples.len().., 0);
            self.history[len - samples.len()..].copy_from_slice(samples);
        }
    }

    fn is_silent(&self) -> bool {
        self.history.iter().all(|sample| *sample == 0.0)
    }

    fn frame(&mut self, sample_rate: u32) -> VisualFrame {
        let wave = self.history[FFT_LEN - VISUAL_WAVE_LEN..]
            .iter()
            .map(|sample| (sample * 10_000.0).round() / 10_000.0)
            .collect();
        if sample_rate == 0 || self.is_silent() {
            return VisualFrame {
                bands: vec![VISUAL_FLOOR_DB; VISUAL_BANDS],
                wave,
            };
        }

        for ((input, sample), weight) in self.input.iter_mut().zip(&self.history).zip(&self.window)
        {
            *input = sample * weight;
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return VisualFrame {
                bands: vec![VISUAL_FLOOR_DB; VISUAL_BANDS],
                wave,
            };
        }
        // The Hann window halves a sine's peak, and the other half of its energy is in the
        // negative frequencies, so this scale makes a full-scale sine read 1.
        let scale = 4.0 / FFT_LEN as f32;
        for (magnitude, bin) in self.magnitudes.iter_mut().zip(&self.output) {
            *magnitude = bin.norm() * scale;
        }

        if self.bands_rate != sample_rate {
            self.bands = band_bins(sample_rate, self.magnitudes.len());
            self.bands_rate = sample_rate;
        }
        let bands = self
            .bands
            .iter()
            .map(|band| {
                let level = match *band {
                    Band::Bins { first, end } => self.magnitudes[first..end]
                        .iter()
                        .fold(0.0_f32, |loudest, level| loudest.max(*level)),
                    Band::Between { center } => {
                        let below = center.floor() as usize;
                        let fraction = center - below as f32;
                        let above = (below + 1).min(self.magnitudes.len() - 1);
                        self.magnitudes[below] * (1.0 - fraction)
                            + self.magnitudes[above] * fraction
                    }
                    Band::AboveNyquist => 0.0,
                };
                to_db(level)
            })
            .collect();
        VisualFrame { bands, wave }
    }
}

fn to_db(level: f32) -> f32 {
    if level <= 0.0 {
        return VISUAL_FLOOR_DB;
    }
    let db = (20.0 * level.log10()).max(VISUAL_FLOOR_DB);
    (db * 10.0).round() / 10.0
}

/// Which bins each band covers at this rate. `bins` counts them, from 0 Hz to Nyquist.
fn band_bins(sample_rate: u32, bins: usize) -> Vec<Band> {
    let bin_hz = sample_rate as f32 / FFT_LEN as f32;
    let ratio = VISUAL_HIGH_HZ / VISUAL_LOW_HZ;
    let edge = |index: usize| VISUAL_LOW_HZ * ratio.powf(index as f32 / VISUAL_BANDS as f32);
    (0..VISUAL_BANDS)
        .map(|index| {
            let (low, high) = (edge(index) / bin_hz, edge(index + 1) / bin_hz);
            let last = bins - 1;
            if low > last as f32 {
                return Band::AboveNyquist;
            }
            let first = low.ceil() as usize;
            let end = (high.ceil() as usize).min(bins);
            if first < end {
                Band::Bins { first, end }
            } else {
                Band::Between {
                    center: (low * high).sqrt().min(last as f32),
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    const RATE: u32 = 48_000;

    fn sine(hz: f32, amplitude: f32, frames: usize, start: usize) -> Vec<f32> {
        (start..start + frames)
            .map(|frame| {
                amplitude * (frame as f32 * hz * std::f32::consts::TAU / RATE as f32).sin()
            })
            .collect()
    }

    /// The band holding this frequency.
    fn band_of(hz: f32) -> usize {
        let position = (hz / VISUAL_LOW_HZ).ln() / (VISUAL_HIGH_HZ / VISUAL_LOW_HZ).ln();
        (position * VISUAL_BANDS as f32) as usize
    }

    #[test]
    fn a_sine_shows_in_its_own_band_at_its_level() {
        for hz in [50.0, 440.0, 1000.0, 9000.0] {
            let mut analysis = Analysis::new();
            analysis.push(&sine(hz, 0.5, FFT_LEN, 0));
            let frame = analysis.frame(RATE);
            assert_eq!(frame.bands.len(), VISUAL_BANDS);
            let band = band_of(hz);
            let loudest = frame
                .bands
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            assert!(
                loudest.0.abs_diff(band) <= 1,
                "{hz} Hz peaks in band {}",
                loudest.0
            );
            // Half of full scale is 6 dB down. A sine between two bins reads up to 1.4 dB
            // lower through the window.
            assert!(
                (-7.5..=-5.5).contains(loudest.1),
                "{hz} Hz reads {} dB",
                loudest.1
            );
            let far = if band > VISUAL_BANDS / 2 {
                5
            } else {
                VISUAL_BANDS - 5
            };
            assert!(
                frame.bands[far] < -60.0,
                "{hz} Hz leaks {} dB",
                frame.bands[far]
            );
        }
    }

    #[test]
    fn silence_reads_as_the_floor_and_the_wave_is_the_latest_samples() {
        let mut analysis = Analysis::new();
        let frame = analysis.frame(RATE);
        assert!(frame.bands.iter().all(|band| *band == VISUAL_FLOOR_DB));
        assert_eq!(frame.wave, vec![0.0; VISUAL_WAVE_LEN]);

        let samples: Vec<f32> = (0..FFT_LEN + 10)
            .map(|index| index as f32 / 8192.0)
            .collect();
        let rounded = |sample: f32| (sample * 10_000.0).round() / 10_000.0;
        analysis.push(&samples[..100]);
        analysis.push(&samples[100..]);
        let frame = analysis.frame(RATE);
        assert_eq!(frame.wave.len(), VISUAL_WAVE_LEN);
        assert_eq!(
            *frame.wave.last().unwrap(),
            rounded(*samples.last().unwrap())
        );
        assert_eq!(
            frame.wave[0],
            rounded(samples[samples.len() - VISUAL_WAVE_LEN])
        );
    }

    #[test]
    fn every_band_reads_something_and_bands_past_nyquist_read_the_floor() {
        for rate in [22_050, 44_100, 48_000, 96_000, 192_000] {
            let bands = band_bins(rate, FFT_LEN / 2 + 1);
            assert_eq!(bands.len(), VISUAL_BANDS);
            let above = bands
                .iter()
                .filter(|band| **band == Band::AboveNyquist)
                .count();
            if rate >= 44_100 {
                assert_eq!(above, 0, "{rate}");
            } else {
                assert!(above > 0, "{rate}");
            }
        }
        // Low bands are narrower than a bin, and read between bins.
        assert!(matches!(
            band_bins(RATE, FFT_LEN / 2 + 1)[0],
            Band::Between { .. }
        ));
    }

    /// Feeds the tap as an output device would, in real time, for `duration`.
    fn play(tap: &mut VisualTap, duration: Duration, mut make: impl FnMut(usize) -> Vec<f32>) {
        let block = 480;
        let started = Instant::now();
        let mut frame = 0;
        while started.elapsed() < duration {
            let mono = make(frame);
            let stereo: Vec<f32> = mono.iter().flat_map(|sample| [*sample, *sample]).collect();
            tap.on_output(&stereo, 2);
            frame += block;
            let due = Duration::from_secs_f64(frame as f64 / f64::from(RATE));
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                thread::sleep(wait);
            }
        }
    }

    #[test]
    fn frames_come_at_a_steady_rate_only_while_enabled() {
        let (sender, frames) = mpsc::channel();
        let (mut tap, visualizer) = visualizer(move |frame| {
            let _ = sender.send((Instant::now(), frame));
        });
        tap.prepare(RATE, 2);
        let tone = |frame| sine(1000.0, 0.5, 480, frame);

        play(&mut tap, Duration::from_millis(200), tone);
        assert!(frames.try_recv().is_err(), "no frames while disabled");

        visualizer.set_enabled(true);
        play(&mut tap, Duration::from_millis(1000), tone);
        let received: Vec<(Instant, VisualFrame)> = frames.try_iter().collect();
        let expected = VISUAL_FRAME_RATE as usize;
        assert!(
            received.len().abs_diff(expected) <= expected / 10,
            "{} frames in a second",
            received.len()
        );
        let (_, last) = received.last().unwrap();
        assert!(last.bands[band_of(1000.0)] > -10.0);
        let longest = received
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .max()
            .unwrap();
        assert!(longest < Duration::from_millis(50), "a gap of {longest:?}");

        // Silence sends one silent frame, then nothing until there is sound again.
        play(&mut tap, Duration::from_millis(400), |_| vec![0.0; 480]);
        let quiet: Vec<VisualFrame> = frames.try_iter().map(|(_, frame)| frame).collect();
        assert!(
            quiet
                .last()
                .unwrap()
                .bands
                .iter()
                .all(|band| *band == VISUAL_FLOOR_DB)
        );
        play(&mut tap, Duration::from_millis(200), |_| vec![0.0; 480]);
        assert!(frames.try_recv().is_err(), "frames kept coming in silence");

        visualizer.set_enabled(false);
        thread::sleep(Duration::from_millis(50));
        let _ = frames.try_iter().count();
        play(&mut tap, Duration::from_millis(200), tone);
        assert!(frames.try_recv().is_err(), "frames after disabling");
        drop(visualizer);
    }
}
