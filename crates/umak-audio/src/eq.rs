//! The graphic equalizer. Runs on the audio thread, so it must not block, allocate or
//! take locks. New settings arrive as atomics, and the filters move toward them a small
//! step at a time, recomputed with plain arithmetic, so a change does not click.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::effects::Effect;
use crate::protocol::{EQ_BANDS, EQ_FREQUENCIES};

/// One octave wide, so neighboring bands meet.
const Q: f64 = std::f64::consts::SQRT_2;
/// Filters get this close to the Nyquist frequency and no closer.
const MAX_FREQUENCY_RATIO: f64 = 0.45;
/// How often filters are recomputed during a change.
const UPDATE_FRAMES: usize = 64;
/// How far a band moves per update. Over 12 dB this takes about 60 ms at 48 kHz.
const STEP_DB: f32 = 0.2;

/// What the engine thread sets and the audio thread reads.
pub struct EqControl {
    enabled: AtomicBool,
    bands_db: [AtomicU32; EQ_BANDS],
}

impl Default for EqControl {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            bands_db: std::array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
        }
    }
}

impl EqControl {
    pub fn set(&self, enabled: bool, bands_db: &[f32; EQ_BANDS]) {
        for (band, db) in self.bands_db.iter().zip(bands_db) {
            band.store(db.to_bits(), Ordering::Relaxed);
        }
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn targets(&self) -> [f32; EQ_BANDS] {
        if !self.enabled.load(Ordering::Relaxed) {
            return [0.0; EQ_BANDS];
        }
        std::array::from_fn(|band| f32::from_bits(self.bands_db[band].load(Ordering::Relaxed)))
    }
}

/// A biquad's coefficients, normalized so a0 is 1.
#[derive(Clone, Copy)]
struct Coefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

const PASS_THROUGH: Coefficients = Coefficients {
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
    a1: 0.0,
    a2: 0.0,
};

/// A peaking filter, from the Audio EQ Cookbook.
fn peaking(frequency: f64, rate: f64, db: f64) -> Coefficients {
    if db == 0.0 || frequency >= rate * MAX_FREQUENCY_RATIO {
        return PASS_THROUGH;
    }
    let a = 10.0_f64.powf(db / 40.0);
    let omega = std::f64::consts::TAU * frequency / rate;
    let alpha = omega.sin() / (2.0 * Q);
    let cos = omega.cos();
    let a0 = 1.0 + alpha / a;
    Coefficients {
        b0: (1.0 + alpha * a) / a0,
        b1: (-2.0 * cos) / a0,
        b2: (1.0 - alpha * a) / a0,
        a1: (-2.0 * cos) / a0,
        a2: (1.0 - alpha / a) / a0,
    }
}

/// The filter memory of one band on one channel.
type BandState = [f64; 2];

pub struct Equalizer {
    rate: f64,
    current_db: [f32; EQ_BANDS],
    coefficients: [Coefficients; EQ_BANDS],
    /// Lowers the whole signal by the largest boost, so boosting a band cannot by itself
    /// push the signal past full scale.
    headroom: f32,
    state: Vec<[BandState; EQ_BANDS]>,
    control: std::sync::Arc<EqControl>,
}

impl Equalizer {
    /// Created off the audio thread, because it allocates.
    pub fn new(sample_rate: u32, channels: usize, control: std::sync::Arc<EqControl>) -> Self {
        Self {
            rate: f64::from(sample_rate),
            current_db: [0.0; EQ_BANDS],
            coefficients: [PASS_THROUGH; EQ_BANDS],
            headroom: 1.0,
            state: vec![[[0.0; 2]; EQ_BANDS]; channels],
            control,
        }
    }

    /// Whether this will change the audio.
    pub fn is_active(&self) -> bool {
        self.current_db.iter().any(|db| *db != 0.0)
            || self.control.targets().iter().any(|db| *db != 0.0)
    }

    fn headroom_for(bands_db: &[f32; EQ_BANDS]) -> f32 {
        let boost = bands_db.iter().fold(0.0_f32, |most, db| most.max(*db));
        10.0_f32.powf(-boost / 20.0)
    }

    fn set_band(&mut self, band: usize, db: f32) {
        self.current_db[band] = db;
        self.coefficients[band] =
            peaking(f64::from(EQ_FREQUENCIES[band]), self.rate, f64::from(db));
    }

    /// Goes straight to the settings, for audio starting after silence, and forgets
    /// what the filters held.
    pub fn restart(&mut self) {
        let targets = self.control.targets();
        for (band, db) in targets.iter().enumerate() {
            if self.current_db[band] != *db {
                self.set_band(band, *db);
            }
        }
        self.headroom = Self::headroom_for(&self.current_db);
        self.state
            .iter_mut()
            .for_each(|channel| *channel = [[0.0; 2]; EQ_BANDS]);
    }

    fn step_toward(&mut self, targets: &[f32; EQ_BANDS]) {
        for (band, target) in targets.iter().enumerate() {
            let current = self.current_db[band];
            if current != *target {
                let moved = if (*target - current).abs() <= STEP_DB {
                    *target
                } else {
                    current + STEP_DB * (*target - current).signum()
                };
                self.set_band(band, moved);
            }
        }
    }
}

impl Effect for Equalizer {
    fn process(&mut self, samples: &mut [f32], channels: usize) {
        let targets = self.control.targets();
        if self.current_db == [0.0; EQ_BANDS] && targets == [0.0; EQ_BANDS] {
            return;
        }
        for block in samples.chunks_mut(UPDATE_FRAMES * channels) {
            self.step_toward(&targets);
            let next_headroom = Self::headroom_for(&self.current_db);
            let frames = (block.len() / channels).max(1);
            let headroom_step = (next_headroom - self.headroom) / frames as f32;
            let mut headroom = self.headroom;
            for frame in block.chunks_exact_mut(channels) {
                headroom += headroom_step;
                for (sample, state) in frame.iter_mut().zip(self.state.iter_mut()) {
                    let mut value = f64::from(*sample);
                    for (filter, memory) in self.coefficients.iter().zip(state.iter_mut()) {
                        let output = filter.b0 * value + memory[0];
                        memory[0] = filter.b1 * value - filter.a1 * output + memory[1];
                        memory[1] = filter.b2 * value - filter.a2 * output;
                        value = output;
                    }
                    *sample = value as f32 * headroom;
                }
            }
            self.headroom = next_headroom;
        }
        if self.current_db == [0.0; EQ_BANDS] {
            // Flat again, so nothing should linger for when the audio goes back to
            // passing straight through.
            self.state
                .iter_mut()
                .for_each(|channel| *channel = [[0.0; 2]; EQ_BANDS]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const RATE: u32 = 48_000;

    fn sine(frequency: f32, frames: usize, level: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let phase =
                    frame as f64 * f64::from(frequency) * std::f64::consts::TAU / f64::from(RATE);
                let value = phase.sin() as f32 * level;
                [value, value]
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum: f64 = samples
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum();
        (sum / samples.len() as f64).sqrt()
    }

    fn equalizer(bands: &[f32; EQ_BANDS]) -> Equalizer {
        let control = Arc::new(EqControl::default());
        control.set(true, bands);
        let mut eq = Equalizer::new(RATE, 2, control);
        eq.restart();
        eq
    }

    /// The change in level of a steady tone, in decibels.
    fn response_db(bands: &[f32; EQ_BANDS], frequency: f32) -> f64 {
        let mut eq = equalizer(bands);
        let input = sine(frequency, RATE as usize, 0.1);
        let mut output = input.clone();
        eq.process(&mut output, 2);
        // The second half, once the filters have settled.
        let half = input.len() / 2;
        20.0 * (rms(&output[half..]) / rms(&input[half..])).log10()
    }

    fn one_band(band: usize, db: f32) -> [f32; EQ_BANDS] {
        let mut bands = [0.0; EQ_BANDS];
        bands[band] = db;
        bands
    }

    #[test]
    fn each_band_changes_its_own_frequency_by_the_stated_amount() {
        for (band, frequency) in EQ_FREQUENCIES.iter().enumerate() {
            // A cut has no headroom to account for, so this is the band alone.
            let measured = response_db(&one_band(band, -6.0), *frequency as f32);
            assert!(
                (measured + 6.0).abs() < 0.1,
                "{frequency} Hz: {measured:.2} dB"
            );
        }
    }

    #[test]
    fn a_boost_keeps_its_shape_and_lowers_everything_to_make_room() {
        let bands = one_band(5, 6.0);
        let at_band = response_db(&bands, 1000.0);
        assert!(at_band.abs() < 0.1, "at the band: {at_band:.2} dB");
        let far_away = response_db(&bands, 60.0);
        assert!((far_away + 6.0).abs() < 0.1, "far away: {far_away:.2} dB");
    }

    #[test]
    fn a_band_leaves_frequencies_far_from_it_alone() {
        for (band, frequency) in [(0, 2000.0), (5, 62.0), (9, 250.0)] {
            let measured = response_db(&one_band(band, -12.0), frequency);
            assert!(
                measured.abs() < 0.2,
                "band {band} at {frequency}: {measured:.2} dB"
            );
        }
    }

    #[test]
    fn a_band_above_the_nyquist_limit_is_skipped_at_a_low_output_rate() {
        let mut eq = Equalizer::new(22_050, 1, Arc::new(EqControl::default()));
        eq.control.set(true, &one_band(9, -12.0));
        eq.restart();
        let input: Vec<f32> = (0..2000)
            .map(|frame| (frame as f32 * 0.1).sin() * 0.5)
            .collect();
        let mut output = input.clone();
        eq.process(&mut output, 1);
        assert_eq!(output, input);
    }

    #[test]
    fn flat_or_disabled_leaves_samples_untouched() {
        let input = sine(1000.0, 4800, 0.5);
        let mut flat = equalizer(&[0.0; EQ_BANDS]);
        let mut output = input.clone();
        flat.process(&mut output, 2);
        assert_eq!(output, input);

        let control = Arc::new(EqControl::default());
        control.set(false, &[9.0; EQ_BANDS]);
        let mut disabled = Equalizer::new(RATE, 2, control);
        let mut output = input.clone();
        disabled.process(&mut output, 2);
        assert_eq!(output, input);
        assert!(!disabled.is_active());
    }

    #[test]
    fn changing_a_band_while_playing_does_not_click() {
        let mut eq = equalizer(&[0.0; EQ_BANDS]);
        let mut audio = sine(1000.0, 24_000, 0.25);
        let (first, rest) = audio.split_at_mut(2 * 2400);
        eq.process(first, 2);
        eq.control.set(true, &one_band(5, 12.0));
        eq.process(rest, 2);

        // The largest jump a 1 kHz tone makes on its own, at the level it ends up at.
        let steady = sine(1000.0, 200, 0.25);
        let natural = steady
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        let left: Vec<f32> = audio.chunks_exact(2).map(|frame| frame[0]).collect();
        let largest = left
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        assert!(largest < natural * 1.25, "{largest} against {natural}");
    }

    #[test]
    fn a_change_finishes_at_the_new_setting() {
        let mut eq = equalizer(&[0.0; EQ_BANDS]);
        eq.control.set(true, &one_band(5, 12.0));
        let mut audio = sine(1000.0, 48_000, 0.1);
        eq.process(&mut audio, 2);
        assert_eq!(eq.current_db[5], 12.0);
        let tail = 2 * 24_000;
        let level = rms(&audio[tail..]) / rms(&sine(1000.0, 24_000, 0.1));
        assert!((20.0 * level.log10()).abs() < 0.1, "{level}");
    }

    #[test]
    fn going_flat_stops_processing_and_leaves_nothing_behind() {
        let mut eq = equalizer(&one_band(5, 6.0));
        let mut audio = sine(1000.0, 4800, 0.5);
        eq.process(&mut audio, 2);
        eq.control.set(true, &[0.0; EQ_BANDS]);
        let mut audio = sine(1000.0, 4800, 0.5);
        eq.process(&mut audio, 2);

        let input = sine(1000.0, 4800, 0.5);
        let mut output = input.clone();
        eq.process(&mut output, 2);
        assert_eq!(output, input);
    }
}
