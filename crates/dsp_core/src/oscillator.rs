//! A single band-limited-ish oscillator with analog-style pitch drift.
//!
//! The saw uses PolyBLEP to round off the discontinuity, which kills most of
//! the aliasing a naive ramp would fold back down into the audible range. It is
//! not perfect (we will revisit anti-aliasing in M6), but it is a correct,
//! cheap starting point and a good thing to write a spectral test against later.
//!
//! ## Waveforms
//!
//! * **Sine** — the trivial `sin(2πφ)`; no edges, so nothing to band-limit.
//! * **Saw** — a naive ramp with one PolyBLEP at its single discontinuity.
//! * **Pulse** — a variable-width square. A pulse has *two* edges per cycle: a
//!   rising one at phase 0 and a falling one at phase `pw` (the duty cycle). We
//!   band-limit *both* with PolyBLEP. Sweeping `pw` is pulse-width modulation
//!   (PWM), the hollow, vocal "wob" of a Juno/Jupiter.
//! * **Triangle** — built by *integrating* a band-limited square. A triangle is
//!   the running sum of a square wave, so if we start from an already
//!   anti-aliased square and integrate it, the triangle inherits that
//!   band-limiting for free. We use a *leaky* integrator (a one-pole that bleeds
//!   off slowly) so DC and rounding error can't accumulate and walk the signal
//!   off to one rail over time.
//! * **Fm** — a self-contained **2-operator FM** voice: one sine *modulator*
//!   bends the phase of one sine *carrier*. `fm_ratio` sets the
//!   carrier:modulator frequency ratio (integer ratios = harmonic/pitched tones,
//!   non-integer = clangorous/inharmonic), and `fm_index` is the classic FM
//!   "brightness/depth" — the peak phase deviation, which grows the sideband
//!   harmonics. FM synthesizes *wide, unbounded* spectra that will alias, so this
//!   arm is **2× oversampled** with a small decimation filter (see below). At
//!   `fm_index = 0` it collapses to a pure sine carrier with no sidebands.
//! * **Wavetable** — reads a band-limited single cycle out of the shared,
//!   mipmapped [`crate::wavetable`] bank, with a `wt_position` scan that morphs
//!   from the selected table toward the next one in the bank. The bank is built
//!   once at startup; the oscillator just steps its phase through it. See that
//!   module for the band-limiting (additive generation + mipmaps) story.
//!
//! Each oscillator also carries its own slow [`Drift`] generator. Real analog
//! oscillators are never perfectly in tune; they wander a few cents, and that
//! tiny independent movement is what makes stacked oscillators shimmer instead
//! of sounding static and digital. FM and Wavetable both reuse the same
//! drifted `frequency`/`increment` computed at the top of [`Oscillator::next_sample`],
//! so they inherit that analog wander for free.

use crate::wavetable;
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waveform {
    // NOTE: every existing variant keeps its original discriminant/order so
    // existing tests (and any serialized indices) stay valid; new variants are
    // APPENDED. `Fm` and `Wavetable` go on the end after `Triangle`, so the GUI
    // `OscWaveParam` round-trip (Saw=0, Pulse=1, Tri=2, Sine=3, FM=4, WT=5) and
    // the `From<OscWaveParam> for Waveform` map line up — and a default patch,
    // which is a Saw, never reaches the new arms (bit-identity preserved).
    Sine,
    Saw,
    Pulse,
    Triangle,
    /// 2-operator FM (sine carrier phase-modulated by a sine modulator).
    Fm,
    /// Mipmapped wavetable read with a position-scan morph.
    Wavetable,
}

/// Number of *extra* detuned saw copies the supersaw stacks around the main
/// phase. Seven total voices (1 center + 6 satellites) is the classic JP-8000
/// "Super Saw" count.
const SUPERSAW_SATELLITES: usize = 6;

/// JP-8000-style detune spread, in cents, for the six satellites at full
/// supersaw amount. Three symmetric pairs spreading progressively wider; the
/// amount knob scales these toward 0 (where every copy collapses onto the main
/// phase and the supersaw vanishes). The shape (tight inner pair, wide outer
/// pair) is what gives the classic "shimmering wall of saws".
const SUPERSAW_DETUNE_CENTS: [f32; SUPERSAW_SATELLITES] =
    [-11.0, 11.0, -24.0, 24.0, -40.0, 40.0];

#[derive(Clone, Debug)]
pub struct Oscillator {
    sample_rate: f32,
    /// Normalized phase in `0.0..1.0`.
    phase: f32,
    /// The note's target frequency, before drift is applied.
    frequency: f32,
    pub waveform: Waveform,
    /// Pulse duty cycle in `0.05..=0.95` — the fraction of each cycle the pulse
    /// spends "high". Only audible for [`Waveform::Pulse`]. 0.5 is a square.
    pulse_width: f32,
    /// Leaky-integrator state for the triangle. The triangle is the integral of
    /// a band-limited square; this holds the running sum between samples.
    tri_z: f32,
    /// Slow per-oscillator pitch wander — the "analog imperfection".
    drift: Drift,
    /// Maximum drift depth in cents (0.0 = perfectly stable / off).
    drift_depth_cents: f32,
    /// Supersaw amount in `0.0..=1.0`. At 0 the oscillator runs only its main
    /// phase (bit-identical to a plain saw); above 0 it sums six detuned saw
    /// satellites for the JP-8000 "Super Saw". Only the [`Waveform::Saw`] path
    /// reads this — other waveforms ignore it entirely.
    supersaw_amount: f32,
    /// Independent phase accumulators for the six supersaw satellites. They only
    /// advance while the supersaw is engaged on a saw, so a non-supersaw osc pays
    /// nothing for them.
    supersaw_phases: [f32; SUPERSAW_SATELLITES],

    // --- FM (2-operator) state. Only the `Waveform::Fm` arm reads these. ---
    /// Carrier:modulator frequency ratio. 1.0 = unison (modulator at the carrier
    /// pitch); integer ratios give harmonic/pitched tones, non-integer give
    /// inharmonic/clangorous ones. Default 1.0.
    fm_ratio: f32,
    /// FM index = peak phase deviation in **cycles** (the classic FM brightness /
    /// depth). 0.0 = pure sine carrier, no sidebands. The plugin pushes
    /// `knob * 8.0` here, so the 0..1 GUI knob spans 0..8 of phase deviation.
    /// Default 0.0 (inert).
    fm_index: f32,
    /// The modulator's own phase accumulator, separate from the carrier `phase`.
    /// Reset to 0 in [`Oscillator::reset`].
    fm_mod_phase: f32,
    /// One-sample history for the 2× FM decimation filter (the previous
    /// oversampled output, used by the 2-tap averaging low-pass). Carried on the
    /// oscillator so the half-band filter has continuity across output samples.
    fm_decim_z: f32,

    // --- Wavetable state. Only the `Waveform::Wavetable` arm reads these. ---
    /// Selected base table index into the [`crate::wavetable`] bank. `wt_position`
    /// 0.0 plays this table exactly. Default 0 (Sine).
    wt_table: usize,
    /// Position scan in `0..1`. 0.0 = the selected `wt_table`; sweeping toward 1.0
    /// morphs to the *next* table in the bank (wrapping). Default 0.0.
    wt_position: f32,
}

impl Oscillator {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            phase: 0.0,
            frequency: 440.0,
            waveform: Waveform::Sine,
            pulse_width: 0.5,
            tri_z: 0.0,
            drift: Drift::new(sample_rate, 0x2545_F491),
            drift_depth_cents: 0.0,
            supersaw_amount: 0.0,
            supersaw_phases: [0.0; SUPERSAW_SATELLITES],
            fm_ratio: 1.0,
            fm_index: 0.0,
            fm_mod_phase: 0.0,
            fm_decim_z: 0.0,
            wt_table: 0,
            wt_position: 0.0,
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.drift.set_sample_rate(sample_rate);
    }

    pub fn set_frequency(&mut self, hz: f32) {
        self.frequency = hz;
    }

    /// Set the pulse duty cycle, clamped to `0.05..=0.95`. The clamp keeps both
    /// the high and low portions of the pulse non-degenerate (a 0%/100% duty is
    /// silence, not a waveform). Only affects [`Waveform::Pulse`].
    pub fn set_pulse_width(&mut self, pw: f32) {
        self.pulse_width = pw.clamp(0.05, 0.95);
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.tri_z = 0.0;
        // FM operator + decimation state start clean so a freshly-triggered FM
        // voice is deterministic and free of leftover ringing.
        self.fm_mod_phase = 0.0;
        self.fm_decim_z = 0.0;
        // Spread the satellite phases out a little so a freshly-triggered
        // supersaw doesn't momentarily phase-align into one loud saw before the
        // detune pulls them apart. The offsets are arbitrary but fixed (no RNG),
        // so the start is deterministic and test-stable.
        for (k, p) in self.supersaw_phases.iter_mut().enumerate() {
            *p = (k as f32 + 1.0) / (SUPERSAW_SATELLITES as f32 + 1.0);
        }
    }

    /// Set the supersaw amount in `0.0..=1.0`. At 0 the oscillator is a plain
    /// saw (the satellites contribute nothing); above 0 it fattens into a stack
    /// of detuned saws. Only [`Waveform::Saw`] uses this.
    pub fn set_supersaw_amount(&mut self, amount: f32) {
        self.supersaw_amount = amount.clamp(0.0, 1.0);
    }

    /// Set the FM carrier:modulator frequency **ratio** (clamped to `0.25..=16`).
    /// 1.0 is unison; integer ratios give harmonic/pitched tones, non-integer
    /// give inharmonic/clangorous ones. Only [`Waveform::Fm`] reads this.
    pub fn set_fm_ratio(&mut self, ratio: f32) {
        self.fm_ratio = ratio.clamp(0.25, 16.0);
    }

    /// Set the FM **index** = peak phase deviation in *cycles* (the classic FM
    /// brightness/depth knob). 0.0 is a pure sine carrier (no sidebands); higher
    /// values pull up progressively brighter sideband harmonics. The plugin scales
    /// its 0..1 knob by 8.0 before calling this, so the usable range is ~0..8.
    /// Clamped non-negative; only [`Waveform::Fm`] reads it.
    pub fn set_fm_index(&mut self, index: f32) {
        self.fm_index = index.max(0.0);
    }

    /// Select the base wavetable (clamped into the bank's range). `wt_position`
    /// 0.0 plays this table exactly; sweeping toward 1.0 morphs to the next table
    /// in the bank. Only [`Waveform::Wavetable`] reads this.
    pub fn set_wt_table(&mut self, table: usize) {
        self.wt_table = table.min(wavetable::N_TABLES - 1);
    }

    /// Set the wavetable position-scan in `0.0..=1.0`. 0.0 = the selected
    /// `wt_table` exactly; 1.0 = the next table in the bank (wrapping); values
    /// between morph linearly. Only [`Waveform::Wavetable`] reads this.
    pub fn set_wt_position(&mut self, position: f32) {
        self.wt_position = position.clamp(0.0, 1.0);
    }

    /// Set how far this oscillator is allowed to drift out of tune, in cents.
    pub fn set_drift_depth_cents(&mut self, cents: f32) {
        self.drift_depth_cents = cents;
    }

    /// Reseed the drift generator so stacked/parallel oscillators wander
    /// independently rather than in lockstep.
    pub fn reseed_drift(&mut self, seed: u32) {
        self.drift.reseed(seed);
    }

    /// Advance one sample and return the next oscillator value in `-1.0..=1.0`.
    pub fn next_sample(&mut self) -> f32 {
        // Apply slow pitch drift. For the few-cent offsets involved,
        // `2^(cents/1200)` is almost perfectly linear, so we skip the expensive
        // `powf` and use the first-order approximation.
        let drift_cents = self.drift.next() * self.drift_depth_cents;
        let frequency = self.frequency * (1.0 + drift_cents * CENTS_TO_RATIO_SLOPE);
        let increment = frequency / self.sample_rate;

        let value = match self.waveform {
            Waveform::Sine => (self.phase * TAU).sin(),
            Waveform::Saw => {
                let mut v = 2.0 * self.phase - 1.0;
                v -= poly_blep(self.phase, increment);
                // SUPERSAW: at amount 0 this whole block is skipped, so the saw
                // is bit-identical to before. Above 0 we sum six detuned saw
                // satellites and mix them with the center using a JP-8000-style
                // "center loses gain as the sides come up" balance, then
                // normalize so the stack stays inside the saw's range envelope.
                if self.supersaw_amount > 0.0 {
                    v = self.supersaw_mix(v, frequency);
                }
                v
            }
            Waveform::Pulse => {
                // A trivial pulse is +1 while phase < duty, -1 after. That has a
                // rising edge at phase 0 and a falling edge at phase = pw. We
                // correct each edge with a PolyBLEP: ADD at the rising edge,
                // SUBTRACT at the falling edge (BLEPs are signed by edge
                // direction). The falling edge sits at phase `pw`, so we shift
                // the BLEP's reference point to it by offsetting the phase.
                let pw = self.pulse_width;
                let mut v = if self.phase < pw { 1.0 } else { -1.0 };
                v += poly_blep(self.phase, increment);
                v -= poly_blep((self.phase + (1.0 - pw)).fract(), increment);
                v
            }
            Waveform::Triangle => {
                // Integrate a band-limited square into a triangle. First build
                // the anti-aliased square (the pw=0.5 pulse), then leaky-
                // integrate it. The `* increment * 4.0` scales the slope so the
                // triangle spans ~[-1, 1] independent of frequency (a higher
                // note advances phase faster, so each step contributes more).
                // The `* 0.999` leak slowly forgets old error, preventing the
                // integrator from drifting to a rail (DC build-up).
                let mut sq = if self.phase < 0.5 { 1.0 } else { -1.0 };
                sq += poly_blep(self.phase, increment);
                sq -= poly_blep((self.phase + 0.5).fract(), increment);
                self.tri_z = sq * increment * 4.0 + self.tri_z * 0.999;
                self.tri_z
            }
            Waveform::Fm => self.fm_sample(frequency, increment),
            Waveform::Wavetable => {
                // Read the band-limited mipmapped bank. Pick the mip level for this
                // (drifted) frequency so a high note uses a harmonically-thinned
                // table and doesn't alias. Then morph from the selected table toward
                // the NEXT table by `wt_position` — position 0 = selected table,
                // position 1 = next table (wrapping). Both reads share the same mip
                // level and the same phase, so the morph is a clean per-sample lerp.
                let bank = wavetable::bank();
                let mip = bank.mip_for_freq(frequency, self.sample_rate);
                let a = bank.sample(self.wt_table, mip, self.phase);
                let b = bank.sample(self.wt_table + 1, mip, self.phase); // bank wraps % N
                a + (b - a) * self.wt_position
            }
        };

        self.phase += increment;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        value
    }

    /// One output sample of the 2-operator FM voice, **2× oversampled**.
    ///
    /// FM (really *phase* modulation here — the standard, stable form) generates a
    /// wide, in principle unbounded spectrum: high `fm_index` and/or high
    /// `fm_ratio` pile up sidebands far above the carrier, and anything above
    /// Nyquist folds back as aliasing. We can't band-limit FM the way we BLEP a
    /// saw, so we attack the worst of it by running the operator update **twice
    /// per output sample at half the increments** and then decimating with a cheap
    /// 2-tap averaging low-pass (a simple half-band-ish FIR) before returning one
    /// sample. That pushes the effective Nyquist up an octave for the internal
    /// computation, so harmonics that would have folded now mostly land above the
    /// base-rate Nyquist and get attenuated by the decimator instead of folding.
    ///
    /// **2× is a pragmatic compromise, not a cure.** High index + high ratio +
    /// high notes can still fold a little; 2× cuts the worst of it and keeps CPU
    /// sane for 16-voice polyphony. (Documented fallback if 2× ever proves too hot
    /// in profiling: clamp the effective index as the carrier frequency rises, so
    /// the highest notes simply get less bright instead of aliasing — leave that as
    /// a fallback, not the default.)
    ///
    /// The carrier reuses the already-drifted `frequency`/`increment` from
    /// [`Oscillator::next_sample`], so FM inherits analog drift for free. The
    /// shared `self.phase += increment` at the bottom of `next_sample` advances the
    /// carrier base phase by one *full* output-rate step; here we only read a local
    /// half-stepped copy of it and advance the persistent modulator phase.
    #[inline]
    fn fm_sample(&mut self, frequency: f32, increment: f32) -> f32 {
        // Half-rate steps for the 2× oversampling.
        let half_carrier_inc = 0.5 * increment;
        let mod_freq = frequency * self.fm_ratio;
        let half_mod_inc = 0.5 * (mod_freq / self.sample_rate);

        // A local carrier phase so we DON'T double-advance `self.phase` (the shared
        // bottom advance already moves it by the full `increment`). We start at the
        // current base phase and step it by the half-increment for each subsample.
        let mut carrier_phase = self.phase;
        let mut acc = 0.0f32;
        for _ in 0..2 {
            // Phase-modulation form: add `index * modulator` (in cycles) to the
            // carrier phase, then take the sine. Both operators are sines.
            let modulator = (self.fm_mod_phase * TAU).sin();
            let sub = ((carrier_phase + self.fm_index * modulator) * TAU).sin();

            // 2-tap averaging decimation FIR: average this subsample with the last
            // one. It is a gentle half-band low-pass — cheap, and enough to knock
            // down the just-above-Nyquist images the oversampling exposed.
            acc += 0.5 * (sub + self.fm_decim_z);
            self.fm_decim_z = sub;

            // Advance both half-rate accumulators for the next subsample.
            carrier_phase += half_carrier_inc;
            if carrier_phase >= 1.0 {
                carrier_phase -= 1.0;
            }
            self.fm_mod_phase += half_mod_inc;
            if self.fm_mod_phase >= 1.0 {
                self.fm_mod_phase -= 1.0;
            }
        }
        // Average the two decimated subsamples into one output sample.
        0.5 * acc
    }

    /// Mix the six supersaw satellites into the center saw `center`.
    ///
    /// `base_freq` is the already-drifted main frequency. Each satellite runs its
    /// own phase accumulator detuned by `SUPERSAW_DETUNE_CENTS[k] * amount` cents,
    /// and is a trivial (un-BLEP'd) saw — the satellites are cheap on purpose; the
    /// dense beating between them, not their individual purity, is what makes the
    /// sound. We balance center-vs-sides the JP-8000 way: as `amount` rises the
    /// center fades and the sides come up, then normalize by the total gain so the
    /// summed output keeps the single-saw amplitude envelope (so `saw_stays_in_range`
    /// holds without widening its bound).
    fn supersaw_mix(&mut self, center: f32, base_freq: f32) -> f32 {
        let amount = self.supersaw_amount;
        // Center loses level as the sides come up; sides scale up with amount.
        // These are bounded so the normalized sum never exceeds the center saw's
        // own range.
        let center_gain = 1.0 - 0.5 * amount;
        let side_gain = 0.55 * amount;

        let mut sum = center * center_gain;
        let mut total_gain = center_gain;
        let inv_sr = 1.0 / self.sample_rate;
        for (ph, &detune_base) in self
            .supersaw_phases
            .iter_mut()
            .zip(SUPERSAW_DETUNE_CENTS.iter())
        {
            let detune = detune_base * amount;
            let freq = base_freq * (1.0 + detune * CENTS_TO_RATIO_SLOPE);
            let inc = freq * inv_sr;
            *ph += inc;
            if *ph >= 1.0 {
                *ph -= 1.0;
            }
            // Trivial saw for the satellite (no PolyBLEP — kept deliberately cheap).
            let s = 2.0 * *ph - 1.0;
            sum += s * side_gain;
            total_gain += side_gain;
        }
        // Normalize so the whole stack sits at roughly the single-saw level.
        sum / total_gain
    }
}

/// PolyBLEP correction for the discontinuity of a trivial saw/pulse.
fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        let t = t / dt;
        2.0 * t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + 2.0 * t + 1.0
    } else {
        0.0
    }
}

/// Cents-to-ratio slope: `2^(c/1200) ≈ 1 + c * ln(2)/1200` for small `c`.
/// `ln(2)/1200 ≈ 0.00057762`.
const CENTS_TO_RATIO_SLOPE: f32 = 0.000_577_623;

/// A slow, smooth random pitch wander — the heart of "analog drift".
///
/// Every ~70 ms it picks a new random target in `-1.0..1.0` and one-pole-slews
/// toward it, producing a continuous, gently meandering value. Multiplying that
/// by a depth in cents gives an oscillator that is never quite in tune. Uses a
/// tiny xorshift PRNG (no allocation, no entropy) so it is cheap and, given a
/// fixed seed, reproducible in tests.
#[derive(Clone, Debug)]
struct Drift {
    rng: u32,
    value: f32,
    target: f32,
    counter: u32,
    interval: u32,
}

impl Drift {
    fn new(sample_rate: f32, seed: u32) -> Self {
        let mut d = Self {
            rng: seed | 1,
            value: 0.0,
            target: 0.0,
            counter: 0,
            interval: 1,
        };
        d.set_sample_rate(sample_rate);
        d
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        // Pick a new random target roughly every 70 ms.
        self.interval = (sample_rate * 0.07).max(1.0) as u32;
    }

    fn reseed(&mut self, seed: u32) {
        self.rng = seed | 1; // avoid the all-zero xorshift fixed point
    }

    fn next(&mut self) -> f32 {
        self.counter += 1;
        if self.counter >= self.interval {
            self.counter = 0;
            self.target = self.next_bipolar();
        }
        // One-pole slew toward the target for a smooth, continuous wander.
        self.value += (self.target - self.value) * 0.0008;
        self.value
    }

    fn next_bipolar(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// The kind of noise to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseType {
    /// Flat spectrum — equal energy per Hz. Bright, hissy.
    White,
    /// `1/f` spectrum — equal energy per *octave*. Darker, "natural", the rush
    /// of wind or surf rather than TV static.
    Pink,
}

/// A tiny stereo-friendly noise source: white plus a cheap pink approximation.
///
/// This is **not** an [`Oscillator`] — noise has no pitch, no phase, no drift —
/// so it lives as its own small generator. White noise is just a stream of
/// uniform random samples from the same xorshift PRNG the [`Drift`] uses. Pink
/// noise is white run through Paul Kellet's one-pole "economy" filter: a handful
/// of leaky integrators summed together approximate the `-3 dB/octave` pink tilt
/// well enough for a synth noise layer (it is not laboratory-flat, but it sounds
/// right and costs almost nothing).
///
/// Each voice seeds its own `Noise` so stacked voices are decorrelated rather
/// than all hissing in perfect sync (which would sound like one loud mono hiss).
#[derive(Clone, Debug)]
pub struct Noise {
    rng: u32,
    // Paul Kellet pink-filter state (the running poles).
    b0: f32,
    b1: f32,
    b2: f32,
}

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: seed | 1, // avoid the all-zero xorshift fixed point
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
        }
    }

    /// Reseed so a different voice's noise is independent of this one's.
    pub fn reseed(&mut self, seed: u32) {
        self.rng = seed | 1;
    }

    /// Next white sample in `-1.0..1.0` (the raw PRNG draw).
    fn next_white(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Next sample of the requested noise color in roughly `-1.0..1.0`.
    pub fn next(&mut self, kind: NoiseType) -> f32 {
        let white = self.next_white();
        match kind {
            NoiseType::White => white,
            NoiseType::Pink => {
                // Paul Kellet's 3-pole economy pink filter. Each pole is a leaky
                // one-pole low-pass of the white input; summing poles tuned to
                // different time constants stacks up the `1/f` tilt. The output
                // is scaled to keep it in a sane range next to white.
                self.b0 = 0.99765 * self.b0 + white * 0.0990460;
                self.b1 = 0.96300 * self.b1 + white * 0.2965164;
                self.b2 = 0.57000 * self.b2 + white * 1.0526913;
                let pink = self.b0 + self.b1 + self.b2 + white * 0.1848;
                // The raw sum runs hot; scale down to keep peaks near ±1.
                pink * 0.2
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_stays_in_range() {
        let mut osc = Oscillator::new(48_000.0);
        osc.set_frequency(440.0);
        for _ in 0..48_000 {
            let s = osc.next_sample();
            assert!((-1.0001..=1.0001).contains(&s), "sine out of range: {s}");
        }
    }

    #[test]
    fn sine_frequency_via_zero_crossings() {
        let sr = 48_000.0;
        let mut osc = Oscillator::new(sr);
        osc.set_frequency(100.0);

        let mut prev = osc.next_sample();
        let mut rising = 0;
        for _ in 0..(sr as usize) {
            let cur = osc.next_sample();
            if prev < 0.0 && cur >= 0.0 {
                rising += 1;
            }
            prev = cur;
        }
        // A 100 Hz tone (drift off by default) should cross zero rising ~100x/s.
        assert!((rising - 100i32).abs() <= 1, "expected ~100 crossings, got {rising}");
    }

    #[test]
    fn saw_stays_in_range() {
        let mut osc = Oscillator::new(48_000.0);
        osc.waveform = Waveform::Saw;
        osc.set_frequency(220.0);
        for _ in 0..48_000 {
            let s = osc.next_sample();
            assert!((-1.5..=1.5).contains(&s), "saw wildly out of range: {s}");
        }
    }

    #[test]
    fn pulse_stays_in_range() {
        let mut osc = Oscillator::new(48_000.0);
        osc.waveform = Waveform::Pulse;
        osc.set_frequency(220.0);
        for _ in 0..48_000 {
            let s = osc.next_sample();
            assert!((-1.5..=1.5).contains(&s), "pulse wildly out of range: {s}");
        }
    }

    /// Changing the pulse width changes the waveform's average (DC) level: a
    /// narrow duty spends most of its time at -1, so the mean is negative; a wide
    /// duty spends most of its time at +1, so the mean is positive. A 50% square
    /// averages ~0. This proves PWM is actually wired to the shape.
    #[test]
    fn pulse_width_shifts_dc_offset() {
        fn mean(pw: f32) -> f32 {
            let mut osc = Oscillator::new(48_000.0);
            osc.waveform = Waveform::Pulse;
            osc.set_frequency(110.0);
            osc.set_pulse_width(pw);
            let n = 48_000;
            let mut sum = 0.0f32;
            for _ in 0..n {
                sum += osc.next_sample();
            }
            sum / n as f32
        }

        let narrow = mean(0.1); // mostly low -> negative average
        let square = mean(0.5); // balanced -> ~0
        let wide = mean(0.9); // mostly high -> positive average

        assert!(narrow < -0.5, "narrow pulse should have negative DC: {narrow}");
        assert!(square.abs() < 0.1, "50% pulse should be ~0 DC: {square}");
        assert!(wide > 0.5, "wide pulse should have positive DC: {wide}");
    }

    #[test]
    fn triangle_stays_in_range() {
        let mut osc = Oscillator::new(48_000.0);
        osc.waveform = Waveform::Triangle;
        osc.set_frequency(220.0);
        // Skip the first cycles while the leaky integrator settles its startup
        // transient (it ramps up from 0 and overshoots slightly before the leak
        // pulls it down to its steady amplitude).
        for _ in 0..4_800 {
            osc.next_sample();
        }
        for _ in 0..48_000 {
            let s = osc.next_sample();
            assert!((-1.6..=1.6).contains(&s), "triangle out of range: {s}");
        }
    }

    /// A triangle is far less harmonically rich than a saw: it moves smoothly
    /// (its sample-to-sample slope is roughly constant within a ramp), so its
    /// mean absolute second difference — a crude "edge energy" metric — is much
    /// lower than a saw of the same pitch, which has a hard once-per-cycle jump.
    #[test]
    fn triangle_is_smoother_than_saw() {
        fn edge_energy(wave: Waveform) -> f32 {
            let mut osc = Oscillator::new(48_000.0);
            osc.waveform = wave;
            osc.set_frequency(220.0);
            for _ in 0..512 {
                osc.next_sample(); // settle
            }
            let mut p0 = osc.next_sample();
            let mut p1 = osc.next_sample();
            let mut sum = 0.0f32;
            let n = 16_000;
            for _ in 0..n {
                let cur = osc.next_sample();
                // second difference ~ curvature / edge content
                sum += ((cur - p1) - (p1 - p0)).abs();
                p0 = p1;
                p1 = cur;
            }
            sum / n as f32
        }

        let tri = edge_energy(Waveform::Triangle);
        let saw = edge_energy(Waveform::Saw);
        assert!(tri < saw, "triangle should be smoother than saw: tri={tri}, saw={saw}");
    }

    #[test]
    fn noise_is_bounded_and_white_differs_from_pink() {
        let mut white = Noise::new(0xABCD_1234);
        let mut pink = Noise::new(0xABCD_1234); // same seed -> same white stream

        let mut any_diff = false;
        for _ in 0..48_000 {
            let w = white.next(NoiseType::White);
            let p = pink.next(NoiseType::Pink);
            assert!((-2.0..=2.0).contains(&w), "white out of range: {w}");
            assert!((-2.0..=2.0).contains(&p), "pink out of range: {p}");
            if (w - p).abs() > 1e-6 {
                any_diff = true;
            }
        }
        // The pink filter colors the (identical) white stream, so the two
        // outputs must not be sample-for-sample equal.
        assert!(any_diff, "pink should differ from white after filtering");
    }

    /// At supersaw amount 0 the oscillator must be bit-identical to a plain saw:
    /// the satellite phases never advance and contribute nothing. This is the
    /// regression guard that flipping the param without raising it changes nothing.
    #[test]
    fn supersaw_zero_is_bit_identical_to_plain_saw() {
        let mut plain = Oscillator::new(48_000.0);
        plain.waveform = Waveform::Saw;
        plain.set_frequency(220.0);

        let mut sup = Oscillator::new(48_000.0);
        sup.waveform = Waveform::Saw;
        sup.set_frequency(220.0);
        sup.set_supersaw_amount(0.0);

        for i in 0..48_000 {
            assert_eq!(
                plain.next_sample(),
                sup.next_sample(),
                "supersaw=0 must equal a plain saw at sample {i}"
            );
        }
    }

    /// An engaged supersaw stays inside the saw's range envelope (we normalize so
    /// it does not need a wider bound than a single saw).
    #[test]
    fn supersaw_stays_in_range() {
        let mut osc = Oscillator::new(48_000.0);
        osc.waveform = Waveform::Saw;
        osc.set_frequency(220.0);
        osc.set_supersaw_amount(1.0);
        for _ in 0..48_000 {
            let s = osc.next_sample();
            assert!((-1.5..=1.5).contains(&s), "supersaw out of range: {s}");
        }
    }

    /// An engaged supersaw must add the characteristic *beating*: seven detuned
    /// saws drift in and out of phase, so the signal's short-term loudness
    /// (windowed RMS) pulses over time. A single saw is steady, so its
    /// window-to-window RMS barely varies. We measure the variance of the
    /// per-window RMS and require the supersaw's to be clearly larger — the
    /// audible "shimmering wall of saws".
    #[test]
    fn supersaw_widens_the_spectrum() {
        fn rms_variation(amount: f32) -> f32 {
            let mut osc = Oscillator::new(48_000.0);
            osc.waveform = Waveform::Saw;
            osc.set_frequency(110.0);
            osc.set_supersaw_amount(amount);
            for _ in 0..2_000 {
                osc.next_sample(); // settle
            }
            // Collect per-window RMS values across a couple of seconds, long
            // enough for the slow inter-saw beating to swing.
            let window = 256;
            let windows = 360;
            let mut rms = Vec::with_capacity(windows);
            for _ in 0..windows {
                let mut sq = 0.0f32;
                for _ in 0..window {
                    let s = osc.next_sample();
                    sq += s * s;
                }
                rms.push((sq / window as f32).sqrt());
            }
            let mean = rms.iter().sum::<f32>() / rms.len() as f32;
            // Coefficient of variation so the metric is amplitude-independent.
            let var = rms.iter().map(|r| (r - mean).powi(2)).sum::<f32>() / rms.len() as f32;
            var.sqrt() / mean.max(1e-6)
        }

        let single = rms_variation(0.0);
        let fat = rms_variation(1.0);
        assert!(
            fat > single * 1.5,
            "supersaw should beat (fluctuating loudness): single={single}, fat={fat}"
        );
    }

    /// Supersaw only affects the saw waveform: a sine with the supersaw amount
    /// cranked must be bit-identical to a plain sine (other waveforms ignore it).
    #[test]
    fn supersaw_does_not_touch_non_saw_waveforms() {
        let mut plain = Oscillator::new(48_000.0);
        plain.set_frequency(330.0); // sine by default

        let mut sup = Oscillator::new(48_000.0);
        sup.set_frequency(330.0);
        sup.set_supersaw_amount(1.0);

        for i in 0..4_800 {
            assert_eq!(
                plain.next_sample(),
                sup.next_sample(),
                "supersaw must not affect a sine at sample {i}"
            );
        }
    }

    // ----- FM oscillator tests ------------------------------------------------

    /// A tiny DFT magnitude at integer harmonic `k` of `f0` over a buffer. Used to
    /// compare spectral content (fundamental vs sidebands) without pulling in an
    /// FFT crate.
    fn goertzel_mag(buf: &[f32], f0: f32, k: f32, sr: f32) -> f32 {
        let freq = f0 * k;
        let w = TAU * freq / sr;
        let (cw, sw) = (w.cos(), w.sin());
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (n, &x) in buf.iter().enumerate() {
            let ang = w * n as f32;
            // Direct (slow but simple) bin accumulation; fine for a test buffer.
            re += x * ang.cos();
            im -= x * ang.sin();
        }
        let _ = (cw, sw);
        (re * re + im * im).sqrt() / buf.len() as f32
    }

    fn render_fm(ratio: f32, index: f32, freq: f32, n: usize) -> Vec<f32> {
        let mut osc = Oscillator::new(48_000.0);
        osc.waveform = Waveform::Fm;
        osc.set_frequency(freq);
        osc.set_fm_ratio(ratio);
        osc.set_fm_index(index);
        (0..n).map(|_| osc.next_sample()).collect()
    }

    /// At `fm_index = 0` the FM voice is a pure sine carrier: virtually all energy
    /// sits at the fundamental, with negligible sideband content. (It is not
    /// *bit-identical* to a raw `sin` because the 2× oversample + decimation FIR
    /// gently colors it, but it must contain no real sidebands.)
    #[test]
    fn fm_index_zero_is_pure_sine_carrier() {
        let sr = 48_000.0;
        let f0 = 200.0;
        let buf = render_fm(1.0, 0.0, f0, 48_000);
        let fund = goertzel_mag(&buf, f0, 1.0, sr);
        // Check several harmonics above the fundamental are all far weaker — a
        // pure sine has no harmonics, so any sideband energy is near zero.
        for k in [2.0, 3.0, 4.0, 5.0] {
            let side = goertzel_mag(&buf, f0, k, sr);
            assert!(
                side < fund * 0.02,
                "index=0 must be a pure sine: harmonic {k} = {side} vs fund {fund}"
            );
        }
    }

    /// Raising the FM index *adds sidebands*: a high-index FM tone has dramatically
    /// more energy in its upper harmonics than the same carrier at index 0. This is
    /// the defining behavior of FM — index is the brightness/depth control.
    #[test]
    fn fm_index_adds_sidebands() {
        let sr = 48_000.0;
        let f0 = 200.0;
        // Ratio 1 => sidebands land on integer harmonics of f0, so we can probe them.
        let quiet = render_fm(1.0, 0.0, f0, 48_000);
        let bright = render_fm(1.0, 5.0, f0, 48_000);

        // Sum the energy in harmonics 2..8 (the sidebands) for each.
        let upper = |buf: &[f32]| -> f32 {
            (2..=8).map(|k| goertzel_mag(buf, f0, k as f32, sr)).sum::<f32>()
        };
        let quiet_upper = upper(&quiet);
        let bright_upper = upper(&bright);
        assert!(
            bright_upper > quiet_upper * 10.0,
            "raising FM index must add sidebands: quiet={quiet_upper}, bright={bright_upper}"
        );
    }

    /// The FM output must stay bounded across a wide range of ratio/index/pitch —
    /// the carrier is a sine so the instantaneous output is always within ±1, and
    /// the decimation filter never amplifies. No NaN, no runaway.
    #[test]
    fn fm_stays_finite_and_bounded() {
        for &ratio in &[0.25, 1.0, 2.0, 3.5, 7.0, 16.0] {
            for &index in &[0.0, 1.0, 4.0, 8.0] {
                for &freq in &[55.0, 440.0, 3_000.0] {
                    let buf = render_fm(ratio, index, freq, 8_000);
                    for &s in &buf {
                        assert!(s.is_finite(), "FM non-finite (r={ratio}, i={index}, f={freq})");
                        assert!(
                            (-1.5..=1.5).contains(&s),
                            "FM out of range (r={ratio}, i={index}, f={freq}): {s}"
                        );
                    }
                }
            }
        }
    }

    /// FM mode must only be reached when `waveform == Fm`: a sine osc with the FM
    /// ratio and index cranked must be bit-identical to a plain sine, proving the
    /// FM arm never touches the other waveforms (and the default-Saw bit-identity
    /// guarantee holds — the FM code is dead on any non-FM waveform).
    #[test]
    fn fm_does_not_touch_other_waveforms() {
        for wave in [Waveform::Sine, Waveform::Saw, Waveform::Pulse, Waveform::Triangle] {
            let mut plain = Oscillator::new(48_000.0);
            plain.waveform = wave;
            plain.set_frequency(330.0);

            let mut fmd = Oscillator::new(48_000.0);
            fmd.waveform = wave;
            fmd.set_frequency(330.0);
            fmd.set_fm_ratio(3.0);
            fmd.set_fm_index(8.0);

            for i in 0..4_800 {
                assert_eq!(
                    plain.next_sample(),
                    fmd.next_sample(),
                    "FM params must not affect {wave:?} at sample {i}"
                );
            }
        }
    }

    // ----- Wavetable oscillator tests -----------------------------------------

    /// A wavetable osc on table 0 (Sine), position 0, must produce a recognizable
    /// sine: dominated by its fundamental with little upper-harmonic content. This
    /// proves the bank is wired in and read at the right phase.
    #[test]
    fn wavetable_sine_table_matches_sine_within_tol() {
        let sr = 48_000.0;
        let f0 = 220.0;
        let mut osc = Oscillator::new(sr);
        osc.waveform = Waveform::Wavetable;
        osc.set_frequency(f0);
        osc.set_wt_table(0); // Sine
        osc.set_wt_position(0.0);
        let buf: Vec<f32> = (0..48_000).map(|_| osc.next_sample()).collect();
        let fund = goertzel_mag(&buf, f0, 1.0, sr);
        let second = goertzel_mag(&buf, f0, 2.0, sr);
        let third = goertzel_mag(&buf, f0, 3.0, sr);
        assert!(fund > 0.1, "sine table should have a strong fundamental: {fund}");
        assert!(
            second < fund * 0.02 && third < fund * 0.02,
            "sine table should be nearly harmonic-free: 2nd={second}, 3rd={third}, fund={fund}"
        );
    }

    /// Output must stay in range for every table across the position sweep, at a
    /// high note (where mipmapping matters). No NaN, no runaway.
    #[test]
    fn wavetable_stays_in_range() {
        for table in 0..wavetable::N_TABLES {
            for &pos in &[0.0, 0.5, 1.0] {
                for &freq in &[55.0, 440.0, 4_000.0] {
                    let mut osc = Oscillator::new(48_000.0);
                    osc.waveform = Waveform::Wavetable;
                    osc.set_frequency(freq);
                    osc.set_wt_table(table);
                    osc.set_wt_position(pos);
                    for _ in 0..8_000 {
                        let s = osc.next_sample();
                        assert!(s.is_finite(), "WT non-finite (t={table}, pos={pos}, f={freq})");
                        assert!(
                            (-1.2..=1.2).contains(&s),
                            "WT out of range (t={table}, pos={pos}, f={freq}): {s}"
                        );
                    }
                }
            }
        }
    }

    /// Sweeping `wt_position` from the selected table toward the next one must
    /// *morph the spectrum*: at position 0 we hear the Sine (table 0, harmonic-
    /// free); at position 1 we hear the next table (Triangle, which has odd
    /// harmonics). So the upper-harmonic content must rise as position sweeps.
    #[test]
    fn wavetable_position_morphs_the_spectrum() {
        let sr = 48_000.0;
        let f0 = 220.0;
        fn upper_energy(pos: f32, f0: f32, sr: f32) -> f32 {
            let mut osc = Oscillator::new(sr);
            osc.waveform = Waveform::Wavetable;
            osc.set_frequency(f0);
            osc.set_wt_table(0); // Sine -> morphs toward Triangle (table 1)
            osc.set_wt_position(pos);
            let buf: Vec<f32> = (0..48_000).map(|_| osc.next_sample()).collect();
            // Triangle's energy is in its odd harmonics (3rd, 5th, ...).
            goertzel_mag(&buf, f0, 3.0, sr) + goertzel_mag(&buf, f0, 5.0, sr)
        }
        let at_sine = upper_energy(0.0, f0, sr); // pure sine: ~no upper harmonics
        let at_tri = upper_energy(1.0, f0, sr); // triangle: real odd harmonics
        assert!(
            at_tri > at_sine * 10.0,
            "position sweep must morph the timbre: sine_upper={at_sine}, tri_upper={at_tri}"
        );
    }

    /// A high note through a bright table (Saw) must not alias badly: the mipmap
    /// keeps the table's top harmonic under Nyquist, so almost no energy appears
    /// at *non-harmonic* (aliased/folded) frequencies. We probe a deliberately
    /// inharmonic bin and require it to stay far below the fundamental.
    #[test]
    fn wavetable_high_note_does_not_alias_badly() {
        let sr = 48_000.0;
        let f0 = 4_000.0; // high note: a naive full-band saw would fold hard
        let mut osc = Oscillator::new(sr);
        osc.waveform = Waveform::Wavetable;
        osc.set_frequency(f0);
        osc.set_wt_table(2); // Saw
        osc.set_wt_position(0.0);
        let buf: Vec<f32> = (0..48_000).map(|_| osc.next_sample()).collect();
        let fund = goertzel_mag(&buf, f0, 1.0, sr);
        // Probe a clearly inharmonic frequency (2.5× f0 = 10 kHz is not a harmonic
        // of 4 kHz; strong energy there would be folded aliasing). With mipmapping
        // it should be deep below the fundamental.
        let alias = goertzel_mag(&buf, f0, 2.5, sr);
        assert!(
            alias < fund * 0.1,
            "high-note wavetable aliasing too strong: alias={alias}, fund={fund}"
        );
    }

    /// Wavetable mode must only be reached when `waveform == Wavetable`: a saw with
    /// the WT table/position set must be bit-identical to a plain saw, proving the
    /// WT arm never touches other waveforms (default-Saw bit-identity holds).
    #[test]
    fn wavetable_does_not_touch_other_waveforms() {
        for wave in [Waveform::Sine, Waveform::Saw, Waveform::Pulse, Waveform::Triangle] {
            let mut plain = Oscillator::new(48_000.0);
            plain.waveform = wave;
            plain.set_frequency(330.0);

            let mut wt = Oscillator::new(48_000.0);
            wt.waveform = wave;
            wt.set_frequency(330.0);
            wt.set_wt_table(2);
            wt.set_wt_position(0.7);

            for i in 0..4_800 {
                assert_eq!(
                    plain.next_sample(),
                    wt.next_sample(),
                    "WT params must not affect {wave:?} at sample {i}"
                );
            }
        }
    }

    #[test]
    fn drift_makes_pitch_diverge_from_a_stable_oscillator() {
        let mut drifting = Oscillator::new(48_000.0);
        drifting.set_frequency(440.0);
        drifting.set_drift_depth_cents(50.0); // exaggerated so the effect is clear

        let mut stable = Oscillator::new(48_000.0);
        stable.set_frequency(440.0);

        let mut total_diff = 0.0f32;
        for _ in 0..48_000 {
            total_diff += (drifting.next_sample() - stable.next_sample()).abs();
        }
        assert!(total_diff > 1.0, "drift should detune the oscillator over time");
    }
}
