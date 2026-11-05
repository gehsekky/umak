//! Streaming decode: packets are read as needed, so a file of any size costs the same
//! memory, and it reads through `AudioSource`, so the bytes can come from anywhere.

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::source::AudioSource;

const NANOS_PER_MS: i128 = 1_000_000;

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    duration_ms: Option<u64>,
    /// After a seek, decoded audio before this timestamp is discarded.
    skip_until: Option<i64>,
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
        let format = symphonia::default::get_probe()
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

        let sample_rate = params.sample_rate.unwrap_or(0);
        let channels = params
            .channels
            .as_ref()
            .map_or(0, |channels| channels.count());
        let duration_ms = track
            .num_frames
            .filter(|_| sample_rate > 0)
            .map(|frames| frames * 1000 / u64::from(sample_rate))
            .or_else(|| {
                let units = track.duration?.get();
                let base = track.time_base?;
                Some(units * u64::from(base.numer.get()) * 1000 / u64::from(base.denom.get()))
            });
        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            duration_ms,
            skip_until: None,
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
            let mut skip_frames = 0;
            if let Some(required) = self.skip_until {
                let start = packet.pts.get();
                let length = packet.dur.get() as i64;
                if start + length <= required {
                    continue;
                }
                self.skip_until = None;
                if required > start && length > 0 {
                    skip_frames = (frames as i64 * (required - start) / length) as usize;
                }
            }
            output.extend_from_slice(&self.scratch[skip_frames * self.channels..]);
            if frames > skip_frames {
                return Ok(true);
            }
        }
    }

    /// Moves to a position. The next chunk starts exactly there.
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        let time = Time::try_from_nanos_i128(i128::from(ms) * NANOS_PER_MS)
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
