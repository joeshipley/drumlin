//! KICK — sine body + fast exponential pitch drop ("boom→thud"), a short noise
//! "click" transient for the knock, light tube drive. Tuned clean and punchy,
//! 909-leaning (design §3.6/§3.7).

use crate::pitch_env::DahdEnv;
use dsp_core::{Drive, DriveKind, Noise, NoiseType, Oscillator, Waveform};

pub struct KickVoice {
    sr: f32,
    osc: Oscillator,
    amp: DahdEnv,
    pitch: DahdEnv,
    click_noise: Noise,
    click_env: DahdEnv,
    drive: Drive,
    base_hz: f32,
    pitch_amount_st: f32,
    click_level: f32,
    accent_amt: f32,
    gain: f32,
}

impl KickVoice {
    pub fn neutral(sr: f32) -> Self {
        let mut osc = Oscillator::new(sr);
        osc.waveform = Waveform::Sine;
        let mut drive = Drive::new(sr);
        drive.set_params(true, DriveKind::Tube, 0.18, 0.5, 16.0, 1.0, 0.0, 1.0);
        let mut v = Self {
            sr,
            osc,
            amp: DahdEnv::new(sr),
            pitch: DahdEnv::new(sr),
            click_noise: Noise::new(0x4B1C_C0DE),
            click_env: DahdEnv::new(sr),
            drive,
            base_hz: 52.0,
            pitch_amount_st: 30.0,
            click_level: 0.35,
            accent_amt: 0.5,
            gain: 1.0,
        };
        v.apply_envs();
        v
    }

    fn apply_envs(&mut self) {
        self.amp.set_params(0.5, 3.0, 260.0); // punchy, not boomy
        self.pitch.set_params(0.0, 0.0, 45.0); // fast 909 pitch drop
        self.click_env.set_params(0.0, 0.0, 2.5); // 2.5 ms knock
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.osc.set_sample_rate(sr);
        self.amp.set_sample_rate(sr);
        self.pitch.set_sample_rate(sr);
        self.click_env.set_sample_rate(sr);
        self.drive.set_sample_rate(sr);
        self.apply_envs();
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.osc.reset();
        self.amp.trigger();
        self.pitch.trigger();
        self.click_env.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let st = self.pitch_amount_st * self.pitch.next();
        // Clamp below Nyquist so future tuning/param ranges can't alias the body.
        let hz = (self.base_hz * 2.0_f32.powf(st / 12.0)).clamp(1.0, 0.45 * self.sr);
        self.osc.set_frequency(hz);
        let body = self.osc.next_sample() * self.amp.next();
        let click = self.click_noise.next(NoiseType::White) * self.click_env.next() * self.click_level;
        let s = self.drive.process(body + click) * self.gain;
        (s, s)
    }

    pub fn choke(&mut self) {
        self.amp.choke(8.0);
        self.click_env.choke(2.0);
    }

    pub fn is_active(&self) -> bool {
        self.amp.is_active() || self.click_env.is_active()
    }

    pub fn reset(&mut self) {
        self.amp.reset();
        self.pitch.reset();
        self.click_env.reset();
        self.osc.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_finite_signal_then_silence() {
        let mut k = KickVoice::neutral(48_000.0);
        k.trigger(1.0, false);
        let mut peak = 0.0_f32;
        let mut n = 0;
        while k.is_active() {
            let (l, r) = k.render();
            assert!(l.is_finite() && r.is_finite());
            peak = peak.max(l.abs());
            n += 1;
            assert!(n < 96_000, "kick should decay within 2s");
        }
        assert!(peak > 0.2, "kick should be audible, peak={peak}");
    }

    #[test]
    fn pitch_sweeps_downward() {
        // Early body energy should be higher-frequency than late: count zero
        // crossings in the first vs second 10 ms.
        let mut k = KickVoice::neutral(48_000.0);
        k.trigger(1.0, false);
        let win = 480;
        let mut early = 0;
        let mut late = 0;
        let mut prev = 0.0_f32;
        for i in 0..(win * 4) {
            let (s, _) = k.render();
            if (s > 0.0) != (prev > 0.0) {
                if i < win {
                    early += 1;
                } else if i >= win * 3 {
                    late += 1;
                }
            }
            prev = s;
        }
        assert!(early > late, "pitch should drop: early={early} late={late}");
    }
}
