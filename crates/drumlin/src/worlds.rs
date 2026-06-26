//! The flagship GROOVE WORLDS (M9, design §4.5/§8) — characterful one-touch
//! recalls of the whole machine: a curated `KitRow` set (voice tone, mix, mod
//! routings, bus FX) + relabeled macros + an embedded groove. Pure `&'static`
//! data; the pattern is a builder `fn` run editor-side at recall, so authoring a
//! groove is the same `set`-the-steps idiom as `Pattern::neutral_demo`.
//!
//! Track map: 0 KICK, 1 SUB, 2 SNARE, 3 CLAP, 4 RIM, 5 CL HAT, 6 OP HAT,
//! 7 RIDE, 8 TOM LO, 9 TOM HI, 10 COWBELL, 11 ZAP.
//! Bus ids (pget!): 1 pump, 2 drive, 3 reverb, 4 delay, 5 pump-rate, 6 pump-
//! curve, 7 parallel, 8 punch, 9 gate-time. Mod src idx: Velocity 1, Accent 2,
//! Lfo1 3, RandomPerHit 9, Macro1..8 13..20. Mod dst idx: Pitch 1, Cutoff 3,
//! Drive 5, Level 6, AmpDecay 9, Pan 10.

use crate::kits::{Kit, KitRow};
use percussion_core::Pattern;

/// Turn on a set of steps (+ optional accents) on a track — the `neutral_demo`
/// idiom, factored for the world grooves.
fn hits(p: &mut Pattern, track: usize, steps: &[usize], accents: &[usize]) {
    for &s in steps {
        p.tracks[track].steps[s].on = true;
        p.tracks[track].steps[s].velocity = 102;
    }
    for &s in accents {
        p.tracks[track].steps[s].on = true;
        p.tracks[track].steps[s].accent = true;
        p.tracks[track].steps[s].velocity = 122;
    }
}

// ---------------------------------------------------------------------------
// Discothèque — Daft Punk French house: four-on-the-floor, filtered swung disco
// hats, sidechain-pumped, glued.
// ---------------------------------------------------------------------------
fn discotheque_groove() -> Pattern {
    let mut p = Pattern::default();
    hits(&mut p, 0, &[0, 4, 8, 12], &[0]); // kick four-on-the-floor
    hits(&mut p, 3, &[4, 12], &[]); // clap backbeat
    hits(&mut p, 5, &[2, 6, 10, 14], &[]); // closed hats on the off-8ths (swing bites)
    hits(&mut p, 6, &[2, 6, 10, 14], &[]); // open-hat offbeats (choked by closed)
    hits(&mut p, 10, &[7], &[]); // a cowbell wink
    p.swing = 60;
    p.groove_amount = 35;
    p.seed = 0xD15C_0001;
    p
}

pub static DISCOTHEQUE: Kit = Kit {
    id: "discotheque",
    name: "Discothèque",
    blurb: "Daft Punk · French house",
    rows: &[
        KitRow::Bus { id: 1, norm: 0.75 },  // pump (the headline duck)
        KitRow::Bus { id: 5, norm: 0.5 },   // pump rate = 1/4
        KitRow::Bus { id: 7, norm: 0.55 },  // parallel glue comp
        KitRow::Bus { id: 2, norm: 0.2 },   // a little bus drive
        KitRow::Bus { id: 3, norm: 0.12 },  // a touch of room
        KitRow::Sidechain(false),           // internal-kick pump
        // K1 "Filter" rides cutoff across all voices (the disco filter sweep).
        KitRow::ModSlot { slot: 0, src: 13, dst: 3, depth: 0.6, voice: 0xFF },
        // Accent opens the hats a touch.
        KitRow::ModSlot { slot: 1, src: 2, dst: 3, depth: 0.25, voice: 5 },
    ],
    macro_labels: ["Filter", "Pump", "Swing", "Glue", "Decay", "Space", "Width", "Crush"],
    pattern: Some(discotheque_groove),
};

// ---------------------------------------------------------------------------
// Marseille — French 79 / Simon Henner: 808-flavoured, half-time snare,
// tape-delayed rim, humanized.
// ---------------------------------------------------------------------------
fn marseille_groove() -> Pattern {
    let mut p = Pattern::default();
    hits(&mut p, 0, &[0, 6, 10], &[0]); // syncopated 808 kick
    hits(&mut p, 2, &[8], &[8]); // half-time snare (beat 3 only)
    hits(&mut p, 4, &[2, 7, 11, 14], &[]); // rim ticks (tape-delayed)
    hits(&mut p, 5, &[0, 4, 8, 12], &[]); // steady closed hats
    p.swing = 56;
    p.humanize = 32; // the "humanize" hand-feel
    p.seed = 0x3A55_0002;
    p
}

pub static MARSEILLE: Kit = Kit {
    id: "marseille",
    name: "Marseille",
    blurb: "French 79 · 808 half-time",
    rows: &[
        KitRow::Bus { id: 4, norm: 0.42 },  // tape delay
        KitRow::Bus { id: 3, norm: 0.26 },  // room
        KitRow::Bus { id: 1, norm: 0.32 },  // gentle pump
        KitRow::Bus { id: 2, norm: 0.12 },
        KitRow::Mix { track: 4, field: 1, norm: 0.6 }, // rim -> Send B (delay)
        KitRow::Mix { track: 2, field: 0, norm: 0.3 }, // snare -> a little reverb
        KitRow::Voice { track: 0, param: 2, norm: 0.45 }, // 808 kick: darker, rounder cutoff
        // K1 "Tape" rides the rim/perc cutoff for the dub-delay sweep.
        KitRow::ModSlot { slot: 0, src: 13, dst: 3, depth: 0.4, voice: 4 },
        KitRow::Sidechain(false),
    ],
    macro_labels: ["Tape", "Humanize", "Density", "Room", "Decay", "Drive", "Width", "Lo-Fi"],
    pattern: Some(marseille_groove),
};

// ---------------------------------------------------------------------------
// Bladerunner — Vangelis: slow, cavernous reverbed toms, gated hits, sparse
// probability-driven evolution.
// ---------------------------------------------------------------------------
fn bladerunner_groove() -> Pattern {
    let mut p = Pattern::default();
    hits(&mut p, 0, &[0, 11], &[0]); // sparse, deep kick
    hits(&mut p, 8, &[4, 14], &[]); // low tom
    hits(&mut p, 9, &[7], &[7]); // high tom answer
    hits(&mut p, 7, &[0, 8], &[]); // ride bell pulse
    p.length = 16;
    p.swing = 52;
    p.groove_amount = 20;
    p.seed = 0xB1AD_0003;
    p
}

pub static BLADERUNNER: Kit = Kit {
    id: "bladerunner",
    name: "Bladerunner",
    blurb: "Vangelis · cavernous toms",
    rows: &[
        KitRow::Bus { id: 3, norm: 0.62 },  // big reverb (the cavern)
        KitRow::Bus { id: 4, norm: 0.22 },  // delay haze
        KitRow::Bus { id: 9, norm: 0.6 },   // long gate time
        KitRow::Mix { track: 8, field: 0, norm: 0.6 }, // tom lo -> reverb
        KitRow::Mix { track: 9, field: 0, norm: 0.6 }, // tom hi -> reverb
        // A slow LFO 1 evolves the cutoff across all voices.
        KitRow::Lfo { idx: 0, shape: 0, rate: 0.4, depth: 1.0, retrig: false },
        KitRow::ModSlot { slot: 0, src: 3, dst: 3, depth: 0.45, voice: 0xFF }, // Lfo1 -> Cutoff
        // K3 "Evolve" rides cutoff too (hand control over the sweep).
        KitRow::ModSlot { slot: 1, src: 15, dst: 3, depth: 0.4, voice: 0xFF }, // Macro3 -> Cutoff
        // A slow mod-env blooms the filter open at the top of playback.
        KitRow::ModEnv { attack: 0.4, decay: 2.0 },
        KitRow::ModSlot { slot: 2, src: 5, dst: 3, depth: 0.3, voice: 0xFF }, // ModEnv -> Cutoff
    ],
    macro_labels: ["Space", "Decay", "Evolve", "Drive", "Filter", "Width", "Pump", "Lo-Fi"],
    pattern: Some(bladerunner_groove),
};

// ---------------------------------------------------------------------------
// Outrun — 80s gated synthwave: detuned synthy perc, polymeter hats, snappy
// gated snare.
// ---------------------------------------------------------------------------
fn outrun_groove() -> Pattern {
    let mut p = Pattern::default();
    hits(&mut p, 0, &[0, 4, 8, 12], &[0]); // driving kick
    hits(&mut p, 2, &[4, 12], &[4, 12]); // big gated snare backbeat
    hits(&mut p, 5, &[0, 2, 4, 6, 8, 10], &[]); // closed hats...
    p.tracks[5].length = 12; // ...on a 12-step lane -> polymeter against the 16
    hits(&mut p, 11, &[6, 14], &[]); // zap stabs
    p.swing = 50;
    p.seed = 0x0427_0004;
    p
}

pub static OUTRUN: Kit = Kit {
    id: "outrun",
    name: "Outrun",
    blurb: "80s gated · synthwave",
    rows: &[
        KitRow::Bus { id: 3, norm: 0.45 },  // gated reverb space
        KitRow::Bus { id: 9, norm: 0.3 },   // short gate (the snap)
        KitRow::Bus { id: 8, norm: 0.4 },   // transient punch
        KitRow::Bus { id: 2, norm: 0.28 },  // drive
        KitRow::Mix { track: 2, field: 4, norm: 1.0 }, // snare -> gated verb
        KitRow::Mix { track: 2, field: 0, norm: 0.6 }, // snare -> reverb send
        KitRow::Mix { track: 2, field: 9, norm: 0.35 }, // a little analog drift on the snare
        // K2 "Detune" rides pitch on the synthy perc (zap).
        KitRow::ModSlot { slot: 0, src: 14, dst: 1, depth: 0.3, voice: 11 }, // Macro2 -> Pitch (zap)
    ],
    macro_labels: ["Bright", "Detune", "Poly", "Snap", "Decay", "Space", "Drive", "Width"],
    pattern: Some(outrun_groove),
};
