//! A Juno-style stereo chorus.
//!
//! A chorus is just a *very short, modulated delay* mixed back with the dry
//! signal. As the delay time wobbles up and down (a fraction of a millisecond
//! of pitch shift), it sounds like several slightly-detuned copies playing at
//! once — the classic Juno "sheen".
//!
//! To turn our mono voice sum into a wide stereo image we read the delay line at
//! two points whose modulation LFOs are a quarter-cycle apart (in *quadrature*).
//! That phase difference between the left and right wobble is what opens up the
//! stereo field.
//!
//! ## Real-time safety
//!
//! The delay line is a fixed ring buffer allocated once in [`Chorus::new`]; the
//! audio-callback path (`process`) only reads, writes, and does float math — no
//! allocation, no locks.

use crate::util::flush_denormal;
use std::f32::consts::TAU;

/// Ring-buffer length in samples. Must comfortably hold our longest delay
/// (base + depth ≈ 11 ms) at the highest sample rate we expect. 4096 samples is
/// ~21 ms even at 192 kHz, and a power of two so we can wrap the index with a
/// cheap bitmask instead of a modulo.
const BUFFER_LEN: usize = 4096;
const BUFFER_MASK: usize = BUFFER_LEN - 1;

#[derive(Clone, Debug)]
pub struct Chorus {
    sample_rate: f32,
    buffer: Vec<f32>,
    write_pos: usize,
    /// Modulation LFO phase in `0.0..1.0`.
    lfo_phase: f32,
    rate_hz: f32,
    base_delay_ms: f32,
    depth_ms: f32,
    /// Wet/dry blend, 0.0 = all dry, 1.0 = all chorus.
    mix: f32,
}

impl Chorus {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            buffer: vec![0.0; BUFFER_LEN],
            write_pos: 0,
            lfo_phase: 0.0,
            rate_hz: 0.5,       // Juno "Chorus I" is a slow ~0.5 Hz wobble
            base_delay_ms: 7.0, // center delay
            depth_ms: 4.0,      // +/- modulation around the center
            mix: 0.5,           // an even blend of dry and chorused
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        // Clamp to >= 1 so a stray 0 (or NaN) can never divide-by-zero in
        // `read_delayed`/the LFO and seed an inf into the ring.
        self.sample_rate = sample_rate.max(1.0);
        // Clear the tail so a rate change doesn't replay stale audio.
        self.reset();
    }

    /// Zero the delay ring (and reset the write head) without touching the sample
    /// rate or parameters. Used by the plugin to flush the chorus on a scene/world
    /// switch so a stale (or non-finite) sample in the ~21 ms ring can't bleed
    /// into the new world.
    pub fn reset(&mut self) {
        for s in &mut self.buffer {
            *s = 0.0;
        }
        self.write_pos = 0;
    }

    /// Process one mono input sample, returning a stereo `(left, right)` pair.
    pub fn process(&mut self, input: f32) -> (f32, f32) {
        // Store the incoming sample at the write head, flushed so a non-finite
        // (or denormal-tiny) input can't sit in the ~21 ms ring and propagate
        // downstream for the whole buffer span.
        self.buffer[self.write_pos] = flush_denormal(input);

        // Two LFOs a quarter-cycle apart so the left and right delays move in
        // quadrature — that phase offset is what spreads the sound to stereo.
        let lfo_left = (self.lfo_phase * TAU).sin();
        let lfo_right = ((self.lfo_phase + 0.25) * TAU).sin();

        self.lfo_phase += self.rate_hz / self.sample_rate;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }

        let wet_left = self.read_delayed(self.base_delay_ms + self.depth_ms * lfo_left);
        let wet_right = self.read_delayed(self.base_delay_ms + self.depth_ms * lfo_right);

        // Advance the write head for next time.
        self.write_pos = (self.write_pos + 1) & BUFFER_MASK;

        let dry_gain = 1.0 - self.mix;
        (
            dry_gain * input + self.mix * wet_left,
            dry_gain * input + self.mix * wet_right,
        )
    }

    /// Read the buffer `delay_ms` in the past, with linear interpolation between
    /// the two nearest samples for a smooth, click-free fractional delay.
    fn read_delayed(&self, delay_ms: f32) -> f32 {
        let delay_samples = (delay_ms * 0.001 * self.sample_rate).max(1.0);
        let read = (self.write_pos as f32 - delay_samples).rem_euclid(BUFFER_LEN as f32);

        let i0 = read.floor() as usize;
        let frac = read - i0 as f32;
        let a = self.buffer[i0 & BUFFER_MASK];
        let b = self.buffer[(i0 + 1) & BUFFER_MASK];
        a + (b - a) * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_finite_and_bounded() {
        let mut chorus = Chorus::new(48_000.0);
        let mut phase = 0.0f32;
        for _ in 0..48_000 {
            let input = (phase * TAU).sin();
            phase = (phase + 200.0 / 48_000.0).fract();
            let (l, r) = chorus.process(input);
            assert!(l.is_finite() && r.is_finite());
            assert!(l.abs() < 2.0 && r.abs() < 2.0, "chorus out of range: {l}, {r}");
        }
    }

    #[test]
    fn spreads_mono_input_to_stereo() {
        let mut chorus = Chorus::new(48_000.0);
        let mut phase = 0.0f32;
        let mut max_diff = 0.0f32;
        // A steady mono tone should emerge with the two sides differing, because
        // the left/right delays are modulated out of phase.
        for _ in 0..48_000 {
            let input = (phase * TAU).sin();
            phase = (phase + 220.0 / 48_000.0).fract();
            let (l, r) = chorus.process(input);
            max_diff = max_diff.max((l - r).abs());
        }
        assert!(max_diff > 0.01, "expected stereo movement, got {max_diff}");
    }

    /// REGRESSION: a non-finite input must not sit in the ~21 ms ring and emit
    /// NaN for the whole buffer span. With the `flush_denormal` on the write, the
    /// bad sample becomes 0.0 immediately, so the output is finite from the very
    /// next read onward.
    #[test]
    fn recovers_from_injected_nan_and_inf() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut chorus = Chorus::new(48_000.0);
            // Warm up with a tone.
            let mut phase = 0.0f32;
            for _ in 0..2_000 {
                let x = (phase * TAU).sin();
                phase = (phase + 200.0 / 48_000.0).fract();
                let _ = chorus.process(x);
            }
            // Inject the poison sample.
            let _ = chorus.process(bad);
            // Every subsequent output (on silence) must be finite — the bad write
            // was already folded to 0.0, so nothing non-finite is ever read back.
            for i in 0..(BUFFER_LEN * 2) {
                let (l, r) = chorus.process(0.0);
                assert!(
                    l.is_finite() && r.is_finite(),
                    "chorus emitted non-finite after injecting {bad} at i={i}: {l},{r}"
                );
            }
        }
    }

    /// After excitation then SILENCE, the ring read-back must reach *exactly* 0.0
    /// once the buffer has cycled past the last non-zero write (mix path included).
    #[test]
    fn tail_decays_to_exactly_zero_on_silence() {
        let mut chorus = Chorus::new(48_000.0);
        let _ = chorus.process(1.0);
        let mut phase = 0.0f32;
        for _ in 0..500 {
            let x = (phase * TAU).sin();
            phase = (phase + 330.0 / 48_000.0).fract();
            let _ = chorus.process(x);
        }
        // Feed silence for well over a full buffer span so every stored sample is
        // overwritten with the flushed 0.0 input.
        let mut last = (1.0f32, 1.0f32);
        for _ in 0..(BUFFER_LEN * 2) {
            last = chorus.process(0.0);
        }
        assert_eq!(last.0, 0.0, "chorus left tail did not reach exactly 0: {}", last.0);
        assert_eq!(last.1, 0.0, "chorus right tail did not reach exactly 0: {}", last.1);
    }

    /// A zero (or non-finite) sample rate must be clamped to >= 1 so the delay
    /// math can never divide by zero and seed an inf into the ring.
    #[test]
    fn sample_rate_is_clamped() {
        let mut chorus = Chorus::new(0.0);
        // Even at a pathological rate the output stays finite.
        for _ in 0..1_000 {
            let (l, r) = chorus.process(0.5);
            assert!(l.is_finite() && r.is_finite(), "zero-rate chorus went non-finite");
        }
        chorus.set_sample_rate(f32::NAN);
        for _ in 0..1_000 {
            let (l, r) = chorus.process(0.5);
            assert!(l.is_finite() && r.is_finite(), "NaN-rate chorus went non-finite");
        }
    }
}
