//! The engine thread: holds the player playlist, decodes ahead and keeps the sample
//! queue full. Commands arrive on a channel and events go to a callback.
//!
//! Playback is gapless because the queue never ends between tracks: when a track runs
//! out of audio, the next one's goes straight after it. What is audible lags what is
//! decoded by up to a queue's length, so the engine notes the frame where each track
//! starts, and changes the current track when playback reaches that frame.

use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::decoder::Decoder;
use crate::effects::{EffectChain, OutputTap};
use crate::output::{AudioOutput, OutputFormat};
use crate::protocol::{PlaybackState, PlayerCommand, PlayerEvent, TrackEntry};
use crate::render::{self, Feeds, GainChange, Shared};
use crate::resample::{RateConverter, map_channels};
use crate::source::{AudioSource, LocalFile};

const QUEUE_SECONDS: usize = 2;
const IDLE_WAIT: Duration = Duration::from_millis(20);
const FULL_QUEUE_WAIT: Duration = Duration::from_millis(5);
const FLUSH_TIMEOUT: Duration = Duration::from_millis(500);
const POSITION_INTERVAL: Duration = Duration::from_millis(250);
/// Past this point, Previous restarts the track instead of going back one.
const RESTART_THRESHOLD_MS: u64 = 3000;

type EventSink = Box<dyn FnMut(PlayerEvent) + Send>;

/// A running engine. Dropping it stops playback and ends the thread.
pub struct Engine {
    commands: Sender<PlayerCommand>,
    thread: Option<JoinHandle<()>>,
}

/// What goes into the fixed signal chain, for the audio thread.
#[derive(Default)]
pub struct ChainParts {
    pub effects: EffectChain,
    pub tap: Option<Box<dyn OutputTap>>,
}

impl Engine {
    /// Starts the engine. `make_output` runs on the engine thread, because a device
    /// stream may have to stay on the thread that created it.
    pub fn spawn(
        make_output: impl FnOnce() -> Result<Box<dyn AudioOutput>, String> + Send + 'static,
        chain: ChainParts,
        mut events: impl FnMut(PlayerEvent) + Send + 'static,
    ) -> Self {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::spawn(move || match make_output() {
            Ok(output) => {
                let sink: EventSink = Box::new(events);
                match Worker::start(output, chain, receiver, sink) {
                    Ok(mut worker) => worker.run(),
                    Err((mut sink, message)) => sink(PlayerEvent::Error { message }),
                }
            }
            Err(message) => events(PlayerEvent::Error { message }),
        });
        Self {
            commands,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: PlayerCommand) {
        let _ = self.commands.send(command);
    }

    /// Like `send`, but reports an engine that has ended.
    pub fn try_send(&self, command: PlayerCommand) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "the audio engine has stopped".to_owned())
    }

    /// Waits for the engine to end, which a Quit command causes.
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(PlayerCommand::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What is audible now. `played_base` is where its audio begins in the played frames.
struct Playing {
    base_ms: u64,
    played_base: u64,
    duration_ms: Option<u64>,
}

/// The track being decoded. It runs ahead of what is audible by up to a queue's length.
struct Feed {
    index: usize,
    decoder: Decoder,
    /// Whether its first audio has been queued, so its place in the output is known.
    announced: bool,
}

/// A queued track that becomes the audible one when playback reaches frame `at`.
struct Join {
    at: u64,
    index: usize,
    duration_ms: Option<u64>,
}

struct Worker {
    receiver: Receiver<PlayerCommand>,
    events: EventSink,
    feeds: Feeds,
    shared: Arc<Shared>,
    format: OutputFormat,
    capacity: usize,
    tracks: Vec<TrackEntry>,
    index: Option<usize>,
    playing: Option<Playing>,
    feed: Option<Feed>,
    upcoming: VecDeque<Join>,
    /// Where the track after the last one decoded starts in the played frames.
    join_at: Option<u64>,
    /// Created at the first decoded audio, once the file's format is certain. Tracks at
    /// the same rate share it, so the audio across a join is converted as one stream.
    converter: Option<RateConverter>,
    converter_rate: Option<u32>,
    /// Where the converter's first output frame falls in the played frames.
    converter_start: u64,
    /// Converted samples waiting for room in the queue.
    pending: Vec<f32>,
    sent: usize,
    state: PlaybackState,
    decoded: Vec<f32>,
    mapped: Vec<f32>,
    last_position: Instant,
    _stream: Box<dyn Any>,
}

impl Worker {
    fn start(
        output: Box<dyn AudioOutput>,
        chain: ChainParts,
        receiver: Receiver<PlayerCommand>,
        mut events: EventSink,
    ) -> Result<Self, (EventSink, String)> {
        let format = output.format();
        let channels = usize::from(format.channels);
        let capacity = format.sample_rate as usize * channels * QUEUE_SECONDS;
        let (feeds, renderer, shared) =
            render::channel(capacity, channels, chain.effects, chain.tap);
        let stream = match output.start(renderer) {
            Ok(stream) => stream,
            Err(message) => return Err((events, message)),
        };
        events(PlayerEvent::Ready {
            device: format.device.clone(),
            sample_rate: format.sample_rate,
            channels: format.channels,
        });
        Ok(Self {
            receiver,
            events,
            feeds,
            shared,
            format,
            capacity,
            tracks: Vec::new(),
            index: None,
            playing: None,
            feed: None,
            upcoming: VecDeque::new(),
            join_at: None,
            converter: None,
            converter_rate: None,
            converter_start: 0,
            pending: Vec::new(),
            sent: 0,
            state: PlaybackState::Stopped,
            decoded: Vec::new(),
            mapped: Vec::new(),
            last_position: Instant::now(),
            _stream: stream,
        })
    }

    fn run(&mut self) {
        loop {
            let wait = self.pump();
            match self.receiver.recv_timeout(wait) {
                Ok(PlayerCommand::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(command) => {
                    self.handle(command);
                    // Commands that arrived together apply together.
                    while let Ok(command) = self.receiver.try_recv() {
                        if command == PlayerCommand::Quit {
                            return;
                        }
                        self.handle(command);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if self.shared.take_output_failed() {
                (self.events)(PlayerEvent::Error {
                    message: "the audio output stopped working".to_owned(),
                });
            }
            self.report_position();
        }
    }

    fn handle(&mut self, command: PlayerCommand) {
        match command {
            PlayerCommand::Load {
                tracks,
                start_index,
                start_ms,
            } => {
                self.tracks = tracks;
                self.start_track(start_index, start_ms);
            }
            PlayerCommand::EditList { tracks, origin } => self.edit_list(tracks, &origin),
            PlayerCommand::Play => match (self.state, self.index) {
                (PlaybackState::Paused, _) => {
                    self.shared.set_paused(false);
                    self.set_state(PlaybackState::Playing);
                }
                (PlaybackState::Stopped, Some(index)) => self.start_track(index, 0),
                (PlaybackState::Stopped, None) if !self.tracks.is_empty() => {
                    self.start_track(0, 0);
                }
                _ => {}
            },
            PlayerCommand::Pause => {
                if self.state == PlaybackState::Playing {
                    self.shared.set_paused(true);
                    self.set_state(PlaybackState::Paused);
                }
            }
            PlayerCommand::Stop => {
                self.flush();
                self.clear_feeding();
                self.playing = None;
                self.shared.set_paused(false);
                self.set_state(PlaybackState::Stopped);
            }
            PlayerCommand::Next => {
                if let Some(index) = self.index {
                    self.start_track(index + 1, 0);
                }
            }
            PlayerCommand::Previous => {
                if self.position_ms() > RESTART_THRESHOLD_MS {
                    self.seek(0);
                } else if let Some(index) = self.index {
                    self.start_track(index.saturating_sub(1), 0);
                }
            }
            PlayerCommand::Seek { ms } => self.seek(ms),
            PlayerCommand::SetVolume { volume } => self.shared.set_volume(volume),
            PlayerCommand::SetGain {
                replay_gain,
                preamp_db,
            } => self.shared.set_gain(replay_gain, preamp_db),
            PlayerCommand::Quit => {}
        }
    }

    fn position_ms(&self) -> u64 {
        let Some(playing) = &self.playing else {
            return 0;
        };
        let frames = self
            .shared
            .played_frames()
            .saturating_sub(playing.played_base);
        playing.base_ms + frames * 1000 / u64::from(self.format.sample_rate)
    }

    fn duration_ms(&self) -> Option<u64> {
        self.playing
            .as_ref()
            .and_then(|playing| playing.duration_ms)
    }

    fn emit_state(&mut self) {
        let event = PlayerEvent::State {
            state: self.state,
            index: self.index,
            track_id: self
                .index
                .and_then(|index| self.tracks.get(index))
                .map(|track| track.id.clone()),
            position_ms: self.position_ms(),
            duration_ms: self.duration_ms(),
        };
        (self.events)(event);
    }

    fn set_state(&mut self, state: PlaybackState) {
        self.state = state;
        self.emit_state();
    }

    fn report_position(&mut self) {
        if self.state == PlaybackState::Playing && self.last_position.elapsed() >= POSITION_INTERVAL
        {
            self.last_position = Instant::now();
            let position_ms = self.position_ms();
            (self.events)(PlayerEvent::Position { position_ms });
            let channels = u64::from(self.format.channels);
            let queued = (self.capacity - self.feeds.samples.slots()) as u64 / channels;
            let underruns = self.shared.underruns();
            (self.events)(PlayerEvent::Health {
                underruns,
                queued_ms: queued * 1000 / u64::from(self.format.sample_rate),
                longest_gap_us: self.shared.take_longest_gap_us(),
            });
        }
    }

    /// Empties the queue. The audio thread does the discarding, because only it may
    /// read the queue, and it runs every few milliseconds even when paused.
    fn flush(&mut self) {
        let request = self.shared.request_flush();
        let started = Instant::now();
        while !self.shared.flush_done(request) && started.elapsed() < FLUSH_TIMEOUT {
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Forgets everything decoded ahead of what is audible.
    fn clear_feeding(&mut self) {
        self.feed = None;
        self.upcoming.clear();
        self.join_at = None;
        self.converter = None;
        self.converter_rate = None;
        self.pending.clear();
        self.sent = 0;
    }

    fn open(track: &TrackEntry, start_ms: u64) -> Result<Decoder, String> {
        let source: Box<dyn AudioSource> = Box::new(LocalFile(PathBuf::from(&track.path)));
        let mut decoder = Decoder::open(source.as_ref())?;
        if start_ms > 0 {
            decoder.seek(start_ms)?;
        }
        Ok(decoder)
    }

    /// Opens the track at `index`, or the first after it that opens, reporting the rest.
    fn open_from(&mut self, mut index: usize, mut start_ms: u64) -> Option<(usize, Decoder)> {
        while let Some(track) = self.tracks.get(index) {
            match Self::open(track, start_ms) {
                Ok(decoder) => return Some((index, decoder)),
                Err(message) => {
                    let id = track.id.clone();
                    (self.events)(PlayerEvent::TrackFailed { id, message });
                    index += 1;
                    start_ms = 0;
                }
            }
        }
        None
    }

    /// Plays the track at `index` right away, dropping what was queued, and skipping any
    /// that cannot be opened. Past the end of the list, playback stops.
    fn start_track(&mut self, index: usize, start_ms: u64) {
        self.flush();
        self.clear_feeding();
        self.playing = None;
        self.shared.set_paused(false);
        let played = self.shared.played_frames();
        self.converter_start = played;
        match self.open_from(index, start_ms) {
            Some((opened, decoder)) => {
                // A skipped track's start position does not carry over to the next.
                let base_ms = if opened == index { start_ms } else { 0 };
                self.playing = Some(Playing {
                    base_ms,
                    played_base: played,
                    duration_ms: decoder.duration_ms(),
                });
                self.feed = Some(Feed {
                    index: opened,
                    decoder,
                    announced: true,
                });
                self.queue_gain(played, opened);
                self.index = Some(opened);
                self.set_state(PlaybackState::Playing);
            }
            None => {
                self.index = None;
                self.set_state(PlaybackState::Stopped);
            }
        }
    }

    /// Tells the audio thread a track's levels, for when its audio starts.
    fn queue_gain(&mut self, at: u64, index: usize) {
        if let Some(track) = self.tracks.get(index) {
            let _ = self.feeds.gains.push(GainChange::new(at, &track.gain));
        }
    }

    /// Starts a track over from a position, staying paused if playback was paused.
    fn restart_track(&mut self, index: usize, ms: u64) {
        let paused = self.state == PlaybackState::Paused;
        self.start_track(index, ms);
        if paused && self.state == PlaybackState::Playing {
            self.shared.set_paused(true);
            self.set_state(PlaybackState::Paused);
        }
    }

    /// Replaces the list. Audio keeps flowing unless the edit changes what is already
    /// queued, which can only be discarded along with the current track's queued audio,
    /// so then the current track resumes from where it is.
    fn edit_list(&mut self, tracks: Vec<TrackEntry>, origin: &[Option<usize>]) {
        let mut moved_to = vec![None; self.tracks.len()];
        for (new, old) in origin.iter().enumerate() {
            if let Some(slot) = old.and_then(|old| moved_to.get_mut(old)) {
                *slot = Some(new);
            }
        }
        let Some(old) = self.index else {
            self.tracks = tracks;
            return;
        };

        let Some(current) = moved_to.get(old).copied().flatten() else {
            // The current track is gone, so the one that followed it takes over.
            let following = moved_to.iter().skip(old + 1).find_map(|new| *new);
            self.tracks = tracks;
            if self.state == PlaybackState::Stopped {
                self.index = following;
                self.emit_state();
            } else {
                self.restart_track(following.unwrap_or(usize::MAX), 0);
            }
            return;
        };

        let mut ahead: Vec<usize> = self.upcoming.iter().map(|join| join.index).collect();
        if let Some(feed) = &self.feed
            && feed.index != old
            && ahead.last() != Some(&feed.index)
        {
            ahead.push(feed.index);
        }
        let unchanged = ahead
            .iter()
            .enumerate()
            .all(|(offset, was)| moved_to[*was] == Some(current + 1 + offset));
        self.tracks = tracks;
        self.index = Some(current);
        if !unchanged {
            let position = self.position_ms();
            self.restart_track(current, position);
            return;
        }

        for join in &mut self.upcoming {
            join.index = moved_to[join.index].unwrap_or(join.index);
        }
        if let Some(feed) = &mut self.feed {
            feed.index = moved_to[feed.index].unwrap_or(feed.index);
        } else if self.playing.is_some() {
            // Decoding had reached the end of the list, and there may be more now.
            if let Some((index, decoder)) = self.open_from(current + ahead.len() + 1, 0) {
                self.feed = Some(Feed {
                    index,
                    decoder,
                    announced: false,
                });
                self.converter_start = self.join_at.unwrap_or(self.converter_start);
            }
        }
        self.emit_state();
    }

    fn seek(&mut self, ms: u64) {
        let (Some(index), Some(_)) = (self.index, &self.playing) else {
            self.flush();
            return;
        };
        let ms = match self.duration_ms() {
            Some(duration) => ms.min(duration),
            None => ms,
        };
        let still_decoding =
            self.upcoming.is_empty() && self.feed.as_ref().is_some_and(|feed| feed.index == index);
        if !still_decoding {
            // Its decoder is gone: the next track is already being decoded.
            self.restart_track(index, ms);
            return;
        }

        self.flush();
        let played = self.shared.played_frames();
        self.pending.clear();
        self.sent = 0;
        self.join_at = None;
        self.converter_start = played;
        if let Some(converter) = &mut self.converter {
            converter.reset();
        }
        if let Some(playing) = &mut self.playing {
            playing.base_ms = ms;
            playing.played_base = played;
        }
        let Some(feed) = &mut self.feed else {
            return;
        };
        match feed.decoder.seek(ms) {
            Ok(()) => self.emit_state(),
            Err(message) => {
                // A seek past what the file holds ends the track, as reaching there would.
                (self.events)(PlayerEvent::Error { message });
                self.end_feed();
            }
        }
    }

    /// Makes queued tracks audible once playback has reached them.
    fn advance_tracks(&mut self) {
        let played = self.shared.played_frames();
        while let Some(join) = self.upcoming.front() {
            if played < join.at {
                return;
            }
            self.index = Some(join.index);
            self.playing = Some(Playing {
                base_ms: 0,
                played_base: join.at,
                duration_ms: join.duration_ms,
            });
            self.upcoming.pop_front();
            self.emit_state();
        }
    }

    /// Does one unit of decoding work and says how long to wait for a command before
    /// the next one.
    fn pump(&mut self) -> Duration {
        self.advance_tracks();
        let channels = usize::from(self.format.channels);
        if self.sent < self.pending.len() {
            let room = self.feeds.samples.slots();
            let left = self.pending.len() - self.sent;
            let count = room.min(left);
            let count = count - count % channels;
            let (pushed, _) = self
                .feeds
                .samples
                .push_partial_slice(&self.pending[self.sent..self.sent + count]);
            let _ = pushed;
            self.sent += count;
            if self.sent < self.pending.len() {
                return FULL_QUEUE_WAIT;
            }
            self.pending.clear();
            self.sent = 0;
        }

        if self.feed.is_some() {
            return self.decode_next();
        }
        if self.playing.is_none() {
            return IDLE_WAIT;
        }
        // Nothing is left to decode, so the list ends when the queue runs out.
        if self.feeds.samples.slots() == self.capacity {
            self.advance_tracks();
            self.playing = None;
            self.index = None;
            self.set_state(PlaybackState::Stopped);
            return Duration::ZERO;
        }
        FULL_QUEUE_WAIT
    }

    fn decode_next(&mut self) -> Duration {
        let Some(feed) = &mut self.feed else {
            return IDLE_WAIT;
        };
        self.decoded.clear();
        let more = match feed.decoder.next_chunk(&mut self.decoded) {
            Ok(more) => more,
            Err(message) => {
                self.fail_feed(message);
                return Duration::ZERO;
            }
        };
        let rate = feed.decoder.sample_rate();
        let source_channels = feed.decoder.channels();
        if !self.decoded.is_empty()
            && let Err(message) = self.queue_decoded(rate, source_channels)
        {
            self.fail_feed(message);
            return Duration::ZERO;
        }
        if !more {
            self.end_feed();
        }
        Duration::ZERO
    }

    /// Converts the decoded audio into `pending`. A track at another rate first drains
    /// the converter, which ends the previous track at its exact length.
    fn queue_decoded(&mut self, rate: u32, source_channels: usize) -> Result<(), String> {
        let channels = usize::from(self.format.channels);
        if self.converter_rate != Some(rate) {
            if let Some(old) = &mut self.converter {
                old.finish(&mut self.pending)?;
                self.converter_start += old.expected_output_frames();
            }
            self.converter = Some(RateConverter::new(rate, self.format.sample_rate, channels)?);
            self.converter_rate = Some(rate);
        }
        if let Some(feed) = &mut self.feed
            && !feed.announced
        {
            feed.announced = true;
            if let Some(at) = self.join_at {
                self.upcoming.push_back(Join {
                    at,
                    index: feed.index,
                    duration_ms: feed.decoder.duration_ms(),
                });
                let index = feed.index;
                self.queue_gain(at, index);
            }
        }
        self.mapped.clear();
        map_channels(
            &self.decoded,
            source_channels.max(1),
            channels,
            &mut self.mapped,
        );
        match &mut self.converter {
            Some(converter) => converter.process(&self.mapped, &mut self.pending),
            None => Ok(()),
        }
    }

    /// Moves decoding to the next track, whose audio goes straight after this one's.
    /// After the last track, the converter gives up the audio it holds.
    fn end_feed(&mut self) {
        let Some(feed) = self.feed.take() else {
            return;
        };
        if let Some(converter) = &self.converter {
            self.join_at = Some(self.converter_start + converter.expected_output_frames());
        } else if self.join_at.is_none() {
            self.join_at = Some(self.converter_start);
        }
        if let Some((index, decoder)) = self.open_from(feed.index + 1, 0) {
            self.feed = Some(Feed {
                index,
                decoder,
                announced: false,
            });
            return;
        }
        if let Some(mut converter) = self.converter.take()
            && let Err(message) = converter.finish(&mut self.pending)
        {
            (self.events)(PlayerEvent::Error { message });
        }
        self.converter_rate = None;
    }

    fn fail_feed(&mut self, message: String) {
        let track = self
            .feed
            .as_ref()
            .and_then(|feed| self.tracks.get(feed.index));
        if let Some(track) = track {
            let id = track.id.clone();
            (self.events)(PlayerEvent::TrackFailed { id, message });
        }
        self.end_feed();
    }
}

/// What a full decode of a file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeCheck {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: u64,
    pub output_frames: u64,
    pub decode_errors: u64,
    pub duration_ms: Option<u64>,
}

/// Decodes a whole file through the resampler, as playback would, without playing it.
pub fn check_file(path: &str, output_rate: u32) -> Result<DecodeCheck, String> {
    let source = LocalFile(PathBuf::from(path));
    let mut decoder = Decoder::open(&source)?;
    let duration_ms = decoder.duration_ms();
    let mut converter = None;
    let (mut frames, mut output_frames) = (0_u64, 0_u64);
    let (mut decoded, mut converted) = (Vec::new(), Vec::new());
    loop {
        decoded.clear();
        converted.clear();
        let more = decoder.next_chunk(&mut decoded)?;
        let channels = decoder.channels().max(1);
        frames += (decoded.len() / channels) as u64;
        if !decoded.is_empty() && converter.is_none() {
            converter = Some(RateConverter::new(
                decoder.sample_rate(),
                output_rate,
                channels,
            )?);
        }
        if let Some(converter) = &mut converter {
            converter.process(&decoded, &mut converted)?;
            if !more {
                converter.finish(&mut converted)?;
            }
        }
        output_frames += (converted.len() / channels) as u64;
        if !more {
            break;
        }
    }
    Ok(DecodeCheck {
        sample_rate: decoder.sample_rate(),
        channels: decoder.channels(),
        frames,
        output_frames,
        decode_errors: decoder.decode_errors(),
        duration_ms,
    })
}
