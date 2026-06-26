//! The per-voice **modulation matrix** (design §3.5) — Drumlin's M6 routing
//! table. A 16-slot list of `source → destination` cables, each with a bipolar
//! depth and a `target_voice` so one slot can move "LFO1 → Hat Decay" or
//! "Velocity → Kick Pitch" or "Accent → all-voices Cutoff".
//!
//! This mirrors synth_core's `ModMatrix` mechanism (the ~8-line `accumulate`
//! loop, the Off-skip, the load-bearing index order, the `default = all-Off`
//! regression guard) but defines Drumlin's OWN source/dest vocabulary and adds
//! per-voice scoping — instrument-specific, so it lives here, not in the shared
//! crate. The reusable value generators (`Lfo`, `Adsr`) come straight from
//! synth_core; synth_core itself is untouched.
//!
//! **Evaluation is per-hit, not per-sample.** Drum voices are one-shots
//! configured at note-on, so the matrix is evaluated ONCE per trigger for the
//! one voice being hit — a single stack accumulator, `target_voice` deciding
//! which hit a slot reaches. The hot loop allocates nothing.
//!
//! **Order is load-bearing** for both enums: a variant's position is its index
//! into the source/dest arrays *and* the persisted id ↔ index round-trip. New
//! variants append at the end so saved matrices stay valid; a reconcile test
//! pins index ↔ id-string. A fresh matrix is all-Off, so it accumulates exactly
//! zero modulation and every golden render stays byte-identical.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Routing slots (design §3.5: the 16-slot table, as Esker).
pub const N_DRUM_SLOTS: usize = 16;

/// `target_voice` sentinel: a slot with this value modulates every voice.
pub const ALL_VOICES: u8 = 0xFF;

/// A modulation **source** — something that produces a value each hit.
///
/// Per-hit sources (Velocity/Accent/Trigger/RandomPerHit/BarPhase/StepPosition)
/// are latched at note-on; the LFOs + mod-env advance per block and are sampled
/// at the hit (sample-and-hold); macros + mod-wheel are global block-rate knobs.
/// `PitchEnv`/`AmpEnv` are reserved (their indices are pinned now) but not yet
/// produced as sources — they arrive with per-sample modulation in a later wave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum DrumModSource {
    /// Emits `0.0` always — a disabled cable. The default for every slot.
    Off,
    /// Unipolar `0..1`, this hit's velocity.
    Velocity,
    /// Unipolar `0..1`, this hit's accent (0 or 1) — the 808/909 accent rail.
    Accent,
    /// Bipolar `-1..+1`, LFO 1 sampled at the hit.
    Lfo1,
    /// Bipolar `-1..+1`, LFO 2 sampled at the hit.
    Lfo2,
    /// Unipolar `0..1`, the global mod-envelope sampled at the hit.
    ModEnv,
    /// RESERVED (not yet produced): the voice's pitch envelope as a source.
    PitchEnv,
    /// RESERVED (not yet produced): the voice's amp envelope as a source.
    AmpEnv,
    /// Unipolar `1.0` at the hit it fires (an accent-independent "this hit
    /// happened" gate), `0.0` otherwise.
    Trigger,
    /// Bipolar `-1..+1`, a fresh seeded sample-and-hold per hit (reuses the
    /// drift S&H mechanism via a dedicated `mix_seed` purpose).
    RandomPerHit,
    /// Unipolar `0..1`, the hit's position within the bar (a filter-opens-across-
    /// the-bar staple).
    BarPhase,
    /// Unipolar `0..1`, the hit's step index within its track length.
    StepPosition,
    /// Unipolar `0..1`, CC1 / 127, global, block-rate.
    ModWheel,
    /// Unipolar `0..1`, the K1 macro knob. Global, block-rate.
    Macro1,
    /// Unipolar `0..1`, the K2 macro knob (see [`DrumModSource::Macro1`]).
    Macro2,
    /// Unipolar `0..1`, the K3 macro knob.
    Macro3,
    /// Unipolar `0..1`, the K4 macro knob.
    Macro4,
    /// Unipolar `0..1`, the K5 macro knob.
    Macro5,
    /// Unipolar `0..1`, the K6 macro knob.
    Macro6,
    /// Unipolar `0..1`, the K7 macro knob.
    Macro7,
    /// Unipolar `0..1`, the K8 macro knob.
    Macro8,
}

/// The number of sources (the source-value array length).
pub const N_DRUM_SOURCES: usize = 21;

impl DrumModSource {
    /// The stable index of this source (its position in the source array).
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Build a source from an index. Out-of-range maps to `Off` so a corrupt
    /// saved slot can never index out of bounds.
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => DrumModSource::Velocity,
            2 => DrumModSource::Accent,
            3 => DrumModSource::Lfo1,
            4 => DrumModSource::Lfo2,
            5 => DrumModSource::ModEnv,
            6 => DrumModSource::PitchEnv,
            7 => DrumModSource::AmpEnv,
            8 => DrumModSource::Trigger,
            9 => DrumModSource::RandomPerHit,
            10 => DrumModSource::BarPhase,
            11 => DrumModSource::StepPosition,
            12 => DrumModSource::ModWheel,
            13 => DrumModSource::Macro1,
            14 => DrumModSource::Macro2,
            15 => DrumModSource::Macro3,
            16 => DrumModSource::Macro4,
            17 => DrumModSource::Macro5,
            18 => DrumModSource::Macro6,
            19 => DrumModSource::Macro7,
            20 => DrumModSource::Macro8,
            _ => DrumModSource::Off,
        }
    }

    /// Stable id string (the GUI / preset encoding).
    pub fn id(self) -> &'static str {
        match self {
            DrumModSource::Off => "off",
            DrumModSource::Velocity => "velocity",
            DrumModSource::Accent => "accent",
            DrumModSource::Lfo1 => "lfo1",
            DrumModSource::Lfo2 => "lfo2",
            DrumModSource::ModEnv => "modenv",
            DrumModSource::PitchEnv => "pitchenv",
            DrumModSource::AmpEnv => "ampenv",
            DrumModSource::Trigger => "trigger",
            DrumModSource::RandomPerHit => "random",
            DrumModSource::BarPhase => "barphase",
            DrumModSource::StepPosition => "steppos",
            DrumModSource::ModWheel => "modwheel",
            DrumModSource::Macro1 => "macro1",
            DrumModSource::Macro2 => "macro2",
            DrumModSource::Macro3 => "macro3",
            DrumModSource::Macro4 => "macro4",
            DrumModSource::Macro5 => "macro5",
            DrumModSource::Macro6 => "macro6",
            DrumModSource::Macro7 => "macro7",
            DrumModSource::Macro8 => "macro8",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "velocity" => DrumModSource::Velocity,
            "accent" => DrumModSource::Accent,
            "lfo1" => DrumModSource::Lfo1,
            "lfo2" => DrumModSource::Lfo2,
            "modenv" => DrumModSource::ModEnv,
            "pitchenv" => DrumModSource::PitchEnv,
            "ampenv" => DrumModSource::AmpEnv,
            "trigger" => DrumModSource::Trigger,
            "random" => DrumModSource::RandomPerHit,
            "barphase" => DrumModSource::BarPhase,
            "steppos" => DrumModSource::StepPosition,
            "modwheel" => DrumModSource::ModWheel,
            "macro1" => DrumModSource::Macro1,
            "macro2" => DrumModSource::Macro2,
            "macro3" => DrumModSource::Macro3,
            "macro4" => DrumModSource::Macro4,
            "macro5" => DrumModSource::Macro5,
            "macro6" => DrumModSource::Macro6,
            "macro7" => DrumModSource::Macro7,
            "macro8" => DrumModSource::Macro8,
            _ => DrumModSource::Off,
        }
    }
}

/// A modulation **destination** — a per-voice parameter a source can move.
///
/// `Pitch`/`Cutoff`/`Resonance`/`Drive`/`Level`/`Pan` map onto setters that
/// already exist (the drift pitch hook + the `VoiceTail` chain); the rest need
/// new per-block mod inputs on the engines. `SampleStart`/`LayerMix` are
/// reserved (no sampler/layer voice exists yet) — their indices are pinned but
/// they are no-ops in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum DrumModDest {
    /// Slot disabled (no destination). The default.
    Off,
    /// Pitch, in cents (summed into the same hook drift + a Pitch p-lock use).
    Pitch,
    /// Pitch-envelope depth (fraction of the engine's pitch sweep).
    PitchEnvDepth,
    /// Filter cutoff, in octaves added to the tail cutoff.
    Cutoff,
    /// Filter resonance (knob units, clamped 0..1).
    Resonance,
    /// Drive amount (0..1).
    Drive,
    /// Output level — a clean VCA offset on the tail (linear gain).
    Level,
    /// Noise-layer level (engines with a noise component).
    NoiseLevel,
    /// Tone-layer level (engines with a tonal component).
    ToneLevel,
    /// Amp decay — a multiplicative scale around the baked decay.
    AmpDecay,
    /// Stereo position (`-1..+1`, L..R).
    Pan,
    /// RESERVED (no-op in v1): sample start offset.
    SampleStart,
    /// RESERVED (no-op in v1): layer mix.
    LayerMix,
}

/// The number of destinations (the dest-accumulator array length).
pub const N_DRUM_DESTS: usize = 13;

impl DrumModDest {
    /// The stable index of this destination (its position in the accumulator).
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Build a dest from an index. Out-of-range maps to `Off`.
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => DrumModDest::Pitch,
            2 => DrumModDest::PitchEnvDepth,
            3 => DrumModDest::Cutoff,
            4 => DrumModDest::Resonance,
            5 => DrumModDest::Drive,
            6 => DrumModDest::Level,
            7 => DrumModDest::NoiseLevel,
            8 => DrumModDest::ToneLevel,
            9 => DrumModDest::AmpDecay,
            10 => DrumModDest::Pan,
            11 => DrumModDest::SampleStart,
            12 => DrumModDest::LayerMix,
            _ => DrumModDest::Off,
        }
    }

    /// Stable id string (the GUI / preset encoding).
    pub fn id(self) -> &'static str {
        match self {
            DrumModDest::Off => "off",
            DrumModDest::Pitch => "pitch",
            DrumModDest::PitchEnvDepth => "pitchenvdepth",
            DrumModDest::Cutoff => "cutoff",
            DrumModDest::Resonance => "resonance",
            DrumModDest::Drive => "drive",
            DrumModDest::Level => "level",
            DrumModDest::NoiseLevel => "noiselevel",
            DrumModDest::ToneLevel => "tonelevel",
            DrumModDest::AmpDecay => "ampdecay",
            DrumModDest::Pan => "pan",
            DrumModDest::SampleStart => "samplestart",
            DrumModDest::LayerMix => "layermix",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "pitch" => DrumModDest::Pitch,
            "pitchenvdepth" => DrumModDest::PitchEnvDepth,
            "cutoff" => DrumModDest::Cutoff,
            "resonance" => DrumModDest::Resonance,
            "drive" => DrumModDest::Drive,
            "level" => DrumModDest::Level,
            "noiselevel" => DrumModDest::NoiseLevel,
            "tonelevel" => DrumModDest::ToneLevel,
            "ampdecay" => DrumModDest::AmpDecay,
            "pan" => DrumModDest::Pan,
            "samplestart" => DrumModDest::SampleStart,
            "layermix" => DrumModDest::LayerMix,
            _ => DrumModDest::Off,
        }
    }

    /// The engineering-unit scale a full-depth (`±1.0`) route reaches in this
    /// destination's unit. The accumulator holds the raw fraction; the apply
    /// path multiplies by `scale()` when it consumes the dest.
    #[inline]
    pub fn scale(self) -> f32 {
        match self {
            DrumModDest::Off => 0.0,
            DrumModDest::Pitch => 1200.0,    // cents (±1 octave at full depth)
            DrumModDest::PitchEnvDepth => 1.0,
            DrumModDest::Cutoff => 5.0,      // octaves
            DrumModDest::Resonance => 1.0,   // knob units
            DrumModDest::Drive => 1.0,       // 0..1
            DrumModDest::Level => 1.0,       // linear gain offset
            DrumModDest::NoiseLevel => 1.0,
            DrumModDest::ToneLevel => 1.0,
            DrumModDest::AmpDecay => 1.0,    // fraction; the apply maps to a scale
            DrumModDest::Pan => 1.0,         // L..R
            DrumModDest::SampleStart => 1.0, // reserved
            DrumModDest::LayerMix => 1.0,    // reserved
        }
    }
}

/// One routing slot: a `source → destination` cable with a bipolar depth and a
/// per-voice target (`ALL_VOICES` = every voice, else a track index 0..11).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DrumModSlot {
    pub src: DrumModSource,
    pub dst: DrumModDest,
    /// Bipolar routing amount `-1..+1`; [`DrumModDest::scale`] turns it into units.
    pub depth: f32,
    /// Which voice this slot reaches: `ALL_VOICES` or a track index 0..11.
    pub target_voice: u8,
}

impl Default for DrumModSlot {
    fn default() -> Self {
        // The regression guard: a fresh slot is a disabled cable, so a fresh kit
        // has zero modulation and renders bit-identically to before M6.
        Self {
            src: DrumModSource::Off,
            dst: DrumModDest::Off,
            depth: 0.0,
            target_voice: ALL_VOICES,
        }
    }
}

/// The 16-slot routing table. A small `Copy` block the kit holds and the plugin
/// fans in block-rate (same pattern as the voice patch / mix).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DrumModMatrix {
    pub slots: [DrumModSlot; N_DRUM_SLOTS],
}

impl Default for DrumModMatrix {
    fn default() -> Self {
        Self { slots: [DrumModSlot::default(); N_DRUM_SLOTS] }
    }
}

impl DrumModMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one slot's routing. `i` is `0..16`; out-of-range is ignored. Depth is
    /// clamped to `-1..+1`; `target_voice` is `ALL_VOICES` or a track index.
    pub fn set_slot(&mut self, i: usize, src: DrumModSource, dst: DrumModDest, depth: f32, target_voice: u8) {
        if i < N_DRUM_SLOTS {
            self.slots[i] = DrumModSlot { src, dst, depth: depth.clamp(-1.0, 1.0), target_voice };
        }
    }

    /// Evaluate every active slot that reaches `track`, summing
    /// `source_value × depth` into the per-destination accumulator. `sources` is
    /// the already-computed value of each source (indexed by
    /// [`DrumModSource::index`]); `acc` is zeroed by the caller and filled with
    /// the raw fraction per destination (the per-unit `scale` is applied when the
    /// destination is consumed).
    ///
    /// The hot per-hit loop: 16 iterations, an Off-skip and a `target_voice`
    /// check, one multiply-add each, no allocation.
    #[inline]
    pub fn accumulate(&self, track: usize, sources: &[f32; N_DRUM_SOURCES], acc: &mut [f32; N_DRUM_DESTS]) {
        for slot in &self.slots {
            // A disabled cable (either end Off) contributes nothing; `Off` is
            // index 0 for both enums, so this also guards the array writes.
            if slot.src == DrumModSource::Off || slot.dst == DrumModDest::Off {
                continue;
            }
            // Per-voice scoping: a slot reaches all voices or just one track.
            if slot.target_voice != ALL_VOICES && slot.target_voice as usize != track {
                continue;
            }
            acc[slot.dst.index()] += sources[slot.src.index()] * slot.depth;
        }
    }

    /// True if any slot is wired (the GUI's "x / 16 used" counter and the kit's
    /// gate to skip per-hit mod work entirely when nothing routes).
    pub fn any_active(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.src != DrumModSource::Off && s.dst != DrumModDest::Off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matrix_is_all_off() {
        let m = DrumModMatrix::new();
        assert!(!m.any_active(), "a fresh matrix must have no active routes");
        let sources = [1.0; N_DRUM_SOURCES];
        let mut acc = [0.0; N_DRUM_DESTS];
        m.accumulate(0, &sources, &mut acc);
        assert!(acc.iter().all(|&a| a == 0.0), "an all-Off matrix must accumulate zero modulation");
    }

    #[test]
    fn source_and_dest_indices_match_enum_order() {
        // The array-index contract: variant position == index().
        assert_eq!(DrumModSource::Off.index(), 0);
        assert_eq!(DrumModSource::Velocity.index(), 1);
        assert_eq!(DrumModSource::RandomPerHit.index(), 9);
        assert_eq!(DrumModSource::Macro1.index(), 13);
        assert_eq!(DrumModSource::Macro8.index(), 20);
        assert_eq!(N_DRUM_SOURCES, 21);

        assert_eq!(DrumModDest::Off.index(), 0);
        assert_eq!(DrumModDest::Pitch.index(), 1);
        assert_eq!(DrumModDest::Level.index(), 6);
        assert_eq!(DrumModDest::Pan.index(), 10);
        assert_eq!(DrumModDest::LayerMix.index(), 12);
        assert_eq!(N_DRUM_DESTS, 13);
    }

    #[test]
    fn from_index_round_trips() {
        for i in 0..N_DRUM_SOURCES {
            assert_eq!(DrumModSource::from_index(i).index(), i);
        }
        for i in 0..N_DRUM_DESTS {
            assert_eq!(DrumModDest::from_index(i).index(), i);
        }
        // Out-of-range clamps to Off, never a panic / OOB.
        assert_eq!(DrumModSource::from_index(999), DrumModSource::Off);
        assert_eq!(DrumModDest::from_index(999), DrumModDest::Off);
    }

    #[test]
    fn id_pins_are_literal_and_stable() {
        // Explicit literal pins (not derived from the match) so a reorder/rename
        // — which would silently corrupt a saved matrix — is caught.
        assert_eq!(DrumModSource::from_index(0).id(), "off");
        assert_eq!(DrumModSource::from_index(1).id(), "velocity");
        assert_eq!(DrumModSource::from_index(9).id(), "random");
        assert_eq!(DrumModSource::from_index(13).id(), "macro1");
        assert_eq!(DrumModSource::from_index(20).id(), "macro8");
        assert_eq!(DrumModDest::from_index(1).id(), "pitch");
        assert_eq!(DrumModDest::from_index(6).id(), "level");
        assert_eq!(DrumModDest::from_index(9).id(), "ampdecay");
        assert_eq!(DrumModDest::from_index(12).id(), "layermix");

        // id <-> enum round-trip across both vocabularies.
        for i in 0..N_DRUM_SOURCES {
            let s = DrumModSource::from_index(i);
            assert_eq!(DrumModSource::from_id(s.id()), s, "source id round-trip");
        }
        for i in 0..N_DRUM_DESTS {
            let d = DrumModDest::from_index(i);
            assert_eq!(DrumModDest::from_id(d.id()), d, "dest id round-trip");
        }
        assert_eq!(DrumModSource::from_id("nope"), DrumModSource::Off);
        assert_eq!(DrumModDest::from_id("nope"), DrumModDest::Off);
    }

    #[test]
    fn target_voice_scopes_a_slot_to_one_track() {
        let mut m = DrumModMatrix::new();
        // Velocity (idx 1, value 1.0) -> Cutoff, but only voice 5.
        m.set_slot(0, DrumModSource::Velocity, DrumModDest::Cutoff, 1.0, 5);
        let sources = [1.0; N_DRUM_SOURCES];

        let mut acc5 = [0.0; N_DRUM_DESTS];
        m.accumulate(5, &sources, &mut acc5);
        assert_eq!(acc5[DrumModDest::Cutoff.index()], 1.0, "the targeted voice gets the route");

        let mut acc0 = [0.0; N_DRUM_DESTS];
        m.accumulate(0, &sources, &mut acc0);
        assert_eq!(acc0[DrumModDest::Cutoff.index()], 0.0, "a non-targeted voice is untouched");

        // ALL_VOICES reaches every track.
        m.set_slot(0, DrumModSource::Velocity, DrumModDest::Cutoff, 1.0, ALL_VOICES);
        let mut acc_any = [0.0; N_DRUM_DESTS];
        m.accumulate(3, &sources, &mut acc_any);
        assert_eq!(acc_any[DrumModDest::Cutoff.index()], 1.0, "ALL_VOICES reaches any track");
    }

    #[test]
    fn depth_is_clamped_and_off_skipped() {
        let mut m = DrumModMatrix::new();
        m.set_slot(0, DrumModSource::Velocity, DrumModDest::Pitch, 5.0, ALL_VOICES); // over-range
        assert_eq!(m.slots[0].depth, 1.0, "depth clamps to +1");
        m.set_slot(1, DrumModSource::Off, DrumModDest::Pitch, 1.0, ALL_VOICES); // Off source
        let sources = [1.0; N_DRUM_SOURCES];
        let mut acc = [0.0; N_DRUM_DESTS];
        m.accumulate(0, &sources, &mut acc);
        assert_eq!(acc[DrumModDest::Pitch.index()], 1.0, "only the active slot contributes");
    }
}
