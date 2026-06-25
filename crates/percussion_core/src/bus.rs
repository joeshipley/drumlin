//! `DrumBus` — the kit's shared dynamics bus. "Glue is the headline" (design
//! §5.3): the whole kit is compressed and limited as a unit so it hits like one
//! instrument. M3 ships the SSL-style glue compressor → true-peak limiter, both
//! `dsp_core::Dynamics` verbatim. Transient shaper, drive/bit-crush, parallel/NY
//! comp, sidechain PUMP, tape delay and reverb join at M7.

use dsp_core::{Dynamics, LimiterStyle};

pub struct DrumBus {
    dynamics: Dynamics,
}

impl DrumBus {
    pub fn neutral(sr: f32) -> Self {
        let mut bus = Self { dynamics: Dynamics::new(sr) };
        bus.configure();
        bus
    }

    fn configure(&mut self) {
        // Gentle SSL-style glue: moderate threshold, low ratio, light makeup —
        // it should breathe, not crush.
        self.dynamics.set_glue(true, -18.0, 2.0, 3.0, 1.0);
        // True-peak limiter just under 0 dBTP, transparent release — the safety
        // ceiling that lets the kit be loud without clipping.
        self.dynamics.set_limiter(true, -0.3, 0.05, LimiterStyle::Transparent);
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        // Dynamics sizes its look-ahead rings to the sample rate at construction;
        // rebuild it here (called off the audio thread, in initialize()).
        self.dynamics = Dynamics::new(sr);
        self.configure();
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.dynamics.process(l, r)
    }

    /// Live glue/limiter gain reduction (dB) for the GUI meter (wired in M9).
    pub fn gain_reduction_db(&self) -> f32 {
        self.dynamics.gain_reduction_db()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_quiet_signal_roughly_through() {
        let mut bus = DrumBus::neutral(48_000.0);
        let mut peak = 0.0_f32;
        for i in 0..2_000 {
            let s = 0.1 * (i as f32 * 0.05).sin();
            let (l, _) = bus.process(s, s);
            assert!(l.is_finite());
            peak = peak.max(l.abs());
        }
        assert!(peak > 0.05, "quiet signal should pass, peak={peak}");
    }

    #[test]
    fn limiter_holds_the_ceiling_on_hot_input() {
        let mut bus = DrumBus::neutral(48_000.0);
        let mut peak = 0.0_f32;
        // slam it well past 0 dBFS; after the look-ahead settles the output must
        // stay under ~unity (ceiling -0.3 dBTP)
        for i in 0..8_000 {
            let s = 4.0 * (i as f32 * 0.1).sin();
            let (l, r) = bus.process(s, s);
            if i > 1_000 {
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        assert!(peak <= 1.02, "true-peak limiter must hold ~0 dBFS, peak={peak}");
    }
}
