//! `DrumBus` — the kit's shared dynamics + FX bus. "Glue is the headline"
//! (design §5.3): the whole kit is processed as a unit so it hits like one
//! instrument.
//!
//! Signal flow (design §5.3 order, true-peak limiter LAST):
//!   kit sum → transient PUNCH → bus drive → comp (sidechain PUMP → SSL glue)
//!            → PARALLEL/NY comp → tape/stereo DELAY → plate REVERB → limiter
//!
//! Two intentional deviations from §5.3's numbering: the true-peak limiter is
//! moved to genuinely LAST (so delay/reverb sit before it), and PUMP is fused
//! with glue into the single `comp` stage and therefore precedes PARALLEL —
//! whereas §5.3 numbers pump (#5) after parallel (#4). Fusing pump+glue is the
//! conventional bus-comp gesture, and having the parallel/NY stage squash the
//! already-pumped signal is intended.
//!
//! - **PUNCH** (M7 pt3) — `TransientShaper` at the head of the chain: attack
//!   emphasis on the summed kit. True bypass at amount 0.
//! - **PUMP** (M7) — `Dynamics::set_pump` (`IntKick`, beat-synced duck): the
//!   Daft-Punk/French-house breathe. `pump_envelope()` drives the GUI duck meter.
//!   M7 pt3 exposes its **rate** (note division) and **curve** (duck shape).
//! - **DRIVE** (M7) — lo-fi bus saturation.
//! - **PARALLEL/NY comp** (M7 pt3) — a hard-squashed `Dynamics` added back under
//!   the dry bus for density; the true-peak limiter (last) still holds the ceiling.
//! - **DELAY + REVERB** (M7 pt2) — the "drive & space" the kit was missing; both
//!   tempo-aware, and a true bypass (dry passthrough) at amount 0.
//! - **glue + true-peak limiter** (M3).
//!
//! At the Neutral defaults (punch/pump/drive/reverb/delay/parallel all 0, and
//! pump rate/curve at their factory centers reproducing the original quarter-
//! note, 0.5-curve duck) the chain is a pure passthrough into glue → limiter, so
//! the golden anchors are unaffected.
//!
//! Deferred to **M8**: the snare **gated-verb** send, which needs per-voice send
//! routing (the snare tapped separately into the reverb, gated on its return)
//! that does not exist while the kit sums into one bus — it pairs with M8's
//! Send A infrastructure and a new return-gate primitive.

use synth_core::{
    Delay, DelayMode, Drive, DriveKind, Dynamics, LimiterStyle, PumpSource, Reverb, ReverbAlgo,
    TransientShaper,
};

/// Pump **rate** divisions as quarter-note (beat) multiples: 1/1, 1/2, 1/4, 1/8,
/// 1/16. The normalized macro quantizes into this table; the factory center
/// (0.5) selects 1/4 — the original hardcoded duck — so the pump sound is
/// unchanged by default.
const PUMP_RATE_BEATS: [f32; 5] = [4.0, 2.0, 1.0, 0.5, 0.25];

fn pump_rate_beats(norm: f32) -> f32 {
    let n = PUMP_RATE_BEATS.len();
    let idx = ((norm.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
    PUMP_RATE_BEATS[idx]
}

pub struct DrumBus {
    transient_l: TransientShaper, // PUNCH, at the head of the chain
    transient_r: TransientShaper,
    drive_l: Drive,
    drive_r: Drive,
    comp: Dynamics,     // pump + glue (limiter handled separately so FX sit before it)
    parallel: Dynamics, // NY/parallel comp, blended under the dry bus
    delay: Delay,
    reverb: Reverb,
    limiter: Dynamics, // true-peak limiter, last in the chain
    sr: f32,
    tempo: f32,
    pump: f32,
    pump_rate: f32,  // normalized -> PUMP_RATE_BEATS division
    pump_curve: f32, // duck shape, 0..1
    drive: f32,
    transient_amt: f32, // PUNCH, 0..1 -> attack emphasis
    parallel_amt: f32,  // 0 = dry, 1 = full NY blend added under the bus
    reverb_amt: f32,    // SPACE: global send-to-all into the reverb return
    delay_amt: f32,     // DELAY: global send-to-all into the delay return
    send_a_active: bool, // any voice routes to the reverb send
    send_b_active: bool, // any voice routes to the delay send
}

impl DrumBus {
    pub fn neutral(sr: f32) -> Self {
        let mut bus = Self {
            transient_l: TransientShaper::new(sr),
            transient_r: TransientShaper::new(sr),
            drive_l: Drive::new(sr),
            drive_r: Drive::new(sr),
            comp: Dynamics::new(sr),
            parallel: Dynamics::new(sr),
            delay: Delay::new(sr),
            reverb: Reverb::new(sr),
            limiter: Dynamics::new(sr),
            sr,
            tempo: 120.0,
            pump: 0.0,
            pump_rate: 0.5,  // 1/4 note — the original duck
            pump_curve: 0.5, // the original duck shape
            drive: 0.0,
            transient_amt: 0.0,
            parallel_amt: 0.0,
            reverb_amt: 0.0,
            delay_amt: 0.0,
            send_a_active: false,
            send_b_active: false,
        };
        bus.configure();
        bus
    }

    fn configure(&mut self) {
        // comp stage: SSL glue (+ pump), NO limiter here.
        self.comp.set_glue(true, -18.0, 2.0, 3.0, 1.0);
        self.comp.set_limiter(false, -0.3, 0.05, LimiterStyle::Transparent);
        self.comp.set_tempo(self.tempo);
        // parallel/NY stage: hard squash, no pump, no limiter. Its OUTPUT is
        // added under the dry bus (scaled by `parallel_amt`), so its makeup is
        // baked in here and the blend amount is the macro.
        self.parallel.set_glue(true, -30.0, 8.0, 6.0, 1.0);
        self.parallel.set_pump(0.0, PumpSource::IntKick, 0.5, 0.5, 0.0);
        self.parallel.set_limiter(false, -0.3, 0.05, LimiterStyle::Transparent);
        // final stage: true-peak limiter only.
        self.limiter.set_glue(false, -18.0, 2.0, 3.0, 1.0);
        self.limiter.set_limiter(true, -0.3, 0.05, LimiterStyle::Transparent);
        self.apply_pump();
        self.apply_drive();
        self.apply_transient();
        self.apply_reverb();
        self.apply_delay();
    }

    fn apply_transient(&mut self) {
        // PUNCH macro drives the attack only (bus use); 0 = true bypass.
        self.transient_l.set_params(self.transient_amt, 0.0);
        self.transient_r.set_params(self.transient_amt, 0.0);
    }

    fn apply_pump(&mut self) {
        // rate -> note division (factory center = 1/4); curve -> duck shape.
        let beats = pump_rate_beats(self.pump_rate);
        let division = (60.0 / self.tempo.max(1.0)) * beats;
        self.comp
            .set_pump(self.pump, PumpSource::IntKick, division, self.pump_curve, 0.0);
    }

    fn apply_drive(&mut self) {
        let on = self.drive > 0.001;
        self.drive_l
            .set_params(on, DriveKind::Tube, self.drive, 0.5, 16.0, 1.0, 0.0, 1.0);
        self.drive_r
            .set_params(on, DriveKind::Tube, self.drive, 0.5, 16.0, 1.0, 0.0, 1.0);
    }

    /// Is the reverb send engaged (global SPACE knob up, or any voice sends to it)?
    fn reverb_engaged(&self) -> bool {
        self.reverb_amt > 0.001 || self.send_a_active
    }

    /// Is the delay send engaged?
    fn delay_engaged(&self) -> bool {
        self.delay_amt > 0.001 || self.send_b_active
    }

    fn apply_reverb(&mut self) {
        // Reverb is a wet-only SEND return now (process_wet_send): on whenever
        // engaged, fed a send sum upstream, return at unity. The per-voice Send A
        // and the global SPACE amount scale the input, not this.
        self.reverb.set_params(
            self.reverb_engaged(),
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
        self.reverb.set_send(1.0, 1.0);
    }

    fn apply_delay(&mut self) {
        self.delay.set_tempo(self.tempo);
        // 1/8 on the left, dotted-ish on the right for a wide tempo-synced echo.
        let eighth = (60.0 / self.tempo.max(1.0)) * 0.5;
        // mix = 1.0 -> the delay returns pure wet (it's a send, not an insert).
        self.delay.set_params(
            self.delay_engaged(),
            DelayMode::Tape,
            eighth,        // time L
            eighth * 0.75, // time R
            0.35,          // feedback
            0.2,           // crossfeed (ping-pong)
            0.25,          // wow
            0.35,          // age
            180.0,         // hpf
            6500.0,        // lpf
            1.0,           // mix (pure wet return)
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

    /// Pump rate (normalized), quantized to a note division (1/1..1/16).
    pub fn set_pump_rate(&mut self, norm: f32) {
        self.pump_rate = norm.clamp(0.0, 1.0);
        self.apply_pump();
    }

    /// Pump duck curve/shape, 0..1.
    pub fn set_pump_curve(&mut self, curve: f32) {
        self.pump_curve = curve.clamp(0.0, 1.0);
        self.apply_pump();
    }

    /// Transient PUNCH (attack emphasis), 0..1 (0 = true bypass).
    pub fn set_transient(&mut self, amount: f32) {
        self.transient_amt = amount.clamp(0.0, 1.0);
        self.apply_transient();
    }

    /// Parallel/NY compression blend, 0..1 (0 = dry passthrough).
    pub fn set_parallel(&mut self, amount: f32) {
        self.parallel_amt = amount.clamp(0.0, 1.0);
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

    /// Tell the bus whether any voice routes to the reverb (`a`) / delay (`b`)
    /// send, so the returns run (and tail) whenever a send is in use — not just
    /// when the global SPACE/DELAY knobs are up.
    pub fn set_send_active(&mut self, a: bool, b: bool) {
        self.send_a_active = a;
        self.send_b_active = b;
        self.apply_reverb();
        self.apply_delay();
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.transient_l = TransientShaper::new(sr);
        self.transient_r = TransientShaper::new(sr);
        self.drive_l = Drive::new(sr);
        self.drive_r = Drive::new(sr);
        self.comp = Dynamics::new(sr);
        self.parallel = Dynamics::new(sr);
        self.delay = Delay::new(sr);
        self.reverb = Reverb::new(sr);
        self.limiter = Dynamics::new(sr);
        self.configure();
    }

    /// Process the dry kit sum with no sends (e.g. a bare bus / tests).
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.process_with_sends(l, r, 0.0, 0.0, 0.0, 0.0)
    }

    /// Process the dry kit sum plus the per-voice **send sums** (post-fader) for
    /// the reverb (Send A) and delay (Send B). The dry chain (transient → drive
    /// → comp/pump → parallel) is unchanged; the reverb/delay are now parallel
    /// **wet returns** (not inserts), mixed in just before the limiter, which
    /// stays strictly last. Each return also receives a global send scaled by the
    /// SPACE/DELAY knobs (`reverb_amt`/`delay_amt`) tapped off the dry bus.
    ///
    /// At the default (all sends 0, SPACE/DELAY 0) both returns are skipped, so
    /// the output is `limiter(parallel_out)` — bit-identical to the M7 dry path.
    pub fn process_with_sends(
        &mut self,
        dry_l: f32,
        dry_r: f32,
        send_a_l: f32,
        send_a_r: f32,
        send_b_l: f32,
        send_b_r: f32,
    ) -> (f32, f32) {
        // --- dry chain (unchanged from M7) ---
        let l = self.transient_l.process(dry_l); // PUNCH (bypass at 0)
        let r = self.transient_r.process(dry_r);
        let l = self.drive_l.process(l);
        let r = self.drive_r.process(r);
        let (l, r) = self.comp.process(l, r); // pump + glue
        // Parallel/NY comp: add the hard-squashed signal under the dry bus.
        let (dl, dr) = if self.parallel_amt > 0.001 {
            let (pl, pr) = self.parallel.process(l, r);
            (l + pl * self.parallel_amt, r + pr * self.parallel_amt)
        } else {
            (l, r)
        };

        let mut out_l = dl;
        let mut out_r = dr;
        // --- delay send return (Send B + global DELAY) ---
        if self.delay_engaged() {
            let in_l = send_b_l + dl * self.delay_amt;
            let in_r = send_b_r + dr * self.delay_amt;
            let (wet_l, wet_r) = self.delay.process(in_l, in_r); // mix=1 -> pure wet
            out_l += wet_l;
            out_r += wet_r;
        }
        // --- reverb send return (Send A + global SPACE) ---
        if self.reverb_engaged() {
            let in_l = send_a_l + dl * self.reverb_amt;
            let in_r = send_a_r + dr * self.reverb_amt;
            let (wet_l, wet_r) = self.reverb.process_wet_send(in_l, in_r);
            out_l += wet_l;
            out_r += wet_r;
        }

        self.limiter.process(out_l, out_r) // true-peak limiter, LAST
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
    fn transient_punch_is_dry_at_zero_and_active_when_engaged() {
        // At 0 the head-of-chain shaper must be exact-dry vs a bus that never
        // engaged it (the golden-safety guard); engaging PUNCH changes the bus
        // output. (The peak-emphasis itself is verified in synth_core's
        // TransientShaper unit tests — here the glue+limiter after it would mask
        // a peak delta, so we assert the stage is genuinely doing work.)
        let diff_vs_dry = |amount: f32| {
            let mut bus = DrumBus::neutral(48_000.0);
            bus.set_transient(amount);
            let mut dry = DrumBus::neutral(48_000.0);
            let mut diff = 0.0_f32;
            for i in 0..2_000 {
                let env = (-(i as f32) / 48_000.0 * 22.0).exp();
                let s = 0.5 * env * (i as f32 * 0.12).sin();
                let (l, _) = bus.process(s, s);
                let (dl, _) = dry.process(s, s);
                diff += (l - dl).abs();
            }
            diff
        };
        assert_eq!(diff_vs_dry(0.0), 0.0, "transient at 0 must be exact dry passthrough");
        assert!(diff_vs_dry(0.9) > 0.001, "engaging PUNCH must change the bus output");
    }

    #[test]
    fn parallel_comp_is_dry_at_zero_and_denser_when_engaged() {
        // At amount 0 the bus output must be byte-identical to a bus that never
        // had the parallel stage (exact-dry guard); engaging it adds energy.
        let drive_sum = |amount: f32| {
            let mut bus = DrumBus::neutral(48_000.0);
            bus.set_parallel(amount);
            let mut dry = DrumBus::neutral(48_000.0); // parallel stays 0
            let mut diff = 0.0_f32;
            let mut energy = 0.0_f32;
            for i in 0..8_000 {
                let s = 0.25 * (i as f32 * 0.03).sin();
                let (l, _) = bus.process(s, s);
                let (dl, _) = dry.process(s, s);
                diff += (l - dl).abs();
                energy += l.abs();
            }
            (diff, energy)
        };
        let (diff0, e0) = drive_sum(0.0);
        assert_eq!(diff0, 0.0, "parallel at 0 must be exact dry passthrough");
        let (_, e_on) = drive_sum(1.0);
        assert!(e_on > e0 * 1.02, "engaging parallel comp should add density");
    }

    #[test]
    fn parallel_comp_still_respects_the_limiter() {
        let mut bus = DrumBus::neutral(48_000.0);
        bus.set_parallel(1.0);
        let mut peak = 0.0_f32;
        for i in 0..8_000 {
            let s = 3.0 * (i as f32 * 0.1).sin();
            let (l, r) = bus.process(s, s);
            if i > 1_000 {
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        assert!(peak <= 1.02, "limiter must still hold the ceiling with parallel comp, peak={peak}");
    }

    #[test]
    fn pump_rate_factory_center_reproduces_the_quarter_note_duck() {
        // The factory center (0.5) must select the 1/4-note division so existing
        // pump settings sound identical — verified via the rate table.
        assert_eq!(pump_rate_beats(0.5), 1.0, "center rate is a quarter note");
        assert_eq!(pump_rate_beats(0.0), 4.0, "min rate is 1/1");
        assert_eq!(pump_rate_beats(1.0), 0.25, "max rate is 1/16");
    }

    #[test]
    fn faster_pump_rate_ducks_more_often() {
        let duck_count = |rate: f32| {
            let mut bus = DrumBus::neutral(48_000.0);
            bus.set_tempo(120.0);
            bus.set_pump(0.9);
            bus.set_pump_rate(rate);
            let mut crossings = 0;
            let mut prev = 1.0_f32;
            for _ in 0..96_000 {
                bus.process(0.0, 0.0);
                let e = bus.pump_envelope();
                if prev >= 0.85 && e < 0.85 {
                    crossings += 1;
                }
                prev = e;
            }
            crossings
        };
        assert!(
            duck_count(1.0) > duck_count(0.5),
            "a 1/16 rate must duck more often than 1/4 over the same span"
        );
    }

    #[test]
    fn per_voice_send_rings_without_the_global_knob() {
        // A per-voice Send A (reverb) must engage the return and tail even with
        // the global SPACE knob at 0 — and the dry path must stay clean.
        let mut bus = DrumBus::neutral(48_000.0);
        bus.set_send_active(true, false);
        bus.process_with_sends(0.0, 0.0, 1.0, 1.0, 0.0, 0.0); // impulse into the send only
        let mut tail = 0.0_f32;
        for _ in 0..12_000 {
            let (l, r) = bus.process_with_sends(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            tail += l.abs() + r.abs();
        }
        assert!(tail > 0.01, "per-voice reverb send must ring without the global SPACE knob");
    }

    #[test]
    fn zero_sends_match_the_bare_process() {
        // process_with_sends(x,y,0,0,0,0) is exactly process(x,y) — the dry path
        // is untouched by the send architecture when nothing is routed.
        let mut a = DrumBus::neutral(48_000.0);
        let mut b = DrumBus::neutral(48_000.0);
        for i in 0..4_000 {
            let s = 0.4 * (i as f32 * 0.05).sin();
            let pa = a.process(s, s);
            let pb = b.process_with_sends(s, s, 0.0, 0.0, 0.0, 0.0);
            assert_eq!(pa, pb, "zero-send path must equal the bare dry process");
        }
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
