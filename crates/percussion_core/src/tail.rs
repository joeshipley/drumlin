//! `VoiceTail` — the uniform per-voice post-engine chain (design §3.1/§6.3):
//! optional saturation → CS-80 dual filter (resonant HP → LP) → level → pan.
//! Every track runs through one so the UI and mod matrix stay regular. The
//! lockable tail params (Level/Pan/Cutoff/Resonance/Drive) are user-editable
//! per-track defaults via the VOICE editor (M8); per-voice pitch/decay trims are
//! not lockable and arrive with the mod infrastructure (M6), and per-voice
//! reverb/delay Sends land with Send A/B (M8). Each channel has independent
//! filter/drive state so a stereo voice (the clap) keeps its width.

use synth_core::{Drive, DriveKind, Filter, ShelfEq};

/// Trim-EQ shelf corner frequencies (Hz): a low shelf for body/boom and a high
/// shelf for air/crispness.
const EQ_LOW_HZ: f32 = 150.0;
const EQ_HIGH_HZ: f32 = 4_000.0;

pub struct VoiceTail {
    sr: f32,
    hp_l: Filter,
    hp_r: Filter,
    lp_l: Filter,
    lp_r: Filter,
    drive_l: Drive,
    drive_r: Drive,
    eq_l: ShelfEq,
    eq_r: ShelfEq,
    // stored config so a sample-rate change can re-apply it
    hp_hz: f32,
    lp_hz: f32,
    res: f32,
    drive_kind: DriveKind,
    drive_amt: f32,
    drive_tone: f32,
    eq_low_db: f32,
    eq_high_db: f32,
    level: f32,
    pan: f32,
    filter_on: bool,
    drive_on: bool,
}

impl VoiceTail {
    pub fn new(sr: f32) -> Self {
        let mut t = Self {
            sr,
            hp_l: Filter::new(sr),
            hp_r: Filter::new(sr),
            lp_l: Filter::new(sr),
            lp_r: Filter::new(sr),
            drive_l: Drive::new(sr),
            drive_r: Drive::new(sr),
            eq_l: ShelfEq::new(sr),
            eq_r: ShelfEq::new(sr),
            hp_hz: 20.0,
            lp_hz: 20_000.0,
            res: 0.0,
            drive_kind: DriveKind::Tube,
            drive_amt: 0.0,
            drive_tone: 0.5,
            eq_low_db: 0.0,
            eq_high_db: 0.0,
            level: 1.0,
            pan: 0.0,
            filter_on: true,
            drive_on: false,
        };
        t.apply();
        t
    }

    fn apply(&mut self) {
        self.hp_l.set_cutoff(self.hp_hz);
        self.hp_r.set_cutoff(self.hp_hz);
        self.lp_l.set_cutoff(self.lp_hz);
        self.lp_r.set_cutoff(self.lp_hz);
        self.hp_l.set_resonance(self.res);
        self.hp_r.set_resonance(self.res);
        self.lp_l.set_resonance(self.res);
        self.lp_r.set_resonance(self.res);
        self.drive_l
            .set_params(self.drive_on, self.drive_kind, self.drive_amt, self.drive_tone, 16.0, 1.0, 0.0, 1.0);
        self.drive_r
            .set_params(self.drive_on, self.drive_kind, self.drive_amt, self.drive_tone, 16.0, 1.0, 0.0, 1.0);
        self.eq_l.set_params(EQ_LOW_HZ, self.eq_low_db, EQ_HIGH_HZ, self.eq_high_db);
        self.eq_r.set_params(EQ_LOW_HZ, self.eq_low_db, EQ_HIGH_HZ, self.eq_high_db);
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.hp_l.set_sample_rate(sr);
        self.hp_r.set_sample_rate(sr);
        self.lp_l.set_sample_rate(sr);
        self.lp_r.set_sample_rate(sr);
        self.drive_l.set_sample_rate(sr);
        self.drive_r.set_sample_rate(sr);
        self.eq_l.set_sample_rate(sr);
        self.eq_r.set_sample_rate(sr);
        self.apply();
    }

    /// 2-band trim EQ: low/high shelf gains in dB (0 dB = flat = bypass).
    pub fn set_eq(&mut self, low_db: f32, high_db: f32) {
        self.eq_low_db = low_db;
        self.eq_high_db = high_db;
        self.eq_l.set_params(EQ_LOW_HZ, low_db, EQ_HIGH_HZ, high_db);
        self.eq_r.set_params(EQ_LOW_HZ, low_db, EQ_HIGH_HZ, high_db);
    }

    pub fn set_level(&mut self, level: f32) {
        self.level = level.max(0.0);
    }

    pub fn set_pan(&mut self, pan: f32) {
        self.pan = pan.clamp(-1.0, 1.0);
    }

    pub fn set_filter(&mut self, on: bool, hp_hz: f32, lp_hz: f32, res: f32) {
        self.filter_on = on;
        self.hp_hz = hp_hz;
        self.lp_hz = lp_hz;
        self.res = res;
        self.apply();
    }

    pub fn set_drive(&mut self, on: bool, kind: DriveKind, amount: f32, tone: f32) {
        self.drive_on = on;
        self.drive_kind = kind;
        self.drive_amt = amount;
        self.drive_tone = tone;
        self.apply();
    }

    // --- granular getters/setters for per-step parameter locks (M5) ---
    pub fn level(&self) -> f32 {
        self.level
    }
    pub fn pan(&self) -> f32 {
        self.pan
    }
    pub fn lp_cutoff(&self) -> f32 {
        self.lp_hz
    }
    pub fn resonance(&self) -> f32 {
        self.res
    }
    pub fn drive_amount(&self) -> f32 {
        self.drive_amt
    }
    pub fn drive_on(&self) -> bool {
        self.drive_on
    }

    pub fn set_lp_cutoff(&mut self, hz: f32) {
        self.lp_hz = hz;
        self.lp_l.set_cutoff(hz);
        self.lp_r.set_cutoff(hz);
    }

    pub fn set_resonance(&mut self, res: f32) {
        self.res = res;
        self.hp_l.set_resonance(res);
        self.hp_r.set_resonance(res);
        self.lp_l.set_resonance(res);
        self.lp_r.set_resonance(res);
    }

    pub fn set_drive_amount(&mut self, on: bool, amount: f32) {
        self.drive_on = on;
        self.drive_amt = amount;
        self.drive_l
            .set_params(on, self.drive_kind, amount, self.drive_tone, 16.0, 1.0, 0.0, 1.0);
        self.drive_r
            .set_params(on, self.drive_kind, amount, self.drive_tone, 16.0, 1.0, 0.0, 1.0);
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let mut l = l;
        let mut r = r;
        if self.drive_on {
            l = self.drive_l.process(l);
            r = self.drive_r.process(r);
        }
        if self.filter_on {
            l = self.lp_l.process(self.hp_l.process_high(l));
            r = self.lp_r.process(self.hp_r.process_high(r));
        }
        // Trim EQ (bypasses internally when flat -> exact passthrough).
        l = self.eq_l.process(l);
        r = self.eq_r.process(r);
        let (gl, gr) = pan_gains(self.pan);
        (l * self.level * gl, r * self.level * gr)
    }

    pub fn reset(&mut self) {
        self.hp_l.reset();
        self.hp_r.reset();
        self.lp_l.reset();
        self.lp_r.reset();
        self.eq_l.reset();
        self.eq_r.reset();
        // Drive has no reset() in synth_core (read-only); set_sample_rate is its
        // documented state-clear (tone filter + oversampler memory), leaving
        // params untouched — otherwise the previous hit's residue colors the
        // first samples after a panic reset / KIT recall.
        self.drive_l.set_sample_rate(self.sr);
        self.drive_r.set_sample_rate(self.sr);
    }
}

/// "Full-both-at-center" pan law: at center a mono voice keeps its full level on
/// both channels (no -3 dB center dip); panning fades the opposite channel
/// linearly to silence.
fn pan_gains(pan: f32) -> (f32, f32) {
    let gl = (1.0 - pan).clamp(0.0, 1.0);
    let gr = (1.0 + pan).clamp(0.0, 1.0);
    (gl, gr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a ~760 Hz passband sine through the tail and return the settled peak
    /// (the default HP blocks DC and the filter smoother needs a few ms).
    fn settled_peak(level: f32) -> f32 {
        let mut t = VoiceTail::new(48_000.0);
        t.set_level(level);
        let mut peak = 0.0_f32;
        for i in 0..4_000 {
            let s = 0.5 * (i as f32 * 0.1).sin();
            let (l, r) = t.process(s, s);
            assert!(l.is_finite() && r.is_finite());
            if i > 500 {
                peak = peak.max(l.abs());
            }
        }
        peak
    }

    #[test]
    fn transparent_default_passes_passband_signal() {
        assert!(settled_peak(1.0) > 0.2, "passband signal should pass the transparent tail");
    }

    #[test]
    fn pan_law_is_full_at_center_and_fades_one_side() {
        let center = pan_gains(0.0);
        assert_eq!(center, (1.0, 1.0));
        let hard_l = pan_gains(-1.0);
        assert_eq!(hard_l, (1.0, 0.0));
        let hard_r = pan_gains(1.0);
        assert_eq!(hard_r, (0.0, 1.0));
    }

    #[test]
    fn level_scales_output() {
        let full = settled_peak(1.0);
        let half = settled_peak(0.5);
        assert!(half < full * 0.6, "level=0.5 should roughly halve output: half={half} full={full}");
        assert!(half > full * 0.4, "level=0.5 should not collapse output: half={half} full={full}");
    }
}
