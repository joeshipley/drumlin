//! Tiny numeric guards shared across the DSP modules.
//!
//! These exist to keep recursive/feedback state *clean*. Two distinct hazards
//! plague any IIR/feedback structure:
//!
//! 1. **Denormals.** As a feedback tail decays toward silence the state can drift
//!    into the subnormal float range. On many CPUs/hosts arithmetic on denormals
//!    is dramatically slower, so a fading reverb or delay can spike CPU and cause
//!    crackle even though it is *almost* silent. Folding tiny magnitudes to exact
//!    `0.0` lets the tail die cleanly.
//!
//! 2. **NaN / inf latching.** A single non-finite sample injected into a feedback
//!    loop (from an extreme coefficient, a divide-by-zero, an upstream glitch) is
//!    re-read and re-written every sample. Because `NaN` propagates through every
//!    arithmetic op (and even `NaN.clamp(a, b)` returns `NaN`), the bad value is
//!    *latched forever* and spreads to any state it mixes with. The audible result
//!    is permanent silence-or-garbage that gets worse, never better.
//!
//! [`flush_denormal`] handles both: it folds non-finite values (and denormal-tiny
//! values) to `0.0`, so one bad sample is purged within a single sample instead of
//! circulating forever. Use it on **every write into recursive state**.

/// Threshold below which a magnitude is treated as denormal-tiny and folded to
/// zero. `1e-25` is comfortably above the f32 subnormal range (~1.4e-45) yet far
/// below any audible signal level, so flushing here is inaudible.
const DENORMAL_THRESHOLD: f32 = 1.0e-25;

/// Fold denormals **and** any non-finite (NaN/inf) value to `0.0`; pass every
/// normal, finite value through untouched.
///
/// This is the canonical guard for feedback state. On arm64 the denormal angle is
/// moot (the FPU flushes subnormals to zero in hardware), but the `is_finite`
/// branch is the load-bearing part everywhere: it stops a NaN/inf from latching
/// into a feedback loop forever.
///
/// Normal audio is never altered: `flush_denormal(x) == x` for any finite `x`
/// with `|x| > 1e-25`.
#[inline]
pub fn flush_denormal(x: f32) -> f32 {
    if x.is_finite() && x.abs() > DENORMAL_THRESHOLD {
        x
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_normal_values_unchanged() {
        for &x in &[1.0f32, -1.0, 0.5, -0.333, 12_345.0, -9_999.0, 1.0e-20] {
            assert_eq!(flush_denormal(x), x, "normal value {x} was altered");
        }
    }

    #[test]
    fn folds_non_finite_to_zero() {
        assert_eq!(flush_denormal(f32::NAN), 0.0);
        assert_eq!(flush_denormal(f32::INFINITY), 0.0);
        assert_eq!(flush_denormal(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn folds_denormal_tiny_to_zero() {
        // Below the threshold -> exactly zero (no lingering denormal).
        assert_eq!(flush_denormal(1.0e-30), 0.0);
        assert_eq!(flush_denormal(-1.0e-40), 0.0);
        assert_eq!(flush_denormal(f32::MIN_POSITIVE / 2.0), 0.0);
    }

    #[test]
    fn nan_clamp_pitfall_is_handled() {
        // The exact bug this guards against: NaN.clamp(a, b) returns NaN, so a
        // clamp alone does NOT stop a NaN entering a feedback buffer.
        let clamped = f32::NAN.clamp(-32.0, 32.0);
        assert!(clamped.is_nan(), "sanity: NaN.clamp returns NaN");
        // flush_denormal is what actually purges it.
        assert_eq!(flush_denormal(clamped), 0.0);
    }
}
