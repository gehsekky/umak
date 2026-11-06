//! The signal chain's two seams: effects that change samples, and a tap that reads
//! what goes to the output. Both run on the audio thread, so implementations must not
//! block, allocate or take locks. The chain holds what comes between the equalizer and
//! the limiter, which are fixed stages of their own: it is the place for plugins.

pub trait Effect: Send {
    fn process(&mut self, samples: &mut [f32], channels: usize);
}

/// Sees the final samples, for visualizers.
pub trait OutputTap: Send {
    fn on_output(&mut self, samples: &[f32], channels: usize);
}

/// Effects in a fixed order, applied once each.
#[derive(Default)]
pub struct EffectChain {
    effects: Vec<Box<dyn Effect>>,
}

impl EffectChain {
    pub fn new(effects: Vec<Box<dyn Effect>>) -> Self {
        Self { effects }
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub fn process(&mut self, samples: &mut [f32], channels: usize) {
        for effect in &mut self.effects {
            effect.process(samples, channels);
        }
    }
}
