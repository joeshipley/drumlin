//! SNARE — the 909 dual-tone recipe: two detuned triangle tones with their own
//! decay, plus high-passed white noise (the wire buzz) with its own decay. The
//! tone/noise balance is the defining knob (design §3.6/§3.8). Tuned punchy and
//! crisp for the clean Neutral kit.

use crate::pitch_env::DahdEnv;
use dsp_core::{Filter, Noise, NoiseType, Oscillator, Waveform};

pub struct SnareVoice {
    sr: f32,
    tone1: Oscillator,
    tone2: Oscillator,
    tone_env: DahdEnv,
    noise: Noise,
    noise_hp: Filter,
    noise_env: DahdEnv,
    tune: f32,
    tone2_ratio: f32,
    tone_level: f32,
    noise_level: f32,
    accent_amt: f32,
    gain: f32,
}

impl SnareVoice {
    pub fn neutral(sr: f32) -> Self {
        let mut tone1 = Oscillator::new(sr);
        tone1.waveform = Waveform::Triangle;
        let mut tone2 = Oscillator::new(sr);
        tone2.waveform = Waveform::Triangle;
        let mut noise_hp = Filter::new(sr);
        noise_hp.set_cutoff(1700.0);
        noise_hp.set_resonance(0.08);
        let mut v = Self {
            sr,
            tone1,
            tone2,
            tone_env: DahdEnv::new(sr),
            noise: Noise::new(0x57A1_2E5D),
            noise_hp,
            noise_env: DahdEnv::new(sr),
            tune: 185.0,
            tone2_ratio: 1.6,
            tone_level: 0.5,
            noise_level: 0.7,
            accent_amt: 0.5,
            gain: 1.0,
        };
        v.apply();
        v
    }

    fn apply(&mut self) {
        self.tone1.set_frequency(self.tune);
        self.tone2.set_frequency(self.tune * self.tone2_ratio);
        self.tone_env.set_params(0.3, 0.0, 110.0);
        self.noise_env.set_params(0.3, 0.0, 180.0);
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.tone1.set_sample_rate(sr);
        self.tone2.set_sample_rate(sr);
        self.tone_env.set_sample_rate(sr);
        self.noise_env.set_sample_rate(sr);
        self.noise_hp.set_sample_rate(sr);
        self.noise_hp.set_cutoff(1700.0);
        self.apply();
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.tone1.reset();
        self.tone2.reset();
        self.apply();
        self.tone_env.trigger();
        self.noise_env.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let t = self.tone_env.next();
        let tone = (self.tone1.next_sample() + self.tone2.next_sample() * 0.7) * t * self.tone_level;
        let n = self.noise_hp.process_high(self.noise.next(NoiseType::White));
        let noise = n * self.noise_env.next() * self.noise_level;
        let s = (tone + noise) * self.gain * 0.8;
        (s, s)
    }

    pub fn choke(&mut self) {
        self.tone_env.choke(8.0);
        self.noise_env.choke(8.0);
    }

    pub fn is_active(&self) -> bool {
        self.tone_env.is_active() || self.noise_env.is_active()
    }

    pub fn reset(&mut self) {
        self.tone_env.reset();
        self.noise_env.reset();
        self.tone1.reset();
        self.tone2.reset();
        self.noise_hp.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_finite_signal_then_silence() {
        let mut s = SnareVoice::neutral(48_000.0);
        s.trigger(1.0, false);
        let mut peak = 0.0_f32;
        let mut n = 0;
        while s.is_active() {
            let (l, r) = s.render();
            assert!(l.is_finite() && r.is_finite());
            peak = peak.max(l.abs());
            n += 1;
            assert!(n < 96_000, "snare should decay within 2s");
        }
        assert!(peak > 0.15, "snare should be audible, peak={peak}");
    }
}
