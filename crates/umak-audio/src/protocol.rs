//! Typed player commands and events. The app passes them to the engine in-process, and
//! `umak-play` carries them as one JSON object per line over stdin and stdout.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackEntry {
    pub id: String,
    pub path: String,
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
