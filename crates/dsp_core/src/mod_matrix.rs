//! The modulation matrix — the routing brain that connects *sources* (things
//! that move: LFOs, envelopes, velocity, the mod wheel…) to *destinations*
//! (things that get moved: pitch, cutoff, amp, pan…).
//!
//! ## The big idea
//!
//! A mod matrix is a list of **slots**. Each slot is one cable: "take SOURCE,
//! scale it by DEPTH, and add it to DESTINATION." With 16 slots you can wire 16
//! independent modulations at once — LFO 1 → pitch (vibrato), velocity → cutoff
//! (harder hits open up), the mod wheel → LFO 1 rate, and so on.
//!
//! ```text
//!     slot.dst_accumulator += source_value[slot.src] * slot.depth
//! ```
//!
//! Every slot writes into a fixed `[f32; N_DESTS]` accumulator. Several slots
//! can target the same destination — their contributions simply *sum*, which is
//! exactly how a hardware patch bay behaves (two cables into one input add).
//!
//! ## Units and scale
//!
//! The stored per-slot `depth` is always the same shape: a **bipolar fraction**
//! in `-1..+1`. Each destination then multiplies that fraction by its *own*
//! engineering-unit scale (semitones for pitch, octaves for cutoff, …) via
//! [`ModDest::scale`]. Keeping the stored depth unit-free means every slot's
//! depth control is identical; the engineering units only appear when the
//! destination is applied (and when the GUI formats the readout).
//!
//! ## Bipolar vs unipolar sources
//!
//! Each source returns its *natural* range: bipolar sources (LFOs, key-track)
//! swing `-1..+1`; unipolar sources (envelopes, velocity, mod wheel, random)
//! run `0..+1`. Because the slot depth is bipolar, a unipolar source with a
//! negative depth still *subtracts* — e.g. velocity → amp with a negative depth
//! makes harder hits quieter. That falls out of the multiply for free.
//!
//! ## RT-safety
//!
//! Everything here is `Copy` POD on fixed-size arrays — no allocation, no locks,
//! no branches beyond the per-slot `src != Off && dst != Off` skip. The whole
//! per-sample evaluation is a handful of multiplies, trivially cheap for 16
//! voices × 16 slots.

/// How many modulation **sources** exist. Used to size the per-sample source
/// value array. Keep in sync with [`ModSource`]. Wave 4 appended the 8 macros
/// (`Macro1..Macro8`) after `Random`, so this grew from 10 to 18.
pub const N_SOURCES: usize = 18;

/// How many modulation **destinations** exist. Used to size the per-sample
/// accumulator. Keep in sync with [`ModDest`]. Wave 4 appended four voicing /
/// per-voice dests (`Drift`, `UnisonWidth`, `Glide`, `SubLevel`) after
/// `DriveAmount`, so this grew from 11 to 15.
pub const N_DESTS: usize = 15;

/// The number of routing slots (matches the GUI's "x / 16 SLOTS" ribbon).
pub const N_SLOTS: usize = 16;

/// A modulation **source** — something that produces a moving value.
///
/// **Order is load-bearing**: the index of each variant is the array index into
/// the per-sample source values *and* the normalized round-trip with the
/// plugin-mirror `ModSourceParam`. New sources append at the end so indices stay
/// stable. See the module docs for bipolar vs unipolar ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModSource {
    /// Emits `0.0` always. The default for every slot — a disabled cable.
    Off,
    /// Bipolar `-1..+1` from LFO 1 (per voice).
    Lfo1,
    /// Bipolar `-1..+1` from LFO 2 (per voice).
    Lfo2,
    /// Unipolar `0..+1` from the assignable mod envelope (env 3, per voice).
    ModEnv,
    /// Unipolar `0..+1` from the per-voice filter envelope (env 2).
    FilterEnv,
    /// Unipolar `0..+1` from the amp envelope (env 1).
    AmpEnv,
    /// Unipolar `0..+1`, note-on velocity / 127, latched per voice.
    Velocity,
    /// Unipolar `0..+1`, CC1 / 127, global, smoothed.
    ModWheel,
    /// Bipolar, `(note - 60) / 48` clamped to `-1..+1` (middle C = 0), per voice.
    KeyTrack,
    /// Unipolar `0..+1`, sample-and-hold drawn once per note-on, per voice.
    Random,
    // --- Signature macros (Wave 4), appended after `Random` (index 9) so every
    // existing source index stays byte-stable for the normalized round-trip ---
    //
    // The eight `MacroN` sources are UNIPOLAR `0..1`: each carries the value of
    // one K1..K8 signature-macro knob. They are GLOBAL — one value across all
    // voices, the same on every voice and on the synth-level matrix — and are
    // smoothed at block rate exactly like `ModWheel` (the synth pushes the knob
    // value into every voice and its own copy each block via `set_macros`). A
    // negative slot depth subtracts, just as with any other unipolar source.
    /// Unipolar `0..1`, the K1 signature-macro knob value. Global, one value
    /// across all voices; smoothed at block rate like `ModWheel`.
    Macro1,
    /// Unipolar `0..1`, the K2 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro2,
    /// Unipolar `0..1`, the K3 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro3,
    /// Unipolar `0..1`, the K4 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro4,
    /// Unipolar `0..1`, the K5 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro5,
    /// Unipolar `0..1`, the K6 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro6,
    /// Unipolar `0..1`, the K7 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro7,
    /// Unipolar `0..1`, the K8 signature-macro knob value (see [`ModSource::Macro1`]).
    Macro8,
}

impl ModSource {
    /// The stable index of this source (its position in [`N_SOURCES`] arrays).
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Build a `ModSource` from a normalized index (the plugin-mirror enum's
    /// discriminant). Out-of-range maps to `Off` so a corrupt state can never
    /// index out of bounds.
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => ModSource::Lfo1,
            2 => ModSource::Lfo2,
            3 => ModSource::ModEnv,
            4 => ModSource::FilterEnv,
            5 => ModSource::AmpEnv,
            6 => ModSource::Velocity,
            7 => ModSource::ModWheel,
            8 => ModSource::KeyTrack,
            9 => ModSource::Random,
            10 => ModSource::Macro1,
            11 => ModSource::Macro2,
            12 => ModSource::Macro3,
            13 => ModSource::Macro4,
            14 => ModSource::Macro5,
            15 => ModSource::Macro6,
            16 => ModSource::Macro7,
            17 => ModSource::Macro8,
            _ => ModSource::Off,
        }
    }
}

/// A modulation **destination** — something a source can move.
///
/// **Order is load-bearing** (same reasoning as [`ModSource`]). Destinations
/// `1..=8` are *per-voice* (applied inside [`crate::Voice::render`]);
/// destinations `9..=10` are *global* bus effects (reverb send, drive) that the
/// synth computes once from global sources and the plugin reads per sample.
/// Wave 4 appends `11..=14`: `Drift`, `Glide`, and `SubLevel` are *per-voice*
/// (resolved in `Voice::render`), while `UnisonWidth` is a *voicing* dest the
/// synth reads from the macro values at note allocation — see
/// [`ModDest::is_global`] / [`ModDest::is_voicing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModDest {
    /// Slot disabled (no destination). The default.
    Off,
    /// PER-VOICE. Pitch in semitones. Full depth = ±24 semitones (±2 octaves).
    Pitch,
    /// PER-VOICE. Pulse-width offset added to each osc's duty. Full depth = ±0.45.
    OscPW,
    /// PER-VOICE. Cutoff in octaves added to the cutoff. Full depth = ±5 octaves.
    Cutoff,
    /// PER-VOICE. Resonance in knob units. Full depth = ±1.0 (clamped 0..1).
    Resonance,
    /// PER-VOICE. Linear VCA-gain offset. Full depth = ±1.0 (base 1.0, clamped ≥0).
    Amp,
    /// PER-VOICE. Stereo position `-1..+1` (L..R). Full depth = ±1.0.
    Pan,
    /// PER-VOICE. LFO 1 rate in octaves. Full depth = ±4 octaves.
    Lfo1Rate,
    /// PER-VOICE. LFO 2 rate in octaves. Full depth = ±4 octaves.
    Lfo2Rate,
    /// GLOBAL. Reverb send `0..1`. Full depth = ±1.0 (clamped 0..1).
    ReverbSend,
    /// GLOBAL. Drive amount `0..1`. Full depth = ±1.0 (clamped 0..1).
    DriveAmount,
    // --- Wave 4 voicing / per-voice dests, appended after `DriveAmount`
    // (index 10) so the existing indices stay byte-stable ---
    /// PER-VOICE. Vintage-drift amount `0..1` added to the drift knob, clamped
    /// `0..1`. Full depth = ±1.0. Applied inside [`crate::Voice::render`].
    Drift,
    /// VOICING (synth-level). Unison-width `0..1` added to the width macro,
    /// clamped `0..1`. Full depth = ±1.0. NOT per-voice and NOT a global bus
    /// effect: the synth reads the macro values directly and folds this into the
    /// effective width used at note allocation (see [`ModDest::is_voicing`]).
    UnisonWidth,
    /// PER-VOICE. Glide-time offset. Full depth = ±1.0; the bipolar accumulator
    /// fraction adds up to ±`GLIDE_MOD_SECS` (0.5 s) to the base glide time at
    /// the per-sample slew read. Applied inside [`crate::Voice::render`].
    Glide,
    /// PER-VOICE. Sub-oscillator level `0..1` added to the sub-level knob,
    /// clamped `0..1`. Full depth = ±1.0 (linear). Applied inside
    /// [`crate::Voice::render`].
    SubLevel,
}

impl ModDest {
    /// The stable index of this destination (its position in [`N_DESTS`] arrays).
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Build a `ModDest` from a normalized index. Out-of-range maps to `Off`.
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => ModDest::Pitch,
            2 => ModDest::OscPW,
            3 => ModDest::Cutoff,
            4 => ModDest::Resonance,
            5 => ModDest::Amp,
            6 => ModDest::Pan,
            7 => ModDest::Lfo1Rate,
            8 => ModDest::Lfo2Rate,
            9 => ModDest::ReverbSend,
            10 => ModDest::DriveAmount,
            11 => ModDest::Drift,
            12 => ModDest::UnisonWidth,
            13 => ModDest::Glide,
            14 => ModDest::SubLevel,
            _ => ModDest::Off,
        }
    }

    /// True for the global bus destinations (`ReverbSend`, `DriveAmount`) that
    /// are computed once on the synth from global sources, not per voice.
    ///
    /// The Wave-4 dests are deliberately NOT global: `Drift`, `Glide`, and
    /// `SubLevel` are resolved per-voice inside [`crate::Voice::render`], and
    /// `UnisonWidth` is a *voicing* dest the synth resolves from the macro values
    /// (see [`ModDest::is_voicing`]) — so none of them flow through the
    /// global reverb/drive bus.
    #[inline]
    pub fn is_global(self) -> bool {
        matches!(self, ModDest::ReverbSend | ModDest::DriveAmount)
    }

    /// True for *voicing* destinations the synth resolves at the synth level
    /// (not per voice and not on the global bus). Currently only `UnisonWidth`:
    /// width matters only at note allocation, so the synth folds the summed
    /// `Macro*->UnisonWidth` routing into its effective width directly rather
    /// than plumbing it through every voice.
    #[inline]
    pub fn is_voicing(self) -> bool {
        matches!(self, ModDest::UnisonWidth)
    }

    /// The engineering-unit scale this destination multiplies the bipolar depth
    /// fraction by. Full depth (±1.0) reaches ±`scale` in the destination's unit.
    /// See each variant's doc for the unit.
    #[inline]
    pub fn scale(self) -> f32 {
        match self {
            ModDest::Off => 0.0,
            ModDest::Pitch => 24.0,       // semitones
            ModDest::OscPW => 0.45,       // duty offset
            ModDest::Cutoff => 5.0,       // octaves
            ModDest::Resonance => 1.0,    // knob units
            ModDest::Amp => 1.0,          // linear gain offset
            ModDest::Pan => 1.0,          // L..R
            ModDest::Lfo1Rate => 4.0,     // octaves
            ModDest::Lfo2Rate => 4.0,     // octaves
            ModDest::ReverbSend => 1.0,   // send amount
            ModDest::DriveAmount => 1.0,  // drive amount
            ModDest::Drift => 1.0,        // 0..1 drift amount
            ModDest::UnisonWidth => 1.0,  // 0..1 width
            ModDest::Glide => 1.0,        // bipolar fraction; the voice scales it
                                          // by GLIDE_MOD_SECS into a time offset
            ModDest::SubLevel => 1.0,     // 0..1 linear level add
        }
    }
}

/// One routing slot: a single source → destination cable with a bipolar depth.
#[derive(Clone, Copy, Debug)]
pub struct ModSlot {
    pub src: ModSource,
    pub dst: ModDest,
    /// Bipolar routing amount in `-1..+1`. The destination's [`ModDest::scale`]
    /// turns this into engineering units.
    pub depth: f32,
}

impl Default for ModSlot {
    fn default() -> Self {
        // The regression guard: every slot defaults to a disabled cable, so a
        // fresh synth has *zero* modulation and sounds bit-identical to before.
        Self {
            src: ModSource::Off,
            dst: ModDest::Off,
            depth: 0.0,
        }
    }
}

/// The 16-slot routing table. A small `Copy` block that the synth fans out into
/// every voice each block (same pattern as the oscillator params).
#[derive(Clone, Copy, Debug)]
pub struct ModMatrix {
    pub slots: [ModSlot; N_SLOTS],
}

impl Default for ModMatrix {
    fn default() -> Self {
        Self {
            slots: [ModSlot::default(); N_SLOTS],
        }
    }
}

impl ModMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one slot's routing. `i` is `0..16`; out-of-range is ignored.
    pub fn set_slot(&mut self, i: usize, src: ModSource, dst: ModDest, depth: f32) {
        if i < N_SLOTS {
            self.slots[i] = ModSlot {
                src,
                dst,
                depth: depth.clamp(-1.0, 1.0),
            };
        }
    }

    /// Evaluate every active slot, summing `source_value × depth` into the
    /// per-destination accumulator. `sources` is the already-computed value of
    /// each source (indexed by [`ModSource::index`]); `acc` is zeroed by the
    /// caller and filled with the *raw fraction* per destination (the per-unit
    /// `scale` is applied when the destination is consumed).
    ///
    /// This is the hot inner loop: 16 iterations, one multiply-add each, no
    /// branches beyond the disabled-cable skip and no allocation.
    #[inline]
    pub fn accumulate(&self, sources: &[f32; N_SOURCES], acc: &mut [f32; N_DESTS]) {
        for slot in &self.slots {
            // A disabled cable (either end Off) contributes nothing. `Off` is
            // index 0 for both enums, so this also guards the array writes.
            if slot.src == ModSource::Off || slot.dst == ModDest::Off {
                continue;
            }
            acc[slot.dst.index()] += sources[slot.src.index()] * slot.depth;
        }
    }

    /// True if any slot is wired (used by the GUI's "x / 16 used" counter and
    /// to let the synth skip global-dest work when nothing routes there).
    pub fn any_active(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.src != ModSource::Off && s.dst != ModDest::Off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matrix_is_all_off() {
        let m = ModMatrix::new();
        assert!(!m.any_active(), "a fresh matrix must have no active routes");
        let sources = [1.0; N_SOURCES];
        let mut acc = [0.0; N_DESTS];
        m.accumulate(&sources, &mut acc);
        assert!(
            acc.iter().all(|&a| a == 0.0),
            "an all-Off matrix must accumulate zero modulation"
        );
    }

    #[test]
    fn source_and_dest_indices_match_enum_order() {
        // The array-index contract: variant position == index().
        assert_eq!(ModSource::Off.index(), 0);
        assert_eq!(ModSource::Lfo1.index(), 1);
        assert_eq!(ModSource::Random.index(), 9);
        // Wave 4: the 8 macros append after Random (9) at 10..=17.
        assert_eq!(ModSource::Macro1.index(), 10);
        assert_eq!(ModSource::Macro8.index(), 17);
        assert_eq!(N_SOURCES, 18);

        assert_eq!(ModDest::Off.index(), 0);
        assert_eq!(ModDest::Pitch.index(), 1);
        assert_eq!(ModDest::DriveAmount.index(), 10);
        // Wave 4: the 4 voicing/per-voice dests append after DriveAmount (10).
        assert_eq!(ModDest::Drift.index(), 11);
        assert_eq!(ModDest::UnisonWidth.index(), 12);
        assert_eq!(ModDest::Glide.index(), 13);
        assert_eq!(ModDest::SubLevel.index(), 14);
        assert_eq!(N_DESTS, 15);
    }

    #[test]
    fn from_index_round_trips() {
        for i in 0..N_SOURCES {
            assert_eq!(ModSource::from_index(i).index(), i);
        }
        for i in 0..N_DESTS {
            assert_eq!(ModDest::from_index(i).index(), i);
        }
        // Out-of-range is clamped to Off, never a panic.
        assert_eq!(ModSource::from_index(999), ModSource::Off);
        assert_eq!(ModDest::from_index(999), ModDest::Off);
    }

    #[test]
    fn a_single_route_sums_into_its_destination() {
        let mut m = ModMatrix::new();
        // LFO1 (source 1) -> Cutoff (dest 3) at half depth.
        m.set_slot(0, ModSource::Lfo1, ModDest::Cutoff, 0.5);
        assert!(m.any_active());

        let mut sources = [0.0; N_SOURCES];
        sources[ModSource::Lfo1.index()] = 1.0; // LFO fully up
        let mut acc = [0.0; N_DESTS];
        m.accumulate(&sources, &mut acc);

        assert_eq!(acc[ModDest::Cutoff.index()], 0.5, "1.0 * 0.5 depth = 0.5");
        // Nothing else moved.
        assert_eq!(acc[ModDest::Pitch.index()], 0.0);
    }

    #[test]
    fn multiple_slots_into_one_dest_sum() {
        let mut m = ModMatrix::new();
        m.set_slot(0, ModSource::Lfo1, ModDest::Pitch, 0.5);
        m.set_slot(1, ModSource::Lfo2, ModDest::Pitch, 0.25);

        let mut sources = [0.0; N_SOURCES];
        sources[ModSource::Lfo1.index()] = 1.0;
        sources[ModSource::Lfo2.index()] = 1.0;
        let mut acc = [0.0; N_DESTS];
        m.accumulate(&sources, &mut acc);

        assert!(
            (acc[ModDest::Pitch.index()] - 0.75).abs() < 1e-6,
            "two cables into pitch should sum: 0.5 + 0.25"
        );
    }

    #[test]
    fn negative_depth_subtracts_a_unipolar_source() {
        let mut m = ModMatrix::new();
        // Velocity (unipolar 0..1) -> Amp at negative depth: harder = quieter.
        m.set_slot(0, ModSource::Velocity, ModDest::Amp, -1.0);

        let mut sources = [0.0; N_SOURCES];
        sources[ModSource::Velocity.index()] = 1.0;
        let mut acc = [0.0; N_DESTS];
        m.accumulate(&sources, &mut acc);

        assert_eq!(acc[ModDest::Amp.index()], -1.0, "negative depth subtracts");
    }

    #[test]
    fn depth_is_clamped_to_bipolar_unit() {
        let mut m = ModMatrix::new();
        m.set_slot(0, ModSource::Lfo1, ModDest::Pitch, 5.0);
        assert_eq!(m.slots[0].depth, 1.0, "depth > 1 clamps to +1");
        m.set_slot(1, ModSource::Lfo1, ModDest::Pitch, -5.0);
        assert_eq!(m.slots[1].depth, -1.0, "depth < -1 clamps to -1");
    }

    #[test]
    fn global_dests_are_flagged() {
        assert!(ModDest::ReverbSend.is_global());
        assert!(ModDest::DriveAmount.is_global());
        assert!(!ModDest::Cutoff.is_global());
        assert!(!ModDest::Pitch.is_global());
        // Wave 4 dests are NOT global — they resolve per-voice / at the synth.
        assert!(!ModDest::Drift.is_global());
        assert!(!ModDest::UnisonWidth.is_global());
        assert!(!ModDest::Glide.is_global());
        assert!(!ModDest::SubLevel.is_global());
    }

    #[test]
    fn unison_width_is_the_only_voicing_dest() {
        assert!(ModDest::UnisonWidth.is_voicing());
        // Everything else (per-voice and global alike) is not a voicing dest.
        assert!(!ModDest::Drift.is_voicing());
        assert!(!ModDest::Glide.is_voicing());
        assert!(!ModDest::SubLevel.is_voicing());
        assert!(!ModDest::Cutoff.is_voicing());
        assert!(!ModDest::ReverbSend.is_voicing());
    }

    #[test]
    fn a_macro_source_routes_into_a_destination() {
        let mut m = ModMatrix::new();
        // Macro1 (a unipolar 0..1 source) -> Cutoff at +0.6 depth.
        m.set_slot(0, ModSource::Macro1, ModDest::Cutoff, 0.6);
        assert!(m.any_active());

        let mut sources = [0.0; N_SOURCES];
        sources[ModSource::Macro1.index()] = 1.0; // K1 fully up
        let mut acc = [0.0; N_DESTS];
        m.accumulate(&sources, &mut acc);

        assert!(
            (acc[ModDest::Cutoff.index()] - 0.6).abs() < 1e-6,
            "Macro1 (1.0) * 0.6 depth = 0.6 into Cutoff"
        );
    }

    #[test]
    fn out_of_range_slot_index_is_ignored() {
        let mut m = ModMatrix::new();
        m.set_slot(999, ModSource::Lfo1, ModDest::Pitch, 1.0);
        assert!(!m.any_active(), "an out-of-range slot index must be a no-op");
    }
}
