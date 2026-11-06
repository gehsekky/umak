//! The engine thread: holds the player playlist, decodes the current track and keeps
//! the sample queue full. Commands arrive on a channel and events go to a callback.
//! The playlist, decoder and output are separate stages, so gapless playback can later
//! decode the next track early without changing the others.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rtrb::Producer;

use crate::decoder::Decoder;
use crate::effects::{EffectChain, OutputTap};
use crate::output::{AudioOutput, OutputFormat};
use crate::protocol::{PlaybackState, PlayerCommand, PlayerEvent, TrackEntry};
use crate::render::{self, Shared};
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

struct Current {
    decoder: Decoder,
    /// Created at the first decoded audio, once the file's format is certain.
    converter: Option<RateConverter>,
    /// Converted samples waiting for room in the queue.
    pending: Vec<f32>,
    sent: usize,
    finished: bool,
    /// The position when the frames counted from `played_base` began.
    base_ms: u64,
    played_base: u64,
}

struct Worker {
    receiver: Receiver<PlayerCommand>,
    events: EventSink,
    producer: Producer<f32>,
    shared: Arc<Shared>,
    format: OutputFormat,
    capacity: usize,
    tracks: Vec<TrackEntry>,
    index: Option<usize>,
    current: Option<Current>,
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
        let (producer, renderer, shared) =
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
            producer,
            shared,
            format,
            capacity,
            tracks: Vec::new(),
            index: None,
            current: None,
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
                self.current = None;
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
            PlayerCommand::Quit => {}
        }
    }

    fn position_ms(&self) -> u64 {
        let Some(current) = &self.current else {
            return 0;
        };
        let frames = self
            .shared
            .played_frames()
            .saturating_sub(current.played_base);
        current.base_ms + frames * 1000 / u64::from(self.format.sample_rate)
    }

    fn duration_ms(&self) -> Option<u64> {
        self.current
            .as_ref()
            .and_then(|current| current.decoder.duration_ms())
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
            let queued = (self.capacity - self.producer.slots()) as u64 / channels;
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

    /// Starts the track at `index`, skipping any that cannot be opened. Past the end of
    /// the list, playback stops.
    fn start_track(&mut self, mut index: usize, start_ms: u64) {
        self.flush();
        self.current = None;
        self.shared.set_paused(false);
        let mut start_ms = start_ms;
        while let Some(track) = self.tracks.get(index) {
            let source: Box<dyn AudioSource> = Box::new(LocalFile(PathBuf::from(&track.path)));
            match self.open(source.as_ref(), start_ms) {
                Ok(current) => {
                    self.current = Some(current);
                    self.index = Some(index);
                    self.set_state(PlaybackState::Playing);
                    return;
                }
                Err(message) => {
                    let id = track.id.clone();
                    (self.events)(PlayerEvent::TrackFailed { id, message });
                    index += 1;
                    start_ms = 0;
                }
            }
        }
        self.index = None;
        self.set_state(PlaybackState::Stopped);
    }

    fn open(&self, source: &dyn AudioSource, start_ms: u64) -> Result<Current, String> {
        let mut decoder = Decoder::open(source)?;
        if start_ms > 0 {
            decoder.seek(start_ms)?;
        }
        Ok(Current {
            decoder,
            converter: None,
            pending: Vec::new(),
            sent: 0,
            finished: false,
            base_ms: start_ms,
            played_base: self.shared.played_frames(),
        })
    }

    fn seek(&mut self, ms: u64) {
        let ms = match self.duration_ms() {
            Some(duration) => ms.min(duration),
            None => ms,
        };
        self.flush();
        let played = self.shared.played_frames();
        let Some(current) = &mut self.current else {
            return;
        };
        match current.decoder.seek(ms) {
            Ok(()) => {
                if let Some(converter) = &mut current.converter {
                    converter.reset();
                }
                current.pending.clear();
                current.sent = 0;
                current.finished = false;
                current.base_ms = ms;
                current.played_base = played;
                self.emit_state();
            }
            Err(message) => {
                // A seek past what the file holds ends the track, as reaching there would.
                current.pending.clear();
                current.sent = 0;
                current.finished = true;
                (self.events)(PlayerEvent::Error { message });
            }
        }
    }

    /// Does one unit of decoding work and says how long to wait for a command before
    /// the next one.
    fn pump(&mut self) -> Duration {
        let channels = usize::from(self.format.channels);
        let Some(current) = &mut self.current else {
            return IDLE_WAIT;
        };

        if current.sent < current.pending.len() {
            let room = self.producer.slots();
            let left = current.pending.len() - current.sent;
            let count = room.min(left);
            let count = count - count % channels;
            let (pushed, _) = self
                .producer
                .push_partial_slice(&current.pending[current.sent..current.sent + count]);
            let _ = pushed;
            current.sent += count;
            if current.sent < current.pending.len() {
                return FULL_QUEUE_WAIT;
            }
            current.pending.clear();
            current.sent = 0;
        }

        if current.finished {
            if self.producer.slots() == self.capacity {
                let next = self.index.map_or(0, |index| index + 1);
                self.start_track(next, 0);
                return Duration::ZERO;
            }
            return FULL_QUEUE_WAIT;
        }

        self.decoded.clear();
        match current.decoder.next_chunk(&mut self.decoded) {
            Ok(more) => {
                let rate = current.decoder.sample_rate();
                let source_channels = current.decoder.channels();
                if current.converter.is_none() && !self.decoded.is_empty() {
                    match RateConverter::new(rate, self.format.sample_rate, channels) {
                        Ok(converter) => current.converter = Some(converter),
                        Err(message) => return self.fail_current(message),
                    }
                }
                self.mapped.clear();
                map_channels(
                    &self.decoded,
                    source_channels.max(1),
                    channels,
                    &mut self.mapped,
                );
                if let Some(converter) = &mut current.converter {
                    let mut result = converter.process(&self.mapped, &mut current.pending);
                    if !more && result.is_ok() {
                        result = converter.finish(&mut current.pending);
                    }
                    if let Err(message) = result {
                        return self.fail_current(message);
                    }
                }
                if !more {
                    current.finished = true;
                }
                Duration::ZERO
            }
            Err(message) => self.fail_current(message),
        }
    }

    fn fail_current(&mut self, message: String) -> Duration {
        if let Some(track) = self.index.and_then(|index| self.tracks.get(index)) {
            let id = track.id.clone();
            (self.events)(PlayerEvent::TrackFailed { id, message });
        }
        if let Some(current) = &mut self.current {
            current.pending.clear();
            current.sent = 0;
            current.finished = true;
        }
        Duration::ZERO
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
