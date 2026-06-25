//! `DahdEnv` — a Decay-Attack-Hold-Decay **exponential** envelope.
//!
//! The single most important primitive Esker lacks (design §3.4). Real
//! percussion needs an exponential contour with a near-instant attack, not the
//! linear `Adsr`. One envelope shape serves two jobs:
//!   - **pitch sweep:** output scaled by a semitone amount drives the kick/tom
//!     "boom→thud" frequency drop;
//!   - **amp decay:** output multiplies the voice for the body's amplitude tail.
//!
//! Output is a normalized `0.0..=1.0` contour. It is `Copy`, allocation-free,
//! and `flush_denormal`s its recursive decay state.

use synth_core::flush_denormal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Hold,
    Decay,
}

#[derive(Clone, Copy, Debug)]
pub struct DahdEnv {
    sr: f32,
    stage: Stage,
    level: f32,
    attack_inc: f32,
    hold_len: u32,
    hold_count: u32,
    decay_coef: f32,
    /// Below this the decay is snapped to silence and the stage goes Idle.
    floor: f32,
}

impl DahdEnv {
    pub fn new(sr: f32) -> Self {
        let mut e = Self {
            sr,
            stage: Stage::Idle,
            level: 0.0,
            attack_inc: 1.0,
            hold_len: 0,
            hold_count: 0,
            decay_coef: 0.999,
            floor: 1.0e-4,
        };
        e.set_params(0.5, 0.0, 300.0);
        e
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr.max(1.0);
    }

    /// `attack_ms` ramp 0→1, `hold_ms` at 1, then exponential decay reaching
    /// ~ -60 dB after `decay_ms`. An attack under one sample makes the env start
    /// at full level instantly (the percussion default for pitch sweeps).
    pub fn set_params(&mut self, attack_ms: f32, hold_ms: f32, decay_ms: f32) {
        let a = (attack_ms * 0.001 * self.sr).max(0.0);
        self.attack_inc = if a < 1.0 { 1.0 } else { 1.0 / a };
        self.hold_len = (hold_ms * 0.001 * self.sr).max(0.0) as u32;
        let d = (decay_ms * 0.001 * self.sr).max(1.0);
        // level *= coef each sample; coef^d == 0.001 (-60 dB).
        self.decay_coef = (0.001_f32).powf(1.0 / d);
    }

    /// Start the envelope from the top of its attack.
    pub fn trigger(&mut self) {
        if self.attack_inc >= 1.0 {
            self.stage = Stage::Hold;
            self.level = 1.0;
        } else {
            self.stage = Stage::Attack;
            self.level = 0.0;
        }
        self.hold_count = 0;
    }

    /// Fast-release override (choke groups): jump to decay with a short time.
    pub fn choke(&mut self, release_ms: f32) {
        let d = (release_ms * 0.001 * self.sr).max(1.0);
        self.decay_coef = (0.001_f32).powf(1.0 / d);
        if self.stage != Stage::Idle {
            self.stage = Stage::Decay;
        }
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
        self.hold_count = 0;
    }

    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => 0.0,
            Stage::Attack => {
                let v = self.level;
                self.level += self.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Hold;
                    self.hold_count = 0;
                }
                v
            }
            Stage::Hold => {
                if self.hold_count >= self.hold_len {
                    self.stage = Stage::Decay;
                }
                self.hold_count = self.hold_count.saturating_add(1);
                1.0
            }
            Stage::Decay => {
                let v = self.level;
                self.level = flush_denormal(self.level * self.decay_coef);
                if self.level < self.floor {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
                v
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_is_silent() {
        let mut e = DahdEnv::new(48_000.0);
        for _ in 0..16 {
            assert_eq!(e.next(), 0.0);
        }
        assert!(!e.is_active());
    }

    #[test]
    fn instant_attack_starts_at_full() {
        let mut e = DahdEnv::new(48_000.0);
        e.set_params(0.0, 1.0, 100.0);
        e.trigger();
        assert_eq!(e.next(), 1.0);
        assert!(e.is_active());
    }

    #[test]
    fn decays_monotonically_to_idle() {
        let mut e = DahdEnv::new(48_000.0);
        e.set_params(0.0, 0.0, 20.0);
        e.trigger();
        let mut prev = f32::INFINITY;
        let mut samples = 0;
        while e.is_active() {
            let v = e.next();
            assert!(v.is_finite());
            assert!(v <= prev + 1e-6, "envelope must not rise during decay");
            prev = v;
            samples += 1;
            assert!(samples < 48_000, "20ms decay should reach idle well under 1s");
        }
        assert!(samples > 100, "20ms decay at 48k should take hundreds of samples");
    }

    #[test]
    fn choke_shortens_the_tail() {
        let make = || {
            let mut e = DahdEnv::new(48_000.0);
            e.set_params(0.0, 0.0, 500.0);
            e.trigger();
            e
        };
        let mut natural = make();
        let mut choked = make();
        // let both run a little, then choke one
        for _ in 0..64 {
            natural.next();
            choked.next();
        }
        choked.choke(5.0);
        let mut n_count = 0;
        while natural.is_active() {
            natural.next();
            n_count += 1;
            if n_count > 48_000 {
                break;
            }
        }
        let mut c_count = 0;
        while choked.is_active() {
            choked.next();
            c_count += 1;
        }
        assert!(c_count < n_count, "choked tail must end before the natural one");
    }
}
