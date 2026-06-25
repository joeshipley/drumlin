//! A low-frequency oscillator (LFO) — a slow, sub-audio modulation source.
//!
//! An LFO is just an oscillator running *below* the audible range (fractions of
//! a Hz up to a few tens of Hz). You never hear it directly; instead its
//! bipolar `-1..+1` swing is routed through the mod matrix to *move* something
//! else — vibrato (pitch), tremolo (amp), a filter wobble (cutoff), PWM, and so
//! on. This is the classic "slow wave drives a fast wave" of every analog synth.
//!
//! ## What is different from [`crate::Oscillator`]?
//!
//! The audio oscillator is band-limited (PolyBLEP) because its harmonics live in
//! the audible range and would otherwise alias. An LFO runs so slowly that its
//! edges are far below Nyquist — a hard square at 2 Hz has no audible aliasing —
//! so we use the *trivial* (naive) waveforms here. They are cheaper and the
//! shape is exactly what the ear expects from a modulation source.
//!
//! ## Shapes
//!
//! * **Sine** — `sin(2π·phase)`, the smooth round wobble (vibrato/tremolo).
//! * **Triangle** — a linear up-then-down ramp; like a sine but with a sharper
//!   turn-around. Built as `1 - 4·|frac - 0.5|` and rescaled to `-1..+1`.
//! * **Saw** — a linear ramp that snaps back: `2·frac - 1`. Good for ramps and
//!   rising sweeps.
//! * **Square** — `+1` for the first half of the cycle, `-1` for the second. A
//!   hard on/off gate (trills, gated tremolo).
//! * **SampleHold** — a *stepped random* source: at every cycle wrap it draws a
//!   fresh random level (via the same xorshift PRNG the drift/noise use) and
//!   holds it flat until the next wrap. The staircase of random plateaus is the
//!   "computer/sci-fi burble" modulation.
//!
//! ## Rate, depth, phase, retrigger
//!
//! * **rate_hz** — cycles per second. The host can tempo-sync this, but that is
//!   resolved to a plain Hz *at the plugin layer* (like the delay) so the DSP
//!   stays host-agnostic and only ever sees a frequency.
//! * **depth** — the LFO's own `0..1` output scaler (separate from the mod
//!   matrix's per-slot depth). At `depth = 1` the wave spans the full `-1..+1`.
//! * **phase_offset** — where in the cycle the wave starts (`0..1`), so two LFOs
//!   can run a quarter-cycle apart, etc.
//! * **retrigger** — when true, [`Lfo::retrigger`] resets the phase to
//!   `phase_offset` on each note-on, so every note gets an identical, repeatable
//!   wobble (per-voice vibrato). When false the LFO free-runs across notes.
//!
//! ## Rate modulation
//!
//! [`Lfo::next`] takes a `rate_mult` argument so the mod matrix's `Lfo1Rate` /
//! `Lfo2Rate` destinations can speed the LFO up or slow it down: the effective
//! rate is `rate_hz * 2^(4·rate_mult)`, i.e. ±4 octaves at full modulation. The
//! `2^(...)` keeps rate changes musical (octave-per-unit), exactly like the
//! cutoff math in [`crate::Voice`].

use std::f32::consts::TAU;

/// The LFO waveform. **Order matters**: it must match the GUI shape selector
/// (∿ △ ◺ □ ⊓) and the plugin-mirror `LfoShapeParam` so the normalized enum
/// round-trip lines up. New shapes append at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    Saw,
    Square,
    SampleHold,
}

#[derive(Clone, Debug)]
pub struct Lfo {
    sample_rate: f32,
    /// Normalized phase in `0.0..1.0`.
    phase: f32,
    shape: LfoShape,
    /// Free-run rate in Hz (before any `rate_mult` from the mod matrix).
    rate_hz: f32,
    /// The LFO's own output scaler in `0..1` (distinct from the matrix depth).
    depth: f32,
    /// Start phase in `0..1`; where `retrigger` resets to.
    phase_offset: f32,
    /// Key-retrigger: reset phase on note-on when true; free-run when false.
    retrigger: bool,
    /// Current sample-and-hold output, redrawn at each phase wrap.
    sh_value: f32,
    /// The last value [`Lfo::next`] returned (for a non-advancing GUI read).
    last_value: f32,
    /// xorshift PRNG state for the SampleHold shape (same style as drift/noise).
    rng: u32,
}

impl Lfo {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            phase: 0.0,
            shape: LfoShape::Sine,
            rate_hz: 2.0,
            depth: 1.0,
            phase_offset: 0.0,
            retrigger: true,
            sh_value: 0.0,
            last_value: 0.0,
            rng: 0x1234_5678,
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    /// Push the LFO's configuration in a single call (the plugin fans this out
    /// per block, exactly like the oscillator setters). `phase_offset` is in
    /// `0..1` cycles; `rate_hz` and `depth` are clamped to sane ranges.
    pub fn set_params(
        &mut self,
        shape: LfoShape,
        rate_hz: f32,
        depth: f32,
        phase_offset: f32,
        retrigger: bool,
    ) {
        self.shape = shape;
        self.rate_hz = rate_hz.max(0.0);
        self.depth = depth.clamp(0.0, 1.0);
        self.phase_offset = phase_offset.rem_euclid(1.0);
        self.retrigger = retrigger;
    }

    /// Reset the cycle to the start and draw a fresh sample-and-hold value.
    /// The voice calls this from `note_on` only when this LFO has
    /// `retrigger == true`; free-running LFOs keep their phase across notes.
    ///
    /// We reset `phase` to `0.0` rather than to `phase_offset`: the offset is a
    /// pure *read* offset folded into the wave in [`Lfo::next`], so a retriggered
    /// LFO begins sampling at the offset position (`sin(2π·offset)`, etc.). That
    /// keeps the offset's meaning identical for free-running and retriggered
    /// LFOs — both read the wave at `phase + phase_offset`.
    pub fn retrigger(&mut self) {
        self.phase = 0.0;
        // Draw an initial S&H value so a retriggered S&H LFO starts on a real
        // step rather than whatever was held from the previous note.
        self.sh_value = self.next_bipolar_rand();
    }

    /// Whether this LFO retriggers on note-on (so the voice knows whether to
    /// call [`Lfo::retrigger`]).
    pub fn retriggers(&self) -> bool {
        self.retrigger
    }

    /// Reseed the SampleHold PRNG so per-voice LFOs draw independent random
    /// staircases rather than stepping in lockstep across a chord.
    pub fn reseed(&mut self, seed: u32) {
        self.rng = seed | 1; // avoid the all-zero xorshift fixed point
    }

    /// Advance one sample and return the **bipolar** output in `-depth..+depth`
    /// (so `-1..+1` at full depth). `rate_mult` is the mod-matrix rate
    /// modulation in `-1..+1`: the effective rate is `rate_hz * 2^(4·rate_mult)`
    /// — ±4 octaves at full modulation. Pass `0.0` for no rate modulation.
    pub fn next(&mut self, rate_mult: f32) -> f32 {
        // Resolve the effective rate. The `2^(4·mult)` keeps rate changes
        // octave-linear, the same logarithmic-pitch trick the cutoff uses.
        let rate = if rate_mult == 0.0 {
            self.rate_hz
        } else {
            self.rate_hz * (4.0 * rate_mult).exp2()
        };
        let increment = rate / self.sample_rate;

        // Sample the shape at the *current* phase (offset folded in).
        let p = (self.phase + self.phase_offset).rem_euclid(1.0);
        let raw = match self.shape {
            LfoShape::Sine => (p * TAU).sin(),
            // Triangle: 0 at p=0, +1 at p=0.25, 0 at p=0.5, -1 at p=0.75. The
            // `|frac-0.5|` makes a V; `1 - 4·V` flips/scales it into a wave that
            // peaks at +1 and troughs at -1.
            LfoShape::Triangle => 1.0 - 4.0 * (p - 0.5).abs(),
            LfoShape::Saw => 2.0 * p - 1.0,
            LfoShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            // SampleHold holds `sh_value` flat; it is only redrawn at the wrap
            // below, so within a cycle it is a constant plateau.
            LfoShape::SampleHold => self.sh_value,
        };

        // Advance the phase and detect a cycle wrap. On a wrap, the SampleHold
        // shape draws a new random plateau (the "stepped random" staircase).
        self.phase += increment;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            if self.shape == LfoShape::SampleHold {
                self.sh_value = self.next_bipolar_rand();
            }
        }

        let out = raw * self.depth;
        self.last_value = out;
        out
    }

    /// The last value [`Lfo::next`] produced, without advancing — for the GUI
    /// LED ribbon to read the live LFO position cheaply.
    pub fn value(&self) -> f32 {
        self.last_value
    }

    /// One xorshift draw mapped to bipolar `-1..+1` (same PRNG shape as the
    /// drift/noise generators, so the S&H staircase is cheap and reproducible).
    fn next_bipolar_rand(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run an LFO for one full cycle and collect the samples.
    fn one_cycle(lfo: &mut Lfo) -> Vec<f32> {
        // At 1 Hz on a 48 kHz rate, one cycle is exactly 48_000 samples.
        (0..48_000).map(|_| lfo.next(0.0)).collect()
    }

    #[test]
    fn every_shape_stays_in_bipolar_range() {
        let sr = 48_000.0;
        for shape in [
            LfoShape::Sine,
            LfoShape::Triangle,
            LfoShape::Saw,
            LfoShape::Square,
            LfoShape::SampleHold,
        ] {
            let mut lfo = Lfo::new(sr);
            lfo.set_params(shape, 1.0, 1.0, 0.0, true);
            lfo.retrigger();
            for _ in 0..(48_000 * 4) {
                let v = lfo.next(0.0);
                assert!(
                    (-1.0001..=1.0001).contains(&v),
                    "{shape:?} out of bipolar range: {v}"
                );
            }
        }
    }

    #[test]
    fn sine_swings_both_polarities_and_centers_on_zero() {
        let mut lfo = Lfo::new(48_000.0);
        lfo.set_params(LfoShape::Sine, 1.0, 1.0, 0.0, true);
        lfo.retrigger();
        let samples = one_cycle(&mut lfo);

        let max = samples.iter().cloned().fold(f32::MIN, f32::max);
        let min = samples.iter().cloned().fold(f32::MAX, f32::min);
        let mean: f32 = samples.iter().sum::<f32>() / samples.len() as f32;

        assert!(max > 0.99, "sine should reach ~+1, got {max}");
        assert!(min < -0.99, "sine should reach ~-1, got {min}");
        assert!(mean.abs() < 0.01, "a full sine cycle should average ~0, got {mean}");
    }

    #[test]
    fn depth_scales_the_output() {
        let mut full = Lfo::new(48_000.0);
        full.set_params(LfoShape::Sine, 1.0, 1.0, 0.0, true);
        full.retrigger();
        let mut half = Lfo::new(48_000.0);
        half.set_params(LfoShape::Sine, 1.0, 0.5, 0.0, true);
        half.retrigger();

        let full_peak = one_cycle(&mut full)
            .iter()
            .cloned()
            .fold(0.0f32, |a, b| a.max(b.abs()));
        let half_peak = one_cycle(&mut half)
            .iter()
            .cloned()
            .fold(0.0f32, |a, b| a.max(b.abs()));

        assert!(
            (half_peak - full_peak * 0.5).abs() < 0.02,
            "depth 0.5 should halve the swing: full={full_peak}, half={half_peak}"
        );
    }

    #[test]
    fn square_is_high_then_low() {
        let mut lfo = Lfo::new(48_000.0);
        lfo.set_params(LfoShape::Square, 1.0, 1.0, 0.0, true);
        lfo.retrigger();
        // First sample is in the first half -> +1; a sample just past halfway is
        // in the second half -> -1.
        assert!(lfo.next(0.0) > 0.5, "square should start high");
        for _ in 0..(24_000 + 10) {
            lfo.next(0.0);
        }
        assert!(lfo.next(0.0) < -0.5, "square should be low in the second half");
    }

    #[test]
    fn rate_multiplier_speeds_the_lfo_up() {
        // Count rising zero crossings over one second at base rate vs +1 octave
        // of rate modulation (rate_mult = 0.25 -> 2^(4*0.25) = 2x).
        fn rising_crossings(rate_mult: f32) -> usize {
            let mut lfo = Lfo::new(48_000.0);
            lfo.set_params(LfoShape::Sine, 2.0, 1.0, 0.0, true);
            lfo.retrigger();
            let mut prev = lfo.next(rate_mult);
            let mut count = 0;
            for _ in 0..48_000 {
                let cur = lfo.next(rate_mult);
                if prev < 0.0 && cur >= 0.0 {
                    count += 1;
                }
                prev = cur;
            }
            count
        }

        let base = rising_crossings(0.0); // ~2 Hz
        let doubled = rising_crossings(0.25); // ~4 Hz
        assert!(
            doubled >= base * 2 - 1 && doubled <= base * 2 + 1,
            "rate_mult 0.25 should roughly double the rate: base={base}, doubled={doubled}"
        );
    }

    #[test]
    fn retrigger_restarts_the_cycle_at_the_offset() {
        let mut lfo = Lfo::new(48_000.0);
        // Saw reads `2·(phase+offset)-1`. With a 0.5 offset, restarting the cycle
        // (phase -> 0) makes the first read land at phase 0.5 -> saw value ~0.
        lfo.set_params(LfoShape::Saw, 1.0, 1.0, 0.5, true);
        // Advance a bunch so the phase is somewhere arbitrary.
        for _ in 0..1000 {
            lfo.next(0.0);
        }
        lfo.retrigger();
        let v = lfo.next(0.0);
        assert!(
            v.abs() < 0.05,
            "retrigger should restart the saw at the 0.5 offset (~0), got {v}"
        );
    }

    #[test]
    fn retrigger_is_repeatable_across_notes() {
        // Two consecutive retriggers must produce the same opening sample — that
        // is the whole point of per-voice key-retrigger (an identical wobble per
        // note). We use a deterministic shape (sine) so the comparison is exact.
        let mut lfo = Lfo::new(48_000.0);
        lfo.set_params(LfoShape::Sine, 3.0, 1.0, 0.0, true);
        lfo.retrigger();
        let first = lfo.next(0.0);
        for _ in 0..5000 {
            lfo.next(0.0);
        }
        lfo.retrigger();
        let second = lfo.next(0.0);
        assert!(
            (first - second).abs() < 1e-6,
            "retrigger should reproduce the same opening sample: {first} vs {second}"
        );
    }

    #[test]
    fn sample_hold_holds_between_wraps_and_changes_across_them() {
        let mut lfo = Lfo::new(48_000.0);
        // Slow rate so each plateau is long; 1 Hz -> a new step every 48k samples.
        lfo.set_params(LfoShape::SampleHold, 1.0, 1.0, 0.0, true);
        lfo.retrigger();

        let first = lfo.next(0.0);
        // Within the same cycle the value is held flat.
        for _ in 0..1000 {
            assert!(
                (lfo.next(0.0) - first).abs() < 1e-6,
                "S&H should hold its value flat within a cycle"
            );
        }
        // Cross at least one wrap; the held value should change at least once
        // across several steps (random, but not constant).
        let mut changed = false;
        let mut prev = first;
        for _ in 0..(48_000 * 6) {
            let cur = lfo.next(0.0);
            if (cur - prev).abs() > 1e-6 {
                changed = true;
            }
            prev = cur;
        }
        assert!(changed, "S&H should redraw a new value at cycle wraps");
    }

    #[test]
    fn value_mirrors_last_next() {
        let mut lfo = Lfo::new(48_000.0);
        lfo.set_params(LfoShape::Sine, 3.0, 1.0, 0.0, true);
        let v = lfo.next(0.0);
        assert_eq!(lfo.value(), v, "value() should return the last next() output");
    }
}
