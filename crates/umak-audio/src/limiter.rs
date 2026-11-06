//! The last stage before the volume: keeps the signal at or under full scale. Runs on the
//! audio thread, so it must not block, allocate or take locks.
//!
//! It reacts within the frame that goes over, so it adds no latency and needs no
//! lookahead, which would delay all the audio and hold back the end of a track. Audio
//! under full scale passes through exactly, once any reduction has recovered.

use crate::effects::Effect;

/// The most the signal may reach. Just under 1, so rounding cannot pass full scale.
const CEILING: f32 = 0.9999;
/// How long the gain takes to recover after a peak, as a time constant.
const RELEASE_SECONDS: f32 = 0.1;
/// Little enough reduction to stop reducing at all.
const RECOVERED: f32 = 0.0001;

pub struct Limiter {
    /// How far the gain is below 1. Holding this and not the gain keeps recovery exact,
    /// because a float near 1 is too coarse to take the small steps near the end.
    reduction: f32,
    release: f32,
}

impl Limiter {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            reduction: 0.0,
            release: (-1.0 / (RELEASE_SECONDS * sample_rate as f32)).exp(),
        }
    }

    /// Forgets any reduction, for audio starting after silence.
    pub fn restart(&mut self) {
        self.reduction = 0.0;
    }
}

impl Effect for Limiter {
    fn process(&mut self, samples: &mut [f32], channels: usize) {
        for frame in samples.chunks_exact_mut(channels) {
            self.reduction *= self.release;
            if self.reduction < RECOVERED {
                self.reduction = 0.0;
            }
            // Channels share one gain, so limiting cannot shift the stereo image.
            let peak = frame
                .iter()
                .fold(0.0_f32, |most, sample| most.max(sample.abs()));
            if peak * (1.0 - self.reduction) > CEILING {
                self.reduction = 1.0 - CEILING / peak;
            }
            if self.reduction > 0.0 {
                let gain = 1.0 - self.reduction;
                for sample in frame {
                    *sample *= gain;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(frames: usize, level: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let value = (frame as f32 * 1000.0 * std::f32::consts::TAU / RATE as f32).sin();
                [value * level, value * level]
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |most, sample| most.max(sample.abs()))
    }

    #[test]
    fn audio_under_full_scale_passes_through_exactly() {
        let input = sine(4800, 0.99);
        let mut output = input.clone();
        Limiter::new(RATE).process(&mut output, 2);
        assert_eq!(output, input);
    }

    #[test]
    fn no_sample_ever_passes_full_scale_however_loud_the_input() {
        for level in [1.01, 1.5, 4.0, 16.0, 1000.0] {
            let mut audio = sine(48_000, level);
            Limiter::new(RATE).process(&mut audio, 2);
            assert!(peak(&audio) <= 1.0, "level {level}: {}", peak(&audio));
            // Limited, but not silenced.
            assert!(peak(&audio) > 0.9, "level {level}: {}", peak(&audio));
        }
    }

    #[test]
    fn it_recovers_after_a_peak_and_then_leaves_audio_alone() {
        let mut limiter = Limiter::new(RATE);
        let mut burst = sine(4800, 2.0);
        limiter.process(&mut burst, 2);

        // A second is ten time constants.
        let mut quiet = sine(48_000, 0.5);
        limiter.process(&mut quiet, 2);
        assert_eq!(limiter.reduction, 0.0);
        let input = sine(4800, 0.5);
        let mut output = input.clone();
        limiter.process(&mut output, 2);
        assert_eq!(output, input);
    }

    #[test]
    fn recovery_is_gradual() {
        let mut limiter = Limiter::new(RATE);
        let mut burst = sine(4800, 2.0);
        limiter.process(&mut burst, 2);
        let after_burst = limiter.reduction;
        assert!(after_burst > 0.49, "{after_burst}");

        let mut block = vec![0.0; 2 * 480];
        limiter.process(&mut block, 2);
        // 10 ms is a tenth of the time constant.
        let expected = after_burst * (-0.1_f32).exp();
        assert!(
            (limiter.reduction - expected).abs() < 0.01,
            "{}",
            limiter.reduction
        );
    }

    #[test]
    fn channels_are_reduced_together() {
        let mut limiter = Limiter::new(RATE);
        let mut frame = [2.0, 0.5];
        limiter.process(&mut frame, 2);
        assert!((frame[0] - CEILING).abs() < 1e-5);
        assert!((frame[1] - CEILING / 4.0).abs() < 1e-5);
    }
}
