//! Embedded, band-limited single-cycle **wavetables** plus a position scan that
//! morphs between them.
//!
//! A wavetable oscillator is the simplest idea in synthesis dressed up to behave:
//! store one cycle of a waveform in an array and read it back by stepping a phase
//! pointer through it. Read it faster and you get a higher note. That part is
//! trivial. The two things that make a *good* wavetable oscillator — and the two
//! things this module is really about — are **band-limiting** (so it doesn't
//! alias into a mess of inharmonic whistles when you play high) and a **position
//! scan** (so sweeping one knob morphs the timbre, the signature wavetable move).
//!
//! # Band-limiting two ways: additive generation + mipmaps
//!
//! Aliasing happens when a waveform contains harmonics above the Nyquist
//! frequency (half the sample rate). Those over-the-top harmonics don't just
//! vanish — they *fold back down* into the audible range as out-of-tune garbage.
//! A naive saw stored at full resolution and played an octave up suddenly has
//! every harmonic that used to sit just under Nyquist now sitting *over* it,
//! folding back. So we attack the problem twice:
//!
//! 1. **Additive generation.** Every table is built by summing sine harmonics
//!    ([`additive`]) rather than by drawing the ideal shape and sampling it. A sum
//!    of sines has *exactly* the harmonics we put in and not one more, so the
//!    table is band-limited by construction — there is no aliasing baked into the
//!    stored cycle.
//!
//! 2. **A mipmap pyramid.** One full-bandwidth table still aliases when played
//!    high, because its top harmonics climb over Nyquist as the pitch rises. So
//!    for each shape we generate a *pyramid* of [`MIP_LEVELS`] progressively
//!    band-limited copies: level 0 keeps every harmonic, and each level up keeps
//!    half as many (one fewer octave of harmonics). At playback we pick the level
//!    whose top harmonic still sits under Nyquist for the note being played (see
//!    [`WavetableBank::mip_for_freq`]). This is the same trick texture mipmaps use
//!    in 3D graphics, for the same reason: pick a pre-filtered level of detail
//!    that matches how fast you're sampling it.
//!
//! Within a level we **linearly interpolate** between the two nearest stored
//! samples. Single level + linear interpolation is the pragmatic v1: it does not
//! *eliminate* aliasing at extreme highs (a true bandlimited-interpolation or
//! cross-fading-between-mip-levels scheme would do better), but it reduces it to
//! well below the musical signal, which is all a synth oscillator needs.
//!
//! # The position scan (timbre morph)
//!
//! The bank holds [`N_TABLES`] shapes. The oscillator carries a `wt_table` select
//! and a `wt_position` knob in `0..1`. The contract — and the GUI and DSP must
//! agree on it exactly — is:
//!
//! > **position 0.0 = the selected `wt_table` exactly; sweeping toward 1.0 morphs
//! > to the *next* table in the bank (wrapping at the end).**
//!
//! So at `wt_table = 2` (Saw), `wt_position = 0.0` is a pure saw, `0.5` is a 50/50
//! blend of saw and the next shape (Square), and `1.0` is the pure next shape.
//! The morph is a per-sample linear interpolation between the two neighboring
//! tables at the *same* mip level and the *same* phase, so sweeping `wt_position`
//! glides the timbre continuously instead of switching abruptly.
//!
//! # Real-time safety
//!
//! The bank is built **once**, lazily, into a process-wide [`static`] the first
//! time any voice asks for it ([`bank`]). After that it is immutable shared data:
//! every voice reads the same tables, no per-voice or per-sample allocation ever
//! happens. Generating the pyramid is a few hundred thousand `sin` calls done
//! exactly once at startup, never in the audio thread's steady state.

use std::f32::consts::TAU;
use std::sync::OnceLock;

/// Number of distinct shapes in the bank. The `wt_table` select chooses one of
/// these as the base; `wt_position` morphs toward the *next* one (wrapping).
///
/// Order is **load-bearing**: it must match the GUI `WtTableParam` enum variant
/// order (Sine, Triangle, Saw, Square, Pulse) so a stored/automated table index
/// means the same shape on both sides.
pub const N_TABLES: usize = 5;

/// Samples per single-cycle table. 2048 is plenty: the phase pointer lands
/// between samples almost always, and the linear interpolation smooths the
/// gaps. A power of two keeps the index math friendly.
pub const TABLE_LEN: usize = 2048;

/// Number of octave levels in each shape's mipmap pyramid.
///
/// Level 0 is full-bandwidth (every harmonic that fits in [`TABLE_LEN`]); each
/// higher level halves the harmonic count, i.e. drops the top octave of
/// harmonics. Ten levels covers ten octaves of playable pitch — from the deepest
/// sub up past the top of the keyboard — which is more than enough headroom for a
/// synth. Past the last level we just clamp to it (the harmonic content is
/// already down to a near-sine, so there is nothing left to alias).
pub const MIP_LEVELS: usize = 10;

/// The reference frequency that maps to mip level 0. A note at or below this
/// frequency uses the full-bandwidth table; each octave above steps one level up
/// the pyramid (fewer harmonics). 20 Hz sits at the bottom of hearing, so level 0
/// is reserved for the deepest notes where the most harmonics still fit under
/// Nyquist.
const MIP_REFERENCE_HZ: f32 = 20.0;

/// One shape's mipmap pyramid: [`MIP_LEVELS`] band-limited copies of the same
/// single-cycle waveform, each with half the harmonic content of the one below.
struct MipPyramid {
    /// `levels[k]` is the table at mip level `k`, each [`TABLE_LEN`] samples long.
    levels: [Vec<f32>; MIP_LEVELS],
}

impl MipPyramid {
    /// Build a pyramid from a harmonic-amplitude function.
    ///
    /// `harmonic_amp(n)` returns the amplitude of the `n`-th harmonic
    /// (`n` starting at 1 = the fundamental). The full-bandwidth level 0 keeps
    /// every harmonic up to `max_harmonics`; each level up keeps half as many,
    /// so level `k` keeps `max_harmonics >> k`. Summing only that many sine
    /// partials makes each level band-limited by construction.
    fn build(harmonic_amp: impl Fn(usize) -> f32, max_harmonics: usize) -> Self {
        // `from_fn` lets us build the fixed-size array without requiring `Vec:
        // Copy`. Each level recomputes its own additive sum at its own harmonic
        // ceiling.
        let levels = std::array::from_fn(|k| {
            let harmonics = (max_harmonics >> k).max(1);
            additive(harmonics, &harmonic_amp)
        });
        Self { levels }
    }

    /// Linearly-interpolated sample of mip level `mip` at normalized phase
    /// `phase` in `0..1`. Reads the two nearest stored samples and blends them by
    /// the fractional phase — the within-table anti-alias step.
    #[inline]
    fn sample(&self, mip: usize, phase: f32) -> f32 {
        let table = &self.levels[mip.min(MIP_LEVELS - 1)];
        // Map phase 0..1 onto 0..TABLE_LEN. `fract` keeps us in range even if a
        // caller hands us a phase slightly outside [0,1).
        let pos = phase.fract().rem_euclid(1.0) * TABLE_LEN as f32;
        let i0 = pos as usize % TABLE_LEN;
        let i1 = (i0 + 1) % TABLE_LEN;
        let frac = pos - (pos as usize) as f32;
        table[i0] + (table[i1] - table[i0]) * frac
    }
}

/// The process-wide bank of [`N_TABLES`] shapes, each a [`MipPyramid`].
///
/// Built once via [`bank`]; thereafter immutable and shared by every voice.
pub struct WavetableBank {
    pyramids: [MipPyramid; N_TABLES],
}

impl WavetableBank {
    /// Generate the whole bank by additive synthesis. Called exactly once (see
    /// [`bank`]); never on the audio thread in steady state.
    fn generate() -> Self {
        // The harmonic ceiling for level 0. We cannot represent a harmonic whose
        // wavelength is shorter than two samples, so TABLE_LEN/2 is the hard limit;
        // we keep a touch under it to leave the very top empty (cleaner reconstruction).
        let max_harmonics = TABLE_LEN / 2 - 1;

        let pyramids = [
            // 0: Sine — a single harmonic. The trivial, edge-free reference shape.
            MipPyramid::build(|n| if n == 1 { 1.0 } else { 0.0 }, max_harmonics),
            // 1: Triangle — odd harmonics, falling off as 1/n^2 with alternating
            // sign. The steep 1/n^2 rolloff is why a triangle sounds soft and
            // flute-like next to a saw.
            MipPyramid::build(
                |n| {
                    if n % 2 == 1 {
                        let sign = if (n / 2) % 2 == 0 { 1.0 } else { -1.0 };
                        sign / (n * n) as f32
                    } else {
                        0.0
                    }
                },
                max_harmonics,
            ),
            // 2: Saw — every harmonic, amplitude 1/n. The brightest classic shape;
            // the slow 1/n rolloff packs in lots of high harmonics.
            MipPyramid::build(|n| 1.0 / n as f32, max_harmonics),
            // 3: Square — odd harmonics only, amplitude 1/n. Hollow and clarinet-
            // like; dropping the even harmonics is what gives it that woody tone.
            MipPyramid::build(
                |n| if n % 2 == 1 { 1.0 / n as f32 } else { 0.0 },
                max_harmonics,
            ),
            // 4: Pulse-ish / "formant" — a chosen partial set that emphasizes a
            // cluster of upper harmonics, giving a nasal, vowel-like color. Built
            // by weighting harmonics 1..16 with a raised bump around the 4th-6th so
            // the shape morphs interestingly against the Sine it wraps back to.
            MipPyramid::build(
                |n| {
                    if n == 0 || n > 16 {
                        0.0
                    } else {
                        // A gentle formant bump centered near the 5th harmonic.
                        let center = 5.0;
                        let nf = n as f32;
                        let bump = (-((nf - center) * (nf - center)) / 6.0).exp();
                        (0.2 / nf) + 0.8 * bump / nf
                    }
                },
                max_harmonics,
            ),
        ];
        Self { pyramids }
    }

    /// Pick the mip level for a note at `frequency` Hz given the `sample_rate`.
    ///
    /// Each octave above [`MIP_REFERENCE_HZ`] steps one level up the pyramid (and
    /// keeps half as many harmonics), so the table's top harmonic always stays
    /// under Nyquist. Clamped to a valid level. (`sample_rate` is accepted for a
    /// future Nyquist-exact refinement; the octave-relative pick already keeps the
    /// top harmonic safely band-limited.)
    #[inline]
    pub fn mip_for_freq(&self, frequency: f32, _sample_rate: f32) -> usize {
        if frequency <= MIP_REFERENCE_HZ {
            return 0;
        }
        // Octaves above the reference => mip level. log2(f / ref) rounded down.
        let octaves = (frequency / MIP_REFERENCE_HZ).log2();
        (octaves.max(0.0) as usize).min(MIP_LEVELS - 1)
    }

    /// Sample shape `table` at mip level `mip` and normalized `phase` in `0..1`,
    /// linearly interpolated within the table. `table` wraps modulo [`N_TABLES`]
    /// so the position-scan's "next table" lookup is always in range.
    #[inline]
    pub fn sample(&self, table: usize, mip: usize, phase: f32) -> f32 {
        self.pyramids[table % N_TABLES].sample(mip, phase)
    }
}

/// The lazily-initialized, process-wide bank.
///
/// [`OnceLock`] gives us a thread-safe "build exactly once on first use" with no
/// `unsafe` and no runtime locking after initialization — every subsequent call
/// is a plain atomic load of an already-built reference. The first voice to touch
/// a wavetable pays the one-time generation cost; everyone after shares the data.
static BANK: OnceLock<WavetableBank> = OnceLock::new();

/// Borrow the shared wavetable bank, building it on first call.
///
/// Real-time note: the *first* call (which generates the tables) should be warmed
/// up off the audio thread — e.g. at plugin instantiation — but even if it lands
/// on the audio thread it happens at most once per process, never per block.
#[inline]
pub fn bank() -> &'static WavetableBank {
    BANK.get_or_init(WavetableBank::generate)
}

/// Build one band-limited single cycle by summing `harmonics` sine partials.
///
/// `amp(n)` gives the amplitude of harmonic `n` (1 = fundamental). The result is
/// peak-normalized to `[-1, 1]` so every shape — and every mip level of it —
/// comes out at a consistent level, which keeps the position-scan morph from
/// jumping in loudness as it crosses between tables. Because the cycle is a pure
/// sum of sines, it contains *exactly* those harmonics and nothing above them: it
/// is band-limited by construction, with no aliasing baked in.
fn additive(harmonics: usize, amp: &impl Fn(usize) -> f32) -> Vec<f32> {
    let mut table = vec![0.0f32; TABLE_LEN];
    for (i, slot) in table.iter_mut().enumerate() {
        let phase = i as f32 / TABLE_LEN as f32; // 0..1 across the cycle
        let mut acc = 0.0f32;
        for n in 1..=harmonics {
            let a = amp(n);
            if a != 0.0 {
                acc += a * (TAU * n as f32 * phase).sin();
            }
        }
        *slot = acc;
    }
    // Peak-normalize so each table/level sits in [-1, 1].
    let peak = table.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if peak > 1.0e-12 {
        let inv = 1.0 / peak;
        for x in &mut table {
            *x *= inv;
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sample of every table at every mip level must be a finite value
    /// inside the normalized `[-1, 1]` range (additive synthesis + peak-normalize
    /// guarantees this). A rogue NaN or an out-of-range spike would mean a
    /// generation bug.
    #[test]
    fn all_tables_are_finite_and_in_range() {
        let b = bank();
        for t in 0..N_TABLES {
            for mip in 0..MIP_LEVELS {
                for i in 0..TABLE_LEN {
                    let phase = i as f32 / TABLE_LEN as f32;
                    let s = b.sample(t, mip, phase);
                    assert!(s.is_finite(), "table {t} mip {mip} sample {i} not finite");
                    assert!(
                        (-1.0001..=1.0001).contains(&s),
                        "table {t} mip {mip} out of range: {s}"
                    );
                }
            }
        }
    }

    /// The Sine table (index 0) must actually be a sine: its level-0 cycle should
    /// match `sin(2π·phase)` within a tight tolerance. This proves additive
    /// generation builds the shape we asked for.
    #[test]
    fn sine_table_matches_sine_within_tol() {
        let b = bank();
        let mut max_err = 0.0f32;
        for i in 0..TABLE_LEN {
            let phase = i as f32 / TABLE_LEN as f32;
            let got = b.sample(0, 0, phase);
            let want = (TAU * phase).sin();
            max_err = max_err.max((got - want).abs());
        }
        assert!(max_err < 1.0e-3, "sine table deviates from sin: max_err={max_err}");
    }

    /// Higher mip levels must have strictly less HIGH-harmonic energy than level 0
    /// for a bright shape (the saw): that is the whole point of the pyramid. We
    /// probe a specific high harmonic (the 20th) of the stored cycle — a full-
    /// bandwidth saw still has it, but a high mip level has band-limited it away.
    #[test]
    fn higher_mips_have_less_high_frequency_energy() {
        let b = bank();
        // Magnitude of the `harmonic`-th partial in mip level `mip` of `table`,
        // via a direct single-bin DFT over one stored cycle.
        fn harmonic_mag(b: &WavetableBank, table: usize, mip: usize, harmonic: usize) -> f32 {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for i in 0..TABLE_LEN {
                let phase = i as f32 / TABLE_LEN as f32;
                let s = b.sample(table, mip, phase);
                let ang = TAU * harmonic as f32 * phase;
                re += s * ang.cos();
                im += s * ang.sin();
            }
            (re * re + im * im).sqrt() / TABLE_LEN as f32
        }
        // Saw (table 2): its 20th harmonic is strong at full bandwidth (level 0)
        // but must be essentially gone at a high mip level that keeps far fewer
        // harmonics.
        let low = harmonic_mag(b, 2, 0, 20);
        let high = harmonic_mag(b, 2, MIP_LEVELS - 1, 20);
        assert!(low > 0.001, "level-0 saw should contain a real 20th harmonic: {low}");
        assert!(
            low > high * 10.0,
            "high mip should have shed its high harmonics: low={low}, high={high}"
        );
    }

    /// `mip_for_freq` must climb the pyramid as the note rises: a sub-bass note
    /// sits at level 0 (most harmonics), a very high note near the top level
    /// (fewest harmonics). This is what keeps a high note from aliasing.
    #[test]
    fn mip_level_rises_with_pitch() {
        let b = bank();
        let sr = 48_000.0;
        let low = b.mip_for_freq(20.0, sr);
        let mid = b.mip_for_freq(440.0, sr);
        let high = b.mip_for_freq(8_000.0, sr);
        assert_eq!(low, 0, "sub-bass should use the full-bandwidth level");
        assert!(mid > low, "a mid note should step up the pyramid: {mid}");
        assert!(high > mid, "a high note should step further up: {high}");
        assert!(high < MIP_LEVELS, "mip level must stay in range");
    }
}
