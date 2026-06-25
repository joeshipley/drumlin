//! ZAP — the modular "anything" percussive/FX voice on the SAMPLE/USER track
//! until user-sample loading lands (design §3.6 Zap/FX). An FM oscillator with a
//! fast exponential pitch sweep: pitched zaps, lasers, synthy accents.

use crate::pitch_env::DahdEnv;
use dsp_core::{Oscillator, Waveform};

pub struct ZapVoice {
    sr: f32,
    osc: Oscillator,
    pitch: DahdEnv,
    amp: DahdEnv,
    base_hz: f32,
    pitch_amount_st: f32,
    accent_amt: f32,
    gain: f32,
}

impl ZapVoice {
    pub fn neutral(sr: f32) -> Self {
        let mut osc = Oscillator::new(sr);
        osc.waveform = Waveform::Fm;
        osc.set_fm_ratio(2.0);
        osc.set_fm_index(3.0);
        let mut v = Self {
            sr,
            osc,
            pitch: DahdEnv::new(sr),
            amp: DahdEnv::new(sr),
            base_hz: 220.0,
            pitch_amount_st: 24.0, // sweeps down ~2 octaves -> "zap"
            accent_amt: 0.5,
            gain: 1.0,
        };
        v.apply();
        v
    }

    fn apply(&mut self) {
        self.pitch.set_params(0.0, 0.0, 80.0);
        self.amp.set_params(0.5, 0.0, 180.0);
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.osc.set_sample_rate(sr);
        self.pitch.set_sample_rate(sr);
        self.amp.set_sample_rate(sr);
        self.apply();
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.osc.reset();
        self.pitch.trigger();
        self.amp.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let st = self.pitch_amount_st * self.pitch.next();
        let hz = (self.base_hz * 2.0_f32.powf(st / 12.0)).clamp(1.0, 0.45 * self.sr);
        self.osc.set_frequency(hz);
        let s = self.osc.next_sample() * self.amp.next() * self.gain;
        (s, s)
    }

    pub fn choke(&mut self) {
        self.amp.choke(8.0);
    }

    pub fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    pub fn reset(&mut self) {
        self.amp.reset();
        self.pitch.reset();
        self.osc.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audible_finite_and_sweeps_down() {
        let mut z = ZapVoice::neutral(48_000.0);
        z.trigger(1.0, false);
        let win = 480;
        let mut early = 0;
        let mut late = 0;
        let mut prev = 0.0_f32;
        let mut peak = 0.0_f32;
        for i in 0..(win * 4) {
            let (s, _) = z.render();
            assert!(s.is_finite());
            peak = peak.max(s.abs());
            if (s > 0.0) != (prev > 0.0) {
                if i < win {
                    early += 1;
                } else if i >= win * 3 {
                    late += 1;
                }
            }
            prev = s;
        }
        assert!(peak > 0.1, "zap should be audible, peak={peak}");
        assert!(early > late, "zap pitch should sweep down: early={early} late={late}");
    }
}
