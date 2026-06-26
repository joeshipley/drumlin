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
/// velocity / humanize-timing draws; drift adds these (5 reserved for a future
/// per-voice micro-timing drift). Stable — M6's RandomPerHit reads the same cells.
pub const PURPOSE_DRIFT_PITCH: u32 = 3;
pub const PURPOSE_DRIFT_LEVEL: u32 = 4;

/// Cents → frequency ratio: `2^(cents/1200)`. Exactly `1.0` at 0 cents, so a
/// drift of 0 is a bit-exact no-op multiply on a voice's pitch.
#[inline]
pub fn cents_to_ratio(cents: f32) -> f32 {
    2.0_f32.powf(cents / 1200.0)
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
}
