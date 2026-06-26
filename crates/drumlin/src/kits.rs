//! KITS + GROOVE WORLDS (M9) — the sound-world **lens**. A `Kit` recalls the
//! whole machine in one touch (per design §4.5/§4.6): a curated, partial list of
//! normalized parameter values, the 8 macro-knob labels, and — for a GROOVE
//! WORLD — an embedded pattern. It is Drumlin's analogue of Esker's `Scene`,
//! reusing the *discipline* (a partial normalized table = "leave the rest at
//! default") but its own typed address space, because Drumlin's machine is half
//! persisted structs (voice patch / mix / mod) and half host FX params.
//!
//! Factory kits are `&'static` data, so they're zero-alloc and the recall path
//! (chunk 2) just stages them into `PersistState`. **Neutral has empty rows and
//! no pattern** — recalling it resets the staged state to its defaults, which
//! are derived from `DrumKit::neutral()`, so Neutral is byte-exact and the
//! goldens are untouched.

use percussion_core::Pattern;

/// The default K1–K8 macro labels (design §4.5) — what Neutral and any kit that
/// doesn't relabel shows. Display-only; never touches DSP.
pub const DEFAULT_MACRO_LABELS: [&str; 8] =
    ["Punch", "Swing Feel", "Filter Sweep", "Drive/Glue", "Decay", "Space", "Stereo Width", "Lo-Fi"];

/// One parameter a kit sets. Typed (not a flat `(&str, f32)` map) because the id
/// strings collide across spaces — `cutoff`/`level`/`pan` exist in the tail
/// patch, the mod dests, and the mix. The recall path decodes each variant onto
/// the matching `DrumKit` / plugin setter. A kit lists only what it touches;
/// anything absent stays at the default.
#[derive(Clone, Copy, Debug)]
pub enum KitRow {
    /// Per-voice tail patch default: `param` is a `LockableParam` index `0..5`
    /// (Level/Pan/Cutoff/Resonance/Drive — the `N_TAIL_PARAMS` patch subset;
    /// Pitch/Decay are p-lock-only, not patch defaults). `norm` is `0..1`.
    Voice { track: u8, param: u8, norm: f32 },
    /// Per-voice MIX field `0..9` (send A/B, mute, solo, gated-verb, choke, EQ
    /// lo/hi, output, drift), normalized `0..1`.
    Mix { track: u8, field: u8, norm: f32 },
    /// A mod-matrix slot `0..15`: `src`/`dst` are `DrumModSource`/`DrumModDest`
    /// indices, `depth` `-1..1`, `voice` is `0xFF` (all) or a track. Macro
    /// routings are just slots with a `Macro1..8` source.
    ModSlot { slot: u8, src: u8, dst: u8, depth: f32, voice: u8 },
    /// LFO config (`idx` 0/1): shape discriminant, rate Hz, depth, retrigger.
    Lfo { idx: u8, shape: u8, rate: f32, depth: f32, retrig: bool },
    /// Mod-env attack + decay (seconds).
    ModEnv { attack: f32, decay: f32 },
    /// A bus-FX host param: `id` is the `pget!` gesture id `1..9`, `norm` `0..1`.
    Bus { id: u8, norm: f32 },
    /// The sidechain-key toggle.
    Sidechain(bool),
}

/// A KIT (timbral lens, `pattern = None`) or GROOVE WORLD (`pattern = Some`).
pub struct Kit {
    /// Stable id (the GUI highlight key + the disk-preset factory id).
    pub id: &'static str,
    pub name: &'static str,
    /// Short attribution shown under the name (e.g. "Daft Punk"). Display-only.
    pub blurb: &'static str,
    /// The curated parameter overrides; empty = "all defaults" (Neutral).
    pub rows: &'static [KitRow],
    /// K1–K8 labels for this world (display-only — relabels the MOD page knobs).
    pub macro_labels: [&'static str; 8],
    /// `Some` for a GROOVE WORLD — a builder run editor-side at recall, whose
    /// `Pattern` is memcpy'd into the selected slot. `None` for a timbral KIT
    /// (leaves the user's pattern untouched). A fn (not `&'static Pattern`) so a
    /// groove is authored with the `set`-the-steps idiom, not a 64-step const.
    pub pattern: Option<fn() -> Pattern>,
}

/// The Neutral anchor: no overrides, no pattern, default labels. Recalling it
/// resets the staged state to its `::default()` (derived from `DrumKit::neutral`)
/// — byte-exact, so the goldens never move.
pub static NEUTRAL: Kit = Kit {
    id: "neutral",
    name: "Neutral",
    blurb: "the bare machine",
    rows: &[],
    macro_labels: DEFAULT_MACRO_LABELS,
    pattern: None,
};

/// The factory kit/world list, surfaced in the KITS page: Neutral (the anchor)
/// + the flagship GROOVE WORLDS. Grows toward a 50+ library.
pub static FACTORY_KITS: &[&Kit] = &[
    &NEUTRAL,
    &crate::worlds::DISCOTHEQUE,
    &crate::worlds::MARSEILLE,
    &crate::worlds::BLADERUNNER,
    &crate::worlds::OUTRUN,
];

#[cfg(test)]
mod tests {
    use super::*;
    use percussion_core::{LockableParam, N_DRUM_DESTS, N_DRUM_SOURCES, N_TAIL_PARAMS};

    #[test]
    fn kit_address_space_is_pinned() {
        // A KitRow addresses params by index into these spaces; if any count
        // shifts, factory kit data would silently point at the wrong param.
        // Pin them here so such a change is caught at the kit layer too.
        assert_eq!(N_TAIL_PARAMS, 5, "Voice rows address the tail patch subset 0..5");
        assert_eq!(LockableParam::COUNT, 7);
        assert_eq!(N_DRUM_SOURCES, 21, "ModSlot.src indices");
        assert_eq!(N_DRUM_DESTS, 13, "ModSlot.dst indices");
    }

    #[test]
    fn neutral_is_an_empty_lens() {
        assert!(NEUTRAL.rows.is_empty(), "Neutral must override nothing (byte-exact)");
        assert!(NEUTRAL.pattern.is_none(), "Neutral leaves the pattern alone");
        assert_eq!(NEUTRAL.macro_labels, DEFAULT_MACRO_LABELS);
        // Neutral is the first factory entry the GUI shows.
        assert!(FACTORY_KITS.iter().any(|k| k.id == "neutral"));
    }
}
