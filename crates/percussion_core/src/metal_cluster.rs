//! `MetalCluster` — the classic 808 metallic source: six square oscillators at
//! fixed inharmonic frequencies, summed (design §3.4). Drives the hi-hats, ride
//! and (with 2 of the 6) the cowbell. Band-passing + an envelope downstream
//! turns this raw cluster into a closed hat, open hat or cymbal.
//!
//! The six frequencies are the well-known TR-808 cymbal oscillator set; a `tune`
//! multiplier scales them all together. Phases reset on `trigger` so a given
//! pattern renders bit-identically (the family's reproducibility signature).

/// The six TR-808 metallic oscillator frequencies (Hz).
const FREQS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

#[derive(Clone, Copy, Debug)]
pub struct MetalCluster {
    sr: f32,
    phases: [f32; 6],
    incs: [f32; 6],
    tune: f32,
}

impl MetalCluster {
    pub fn new(sr: f32) -> Self {
        let mut c = Self {
            sr: sr.max(1.0),
            phases: [0.0; 6],
            incs: [0.0; 6],
            tune: 1.0,
        };
        c.set_tune(1.0);
        c
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr.max(1.0);
        self.set_tune(self.tune);
    }

    /// Scale all six oscillator frequencies (1.0 = stock 808 tuning).
    pub fn set_tune(&mut self, tune: f32) {
        self.tune = tune.max(0.01);
        for i in 0..6 {
            self.incs[i] = (FREQS[i] * self.tune / self.sr).clamp(0.0, 0.5);
        }
    }

    /// Reset all six phases — call on note trigger for deterministic renders.
    pub fn trigger(&mut self) {
        self.phases = [0.0; 6];
    }

    pub fn reset(&mut self) {
        self.phases = [0.0; 6];
    }

    /// One sample of the summed six-square cluster, normalized to ~ -1..1.
    pub fn next(&mut self) -> f32 {
        let mut sum = 0.0;
        for i in 0..6 {
            self.phases[i] += self.incs[i];
            if self.phases[i] >= 1.0 {
                self.phases[i] -= 1.0;
            }
            sum += if self.phases[i] < 0.5 { 1.0 } else { -1.0 };
        }
        sum * (1.0 / 6.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_finite_and_bounded() {
        let mut c = MetalCluster::new(48_000.0);
        c.trigger();
        for _ in 0..48_000 {
            let s = c.next();
            assert!(s.is_finite());
            assert!((-1.0..=1.0).contains(&s), "cluster out of range: {s}");
        }
    }

    #[test]
    fn produces_signal() {
        let mut c = MetalCluster::new(48_000.0);
        c.trigger();
        let mut peak = 0.0_f32;
        for _ in 0..4_000 {
            peak = peak.max(c.next().abs());
        }
        assert!(peak > 0.2, "cluster should produce an audible signal, peak={peak}");
    }

    #[test]
    fn trigger_is_deterministic() {
        let mut a = MetalCluster::new(48_000.0);
        let mut b = MetalCluster::new(48_000.0);
        a.trigger();
        b.trigger();
        for i in 0..2_000 {
            assert_eq!(a.next().to_bits(), b.next().to_bits(), "diverged at {i}");
        }
    }

    #[test]
    fn tune_raises_pitch_energy() {
        // Higher tune -> faster zero crossings (more high-frequency energy).
        let count_crossings = |tune: f32| {
            let mut c = MetalCluster::new(48_000.0);
            c.set_tune(tune);
            c.trigger();
            let mut prev = c.next();
            let mut x = 0;
            for _ in 0..4_000 {
                let s = c.next();
                if (s > 0.0) != (prev > 0.0) {
                    x += 1;
                }
                prev = s;
            }
            x
        };
        assert!(count_crossings(2.0) > count_crossings(1.0));
    }
}
