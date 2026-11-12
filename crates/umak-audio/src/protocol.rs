//! Typed player commands and events. The app passes them to the engine in-process, and
//! `umak-play` carries them as one JSON object per line over stdin and stdout.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The graphic equalizer's bands, in hertz.
pub const EQ_BANDS: usize = 10;
pub const EQ_FREQUENCIES: [u32; EQ_BANDS] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackEntry {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub gain: TrackGain,
}

/// A track's ReplayGain tags, as written in the file. The gains are in decibels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackGain {
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// Which ReplayGain value to play by. A track lacking that one uses the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ReplayGainMode {
    /// Samples pass through unchanged, and the preamp does not apply.
    #[default]
    Off,
    Track,
    Album,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PlayerCommand {
    /// Replaces the player playlist and starts playing it, or with `paused`, waits at the
    /// start position for Play.
    #[serde(rename_all = "camelCase")]
    Load {
        tracks: Vec<TrackEntry>,
        start_index: usize,
        start_ms: u64,
        #[serde(default)]
        paused: bool,
    },
    /// Changes the list without disturbing what is playing. For each entry in `tracks`,
    /// `origin` gives its index in the old list, or null for a new entry. The engine maps
    /// its own position through this, so an edit cannot race a track change.
    EditList {
        tracks: Vec<TrackEntry>,
        origin: Vec<Option<usize>>,
    },
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Seek {
        ms: u64,
    },
    /// Output gain from 0 to 1. 1 leaves the samples unchanged.
    SetVolume {
        volume: f32,
    },
    /// How to level tracks. The preamp is in decibels, on top of the ReplayGain value.
    #[serde(rename_all = "camelCase")]
    SetGain {
        replay_gain: ReplayGainMode,
        preamp_db: f32,
    },
    /// The equalizer. The bands are in decibels, in the order of `EQ_FREQUENCIES`.
    SetEq {
        enabled: bool,
        bands_db: [f32; EQ_BANDS],
    },
    /// Past this far into a track, Previous starts it again instead of going back one. At 0,
    /// Previous always goes back.
    SetRestartThreshold {
        ms: u64,
    },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PlayerEvent {
    #[serde(rename_all = "camelCase")]
    Ready {
        device: String,
        sample_rate: u32,
        channels: u16,
    },
    /// The state, the current track, or both changed.
    #[serde(rename_all = "camelCase")]
    State {
        state: PlaybackState,
        index: Option<usize>,
        track_id: Option<String>,
        position_ms: u64,
        duration_ms: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Position {
        position_ms: u64,
    },
    /// Playback health, sent with each position while playing.
    #[serde(rename_all = "camelCase")]
    Health {
        /// Times the queue ran dry since the engine started. Each is an audible gap.
        underruns: u64,
        queued_ms: u64,
        /// The longest wait between output callbacks since the last report.
        longest_gap_us: u64,
    },
    /// A track could not be played. Playback moves on to the next one.
    TrackFailed {
        id: String,
        message: String,
    },
    /// The device stopped taking audio, as when it is unplugged. Nothing more will be heard
    /// from this engine, so it needs replacing.
    OutputFailed {
        message: String,
    },
    Error {
        message: String,
    },
}

/// What the output sounds like now, for visualizers. Sent at a steady rate while they are
/// open, and once more when the output falls silent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct VisualFrame {
    /// Levels in dBFS, in bands spaced evenly in pitch from `VISUAL_LOW_HZ` to
    /// `VISUAL_HIGH_HZ`. A full-scale sine reads 0, and nothing reads below
    /// `VISUAL_FLOOR_DB`.
    pub bands: Vec<f32>,
    /// The latest samples, the channels mixed to one, oldest first.
    pub wave: Vec<f32>,
}

pub const VISUAL_BANDS: usize = 96;
pub const VISUAL_LOW_HZ: f32 = 20.0;
pub const VISUAL_HIGH_HZ: f32 = 20_000.0;
pub const VISUAL_FLOOR_DB: f32 = -100.0;
pub const VISUAL_WAVE_LEN: usize = 1024;
pub const VISUAL_FRAME_RATE: u32 = 60;
