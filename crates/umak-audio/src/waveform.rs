//! A track's waveform for the seek bar: the whole file decoded once, summed up as columns of
//! peak and loudness.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::decoder::Decoder;
use crate::source::LocalFile;

/// Frames summed up together before the columns are made, so a file of any length takes a
/// bounded amount of memory per hour: about 1.3 MB at 48 kHz.
const BLOCK_FRAMES: usize = 256;

/// Each column is the loudest sample in its stretch of the track, and the root mean square
/// of its samples, both full scale at 255. Channels count together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waveform {
    pub peaks: Vec<u8>,
    pub levels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Block {
    peak: f32,
    squares: f64,
    samples: u64,
}

/// Decodes the file and sums it up in `columns` columns. A file shorter than that many blocks
/// gets fewer. Setting `stop` gives up early with an error.
pub fn waveform(path: &Path, columns: usize, stop: &AtomicBool) -> Result<Waveform, String> {
    let mut decoder = Decoder::open(&LocalFile(path.to_path_buf()))?;
    let mut blocks: Vec<Block> = Vec::new();
    let mut current = Block::default();
    let mut frames_in_block = 0;
    let mut decoded = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".to_owned());
        }
        decoded.clear();
        let more = decoder.next_chunk(&mut decoded)?;
        let channels = decoder.channels().max(1);
        for frame in decoded.chunks_exact(channels) {
            for sample in frame {
                current.peak = current.peak.max(sample.abs());
                current.squares += f64::from(*sample) * f64::from(*sample);
            }
            current.samples += channels as u64;
            frames_in_block += 1;
            if frames_in_block == BLOCK_FRAMES {
                blocks.push(current);
                current = Block::default();
                frames_in_block = 0;
            }
        }
        if !more {
            break;
        }
    }
    if frames_in_block > 0 {
        blocks.push(current);
    }
    if blocks.is_empty() {
        return Err("the file holds no audio".to_owned());
    }
    Ok(columns_of(&blocks, columns))
}

fn columns_of(blocks: &[Block], columns: usize) -> Waveform {
    let count = columns.min(blocks.len()).max(1);
    let scale = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (mut peaks, mut levels) = (Vec::with_capacity(count), Vec::with_capacity(count));
    for column in 0..count {
        let first = column * blocks.len() / count;
        let end = ((column + 1) * blocks.len() / count).max(first + 1);
        let mut summed = Block::default();
        for block in &blocks[first..end] {
            summed.peak = summed.peak.max(block.peak);
            summed.squares += block.squares;
            summed.samples += block.samples;
        }
        peaks.push(scale(f64::from(summed.peak)));
        levels.push(scale(
            (summed.squares / summed.samples.max(1) as f64).sqrt(),
        ));
    }
    Waveform { peaks, levels }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const RATE: u32 = 44_100;

    /// A 16-bit stereo WAV whose amplitude is `level(second)` in each second.
    fn wav(path: &Path, seconds: u32, level: impl Fn(u32) -> f32) {
        let mut samples = Vec::new();
        for frame in 0..RATE * seconds {
            let value = (frame as f32 * 441.0 * std::f32::consts::TAU / RATE as f32).sin();
            let sample = (value * level(frame / RATE) * f32::from(i16::MAX)) as i16;
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

    #[test]
    fn columns_follow_the_loudness_through_the_track() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steps.wav");
        // Silence, then half scale, then full scale.
        wav(&path, 3, |second| [0.0, 0.5, 1.0][second as usize]);
        let waveform = waveform(&path, 300, &AtomicBool::new(false)).unwrap();
        assert_eq!(waveform.peaks.len(), 300);
        assert_eq!(waveform.levels.len(), 300);
        let (silent, half, full) = (50, 150, 250);
        assert_eq!(waveform.peaks[silent], 0);
        assert!(
            (waveform.peaks[half] as i32 - 128).abs() <= 2,
            "{}",
            waveform.peaks[half]
        );
        assert!(waveform.peaks[full] >= 253, "{}", waveform.peaks[full]);
        // A sine's RMS is its peak over the square root of two.
        assert!(
            (waveform.levels[full] as i32 - 180).abs() <= 2,
            "{}",
            waveform.levels[full]
        );
    }

    #[test]
    fn a_short_file_gets_a_column_per_block_and_stop_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.wav");
        wav(&path, 1, |_| 0.5);
        let waveform = waveform(&path, 100_000, &AtomicBool::new(false)).unwrap();
        assert_eq!(waveform.peaks.len(), (RATE as usize).div_ceil(BLOCK_FRAMES));
        assert!(super::waveform(&path, 10, &AtomicBool::new(true)).is_err());
        assert!(
            super::waveform(&dir.path().join("none.wav"), 10, &AtomicBool::new(false)).is_err()
        );
    }
}
