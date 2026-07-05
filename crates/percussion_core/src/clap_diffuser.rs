//! `ClapDiffuser` — the 808 stacked-burst clap (design §3.4): three rapid noise
//! bursts ~10 ms apart plus a longer diffuse tail. Stacking the bursts is what
//! makes one "clap" sound like several hands at once. A band-pass downstream
//! gives the ~1 kHz clap timbre; running two diffusers with slightly different
//! burst timing gives the wide stereo "splatter."
//!
//! Mono output, `Copy`, allocation-free; the recursive burst/tail envelopes are
//! `flush_denormal`d.

use synth_core::{flush_denormal, Noise, NoiseType};

/// Number of stacked attack bursts.
const N_BURSTS: usize = 3;

#[derive(Clone, Debug)]
pub struct ClapDiffuser {
    sr: f32,
    noise: Noise,
    t: u32,
    /// Sample offsets at which each burst re-kicks the fast envelope.
    burst_at: [u32; N_BURSTS],
    burst_coef: f32,
    tail_at: u32,
    tail_coef: f32,
    /// The configured (un-choked) coefficients. `choke()` swaps the live coefs
    /// for a fast fade; `trigger()` restores from here (identical bits when
    /// never choked — a no-op).
    burst_coef_nat: f32,
    tail_coef_nat: f32,
    env: f32,
    tail_env: f32,
    tail_level: f32,
    active: bool,
    /// Per-channel burst-timing offset (ms); stored so it survives a
    /// sample-rate change (it drives the stereo spread).
    spread_ms: f32,
}

impl ClapDiffuser {
    /// `seed` decorrelates the L/R noise streams; `spread_ms` nudges this
    /// channel's burst timing for stereo width.
    pub fn new(sr: f32, seed: u32, spread_ms: f32) -> Self {
        let mut c = Self {
            sr: sr.max(1.0),
            noise: Noise::new(seed),
            t: 0,
            burst_at: [0; N_BURSTS],
            burst_coef: 0.99,
            tail_at: 0,
            tail_coef: 0.999,
            burst_coef_nat: 0.99,
            tail_coef_nat: 0.999,
            env: 0.0,
            tail_env: 0.0,
            tail_level: 0.55,
            active: false,
            spread_ms,
        };
        c.configure();
        c
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr.max(1.0);
        self.configure();
        // The burst offsets are re-derived in new sample units; rebase the
        // running counter so an in-flight one-shot can't mis-fire bursts.
        self.reset();
    }

    fn configure(&mut self) {
        let ms = |m: f32| (m * 0.001 * self.sr) as u32;
        // Clamp the spread once, up front, so bursts and tail shift together.
        let spread = (self.spread_ms * 0.001 * self.sr).max(0.0);
        // Bursts at 0, 10, 20 ms (+ per-channel spread offset).
        for (i, slot) in self.burst_at.iter_mut().enumerate() {
            *slot = ((i as f32 * 10.0 * 0.001 * self.sr) + spread) as u32;
        }
        self.tail_at = ms(30.0) + spread as u32;
        // ~6 ms burst decay, ~90 ms tail decay.
        let bd = (6.0 * 0.001 * self.sr).max(1.0);
        let td = (90.0 * 0.001 * self.sr).max(1.0);
        self.burst_coef = (0.001_f32).powf(1.0 / bd);
        self.tail_coef = (0.001_f32).powf(1.0 / td);
        self.burst_coef_nat = self.burst_coef;
        self.tail_coef_nat = self.tail_coef;
    }

    pub fn trigger(&mut self) {
        self.t = 0;
        self.env = 0.0;
        self.tail_env = 0.0;
        self.active = true;
        // Restore the configured decays: a prior choke() shortened only the
        // burst it interrupted, never this hit.
        self.burst_coef = self.burst_coef_nat;
        self.tail_coef = self.tail_coef_nat;
    }

    /// Choke-group fade: ~4 ms exponential ramp to silence instead of a hard
    /// cut (every other engine fades via `DahdEnv::choke`; zeroing the envs here
    /// would step full-scale noise to zero between two samples — a click).
    /// Jumping `t` past `tail_at` disarms the burst/tail re-kicks, and lets the
    /// deactivation check retire the voice once the fade lands.
    pub fn choke(&mut self) {
        if self.active {
            let d = (4.0 * 0.001 * self.sr).max(1.0);
            let fast = (0.001_f32).powf(1.0 / d);
            self.burst_coef = fast;
            self.tail_coef = fast;
            self.t = self.tail_at.saturating_add(1);
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.env = 0.0;
        self.tail_env = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn next(&mut self) -> f32 {
        if !self.active {
            return 0.0;
        }
        for &b in &self.burst_at {
            if self.t == b {
                self.env = 1.0;
            }
        }
        if self.t == self.tail_at {
            self.tail_env = 1.0;
        }
        let n = self.noise.next(NoiseType::White);
        let amp = self.env + self.tail_env * self.tail_level;
        let out = n * amp;

        self.env = flush_denormal(self.env * self.burst_coef);
        self.tail_env = flush_denormal(self.tail_env * self.tail_coef);
        self.t = self.t.saturating_add(1);
        if self.t > self.tail_at && self.tail_env < 1.0e-4 && self.env < 1.0e-4 {
            self.active = false;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_is_silent() {
        let mut c = ClapDiffuser::new(48_000.0, 1, 0.0);
        for _ in 0..16 {
            assert_eq!(c.next(), 0.0);
        }
    }

    #[test]
    fn finite_and_eventually_silent() {
        let mut c = ClapDiffuser::new(48_000.0, 7, 0.0);
        c.trigger();
        let mut n = 0;
        while c.is_active() {
            assert!(c.next().is_finite());
            n += 1;
            assert!(n < 48_000, "clap should decay to silence within 1s");
        }
        assert!(n > 1_000, "clap tail should last a meaningful time");
    }

    #[test]
    fn has_multiple_burst_peaks() {
        // Energy should re-spike at each burst offset, not decay once.
        let mut c = ClapDiffuser::new(48_000.0, 3, 0.0);
        c.trigger();
        let ms = |m: f32| (m * 0.001 * 48_000.0) as usize;
        let mut samples = vec![0.0_f32; ms(40.0)];
        for s in samples.iter_mut() {
            *s = c.next().abs();
        }
        // window-max around burst 1 (~10ms) should be comparable to burst 0.
        let win = |center: usize| {
            let lo = center.saturating_sub(40);
            let hi = (center + 40).min(samples.len());
            samples[lo..hi].iter().cloned().fold(0.0_f32, f32::max)
        };
        let b0 = win(ms(0.5));
        let b1 = win(ms(10.0));
        assert!(b0 > 0.05 && b1 > 0.05, "expected energy at bursts 0 and 1: {b0}, {b1}");
    }

    #[test]
    fn spread_survives_sample_rate_change() {
        // Regression: spread_ms must be stored, not lost on a host SR change,
        // or the clap collapses toward mono.
        let mut a = ClapDiffuser::new(48_000.0, 5, 0.0);
        let mut b = ClapDiffuser::new(48_000.0, 5, 3.0);
        a.set_sample_rate(96_000.0);
        b.set_sample_rate(96_000.0);
        a.trigger();
        b.trigger();
        let mut any_diff = false;
        for _ in 0..8_000 {
            if a.next().to_bits() != b.next().to_bits() {
                any_diff = true;
            }
        }
        assert!(any_diff, "spread must survive a sample-rate change");
    }

    #[test]
    fn spread_shifts_timing() {
        let mut a = ClapDiffuser::new(48_000.0, 5, 0.0);
        let mut b = ClapDiffuser::new(48_000.0, 5, 3.0);
        a.trigger();
        b.trigger();
        // With identical seed but different spread, the streams differ.
        let mut any_diff = false;
        for _ in 0..2_000 {
            if a.next().to_bits() != b.next().to_bits() {
                any_diff = true;
            }
        }
        assert!(any_diff, "spread offset should change the burst timing");
    }
}
