//! Streaming decode: packets are read as needed, so a file of any size costs the same
//! memory, and it reads through `AudioSource`, so the bytes can come from anywhere.
//!
//! Encoders add silence to the start and end of a track, which must go for tracks to
//! join without a gap. Symphonia removes it from MP3 using the LAME tag. For AAC in MP4
//! it does not, so this reads the iTunes tag that records it. Files with neither tag
//! play with their silence, since nothing says how much there is.

use std::ops::Range;

use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::source::AudioSource;

const NANOS_PER_MS: i128 = 1_000_000;
const NANOS_PER_SECOND: i128 = 1_000_000_000;
const ITUNES_GAPLESS_TAG: &str = "itunsmpb";

/// Reads the delay and length in frames from an iTunes gapless tag's value, which is hex
/// fields: a reserved one, the delay, the padding, the length, and more that do not matter.
fn parse_itunes_gapless(value: &str) -> Option<(u64, u64)> {
    let mut fields = value.split_whitespace().skip(1);
    let delay = u64::from_str_radix(fields.next()?, 16).ok()?;
    let _padding = fields.next()?;
    let length = u64::from_str_radix(fields.next()?, 16).ok()?;
    (length > 0).then_some((delay, length))
}

/// Which decoded frames of a packet lie between `from` and `to`, given in packet time.
fn kept_frames(
    frames: usize,
    start: i64,
    length: i64,
    from: Option<i64>,
    to: Option<i64>,
) -> Range<usize> {
    let at = |time: i64| -> usize {
        if length <= 0 {
            return 0;
        }
        let offset = (time - start).clamp(0, length);
        (frames as i64 * offset / length) as usize
    };
    let first = from.map_or(0, at);
    let last = to.map_or(frames, at);
    first..last.max(first)
}

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    duration_ms: Option<u64>,
    /// Decoded audio before this timestamp is discarded: encoder delay, or what comes
    /// before a seek's target.
    skip_until: Option<i64>,
    /// Decoded audio from this timestamp on is encoder padding.
    end_at: Option<i64>,
    /// Encoder delay in frames that the container's timestamps include.
    delay_frames: u64,
    decode_errors: u64,
    scratch: Vec<f32>,
}

impl Decoder {
    pub fn open(source: &dyn AudioSource) -> Result<Self, String> {
        let media = source.open().map_err(|error| error.to_string())?;
        let stream = MediaSourceStream::new(media, Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = source.extension() {
            hint.with_extension(&extension);
        }
        let mut format = symphonia::default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|error| format!("could not read this file: {error}"))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or("this file has no audio track")?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or("this file's audio format is unknown")?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|error| format!("unsupported audio codec: {error}"))?;

        let is_aac = params.codec == CODEC_ID_AAC;
        let time_base = track.time_base;
        let sample_rate = params.sample_rate.unwrap_or(0);
        let channels = params
            .channels
            .as_ref()
            .map_or(0, |channels| channels.count());
        let mut duration_ms = track
            .num_frames
            .filter(|_| sample_rate > 0)
            .map(|frames| frames * 1000 / u64::from(sample_rate))
            .or_else(|| {
                let units = track.duration?.get();
                let base = track.time_base?;
                Some(units * u64::from(base.numer.get()) * 1000 / u64::from(base.denom.get()))
            });

        let gapless = if is_aac && sample_rate > 0 {
            format.metadata().skip_to_latest().and_then(|revision| {
                let tag = revision.media.tags.iter().find(|tag| {
                    tag.raw
                        .key
                        .to_ascii_lowercase()
                        .ends_with(ITUNES_GAPLESS_TAG)
                })?;
                parse_itunes_gapless(&tag.raw.value.to_string())
            })
        } else {
            None
        };
        let to_time = |frames: u64| -> i64 {
            match time_base {
                Some(base) => {
                    (frames * u64::from(base.denom.get())
                        / (u64::from(base.numer.get()) * u64::from(sample_rate)))
                        as i64
                }
                None => frames as i64,
            }
        };
        let (mut skip_until, mut end_at, mut delay_frames) = (None, None, 0);
        if let Some((delay, length)) = gapless {
            skip_until = Some(to_time(delay));
            end_at = Some(to_time(delay + length));
            delay_frames = delay;
            duration_ms = Some(length * 1000 / u64::from(sample_rate));
        }
        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            duration_ms,
            skip_until,
            end_at,
            delay_frames,
            decode_errors: 0,
            scratch: Vec::new(),
        })
    }

    /// The sample rate. Some files state it only in their first decoded packet, so this
    /// can be zero until then.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    /// Packets that failed to decode and were skipped.
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }

    /// Appends the next decoded audio to `output` as interleaved samples. Returns false
    /// at the end of the stream.
    pub fn next_chunk(&mut self, output: &mut Vec<f32>) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) | Err(Error::ResetRequired) => return Ok(false),
                Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(error) => return Err(error.to_string()),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                // A damaged packet costs a moment of audio, not the whole track.
                Err(Error::DecodeError(_) | Error::IoError(_)) => {
                    self.decode_errors += 1;
                    continue;
                }
                Err(error) => return Err(error.to_string()),
            };
            self.sample_rate = decoded.spec().rate();
            self.channels = decoded.spec().channels().count();
            decoded.copy_to_vec_interleaved(&mut self.scratch);

            let frames = self.scratch.len() / self.channels.max(1);
            let start = packet.pts.get();
            let length = packet.dur.get() as i64;
            if self.skip_until.is_some_and(|from| start + length <= from) {
                continue;
            }
            let kept = kept_frames(frames, start, length, self.skip_until, self.end_at);
            self.skip_until = None;
            let has_audio = !kept.is_empty();
            output.extend_from_slice(
                &self.scratch[kept.start * self.channels..kept.end * self.channels],
            );
            if self.end_at.is_some_and(|end| start + length >= end) {
                return Ok(false);
            }
            if has_audio {
                return Ok(true);
            }
        }
    }

    /// Moves to a position. The next chunk starts exactly there.
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        let delay_nanos = if self.sample_rate > 0 {
            i128::from(self.delay_frames) * NANOS_PER_SECOND / i128::from(self.sample_rate)
        } else {
            0
        };
        let time = Time::try_from_nanos_i128(i128::from(ms) * NANOS_PER_MS + delay_nanos)
            .ok_or("that position is out of range")?;
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| error.to_string())?;
        self.decoder.reset();
        self.skip_until = Some(seeked.required_ts.get());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_itunes_gapless_tag_gives_the_delay_and_length() {
        let value =
            " 00000000 00000840 000003E1 00000000052F6A3F 00000000 00000000 00000000 00000000";
        assert_eq!(parse_itunes_gapless(value), Some((0x840, 0x052F_6A3F)));
    }

    #[test]
    fn a_damaged_or_empty_itunes_gapless_tag_is_ignored() {
        assert_eq!(parse_itunes_gapless(""), None);
        assert_eq!(parse_itunes_gapless(" 00000000 00000840"), None);
        assert_eq!(parse_itunes_gapless(" 0 840 3E1 not-hex"), None);
        assert_eq!(parse_itunes_gapless(" 0 840 3E1 0"), None);
    }

    #[test]
    fn only_frames_inside_the_track_are_kept() {
        // The first packet holds the end of the delay.
        assert_eq!(kept_frames(1024, 2048, 1024, Some(2112), None), 64..1024);
        // A packet inside the track keeps everything.
        assert_eq!(kept_frames(1024, 4096, 1024, None, Some(9000)), 0..1024);
        // The last packet holds the start of the padding.
        assert_eq!(kept_frames(1024, 8192, 1024, None, Some(9000)), 0..808);
        // A very short track can start and end in one packet.
        assert_eq!(kept_frames(1024, 0, 1024, Some(100), Some(200)), 100..200);
        // Timestamps need not count in frames.
        assert_eq!(kept_frames(1024, 0, 2048, Some(1024), None), 512..1024);
    }
}
