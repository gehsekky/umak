//! Converts decoded audio to the output's sample rate and channel count. Runs on the
//! engine thread, never the audio thread.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

const CHUNK_FRAMES: usize = 1024;

/// Copies frames to a new channel count. Mono goes to the first two channels. Extra
/// source channels are dropped; the library is all stereo, so a proper downmix waits.
pub fn map_channels(input: &[f32], from: usize, to: usize, output: &mut Vec<f32>) {
    if from == to {
        output.extend_from_slice(input);
        return;
    }
    for frame in input.chunks_exact(from) {
        for channel in 0..to {
            let sample = match frame.get(channel) {
                Some(sample) => *sample,
                None if from == 1 && channel == 1 => frame[0],
                None => 0.0,
            };
            output.push(sample);
        }
    }
}

struct Converter {
    resampler: Fft<f32>,
    /// Frames still to drop from the start of the output: the resampler's own delay.
    /// Trimming it keeps the position accurate.
    delay_left: usize,
    pending: Vec<f32>,
    converted: Vec<f32>,
    frames_in: u64,
    frames_out: u64,
}

/// Interleaved sample rate conversion, with a pass-through when the rates match.
pub struct RateConverter {
    channels: usize,
    converter: Option<Converter>,
}

impl RateConverter {
    pub fn new(from_rate: u32, to_rate: u32, channels: usize) -> Result<Self, String> {
        if from_rate == to_rate {
            return Ok(Self {
                channels,
                converter: None,
            });
        }
        let resampler = Fft::<f32>::new(
            from_rate as usize,
            to_rate as usize,
            CHUNK_FRAMES,
            channels,
            FixedSync::Input,
        )
        .map_err(|error| error.to_string())?;
        let delay_left = resampler.output_delay();
        let converted = vec![0.0; resampler.output_frames_max() * channels];
        Ok(Self {
            channels,
            converter: Some(Converter {
                delay_left,
                pending: Vec::new(),
                converted,
                frames_in: 0,
                frames_out: 0,
                resampler,
            }),
        })
    }

    /// Converts as much as whole chunks allow, keeping the rest for the next call.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), String> {
        let channels = self.channels;
        let Some(state) = &mut self.converter else {
            output.extend_from_slice(input);
            return Ok(());
        };
        state.pending.extend_from_slice(input);
        let mut used = 0;
        while state.pending.len() - used >= state.resampler.input_frames_next() * channels {
            let needed = state.resampler.input_frames_next() * channels;
            state.frames_out += convert(
                &mut state.resampler,
                &state.pending[used..used + needed],
                &mut state.converted,
                &mut state.delay_left,
                channels,
                output,
            )? as u64;
            state.frames_in += (needed / channels) as u64;
            used += needed;
        }
        state.pending.drain(..used);
        Ok(())
    }

    /// Converts what is left at the end of a track. The resampler holds audio back, so
    /// silence pushes the last real frames out, stopping at the exact expected length.
    pub fn finish(&mut self, output: &mut Vec<f32>) -> Result<(), String> {
        let channels = self.channels;
        let Some(state) = &mut self.converter else {
            return Ok(());
        };
        state.frames_in += (state.pending.len() / channels) as u64;
        let ratio = state.resampler.resample_ratio();
        let expected = (state.frames_in as f64 * ratio).round() as u64;
        let mut chunk = std::mem::take(&mut state.pending);
        while state.frames_out < expected {
            chunk.resize(state.resampler.input_frames_next() * channels, 0.0);
            state.frames_out += convert(
                &mut state.resampler,
                &chunk,
                &mut state.converted,
                &mut state.delay_left,
                channels,
                output,
            )? as u64;
            chunk.clear();
        }
        let extra = (state.frames_out - expected) as usize;
        output.truncate(output.len() - extra * channels);
        state.frames_out = expected;
        Ok(())
    }

    /// Forgets all queued audio, for a seek.
    pub fn reset(&mut self) {
        if let Some(state) = &mut self.converter {
            state.resampler.reset();
            state.delay_left = state.resampler.output_delay();
            state.pending.clear();
            state.frames_in = 0;
            state.frames_out = 0;
        }
    }
}

fn convert(
    resampler: &mut Fft<f32>,
    chunk: &[f32],
    converted: &mut [f32],
    delay_left: &mut usize,
    channels: usize,
    output: &mut Vec<f32>,
) -> Result<usize, String> {
    let frames_in = chunk.len() / channels;
    let frames_out = converted.len() / channels;
    let input =
        InterleavedSlice::new(chunk, channels, frames_in).map_err(|error| error.to_string())?;
    let mut target = InterleavedSlice::new_mut(converted, channels, frames_out)
        .map_err(|error| error.to_string())?;
    let (_, written) = resampler
        .process_into_buffer(&input, &mut target, None)
        .map_err(|error| error.to_string())?;
    let drop = (*delay_left).min(written);
    *delay_left -= drop;
    output.extend_from_slice(&converted[drop * channels..written * channels]);
    Ok(written - drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frequency: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let value = (frame as f32 * frequency * std::f32::consts::TAU / rate as f32).sin();
                [value, value]
            })
            .collect()
    }

    fn crossings(samples: &[f32]) -> usize {
        samples
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count()
    }

    fn convert_all(from: u32, to: u32, input: &[f32], block: usize) -> Vec<f32> {
        let mut converter = RateConverter::new(from, to, 2).unwrap();
        let mut output = Vec::new();
        for chunk in input.chunks(block * 2) {
            converter.process(chunk, &mut output).unwrap();
        }
        converter.finish(&mut output).unwrap();
        output
    }

    #[test]
    fn matching_rates_pass_samples_through_unchanged() {
        let input = sine(440.0, 48_000, 1000);
        assert_eq!(convert_all(48_000, 48_000, &input, 100), input);
    }

    #[test]
    fn conversion_keeps_duration_and_pitch_for_every_rate_in_the_library() {
        for from in [44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
            for to in [44_100, 48_000] {
                let seconds = 2;
                let input = sine(1000.0, from, from as usize * seconds);
                let output = convert_all(from, to, &input, 777);
                let frames = output.len() / 2;
                let expected = to as usize * seconds;
                let off = frames.abs_diff(expected);
                assert!(
                    off <= 4,
                    "{from} to {to}: {frames} frames, expected {expected}"
                );

                let cycles = crossings(&output);
                assert!(
                    cycles.abs_diff(2000) <= 2,
                    "{from} to {to}: {cycles} cycles"
                );
            }
        }
    }

    #[test]
    fn the_start_of_the_audio_is_not_delayed() {
        let mut input = vec![0.0; 2 * 4800];
        input.extend(sine(1000.0, 48_000, 48_000));
        let output = convert_all(48_000, 44_100, &input, 512);
        let first_sound = output
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 0.2)
            .unwrap();
        // 100 ms of silence at the output rate.
        assert!(
            first_sound.abs_diff(4410) < 40,
            "sound starts at {first_sound}"
        );
    }

    #[test]
    fn reset_forgets_queued_audio() {
        let mut converter = RateConverter::new(44_100, 48_000, 2).unwrap();
        let mut output = Vec::new();
        converter
            .process(&sine(1000.0, 44_100, 1500), &mut output)
            .unwrap();
        converter.reset();
        let mut after = Vec::new();
        converter.finish(&mut after).unwrap();
        assert!(after.iter().all(|sample| sample.abs() < 0.01));
    }

    #[test]
    fn channels_map_up_and_down() {
        let mut output = Vec::new();
        map_channels(&[0.5, -0.5], 1, 2, &mut output);
        assert_eq!(output, [0.5, 0.5, -0.5, -0.5]);

        output.clear();
        map_channels(&[0.1, 0.2, 0.3, 0.4], 2, 4, &mut output);
        map_channels(&[0.5, 0.6], 2, 1, &mut output);
        assert_eq!(output, [0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.0, 0.0, 0.5]);
    }
}
