//! CLAP — two `ClapDiffuser`s (the stacked multi-burst recipe) panned hard L/R
//! with slightly different burst timing for the wide stereo "splatter," each
//! band-passed to the ~1 kHz clap timbre (design §3.6). The stacked bursts are
//! why one trigger sounds like a couple of hands clapping at once.

use crate::clap_diffuser::ClapDiffuser;
use synth_core::Filter;

pub struct ClapVoice {
    sr: f32,
    diff_l: ClapDiffuser,
    diff_r: ClapDiffuser,
    hp_l: Filter,
    hp_r: Filter,
    accent_amt: f32,
    gain: f32,
}

impl ClapVoice {
    pub fn neutral(sr: f32) -> Self {
        let mut hp_l = Filter::new(sr);
        hp_l.set_cutoff(650.0);
        hp_l.set_resonance(0.1);
        let mut hp_r = Filter::new(sr);
        hp_r.set_cutoff(650.0);
        hp_r.set_resonance(0.1);
        Self {
            sr,
            // Different seeds + a 1.5 ms timing spread on the right -> stereo width.
            diff_l: ClapDiffuser::new(sr, 0xC1A9_0001, 0.0),
            diff_r: ClapDiffuser::new(sr, 0xC1A9_0002, 1.5),
            hp_l,
            hp_r,
            accent_amt: 0.5,
            gain: 1.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.diff_l.set_sample_rate(sr);
        self.diff_r.set_sample_rate(sr);
        self.hp_l.set_sample_rate(sr);
        self.hp_r.set_sample_rate(sr);
        self.hp_l.set_cutoff(650.0);
        self.hp_r.set_cutoff(650.0);
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.diff_l.trigger();
        self.diff_r.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let l = self.hp_l.process_high(self.diff_l.next()) * self.gain * 0.9;
        let r = self.hp_r.process_high(self.diff_r.next()) * self.gain * 0.9;
        (l, r)
    }

    pub fn choke(&mut self) {
        self.diff_l.reset();
        self.diff_r.reset();
    }

    pub fn is_active(&self) -> bool {
        self.diff_l.is_active() || self.diff_r.is_active()
    }

    pub fn reset(&mut self) {
        self.diff_l.reset();
        self.diff_r.reset();
        self.hp_l.reset();
        self.hp_r.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_stereo_signal_then_silence() {
        let mut c = ClapVoice::neutral(48_000.0);
        c.trigger(1.0, false);
        let mut peak = 0.0_f32;
        let mut stereo_diff = false;
        let mut n = 0;
        while c.is_active() {
            let (l, r) = c.render();
            assert!(l.is_finite() && r.is_finite());
            peak = peak.max(l.abs()).max(r.abs());
            if (l - r).abs() > 1e-4 {
                stereo_diff = true;
            }
            n += 1;
            assert!(n < 96_000, "clap should decay within 2s");
        }
        assert!(peak > 0.05, "clap should be audible, peak={peak}");
        assert!(stereo_diff, "clap should be genuinely stereo (L != R)");
    }
}
