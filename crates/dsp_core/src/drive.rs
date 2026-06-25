//! A mono saturation + bitcrush/downsample stage — the synth's "dirt" module.
//!
//! This sits *first* in the post-synth effects chain (before the chorus widens
//! the bus to stereo), so all the grit hits the raw voice sum while it is still
//! mono. It is two effects in series:
//!
//! 1. **A waveshaper.** A nonlinear curve — `tanh`, a soft diode, etc. — that
//!    rounds off (or mangles) the peaks of the signal. Rounding a sine's peaks
//!    *adds harmonics*: that extra harmonic content is what your ear hears as
//!    "warmth", "edge", or "fuzz" depending on how hard you push it and which
//!    curve you pick.
//!
//! 2. **A bitcrush + downsampler.** Deliberate *digital* lo-fi. Bitcrushing
//!    quantises the amplitude to a coarse grid (fewer "bits"), and downsampling
//!    holds each sample for several output samples (a lower effective sample
//!    rate). Both throw away information on purpose — the result is the gritty,
//!    aliased, early-sampler sound.
//!
//! # Aliasing, and why we oversample the waveshaper
//!
//! A nonlinearity generates harmonics *above* the ones already present. If a
//! harmonic lands above Nyquist (half the sample rate) it cannot be represented
//! — it *folds back* down to a lower, inharmonic frequency. That folded junk is
//! **aliasing**, and it sounds harsh and detuned. The fix is to run the
//! waveshaper at a higher sample rate (here **2×**), where Nyquist is twice as
//! high, so most of the new harmonics fit before folding; then we filter and
//! decimate back down. We upsample with a cheap zero-order-hold + the new
//! sample, run `tanh` on both half-samples, and average — a tiny 2-point box
//! that knocks down the worst of the fold-back for almost no cost.
//!
//! The **bitcrush/downsample** is the opposite philosophy: its aliasing is the
//! *point* (that's the lo-fi character), so it is intentionally **not**
//! oversampled. We do offer a `crush_mix` so you can dial the crushed signal
//! against the clean-but-saturated signal, and the crush runs *after* the
//! (clean) waveshaper so the quantisation grid bites the already-saturated tone.
//!
//! # Real-time safety
//!
//! There are no buffers to allocate — every bit of state is a handful of `f32`
//! scalars set up in [`Drive::new`]. The audio-callback path ([`Drive::process`])
//! only does float math: no allocation, no locks.

use crate::util::flush_denormal;
use std::f32::consts::PI;

/// The waveshaper flavour. Each is a different nonlinear curve with its own
/// harmonic fingerprint.
///
/// * [`Clean`](DriveKind::Clean) — unity passthrough; the shaper is bypassed
///   (you still get bitcrush/downsample if those are engaged).
/// * [`Tube`](DriveKind::Tube) — an *asymmetric* `tanh`. A small DC bias is
///   added before the curve so the top and bottom of the wave clip differently,
///   which produces **even** harmonics (2nd, 4th…) — the warm, "tube" colour.
/// * [`Diode`](DriveKind::Diode) — `x / (1 + |x|)`, a soft, rectifier-ish fold
///   that softens gently and never fully flattens; smoother than `tanh`.
/// * [`RatFuzz`](DriveKind::RatFuzz) — a hot `tanh(k·x)` with a large `k` so it
///   clips early and hard: the buzzy, aggressive "RAT pedal" voice. Brightness
///   is sculpted afterward by the Tone control.
/// * [`Compound`](DriveKind::Compound) — Tube **then** Diode in series, stacking
///   the asymmetric warmth into the soft fold for a thicker, more complex grind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DriveKind {
    Clean,
    /// The default flavour — gentle, warm asymmetric tube saturation.
    #[default]
    Tube,
    Diode,
    RatFuzz,
    Compound,
}

/// Mono saturation + bitcrush/downsample. See the module docs for the DSP story.
#[derive(Clone, Debug)]
pub struct Drive {
    sample_rate: f32,

    // --- parameters (already mapped to engineering units by `set_params`) ---
    /// Master enable. When `false`, [`process`](Drive::process) returns the
    /// input untouched (true bypass).
    on: bool,
    kind: DriveKind,
    /// Pre-shaper input gain, `1.0..≈16.0`. Derived from the 0..1 "drive" knob.
    in_gain: f32,
    /// Tone tilt in `0.0..=1.0`. 0.5 is flat; below tilts dark, above bright.
    tone: f32,
    /// Bitcrush depth in bits, `1.0..=16.0`. `>= 16` means bypass the crush.
    bits: f32,
    /// Downsample factor, `1.0..=50.0`. `<= 1.0` means bypass the decimator.
    downsample: f32,
    /// Blend of crushed/decimated vs. the saturated-but-clean signal, `0..=1`.
    crush_mix: f32,
    /// Overall wet/dry blend of the whole driven signal vs. the raw input.
    mix: f32,

    // --- per-sample state (no allocation; all scalars) ---
    /// One-pole low-pass state, shared by the tone tilt EQ.
    tone_lp: f32,
    /// The previous (input-gained) shaper input, kept for the 2× oversampler's
    /// midpoint interpolation.
    prev_in: f32,
    /// Sample-and-hold value for the downsampler.
    sh_hold: f32,
    /// Fractional phase accumulator for the downsampler.
    sh_phase: f32,
}

impl Drive {
    /// Smoothing coefficient for the one-pole tone filter, expressed as a cutoff
    /// in Hz. The tilt pivots around this frequency; ~700 Hz puts the "dark vs
    /// bright" hinge in the lower-mids, which is musically where it reads as a
    /// tone control rather than a mud/ice switch.
    const TONE_PIVOT_HZ: f32 = 700.0;

    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            on: false,
            kind: DriveKind::Tube,
            in_gain: 1.0,
            tone: 0.5,
            bits: 16.0,
            downsample: 1.0,
            crush_mix: 1.0,
            mix: 1.0,
            tone_lp: 0.0,
            prev_in: 0.0,
            sh_hold: 0.0,
            sh_phase: 0.0,
        }
    }

    /// Change the sample rate and clear all filter/hold state so a rate switch
    /// cannot replay stale audio. No allocation — the struct holds only scalars.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.tone_lp = 0.0;
        self.prev_in = 0.0;
        self.sh_hold = 0.0;
        self.sh_phase = 0.0;
    }

    /// Set every parameter in one call, mapping the raw 0..1 knob values to
    /// engineering units. Call this at block rate (once per audio block).
    ///
    /// * `on` — master enable.
    /// * `kind` — which waveshaper curve.
    /// * `drive` — `0.0..=1.0` knob; mapped to a 1×..≈16× input gain with a
    ///   square-law skew so the lower half of the knob has fine control.
    /// * `tone` — `0.0..=1.0`; 0.5 flat, `<0.5` darker, `>0.5` brighter.
    /// * `bits` — bit depth `1.0..=16.0`; `>= 16` bypasses the crush.
    /// * `downsample` — decimation factor `1.0..=50.0`; `<= 1.0` bypasses it.
    /// * `crush_mix` — `0.0..=1.0` blend of crushed vs. uncrushed saturated tone.
    /// * `mix` — `0.0..=1.0` wet/dry of the whole module.
    #[allow(clippy::too_many_arguments)]
    pub fn set_params(
        &mut self,
        on: bool,
        kind: DriveKind,
        drive: f32,
        tone: f32,
        bits: f32,
        downsample: f32,
        crush_mix: f32,
        mix: f32,
    ) {
        self.on = on;
        self.kind = kind;

        // Map the 0..1 drive knob to a pre-gain. A square-law (`d²`) skew keeps
        // the bottom of the travel gentle — small twists = subtle warmth — and
        // crams the aggressive gains into the top. 1× at 0, ~16× at 1.
        let d = clamp01(drive);
        self.in_gain = 1.0 + 15.0 * d * d;

        self.tone = clamp01(tone);
        // Guard the crush controls so out-of-range automation can't NaN us.
        self.bits = guard(bits, 16.0).clamp(1.0, 16.0);
        self.downsample = guard(downsample, 1.0).clamp(1.0, 50.0);
        self.crush_mix = clamp01(crush_mix);
        self.mix = clamp01(mix);
    }

    /// Process one mono sample, returning the (optionally driven) mono output.
    ///
    /// The signal path is: input gain → 2× oversampled waveshaper → tone tilt →
    /// bitcrush → downsample → `crush_mix` blend → `mix` blend with the dry input.
    pub fn process(&mut self, input: f32) -> f32 {
        // True bypass: nothing engaged, return the sample bit-for-bit.
        if !self.on {
            return input;
        }

        // --- 1. Waveshaper, oversampled 2× -----------------------------------
        // We have a continuous-time nonlinearity but only discrete samples. To
        // approximate the in-between sample (where harmonics would otherwise
        // alias) we linearly interpolate halfway between the previous and
        // current *shaper input*, shape both half-samples, and average. That 2×
        // run + 2-point average is a tiny, cheap anti-alias that tames the worst
        // fold-back without a full polyphase resampler.
        let shaped = if self.kind == DriveKind::Clean {
            // Clean: no curve, and deliberately no input-gain boost — "drive" on
            // a flat curve would just be a volume knob, which is surprising. The
            // crush/downsample below is the only effect in Clean.
            input
        } else {
            let x = input * self.in_gain;
            // The midpoint between last call's shaper input and this one. We run
            // the curve on both the interpolated half-sample and the real one,
            // then average — a 2× oversample that knocks down alias fold-back.
            // `prev_shaper_in` (stored in `prev_in`) is the previous `x`.
            let mid = 0.5 * (self.prev_shaper_in() + x);
            let half_a = self.shape(mid);
            let half_b = self.shape(x);
            self.set_prev_shaper_in(x);
            0.5 * (half_a + half_b)
        };

        // --- 2. Tone tilt EQ -------------------------------------------------
        let toned = self.apply_tone(shaped);

        // --- 3. Bitcrush -----------------------------------------------------
        let crushed = self.bitcrush(toned);

        // --- 4. Downsample (sample & hold) ----------------------------------
        let decimated = self.downsample_hold(crushed);

        // `crush_mix` blends the crushed+decimated path against the clean (but
        // saturated + tone-shaped) path, so you can keep the saturation while
        // dialing the lo-fi grit in and out.
        let crushed_blend = toned + (decimated - toned) * self.crush_mix;

        // --- 5. Final wet/dry ------------------------------------------------
        input + (crushed_blend - input) * self.mix
    }

    // ---- waveshapers --------------------------------------------------------

    /// The previous (input-gained) shaper input, used by the 2× oversampler to
    /// interpolate the in-between half-sample. Stored in `prev_in`.
    #[inline]
    fn prev_shaper_in(&self) -> f32 {
        self.prev_in
    }

    #[inline]
    fn set_prev_shaper_in(&mut self, x: f32) {
        self.prev_in = x;
    }

    /// Apply the selected nonlinear curve to one (already input-gained) sample.
    ///
    /// Every curve is **level-compensated**: we divide out the curve's slope at
    /// the origin so a quiet signal passes at ~unity gain and only louder peaks
    /// get squashed. (Same trick as `Filter::apply_drive`.) That keeps "drive"
    /// from being a disguised volume knob and keeps the curves comparable.
    #[inline]
    fn shape(&self, x: f32) -> f32 {
        match self.kind {
            DriveKind::Clean => x,
            DriveKind::Tube => tube(x),
            DriveKind::Diode => diode(x),
            DriveKind::RatFuzz => ratfuzz(x),
            DriveKind::Compound => diode(tube(x)),
        }
    }

    // ---- tone ---------------------------------------------------------------

    /// A one-pole *tilt* EQ. We split the signal into a low-passed part and its
    /// high-frequency residual, then re-weight them around the 0.5 center:
    ///
    /// * `tone == 0.5` → flat (low + high == original).
    /// * `tone < 0.5`  → fade the highs out → darker.
    /// * `tone > 0.5`  → boost the highs over the lows → brighter.
    #[inline]
    fn apply_tone(&mut self, x: f32) -> f32 {
        // One-pole low-pass coefficient for TONE_PIVOT_HZ. `a` is the standard
        // `1 - exp(-2π fc / fs)`; clamp keeps it sane at extreme sample rates.
        let fc = Self::TONE_PIVOT_HZ;
        let a = (1.0 - (-2.0 * PI * fc / self.sample_rate).exp()).clamp(0.0, 1.0);
        // One-pole low-pass is recursive: flush so a non-finite input can't
        // latch into `tone_lp` and re-emit forever (the `+=` accumulation never
        // self-heals on its own, unlike the overwrite-style S&H/`prev_in` state).
        self.tone_lp = flush_denormal(self.tone_lp + a * (x - self.tone_lp));
        let low = self.tone_lp;
        let high = x - low;

        // Tilt weights. At t=0.5 both are 1.0 (flat). Below 0.5 the high gain
        // falls to 0 (dark); above 0.5 the high gain rises to 2× while the low
        // eases back a touch so "bright" actually sounds brighter, not just hot.
        let t = self.tone;
        let (low_gain, high_gain) = if t <= 0.5 {
            (1.0, t * 2.0) // 0.0..1.0
        } else {
            (1.0 - (t - 0.5) * 0.5, 1.0 + (t - 0.5) * 2.0) // low 1→0.75, high 1→2
        };
        low * low_gain + high * high_gain
    }

    // ---- bitcrush -----------------------------------------------------------

    /// Quantise the amplitude to `2^(bits-1)` steps. With `bits` near 16 the
    /// grid is so fine it is inaudible, so we treat `>= 16` as a clean bypass.
    /// Fewer bits = a coarser staircase = the gritty quantisation noise.
    #[inline]
    fn bitcrush(&self, x: f32) -> f32 {
        if self.bits >= 16.0 {
            return x;
        }
        // 2^(bits-1) levels per polarity. `bits` can be fractional (the knob is
        // continuous) which just gives a fractional number of steps — fine.
        let q = 2.0f32.powf(self.bits - 1.0);
        // round() to the nearest step, then back to the -1..1 range.
        (x * q).round() / q
    }

    // ---- downsample ---------------------------------------------------------

    /// Sample-and-hold decimation. We advance a fractional phase by
    /// `1/downsample` each sample and only latch a *new* held value when the
    /// phase rolls past 1.0 — so a factor of 4 latches every 4th sample, a
    /// factor of 8.5 latches every 8th or 9th, etc. Between latches we repeat
    /// the held value, which is exactly the stair-step a lower sample rate makes
    /// (aliasing and all — that's the sound).
    #[inline]
    fn downsample_hold(&mut self, x: f32) -> f32 {
        if self.downsample <= 1.0 {
            // Keep the hold tracking the input so toggling it on is click-free.
            self.sh_hold = x;
            self.sh_phase = 0.0;
            return x;
        }
        self.sh_phase += 1.0 / self.downsample;
        if self.sh_phase >= 1.0 {
            self.sh_phase -= 1.0;
            self.sh_hold = x;
        }
        self.sh_hold
    }
}

// ---- free-function curves (so tests and Compound can reuse them) -----------

/// Tube: an **asymmetric** `tanh`. We nudge the input up by a small DC bias so
/// the positive and negative halves saturate differently; that asymmetry is
/// what manufactures even-order harmonics (the "warm" tube colour). We then
/// subtract the bias's own `tanh` so silence stays at silence (no DC offset
/// leaks through), and divide by the slope at the origin for unity small-signal
/// gain.
#[inline]
fn tube(x: f32) -> f32 {
    const BIAS: f32 = 0.2;
    // tanh slope at `BIAS` is `1 - tanh(BIAS)^2`; that is the local gain the
    // small signal sees, so dividing by it keeps quiet input near unity.
    let t_bias = BIAS.tanh();
    let slope = 1.0 - t_bias * t_bias;
    ((x + BIAS).tanh() - t_bias) / slope
}

/// Diode: `x / (1 + |x|)`, a soft rectifier-ish fold. Its slope at the origin is
/// exactly 1, so no compensation is needed — quiet signals already pass at unity
/// and only the peaks bend over. It never fully flattens, giving a rounder,
/// gentler saturation than `tanh`.
#[inline]
fn diode(x: f32) -> f32 {
    x / (1.0 + x.abs())
}

/// RatFuzz: a hot `tanh(k·x)`, level-compensated by `k` (the slope at 0). The
/// large `k` clips early and hard for a buzzy, aggressive fuzz — the brightness
/// and bite are then sculpted by the user's Tone control downstream.
#[inline]
fn ratfuzz(x: f32) -> f32 {
    const K: f32 = 6.0;
    (K * x).tanh() / K
}

// ---- small numeric helpers --------------------------------------------------

/// Clamp to `0.0..=1.0`, turning any NaN into 0.0 so bad automation can't poison
/// the signal path.
#[inline]
fn clamp01(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Replace a NaN/inf with `fallback`, otherwise pass the value through. Used on
/// the crush controls before their range-clamp.
#[inline]
fn guard(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const SR: f32 = 48_000.0;

    /// A driving sine generator for sweeps.
    fn sine(freq: f32, n: usize, amp: f32) -> impl Iterator<Item = f32> {
        let step = freq / SR;
        let mut phase = 0.0f32;
        (0..n).map(move |_| {
            let s = (phase * TAU).sin() * amp;
            phase = (phase + step).fract();
            s
        })
    }

    #[test]
    fn disabled_is_true_bypass() {
        let mut d = Drive::new(SR);
        // Default `on=false`; even with crush params set, no set_params(on=true).
        for x in sine(220.0, 4_000, 0.9) {
            assert_eq!(d.process(x), x, "disabled module must pass input untouched");
        }
    }

    #[test]
    fn finite_and_bounded_across_param_sweep() {
        // Hammer every kind across the corners of the parameter space and make
        // sure nothing ever goes NaN/inf or blows up.
        let kinds = [
            DriveKind::Clean,
            DriveKind::Tube,
            DriveKind::Diode,
            DriveKind::RatFuzz,
            DriveKind::Compound,
        ];
        for &kind in &kinds {
            for &drive in &[0.0, 0.25, 0.5, 1.0] {
                for &tone in &[0.0, 0.5, 1.0] {
                    for &bits in &[1.0, 4.0, 8.0, 16.0] {
                        for &ds in &[1.0, 2.5, 16.0, 50.0] {
                            for &mix in &[0.0, 0.5, 1.0] {
                                let mut d = Drive::new(SR);
                                d.set_params(true, kind, drive, tone, bits, ds, 1.0, mix);
                                // Push a hot signal plus the occasional realistic
                                // spike (a resonant overshoot to ~3×).
                                for (i, x) in sine(330.0, 2_000, 1.5).enumerate() {
                                    let x = if i % 257 == 0 { x + 1.5 } else { x };
                                    let y = d.process(x);
                                    assert!(
                                        y.is_finite(),
                                        "non-finite out: kind={kind:?} drive={drive} \
                                         tone={tone} bits={bits} ds={ds} mix={mix}"
                                    );
                                    // Generous bound: the saturating curves compress,
                                    // and Clean only passes the input (≤3) through
                                    // bounded crush/mix — nothing should run away.
                                    assert!(
                                        y.abs() < 6.0,
                                        "out of range {y}: kind={kind:?} drive={drive}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nan_and_inf_params_are_guarded() {
        let mut d = Drive::new(SR);
        d.set_params(
            true,
            DriveKind::RatFuzz,
            f32::NAN,
            f32::INFINITY,
            f32::NAN,
            f32::INFINITY,
            f32::NAN,
            f32::NAN,
        );
        for x in sine(440.0, 1_000, 1.0) {
            let y = d.process(x);
            assert!(y.is_finite(), "guarded params still produced non-finite out");
        }
    }

    #[test]
    fn saturation_adds_harmonics() {
        // A pure sine through a nonlinearity should gain energy at the 2nd/3rd
        // harmonic. We measure the magnitude of the 3rd harmonic (a Goertzel-ish
        // single-bin DFT) clean vs. driven and expect it to grow.
        fn harmonic_mag(samples: &[f32], freq: f32) -> f32 {
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for (n, &s) in samples.iter().enumerate() {
                let ph = TAU * freq * n as f32 / SR;
                re += s * ph.cos();
                im += s * ph.sin();
            }
            (re * re + im * im).sqrt() / samples.len() as f32
        }

        let f0 = 200.0;
        let clean: Vec<f32> = sine(f0, 8_000, 0.8).collect();

        let mut d = Drive::new(SR);
        // Heavy drive, no crush, full wet — isolate the waveshaper.
        d.set_params(true, DriveKind::Tube, 1.0, 0.5, 16.0, 1.0, 1.0, 1.0);
        let driven: Vec<f32> = sine(f0, 8_000, 0.8).map(|x| d.process(x)).collect();

        let third_clean = harmonic_mag(&clean, 3.0 * f0);
        let third_driven = harmonic_mag(&driven, 3.0 * f0);
        // The clean sine has essentially zero 3rd-harmonic energy; the driven one
        // must have meaningfully more.
        assert!(
            third_driven > third_clean + 1e-3,
            "expected drive to add 3rd-harmonic energy: clean={third_clean}, driven={third_driven}"
        );
    }

    #[test]
    fn tube_is_asymmetric_diode_is_not() {
        // Tube's DC bias should make the curve asymmetric: shaping +x and -x with
        // the same magnitude should NOT be mirror images. Diode is odd-symmetric.
        let x = 0.7;
        assert!(
            (tube(x) + tube(-x)).abs() > 1e-3,
            "tube curve should be asymmetric (even harmonics)"
        );
        assert!(
            (diode(x) + diode(-x)).abs() < 1e-6,
            "diode curve should be odd-symmetric"
        );
    }

    #[test]
    fn small_signal_is_near_unity_gain() {
        // The level-compensation should keep quiet signals near unity for every
        // curve, so "drive" is not a disguised volume knob at low input.
        for kind in [DriveKind::Tube, DriveKind::Diode, DriveKind::RatFuzz, DriveKind::Compound] {
            let mut d = Drive::new(SR);
            // Drive=0 → in_gain 1×; tiny amplitude stays in the linear region.
            d.set_params(true, kind, 0.0, 0.5, 16.0, 1.0, 1.0, 1.0);
            let amp = 1e-3;
            let mut peak_in = 0.0f32;
            let mut peak_out = 0.0f32;
            for x in sine(500.0, 4_000, amp) {
                let y = d.process(x);
                peak_in = peak_in.max(x.abs());
                peak_out = peak_out.max(y.abs());
            }
            let ratio = peak_out / peak_in;
            assert!(
                (ratio - 1.0).abs() < 0.1,
                "small-signal gain should be ~unity for {kind:?}, got {ratio}"
            );
        }
    }

    #[test]
    fn bitcrush_quantises_to_a_grid() {
        // At a low bit depth the output must land on a coarse staircase: the set
        // of distinct output values should be small and each a multiple of the
        // step size. We feed a slow ramp through a crush-only config.
        let mut d = Drive::new(SR);
        // Clean curve so the only effect is the crush; 2 bits = 2 steps/polarity.
        d.set_params(true, DriveKind::Clean, 0.0, 0.5, 2.0, 1.0, 1.0, 1.0);
        let q = 2.0f32.powf(2.0 - 1.0); // == 2
        for i in 0..1000 {
            let x = (i as f32 / 1000.0) * 2.0 - 1.0; // -1..1 ramp
            let y = d.process(x);
            // Every output should be an exact multiple of 1/q.
            let steps = y * q;
            assert!(
                (steps - steps.round()).abs() < 1e-4,
                "crushed output {y} is not on the {q}-step grid"
            );
        }
    }

    #[test]
    fn bitcrush_16_bits_is_bypass() {
        // 16 bits is documented as a clean bypass of the crush stage.
        let mut d = Drive::new(SR);
        d.set_params(true, DriveKind::Clean, 0.0, 0.5, 16.0, 1.0, 1.0, 1.0);
        for x in sine(300.0, 2_000, 0.9) {
            // Clean curve + 16-bit + no downsample + full wet == identity.
            assert!(
                (d.process(x) - x).abs() < 1e-6,
                "16-bit clean chain should be transparent"
            );
        }
    }

    #[test]
    fn downsample_holds_samples() {
        // With a downsample factor of ~4 the output should change at most ~once
        // every 4 input samples — i.e. lots of repeated consecutive values.
        let mut d = Drive::new(SR);
        d.set_params(true, DriveKind::Clean, 0.0, 0.5, 16.0, 4.0, 1.0, 1.0);
        let mut held_runs = 0;
        let mut prev = f32::NAN;
        let n = 4_000;
        for x in sine(777.0, n, 0.9) {
            let y = d.process(x);
            if y == prev {
                held_runs += 1;
            }
            prev = y;
        }
        // Roughly 3 of every 4 samples are repeats of the held value.
        assert!(
            held_runs as f32 / n as f32 > 0.6,
            "downsample x4 should repeat held samples a lot, got ratio {}",
            held_runs as f32 / n as f32
        );
    }

    #[test]
    fn downsample_1x_is_bypass() {
        let mut d = Drive::new(SR);
        d.set_params(true, DriveKind::Clean, 0.0, 0.5, 16.0, 1.0, 1.0, 1.0);
        for x in sine(640.0, 2_000, 0.8) {
            assert!(
                (d.process(x) - x).abs() < 1e-6,
                "downsample 1x with clean chain must be transparent"
            );
        }
    }

    #[test]
    fn tone_brighter_has_more_high_frequency_energy() {
        // Compare high-band energy of a bright vs. dark tone setting on the same
        // driven signal. Bright must have more energy up high.
        fn high_band_energy(samples: &[f32]) -> f32 {
            // crude high-pass: energy of the first difference (emphasises highs).
            let mut e = 0.0f32;
            for w in samples.windows(2) {
                let d = w[1] - w[0];
                e += d * d;
            }
            e
        }

        let render = |tone: f32| -> Vec<f32> {
            let mut d = Drive::new(SR);
            d.set_params(true, DriveKind::Tube, 0.5, tone, 16.0, 1.0, 1.0, 1.0);
            // A signal with both low and high content (sine + its 8th harmonic).
            let step1 = 150.0 / SR;
            let step2 = 1_200.0 / SR;
            let mut p1 = 0.0f32;
            let mut p2 = 0.0f32;
            (0..8_000)
                .map(|_| {
                    let s = (p1 * TAU).sin() * 0.6 + (p2 * TAU).sin() * 0.4;
                    p1 = (p1 + step1).fract();
                    p2 = (p2 + step2).fract();
                    d.process(s)
                })
                .collect()
        };

        let dark = high_band_energy(&render(0.0));
        let bright = high_band_energy(&render(1.0));
        assert!(
            bright > dark * 1.2,
            "bright tone should have more HF energy than dark: dark={dark}, bright={bright}"
        );
    }

    #[test]
    fn mix_zero_is_dry() {
        // mix=0 must return the dry input regardless of how violent the wet path
        // is configured.
        let mut d = Drive::new(SR);
        d.set_params(true, DriveKind::RatFuzz, 1.0, 0.0, 1.0, 50.0, 1.0, 0.0);
        for x in sine(440.0, 2_000, 0.9) {
            assert!(
                (d.process(x) - x).abs() < 1e-6,
                "mix=0 must be fully dry"
            );
        }
    }

    #[test]
    fn crush_mix_zero_keeps_saturation_without_grit() {
        // crush_mix=0 should mean: keep the saturated+toned signal, no crush/
        // downsample applied — even though bits/downsample are extreme.
        let mut d = Drive::new(SR);
        d.set_params(true, DriveKind::Tube, 0.6, 0.5, 1.0, 50.0, 0.0, 1.0);
        // Reference: same config but crush bypassed (16 bits, ds 1) → should match.
        let mut ref_d = Drive::new(SR);
        ref_d.set_params(true, DriveKind::Tube, 0.6, 0.5, 16.0, 1.0, 1.0, 1.0);
        let mut max_diff = 0.0f32;
        for x in sine(330.0, 4_000, 0.8) {
            let a = d.process(x);
            let b = ref_d.process(x);
            max_diff = max_diff.max((a - b).abs());
        }
        assert!(
            max_diff < 1e-5,
            "crush_mix=0 should bypass the crush path, max diff {max_diff}"
        );
    }

    #[test]
    fn set_sample_rate_clears_state() {
        let mut d = Drive::new(SR);
        d.set_params(true, DriveKind::Tube, 1.0, 0.5, 4.0, 8.0, 1.0, 1.0);
        for x in sine(440.0, 1_000, 0.9) {
            let _ = d.process(x);
        }
        d.set_sample_rate(96_000.0);
        // After a rate change, the very first output of silence must be silence
        // (no stale hold/filter state leaking through).
        let y = d.process(0.0);
        assert!(y.abs() < 1e-6, "state not cleared on set_sample_rate: {y}");
    }

    /// REGRESSION: the tone-tilt one-pole (`tone_lp`) is recursive, so a single
    /// non-finite input used to latch a NaN into it permanently (the `+=`
    /// accumulation never self-heals, unlike the overwrite-style S&H/`prev_in`
    /// state). The `flush_denormal` on the `tone_lp` write folds the bad value to
    /// 0.0, so the output returns to finite within a couple of samples and never
    /// re-poisons. (Drive is fed a flushed synth signal in the plugin, but this
    /// guards the module in isolation as defense in depth.)
    #[test]
    fn recovers_from_injected_nan_and_inf() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut d = Drive::new(SR);
            // A hot config that exercises every stage (shaper + tone + crush + ds).
            d.set_params(true, DriveKind::Tube, 0.9, 0.7, 4.0, 8.0, 0.8, 1.0);
            for x in sine(220.0, 2_000, 0.6) {
                let _ = d.process(x);
            }
            // Inject one poison sample, then feed clean audio.
            let _ = d.process(bad);
            // The oversampler's 1-sample delay can carry the bad value forward at
            // most one more sample; after that the flush must have purged it.
            let _ = d.process(0.2);
            for i in 0..10_000 {
                let y = d.process(0.2);
                assert!(
                    y.is_finite(),
                    "drive emitted non-finite after injecting {bad} at i={i}: {y}"
                );
            }
        }
    }
}

