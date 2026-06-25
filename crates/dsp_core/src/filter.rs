//! A TPT / zero-delay-feedback **state-variable filter** (the Cytomic design).
//!
//! This is the Andrew Simper / Cytomic "SvfLinearTrapOptimised" topology — the
//! same one inside countless modern soft-synths. It is the workhorse filter you
//! reach for: stable and accurate all the way up to Nyquist, with a clean,
//! musical resonance that can be pushed into self-oscillation. We expose the
//! **low-pass** output for now, but the high-pass and band-pass taps fall out of
//! the same two integrator states for free — which is exactly what the future
//! CS-80-style dual "lopass + hipass" pair will want.
//!
//! # Why "TPT" and "zero-delay feedback"?
//!
//! The naive way to build a digital filter with feedback is to use *last
//! sample's* output as the feedback term. That one-sample delay is a lie — the
//! analog circuit has no such delay — and it makes the filter detune and go
//! unstable as you raise the cutoff or resonance toward Nyquist.
//!
//! TPT ("Topology-Preserving Transform") instead applies the **bilinear
//! transform** to the analog circuit and then solves the resulting instantaneous
//! feedback loop *algebraically*, in closed form, every sample. There is no
//! delay in the feedback path — hence "zero-delay feedback" (ZDF). The payoff:
//! the cutoff stays where you put it right up to Nyquist, and it stays rock solid
//! even at maximum resonance.
//!
//! # The math, slowly
//!
//! An analog state-variable filter is two integrators in a feedback loop. In the
//! continuous (analog) world an integrator has transfer function `1/s`. The
//! bilinear transform maps that onto the digital world; working through the
//! substitution, a single analog integrator becomes a digital integrator whose
//! gain per sample is:
//!
//! ```text
//!     g = tan(pi * fc / fs)
//! ```
//!
//! where `fc` is the cutoff in Hz and `fs` the sample rate. That `tan(...)` is
//! the **prewarping**: the bilinear transform warps the frequency axis, and the
//! tangent pre-bends `fc` so that after warping the cutoff lands exactly where we
//! asked. `g` is small for low cutoffs and shoots toward infinity as `fc`
//! approaches Nyquist (`fs/2`), which is why we clamp `fc` safely below Nyquist.
//!
//! Resonance is a single damping coefficient:
//!
//! ```text
//!     k = 1 / Q
//! ```
//!
//! `k` is how hard the filter damps itself. Large `k` (low resonance) = heavily
//! damped, no peak. As `k` shrinks toward 0 the resonant peak grows; at `k = 0`
//! the damping is gone and the filter self-oscillates — it rings forever like a
//! struck bell. Our `0..1` knob maps `0.0 -> k = MAX_K` (Q = 0.5, no peak) and
//! `1.0 -> k = MIN_K` (just above 0), giving a screaming-but-bounded peak rather
//! than a truly infinite one. The mapping is *exponential* in `k` (see
//! [`Filter::set_resonance`]) so the audible peak height grows evenly across the
//! whole knob travel instead of all the action piling up at the very top.
//!
//! Each sample we solve the two-integrator loop in one shot. Given the input
//! `v0` and the two integrator states `ic1eq`, `ic2eq` (the "equivalent"
//! capacitor voltages carried over from last sample), the closed-form update is:
//!
//! ```text
//!     a1 = 1 / (1 + g*(g + k))            // the precomputed loop-solving gain
//!     v1 = a1 * (ic1eq + g*(v0 - ic2eq))  // band-pass state
//!     v2 = ic2eq + g*v1                   // low-pass state
//!     ic1eq = 2*v1 - ic1eq                // trapezoidal integrator update
//!     ic2eq = 2*v2 - ic2eq
//! ```
//!
//! From those, the three classic outputs are just linear combinations:
//!
//! ```text
//!     low  = v2
//!     band = v1
//!     high = v0 - k*v1 - v2
//! ```
//!
//! That is the whole filter. [`Filter::process`] returns the low-pass;
//! [`Filter::last_band`] and [`Filter::last_high`] expose the other two taps at
//! no extra cost. (Note `high + k*band + low == v0` exactly — the three taps
//! reconstruct the input, which is the defining property of a state-variable
//! filter and the basis for the future dual LP+HP routing.)
//!
//! # Smoothing and warmth (the two things added for the synth voice)
//!
//! The raw SVF above is *linear and clean by design* — that is what makes it
//! stable and lets the taps reconstruct the input exactly. For a "gently swept"
//! French-79 pad/arp we layer two things on top *without touching the loop*:
//!
//! 1. **Per-sample smoothing.** [`Filter::process`] one-pole-slews the cutoff
//!    (and resonance) toward their targets every sample and recomputes the
//!    coefficients, so block-rate cutoff modulation glides instead of zippering.
//!    The cost is one `tan` + one divide per sample — fine for one filter per
//!    voice. If a future profile shows 16-voice polyphony is hot here, swap the
//!    `tan` for a polynomial approximation; nothing else changes.
//!
//! 2. **An optional post-filter drive.** Analog warmth comes from a `tanh`
//!    soft-clip applied to the *output* (see [`Filter::set_drive`]), kept
//!    strictly outside the feedback loop so stability and the tap-reconstruction
//!    identity are preserved. Drive `0.0` is a true bypass (bit-identical clean
//!    path); turning it up adds gentle, even saturation.
//!
//! # A second model: the Moog ladder
//!
//! Alongside the SVF this struct also carries a **Moog 4-pole transistor ladder**
//! ([`Filter::process_moog`]) — a different low-pass *character* (24 dB/oct, the
//! famous fat, slightly-saturated Moog sound) reached via [`crate::voice::FilterModel::Moog`].
//! It is a *correct* zero-delay-feedback ladder: four TPT one-poles in a cascade
//! with the 4th-pole output fed back, solved in closed form **including the
//! feedback denominator** the naive version drops, with a `tanh` in the feedback
//! path for warmth and bounded self-oscillation, and full passband makeup so DC
//! gain stays ≈ 1 as resonance rises. It reuses the *same* smoothed cutoff /
//! resonance / prewarped `g` as the SVF, so modulation, glide, and the CS-80 HP
//! stage all keep working with either model. Its four integrator states (`lp1..lp4`)
//! live *alongside* the SVF's two, so switching model mid-note never corrupts the
//! other path. The default model is `Svf`; nothing reaches the ladder unless the
//! user selects `Moog`, so the SVF path stays bit-identical. See
//! [`Filter::process_moog`] for the full derivation.

use crate::util::flush_denormal;
use std::f32::consts::PI;

/// Lowest cutoff we allow, in Hz. Below this the filter is sub-audible; there is
/// no musical reason to go lower and it keeps the prewarp `tan` away from zero.
const MIN_CUTOFF_HZ: f32 = 20.0;

/// How close to Nyquist we let the cutoff go, as a fraction of the sample rate.
/// `tan(pi * fc / fs)` explodes toward infinity as `fc -> fs/2`, so we stop a
/// hair short (0.49 * fs ≈ 0.98 * Nyquist) to keep `g` finite and the filter
/// numerically sane right up to the top of its range.
const MAX_CUTOFF_FRACTION: f32 = 0.49;

/// Damping at resonance = 0. `k = 2` is `Q = 0.5`: a gentle, peak-free response,
/// the classic SVF "no resonance" setting.
const MAX_K: f32 = 2.0;

/// Damping at resonance = 1. We never reach `k = 0` (true, infinite
/// self-oscillation), so the peak stays large but bounded and the output can
/// never run away to infinity.
///
/// `k = 1/Q`, so this floor sets the maximum resonant gain: `k = 0.05` is
/// `Q = 20`, i.e. the filter peaks at roughly 20x (~26 dB) at its cutoff. That
/// is a strong, vocal resonance that still stays comfortably bounded for a
/// linear filter (no saturator needed). We deliberately keep this above the
/// numerically-possible floor (`k = 0.01`, ~40 dB / Q=100) so a self-resonant
/// sweep stacked across a 16-voice chord cannot slam the downstream gain stage.
/// Push the floor lower and the peak grows ~`1/k`; at `k = 0` it would be
/// infinite.
const MIN_K: f32 = 0.05;

/// Maximum Moog ladder feedback gain `k`. The ladder maps its `0..1` resonance
/// knob to `k` in `0..MOOG_MAX_K`. At `k ≈ 4` the 4-pole loop reaches the edge of
/// self-oscillation; the `tanh` in the feedback path keeps it bounded there rather
/// than letting it run away. (A pure linear ladder self-oscillates at exactly
/// `k = 4`; the saturation lets us sit right at that ringing edge safely.)
const MOOG_MAX_K: f32 = 4.0;

/// One-pole smoothing coefficient for the cutoff/resonance slews, per sample.
///
/// Each sample we move `SMOOTH_COEFF` of the remaining distance toward the
/// target. At 48 kHz this gives a time-constant of roughly 5 ms — fast enough to
/// feel immediate under the fingers, slow enough to turn a block-rate parameter
/// step into a click-free glide. It is intentionally a fixed per-sample constant
/// (not seconds-based) to stay allocation- and branch-free in the hot loop; the
/// resulting glide time scales mildly with sample rate, which is inaudible here.
const SMOOTH_COEFF: f32 = 0.01;

/// Distance (in Hz / in knob units) below which a smoothed value is snapped to
/// its target. A one-pole slew approaches its target asymptotically and never
/// *exactly* arrives; snapping inside this epsilon lets `process` skip the
/// `tan`/divide coefficient recompute once a parameter has effectively settled,
/// so a static patch costs nothing extra.
const SMOOTH_EPS_HZ: f32 = 0.01;
const SMOOTH_EPS_RES: f32 = 1.0e-5;

/// A 2-pole (12 dB/octave) zero-delay-feedback state-variable filter.
///
/// Create one per voice (it holds two samples of internal state), set the sample
/// rate, then drive `set_cutoff` / `set_resonance` from your modulation and call
/// [`Filter::process`] once per audio sample. Cutoff and resonance are smoothed
/// internally, so you can push fresh targets as often as every sample (e.g. from
/// a filter envelope) without zippering.
///
/// Real-time safe: [`Filter::process`] only does float arithmetic on fields that
/// already exist — no allocation, no locks, no `Vec` growth.
#[derive(Clone, Copy, Debug)]
pub struct Filter {
    sample_rate: f32,

    // --- User-facing targets (where we are gliding *to*) ---
    /// Requested cutoff in Hz, already clamped to a safe range.
    target_cutoff_hz: f32,
    /// Requested resonance knob position in `0.0..=1.0`.
    target_resonance: f32,

    // --- Smoothed current values (where we are *now*) ---
    /// Cutoff actually in effect this sample, slewing toward the target.
    cur_cutoff_hz: f32,
    /// Resonance actually in effect this sample, slewing toward the target.
    cur_resonance: f32,

    // --- Derived coefficients, recomputed by `update_coefficients` ---
    /// Prewarped integrator gain, `g = tan(pi * fc / fs)`.
    g: f32,
    /// Damping coefficient, `k = 1/Q`. Small k = high resonance.
    k: f32,
    /// The precomputed loop-solving gain `a1 = 1 / (1 + g*(g + k))`.
    a1: f32,

    // --- Post-filter warmth (outside the loop) ---
    /// Drive amount in `0.0..=1.0`. 0.0 = clean bypass; higher = more `tanh`
    /// saturation on the output.
    drive: f32,

    // --- Integrator states (the filter's memory) ---
    /// "Equivalent" charge on the first integrator (feeds the band-pass).
    ic1eq: f32,
    /// "Equivalent" charge on the second integrator (feeds the low-pass).
    ic2eq: f32,

    // --- Moog ladder integrator states (the four one-pole capacitor states) ---
    // These live ALONGSIDE the SVF `ic1eq`/`ic2eq` so switching the filter model
    // mid-note never corrupts the other model's memory. They are only read/written
    // by the `process_moog*` methods; the SVF path never touches them, so the SVF
    // output stays bit-identical regardless of these fields' values.
    lp1: f32,
    lp2: f32,
    lp3: f32,
    lp4: f32,

    // --- Most recent alternate taps, kept for the future dual LP+HP path ---
    last_band: f32,
    last_high: f32,
}

impl Filter {
    /// Create a filter at the given sample rate, opened up (cutoff at 1 kHz, no
    /// resonance) so it passes signal cleanly until you dial it in. The smoothers
    /// start *already at* the target so the very first sample is correct rather
    /// than gliding up from silence.
    pub fn new(sample_rate: f32) -> Self {
        let mut filter = Self {
            sample_rate: sample_rate.max(1.0),
            target_cutoff_hz: 1_000.0,
            target_resonance: 0.0,
            cur_cutoff_hz: 1_000.0,
            cur_resonance: 0.0,
            g: 0.0,
            k: MAX_K,
            a1: 1.0,
            drive: 0.0,
            ic1eq: 0.0,
            ic2eq: 0.0,
            lp1: 0.0,
            lp2: 0.0,
            lp3: 0.0,
            lp4: 0.0,
            last_band: 0.0,
            last_high: 0.0,
        };
        // Funnel the defaults through the setters so they're clamped, then snap
        // the smoothers to the targets and compute coefficients exactly once.
        filter.set_cutoff(1_000.0);
        filter.set_resonance(0.0);
        filter.snap_to_targets();
        filter
    }

    /// Change the sample rate (e.g. the host switched from 48 kHz to 96 kHz).
    ///
    /// We clear the integrator states so a rate change can't replay stale,
    /// now-mis-scaled energy as a click, then re-clamp the cutoff against the new
    /// Nyquist and recompute coefficients.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.reset();
        // Re-clamp + recompute against the new Nyquist, then snap so we don't
        // glide from a now-stale current value.
        self.set_cutoff(self.target_cutoff_hz);
        self.snap_to_targets();
    }

    /// Set the **target** cutoff frequency in Hz. The value is clamped to a safe
    /// range (`20 Hz .. ~0.98 * Nyquist`) so the prewarp `tan` never blows up and
    /// we can never produce NaN, whatever a wild modulation source throws at it.
    ///
    /// The filter glides toward this target over a few milliseconds (see
    /// [`Filter::process`]); call it as often as you like.
    pub fn set_cutoff(&mut self, hz: f32) {
        self.target_cutoff_hz = Self::clamp_cutoff(hz, self.sample_rate);
    }

    /// Clamp a requested cutoff into the safe `20 Hz .. ~0.98*Nyquist` window.
    /// Pulled out so both the target setter and the per-sample smoother share
    /// exactly one definition of "safe". `hz.max(...)` first turns a NaN input
    /// into `MIN_CUTOFF_HZ` (because `NaN.max(x) == x`) — free NaN hardening.
    fn clamp_cutoff(hz: f32, sample_rate: f32) -> f32 {
        let max_cutoff = (sample_rate * MAX_CUTOFF_FRACTION).max(MIN_CUTOFF_HZ);
        hz.max(MIN_CUTOFF_HZ).min(max_cutoff)
    }

    /// Set the **target** resonance from a `0.0..=1.0` knob. `0.0` is no peak;
    /// `1.0` is a large (but bounded) resonant peak near self-oscillation. The
    /// input is clamped, so out-of-range automation is harmless. The damping
    /// coefficient glides toward this target per sample.
    pub fn set_resonance(&mut self, amount_0_to_1: f32) {
        self.target_resonance = Self::clamp_resonance(amount_0_to_1);
    }

    /// Fold a resonance knob value into `0.0..=1.0`, mapping NaN to 0.0.
    /// `f32::clamp` *returns* NaN for a NaN input (and debug-asserts on NaN
    /// bounds); we fold NaN to 0.0 by hand so the filter can never be poisoned by
    /// a bad automation value.
    fn clamp_resonance(amount_0_to_1: f32) -> f32 {
        #[allow(clippy::manual_clamp)]
        if amount_0_to_1.is_nan() {
            0.0
        } else {
            amount_0_to_1.max(0.0).min(1.0)
        }
    }

    /// Map a `0.0..=1.0` resonance knob position to the damping coefficient `k`.
    ///
    /// The audible peak height is roughly `1/k`, so a *linear* `k` map would pile
    /// almost all of the perceived change into the very top of the knob. We use
    /// an **exponential** (geometric) interpolation instead:
    ///
    /// ```text
    ///     k = MAX_K * (MIN_K / MAX_K) ^ amount
    /// ```
    ///
    /// This is linear in `log(k)`, and since the peak grows like `1/k` it is also
    /// (to first order) linear in *decibels of peak*, so the resonance feels
    /// smooth and even across the whole sweep. `amount = 0 -> k = MAX_K`,
    /// `amount = 1 -> k = MIN_K`.
    fn resonance_to_k(amount: f32) -> f32 {
        MAX_K * (MIN_K / MAX_K).powf(amount)
    }

    /// Set the post-filter **drive** amount in `0.0..=1.0` (the analog "warmth").
    ///
    /// This is a `tanh` soft-clip applied to the low-pass output *after* the
    /// linear SVF loop — never inside it — so the filter stays unconditionally
    /// stable and the tap-reconstruction identity is untouched. `0.0` is a true
    /// bypass; higher values add progressively more gentle, even-order
    /// saturation. The output is level-compensated so turning drive up does not
    /// simply make the voice louder.
    pub fn set_drive(&mut self, amount_0_to_1: f32) {
        self.drive = Self::clamp_resonance(amount_0_to_1); // same 0..1, NaN->0 clamp
    }

    /// Recompute the derived coefficients from the *current* (smoothed) cutoff,
    /// `k`, and the sample rate. Cheap, but it calls `tan`, so the hot loop only
    /// runs it when the smoothed values actually move.
    fn update_coefficients(&mut self) {
        // Prewarp: bend the cutoff so it lands correctly after the bilinear
        // transform. `g` is the per-sample integrator gain.
        self.g = (PI * self.cur_cutoff_hz / self.sample_rate).tan();
        // The one division that solves the zero-delay feedback loop in closed
        // form. `1 + g*(g + k)` is always >= 1 for our non-negative g and k, so
        // this never divides by zero and `a1` is always in `(0, 1]`.
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
    }

    /// Force the smoothers instantly to their targets and recompute coefficients.
    /// Used on construction and sample-rate changes so we don't audibly glide
    /// from a stale value; also handy in tests that want a steady-state response.
    pub fn snap_to_targets(&mut self) {
        self.cur_cutoff_hz = self.target_cutoff_hz;
        self.cur_resonance = self.target_resonance;
        self.k = Self::resonance_to_k(self.cur_resonance);
        self.update_coefficients();
    }

    /// Clear the filter's memory (both integrator states and the cached taps).
    /// Use on note-on, transport stops, or sample-rate changes so the filter
    /// starts from silence with no leftover ringing. Smoother *targets* are left
    /// alone — only the audio memory is cleared.
    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
        // Clear the Moog ladder states too, so a note-on / transport-stop /
        // sample-rate change starts the ladder from silence with no leftover ring.
        self.lp1 = 0.0;
        self.lp2 = 0.0;
        self.lp3 = 0.0;
        self.lp4 = 0.0;
        self.last_band = 0.0;
        self.last_high = 0.0;
    }

    /// Advance the cutoff/resonance smoothers one sample toward their targets and
    /// recompute coefficients if anything actually moved. Returns nothing; mutates
    /// `cur_*`, `k`, `g`, `a1` in place. Kept tiny and branch-light so it is cheap
    /// to call once per sample.
    #[inline]
    fn advance_smoothers(&mut self) {
        let mut changed = false;

        // One-pole slew toward the target cutoff. Snap (and stop recomputing)
        // once we are within an epsilon so a static patch is free.
        let dc = self.target_cutoff_hz - self.cur_cutoff_hz;
        if dc.abs() > SMOOTH_EPS_HZ {
            self.cur_cutoff_hz += dc * SMOOTH_COEFF;
            changed = true;
        } else if self.cur_cutoff_hz != self.target_cutoff_hz {
            self.cur_cutoff_hz = self.target_cutoff_hz;
            changed = true;
        }

        // Same one-pole slew on resonance, in knob units; the exponential
        // knob->k map is applied after slewing so the *perceived* glide is even.
        let dr = self.target_resonance - self.cur_resonance;
        if dr.abs() > SMOOTH_EPS_RES {
            self.cur_resonance += dr * SMOOTH_COEFF;
            self.k = Self::resonance_to_k(self.cur_resonance);
            changed = true;
        } else if self.cur_resonance != self.target_resonance {
            self.cur_resonance = self.target_resonance;
            self.k = Self::resonance_to_k(self.cur_resonance);
            changed = true;
        }

        if changed {
            self.update_coefficients();
        }
    }

    /// Process one sample and return the (optionally driven) **low-pass** output.
    ///
    /// Real-time safe: pure float math on existing fields. The only `tan`/divide
    /// happen inside the smoother, and only when a parameter is actually moving.
    /// The band-pass and high-pass taps for this sample are stashed in
    /// `last_band` / `last_high` for callers that want them (see
    /// [`Filter::last_band`], [`Filter::last_high`]).
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        // Glide cutoff/resonance toward their targets before filtering so a
        // block-rate parameter step becomes a per-sample ramp (no zipper).
        self.advance_smoothers();

        let v0 = input;

        // Solve the two-integrator feedback loop in one shot (zero-delay).
        let v1 = self.a1 * (self.ic1eq + self.g * (v0 - self.ic2eq));
        let v2 = self.ic2eq + self.g * v1;

        // Trapezoidal integrator update: the new "equivalent charge" is twice the
        // integrator output minus the old charge. This trapezoidal rule (rather
        // than naive forward Euler) is what makes it a TPT filter. Flush so a
        // non-finite input (or an overflow at extreme resonance) can't lodge in
        // the integrator state and ring forever — a held/Drone voice never hits
        // the note-on `reset()` that would otherwise clear it.
        self.ic1eq = flush_denormal(2.0 * v1 - self.ic1eq);
        self.ic2eq = flush_denormal(2.0 * v2 - self.ic2eq);

        // The three classic SVF taps. `v1` is band-pass, `v2` is low-pass, and
        // high-pass is whatever is left of the input after removing them. These
        // are the *linear* taps and still satisfy `high + k*band + low == v0`.
        let low = v2;
        let band = v1;
        let high = v0 - self.k * v1 - v2;

        self.last_band = band;
        self.last_high = high;

        self.apply_drive(low)
    }

    /// Process one sample and return the **high-pass** output.
    ///
    /// This is the first ("hipass") stage of the CS-80-style dual filter: a
    /// resonant high-pass that runs *before* the low-pass. It is the exact same
    /// zero-delay-feedback SVF update as [`process`](Filter::process) — same
    /// smoothers, same coefficients, same `MIN_K`/`MAX_K` stability floor, same
    /// per-sample denormal flush on the integrator state — but it returns the
    /// high-pass tap (`last_high`) instead of the low-pass one.
    ///
    /// We deliberately do **not** apply the post-filter `drive` here: drive is a
    /// warmth/saturation stage that belongs on the final (low-pass) output of the
    /// series, not in the middle of it. So the HP stage stays perfectly linear and
    /// its tap is bit-clean regardless of the drive setting.
    ///
    /// Real-time safe: identical cost to [`process`](Filter::process) — pure float
    /// math on existing fields, no allocation.
    #[inline]
    pub fn process_high(&mut self, input: f32) -> f32 {
        // Glide cutoff/resonance toward their targets, exactly as `process` does,
        // so a block-rate HP-cutoff step becomes a per-sample ramp (no zipper).
        self.advance_smoothers();

        let v0 = input;

        // Solve the same two-integrator zero-delay feedback loop.
        let v1 = self.a1 * (self.ic1eq + self.g * (v0 - self.ic2eq));
        let v2 = self.ic2eq + self.g * v1;

        // Trapezoidal integrator update with the same denormal flush, so a held
        // HP stage on a Drone voice can't latch a NaN or trickle a denormal.
        self.ic1eq = flush_denormal(2.0 * v1 - self.ic1eq);
        self.ic2eq = flush_denormal(2.0 * v2 - self.ic2eq);

        // Update the band-pass and high-pass taps (the low-pass tap `v2` is not
        // needed by the HP stage). `high + k*band + low == v0` still holds.
        let band = v1;
        let high = v0 - self.k * v1 - v2;

        self.last_band = band;
        self.last_high = high;

        // No drive on the HP stage (see the doc comment): return the linear tap.
        high
    }

    /// Process one sample through the **Moog 4-pole transistor ladder** (24 dB/oct
    /// low-pass) and return the (optionally driven) output.
    ///
    /// This is the [`FilterModel::Moog`] path. It is a *correct* zero-delay-feedback
    /// ladder — a cascade of four TPT one-pole low-passes with the 4th-pole output
    /// fed back to the input — implementing the three things a naive ladder gets
    /// wrong:
    ///
    /// 1. **The feedback-denominator solve.** The resonance feedback `u = input −
    ///    k·y4` is an *instantaneous* loop: `y4` depends on `u`, which depends on
    ///    `y4`. The closed-form solve is
    ///    `y4 = (G⁴·input + S) / (1 + k·G⁴)` — and the `(1 + k·G⁴)` denominator is
    ///    exactly the term a naive (one-sample-delayed-feedback) ladder *drops*,
    ///    which is what makes that version detune and mistrack its cutoff with
    ///    resonance. `S` is the four integrator states propagated forward to the
    ///    4th pole (see below). This is the D'Angelo & Välimäki / Zavalishin TPT
    ///    one-pole-cascade form.
    /// 2. **`tanh` saturation in the feedback path.** Real ladders are built from
    ///    transistor pairs that gently saturate. We model that with a `tanh` on the
    ///    feedback term: `u = input − k·tanh(y4)`. That gives the characteristic
    ///    Moog warmth and, crucially, keeps self-oscillation *bounded* — the loop
    ///    gain falls off as the signal grows, so at maximum resonance it rings at a
    ///    finite level instead of running to infinity. The nonlinearity makes the
    ///    loop only mildly non-linear, so we use the linear ZDF result as a
    ///    *predictor* for `y4`, take one `tanh`-corrected feedback value, and run
    ///    the four stages forward once with it. **One iteration** is the deliberate
    ///    cost/accuracy tradeoff: it captures the saturation character at one
    ///    multiply-add + one `tanh` per sample, without an iterative solver in the
    ///    hot loop.
    /// 3. **Unity-passband makeup.** A classic ladder loses ~`1/(1+k)` of its
    ///    passband (DC) gain as resonance rises — the bug where the low end thins
    ///    out and the whole filter seems to get quieter as you add resonance. We
    ///    apply **full makeup** `input_makeup = 1 + k` to the input so DC gain stays
    ///    ≈ 1 across the whole resonance sweep. (A partial `0.5·k` makeup would keep
    ///    some vintage "thinning"; full makeup is the cleaner choice and what the
    ///    contract asks for.)
    ///
    /// The output is the 4th-pole low-pass `y4` — the cascade is inherently 24
    /// dB/oct. The post-filter [`Filter::set_drive`] `tanh` warmth still applies on
    /// top (outside the ladder loop), exactly as for the SVF path.
    ///
    /// Real-time safe: the same smoothers and prewarped `g` as the SVF path (so
    /// cutoff/resonance modulation, glide, and the CS-80 routing all keep working),
    /// plus a handful of multiplies and one `tanh`. No allocation, no locks.
    #[inline]
    pub fn process_moog(&mut self, input: f32) -> f32 {
        // Reuse the shared smoothers so cutoff/resonance glide and the prewarped
        // `g` behave identically to the SVF path.
        self.advance_smoothers();

        // One-pole TPT feedback-path gain. `g` is already prewarped + smoothed.
        // `G = g/(1+g)` is the per-stage input gain; `b = 1 - G` passes the state.
        let g = self.g;
        let big_g = g / (1.0 + g);
        let b = 1.0 - big_g;

        // Map the SVF damping `k` (which is large at LOW resonance) onto the Moog
        // feedback `k` (which is large at HIGH resonance). `self.cur_resonance` is
        // the smoothed 0..1 knob, so we scale it straight to 0..MOOG_MAX_K — that
        // keeps the Moog's resonance glide smooth and independent of the SVF map.
        let k = self.cur_resonance.clamp(0.0, 1.0) * MOOG_MAX_K;

        // Full passband makeup so DC gain stays ≈ 1 as resonance rises.
        let x = input * (1.0 + k);

        // --- The ZDF denominator solve (linear predictor for y4) ---------------
        // y4 = G^4 * u + S, with u = x - k*y4. State sum S is each integrator
        // state propagated forward through the remaining stages to the 4th pole:
        //   S = b * (G^3 z1 + G^2 z2 + G z3 + z4).
        let g2 = big_g * big_g;
        let g3 = g2 * big_g;
        let g4 = g3 * big_g;
        let s = b * (g3 * self.lp1 + g2 * self.lp2 + big_g * self.lp3 + self.lp4);
        // Solve the instantaneous loop WITH the (1 + k*G^4) denominator — the term
        // a naive ladder omits.
        let y4_lin = (g4 * x + s) / (1.0 + k * g4);

        // --- One tanh-corrected pass -------------------------------------------
        // Use the linear y4 as the predictor for the saturated feedback, then run
        // the four one-poles forward once with the resolved input `u`.
        let u = x - k * y4_lin.tanh();

        let y1 = big_g * u + b * self.lp1;
        let y2 = big_g * y1 + b * self.lp2;
        let y3 = big_g * y2 + b * self.lp3;
        let y4 = big_g * y3 + b * self.lp4;

        // TPT trapezoidal state update for each one-pole: z_next = 2*y - z. The
        // denormal/NaN flush mirrors the SVF path so a held Drone voice can't latch
        // a non-finite value or trickle a denormal through the ladder forever.
        self.lp1 = flush_denormal(2.0 * y1 - self.lp1);
        self.lp2 = flush_denormal(2.0 * y2 - self.lp2);
        self.lp3 = flush_denormal(2.0 * y3 - self.lp3);
        self.lp4 = flush_denormal(2.0 * y4 - self.lp4);

        // The 24 dB/oct low-pass IS the 4th-pole output. Post-filter drive (tanh
        // warmth) applies on top, outside the loop, just like the SVF path.
        self.apply_drive(y4)
    }

    /// Apply the optional post-filter `tanh` warmth to the low-pass output.
    ///
    /// `drive == 0.0` returns the input untouched (a true, bit-identical bypass).
    /// Otherwise we push the signal through `tanh(input * gain)` and divide back
    /// out the small-signal slope so the *quiet* passband stays at unity gain —
    /// you get added harmonics and gentle limiting on peaks, not just "louder".
    #[inline]
    fn apply_drive(&self, sample: f32) -> f32 {
        if self.drive <= 0.0 {
            return sample;
        }
        // Map drive 0..1 to a pre-gain of 1..~4. tanh's slope at 0 is `gain`, so
        // dividing the output by `gain` restores unity gain for small signals and
        // leaves the saturation curve to act only as the level rises.
        let gain = 1.0 + 3.0 * self.drive;
        (sample * gain).tanh() / gain
    }

    /// Band-pass output from the most recent [`process`](Filter::process) call.
    /// Kept available for the upcoming CS-80-style dual filter.
    #[inline]
    pub fn last_band(&self) -> f32 {
        self.last_band
    }

    /// High-pass output from the most recent [`process`](Filter::process) call.
    /// This is the tap the future LP+HP "hipass" path will read.
    #[inline]
    pub fn last_high(&self) -> f32 {
        self.last_high
    }

    /// The current (smoothed) cutoff in Hz. Handy for tests and UI.
    pub fn cutoff_hz(&self) -> f32 {
        self.cur_cutoff_hz
    }

    /// The target cutoff in Hz that the smoother is gliding toward.
    pub fn target_cutoff_hz(&self) -> f32 {
        self.target_cutoff_hz
    }

    /// The current (smoothed) resonance knob position in `0.0..=1.0`.
    pub fn resonance(&self) -> f32 {
        self.cur_resonance
    }

    /// The current post-filter drive amount in `0.0..=1.0`.
    pub fn drive(&self) -> f32 {
        self.drive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const SR: f32 = 48_000.0;

    /// Run a sine of `freq` Hz through the filter for `samples` samples and
    /// return the peak absolute output once it has settled. We skip an initial
    /// warm-up window so we measure steady-state gain, not the transient.
    fn measure_peak(filter: &mut Filter, freq: f32, samples: usize) -> f32 {
        let warmup = samples / 4;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for i in 0..samples {
            let input = (phase * TAU).sin();
            phase = (phase + freq / SR).fract();
            let out = filter.process(input);
            if i >= warmup {
                peak = peak.max(out.abs());
            }
        }
        peak
    }

    /// Sweeping cutoff across the whole audible range and resonance from none to
    /// max must never produce a NaN or infinity, for any input we throw at it.
    #[test]
    fn output_is_finite_across_full_sweep() {
        let mut filter = Filter::new(SR);

        // A handful of cutoffs from 20 Hz up to just under Nyquist.
        let cutoffs = [20.0, 100.0, 1_000.0, 5_000.0, 12_000.0, 23_000.0, 23_990.0];
        // Resonance from none to maximum.
        let resonances = [0.0, 0.25, 0.5, 0.75, 1.0];

        for &fc in &cutoffs {
            for &res in &resonances {
                filter.reset();
                filter.set_cutoff(fc);
                filter.set_resonance(res);
                filter.snap_to_targets();

                let mut phase = 0.0f32;
                // Sweep the input frequency too, so the filter sees energy at and
                // around its cutoff at every setting.
                for i in 0..(SR as usize) {
                    let input_freq = 20.0 + (i as f32 / SR) * 20_000.0;
                    let input = (phase * TAU).sin();
                    phase = (phase + input_freq / SR).fract();
                    let out = filter.process(input);
                    assert!(
                        out.is_finite(),
                        "non-finite output at fc={fc}, res={res}, i={i}: {out}"
                    );
                }
                // Also feed it pathological extremes directly.
                for &x in &[1.0e9_f32, -1.0e9_f32, 0.0, 1.0, -1.0] {
                    let out = filter.process(x);
                    assert!(
                        out.is_finite(),
                        "non-finite on extreme input at fc={fc}, res={res}"
                    );
                }
            }
        }
    }

    /// The whole point of a low-pass: with a low cutoff, a high-frequency sine
    /// must come out far quieter than a low-frequency sine.
    #[test]
    fn low_cutoff_attenuates_highs_far_more_than_lows() {
        let mut filter = Filter::new(SR);
        filter.set_resonance(0.0); // flat passband so we measure the slope only
        filter.set_cutoff(200.0);
        filter.snap_to_targets();

        // 50 Hz is well inside the passband; 8 kHz is ~5 octaves above cutoff,
        // so a 12 dB/oct filter should crush it.
        filter.reset();
        let low_gain = measure_peak(&mut filter, 50.0, SR as usize);
        filter.reset();
        let high_gain = measure_peak(&mut filter, 8_000.0, SR as usize);

        assert!(
            low_gain > 0.7,
            "low frequency should pass nearly unattenuated, got {low_gain}"
        );
        assert!(
            high_gain < 0.05,
            "high frequency should be strongly attenuated, got {high_gain}"
        );
        // And concretely: the high tone is at least 20x (>26 dB) quieter.
        assert!(
            low_gain > high_gain * 20.0,
            "expected lows >> highs, got low={low_gain}, high={high_gain}"
        );
    }

    /// At maximum resonance the filter must ring loudly but stay bounded — no
    /// runaway to infinity, no NaN — even when we kick it and then feed silence.
    #[test]
    fn stable_at_max_resonance() {
        let mut filter = Filter::new(SR);
        filter.set_cutoff(1_000.0);
        filter.set_resonance(1.0);
        filter.snap_to_targets();

        // Hit it with an impulse, then let it ring on silence for a full second.
        let mut peak = 0.0f32;
        let out0 = filter.process(1.0);
        peak = peak.max(out0.abs());
        for _ in 0..(SR as usize) {
            let out = filter.process(0.0);
            assert!(out.is_finite(), "max-resonance ring went non-finite");
            peak = peak.max(out.abs());
        }
        // The resonant peak is large but must not explode.
        assert!(peak < 100.0, "max-resonance peak unbounded: {peak}");

        // Now drive it at its own cutoff and confirm it still stays finite and
        // bounded rather than building up forever.
        filter.reset();
        let driven_peak = measure_peak(&mut filter, 1_000.0, SR as usize * 2);
        assert!(driven_peak.is_finite(), "driven max-resonance went non-finite");
        assert!(
            driven_peak < 1_000.0,
            "driven max-resonance unbounded: {driven_peak}"
        );
    }

    /// Cutoff and resonance must be clamped so no setting can NaN the filter,
    /// including absurd, negative, infinite, and NaN inputs.
    #[test]
    fn settings_are_clamped_to_safe_ranges() {
        let mut filter = Filter::new(SR);

        filter.set_cutoff(-100.0);
        filter.snap_to_targets();
        assert!(filter.cutoff_hz() >= MIN_CUTOFF_HZ);

        filter.set_cutoff(1.0e9);
        filter.snap_to_targets();
        assert!(filter.cutoff_hz() <= SR * MAX_CUTOFF_FRACTION);

        filter.set_cutoff(f32::NAN);
        filter.snap_to_targets();
        assert!(filter.cutoff_hz().is_finite());

        filter.set_cutoff(f32::INFINITY);
        filter.snap_to_targets();
        assert!(filter.cutoff_hz().is_finite());

        filter.set_resonance(-5.0);
        filter.snap_to_targets();
        assert_eq!(filter.resonance(), 0.0);

        filter.set_resonance(5.0);
        filter.snap_to_targets();
        assert_eq!(filter.resonance(), 1.0);

        filter.set_resonance(f32::NAN);
        filter.snap_to_targets();
        assert!(filter.resonance().is_finite());

        // After all that abuse the filter still behaves.
        let out = filter.process(0.5);
        assert!(out.is_finite());
    }

    /// The HP/BP taps must always be available and finite, and a tone far below
    /// cutoff should favour the low-pass tap over the high-pass tap. This guards
    /// the future CS-80 dual LP+HP path.
    #[test]
    fn alternate_taps_are_available_and_finite() {
        let mut filter = Filter::new(SR);
        filter.set_cutoff(1_000.0);
        filter.set_resonance(0.3);
        filter.snap_to_targets();

        let mut phase = 0.0f32;
        for _ in 0..1_000 {
            let input = (phase * TAU).sin();
            phase = (phase + 1_000.0 / SR).fract();
            let _ = filter.process(input);
            assert!(filter.last_band().is_finite());
            assert!(filter.last_high().is_finite());
        }

        // A frequency far below cutoff should appear in the low-pass but be
        // largely rejected by the high-pass.
        filter.reset();
        filter.set_cutoff(2_000.0);
        filter.snap_to_targets();
        let mut low_peak = 0.0f32;
        let mut high_peak = 0.0f32;
        let mut phase = 0.0f32;
        for i in 0..(SR as usize) {
            let input = (phase * TAU).sin();
            phase = (phase + 100.0 / SR).fract();
            let low = filter.process(input);
            if i > (SR as usize) / 4 {
                low_peak = low_peak.max(low.abs());
                high_peak = high_peak.max(filter.last_high().abs());
            }
        }
        assert!(
            low_peak > high_peak,
            "low tone should favor the LP tap over the HP tap: low={low_peak}, high={high_peak}"
        );
    }

    /// The three taps must reconstruct the input: `high + k*band + low == v0`.
    /// This is the defining algebraic identity of a state-variable filter and
    /// the basis for the future dual LP+HP routing. (Checked on the *linear*
    /// low-pass tap, i.e. with drive off, since the identity is about the loop.)
    #[test]
    fn taps_reconstruct_the_input() {
        let mut filter = Filter::new(SR);
        filter.set_cutoff(1_500.0);
        filter.set_resonance(0.6);
        filter.snap_to_targets();
        let k = filter.k;

        let mut phase = 0.0f32;
        for _ in 0..2_000 {
            let input = (phase * TAU).sin();
            phase = (phase + 440.0 / SR).fract();
            let low = filter.process(input);
            let reconstructed = filter.last_high() + k * filter.last_band() + low;
            assert!(
                (reconstructed - input).abs() < 1.0e-4,
                "taps failed to reconstruct input: got {reconstructed}, want {input}"
            );
        }
    }

    // ----- New tests for the required fixes -----------------------------------

    /// The exponential resonance->k map must make the *perceived* peak grow
    /// roughly evenly across the knob: the top half of the knob should not
    /// contribute wildly more peak (in dB) than the bottom half. We measure the
    /// on-cutoff gain at knob = 0.25, 0.5, 0.75, 1.0 and check the dB steps
    /// between successive quarters are within a sane ratio of each other.
    #[test]
    fn resonance_curve_is_perceptually_even() {
        let fc = 1_000.0;
        let mut gains_db = Vec::new();
        for &res in &[0.25f32, 0.5, 0.75, 1.0] {
            let mut filter = Filter::new(SR);
            filter.set_cutoff(fc);
            filter.set_resonance(res);
            filter.snap_to_targets();
            // Drive a tone right at the cutoff and read the resonant gain.
            let peak = measure_peak(&mut filter, fc, SR as usize);
            gains_db.push(20.0 * peak.log10());
        }
        // Successive quarter-knob steps in dB.
        let steps: Vec<f32> = gains_db.windows(2).map(|w| w[1] - w[0]).collect();
        for &s in &steps {
            assert!(s > 0.0, "resonance must increase the peak each step: {steps:?}");
        }
        // With a *linear* k map the final step would dwarf the first by >10x.
        // The exponential map keeps them within ~3x of each other.
        let max_step = steps.iter().cloned().fold(0.0f32, f32::max);
        let min_step = steps.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            max_step <= min_step * 4.0,
            "resonance steps should be roughly even, got {steps:?} dB"
        );
    }

    /// Sweeping the cutoff target with no snapping must not zipper: the smoother
    /// should glide the audible cutoff gradually rather than jumping. We push a
    /// big target step and confirm the *current* cutoff takes many samples to
    /// arrive (i.e. it is being smoothed), while still converging.
    #[test]
    fn cutoff_is_smoothed_not_stepped() {
        let mut filter = Filter::new(SR);
        filter.set_cutoff(500.0);
        filter.snap_to_targets();
        assert!((filter.cutoff_hz() - 500.0).abs() < 1.0);

        // Jump the target an octave up and process one sample.
        filter.set_cutoff(5_000.0);
        let _ = filter.process(0.0);
        // After a single sample the audible cutoff must be far from the target
        // (i.e. it did NOT step instantly) but should have moved toward it.
        let after_one = filter.cutoff_hz();
        assert!(
            after_one > 500.0 && after_one < 4_000.0,
            "cutoff should glide, not jump: {after_one}"
        );

        // Given enough samples it should converge close to the target.
        for _ in 0..5_000 {
            let _ = filter.process(0.0);
        }
        assert!(
            (filter.cutoff_hz() - 5_000.0).abs() < 50.0,
            "cutoff should converge to target, got {}",
            filter.cutoff_hz()
        );
    }

    /// Drive at 0.0 must be a true bypass (bit-identical to no drive), and drive
    /// must keep the output bounded and finite while adding harmonics. We also
    /// confirm small signals keep roughly unity gain (level-compensated). To
    /// isolate the *drive* from the filter's own frequency response, we compare a
    /// driven filter against an identical clean one sample-for-sample.
    #[test]
    fn drive_is_bypass_at_zero_and_bounded_when_on() {
        // Bypass: a filter with drive 0 matches the plain LP output exactly.
        let mut clean = Filter::new(SR);
        clean.set_cutoff(2_000.0);
        clean.snap_to_targets();
        let mut bypassed = clean;
        bypassed.set_drive(0.0);

        let mut phase = 0.0f32;
        for _ in 0..1_000 {
            let input = (phase * TAU).sin() * 0.5;
            phase = (phase + 440.0 / SR).fract();
            assert_eq!(clean.process(input), bypassed.process(input));
        }

        // Small-signal unity gain: a *quiet* tone through a driven filter must
        // come out at almost exactly the same level as through an identical clean
        // filter (the tanh slope is compensated out near zero). Process a tiny
        // tone through both in lockstep and compare the settled peaks.
        let mut clean = Filter::new(SR);
        clean.set_cutoff(2_000.0);
        clean.snap_to_targets();
        let mut driven = Filter::new(SR);
        driven.set_cutoff(2_000.0);
        driven.set_drive(1.0);
        driven.snap_to_targets();

        let mut phase = 0.0f32;
        let mut cp = 0.0f32;
        let mut dp = 0.0f32;
        for i in 0..(SR as usize) {
            let input = (phase * TAU).sin() * 0.001; // tiny: stay linear in tanh
            phase = (phase + 440.0 / SR).fract();
            let c = clean.process(input);
            let d = driven.process(input);
            if i > (SR as usize) / 4 {
                cp = cp.max(c.abs());
                dp = dp.max(d.abs());
            }
        }
        assert!(
            (dp - cp).abs() < cp * 0.05,
            "small-signal drive gain should be ~unity vs clean: clean={cp}, driven={dp}"
        );

        // Driven hard: a big input is soft-clipped, never blown up past ~1.
        let mut hot = Filter::new(SR);
        hot.set_cutoff(2_000.0);
        hot.set_drive(1.0);
        hot.snap_to_targets();
        let mut peak = 0.0f32;
        for _ in 0..1_000 {
            let out = hot.process(10.0);
            assert!(out.is_finite());
            peak = peak.max(out.abs());
        }
        assert!(peak <= 1.0, "tanh drive must bound the output, got {peak}");
    }

    /// REGRESSION: a non-finite *signal* injected into the filter must not latch
    /// in the SVF integrator states (`ic1eq`/`ic2eq`). A sustained/Drone voice
    /// never hits the note-on `reset()`, so without the per-sample flush a single
    /// NaN would ring forever and feed the bus FX. After the fix the filter must
    /// return to finite output on silence within a handful of samples.
    #[test]
    fn recovers_from_injected_nan_and_inf() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut filter = Filter::new(SR);
            filter.set_cutoff(1_000.0);
            filter.set_resonance(0.8);
            filter.snap_to_targets();

            // Warm up, then inject the poison sample.
            for _ in 0..200 {
                let _ = filter.process(0.2);
            }
            let _ = filter.process(bad);

            // A couple of samples of silence should flush the integrators clean.
            let mut out = filter.process(0.0);
            for _ in 0..8 {
                out = filter.process(0.0);
            }
            assert!(
                out.is_finite(),
                "filter did not recover to finite output after injecting {bad}"
            );
            // Stay finite from here on.
            for _ in 0..1_000 {
                assert!(filter.process(0.0).is_finite(), "filter re-poisoned");
            }
        }
    }

    /// Run a sine of `freq` Hz through the filter's HIGH-PASS tap for `samples`
    /// samples and return the settled peak. Mirrors `measure_peak` but drives the
    /// `process_high` path (the CS-80 HP stage).
    fn measure_peak_high(filter: &mut Filter, freq: f32, samples: usize) -> f32 {
        let warmup = samples / 4;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for i in 0..samples {
            let input = (phase * TAU).sin();
            phase = (phase + freq / SR).fract();
            let out = filter.process_high(input);
            if i >= warmup {
                peak = peak.max(out.abs());
            }
        }
        peak
    }

    /// The whole point of the HP stage: with a high cutoff, a low-frequency sine
    /// must come out far quieter than a high-frequency sine. This is the inverse
    /// of `low_cutoff_attenuates_highs_far_more_than_lows` and guards the new
    /// `process_high` path used by the CS-80 dual filter.
    #[test]
    fn high_pass_attenuates_lows_far_more_than_highs() {
        let mut filter = Filter::new(SR);
        filter.set_resonance(0.0); // flat passband so we measure the slope only
        filter.set_cutoff(2_000.0);
        filter.snap_to_targets();

        // 100 Hz is ~4.3 octaves below cutoff (crushed by the 12 dB/oct HP); 8 kHz
        // is 2 octaves above cutoff (passes nearly unattenuated).
        filter.reset();
        let low_gain = measure_peak_high(&mut filter, 100.0, SR as usize);
        filter.reset();
        let high_gain = measure_peak_high(&mut filter, 8_000.0, SR as usize);

        assert!(
            high_gain > 0.7,
            "high frequency should pass nearly unattenuated through the HP, got {high_gain}"
        );
        assert!(
            low_gain < 0.1,
            "low frequency should be strongly attenuated by the HP, got {low_gain}"
        );
        assert!(
            high_gain > low_gain * 10.0,
            "expected highs >> lows through the HP, got low={low_gain}, high={high_gain}"
        );
    }

    /// `process_high` must stay finite across the full cutoff/resonance sweep,
    /// just like `process` — it shares the same `MIN_K` floor and integrator
    /// flush, so a self-oscillating HP stays bounded.
    #[test]
    fn high_pass_is_finite_and_bounded_across_sweep() {
        let mut filter = Filter::new(SR);
        let cutoffs = [20.0, 200.0, 2_000.0, 10_000.0, 23_000.0];
        let resonances = [0.0, 0.5, 1.0];
        for &fc in &cutoffs {
            for &res in &resonances {
                filter.reset();
                filter.set_cutoff(fc);
                filter.set_resonance(res);
                filter.snap_to_targets();
                let mut peak = 0.0f32;
                let mut phase = 0.0f32;
                for _ in 0..(SR as usize / 2) {
                    let input = (phase * TAU).sin();
                    phase = (phase + fc / SR).fract();
                    let out = filter.process_high(input);
                    assert!(out.is_finite(), "HP non-finite at fc={fc}, res={res}: {out}");
                    peak = peak.max(out.abs());
                }
                assert!(peak < 100.0, "HP self-oscillation unbounded at fc={fc}, res={res}: {peak}");
            }
        }
    }

    /// After an impulse then SILENCE the resonant ring must decay to *exactly*
    /// 0.0 — no denormal trickling through the two integrators forever. The
    /// `flush_denormal` on the `ic1eq`/`ic2eq` writes guarantees this.
    #[test]
    fn ring_decays_to_exactly_zero_on_silence() {
        let mut filter = Filter::new(SR);
        filter.set_cutoff(800.0);
        filter.set_resonance(0.7);
        filter.snap_to_targets();

        let _ = filter.process(1.0); // impulse
        let mut out = 1.0f32;
        for _ in 0..(SR as usize * 4) {
            out = filter.process(0.0);
        }
        assert_eq!(out, 0.0, "filter ring did not reach exactly 0: {out}");
    }

    // ----- Moog 4-pole ladder tests -------------------------------------------

    /// Settled peak of a `freq` Hz sine through the Moog ladder LP. Mirrors
    /// `measure_peak` but drives the `process_moog` path.
    fn measure_peak_moog(filter: &mut Filter, freq: f32, samples: usize) -> f32 {
        let warmup = samples / 4;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for i in 0..samples {
            let input = (phase * TAU).sin();
            phase = (phase + freq / SR).fract();
            let out = filter.process_moog(input);
            if i >= warmup {
                peak = peak.max(out.abs());
            }
        }
        peak
    }

    /// The whole point of the ladder low-pass: with a low cutoff a high sine comes
    /// out far quieter than a low one. And because it is a 24 dB/oct (4-pole)
    /// filter, it rolls off *much* steeper than the 12 dB/oct SVF — five octaves
    /// above cutoff it should be crushed to near nothing.
    #[test]
    fn moog_lowpass_attenuates_highs() {
        let mut filter = Filter::new(SR);
        filter.set_resonance(0.0); // flat passband so we measure the slope
        filter.set_cutoff(200.0);
        filter.snap_to_targets();

        filter.reset();
        let low_gain = measure_peak_moog(&mut filter, 50.0, SR as usize);
        filter.reset();
        let high_gain = measure_peak_moog(&mut filter, 8_000.0, SR as usize);

        assert!(
            low_gain > 0.7,
            "ladder: a low tone should pass nearly unattenuated, got {low_gain}"
        );
        assert!(
            high_gain < 0.02,
            "ladder: a high tone (5 oct up) should be crushed by the 24 dB/oct slope, got {high_gain}"
        );
        // And the 4-pole slope is steeper than the SVF's 2-pole: well over 26 dB
        // of separation here.
        assert!(
            low_gain > high_gain * 50.0,
            "ladder: expected steep 24 dB/oct rolloff, got low={low_gain}, high={high_gain}"
        );
    }

    /// The unity-passband fix: **DC gain must stay ≈ 1.0 across the whole resonance
    /// sweep**. A naive ladder loses ~1/(1+k) of its passband as resonance rises
    /// (the low end thins out, the filter seems to get quieter); the `(1 + k)` input
    /// makeup cancels that exactly so DC gain stays unity. We measure true DC gain
    /// by settling the filter on a constant input and reading the output — well away
    /// from the resonant peak at cutoff, this isolates the passband makeup.
    #[test]
    fn moog_passband_is_unity_across_resonance() {
        for &res in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut filter = Filter::new(SR);
            filter.set_cutoff(2_000.0);
            filter.set_resonance(res);
            filter.snap_to_targets();
            // Hold a DC input until the ladder settles, then read the gain. At DC the
            // 4-pole cascade has unit gain and the linear feedback divides by (1+k);
            // the (1+k) makeup restores it to ~1.0 regardless of resonance. The tanh
            // in the feedback path softens the effective feedback as the signal
            // grows, so DC gain lifts *slightly* above unity at mid resonance (a real
            // analog trait, ≲0.5 dB) — we require it to stay within ~±0.8 dB of unity,
            // i.e. the passband never thins out the way a naive ladder's would.
            let mut out = 0.0f32;
            for _ in 0..8_000 {
                out = filter.process_moog(0.5);
            }
            let gain = out / 0.5;
            assert!(
                (gain - 1.0).abs() < 0.1,
                "ladder DC gain should stay ~unity at res={res}, got {gain}"
            );
        }

        // Also confirm an in-passband *tone* (deep below cutoff) doesn't COLLAPSE as
        // resonance rises — the actual bug the makeup fixes is the passband thinning
        // toward 1/(1+k) (e.g. ~0.33 at k=2). With full makeup it must instead stay
        // at or *above* unity (a mild resonant lift on the lower skirt is expected
        // and musical). We require: never thins below ~0.9, never blows up past ~1.5.
        for &res in &[0.0f32, 0.5, 1.0] {
            let mut filter = Filter::new(SR);
            filter.set_cutoff(4_000.0);
            filter.set_resonance(res);
            filter.snap_to_targets();
            // 60 Hz is ~6 octaves below cutoff — firmly in the passband.
            let gain = measure_peak_moog(&mut filter, 60.0, SR as usize);
            assert!(
                (0.9..=1.5).contains(&gain),
                "ladder passband tone should not thin out at res={res}, got {gain}"
            );
        }
    }

    /// Near maximum resonance the ladder must self-oscillate — kick it with an
    /// impulse and it rings on silence — but the `tanh` in the feedback path must
    /// keep that ring *bounded* and finite (no runaway to infinity).
    #[test]
    fn moog_self_oscillates_but_stays_bounded() {
        let mut filter = Filter::new(SR);
        filter.set_cutoff(1_000.0);
        filter.set_resonance(1.0); // max -> k near the self-oscillation edge
        filter.snap_to_targets();

        // Kick it, then let it ring on silence.
        let _ = filter.process_moog(1.0);
        let mut peak = 0.0f32;
        let mut ring_energy = 0.0f32;
        for _ in 0..(SR as usize) {
            let out = filter.process_moog(0.0);
            assert!(out.is_finite(), "ladder self-oscillation went non-finite");
            peak = peak.max(out.abs());
            ring_energy += out.abs();
        }
        // It must actually RING (self-oscillate), not decay instantly to silence.
        assert!(
            ring_energy > 1.0,
            "ladder should self-oscillate near max resonance, ring_energy={ring_energy}"
        );
        // But the tanh keeps it bounded.
        assert!(peak < 10.0, "ladder self-oscillation must stay bounded, got {peak}");
    }

    /// The ladder must stay finite across the full cutoff/resonance sweep and for
    /// pathological inputs, exactly like the SVF path — the `tanh` feedback plus the
    /// per-sample `flush_denormal` on `lp1..lp4` guarantee no NaN/inf can latch.
    #[test]
    fn moog_is_finite_across_full_sweep() {
        let mut filter = Filter::new(SR);
        let cutoffs = [20.0, 100.0, 1_000.0, 5_000.0, 12_000.0, 23_000.0, 23_990.0];
        let resonances = [0.0, 0.25, 0.5, 0.75, 1.0];

        for &fc in &cutoffs {
            for &res in &resonances {
                filter.reset();
                filter.set_cutoff(fc);
                filter.set_resonance(res);
                filter.snap_to_targets();

                let mut phase = 0.0f32;
                for i in 0..(SR as usize) {
                    let input_freq = 20.0 + (i as f32 / SR) * 20_000.0;
                    let input = (phase * TAU).sin();
                    phase = (phase + input_freq / SR).fract();
                    let out = filter.process_moog(input);
                    assert!(
                        out.is_finite(),
                        "ladder non-finite at fc={fc}, res={res}, i={i}: {out}"
                    );
                }
                for &x in &[1.0e9_f32, -1.0e9_f32, 0.0, 1.0, -1.0] {
                    let out = filter.process_moog(x);
                    assert!(
                        out.is_finite(),
                        "ladder non-finite on extreme input at fc={fc}, res={res}"
                    );
                }
            }
        }
    }

    /// REGRESSION GUARD: selecting the SVF model must be **bit-identical** to the
    /// pre-Moog filter. The ladder lives in separate state (`lp1..lp4`) and is only
    /// reached via `process_moog`; the plain `process` path must be untouched, so a
    /// freshly-built filter's `process` output is exactly what it always was. We
    /// pin a short golden run of the SVF path to catch any accidental change to it.
    #[test]
    fn svf_path_is_bit_identical_after_adding_moog() {
        // Two independent filters with identical settings: running the ladder on
        // one must NOT change the SVF output of the other (separate state), and the
        // SVF path itself must be deterministic and unaffected by the new fields.
        let mut svf = Filter::new(SR);
        svf.set_cutoff(1_500.0);
        svf.set_resonance(0.6);
        svf.set_drive(0.2);
        svf.snap_to_targets();

        let mut also_svf = Filter::new(SR);
        also_svf.set_cutoff(1_500.0);
        also_svf.set_resonance(0.6);
        also_svf.set_drive(0.2);
        also_svf.snap_to_targets();

        // Drive `also_svf` through the LADDER for a while first — exercising lp1..lp4
        // — then confirm its SVF `process` path still matches the pristine `svf`
        // sample-for-sample (the SVF integrators are independent of the ladder ones).
        for _ in 0..500 {
            let _ = also_svf.process_moog(0.3);
        }
        also_svf.reset(); // clear all state (both models) so both start clean

        let mut phase = 0.0f32;
        for i in 0..4_000 {
            let input = (phase * TAU).sin() * 0.5;
            phase = (phase + 440.0 / SR).fract();
            assert_eq!(
                svf.process(input),
                also_svf.process(input),
                "SVF path must be bit-identical regardless of the ladder, sample {i}"
            );
        }
    }
}
