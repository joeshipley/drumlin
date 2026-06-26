//! TOMS — pitched sine body with a gentle exponential pitch drop, a `Resonator`
//! shell for body, and a touch of noise attack (design §3.6). Lo/Hi differ in
//! base tuning. Mono retrigger for now; Poly(2) ring-through is a later refinement.

use crate::pitch_env::DahdEnv;
use crate::resonator::Resonator;
use synth_core::{Noise, NoiseType, Oscillator, Waveform};

pub struct TomVoice {
    sr: f32,
    osc: Oscillator,
    pitch: DahdEnv,
    amp: DahdEnv,
    shell: Resonator,
    exciter: DahdEnv,
    noise: Noise,
    base_hz: f32,
    pitch_amount_st: f32,
    amp_decay_ms: f32,
    accent_amt: f32,
    gain: f32,
    drift_cents: f32,
    decay_scale: f32,
}

impl TomVoice {
    fn make(sr: f32, base_hz: f32, amp_decay_ms: f32, seed: u32) -> Self {
        let mut osc = Oscillator::new(sr);
        osc.waveform = Waveform::Sine;
        let mut shell = Resonator::new(sr);
        shell.set_count(1); // the partial is baked in apply() so AmpDecay scales it too
        let mut v = Self {
            sr,
            osc,
            pitch: DahdEnv::new(sr),
            amp: DahdEnv::new(sr),
            shell,
            exciter: DahdEnv::new(sr),
            noise: Noise::new(seed),
            base_hz,
            pitch_amount_st: 6.0,
            amp_decay_ms,
            accent_amt: 0.5,
            gain: 1.0,
            drift_cents: 0.0,
            decay_scale: 1.0,
        };
        v.apply();
        v
    }

    pub fn low(sr: f32) -> Self {
        Self::make(sr, 90.0, 320.0, 0x701A_0001)
    }

    pub fn high(sr: f32) -> Self {
        Self::make(sr, 160.0, 240.0, 0x701A_0002)
    }

    fn apply(&mut self) {
        self.pitch.set_params(0.0, 0.0, 60.0);
        // Body amp env + shell ring both scale with AmpDecay (uniform decay mod);
        // at decay_scale = 1.0 both reproduce the baked value bit-exactly.
        self.amp.set_params(0.5, 2.0, self.amp_decay_ms * self.decay_scale);
        self.shell.set_partial(0, self.base_hz * 1.5, self.amp_decay_ms * 0.8 * self.decay_scale, 0.5);
        self.exciter.set_params(0.0, 0.0, 5.0);
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
        self.osc.set_sample_rate(sr);
        self.pitch.set_sample_rate(sr);
        self.amp.set_sample_rate(sr);
        self.exciter.set_sample_rate(sr);
        self.shell.set_sample_rate(sr);
        self.apply();
    }

    pub fn set_pitch_drift_cents(&mut self, cents: f32) {
        self.drift_cents = cents;
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.osc.reset();
        self.shell.reset();
        self.pitch.trigger();
        self.amp.trigger();
        self.exciter.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let st = self.pitch_amount_st * self.pitch.next();
        // Drift folds into the pitch exponent (one powf; bit-exact at 0 cents).
        let hz = (self.base_hz * 2.0_f32.powf(st / 12.0 + self.drift_cents / 1200.0))
            .clamp(1.0, 0.45 * self.sr);
        self.osc.set_frequency(hz);
        let body = self.osc.next_sample() * self.amp.next();
        let exc = self.noise.next(NoiseType::White) * self.exciter.next() * 0.4;
        let shell = self.shell.process(exc);
        // Internal trim so a tom sits below the kick instead of pinning the bus
        // limiter (matches the hat/cowbell convention of an intrinsic scale).
        let s = (body + shell) * self.gain * 0.5;
        (s, s)
    }

    pub fn choke(&mut self) {
        self.amp.choke(8.0);
        self.exciter.choke(2.0);
    }

    pub fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    pub fn reset(&mut self) {
        self.amp.reset();
        self.pitch.reset();
        self.exciter.reset();
        self.osc.reset();
        self.shell.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lo_and_hi_are_audible_and_finite() {
        for mut tom in [TomVoice::low(48_000.0), TomVoice::high(48_000.0)] {
            tom.trigger(1.0, false);
            let mut peak = 0.0_f32;
            let mut n = 0;
            while tom.is_active() {
                let (l, r) = tom.render();
                assert!(l.is_finite() && r.is_finite());
                peak = peak.max(l.abs());
                n += 1;
                assert!(n < 96_000);
            }
            assert!(peak > 0.15, "tom should be audible, peak={peak}");
        }
    }

    #[test]
    fn low_tom_is_lower_than_high_tom() {
        let cross = |mut t: TomVoice| {
            t.trigger(1.0, false);
            let mut prev = 0.0_f32;
            let mut x = 0;
            for _ in 0..2_000 {
                let (s, _) = t.render();
                if (s > 0.0) != (prev > 0.0) {
                    x += 1;
                }
                prev = s;
            }
            x
        };
        assert!(cross(TomVoice::low(48_000.0)) < cross(TomVoice::high(48_000.0)));
    }
}
