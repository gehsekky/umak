//! Holds the audio thread to its rule: rendering must never allocate. This replaces the
//! process's allocator to count allocations on the rendering thread, so it lives alone in
//! its own test binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use umak_audio::analysis;
use umak_audio::effects::{Effect, EffectChain};
use umak_audio::protocol::{ReplayGainMode, TrackGain};
use umak_audio::render::{self, GainChange};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Set on the thread that renders. Other threads, such as the visualizer's, may allocate.
    static RENDERING: Cell<bool> = const { Cell::new(false) };
}

fn note_allocation() {
    if RENDERING.with(Cell::get) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        note_allocation();
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        note_allocation();
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

struct Halve;

impl Effect for Halve {
    fn process(&mut self, samples: &mut [f32], _channels: usize) {
        for sample in samples {
            *sample *= 0.5;
        }
    }
}

#[test]
fn rendering_never_allocates_in_any_state() {
    let chain = EffectChain::new(vec![Box::new(Halve)]);
    let (tap, visualizer) = analysis::visualizer(|_| {});
    visualizer.set_enabled(true);
    let (mut feeds, mut renderer, shared) =
        render::channel(48_000, 48_000, 2, chain, Some(Box::new(tap)));
    let gain = TrackGain {
        track_db: Some(-6.0),
        track_peak: Some(0.9),
        ..TrackGain::default()
    };
    shared.set_gain(ReplayGainMode::Track, 3.0);
    let mut output = vec![0.0_f32; 960];
    let block = vec![0.5_f32; 4800];

    RENDERING.with(|rendering| rendering.set(true));
    let before = ALLOCATIONS.load(Ordering::SeqCst);
    let mut count = 0;
    for pass in 0..200 {
        match pass % 7 {
            0 => shared.set_paused(true),
            1 => shared.set_paused(false),
            2 => {
                shared.set_volume(if pass % 2 == 0 { 0.25 } else { 0.75 });
                // Loud enough that the limiter works, while the equalizer moves.
                shared.set_gain(ReplayGainMode::Album, 12.0);
                shared.set_eq(pass % 2 == 0, &[pass as f32 % 12.0; 10]);
            }
            3 => {
                shared.request_flush();
            }
            _ => {}
        }
        if pass % 3 == 0 {
            let _ = feeds.samples.push_partial_slice(&block);
            let _ = feeds
                .gains
                .push(GainChange::new(shared.played_frames() + 100, &gain));
        }
        renderer.render(&mut output);
        count += 1;
    }
    let after = ALLOCATIONS.load(Ordering::SeqCst);
    RENDERING.with(|rendering| rendering.set(false));
    assert_eq!(count, 200);
    assert_eq!(after - before, 0, "the audio path allocated");
}
