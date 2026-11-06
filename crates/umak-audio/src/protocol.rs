//! Typed player commands and events. The app passes them to the engine in-process, and
//! `umak-play` carries them as one JSON object per line over stdin and stdout.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    /// Replaces the player playlist and starts playing it.
    #[serde(rename_all = "camelCase")]
    Load {
        tracks: Vec<TrackEntry>,
        start_index: usize,
        start_ms: u64,
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
    Error {
        message: String,
    },
}
