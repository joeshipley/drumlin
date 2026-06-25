//! RIM — a very short metallic `Resonator` "tonk" excited by a noise burst, plus
//! a raw click transient (design §3.6). High-Q, short decay: groove glue.

use crate::pitch_env::DahdEnv;
use crate::resonator::Resonator;
use dsp_core::{Noise, NoiseType};

pub struct RimVoice {
    sr: f32,
    res: Resonator,
    amp: DahdEnv,
    exciter: DahdEnv,
    noise: Noise,
    click_level: f32,
    accent_amt: f32,
    gain: f32,
}

impl RimVoice {
    pub fn neutral(sr: f32) -> Self {
        let mut res = Resonator::new(sr);
        res.set_count(2);
        res.set_partial(0, 1700.0, 35.0, 1.0);
        res.set_partial(1, 2550.0, 25.0, 0.6);
        let mut v = Self {
            sr,
            res,
            amp: DahdEnv::new(sr),
            exciter: DahdEnv::new(sr),
            noise: Noise::new(0x21B0_5A1E),
            click_level: 0.4,
            accent_amt: 0.5,
            gain: 1.0,
        };
        v.apply();
        v
    }

    fn apply(&mut self) {
        self.amp.set_params(0.2, 0.0, 50.0);
        self.exciter.set_params(0.0, 0.0, 2.0);
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.res.set_sample_rate(sr);
        self.amp.set_sample_rate(sr);
        self.exciter.set_sample_rate(sr);
        self.apply();
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.res.reset();
        self.amp.trigger();
        self.exciter.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let exc = self.noise.next(NoiseType::White) * self.exciter.next();
        let body = self.res.process(exc);
        let s = (body * self.amp.next() + exc * self.click_level) * self.gain;
        (s, s)
    }

    pub fn choke(&mut self) {
        self.amp.choke(4.0);
        self.exciter.choke(1.0);
    }

    pub fn is_active(&self) -> bool {
        self.amp.is_active() || self.exciter.is_active()
    }

    pub fn reset(&mut self) {
        self.res.reset();
        self.amp.reset();
        self.exciter.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audible_finite_and_short() {
        let mut r = RimVoice::neutral(48_000.0);
        r.trigger(1.0, false);
        let mut peak = 0.0_f32;
        let mut n = 0;
        while r.is_active() {
            let (l, _) = r.render();
            assert!(l.is_finite());
            peak = peak.max(l.abs());
            n += 1;
            assert!(n < 48_000);
        }
        assert!(peak > 0.1, "rim should be audible, peak={peak}");
        assert!(n < 12_000, "rim should be short (<250ms), got {n} samples");
    }
}
