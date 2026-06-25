//! The master **dynamics** section: three classic processors in one stereo box.
//!
//! Signal flows through three internal stages, always in this fixed order:
//!
//! ```text
//!     in -> [ 1. PUMP ] -> [ 2. GLUE comp ] -> [ 3. true-peak LIMITER ] -> out
//! ```
//!
//! 1. **PUMP** — a tempo-locked "sidechain" ducking envelope. This is the
//!    French-touch / Daft-Punk movement: on every musical division the whole
//!    bus is ducked down and then swells back up, so the pad "breathes" in time
//!    even though there is no real kick drum routed in. It is purely a gain
//!    envelope generator clocked off the host tempo — no detector, no trigger
//!    input — so it is dead simple and rock-solid. Its current value is exposed
//!    via [`Dynamics::pump_envelope`] so it can later double as a mod source.
//!
//! 2. **GLUE** — a gentle, program-dependent stereo bus compressor in the
//!    spirit of the Alesis 3630 / SSL bus comp: a hybrid peak/RMS detector on
//!    the stereo-summed level, a soft knee, a fixed musical attack, an
//!    auto-release that follows the program material, makeup gain, and a
//!    parallel "mix" so you can blend in the uncompressed signal (New-York
//!    style). One shared gain is applied to both channels so the stereo image
//!    never wanders.
//!
//! 3. **LIMITER** — the critical safety net. The synth's filter can self-
//!    resonate to enormous gains, and stacking that across a 16-voice chord
//!    (plus drive) can spike the bus far past 0 dBFS. This is a **look-ahead,
//!    true-peak brickwall limiter**: it upsamples 4x to *see* inter-sample peaks
//!    the raw samples miss, delays the audio by a short look-ahead window so the
//!    gain reduction is applied slightly *before* a transient arrives, and
//!    guarantees the output true-peak never exceeds the ceiling. A final hard
//!    clamp to the ceiling catches anything the smooth envelope somehow missed,
//!    so the output is *unconditionally* bounded — that is what makes the
//!    up-to-+40 dB filter resonance safe to ship.
//!
//! ## Real-time safety
//!
//! Every buffer (the limiter's per-channel look-ahead rings) is allocated once
//! in [`Dynamics::new`]. [`Dynamics::process`] only reads, writes, and does
//! float math — no allocation, no locks. All inputs are clamped and NaN-folded
//! so no automation value, however wild, can poison the state with a NaN.

/// What clocks the pump's ducking envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpSource {
    /// A synthesized internal "kick" transient defines the duck onset: the duck
    /// fires with a fast attack like a kick hitting the sidechain.
    IntKick,
    /// Trigger-less: the duck boundaries are derived purely from the host tempo
    /// and the chosen division (same sample-counter idea as the arpeggiator).
    Tempo,
}

/// The limiter's release character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimiterStyle {
    /// Smooth, slow release — the most transparent, least "pumping" setting.
    Transparent,
    /// Faster, more aggressive release — louder and punchier, lets more of the
    /// transient through at the cost of a little more distortion.
    Punchy,
}

/// Look-ahead window for the limiter, in milliseconds. The gain envelope is
/// applied to audio delayed by this much, so the limiter can start ducking
/// *before* a peak actually arrives at the output — that is what lets a smooth
/// (non-clicky) envelope still catch a fast transient. ~1.5 ms is a good
/// trade-off between transient protection and the slight "dulling" a long
/// look-ahead causes.
const LOOKAHEAD_MS: f32 = 1.5;

/// The maximum look-ahead ring length in samples, sized for the highest sample
/// rate we expect (192 kHz). `1.5 ms * 192 kHz ≈ 288`; we round up generously so
/// `set_sample_rate` never has to grow the buffer (which would allocate).
const MAX_LOOKAHEAD_SAMPLES: usize = 512;

/// 4x oversampling factor for true-peak detection. We estimate the inter-sample
/// peak by reconstructing the signal between the digital samples with a short
/// cubic kernel and taking the max magnitude across the sub-samples. This
/// catches peaks that live *between* the samples, which a naive sample-peak
/// limiter would let through and a downstream DAC would then reconstruct above
/// the ceiling.
const OVERSAMPLE: usize = 4;

/// Number of input-sample taps the true-peak interpolator looks at. A short
/// 4-tap window (one cubic span) is enough for a good inter-sample estimate.
const TP_TAPS: usize = 4;

/// The master dynamics processor: pump -> glue comp -> true-peak limiter.
///
/// Build one at the host sample rate, push the host tempo every block with
/// [`Dynamics::set_tempo`], push the three groups of parameters with
/// [`Dynamics::set_pump`] / [`Dynamics::set_glue`] / [`Dynamics::set_limiter`]
/// (all taking already-mapped engineering units), then call
/// [`Dynamics::process`] once per stereo sample.
#[derive(Clone, Debug)]
pub struct Dynamics {
    sample_rate: f32,
    tempo_bpm: f32,

    // ---------------- PUMP ----------------
    /// Duck depth, 0.0 = no ducking, 1.0 = duck all the way to silence.
    pump_amount: f32,
    pump_source: PumpSource,
    /// Length of one pump cycle in seconds (already resolved from tempo +
    /// division by the plugin).
    pump_division_secs: f32,
    /// Shape of the recovery: 0.0 = linear ramp back up, 1.0 = sharp
    /// exponential "thump" that snaps back quickly then eases.
    pump_curve: f32,
    /// Where in the cycle the duck fires, as a fraction `0.0..1.0` of the cycle.
    pump_phase: f32,
    /// The pump cycle position in `0.0..1.0`, advanced per sample.
    pump_pos: f32,
    /// The pump gain in effect *right now* (1.0 = no duck). Exposed as a getter.
    pump_gain: f32,

    // ---------------- GLUE comp ----------------
    glue_on: bool,
    /// Threshold as a *linear* amplitude (already converted from dB).
    glue_threshold: f32,
    /// Compression ratio (2.0 = 2:1, etc.).
    glue_ratio: f32,
    /// Makeup gain as a *linear* multiplier.
    glue_makeup: f32,
    /// Parallel blend, 0.0 = all dry (uncompressed), 1.0 = all compressed.
    glue_mix: f32,
    /// Smoothed envelope of the detector (a level estimate), in linear amplitude.
    glue_env: f32,
    /// Per-sample attack coefficient (one-pole).
    glue_atk_coeff: f32,
    /// Per-sample release coefficient (one-pole).
    glue_rel_coeff: f32,
    /// The gain reduction the comp applied on the last sample, in dB (<= 0).
    glue_gr_db: f32,

    // ---------------- LIMITER ----------------
    limiter_on: bool,
    /// Output ceiling as a *linear* amplitude (already converted from dBTP).
    limiter_ceiling: f32,
    /// Release time in seconds.
    limiter_release_secs: f32,
    limiter_style: LimiterStyle,
    /// Per-channel look-ahead delay rings (the audio is delayed by these).
    look_l: Vec<f32>,
    look_r: Vec<f32>,
    /// Write position into the look-ahead rings.
    look_pos: usize,
    /// Active look-ahead length in samples (<= ring capacity).
    look_len: usize,
    /// The limiter's smoothed gain-reduction envelope (1.0 = no reduction). This
    /// only ever moves *down* instantly (to catch a peak) and recovers up over
    /// the release time.
    limiter_gain: f32,
    /// Per-sample release coefficient for the limiter envelope.
    limiter_rel_coeff: f32,
    /// History samples for the true-peak (oversampling) estimator, per channel.
    /// We keep the last few input samples so the cubic kernel has context.
    tp_hist_l: [f32; TP_TAPS],
    tp_hist_r: [f32; TP_TAPS],
    /// The gain reduction the limiter applied on the last sample, in dB (<= 0).
    limiter_gr_db: f32,
}

impl Dynamics {
    /// Linear gain (`10^(db/20)`), NaN-folded to a safe finite value. Used for
    /// thresholds, ceilings and makeup so a NaN automation value can never reach
    /// the audio path.
    #[inline]
    fn db_to_gain(db: f32) -> f32 {
        if db.is_nan() {
            return 1.0;
        }
        10.0f32.powf(db / 20.0)
    }

    /// Create a dynamics section at `sample_rate`, with the pump off (so the
    /// default patch is unchanged), the glue comp on and gentle, and the limiter
    /// on at a -0.3 dBTP ceiling (the always-present resonance guard).
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut d = Self {
            sample_rate: sr,
            tempo_bpm: 120.0,

            pump_amount: 0.0,
            pump_source: PumpSource::Tempo,
            pump_division_secs: 0.5, // one beat at 120 bpm
            pump_curve: 0.5,
            pump_phase: 0.0,
            pump_pos: 0.0,
            pump_gain: 1.0,

            glue_on: true,
            glue_threshold: Self::db_to_gain(-18.0),
            glue_ratio: 2.0,
            glue_makeup: 1.0,
            glue_mix: 1.0,
            glue_env: 0.0,
            glue_atk_coeff: 0.0,
            glue_rel_coeff: 0.0,
            glue_gr_db: 0.0,

            limiter_on: true,
            limiter_ceiling: Self::db_to_gain(-0.3),
            limiter_release_secs: 0.05,
            limiter_style: LimiterStyle::Transparent,
            look_l: vec![0.0; MAX_LOOKAHEAD_SAMPLES],
            look_r: vec![0.0; MAX_LOOKAHEAD_SAMPLES],
            look_pos: 0,
            look_len: 1,
            limiter_gain: 1.0,
            limiter_rel_coeff: 0.0,
            tp_hist_l: [0.0; TP_TAPS],
            tp_hist_r: [0.0; TP_TAPS],
            limiter_gr_db: 0.0,
        };
        d.recompute_coeffs();
        d
    }

    /// Change the sample rate. Clears all audio memory (so a rate change can't
    /// replay stale, mis-scaled energy) and recomputes every time-based
    /// coefficient against the new rate. Never allocates: the look-ahead rings
    /// were sized for the highest expected rate in [`Dynamics::new`].
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        for s in &mut self.look_l {
            *s = 0.0;
        }
        for s in &mut self.look_r {
            *s = 0.0;
        }
        self.look_pos = 0;
        self.glue_env = 0.0;
        self.limiter_gain = 1.0;
        self.pump_pos = 0.0;
        self.pump_gain = 1.0;
        self.glue_gr_db = 0.0;
        self.limiter_gr_db = 0.0;
        self.tp_hist_l = [0.0; TP_TAPS];
        self.tp_hist_r = [0.0; TP_TAPS];
        self.recompute_coeffs();
    }

    /// Set the host tempo (BPM), called per block like the arpeggiator. A bogus
    /// 0 tempo is clamped so the pump cycle can never divide by zero.
    pub fn set_tempo(&mut self, bpm: f32) {
        self.tempo_bpm = if bpm.is_nan() { 120.0 } else { bpm.max(1.0) };
    }

    /// Configure the **pump** stage.
    ///
    /// * `amount` — duck depth, `0.0..=1.0` (0 = off, the default).
    /// * `source` — [`PumpSource::Tempo`] (trigger-less) or [`PumpSource::IntKick`].
    /// * `division_secs` — length of one pump cycle in seconds (the plugin
    ///   resolves this from tempo + the chosen note division).
    /// * `curve` — recovery shape, `0.0` linear .. `1.0` sharp exponential thump.
    /// * `phase_deg` — where in the cycle the duck fires, `0..360` degrees.
    pub fn set_pump(
        &mut self,
        amount: f32,
        source: PumpSource,
        division_secs: f32,
        curve: f32,
        phase_deg: f32,
    ) {
        self.pump_amount = clamp01(amount);
        self.pump_source = source;
        // Guard the cycle length: never below one sample's worth of time, and
        // fold NaN/inf to a sane default.
        self.pump_division_secs = if division_secs.is_finite() {
            division_secs.max(1.0 / self.sample_rate)
        } else {
            0.5
        };
        self.pump_curve = clamp01(curve);
        let deg = if phase_deg.is_finite() { phase_deg } else { 0.0 };
        self.pump_phase = (deg / 360.0).rem_euclid(1.0);
    }

    /// Configure the **glue compressor**.
    ///
    /// * `on` — enable/bypass.
    /// * `threshold_db` — where compression begins, in dBFS (e.g. -18).
    /// * `ratio` — compression ratio (2.0 = 2:1).
    /// * `makeup_db` — output makeup gain in dB.
    /// * `mix` — parallel blend, `0.0` dry .. `1.0` fully compressed.
    pub fn set_glue(&mut self, on: bool, threshold_db: f32, ratio: f32, makeup_db: f32, mix: f32) {
        self.glue_on = on;
        // Clamp threshold into a sane window before converting to linear.
        let th = if threshold_db.is_finite() {
            threshold_db.clamp(-60.0, 0.0)
        } else {
            -18.0
        };
        self.glue_threshold = Self::db_to_gain(th);
        self.glue_ratio = if ratio.is_finite() {
            ratio.clamp(1.0, 20.0)
        } else {
            2.0
        };
        let mk = if makeup_db.is_finite() {
            makeup_db.clamp(0.0, 24.0)
        } else {
            0.0
        };
        self.glue_makeup = Self::db_to_gain(mk);
        self.glue_mix = clamp01(mix);
    }

    /// Configure the **true-peak limiter**.
    ///
    /// * `on` — enable/bypass (default on — it is the resonance guard).
    /// * `ceiling_dbtp` — output true-peak ceiling in dBTP (e.g. -0.3).
    /// * `release_secs` — how fast the gain recovers after a peak.
    /// * `style` — [`LimiterStyle::Transparent`] or [`LimiterStyle::Punchy`].
    pub fn set_limiter(
        &mut self,
        on: bool,
        ceiling_dbtp: f32,
        release_secs: f32,
        style: LimiterStyle,
    ) {
        self.limiter_on = on;
        let ceil = if ceiling_dbtp.is_finite() {
            ceiling_dbtp.clamp(-24.0, 0.0)
        } else {
            -0.3
        };
        self.limiter_ceiling = Self::db_to_gain(ceil);
        self.limiter_release_secs = if release_secs.is_finite() {
            release_secs.clamp(0.001, 2.0)
        } else {
            0.05
        };
        self.limiter_style = style;
        self.recompute_coeffs();
    }

    /// The pump gain in effect *right now*, `0.0..=1.0` (1.0 = not ducked,
    /// smaller = ducked further). Exposed so the pump can later be a modulation
    /// source feeding, say, a filter cutoff in time with the movement.
    #[inline]
    pub fn pump_envelope(&self) -> f32 {
        self.pump_gain
    }

    /// The most recent total gain reduction applied by the glue compressor, in
    /// dB (always `<= 0`). For the GR meter.
    #[inline]
    pub fn gain_reduction_db(&self) -> f32 {
        self.glue_gr_db
    }

    /// The most recent gain reduction applied by the limiter, in dB (`<= 0`).
    #[inline]
    pub fn limiter_gr_db(&self) -> f32 {
        self.limiter_gr_db
    }

    /// Recompute every per-sample time-coefficient from the current sample rate
    /// and release settings. One-pole coefficient for a time-constant `tau`
    /// seconds is `exp(-1 / (tau * sr))`. Cheap; only called on setup, sample-
    /// rate changes and limiter-release changes (not in the hot loop).
    fn recompute_coeffs(&mut self) {
        let sr = self.sample_rate.max(1.0);

        // Glue: ~10 ms attack, ~120 ms auto-release baseline.
        self.glue_atk_coeff = one_pole_coeff(0.010, sr);
        self.glue_rel_coeff = one_pole_coeff(0.120, sr);

        // Limiter release depends on the style: Punchy is roughly half the set
        // release so it recovers faster (louder, more transient through).
        let rel = match self.limiter_style {
            LimiterStyle::Transparent => self.limiter_release_secs,
            LimiterStyle::Punchy => self.limiter_release_secs * 0.5,
        };
        self.limiter_rel_coeff = one_pole_coeff(rel.max(0.0005), sr);

        // Look-ahead length in samples, clamped to the ring capacity.
        let n = (LOOKAHEAD_MS * 0.001 * sr).round() as usize;
        self.look_len = n.clamp(1, MAX_LOOKAHEAD_SAMPLES - 1);
    }

    /// Process one stereo sample through pump -> glue -> limiter.
    ///
    /// Real-time safe: pure float math on existing fields plus a fixed-size ring
    /// write/read. The output is guaranteed finite and `|out| <= ceiling`.
    #[inline]
    pub fn process(&mut self, l_in: f32, r_in: f32) -> (f32, f32) {
        // Fold any NaN/inf input to 0 so it can never poison the envelopes.
        let l0 = if l_in.is_finite() { l_in } else { 0.0 };
        let r0 = if r_in.is_finite() { r_in } else { 0.0 };

        // ---- 1) PUMP ---------------------------------------------------------
        let (mut l, mut r) = self.process_pump(l0, r0);

        // ---- 2) GLUE compressor ---------------------------------------------
        let (gl, gr) = self.process_glue(l, r);
        l = gl;
        r = gr;

        // ---- 3) true-peak LIMITER -------------------------------------------
        self.process_limiter(l, r)
    }

    /// Advance the pump cycle one sample and apply the duck gain to both
    /// channels. The duck gain is also stashed in `pump_gain` for the getter.
    #[inline]
    fn process_pump(&mut self, l: f32, r: f32) -> (f32, f32) {
        // Advance the cycle position. `pump_division_secs` is the resolved cycle
        // length (from tempo + division); one cycle = `cycle_samples` samples.
        let cycle_samples = (self.pump_division_secs * self.sample_rate).max(1.0);
        self.pump_pos += 1.0 / cycle_samples;
        if self.pump_pos >= 1.0 {
            self.pump_pos -= 1.0;
        }

        // Position within the cycle, offset by the user phase. `t` runs 0..1,
        // 0 = the instant the duck fires (deepest), 1 = fully recovered.
        let t = (self.pump_pos + self.pump_phase).rem_euclid(1.0);

        // The recovery curve, shape(t): 0 at t=0 (fully ducked), 1 at t=1
        // (recovered). `curve` blends a linear ramp into a sharp exponential
        // "thump" recovery (fast snap-back then ease).
        let lin = t;
        // An ease-out exponential: rises fast early then flattens. k controls the
        // sharpness; curve=1 gives the snappiest kick-style recovery.
        let k = 1.0 + 8.0 * self.pump_curve;
        let expo = 1.0 - (-k * t).exp();
        // Normalize the exponential so it actually reaches 1.0 at t=1 (otherwise
        // the duck would never fully recover for finite k).
        let expo_norm = expo / (1.0 - (-k).exp()).max(1.0e-6);
        let shape = lin * (1.0 - self.pump_curve) + expo_norm * self.pump_curve;

        // IntKick sharpens the onset: the first part of the cycle ducks harder
        // (a synthesized kick transient defines the duck), modeled as an extra
        // fast initial dip that decays over the first few percent of the cycle.
        let onset = if self.pump_source == PumpSource::IntKick {
            (-t * 40.0).exp()
        } else {
            0.0
        };

        // Duck gain: 1 - amount at the bottom, recovering to 1 over the cycle,
        // with the optional kick onset deepening the very start.
        let duck = (1.0 - self.pump_amount) + self.pump_amount * shape;
        let gain = (duck - onset * self.pump_amount * 0.5).clamp(0.0, 1.0);

        self.pump_gain = gain;
        (l * gain, r * gain)
    }

    /// Apply the glue compressor. A hybrid peak/RMS-ish detector on the
    /// stereo-summed level drives a soft-knee static curve; one shared gain is
    /// applied to both channels to keep the image centered. Parallel `mix`
    /// blends the compressed and uncompressed signals.
    #[inline]
    fn process_glue(&mut self, l: f32, r: f32) -> (f32, f32) {
        if !self.glue_on {
            self.glue_gr_db = 0.0;
            return (l, r);
        }

        // Detector input: the louder of the two channels (peak-ish) blended with
        // the mean — a cheap stand-in for the 3630's peak/RMS hybrid.
        let peak = l.abs().max(r.abs());
        let mean = 0.5 * (l.abs() + r.abs());
        let detector = 0.7 * peak + 0.3 * mean;

        // Envelope follower with fast attack / slow release. When the detector is
        // above the envelope we attack quickly; below, we release slowly (the
        // program-dependent "auto" feel of a bus comp).
        let coeff = if detector > self.glue_env {
            self.glue_atk_coeff
        } else {
            self.glue_rel_coeff
        };
        self.glue_env += (detector - self.glue_env) * (1.0 - coeff);

        // Static gain computer with a soft knee around the threshold. Work in dB.
        let env = self.glue_env.max(1.0e-9);
        let env_db = 20.0 * env.log10();
        let thresh_db = 20.0 * self.glue_threshold.max(1.0e-9).log10();
        let over = env_db - thresh_db; // dB above threshold (can be negative)

        // Soft knee width in dB. Inside the knee we interpolate quadratically so
        // the onset of compression is gentle rather than a hard corner.
        const KNEE_DB: f32 = 6.0;
        let gr_db = if over <= -KNEE_DB * 0.5 {
            0.0
        } else if over >= KNEE_DB * 0.5 {
            // Above the knee: full ratio. Reduction = over * (1 - 1/ratio).
            -(over) * (1.0 - 1.0 / self.glue_ratio)
        } else {
            // Within the knee: quadratic interpolation of the gain reduction.
            let x = over + KNEE_DB * 0.5; // 0..KNEE_DB
            let slope = 1.0 - 1.0 / self.glue_ratio;
            -(slope * x * x) / (2.0 * KNEE_DB)
        };

        self.glue_gr_db = gr_db;
        let comp_gain = Self::db_to_gain(gr_db) * self.glue_makeup;

        // Compressed signal (shared gain), then parallel blend with the dry.
        let cl = l * comp_gain;
        let cr = r * comp_gain;
        let dry = 1.0 - self.glue_mix;
        (dry * l + self.glue_mix * cl, dry * r + self.glue_mix * cr)
    }

    /// The look-ahead true-peak limiter. Estimates the inter-sample (true) peak
    /// of the *incoming* sample, decides what gain would keep that peak under
    /// the ceiling, drives a gain envelope that drops instantly and releases
    /// slowly, and applies that envelope to the *delayed* (look-ahead) audio.
    /// A final hard clamp guarantees the ceiling is never exceeded.
    #[inline]
    fn process_limiter(&mut self, l: f32, r: f32) -> (f32, f32) {
        let ceiling = self.limiter_ceiling;

        if !self.limiter_on {
            // Even bypassed, we hard-clamp to the ceiling so the "resonance
            // guard" promise holds: the output can never exceed the ceiling.
            self.limiter_gr_db = 0.0;
            return (l.clamp(-ceiling, ceiling), r.clamp(-ceiling, ceiling));
        }

        // Push the new samples into the true-peak history (shift register).
        for i in 0..TP_TAPS - 1 {
            self.tp_hist_l[i] = self.tp_hist_l[i + 1];
            self.tp_hist_r[i] = self.tp_hist_r[i + 1];
        }
        self.tp_hist_l[TP_TAPS - 1] = l;
        self.tp_hist_r[TP_TAPS - 1] = r;

        // Estimate the true (inter-sample) peak magnitude across both channels.
        let tp = true_peak(&self.tp_hist_l).max(true_peak(&self.tp_hist_r));

        // What gain would bring that true peak down to the ceiling? If the peak
        // is already under the ceiling, the target is unity (1.0).
        let target_gain = if tp > ceiling {
            (ceiling / tp).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // The envelope drops *instantly* to a new lower target (so we never miss
        // a peak — the look-ahead delay buys us the time to apply it) and
        // recovers toward unity over the release time.
        if target_gain < self.limiter_gain {
            self.limiter_gain = target_gain;
        } else {
            self.limiter_gain += (target_gain - self.limiter_gain) * (1.0 - self.limiter_rel_coeff);
        }
        // Numerical safety: keep the envelope in [0, 1].
        self.limiter_gain = self.limiter_gain.clamp(0.0, 1.0);

        // Write the *current* input into the look-ahead ring and read out the
        // sample from `look_len` samples ago — that delayed sample is what we
        // actually apply the (computed-slightly-ahead) gain to.
        let read = (self.look_pos + MAX_LOOKAHEAD_SAMPLES - self.look_len) % MAX_LOOKAHEAD_SAMPLES;
        let delayed_l = self.look_l[read];
        let delayed_r = self.look_r[read];
        self.look_l[self.look_pos] = l;
        self.look_r[self.look_pos] = r;
        self.look_pos = (self.look_pos + 1) % MAX_LOOKAHEAD_SAMPLES;

        let g = self.limiter_gain;
        let mut out_l = delayed_l * g;
        let mut out_r = delayed_r * g;

        // Final hard safety clamp: even if the smooth envelope somehow missed a
        // transient (e.g. a true-peak between samples larger than our estimate),
        // the output physically cannot exceed the ceiling.
        out_l = out_l.clamp(-ceiling, ceiling);
        out_r = out_r.clamp(-ceiling, ceiling);

        // Report the limiter's gain reduction in dB for the meter.
        self.limiter_gr_db = 20.0 * g.max(1.0e-9).log10();

        (out_l, out_r)
    }
}

/// One-pole smoothing coefficient for a time-constant of `tau` seconds at
/// sample rate `sr`. The recursion `y += (x - y) * (1 - coeff)` then has the
/// requested time constant. Guards against tau <= 0.
#[inline]
fn one_pole_coeff(tau: f32, sr: f32) -> f32 {
    let t = tau.max(1.0e-6);
    (-1.0 / (t * sr)).exp()
}

/// Clamp a value to `0.0..=1.0`, folding NaN to 0.0 (so a NaN automation value
/// can never reach the audio path).
#[inline]
fn clamp01(x: f32) -> f32 {
    // `f32::clamp` *returns* NaN for a NaN input; we fold NaN to 0.0 by hand so a
    // bad automation value can never poison the audio path. Same trick as the
    // filter's resonance clamp.
    #[allow(clippy::manual_clamp)]
    if x.is_nan() {
        0.0
    } else {
        x.max(0.0).min(1.0)
    }
}

/// Estimate the true-peak magnitude around the newest sample by reconstructing
/// `OVERSAMPLE`-1 inter-sample points between the central pair with a 4-point
/// cubic (Catmull-Rom) and taking the largest absolute value (including the
/// samples themselves). `hist` is the last [`TP_TAPS`] input samples with the
/// newest at the end. This is a cheap inter-sample peak estimate — not a perfect
/// resampler, but enough to catch peaks the raw samples hide.
#[inline]
fn true_peak(hist: &[f32; TP_TAPS]) -> f32 {
    // The sample peaks themselves are always part of the true peak.
    let mut peak = 0.0f32;
    for &h in hist.iter() {
        peak = peak.max(h.abs());
    }
    // hist[1] and hist[2] are the central pair; hist[0]/hist[3] give the slopes.
    let p0 = hist[0];
    let p1 = hist[1];
    let p2 = hist[2];
    let p3 = hist[3];
    for k in 1..OVERSAMPLE {
        let frac = k as f32 / OVERSAMPLE as f32;
        let interp = catmull_rom(p0, p1, p2, p3, frac);
        peak = peak.max(interp.abs());
    }
    peak
}

/// 4-point Catmull-Rom cubic interpolation between `p1` and `p2` (with `p0`/`p3`
/// supplying the tangents), evaluated at `t` in `0.0..=1.0`. Used by the
/// true-peak estimator to reconstruct inter-sample values.
#[inline]
fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const SR: f32 = 48_000.0;

    /// A fresh dynamics box at 48 kHz, tempo 120.
    fn dyn_default() -> Dynamics {
        let mut d = Dynamics::new(SR);
        d.set_tempo(120.0);
        d
    }

    /// The default (pump off, glue gentle, limiter on at -0.3 dBTP) must never
    /// produce a non-finite sample and must never exceed the ceiling, for a wide
    /// range of inputs including pathological extremes.
    #[test]
    fn output_is_finite_and_under_ceiling() {
        let mut d = dyn_default();
        let ceiling = Dynamics::db_to_gain(-0.3);
        let mut phase = 0.0f32;
        for i in 0..(SR as usize) {
            // A loud-ish tone, occasionally slammed with a huge spike to mimic a
            // resonant filter peak.
            let mut x = 0.9 * (phase * TAU).sin();
            phase = (phase + 220.0 / SR).fract();
            if i % 1000 == 0 {
                x = 50.0; // +34 dB spike — the filter resonance worst case
            }
            let (l, r) = d.process(x, -x);
            assert!(l.is_finite() && r.is_finite(), "non-finite at i={i}: {l},{r}");
            assert!(
                l.abs() <= ceiling + 1.0e-4 && r.abs() <= ceiling + 1.0e-4,
                "exceeded ceiling at i={i}: {l},{r} > {ceiling}"
            );
        }
    }

    /// The headline guarantee: no matter how huge the input (a stacked +40 dB
    /// resonant peak), the limiter output never exceeds the ceiling, across a
    /// range of ceiling settings and both styles.
    #[test]
    fn limiter_never_exceeds_ceiling() {
        for &ceil_db in &[-12.0f32, -6.0, -3.0, -0.3, 0.0] {
            for style in [LimiterStyle::Transparent, LimiterStyle::Punchy] {
                let mut d = dyn_default();
                // Disable glue so we test the limiter in isolation.
                d.set_glue(false, -18.0, 2.0, 0.0, 1.0);
                d.set_limiter(true, ceil_db, 0.05, style);
                let ceiling = Dynamics::db_to_gain(ceil_db);

                let mut phase = 0.0f32;
                let mut max_out = 0.0f32;
                for i in 0..20_000 {
                    // Ramp the input from huge to insane to probe every regime.
                    let amp = 10.0 + (i as f32 / 20_000.0) * 200.0; // up to +46 dB
                    let x = amp * (phase * TAU).sin();
                    phase = (phase + 1000.0 / SR).fract();
                    let (l, r) = d.process(x, x);
                    assert!(l.is_finite() && r.is_finite());
                    max_out = max_out.max(l.abs()).max(r.abs());
                }
                assert!(
                    max_out <= ceiling + 1.0e-4,
                    "limiter exceeded ceiling: out={max_out} ceiling={ceiling} ({ceil_db} dB, {style:?})"
                );
            }
        }
    }

    /// A signal already below the ceiling must pass essentially untouched
    /// (transparent) once the look-ahead delay has filled — the limiter only
    /// acts on overs.
    #[test]
    fn quiet_signal_passes_through_limiter() {
        let mut d = dyn_default();
        d.set_glue(false, -18.0, 2.0, 0.0, 1.0);
        d.set_limiter(true, -0.3, 0.05, LimiterStyle::Transparent);

        // Push a quiet steady tone and compare in/out after the look-ahead fills.
        let mut phase = 0.0f32;
        let mut max_err = 0.0f32;
        let mut history = Vec::new();
        for _ in 0..2000 {
            let x = 0.2 * (phase * TAU).sin();
            phase = (phase + 440.0 / SR).fract();
            history.push(x);
            let (l, _r) = d.process(x, x);
            // Compare against the look-ahead-delayed input.
            if history.len() > d.look_len {
                let delayed = history[history.len() - 1 - d.look_len];
                max_err = max_err.max((l - delayed).abs());
            }
        }
        assert!(
            max_err < 1.0e-3,
            "quiet signal should pass the limiter cleanly, max err {max_err}"
        );
    }

    /// The glue compressor must actually reduce gain on loud material and report
    /// a negative gain-reduction figure, while leaving quiet material alone.
    #[test]
    fn glue_compresses_loud_passes_quiet() {
        let mut d = dyn_default();
        // Glue on, limiter off so we measure the comp alone.
        d.set_glue(true, -18.0, 4.0, 0.0, 1.0);
        d.set_limiter(false, 0.0, 0.05, LimiterStyle::Transparent);

        // Quiet tone well below threshold: little/no reduction.
        let mut phase = 0.0f32;
        for _ in 0..5000 {
            let x = 0.05 * (phase * TAU).sin(); // ~-26 dB, below -18 thresh
            phase = (phase + 200.0 / SR).fract();
            d.process(x, x);
        }
        let quiet_gr = d.gain_reduction_db();
        assert!(quiet_gr > -1.0, "quiet material should barely compress: {quiet_gr} dB");

        // Loud tone well above threshold: clear reduction.
        let mut d2 = dyn_default();
        d2.set_glue(true, -18.0, 4.0, 0.0, 1.0);
        d2.set_limiter(false, 0.0, 0.05, LimiterStyle::Transparent);
        let mut phase = 0.0f32;
        for _ in 0..5000 {
            let x = 0.9 * (phase * TAU).sin(); // ~-1 dB, well above thresh
            phase = (phase + 200.0 / SR).fract();
            d2.process(x, x);
        }
        let loud_gr = d2.gain_reduction_db();
        assert!(
            loud_gr < -2.0,
            "loud material should compress noticeably: {loud_gr} dB"
        );
    }

    /// The glue compressor preserves the stereo image: it applies the *same*
    /// gain to both channels, so the L/R ratio is unchanged by compression.
    #[test]
    fn glue_preserves_stereo_image() {
        let mut d = dyn_default();
        d.set_glue(true, -24.0, 4.0, 0.0, 1.0);
        d.set_limiter(false, 0.0, 0.05, LimiterStyle::Transparent);

        let mut phase = 0.0f32;
        let mut max_ratio_err = 0.0f32;
        for _ in 0..5000 {
            let s = (phase * TAU).sin();
            phase = (phase + 300.0 / SR).fract();
            // L is twice R going in.
            let (l, r) = d.process(0.8 * s, 0.4 * s);
            if r.abs() > 1.0e-4 {
                // The output ratio should stay ~2:1 (same gain on both).
                max_ratio_err = max_ratio_err.max(((l / r) - 2.0).abs());
            }
        }
        assert!(
            max_ratio_err < 1.0e-3,
            "comp must apply equal gain to both channels, ratio err {max_ratio_err}"
        );
    }

    /// The pump must duck the signal when enabled, and its exposed envelope must
    /// actually move between fully-ducked and unity over a cycle.
    #[test]
    fn pump_ducks_and_exposes_moving_envelope() {
        let mut d = dyn_default();
        d.set_glue(false, -18.0, 2.0, 0.0, 1.0);
        d.set_limiter(false, 0.0, 0.05, LimiterStyle::Transparent);
        // Strong pump, one-beat cycle (0.5 s at 120 bpm).
        d.set_pump(1.0, PumpSource::Tempo, 0.5, 0.5, 0.0);

        let mut min_env = f32::MAX;
        let mut max_env = f32::MIN;
        // Run two full cycles.
        for _ in 0..(SR as usize) {
            d.process(1.0, 1.0);
            let e = d.pump_envelope();
            assert!(e.is_finite() && (0.0..=1.0).contains(&e), "pump env out of range: {e}");
            min_env = min_env.min(e);
            max_env = max_env.max(e);
        }
        // With amount=1.0 the duck should reach near 0 and recover to near 1.
        assert!(min_env < 0.1, "pump should duck deeply, min env {min_env}");
        assert!(max_env > 0.9, "pump should recover to unity, max env {max_env}");
    }

    /// Pump off (amount 0) must be a true bypass of the pump stage: the envelope
    /// sits at unity and the signal is unchanged by the pump.
    #[test]
    fn pump_off_is_unity() {
        let mut d = dyn_default();
        d.set_glue(false, -18.0, 2.0, 0.0, 1.0);
        d.set_limiter(false, 0.0, 0.05, LimiterStyle::Transparent);
        d.set_pump(0.0, PumpSource::Tempo, 0.5, 0.5, 0.0);

        for _ in 0..5000 {
            let (l, r) = d.process(0.5, -0.3);
            assert_eq!(d.pump_envelope(), 1.0, "pump off must hold unity");
            // Limiter disabled here, glue off: signal should be exactly the input.
            assert!((l - 0.5).abs() < 1.0e-6 && (r + 0.3).abs() < 1.0e-6);
        }
    }

    /// The IntKick source must still keep the envelope finite and in range, and
    /// duck deeply (its onset transient adds an extra initial dip).
    #[test]
    fn pump_intkick_is_bounded() {
        let mut d = dyn_default();
        d.set_pump(0.8, PumpSource::IntKick, 0.5, 0.7, 45.0);
        let mut min_env = f32::MAX;
        for _ in 0..(SR as usize / 2) {
            d.process(1.0, 1.0);
            let e = d.pump_envelope();
            assert!(e.is_finite() && (0.0..=1.0).contains(&e));
            min_env = min_env.min(e);
        }
        assert!(min_env < 0.3, "intkick should duck, min env {min_env}");
    }

    /// Full parameter sweep abuse: every setter fed extremes (including NaN and
    /// infinities) must never produce a non-finite output or exceed the ceiling.
    #[test]
    fn survives_pathological_params() {
        let mut d = dyn_default();
        let bad = [f32::NAN, f32::INFINITY, -f32::INFINITY, 1.0e9, -1.0e9, 0.0];
        for &a in &bad {
            for &b in &bad {
                d.set_tempo(a);
                d.set_pump(a, PumpSource::IntKick, b, a, b);
                d.set_glue(true, a, b, a, b);
                d.set_limiter(true, a, b, LimiterStyle::Punchy);
                let ceiling = d.limiter_ceiling;
                for _ in 0..256 {
                    let (l, r) = d.process(a, b);
                    assert!(l.is_finite() && r.is_finite(), "non-finite from bad params a={a} b={b}");
                    assert!(
                        l.abs() <= ceiling + 1.0e-3 && r.abs() <= ceiling + 1.0e-3,
                        "ceiling breached with bad params"
                    );
                }
            }
        }
    }

    /// set_sample_rate must keep the look-ahead within its preallocated ring and
    /// the output bounded after a rate change mid-stream.
    #[test]
    fn sample_rate_change_is_safe() {
        let mut d = dyn_default();
        d.set_limiter(true, -1.0, 0.05, LimiterStyle::Transparent);
        for &sr in &[44_100.0f32, 48_000.0, 96_000.0, 192_000.0] {
            d.set_sample_rate(sr);
            assert!(d.look_len < MAX_LOOKAHEAD_SAMPLES);
            let ceiling = d.limiter_ceiling;
            for _ in 0..1000 {
                let (l, r) = d.process(20.0, -20.0);
                assert!(l.is_finite() && r.is_finite());
                assert!(l.abs() <= ceiling + 1.0e-4 && r.abs() <= ceiling + 1.0e-4);
            }
        }
    }

    /// A true inter-sample peak that hides *between* samples must still be caught
    /// by the true-peak detector: feed an alternating +/- pattern (whose
    /// reconstructed inter-sample peak is well above the sample values) and
    /// confirm the output true-peak (estimated the same way) stays under ceiling.
    #[test]
    fn true_peak_intersample_is_caught() {
        let mut d = dyn_default();
        d.set_glue(false, -18.0, 2.0, 0.0, 1.0);
        d.set_limiter(true, -1.0, 0.02, LimiterStyle::Transparent);
        let ceiling = Dynamics::db_to_gain(-1.0);

        // A full-scale alternation has large inter-sample overshoot.
        let mut sign = 1.0f32;
        let mut hist_l = [0.0f32; TP_TAPS];
        let mut max_tp = 0.0f32;
        for _ in 0..20_000 {
            let x = sign * 1.5; // already above the ceiling
            sign = -sign;
            let (l, _r) = d.process(x, x);
            // Track the inter-sample true peak of the OUTPUT using the same
            // estimator the limiter uses.
            for i in 0..TP_TAPS - 1 {
                hist_l[i] = hist_l[i + 1];
            }
            hist_l[TP_TAPS - 1] = l;
            max_tp = max_tp.max(true_peak(&hist_l));
        }
        // The estimated output true peak must stay at/under the ceiling (with a
        // small tolerance for the estimator vs the hard clamp).
        assert!(
            max_tp <= ceiling * 1.05,
            "inter-sample true peak leaked past ceiling: {max_tp} > {ceiling}"
        );
    }

    /// All three stages in series must still be finite and bounded, the whole
    /// point of the chained box.
    #[test]
    fn full_chain_finite_and_bounded() {
        let mut d = dyn_default();
        d.set_pump(0.6, PumpSource::Tempo, 0.25, 0.7, 90.0);
        d.set_glue(true, -20.0, 4.0, 6.0, 0.8);
        d.set_limiter(true, -0.5, 0.03, LimiterStyle::Punchy);
        let ceiling = Dynamics::db_to_gain(-0.5);

        let mut phase = 0.0f32;
        for i in 0..(SR as usize) {
            let mut x = 1.2 * (phase * TAU).sin();
            phase = (phase + 110.0 / SR).fract();
            if i % 777 == 0 {
                x = 30.0;
            }
            let (l, r) = d.process(x, 0.5 * x);
            assert!(l.is_finite() && r.is_finite());
            assert!(l.abs() <= ceiling + 1.0e-4 && r.abs() <= ceiling + 1.0e-4);
        }
        // The meters should report finite, non-positive reductions.
        assert!(d.gain_reduction_db() <= 0.0 && d.gain_reduction_db().is_finite());
        assert!(d.limiter_gr_db() <= 0.0 && d.limiter_gr_db().is_finite());
    }
}

