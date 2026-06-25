//! `DrumBus` — the kit's shared dynamics + FX bus. "Glue is the headline"
//! (design §5.3): the whole kit is processed as a unit so it hits like one
//! instrument.
//!
//! Signal flow (design §5.3 order, true-peak limiter LAST):
//!   kit sum → bus drive → comp (sidechain PUMP → SSL glue) → tape/stereo DELAY
//!            → plate REVERB send → true-peak limiter
//!
//! - **PUMP** (M7) — `Dynamics::set_pump` (`IntKick`, beat-synced quarter-note
//!   duck): the Daft-Punk/French-house breathe. `pump_envelope()` drives the GUI
//!   duck meter.
//! - **DRIVE** (M7) — lo-fi bus saturation.
//! - **DELAY + REVERB** (M7 pt2) — the "drive & space" the kit was missing; both
//!   tempo-aware, and a true bypass (dry passthrough) at amount 0.
//! - **glue + true-peak limiter** (M3).
//!
//! At the Neutral defaults (pump/drive/reverb/delay all 0) the chain is a pure
//! passthrough into glue → limiter, so the golden anchors are unaffected.
//! Transient shaper, parallel/NY comp and the snare gated-verb send join next.

use synth_core::{Delay, DelayMode, Drive, DriveKind, Dynamics, LimiterStyle, PumpSource, Reverb, ReverbAlgo};

pub struct DrumBus {
    drive_l: Drive,
    drive_r: Drive,
    comp: Dynamics,    // pump + glue (limiter handled separately so FX sit before it)
    delay: Delay,
    reverb: Reverb,
    limiter: Dynamics, // true-peak limiter, last in the chain
    sr: f32,
    tempo: f32,
    pump: f32,
    drive: f32,
    reverb_amt: f32,
    delay_amt: f32,
}

impl DrumBus {
    pub fn neutral(sr: f32) -> Self {
        let mut bus = Self {
            drive_l: Drive::new(sr),
            drive_r: Drive::new(sr),
            comp: Dynamics::new(sr),
            delay: Delay::new(sr),
            reverb: Reverb::new(sr),
            limiter: Dynamics::new(sr),
            sr,
            tempo: 120.0,
            pump: 0.0,
            drive: 0.0,
            reverb_amt: 0.0,
            delay_amt: 0.0,
        };
        bus.configure();
        bus
    }

    fn configure(&mut self) {
        // comp stage: SSL glue (+ pump), NO limiter here.
        self.comp.set_glue(true, -18.0, 2.0, 3.0, 1.0);
        self.comp.set_limiter(false, -0.3, 0.05, LimiterStyle::Transparent);
        self.comp.set_tempo(self.tempo);
        // final stage: true-peak limiter only.
        self.limiter.set_glue(false, -18.0, 2.0, 3.0, 1.0);
        self.limiter.set_limiter(true, -0.3, 0.05, LimiterStyle::Transparent);
        self.apply_pump();
        self.apply_drive();
        self.apply_reverb();
        self.apply_delay();
    }

    fn apply_pump(&mut self) {
        let division = 60.0 / self.tempo.max(1.0); // quarter-note duck
        self.comp
            .set_pump(self.pump, PumpSource::IntKick, division, 0.5, 0.0);
    }

    fn apply_drive(&mut self) {
        let on = self.drive > 0.001;
        self.drive_l
            .set_params(on, DriveKind::Tube, self.drive, 0.5, 16.0, 1.0, 0.0, 1.0);
        self.drive_r
            .set_params(on, DriveKind::Tube, self.drive, 0.5, 16.0, 1.0, 0.0, 1.0);
    }

    fn apply_reverb(&mut self) {
        let on = self.reverb_amt > 0.001;
        // a bright EMT-style plate sized for drums; send level is the macro.
        self.reverb.set_params(
            on,
            ReverbAlgo::Plate224,
            1.6,    // decay s
            0.5,    // size
            0.0,    // predelay
            0.4,    // damping
            150.0,  // locut
            8500.0, // hicut
            0.2,    // modulation
            1.0,    // width
            false,  // freeze
        );
        self.reverb.set_send(self.reverb_amt, 1.0);
    }

    fn apply_delay(&mut self) {
        let on = self.delay_amt > 0.001;
        self.delay.set_tempo(self.tempo);
        // 1/8 on the left, dotted-ish on the right for a wide tempo-synced echo.
        let eighth = (60.0 / self.tempo.max(1.0)) * 0.5;
        self.delay.set_params(
            on,
            DelayMode::Tape,
            eighth,         // time L
            eighth * 0.75,  // time R
            0.35,           // feedback
            0.2,            // crossfeed (ping-pong)
            0.25,           // wow
            0.35,           // age
            180.0,          // hpf
            6500.0,         // lpf
            self.delay_amt, // mix
        );
    }

    pub fn set_tempo(&mut self, bpm: f32) {
        self.tempo = bpm.max(1.0);
        self.comp.set_tempo(self.tempo);
        self.apply_pump();
        self.apply_delay();
    }

    pub fn set_pump(&mut self, amount: f32) {
        self.pump = amount.clamp(0.0, 1.0);
        self.apply_pump();
    }

    pub fn set_drive(&mut self, amount: f32) {
        self.drive = amount.clamp(0.0, 1.0);
        self.apply_drive();
    }

    pub fn set_reverb(&mut self, amount: f32) {
        self.reverb_amt = amount.clamp(0.0, 1.0);
        self.apply_reverb();
    }

    pub fn set_delay(&mut self, amount: f32) {
        self.delay_amt = amount.clamp(0.0, 1.0);
        self.apply_delay();
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.drive_l = Drive::new(sr);
        self.drive_r = Drive::new(sr);
        self.comp = Dynamics::new(sr);
        self.delay = Delay::new(sr);
        self.reverb = Reverb::new(sr);
        self.limiter = Dynamics::new(sr);
        self.configure();
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let l = self.drive_l.process(l);
        let r = self.drive_r.process(r);
        let (l, r) = self.comp.process(l, r); // pump + glue
        let (l, r) = self.delay.process(l, r); // tape delay (bypass at 0)
        let (l, r) = self.reverb.process_send(l, r); // plate reverb (dry at 0)
        self.limiter.process(l, r) // true-peak limiter, last
    }

    pub fn pump_envelope(&self) -> f32 {
        self.comp.pump_envelope()
    }

    pub fn gain_reduction_db(&self) -> f32 {
        self.comp.gain_reduction_db()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut min_env = 1.0_f32;
        let mut max_env = 0.0_f32;
        for i in 0..96_000 {
            let s = 0.3 * (i as f32 * 0.02).sin();
            bus.process(s, s);
            let e = bus.pump_envelope();
            min_env = min_env.min(e);
            max_env = max_env.max(e);
        }
        assert!(min_env < 0.8, "pump should duck the bus, min={min_env}");
        assert!(max_env > 0.95, "and recover between ducks, max={max_env}");
    }

    #[test]
    fn reverb_adds_a_tail() {
        // an impulse into a reverbed bus should still be ringing well after it.
        let tail_energy = |amount: f32| {
            let mut bus = DrumBus::neutral(48_000.0);
            bus.set_reverb(amount);
            bus.process(1.0, 1.0); // impulse
            let mut e = 0.0_f32;
            for i in 0..24_000 {
                let (l, r) = bus.process(0.0, 0.0);
                if i > 4_000 {
                    e += l.abs() + r.abs();
                }
            }
            e
        };
        assert!(tail_energy(0.7) > tail_energy(0.0) + 0.05, "reverb should add a tail");
    }

    #[test]
    fn delay_adds_repeats() {
        let tail_energy = |amount: f32| {
            let mut bus = DrumBus::neutral(48_000.0);
            bus.set_tempo(120.0);
            bus.set_delay(amount);
            bus.process(1.0, 1.0); // impulse
            let mut e = 0.0_f32;
            for i in 0..48_000 {
                let (l, r) = bus.process(0.0, 0.0);
                if i > 4_000 {
                    e += l.abs() + r.abs();
                }
            }
            e
        };
        assert!(tail_energy(0.7) > tail_energy(0.0) + 0.05, "delay should add repeats");
    }

    #[test]
    fn fully_dry_at_default_is_finite() {
        let mut bus = DrumBus::neutral(48_000.0);
        let mut peak = 0.0_f32;
        for i in 0..4_000 {
            let s = 0.3 * (i as f32 * 0.05).sin();
            let (l, _) = bus.process(s, s);
            assert!(l.is_finite());
            peak = peak.max(l.abs());
        }
        assert!(peak > 0.05);
    }
}
