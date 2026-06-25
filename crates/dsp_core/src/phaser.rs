//! A stereo multi-stage **phaser**.
//!
//! A phaser is built from a chain of **all-pass filters**. An all-pass passes
//! every frequency at full volume but *rotates its phase* — it delays some
//! frequencies more than others without changing their level. On its own that is
//! inaudible. The magic happens when we add the all-pass chain's output back to
//! the dry signal: at the frequencies where the chain has rotated the phase by
//! 180°, the wet and dry cancel, punching a **notch** in the spectrum. It takes
//! two first-order all-pass stages to swing the phase through a full 180°, so an
//! `N`-stage phaser carves `N / 2` notches.
//!
//! An LFO slowly sweeps every stage's break frequency up and down. The notches
//! glide through the spectrum with it, and *that* sweeping comb is the classic
//! whooshing phaser sound — Vangelis pads breathing, Ratatat leads shimmering.
//!
//! ## Stereo width
//!
//! We run an independent all-pass chain per channel and offset the right
//! channel's LFO from the left's by a `spread` angle (the same quadrature trick
//! the [`Chorus`](crate::chorus::Chorus) uses to open up a mono source). With
//! `spread = 90°` the two sides' notches sweep a quarter-cycle apart, so the
//! left and right ears hear the whoosh move at different moments — a wide,
//! enveloping stereo image from a mono input.
//!
//! ## Feedback
//!
//! Routing the chain's output back into its input sharpens and deepens the
//! notches into resonant peaks (the "Univibe"/"Small Stone" character). The
//! feedback knob is **bipolar**: positive feedback emphasises one set of notches,
//! negative the set in between, for two distinct flavours. We clamp the feedback
//! magnitude safely below 1.0 so the loop can never run away.
//!
//! ## Real-time safety
//!
//! All state is fixed-size: a `[f32; MAX_STAGES]` of all-pass memory per channel,
//! sized for the largest stage count at compile time. [`Phaser::new`] does the
//! only allocation; [`Phaser::process`] is pure float math — no allocation, no
//! locks, no `Vec` growth.

use crate::util::flush_denormal;
use std::f32::consts::PI;

/// The most all-pass stages we ever run per channel. The `stages` setter accepts
/// 2, 4, 6, 8 or 12; we size the per-channel state arrays for the maximum so a
/// stage-count change is just a different loop bound, never a reallocation.
const MAX_STAGES: usize = 12;

/// Lowest break frequency the swept notches are allowed to reach, in Hz. Keeps
/// the all-pass prewarp `tan` away from zero and the notches in audible territory.
const MIN_FC_HZ: f32 = 20.0;

/// How close to Nyquist a swept break frequency may go, as a fraction of the
/// sample rate. `tan(pi * fc / fs)` runs toward infinity as `fc -> fs/2`, so we
/// stop a hair short (0.49 * fs) to keep the coefficient finite and the chain
/// numerically sane at the very top of the sweep.
const MAX_FC_FRACTION: f32 = 0.49;

/// How wide the LFO sweeps the break frequency, expressed in octaves at full
/// `depth`. The sweep is geometric (octaves), so the notches glide evenly in
/// pitch rather than bunching up at the low end. `±2` octaves at depth 1.0 is a
/// big, obvious whoosh; smaller depths narrow it around the centre.
const MAX_DEPTH_OCTAVES: f32 = 2.0;

/// Hard ceiling on the feedback coefficient magnitude. The all-pass chain has
/// unity gain, so a feedback magnitude of exactly 1.0 would sit on the edge of
/// self-oscillation; clamping a touch below keeps the resonant peaks dramatic but
/// the loop unconditionally stable and finite.
const MAX_FEEDBACK: f32 = 0.97;

/// One first-order all-pass stage's worth of state: a single integrator memory.
///
/// We use the **TPT** (topology-preserving transform) one-pole form, the same
/// zero-delay-feedback approach as [`Filter`](crate::filter::Filter): compute the
/// low-pass output `lp` from the input and the stored state `s`, then form the
/// all-pass as `ap = 2 * lp - x`. This stays accurate and stable right up to
/// Nyquist, unlike the naive direct-form all-pass which detunes near the top.
#[derive(Clone, Copy, Debug, Default)]
struct AllpassState {
    /// The integrator's stored "equivalent charge" carried to the next sample.
    s: f32,
}

impl AllpassState {
    /// Process one sample through a first-order all-pass with prewarped
    /// coefficient `g = tan(pi * fc / fs)`.
    ///
    /// `a = g / (1 + g)` is the TPT one-pole's loop-solving gain; `lp` is the
    /// low-pass output and `2*lp - x` is the all-pass (it shares the same two
    /// states as a state-variable filter, so the all-pass is "free"). The phase
    /// rotates from 0° at DC through −90° at the break frequency to −180° high up.
    #[inline]
    fn process(&mut self, x: f32, g: f32) -> f32 {
        // Solve the zero-delay one-pole loop in closed form.
        let v = (x - self.s) * g / (1.0 + g);
        let lp = v + self.s;
        // Trapezoidal integrator update (same rule as the SVF): new charge is
        // twice the integrator output minus the old charge. Flush so a non-finite
        // value can't latch into this integrator — the chain is only reset on a
        // sample-rate change, so a stuck NaN would otherwise live for the whole
        // plugin instance.
        self.s = flush_denormal(lp + v);
        // First-order all-pass = 2*lowpass - input. Unity magnitude, swept phase.
        2.0 * lp - x
    }

    #[inline]
    fn reset(&mut self) {
        self.s = 0.0;
    }
}

/// One channel's all-pass chain plus its feedback memory.
#[derive(Clone, Debug)]
struct Channel {
    /// Fixed-size all-pass state, one slot per possible stage. Slots beyond the
    /// active `stages` count are simply not visited.
    stages: [AllpassState; MAX_STAGES],
    /// Last sample fed back from the chain output into the chain input.
    feedback_sample: f32,
}

impl Channel {
    fn new() -> Self {
        Self {
            stages: [AllpassState::default(); MAX_STAGES],
            feedback_sample: 0.0,
        }
    }

    fn reset(&mut self) {
        for st in &mut self.stages {
            st.reset();
        }
        self.feedback_sample = 0.0;
    }
}

/// A stereo, multi-stage, LFO-swept all-pass phaser.
///
/// Create one, set the sample rate, push already-mapped engineering units in via
/// [`Phaser::set_params`], then call [`Phaser::process`] once per stereo sample
/// pair. When disabled (`on = false`) it passes the input through untouched at
/// zero CPU beyond the branch.
///
/// Real-time safe: the only allocation is the two per-channel chains built in
/// [`Phaser::new`]; [`Phaser::process`] does pure float math on existing fields.
#[derive(Clone, Debug)]
pub struct Phaser {
    sample_rate: f32,

    // --- Parameters (already mapped to engineering units by the caller) ---
    /// Master enable. `false` is a clean bypass.
    on: bool,
    /// Active all-pass stage count, clamped to an even number in `2..=MAX_STAGES`.
    stages: usize,
    /// LFO sweep rate in Hz.
    rate_hz: f32,
    /// Sweep width in `0.0..=1.0`; scales [`MAX_DEPTH_OCTAVES`] around the centre.
    depth: f32,
    /// Bipolar feedback in `-MAX_FEEDBACK..=MAX_FEEDBACK`.
    feedback: f32,
    /// Centre (mid-sweep) break frequency in Hz, clamped to a safe range.
    center_hz: f32,
    /// Right-channel LFO offset as a fraction of a cycle (`spread_deg / 360`).
    spread_fraction: f32,
    /// Wet/dry blend, `0.0` = all dry, `1.0` = all phased.
    mix: f32,

    // --- State ---
    /// Shared LFO phase in `0.0..1.0`; the right channel reads it offset.
    lfo_phase: f32,
    left: Channel,
    right: Channel,
}

impl Phaser {
    /// Create a phaser at `sample_rate`, configured to a sensible default sweep
    /// (4 stages, 0.5 Hz, moderate depth and feedback, 1 kHz centre, 90° spread,
    /// 50% mix) but **disabled**, so it is an exact bypass until switched on.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            on: false,
            stages: 4,
            rate_hz: 0.5,
            depth: 0.6,
            feedback: 0.3,
            center_hz: 1_000.0,
            spread_fraction: 0.25, // 90° / 360°
            mix: 0.5,
            lfo_phase: 0.0,
            left: Channel::new(),
            right: Channel::new(),
        }
    }

    /// Change the sample rate. Clears all all-pass and feedback memory so a rate
    /// change can't replay stale, now-mis-scaled energy as a click, and resets the
    /// LFO so left/right stay in their intended phase relationship.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.reset();
    }

    /// Clear all all-pass and feedback memory (and the LFO phase) without touching
    /// the sample rate or parameters. Used by the plugin to flush the phaser on a
    /// scene/world switch so stale (or, worst case, non-finite) feedback state
    /// can't carry into the new world.
    pub fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.lfo_phase = 0.0;
    }

    /// Push a fresh block of parameters, already mapped to engineering units.
    ///
    /// * `on` — master enable; `false` bypasses.
    /// * `stages` — all-pass stage count; rounded to the nearest even value in
    ///   `2..=12` (an odd count can't form complete 180° notches).
    /// * `rate_hz` — LFO sweep rate.
    /// * `depth` — sweep width `0..1` (scales [`MAX_DEPTH_OCTAVES`]).
    /// * `feedback` — bipolar resonance `-1..1`, clamped to ±[`MAX_FEEDBACK`].
    /// * `center_hz` — mid-sweep break frequency, clamped to a safe range.
    /// * `spread_deg` — right-channel LFO offset in degrees (stereo width).
    /// * `mix` — wet/dry blend `0..1`.
    ///
    /// Every value is clamped/sanitised here so wild host automation (negatives,
    /// NaN, absurd magnitudes) can never poison the audio path.
    #[allow(clippy::too_many_arguments)]
    pub fn set_params(
        &mut self,
        on: bool,
        stages: usize,
        rate_hz: f32,
        depth: f32,
        feedback: f32,
        center_hz: f32,
        spread_deg: f32,
        mix: f32,
    ) {
        self.on = on;

        // Snap to an even stage count in [2, MAX_STAGES]. Odd counts can't make a
        // clean notch (you need a full 180° = two stages per notch).
        let clamped = stages.clamp(2, MAX_STAGES);
        self.stages = clamped & !1; // round down to even; 2 stays 2, 12 stays 12

        self.rate_hz = sanitize(rate_hz, 0.5).clamp(0.0, 50.0);
        self.depth = sanitize(depth, 0.0).clamp(0.0, 1.0);

        // Bipolar feedback, clamped to a stable magnitude.
        self.feedback = sanitize(feedback, 0.0).clamp(-MAX_FEEDBACK, MAX_FEEDBACK);

        self.center_hz = Self::clamp_fc(sanitize(center_hz, 1_000.0), self.sample_rate);

        // Degrees -> fraction of a cycle, wrapped into [0, 1).
        let frac = sanitize(spread_deg, 90.0) / 360.0;
        self.spread_fraction = frac.rem_euclid(1.0);

        self.mix = sanitize(mix, 0.5).clamp(0.0, 1.0);
    }

    /// Fold a requested break frequency into the safe `20 Hz .. ~0.98*Nyquist`
    /// window. `hz.max(MIN_FC_HZ)` also turns a NaN into the floor for free
    /// (`NaN.max(x) == x`).
    fn clamp_fc(hz: f32, sample_rate: f32) -> f32 {
        let max_fc = (sample_rate * MAX_FC_FRACTION).max(MIN_FC_HZ);
        hz.max(MIN_FC_HZ).min(max_fc)
    }

    /// Process one stereo sample pair, returning the wet/dry-blended `(left,
    /// right)`. A bypass (`on == false`) returns the input verbatim.
    ///
    /// Real-time safe: pure float math on existing fields. One `tan` per stage per
    /// channel per sample (the swept coefficient), which is why the stage count is
    /// capped at 12 — comfortably cheap for a single master-bus effect.
    #[inline]
    pub fn process(&mut self, left_in: f32, right_in: f32) -> (f32, f32) {
        if !self.on {
            return (left_in, right_in);
        }

        // --- Advance the shared LFO once per sample ---
        // A triangle LFO (derived from the phase ramp) sweeps the break frequency
        // symmetrically; we read it for the left channel and again, phase-offset,
        // for the right to open up the stereo image.
        let lfo_left = triangle(self.lfo_phase);
        let lfo_right = triangle(self.lfo_phase + self.spread_fraction);

        self.lfo_phase += self.rate_hz / self.sample_rate;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }

        let out_left = Self::process_channel(
            &mut self.left,
            left_in,
            lfo_left,
            self.stages,
            self.depth,
            self.feedback,
            self.center_hz,
            self.sample_rate,
            self.mix,
        );
        let out_right = Self::process_channel(
            &mut self.right,
            right_in,
            lfo_right,
            self.stages,
            self.depth,
            self.feedback,
            self.center_hz,
            self.sample_rate,
            self.mix,
        );

        (out_left, out_right)
    }

    /// Push one sample through a single channel's all-pass chain and blend.
    ///
    /// `lfo` is this channel's bipolar LFO value in `-1..=1`; it shifts the break
    /// frequency `±depth * MAX_DEPTH_OCTAVES` octaves around `center_hz`. The
    /// chain output is fed back (scaled by `feedback`) into its own input to
    /// sharpen the notches into resonant peaks. The whole channel is `static` (an
    /// associated function) so the borrow checker is happy taking `&mut left` and
    /// `&mut right` from the same `self` without aliasing.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn process_channel(
        ch: &mut Channel,
        input: f32,
        lfo: f32,
        stages: usize,
        depth: f32,
        feedback: f32,
        center_hz: f32,
        sample_rate: f32,
        mix: f32,
    ) -> f32 {
        // Geometric sweep: shift the break frequency by a number of octaves set by
        // depth and the LFO. `2^(octaves)` keeps the notch motion even in pitch.
        let octaves = lfo * depth * MAX_DEPTH_OCTAVES;
        let fc = Self::clamp_fc(center_hz * 2.0_f32.powf(octaves), sample_rate);
        let g = (PI * fc / sample_rate).tan();

        // Feedback: add a scaled copy of last sample's chain output to the input.
        // The all-pass chain has unity gain and `|feedback| < 1`, so the loop is
        // bounded; we leave the *dry* signal out of the feedback path so feedback
        // only ever colours the wet chain.
        let mut x = input + feedback * ch.feedback_sample;

        // Run the active all-pass stages. Untouched slots beyond `stages` keep
        // their (zeroed) state and cost nothing.
        for st in ch.stages.iter_mut().take(stages) {
            x = st.process(x, g);
        }

        // Stash the chain output for next sample's feedback term, flushed so a
        // non-finite value can't recirculate through the feedback path forever.
        ch.feedback_sample = flush_denormal(x);

        // Classic phaser blend: dry + wet. At mix = 0.5 the equal sum gives the
        // deepest cancellation notches; toward mix = 1.0 the dry shrinks and the
        // all-pass (a flat, phasey timbre) dominates. We crossfade dry/wet so the
        // output stays bounded at all settings.
        let dry_gain = 1.0 - mix;
        dry_gain * input + mix * x
    }

    /// The current active stage count (after even-snapping). Handy for tests/UI.
    pub fn stages(&self) -> usize {
        self.stages
    }

    /// Whether the phaser is currently enabled.
    pub fn is_on(&self) -> bool {
        self.on
    }
}

/// Replace a non-finite (NaN/Inf) value with `fallback`, pass anything else
/// through. A tiny guard so one bad automation value can't reach the DSP and
/// poison the all-pass state (which would then ring forever as NaN).
#[inline]
fn sanitize(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

/// A unit triangle LFO from a `0..1` phase ramp, returning `-1..=1`.
///
/// A triangle (rather than a sine) gives the notches a steady, linear-in-octaves
/// glide for most of the cycle — the familiar "scanning" phaser motion — and it
/// is a couple of cheap ops instead of a `sin`. The input phase is wrapped first
/// so callers can pass `phase + offset` without pre-folding.
#[inline]
fn triangle(phase: f32) -> f32 {
    let p = phase.rem_euclid(1.0);
    // Ramp 0->1->0 over the cycle, then map to -1..1.
    // |2p - 1| is a 1->0->1 valley; 1 - 2*that is a -1->1->-1 triangle.
    1.0 - 2.0 * (2.0 * p - 1.0).abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const SR: f32 = 48_000.0;

    /// Helper: configure a phaser and run a sine through it, returning the peak
    /// absolute output across both channels after a short warm-up.
    fn run_sine(phaser: &mut Phaser, freq: f32, samples: usize) -> f32 {
        let warmup = samples / 8;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for i in 0..samples {
            let x = (phase * TAU).sin();
            phase = (phase + freq / SR).fract();
            let (l, r) = phaser.process(x, x);
            if i >= warmup {
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        peak
    }

    /// Across the full parameter space — every stage count, rate, depth, the full
    /// bipolar feedback range, extreme centres and spreads — the output must stay
    /// finite and bounded, for sine, silence, DC and pathological inputs.
    #[test]
    fn output_is_finite_and_bounded_across_full_sweep() {
        let stage_counts = [2usize, 4, 6, 8, 12];
        let rates = [0.0f32, 0.02, 0.5, 5.0, 10.0];
        let depths = [0.0f32, 0.5, 1.0];
        let feedbacks = [-1.0f32, -0.5, 0.0, 0.5, 1.0]; // ±1 gets clamped internally
        let centers = [50.0f32, 1_000.0, 8_000.0];
        let spreads = [0.0f32, 90.0, 180.0];

        for &stages in &stage_counts {
            for &rate in &rates {
                for &depth in &depths {
                    for &fb in &feedbacks {
                        for &center in &centers {
                            for &spread in &spreads {
                                let mut p = Phaser::new(SR);
                                p.set_params(true, stages, rate, depth, fb, center, spread, 0.5);

                                let mut phase = 0.0f32;
                                for i in 0..2_000 {
                                    // Mix of inputs over the run: sine, silence,
                                    // DC, and a big spike, to probe the feedback
                                    // loop's stability.
                                    let x = match i % 200 {
                                        0 => 8.0,   // a hot transient
                                        1..=50 => 0.0,
                                        _ => (phase * TAU).sin(),
                                    };
                                    phase = (phase + 440.0 / SR).fract();
                                    let (l, r) = p.process(x, x);
                                    assert!(
                                        l.is_finite() && r.is_finite(),
                                        "non-finite at stages={stages}, rate={rate}, depth={depth}, fb={fb}, center={center}, spread={spread}"
                                    );
                                    assert!(
                                        l.abs() < 50.0 && r.abs() < 50.0,
                                        "unbounded ({l},{r}) at stages={stages}, fb={fb}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// `on = false` must be a sample-exact bypass: output equals input on both
    /// channels regardless of the other parameters.
    #[test]
    fn disabled_is_exact_bypass() {
        let mut p = Phaser::new(SR);
        p.set_params(false, 8, 1.0, 1.0, 0.9, 1_000.0, 90.0, 1.0);
        let mut phase = 0.0f32;
        for _ in 0..4_000 {
            let x = (phase * TAU).sin();
            phase = (phase + 330.0 / SR).fract();
            let (l, r) = p.process(x, x * 0.5);
            assert_eq!(l, x);
            assert_eq!(r, x * 0.5);
        }
    }

    /// The notches must actually *move*: with the LFO sweeping, the instantaneous
    /// output level of a steady tone sitting near a notch should vary over time.
    /// We feed a fixed sine and confirm the wet output is not a constant-amplitude
    /// copy — i.e. the phaser is modulating it.
    #[test]
    fn sweep_modulates_a_steady_tone() {
        let mut p = Phaser::new(SR);
        // Strong, obvious sweep with deep notches (lots of stages + feedback).
        p.set_params(true, 12, 2.0, 1.0, 0.7, 800.0, 90.0, 0.5);

        // Track the per-sample envelope (cheap peak follower) of the left output
        // and record its min and max once warmed up. A static (un-swept) signal
        // would give min ≈ max; a sweeping notch makes them diverge.
        let mut phase = 0.0f32;
        let mut env = 0.0f32;
        let mut env_min = f32::MAX;
        let mut env_max = 0.0f32;
        for i in 0..(SR as usize) {
            let x = (phase * TAU).sin();
            phase = (phase + 750.0 / SR).fract(); // sit a tone right in the sweep band
            let (l, _r) = p.process(x, x);
            // One-pole peak follower.
            let rectified = l.abs();
            env = if rectified > env {
                rectified
            } else {
                env * 0.999 + rectified * 0.001
            };
            if i > (SR as usize) / 4 {
                env_min = env_min.min(env);
                env_max = env_max.max(env);
            }
        }
        assert!(
            env_max - env_min > 0.05,
            "expected the sweep to modulate the tone's level, got min={env_min}, max={env_max}"
        );
    }

    /// The quadrature spread must produce a genuinely stereo result: with a mono
    /// input and a non-zero spread the two output channels should differ over the
    /// sweep. With zero spread they must be identical.
    #[test]
    fn spread_creates_stereo_difference() {
        // Zero spread -> identical channels (mono in, mono-correlated out).
        let mut mono = Phaser::new(SR);
        mono.set_params(true, 8, 1.0, 1.0, 0.5, 1_000.0, 0.0, 0.5);
        let mut phase = 0.0f32;
        let mut max_diff_mono = 0.0f32;
        for _ in 0..(SR as usize) {
            let x = (phase * TAU).sin();
            phase = (phase + 440.0 / SR).fract();
            let (l, r) = mono.process(x, x);
            max_diff_mono = max_diff_mono.max((l - r).abs());
        }
        assert!(
            max_diff_mono < 1.0e-6,
            "zero spread should keep channels identical, got diff {max_diff_mono}"
        );

        // 90° spread -> the channels diverge as the notches sweep out of phase.
        let mut wide = Phaser::new(SR);
        wide.set_params(true, 8, 1.0, 1.0, 0.5, 1_000.0, 90.0, 0.5);
        let mut phase = 0.0f32;
        let mut max_diff_wide = 0.0f32;
        for _ in 0..(SR as usize) {
            let x = (phase * TAU).sin();
            phase = (phase + 440.0 / SR).fract();
            let (l, r) = wide.process(x, x);
            max_diff_wide = max_diff_wide.max((l - r).abs());
        }
        assert!(
            max_diff_wide > 0.01,
            "90° spread should make the channels differ, got {max_diff_wide}"
        );
    }

    /// At some point in its sweep the phaser must carve a real notch: a tone that
    /// passes nearly untouched at one moment should be clearly attenuated at
    /// another. We hold a fixed tone and confirm the wet output dips well below
    /// the dry level somewhere in the cycle (deep cancellation = a working notch).
    #[test]
    fn carves_an_audible_notch() {
        let mut p = Phaser::new(SR);
        // 4 stages = 2 notches, deep mix, modest feedback, slow sweep so the
        // notch definitely passes across our test tone.
        p.set_params(true, 4, 0.5, 1.0, 0.0, 1_000.0, 90.0, 0.5);

        // Probe several tones; at least one should see a deep notch as the sweep
        // passes through it.
        let mut deepest_dip = 1.0f32; // ratio of min to nominal; lower = deeper notch
        for &freq in &[300.0f32, 700.0, 1_500.0, 3_000.0] {
            // Reset state between tones.
            p.set_sample_rate(SR);
            p.set_params(true, 4, 0.5, 1.0, 0.0, 1_000.0, 90.0, 0.5);

            let mut phase = 0.0f32;
            let mut env = 0.0f32;
            let mut env_min = f32::MAX;
            // Two full LFO cycles (rate 0.5 Hz -> 2 s each) so the notch sweeps
            // through the tone.
            for i in 0..(SR as usize * 4) {
                let x = (phase * TAU).sin();
                phase = (phase + freq / SR).fract();
                let (l, _r) = p.process(x, x);
                let rect = l.abs();
                env = if rect > env { rect } else { env * 0.9995 + rect * 0.0005 };
                if i > SR as usize {
                    env_min = env_min.min(env);
                }
            }
            deepest_dip = deepest_dip.min(env_min);
        }
        // A genuine notch should pull the level well under the ~0.5 nominal wet+dry
        // (mix 0.5) — at least a few dB of cancellation somewhere in the sweep.
        assert!(
            deepest_dip < 0.35,
            "expected a real cancellation notch (dip below 0.35), got {deepest_dip}"
        );
    }

    /// Even with maximum feedback and a hot, sustained input — the worst case for
    /// the resonant loop — the phaser must not blow up. (Internally feedback is
    /// clamped below 1.0 precisely to guarantee this.)
    #[test]
    fn max_feedback_stays_stable() {
        for &fb in &[1.0f32, -1.0] {
            let mut p = Phaser::new(SR);
            p.set_params(true, 12, 3.0, 1.0, fb, 1_000.0, 90.0, 1.0);
            let peak = run_sine(&mut p, 1_000.0, SR as usize * 2);
            assert!(peak.is_finite(), "feedback {fb} produced non-finite output");
            assert!(peak < 50.0, "feedback {fb} resonance unbounded: {peak}");
        }
    }

    /// NaN / infinite parameter values must be rejected at the setter so they
    /// never reach the all-pass state. After feeding garbage params the phaser
    /// must still produce finite audio.
    #[test]
    fn params_are_sanitized() {
        let mut p = Phaser::new(SR);
        p.set_params(
            true,
            8,
            f32::NAN,
            f32::INFINITY,
            f32::NAN,
            f32::NAN,
            f32::INFINITY,
            f32::NAN,
        );
        // Stage count clamps; even an out-of-range usize is folded.
        assert!(p.stages() >= 2 && p.stages() <= MAX_STAGES);
        let out = run_sine(&mut p, 440.0, 4_000);
        assert!(out.is_finite(), "sanitized params still produced non-finite audio");

        // Odd / huge stage counts snap to a valid even count.
        p.set_params(true, 7, 1.0, 0.5, 0.3, 1_000.0, 90.0, 0.5);
        assert_eq!(p.stages() % 2, 0);
        assert!(p.stages() <= MAX_STAGES);
        p.set_params(true, 999, 1.0, 0.5, 0.3, 1_000.0, 90.0, 0.5);
        assert_eq!(p.stages(), MAX_STAGES);
        p.set_params(true, 0, 1.0, 0.5, 0.3, 1_000.0, 90.0, 0.5);
        assert_eq!(p.stages(), 2);
    }

    /// A `mix` of 0 must leave the dry signal untouched (the wet is fully dialled
    /// out) even while the phaser engine runs internally.
    #[test]
    fn zero_mix_is_dry() {
        let mut p = Phaser::new(SR);
        p.set_params(true, 8, 1.0, 1.0, 0.5, 1_000.0, 90.0, 0.0);
        let mut phase = 0.0f32;
        for _ in 0..4_000 {
            let x = (phase * TAU).sin();
            phase = (phase + 440.0 / SR).fract();
            let (l, r) = p.process(x, x);
            assert!((l - x).abs() < 1.0e-6, "mix=0 should pass dry left, got {l} vs {x}");
            assert!((r - x).abs() < 1.0e-6, "mix=0 should pass dry right, got {r} vs {x}");
        }
    }

    /// The triangle LFO helper stays in `[-1, 1]` and is continuous/periodic,
    /// including for phases pushed past 1.0 by the spread offset.
    #[test]
    fn triangle_lfo_is_bounded_and_periodic() {
        for k in 0..1000 {
            let phase = k as f32 / 250.0; // spans several cycles, > 1.0
            let v = triangle(phase);
            assert!((-1.0..=1.0).contains(&v), "triangle out of range: {v} at {phase}");
        }
        // Periodicity: value at p and p+1 match.
        for k in 0..100 {
            let p = k as f32 / 100.0;
            assert!((triangle(p) - triangle(p + 1.0)).abs() < 1.0e-6);
        }
    }

    /// REGRESSION: a non-finite *signal* injected into the phaser must not latch.
    /// The all-pass integrators (`s`) and per-channel `feedback_sample` are only
    /// cleared on a sample-rate change, so without the per-sample flush a single
    /// NaN would live for the whole plugin instance. After the fix the phaser must
    /// return to finite output on silence within a short window.
    #[test]
    fn recovers_from_injected_nan_and_inf() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut p = Phaser::new(SR);
            // Hot, resonant setting to stress the feedback path.
            p.set_params(true, 12, 2.0, 1.0, 0.9, 1_000.0, 90.0, 0.5);

            // Warm up with a tone, then inject the poison sample on both channels.
            let mut phase = 0.0f32;
            for _ in 0..2_000 {
                let x = (phase * TAU).sin();
                phase = (phase + 440.0 / SR).fract();
                let _ = p.process(x, x);
            }
            let _ = p.process(bad, bad);

            // Silence should flush the all-pass + feedback state within a few
            // hundred samples.
            let mut last = (1.0f32, 1.0f32);
            for _ in 0..2_000 {
                last = p.process(0.0, 0.0);
            }
            assert!(
                last.0.is_finite() && last.1.is_finite(),
                "phaser did not recover to finite output after injecting {bad}"
            );
        }
    }

    /// After excitation then SILENCE the feedback path must decay to *exactly*
    /// 0.0 — no denormal recirculating through `feedback_sample`/the all-pass
    /// states forever. `flush_denormal` on those writes guarantees it.
    #[test]
    fn tail_decays_to_exactly_zero_on_silence() {
        let mut p = Phaser::new(SR);
        p.set_params(true, 8, 1.0, 1.0, 0.9, 1_000.0, 0.0, 1.0); // mix=1, zero spread
        // Excite with an impulse + short burst.
        let _ = p.process(1.0, 1.0);
        let mut phase = 0.0f32;
        for _ in 0..500 {
            let x = (phase * TAU).sin();
            phase = (phase + 330.0 / SR).fract();
            let _ = p.process(x, x);
        }
        // Feed silence long enough for the resonant feedback to fully die.
        let mut last = (1.0f32, 1.0f32);
        for _ in 0..(SR as usize * 2) {
            last = p.process(0.0, 0.0);
        }
        assert_eq!(last.0, 0.0, "phaser left tail did not reach exactly 0: {}", last.0);
        assert_eq!(last.1, 0.0, "phaser right tail did not reach exactly 0: {}", last.1);
    }
}
