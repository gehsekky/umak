//! The real-time half of the engine. `Renderer::render` runs on the audio thread, so it
//! must not block, allocate or take locks: it talks to the engine thread only through
//! a lock-free ring buffer and atomics. `tests/realtime.rs` checks it never allocates.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::effects::{EffectChain, OutputTap};

/// State shared between the engine thread and the audio thread.
pub struct Shared {
    paused: AtomicBool,
    flush_request: AtomicU64,
    flush_ack: AtomicU64,
    played_frames: AtomicU64,
    underruns: AtomicU64,
    volume_bits: AtomicU32,
    output_failed: AtomicBool,
    longest_gap_us: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            flush_request: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            played_frames: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            volume_bits: AtomicU32::new(1.0_f32.to_bits()),
            output_failed: AtomicBool::new(false),
            longest_gap_us: AtomicU64::new(0),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume_bits
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn played_frames(&self) -> u64 {
        self.played_frames.load(Ordering::Acquire)
    }

    /// Times the output asked for samples while playing and had to fill in silence.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Asks the audio thread to discard everything queued. Returns the request number.
    pub fn request_flush(&self) -> u64 {
        self.flush_request.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn flush_done(&self, request: u64) -> bool {
        self.flush_ack.load(Ordering::Acquire) >= request
    }

    /// The longest wait between output callbacks since this was last asked. A long one
    /// means the device or the OS stalled, whatever the queue held.
    pub fn take_longest_gap_us(&self) -> u64 {
        self.longest_gap_us.swap(0, Ordering::Relaxed)
    }

    pub fn mark_output_failed(&self) {
        self.output_failed.store(true, Ordering::Release);
    }

    pub fn take_output_failed(&self) -> bool {
        self.output_failed.swap(false, Ordering::AcqRel)
    }
}

pub struct Renderer {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    chain: EffectChain,
    tap: Option<Box<dyn OutputTap>>,
    /// Whether samples have flowed since the last flush, so silence means an underrun.
    active: bool,
    last_call: Option<Instant>,
}

/// Creates the two ends of the sample queue. Its capacity is in samples.
pub fn channel(
    capacity: usize,
    channels: usize,
    chain: EffectChain,
    tap: Option<Box<dyn OutputTap>>,
) -> (Producer<f32>, Renderer, Arc<Shared>) {
    let (producer, consumer) = RingBuffer::new(capacity);
    let shared = Arc::new(Shared::new());
    let renderer = Renderer {
        consumer,
        shared: Arc::clone(&shared),
        channels,
        chain,
        tap,
        active: false,
        last_call: None,
    };
    (producer, renderer, shared)
}

impl Renderer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Fills an output buffer of interleaved samples. Called by the audio thread.
    pub fn render(&mut self, output: &mut [f32]) {
        let now = Instant::now();
        if let Some(last) = self.last_call {
            let gap = now.duration_since(last).as_micros() as u64;
            self.shared.longest_gap_us.fetch_max(gap, Ordering::Relaxed);
        }
        self.last_call = Some(now);

        let request = self.shared.flush_request.load(Ordering::Acquire);
        if request != self.shared.flush_ack.load(Ordering::Relaxed) {
            let queued = self.consumer.slots();
            if let Ok(chunk) = self.consumer.read_chunk(queued) {
                chunk.commit_all();
            }
            self.active = false;
            self.shared.flush_ack.store(request, Ordering::Release);
        }

        let mut written = 0;
        if !self.shared.paused.load(Ordering::Acquire) {
            // Whole frames only, so channels can never swap.
            let wanted = output.len().min(self.consumer.slots());
            let wanted = wanted - wanted % self.channels;
            let (filled, _) = self.consumer.pop_partial_slice(&mut output[..wanted]);
            written = filled.len();
            if written > 0 {
                self.shared
                    .played_frames
                    .fetch_add((written / self.channels) as u64, Ordering::AcqRel);
                self.active = true;
            }
            if written < output.len() && self.active {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                self.active = false;
            }
        }

        let (audio, silence) = output.split_at_mut(written);
        self.chain.process(audio, self.channels);
        let volume = f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed));
        if volume != 1.0 {
            for sample in audio.iter_mut() {
                *sample *= volume;
            }
        }
        silence.fill(0.0);
        if let Some(tap) = &mut self.tap {
            tap.on_output(output, self.channels);
        }
    }
}
