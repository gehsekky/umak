//! Holds the audio thread to its rule: rendering must never allocate. This counts every
//! allocation in the test process, so it lives alone in its own test binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use umak_audio::effects::{Effect, EffectChain, OutputTap};
use umak_audio::protocol::{ReplayGainMode, TrackGain};
use umak_audio::render::{self, GainChange};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
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

struct Peak(f32);

impl OutputTap for Peak {
    fn on_output(&mut self, samples: &[f32], _channels: usize) {
        for sample in samples {
            self.0 = self.0.max(sample.abs());
        }
    }
}

#[test]
fn rendering_never_allocates_in_any_state() {
    let chain = EffectChain::new(vec![Box::new(Halve)]);
    let (mut feeds, mut renderer, shared) =
        render::channel(48_000, 2, chain, Some(Box::new(Peak(0.0))));
    let gain = TrackGain {
        track_db: Some(-6.0),
        track_peak: Some(0.9),
        ..TrackGain::default()
    };
    shared.set_gain(ReplayGainMode::Track, 3.0);
    let mut output = vec![0.0_f32; 960];
    let block = vec![0.5_f32; 4800];

    let before = ALLOCATIONS.load(Ordering::SeqCst);
    let mut count = 0;
    for pass in 0..200 {
        match pass % 7 {
            0 => shared.set_paused(true),
            1 => shared.set_paused(false),
            2 => {
                shared.set_volume(if pass % 2 == 0 { 0.25 } else { 0.75 });
                shared.set_gain(ReplayGainMode::Album, 1.0);
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
    assert_eq!(count, 200);
    assert_eq!(after - before, 0, "the audio path allocated");
}
