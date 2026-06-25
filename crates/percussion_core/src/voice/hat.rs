//! HATS — the 808 metallic cluster into a steep high-pass and a short (closed)
//! or long (open) exponential decay (design §3.6). Closed + open share a choke
//! group at the kit level, so a closed hat chokes the ringing open hat.

use crate::metal_cluster::MetalCluster;
use crate::pitch_env::DahdEnv;
use dsp_core::Filter;

pub struct HatVoice {
    sr: f32,
    cluster: MetalCluster,
    hp: Filter,
    amp: DahdEnv,
    decay_ms: f32,
    hp_hz: f32,
    accent_amt: f32,
    gain: f32,
}

impl HatVoice {
    fn make(sr: f32, decay_ms: f32, hp_hz: f32) -> Self {
        let mut hp = Filter::new(sr);
        hp.set_cutoff(hp_hz);
        hp.set_resonance(0.05);
        let mut v = Self {
            sr,
            cluster: MetalCluster::new(sr),
            hp,
            amp: DahdEnv::new(sr),
            decay_ms,
            hp_hz,
            accent_amt: 0.5,
            gain: 1.0,
        };
        v.amp.set_params(0.2, 0.0, decay_ms);
        v
    }

    /// Tight closed hat.
    pub fn closed(sr: f32) -> Self {
        Self::make(sr, 55.0, 8000.0)
    }

    /// Ringing open hat (choked by the closed hat in the same group).
    pub fn open(sr: f32) -> Self {
        Self::make(sr, 380.0, 7000.0)
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.cluster.set_sample_rate(sr);
        self.hp.set_sample_rate(sr);
        self.hp.set_cutoff(self.hp_hz);
        self.amp.set_sample_rate(sr);
        self.amp.set_params(0.2, 0.0, self.decay_ms);
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        self.cluster.trigger();
        self.amp.trigger();
        let acc = if accent { 1.0 + self.accent_amt } else { 1.0 };
        self.gain = (0.3 + 0.7 * velocity.clamp(0.0, 1.0)) * acc;
    }

    pub fn render(&mut self) -> (f32, f32) {
        let f = self.hp.process_high(self.cluster.next());
        let s = f * self.amp.next() * self.gain * 0.6;
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
        self.cluster.reset();
        self.hp.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_rings_longer_than_closed() {
        let count = |mut h: HatVoice| {
            h.trigger(1.0, false);
            let mut n = 0;
            while h.is_active() {
                h.render();
                n += 1;
                if n > 96_000 {
                    break;
                }
            }
            n
        };
        let closed = count(HatVoice::closed(48_000.0));
        let open = count(HatVoice::open(48_000.0));
        assert!(open > closed * 2, "open hat must ring much longer: open={open} closed={closed}");
    }

    #[test]
    fn audible_and_finite() {
        let mut h = HatVoice::closed(48_000.0);
        h.trigger(1.0, false);
        let mut peak = 0.0_f32;
        for _ in 0..4_000 {
            let (l, _) = h.render();
            assert!(l.is_finite());
            peak = peak.max(l.abs());
        }
        assert!(peak > 0.05, "hat should be audible, peak={peak}");
    }
}
