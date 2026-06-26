//! `ModEngine` — the global modulation **sources** (M6): two LFOs + a
//! mod-envelope. Reuses synth_core's `Lfo` + `Adsr` verbatim; instrument-glue
//! only. The plugin owns ONE of these, advances it once per block on the host
//! thread (LFO rates resolved tempo→Hz at the plugin), and pushes the latched
//! values into the kit via `DrumKit::set_mod_globals`. The kit's matrix then
//! reads them as the `Lfo1`/`Lfo2`/`ModEnv` sources, sampled per hit.
//!
//! It lives here (not the plugin) so it's unit-testable, but the golden render
//! constructs only `Sequencer` + `DrumKit` — never a `ModEngine` — so the
//! goldens are unaffected by its existence.

use synth_core::{Adsr, Lfo, LfoShape};

pub use synth_core::LfoShape as ModLfoShape;

/// Two LFOs + a per-hit-sampled mod-envelope. Default LFOs free-run (2 Hz sine,
/// 5 Hz triangle); the mod-env is an attack/decay shape (sustain 0) fired on the
/// transport play-start edge.
pub struct ModEngine {
    lfo1: Lfo,
    lfo2: Lfo,
    mod_env: Adsr,
    lfo1_val: f32,
    lfo2_val: f32,
    mod_env_val: f32,
}

impl ModEngine {
    pub fn new(sr: f32) -> Self {
        let mut lfo1 = Lfo::new(sr);
        lfo1.set_params(LfoShape::Sine, 2.0, 1.0, 0.0, false); // free-run
        let mut lfo2 = Lfo::new(sr);
        lfo2.set_params(LfoShape::Triangle, 5.0, 1.0, 0.0, false);
        let mut mod_env = Adsr::new(sr);
        mod_env.attack_secs = 0.005;
        mod_env.decay_secs = 0.5;
        mod_env.sustain = 0.0; // AR shape: rise then fall to 0
        mod_env.release_secs = 0.0;
        Self { lfo1, lfo2, mod_env, lfo1_val: 0.0, lfo2_val: 0.0, mod_env_val: 0.0 }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.lfo1.set_sample_rate(sr);
        self.lfo2.set_sample_rate(sr);
        self.mod_env.set_sample_rate(sr);
    }

    /// Configure LFO 1 (`rate_hz` already tempo-resolved by the plugin).
    pub fn set_lfo1(&mut self, shape: ModLfoShape, rate_hz: f32, depth: f32, retrigger: bool) {
        self.lfo1.set_params(shape, rate_hz, depth, 0.0, retrigger);
    }

    /// Configure LFO 2.
    pub fn set_lfo2(&mut self, shape: ModLfoShape, rate_hz: f32, depth: f32, retrigger: bool) {
        self.lfo2.set_params(shape, rate_hz, depth, 0.0, retrigger);
    }

    /// Configure the mod-envelope (attack + decay seconds; sustain stays 0).
    pub fn set_mod_env(&mut self, attack_secs: f32, decay_secs: f32) {
        self.mod_env.attack_secs = attack_secs.max(0.0);
        self.mod_env.decay_secs = decay_secs.max(0.0);
        self.mod_env.sustain = 0.0;
    }

    /// On the transport play-start edge: reset retrigger-mode LFOs to phase 0 and
    /// fire the mod-envelope. Free-run LFOs keep their phase.
    pub fn retrigger(&mut self) {
        if self.lfo1.retriggers() {
            self.lfo1.retrigger();
        }
        if self.lfo2.retriggers() {
            self.lfo2.retrigger();
        }
        self.mod_env.trigger();
    }

    /// Advance the sources by `block_len` samples (host thread, once per block)
    /// and latch each current value. Cheap: a handful of muls per sample × 3
    /// sources, alloc-free, off the audio path.
    pub fn advance(&mut self, block_len: usize) {
        for _ in 0..block_len {
            self.lfo1_val = self.lfo1.next(1.0);
            self.lfo2_val = self.lfo2.next(1.0);
            self.mod_env_val = self.mod_env.next();
        }
    }

    pub fn lfo1(&self) -> f32 {
        self.lfo1_val
    }
    pub fn lfo2(&self) -> f32 {
        self.lfo2_val
    }
    pub fn mod_env(&self) -> f32 {
        self.mod_env_val
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfos_advance_and_stay_bounded() {
        let mut e = ModEngine::new(48_000.0);
        // A sine LFO over a few blocks must move and stay in [-1, 1].
        let mut seen = Vec::new();
        for _ in 0..20 {
            e.advance(256);
            assert!(e.lfo1().abs() <= 1.000_01, "lfo1 stays bounded");
            assert!(e.lfo2().abs() <= 1.000_01, "lfo2 stays bounded");
            seen.push(e.lfo1());
        }
        // It actually moves (not stuck).
        let first = seen[0];
        assert!(seen.iter().any(|&v| (v - first).abs() > 0.1), "lfo1 must sweep");
    }

    #[test]
    fn mod_env_fires_then_decays_to_zero() {
        let mut e = ModEngine::new(48_000.0);
        e.set_mod_env(0.001, 0.05);
        e.retrigger();
        e.advance(64); // just past the 1 ms attack
        assert!(e.mod_env() > 0.5, "mod-env rises after trigger");
        e.advance(48_000); // well past the 50 ms decay
        assert!(e.mod_env() < 0.01, "mod-env decays to ~0 (sustain 0)");
    }
}
