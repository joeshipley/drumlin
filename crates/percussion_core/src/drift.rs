//! Per-voice **analog drift** — cheap "analog wander" so no two hits land
//! identically. A per-voice DRIFT amount (0..1, on `VoiceMixRow`) scales a
//! **seeded** per-hit randomization of pitch (±cents) and level (±%). Seeded
//! (via the sequencer's `mix_seed` GROOVE-LOCK path), so a given step drifts the
//! same way every loop — reproducible, golden-friendly.
//!
//! This is the v1 subset of M6's `RandomPerHit` mod source (design §3.5,
//! "RandomPerHit → Pitch ±15c"): same per-cell seeded sample-and-hold, same
//! depth constants, same per-voice write hooks. M6 will route the same per-hit
//! randoms through the full mod matrix instead of this fixed pitch+level pair.

/// Full-drift pitch deviation, cents (design §3.5 pins ±15c for RandomPerHit→Pitch).
pub const PITCH_CENTS_FULL: f32 = 15.0;
/// Full-drift level deviation, fraction (±6% — a subtle "breathing").
pub const LEVEL_PCT_FULL: f32 = 0.06;

/// `mix_seed` purpose codes. 0/1/2 are the sequencer's probability / humanize-
/// velocity / humanize-timing draws; drift adds 3/4 (5 reserved for a future
/// per-voice micro-timing drift). 6 is M6's mod-matrix `RandomPerHit` source — a
/// SEPARATE per-cell S&H so it never perturbs the drift draws. Stable codes.
pub const PURPOSE_DRIFT_PITCH: u32 = 3;
pub const PURPOSE_DRIFT_LEVEL: u32 = 4;
/// M6 mod-matrix `RandomPerHit` source (independent of drift's 3/4).
pub const PURPOSE_MOD_RANDOM: u32 = 6;

/// Cents → frequency ratio: `2^(cents/1200)`. Exactly `1.0` at 0 cents, so a
/// drift of 0 is a bit-exact no-op multiply on a voice's pitch. A non-finite
/// `cents` (a poisoned mod/drift sum) folds to 0 → ratio `1.0`, so a NaN can
/// never reach a voice's pitch; finite inputs are unchanged (golden-safe).
#[inline]
pub fn cents_to_ratio(cents: f32) -> f32 {
    let cents = if cents.is_finite() { cents } else { 0.0 };
    2.0_f32.powf(cents / 1200.0)
}

/// Fold a computed oscillator frequency to a safe value: NaN/inf → `1.0`, else
/// clamped to `[1.0, 0.45·sr]` (sub-Nyquist, no aliasing). A finite `hz` is just
/// the clamp the voices already applied, so this is bit-exact at the goldens —
/// it only adds the non-finite guard `f32::clamp` lacks (`NaN.clamp` is `NaN`).
#[inline]
pub fn safe_hz(hz: f32, sr: f32) -> f32 {
    if hz.is_finite() {
        hz.clamp(1.0, 0.45 * sr)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_cents_is_exactly_unity() {
        assert_eq!(cents_to_ratio(0.0), 1.0, "drift 0 must be a no-op multiply");
    }

    #[test]
    fn cents_round_trip_octave() {
        assert!((cents_to_ratio(1200.0) - 2.0).abs() < 1e-4, "1200 cents = one octave");
        assert!((cents_to_ratio(-1200.0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn cents_to_ratio_folds_nonfinite() {
        // A poisoned cents sum must fold to unity (1.0), never propagate NaN/inf.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(cents_to_ratio(bad), 1.0, "non-finite cents must yield ratio 1.0");
        }
    }

    #[test]
    fn safe_hz_folds_and_clamps() {
        let sr = 48_000.0;
        // Non-finite -> 1.0 Hz.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(safe_hz(bad, sr), 1.0);
        }
        // Finite is exactly the clamp the voices already applied (golden-safe).
        assert_eq!(safe_hz(0.1, sr), 1.0, "below the floor clamps up to 1.0");
        assert_eq!(safe_hz(1.0e9, sr), 0.45 * sr, "above Nyquist margin clamps down");
        assert_eq!(safe_hz(440.0, sr), 440.0, "an in-range value is unchanged");
    }
}
