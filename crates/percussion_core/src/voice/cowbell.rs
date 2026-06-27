//! COWBELL / PERC — the 808 recipe: two square oscillators at a fixed inharmonic
//! ratio into a high-pass with a medium decay (design §3.6). Two of the metal
//! cluster's six oscillators, essentially — the famous 808 cowbell.

use crate::pitch_env::DahdEnv;
use synth_core::Filter;

/// Fold a phase increment to a safe value: NaN/inf → `0.0` (a stuck-DC guard),
/// else clamped sub-Nyquist. Finite inputs are just the clamp (golden-safe);
/// this only adds the non-finite guard `f32::clamp` lacks.
#[inline]
fn safe_inc(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 0.49)
    } else {
        0.0
    }
}

pub struct CowbellVoice {
    sr: f32,
    phase1: f32,
    phase2: f32,
    inc1: f32,
    inc2: f32,
    base_hz: f32,
    ratio: f32,
    hp: Filter,
    hp_hz: f32,
    amp: DahdEnv,
    accent_amt: f32,
    gain: f32,
    drift_cents: f32,
    decay_scale: f32,
}

impl CowbellVoice {
    pub fn neutral(sr: f32) -> Self {
        let mut v = Self {
            sr,
            phase1: 0.0,
            phase2: 0.0,
            inc1: 0.0,
            inc2: 0.0,
            base_hz: 540.0,
            ratio: 1.48, // the classic 808 cowbell interval (~540 & ~800 Hz)
            hp: Filter::new(sr),
            hp_hz: 550.0,
            amp: DahdEnv::new(sr),
            accent_amt: 0.5,
            gain: 1.0,
            drift_cents: 0.0,
            decay_scale: 1.0,
        };
        v.apply();
        v
    }

    fn apply(&mut self) {
        // stop short of Nyquist (0.49), matching the resonator/filter margin
        self.inc1 = safe_inc(self.base_hz / self.sr);
        self.inc2 = safe_inc(self.base_hz * self.ratio / self.sr);
        self.hp.set_cutoff(self.hp_hz);
        self.hp.set_resonance(0.1);
        self.amp.set_params(0.3, 2.0, 280.0 * self.decay_scale);
    }

    /// Per-hit AmpDecay mod (1.0 = no mod). No-op when unchanged -> bit-exact.
    pub fn set_decay_mod(&mut self, scale: f32) {
        if scale != self.decay_scale {
            self.decay_scale = scale;
            self.apply();
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.hp.set_sample_rate(sr);
        self.amp.set_sample_rate(sr);
        self.apply();
    }

    pub fn set_pitch_drift_cents(&mut self, cents: f32) {
        self.drift_cents = cents;
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.phase1 = 0.0;
        self.phase2 = 0.0;
        // Re-derive the increments with this hit's drift (ratio 1.0 at 0 cents
        // reproduces the setup values exactly).
        let r = crate::drift::cents_to_ratio(self.drift_cents);
        self.inc1 = safe_inc(self.base_hz * r / self.sr);
        self.inc2 = safe_inc(self.base_hz * self.ratio * r / self.sr);
        self.hp.reset(); // start each hit from a clean filter state
        self.amp.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        // An idle voice costs nothing and doesn't warm the filter state.
        if !self.amp.is_active() {
            return (0.0, 0.0);
        }
        self.phase1 += self.inc1;
        if self.phase1 >= 1.0 {
            self.phase1 -= 1.0;
        }
        self.phase2 += self.inc2;
        if self.phase2 >= 1.0 {
            self.phase2 -= 1.0;
        }
        let s1 = if self.phase1 < 0.5 { 1.0 } else { -1.0 };
        let s2 = if self.phase2 < 0.5 { 1.0 } else { -1.0 };
        let mix = (s1 + s2) * 0.5;
        let f = self.hp.process_high(mix);
        let s = f * self.amp.next() * self.gain * 0.55;
        (s, s)
    }

    pub fn choke(&mut self) {
        self.amp.choke(6.0);
    }

    pub fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    pub fn reset(&mut self) {
        self.amp.reset();
        self.hp.reset();
        self.phase1 = 0.0;
        self.phase2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audible_and_finite() {
        let mut c = CowbellVoice::neutral(48_000.0);
        c.trigger(1.0, false);
        let mut peak = 0.0_f32;
        for _ in 0..8_000 {
            let (l, _) = c.render();
            assert!(l.is_finite());
            peak = peak.max(l.abs());
        }
        assert!(peak > 0.08, "cowbell should be audible, peak={peak}");
    }

    #[test]
    fn idle_is_silent() {
        let mut c = CowbellVoice::neutral(48_000.0);
        for _ in 0..16 {
            assert_eq!(c.render(), (0.0, 0.0));
        }
    }
}
