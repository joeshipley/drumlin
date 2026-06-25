//! `Resonator` — a bank of 1..4 tuned, damped two-pole resonators (modal
//! synthesis; design §3.4). The *ring* of a rim, the *shell* of a tom. Each
//! partial is a 2-pole bandpass excited by an impulse/noise burst; the pole
//! radius (from a decay time) sets the ring length. `flush_denormal` on the
//! recursive poles — the project-wide RT-safety invariant.

use core::f32::consts::PI;
use dsp_core::flush_denormal;

const MAX_PARTIALS: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct Resonator {
    sr: f32,
    // config (kept so a sample-rate change can recompute coefficients)
    freq: [f32; MAX_PARTIALS],
    decay_ms: [f32; MAX_PARTIALS],
    gain: [f32; MAX_PARTIALS],
    // coefficients + state
    a1: [f32; MAX_PARTIALS],
    a2: [f32; MAX_PARTIALS],
    b0: [f32; MAX_PARTIALS],
    z1: [f32; MAX_PARTIALS],
    z2: [f32; MAX_PARTIALS],
    n: usize,
}

impl Resonator {
    pub fn new(sr: f32) -> Self {
        let mut r = Self {
            sr: sr.max(1.0),
            freq: [200.0; MAX_PARTIALS],
            decay_ms: [150.0; MAX_PARTIALS],
            gain: [1.0; MAX_PARTIALS],
            a1: [0.0; MAX_PARTIALS],
            a2: [0.0; MAX_PARTIALS],
            b0: [0.0; MAX_PARTIALS],
            z1: [0.0; MAX_PARTIALS],
            z2: [0.0; MAX_PARTIALS],
            n: 1,
        };
        for i in 0..MAX_PARTIALS {
            r.recompute(i);
        }
        r
    }

    fn recompute(&mut self, i: usize) {
        let w = 2.0 * PI * (self.freq[i] / self.sr).clamp(0.0, 0.49);
        let decay_samples = (self.decay_ms[i] * 0.001 * self.sr).max(1.0);
        // pole radius so the ring reaches ~ -60 dB after decay_ms
        let r = (0.001_f32).powf(1.0 / decay_samples);
        self.a1[i] = -2.0 * r * w.cos();
        self.a2[i] = r * r;
        // (1 - r) keeps the resonant peak near unity gain
        self.b0[i] = 1.0 - r;
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr.max(1.0);
        for i in 0..MAX_PARTIALS {
            self.recompute(i);
        }
    }

    pub fn set_count(&mut self, n: usize) {
        self.n = n.clamp(1, MAX_PARTIALS);
    }

    pub fn set_partial(&mut self, i: usize, freq: f32, decay_ms: f32, gain: f32) {
        if i >= MAX_PARTIALS {
            return;
        }
        // Clamp below Nyquist at the API boundary (defense-in-depth; recompute
        // also clamps) so the stored config reflects the safe value.
        self.freq[i] = freq.clamp(0.0, 0.49 * self.sr);
        self.decay_ms[i] = decay_ms.max(0.1);
        self.gain[i] = gain;
        self.recompute(i);
    }

    /// Process one sample of excitation through every active partial, summing
    /// their (gained) outputs. Feed an impulse or short noise burst to ring it.
    pub fn process(&mut self, input: f32) -> f32 {
        let mut out = 0.0;
        for i in 0..self.n {
            let y = flush_denormal(self.b0[i] * input - self.a1[i] * self.z1[i] - self.a2[i] * self.z2[i]);
            self.z2[i] = self.z1[i];
            self.z1[i] = y;
            out += y * self.gain[i];
        }
        out
    }

    pub fn reset(&mut self) {
        self.z1 = [0.0; MAX_PARTIALS];
        self.z2 = [0.0; MAX_PARTIALS];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_rings_oscillates_and_decays() {
        let mut r = Resonator::new(48_000.0);
        r.set_partial(0, 400.0, 80.0, 1.0);
        r.reset();
        r.process(1.0); // strike
        let mut peak = 0.0_f32;
        let mut sign_changes = 0;
        let mut prev = 0.0_f32;
        let mut tail_energy = 0.0_f32;
        let tail_from = 18_000;
        let total = 24_000;
        for n in 0..total {
            let y = r.process(0.0);
            assert!(y.is_finite());
            peak = peak.max(y.abs());
            if (y > 0.0) != (prev > 0.0) {
                sign_changes += 1;
            }
            prev = y;
            if n >= tail_from {
                tail_energy += y.abs();
            }
        }
        assert!(peak > 0.01, "resonator should ring from a strike, peak={peak}");
        assert!(sign_changes > 10, "a tuned resonator should oscillate, changes={sign_changes}");
        let tail_avg = tail_energy / (total - tail_from) as f32;
        assert!(tail_avg < peak * 0.2, "ring should decay toward silence: tail_avg={tail_avg} peak={peak}");
    }

    #[test]
    fn higher_q_rings_longer() {
        let ring_len = |decay_ms: f32| {
            let mut r = Resonator::new(48_000.0);
            r.set_partial(0, 500.0, decay_ms, 1.0);
            r.reset();
            r.process(1.0);
            let mut n = 0;
            let mut quiet = 0;
            loop {
                let y = r.process(0.0);
                if y.abs() < 1e-4 {
                    quiet += 1;
                    if quiet > 64 {
                        break;
                    }
                } else {
                    quiet = 0;
                }
                n += 1;
                if n > 480_000 {
                    break;
                }
            }
            n
        };
        assert!(ring_len(200.0) > ring_len(50.0));
    }
}
