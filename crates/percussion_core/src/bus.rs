//! `DrumBus` — the kit's shared dynamics + FX bus. "Glue is the headline"
//! (design §5.3): the whole kit is processed as a unit so it hits like one
//! instrument.
//!
//! M3 shipped the SSL-style glue compressor → true-peak limiter. **M7 adds the
//! sidechain PUMP** — "the single most genre-defining control in the plugin" —
//! plus a lo-fi bus drive. The pump ducks the whole bus on the beat
//! (`PumpSource::IntKick`, quarter-note division) for the Daft-Punk/French-house
//! breathe; `pump_envelope()` exposes the live duck gain so the GUI can *see* it.
//! Transient shaper, parallel/NY comp, tape delay and reverb send join next.
//!
//! Signal flow: kit sum → bus drive → Dynamics (pump → glue → true-peak limiter).
//! At the Neutral defaults (pump = 0, drive = 0) the bus is bit-identical to M3,
//! so the golden anchors are unaffected.

use synth_core::{Drive, DriveKind, Dynamics, LimiterStyle, PumpSource};

pub struct DrumBus {
    dynamics: Dynamics,
    drive_l: Drive,
    drive_r: Drive,
    sr: f32,
    tempo: f32,
    pump: f32,
    drive: f32,
}

impl DrumBus {
    pub fn neutral(sr: f32) -> Self {
        let mut bus = Self {
            dynamics: Dynamics::new(sr),
            drive_l: Drive::new(sr),
            drive_r: Drive::new(sr),
            sr,
            tempo: 120.0,
            pump: 0.0,
            drive: 0.0,
        };
        bus.configure();
        bus
    }

    fn configure(&mut self) {
        // Gentle SSL-style glue, then a true-peak limiter just under 0 dBTP.
        self.dynamics.set_glue(true, -18.0, 2.0, 3.0, 1.0);
        self.dynamics.set_limiter(true, -0.3, 0.05, LimiterStyle::Transparent);
        self.dynamics.set_tempo(self.tempo);
        self.apply_pump();
        self.apply_drive();
    }

    fn apply_pump(&mut self) {
        // Quarter-note duck = the four-on-the-floor pump. amount 0 -> no duck
        // (bus stays bit-identical to the un-pumped chain).
        let division = 60.0 / self.tempo.max(1.0);
        self.dynamics
            .set_pump(self.pump, PumpSource::IntKick, division, 0.5, 0.0);
    }

    fn apply_drive(&mut self) {
        let on = self.drive > 0.001;
        self.drive_l
            .set_params(on, DriveKind::Tube, self.drive, 0.5, 16.0, 1.0, 0.0, 1.0);
        self.drive_r
            .set_params(on, DriveKind::Tube, self.drive, 0.5, 16.0, 1.0, 0.0, 1.0);
    }

    /// Push the host tempo (for the pump's beat-synced duck). Call once per block.
    pub fn set_tempo(&mut self, bpm: f32) {
        self.tempo = bpm.max(1.0);
        self.dynamics.set_tempo(self.tempo);
        self.apply_pump();
    }

    /// Sidechain PUMP depth, 0..1. The headline transport knob.
    pub fn set_pump(&mut self, amount: f32) {
        self.pump = amount.clamp(0.0, 1.0);
        self.apply_pump();
    }

    /// Lo-fi bus drive, 0..1.
    pub fn set_drive(&mut self, amount: f32) {
        self.drive = amount.clamp(0.0, 1.0);
        self.apply_drive();
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.dynamics = Dynamics::new(sr);
        self.drive_l = Drive::new(sr);
        self.drive_r = Drive::new(sr);
        self.configure();
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let l = self.drive_l.process(l);
        let r = self.drive_r.process(r);
        self.dynamics.process(l, r)
    }

    /// Live pump duck gain (1.0 = open, < 1 = ducking) — for the GUI pump meter.
    pub fn pump_envelope(&self) -> f32 {
        self.dynamics.pump_envelope()
    }

    /// Live glue/limiter gain reduction (dB), for the GUI glue meter (M9).
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
        for i in 0..8_000 {
            let s = 4.0 * (i as f32 * 0.1).sin();
            let (l, r) = bus.process(s, s);
            if i > 1_000 {
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        assert!(peak <= 1.02, "true-peak limiter must hold ~0 dBFS, peak={peak}");
    }

    #[test]
    fn pump_off_is_unity_pump_envelope() {
        let bus = DrumBus::neutral(48_000.0);
        assert_eq!(bus.pump_envelope(), 1.0, "pump off must hold unity duck gain");
    }

    #[test]
    fn pump_ducks_the_bus_on_the_beat() {
        let mut bus = DrumBus::neutral(48_000.0);
        bus.set_tempo(120.0);
        bus.set_pump(0.9);
        // run a steady DC-ish tone through and watch the duck gain swing below 1
        let mut min_env = 1.0_f32;
        let mut max_env = 0.0_f32;
        for i in 0..96_000 {
            // one second = two quarter-note ducks at 120 BPM
            let s = 0.3 * (i as f32 * 0.02).sin();
            bus.process(s, s);
            let e = bus.pump_envelope();
            min_env = min_env.min(e);
            max_env = max_env.max(e);
        }
        assert!(min_env < 0.8, "pump should duck the bus well below unity, min={min_env}");
        assert!(max_env > 0.95, "and recover toward unity between ducks, max={max_env}");
    }

    #[test]
    fn drive_adds_harmonics() {
        let peak_of = |drive: f32| {
            let mut bus = DrumBus::neutral(48_000.0);
            bus.set_drive(drive);
            let mut p = 0.0_f32;
            for i in 0..4_000 {
                let s = 0.4 * (i as f32 * 0.05).sin();
                let (l, _) = bus.process(s, s);
                p = p.max(l.abs());
            }
            p
        };
        // a driven sine should be hotter/fatter than the clean one
        assert!(peak_of(0.8) > peak_of(0.0) * 1.1, "bus drive should add level/harmonics");
    }
}
