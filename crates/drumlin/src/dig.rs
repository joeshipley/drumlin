//! THE DIG (M11) — seed-addressable groove excavation (plan §M11).
//!
//! A generative pattern engine themed to the name: a drumlin is what a glacier
//! leaves behind, and a *dig* excavates grooves from a seeded landscape. The
//! design rests on two ideas:
//!
//! 1. **Grammar, not dice.** A [`Terrain`] is a role-grammar over the 12 tracks
//!    (Anchor kick / Backbeat snare-clap / Motor hats / Color perc): per-position
//!    prior tables + velocity contours, transformed by the four [`DigKnobs`].
//!    Raw random sounds bad instantly; priors + an interestingness [`score`]
//!    (only the best of ~48 internal rolls are surfaced by [`dig_best`]) keep
//!    every candidate musical.
//! 2. **Every dig is an address.** [`dig_one`]`(terrain, knobs, seed)` is a pure
//!    function — the same address regenerates the same groove bit-exactly,
//!    forever. Every per-cell draw is an independent S&H seeded by
//!    `mix_seed(seed, track, step, purpose)` (the GROOVE LOCK idiom), which
//!    also makes DENSITY *monotonic*: turning it up reveals more of the same
//!    site instead of rerolling it. The dig seed becomes `Pattern.seed`, so
//!    humanize/drift/probability playback locks to the same address too.
//!
//! Entirely editor-side (the `worlds.rs` builder shape): no audio-thread cost,
//! no `percussion_core` change, no `synth_core` change — the goldens are
//! untouched by construction. Chunk 1 ships the engine + TECHNO/BREAKS
//! terrains; the full terrain set, the DIG page, mutate/locks, and the full
//! MOTION payload (conditions/ratchets-with-ramps/p-locks/micro) follow in
//! chunks 2-5.

use percussion_core::rng::mix_seed;
use percussion_core::{LockableParam, Pattern, TrigCondition, XorShift32, MAX_TRACKS};

/// Digs author the classic 16-step bar (patterns can still be edited longer).
pub const DIG_STEPS: usize = 16;
/// Candidates surfaced per dig (the DIG page grid).
pub const N_CANDIDATES: usize = 6;
/// Internal rolls per dig, filtered down to `N_CANDIDATES` by score.
const ROLLS_PER_DIG: usize = 48;

// Per-cell draw purposes (the S&H channel per (seed, track, step) cell). These
// seed GENERATION draws, evaluated once on the editor thread — distinct codes
// from the sequencer's playback purposes (0..6) only for clarity of intent.
const P_HIT: u32 = 16;
const P_VEL: u32 = 17;
const P_ACCENT: u32 = 18;
const P_PROB: u32 = 19;
const P_RATCHET: u32 = 20;
const P_MUTATE: u32 = 21;
const P_SPLICE: u32 = 22;
// MOTION payload draws (c5). Each feature uses its purpose for the APPLY
// threshold and purpose + P_VAL_OFF for its VALUE, so application stays
// monotonic in the knob (deeper motion adds features, never rerolls them).
const P_COND: u32 = 23;
const P_RAMP: u32 = 24;
const P_PLOCK_CUT: u32 = 25;
const P_PLOCK_PITCH: u32 = 26;
const P_PLOCK_DECAY: u32 = 27;
const P_MICRO: u32 = 28;
const P_FILL: u32 = 29;
const P_VAL_OFF: u32 = 64;

/// The four DIG knobs, all `0..=1`.
#[derive(Clone, Copy, Debug)]
pub struct DigKnobs {
    /// How much of the site is revealed. Monotonic per seed: hits present at a
    /// lower density are always present at a higher one.
    pub density: f32,
    /// Weight shifted off the strong quarters onto the off positions.
    pub sync: f32,
    /// How much *living* payload is written (chunk 1: per-step probability + a
    /// taste of ratchets; conditions/ramps/p-locks arrive in chunk 5).
    pub motion: f32,
    /// Flattens the priors toward chaos + widens velocity jitter.
    pub wild: f32,
}

impl Default for DigKnobs {
    fn default() -> Self {
        Self { density: 0.6, sync: 0.4, motion: 0.5, wild: 0.25 }
    }
}

/// A lane's job in the groove — decides how the knobs and the MOTION payload
/// treat it (anchors never become probabilistic; motors may ratchet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Anchor,
    Backbeat,
    Motor,
    Color,
}

/// One track's grammar within a terrain.
pub struct LanePrior {
    pub role: Role,
    /// Overall presence of this lane in the terrain (`0.0` = silent).
    pub activity: f32,
    /// Per-position on-prior `0..=100` at the reference knobs. High values are
    /// the lane's identity (they also drive the velocity contour: confident
    /// positions are loud, weak positions come out as ghosts).
    pub prior: [u8; DIG_STEPS],
    /// Velocity contour endpoints: a hit's base velocity lerps `lo..hi` by its
    /// position's prior (the confidence), then WILD jitters it.
    pub vel_lo: u8,
    pub vel_hi: u8,
}

/// A dig site: a named role-grammar + feel defaults. Pure `&'static` data —
/// authoring a terrain is 12 prior rows, exactly like authoring a world.
pub struct Terrain {
    pub id: &'static str,
    pub name: &'static str,
    /// Pattern swing (50..=75) and humanize (0..=100) the dig carries.
    pub swing: u8,
    pub humanize: u8,
    pub lanes: [LanePrior; MAX_TRACKS],
}

const fn lane(role: Role, activity: f32, vel_lo: u8, vel_hi: u8, prior: [u8; DIG_STEPS]) -> LanePrior {
    LanePrior { role, activity, prior, vel_lo, vel_hi }
}

const fn silent() -> LanePrior {
    lane(Role::Color, 0.0, 0, 0, [0; DIG_STEPS])
}

// Track map: 0 KICK, 1 SUB, 2 SNARE, 3 CLAP, 4 RIM, 5 CL HAT, 6 OP HAT,
// 7 RIDE, 8 TOM LO, 9 TOM HI, 10 COWBELL, 11 ZAP.

/// TECHNO — four-on-the-floor kick, clap backbeat, offbeat hats, sparse color.
pub static TECHNO: Terrain = Terrain {
    id: "techno",
    name: "Techno",
    swing: 52,
    humanize: 8,
    lanes: [
        lane(Role::Anchor, 1.0, 100, 122, [95, 2, 4, 2, 95, 2, 5, 3, 95, 2, 4, 2, 95, 3, 6, 10]),
        lane(Role::Anchor, 0.2, 84, 100, [70, 0, 0, 0, 0, 0, 0, 0, 70, 0, 0, 0, 0, 0, 0, 0]),
        lane(Role::Backbeat, 0.35, 78, 106, [0, 4, 2, 4, 55, 3, 2, 8, 0, 4, 2, 4, 55, 4, 3, 12]),
        lane(Role::Backbeat, 0.85, 92, 112, [0, 2, 0, 2, 90, 2, 0, 4, 0, 2, 0, 2, 90, 2, 0, 6]),
        lane(Role::Color, 0.45, 62, 88, [0, 10, 4, 14, 0, 8, 4, 10, 0, 10, 4, 14, 0, 8, 6, 16]),
        lane(Role::Motor, 1.0, 68, 96, [30, 14, 88, 14, 30, 14, 88, 16, 30, 14, 88, 14, 30, 16, 88, 20]),
        lane(Role::Motor, 0.7, 76, 98, [0, 0, 70, 0, 0, 0, 70, 0, 0, 0, 70, 0, 0, 0, 70, 4]),
        lane(Role::Motor, 0.25, 58, 80, [12, 0, 22, 0, 12, 0, 22, 0, 12, 0, 22, 0, 12, 0, 22, 0]),
        lane(Role::Color, 0.3, 70, 96, [0, 3, 0, 6, 0, 4, 0, 8, 0, 3, 0, 6, 0, 5, 3, 14]),
        lane(Role::Color, 0.3, 70, 96, [0, 5, 0, 4, 0, 6, 0, 5, 0, 5, 0, 4, 0, 7, 4, 12]),
        lane(Role::Color, 0.15, 66, 88, [0, 0, 8, 0, 0, 6, 0, 0, 0, 0, 8, 0, 0, 6, 0, 10]),
        lane(Role::Color, 0.4, 72, 100, [0, 6, 0, 12, 0, 6, 0, 10, 0, 6, 0, 12, 0, 8, 0, 16]),
    ],
};

/// BREAKS — funky displaced kick, big snare backbeat with ghost notes, straight
/// 8th hats, late-bar tom color.
pub static BREAKS: Terrain = Terrain {
    id: "breaks",
    name: "Breaks",
    swing: 57,
    humanize: 22,
    lanes: [
        lane(Role::Anchor, 1.0, 98, 120, [95, 3, 6, 4, 3, 4, 60, 8, 4, 3, 70, 4, 6, 4, 10, 6]),
        lane(Role::Anchor, 0.15, 82, 96, [55, 0, 0, 0, 0, 0, 0, 0, 45, 0, 0, 0, 0, 0, 0, 0]),
        lane(Role::Backbeat, 1.0, 56, 118, [0, 8, 4, 14, 90, 6, 4, 18, 3, 14, 4, 10, 90, 5, 10, 22]),
        lane(Role::Backbeat, 0.25, 84, 104, [0, 0, 0, 0, 45, 0, 0, 0, 0, 0, 0, 0, 45, 0, 0, 4]),
        lane(Role::Color, 0.35, 58, 84, [0, 8, 0, 10, 0, 6, 0, 8, 0, 8, 0, 10, 0, 6, 0, 12]),
        lane(Role::Motor, 1.0, 64, 92, [80, 10, 55, 12, 80, 10, 55, 12, 80, 10, 55, 12, 80, 12, 55, 18]),
        lane(Role::Motor, 0.5, 74, 96, [0, 0, 0, 0, 0, 0, 35, 0, 0, 0, 0, 0, 0, 0, 55, 0]),
        lane(Role::Motor, 0.2, 56, 78, [14, 0, 14, 0, 14, 0, 14, 0, 14, 0, 14, 0, 14, 0, 14, 0]),
        lane(Role::Color, 0.35, 68, 94, [0, 0, 0, 4, 0, 3, 0, 6, 0, 0, 0, 5, 0, 8, 12, 18]),
        lane(Role::Color, 0.35, 68, 94, [0, 3, 0, 0, 0, 4, 0, 3, 0, 3, 0, 0, 0, 10, 8, 16]),
        silent(),
        lane(Role::Color, 0.25, 70, 96, [0, 4, 0, 8, 0, 0, 0, 6, 0, 4, 0, 8, 0, 0, 6, 12]),
    ],
};

/// DISCO — the Discothèque dialect: four-on-the-floor, snare+clap backbeat,
/// THE open hat on every off-8th, 16th-leaning closed hats, cowbell winks.
pub static DISCO: Terrain = Terrain {
    id: "disco",
    name: "Disco",
    swing: 58,
    humanize: 14,
    lanes: [
        lane(Role::Anchor, 1.0, 98, 120, [95, 2, 3, 2, 92, 2, 4, 2, 95, 2, 3, 2, 92, 3, 5, 8]),
        lane(Role::Anchor, 0.15, 82, 96, [60, 0, 0, 0, 0, 0, 0, 0, 55, 0, 0, 0, 0, 0, 0, 0]),
        lane(Role::Backbeat, 0.8, 84, 112, [0, 3, 2, 4, 88, 3, 2, 6, 0, 3, 2, 4, 88, 3, 4, 10]),
        lane(Role::Backbeat, 0.75, 88, 110, [0, 2, 0, 2, 85, 2, 0, 3, 0, 2, 0, 2, 85, 2, 0, 5]),
        lane(Role::Color, 0.4, 60, 84, [0, 8, 3, 10, 0, 6, 3, 8, 0, 8, 3, 10, 0, 6, 4, 12]),
        lane(Role::Motor, 1.0, 66, 94, [55, 18, 70, 18, 55, 18, 70, 20, 55, 18, 70, 18, 55, 20, 70, 24]),
        // The off-8th open hat is THE disco signature — fully committed, so it
        // survives the off-position sync discount at default knobs.
        lane(Role::Motor, 1.0, 78, 100, [0, 0, 92, 0, 0, 0, 92, 0, 0, 0, 92, 0, 0, 0, 92, 6]),
        lane(Role::Motor, 0.2, 56, 78, [10, 0, 18, 0, 10, 0, 18, 0, 10, 0, 18, 0, 10, 0, 18, 0]),
        lane(Role::Color, 0.25, 68, 92, [0, 2, 0, 5, 0, 3, 0, 6, 0, 2, 0, 5, 0, 4, 3, 12]),
        lane(Role::Color, 0.25, 68, 92, [0, 4, 0, 3, 0, 5, 0, 4, 0, 4, 0, 3, 0, 6, 3, 10]),
        lane(Role::Color, 0.3, 62, 86, [0, 0, 12, 0, 0, 8, 0, 4, 0, 0, 12, 0, 0, 8, 0, 10]),
        lane(Role::Color, 0.2, 66, 92, [0, 4, 0, 8, 0, 4, 0, 6, 0, 4, 0, 8, 0, 5, 0, 10]),
    ],
};

/// HALFTIME — the Marseille dialect: 808 half-time. Syncopated kick, the
/// backbeat lands ONLY on beat 3 (step 8), hats carry the subdivision, big
/// space, humanized hand-feel.
pub static HALFTIME: Terrain = Terrain {
    id: "halftime",
    name: "Halftime",
    swing: 54,
    humanize: 26,
    lanes: [
        lane(Role::Anchor, 1.0, 100, 122, [95, 4, 8, 3, 3, 5, 55, 10, 3, 4, 50, 4, 8, 5, 12, 6]),
        lane(Role::Anchor, 0.35, 86, 104, [70, 0, 0, 4, 0, 0, 25, 0, 0, 0, 20, 0, 6, 0, 0, 0]),
        lane(Role::Backbeat, 1.0, 60, 118, [0, 4, 2, 6, 0, 3, 2, 8, 92, 4, 8, 6, 0, 6, 3, 14]),
        lane(Role::Backbeat, 0.3, 82, 102, [0, 0, 0, 0, 0, 0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 6]),
        lane(Role::Color, 0.5, 56, 82, [0, 10, 4, 12, 0, 8, 0, 10, 0, 10, 4, 12, 0, 8, 6, 14]),
        lane(Role::Motor, 1.0, 62, 92, [75, 12, 45, 14, 75, 14, 45, 30, 75, 12, 45, 14, 75, 16, 45, 35]),
        lane(Role::Motor, 0.4, 74, 96, [0, 0, 0, 0, 0, 0, 40, 0, 0, 0, 0, 0, 0, 0, 50, 0]),
        lane(Role::Motor, 0.1, 54, 76, [10, 0, 0, 0, 10, 0, 0, 0, 10, 0, 0, 0, 10, 0, 0, 0]),
        lane(Role::Color, 0.3, 66, 92, [0, 0, 0, 5, 0, 4, 0, 8, 0, 0, 0, 6, 0, 10, 8, 16]),
        lane(Role::Color, 0.3, 66, 92, [0, 4, 0, 0, 0, 5, 0, 4, 0, 4, 0, 0, 0, 8, 10, 14]),
        silent(),
        lane(Role::Color, 0.3, 68, 96, [0, 5, 0, 10, 0, 0, 0, 8, 0, 5, 0, 10, 0, 0, 8, 12]),
    ],
};

/// FOOTWORK — 160-BPM juke: the 3-3-2 kick lattice (0,3,6 / 10,13), clap
/// stabs, melodic toms, spare hats, machine-tight feel.
pub static FOOTWORK: Terrain = Terrain {
    id: "footwork",
    name: "Footwork",
    swing: 50,
    humanize: 6,
    lanes: [
        lane(Role::Anchor, 1.0, 98, 122, [92, 2, 4, 70, 3, 4, 72, 3, 4, 3, 68, 3, 4, 60, 6, 4]),
        lane(Role::Anchor, 0.4, 84, 104, [60, 0, 0, 30, 0, 0, 30, 0, 0, 0, 25, 0, 0, 20, 0, 0]),
        lane(Role::Backbeat, 0.5, 58, 108, [0, 3, 0, 6, 55, 3, 0, 10, 0, 4, 0, 8, 55, 4, 10, 14]),
        lane(Role::Backbeat, 0.7, 84, 108, [0, 0, 8, 0, 60, 0, 10, 0, 0, 8, 0, 0, 60, 0, 12, 8]),
        lane(Role::Color, 0.5, 58, 84, [0, 12, 0, 8, 0, 10, 0, 12, 0, 12, 0, 8, 0, 10, 6, 14]),
        lane(Role::Motor, 0.55, 60, 84, [60, 0, 25, 0, 60, 0, 25, 0, 60, 0, 25, 0, 60, 0, 25, 0]),
        lane(Role::Motor, 0.2, 72, 92, [0, 0, 0, 0, 0, 0, 30, 0, 0, 0, 0, 0, 0, 0, 35, 0]),
        silent(),
        lane(Role::Color, 0.6, 70, 100, [0, 6, 0, 18, 0, 8, 0, 14, 0, 6, 0, 18, 0, 12, 10, 8]),
        lane(Role::Color, 0.6, 70, 100, [0, 14, 0, 6, 0, 12, 0, 8, 0, 14, 0, 6, 0, 8, 12, 10]),
        lane(Role::Color, 0.25, 64, 88, [0, 0, 10, 0, 0, 10, 0, 0, 0, 0, 10, 0, 6, 0, 10, 0]),
        lane(Role::Color, 0.5, 72, 102, [0, 10, 0, 14, 0, 6, 0, 12, 0, 10, 0, 14, 0, 8, 0, 16]),
    ],
};

/// CAVERN — the Bladerunner dialect: sparse and deep. Kick at 0 and 11, the
/// TOMS carry the groove (call at 4, answer at 7, echo at 14/15), a ride-bell
/// pulse is the motor, almost nothing else. Space is the instrument.
pub static CAVERN: Terrain = Terrain {
    id: "cavern",
    name: "Cavern",
    swing: 52,
    humanize: 12,
    lanes: [
        lane(Role::Anchor, 1.0, 96, 118, [90, 0, 2, 0, 0, 3, 0, 4, 0, 2, 0, 65, 0, 3, 0, 6]),
        lane(Role::Anchor, 0.3, 84, 100, [55, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 25, 0, 0, 0, 0]),
        lane(Role::Backbeat, 0.15, 70, 96, [0, 0, 0, 0, 25, 0, 0, 0, 0, 0, 0, 0, 25, 0, 0, 6]),
        silent(),
        lane(Role::Color, 0.35, 54, 80, [0, 8, 0, 6, 0, 0, 8, 0, 0, 8, 0, 6, 0, 0, 10, 0]),
        lane(Role::Motor, 0.15, 52, 72, [15, 0, 0, 0, 15, 0, 0, 0, 15, 0, 0, 0, 15, 0, 0, 0]),
        silent(),
        lane(Role::Motor, 0.7, 58, 84, [70, 0, 25, 0, 55, 0, 25, 0, 70, 0, 25, 0, 55, 0, 30, 0]),
        lane(Role::Color, 0.85, 74, 104, [0, 0, 0, 0, 60, 0, 4, 0, 0, 6, 0, 0, 0, 4, 55, 0]),
        lane(Role::Color, 0.85, 74, 104, [0, 0, 0, 10, 0, 0, 0, 60, 0, 4, 0, 0, 8, 0, 0, 45]),
        silent(),
        lane(Role::Color, 0.2, 62, 90, [0, 0, 6, 0, 0, 6, 0, 0, 0, 0, 8, 0, 0, 6, 0, 10]),
    ],
};

/// OUTRUN — the Outrun dialect: 80s gated synthwave. Driving kick, the BIG
/// gated snare on 4 and 12, 8th hats, zap stabs, and the late-bar tom fill.
pub static OUTRUN_T: Terrain = Terrain {
    id: "outrun",
    name: "Outrun",
    swing: 50,
    humanize: 10,
    lanes: [
        lane(Role::Anchor, 1.0, 100, 122, [95, 2, 3, 2, 90, 2, 4, 25, 95, 2, 3, 2, 90, 3, 6, 8]),
        lane(Role::Anchor, 0.1, 82, 96, [50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        lane(Role::Backbeat, 1.0, 88, 120, [0, 2, 2, 3, 95, 2, 3, 6, 0, 2, 2, 3, 95, 3, 5, 12]),
        lane(Role::Backbeat, 0.4, 84, 106, [0, 0, 0, 0, 55, 0, 0, 0, 0, 0, 0, 0, 55, 0, 0, 4]),
        lane(Role::Color, 0.3, 58, 82, [0, 8, 0, 6, 0, 8, 0, 6, 0, 8, 0, 6, 0, 8, 4, 10]),
        lane(Role::Motor, 1.0, 64, 92, [70, 12, 60, 12, 70, 12, 60, 14, 70, 12, 60, 12, 70, 14, 60, 18]),
        lane(Role::Motor, 0.35, 74, 94, [0, 0, 45, 0, 0, 0, 0, 0, 0, 0, 45, 0, 0, 0, 0, 0]),
        lane(Role::Motor, 0.15, 54, 76, [12, 0, 0, 0, 12, 0, 0, 0, 12, 0, 0, 0, 12, 0, 0, 0]),
        lane(Role::Color, 0.45, 72, 100, [0, 0, 0, 4, 0, 3, 0, 6, 0, 0, 0, 5, 0, 14, 18, 10]),
        lane(Role::Color, 0.45, 72, 100, [0, 3, 0, 0, 0, 4, 0, 3, 0, 3, 0, 0, 10, 8, 16, 20]),
        lane(Role::Color, 0.1, 62, 84, [0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 6]),
        lane(Role::Color, 0.6, 74, 104, [0, 8, 0, 14, 0, 6, 25, 0, 0, 8, 0, 14, 0, 10, 0, 20]),
    ],
};

/// The factory dig sites. The first five are the DIG page chips; CAVERN and
/// OUTRUN are the world dialects (reachable via THIS WORLD, and by id).
pub static TERRAINS: &[&Terrain] =
    &[&TECHNO, &BREAKS, &DISCO, &HALFTIME, &FOOTWORK, &CAVERN, &OUTRUN_T];

pub fn terrain(id: &str) -> Option<&'static Terrain> {
    TERRAINS.iter().find(|t| t.id == id).copied()
}

/// THIS WORLD: the dig dialect for a recalled factory kit — read from the
/// kit's own `terrain` tag, so every library kit ships with an infinite groove
/// supply in its genre. Neutral (or an unknown/no kit) speaks the bare
/// machine's tongue: techno.
pub fn terrain_for_world(kit_id: &str) -> &'static Terrain {
    crate::kits::FACTORY_KITS
        .iter()
        .find(|k| k.id == kit_id)
        .and_then(|k| terrain(k.terrain))
        .unwrap_or(&TECHNO)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// One independent S&H draw per (seed, track, step, purpose) cell in `0..1` —
/// the GROOVE LOCK idiom, applied to generation.
#[inline]
fn cell01(seed: u32, t: u32, s: u32, purpose: u32) -> f32 {
    XorShift32::new(mix_seed(seed, t, s, purpose)).next_f32()
}

/// The knob-transformed on-probability for a lane position. Monotonic in
/// `density` for fixed everything-else (the DENSITY guarantee): WILD/SYNC/
/// activity shape a base `p in 0..1`, then density applies a gamma
/// (`2.2 sparse .. 0.55 busy`) — `p^g` rises as `g` falls, and the per-cell hit
/// draw is a fixed threshold test against it.
fn on_probability(lane: &LanePrior, pos: usize, k: &DigKnobs) -> f32 {
    let base = lane.prior[pos] as f32 / 100.0;
    // WILD flattens the identity toward an amorphous field.
    let p = lerp(base, 0.35, k.wild * 0.6);
    // SYNC shifts weight off the strong quarters onto the off positions.
    let strong = pos % 4 == 0;
    let sync_gain = if strong { lerp(1.15, 0.8, k.sync) } else { lerp(0.55, 1.5, k.sync) };
    let p = (p * sync_gain * lane.activity).clamp(0.0, 1.0);
    let gamma = lerp(2.2, 0.55, k.density.clamp(0.0, 1.0));
    p.powf(gamma).min(0.97)
}

/// Excavate ONE pattern from an address. Pure and total: the same
/// `(terrain, knobs, seed)` reproduces the same `Pattern` bit-exactly.
pub fn dig_one(terrain: &Terrain, knobs: &DigKnobs, seed: u32) -> Pattern {
    let mut p = Pattern::default();
    p.length = DIG_STEPS as u8;
    p.swing = terrain.swing;
    p.humanize = terrain.humanize;
    // The dig address IS the groove-lock seed: playback randomness (humanize,
    // drift, probability rolls) locks to the same address as the notes.
    p.seed = seed;

    for (t, lane) in terrain.lanes.iter().enumerate() {
        if lane.activity <= 0.0 {
            continue;
        }
        for s in 0..DIG_STEPS {
            let pr = on_probability(lane, s, knobs);
            if cell01(seed, t as u32, s as u32, P_HIT) >= pr {
                continue;
            }
            let st = &mut p.tracks[t].steps[s];
            st.on = true;

            // Velocity: contour by confidence (the prior), jittered by WILD.
            // Weak-prior positions that rolled on come out quiet — ghost notes
            // for free (the BREAKS snare lives on this).
            let conf = lane.prior[s] as f32 / 100.0;
            let base = lerp(lane.vel_lo as f32, lane.vel_hi as f32, conf);
            let jitter = (cell01(seed, t as u32, s as u32, P_VEL) - 0.5)
                * 2.0
                * lerp(4.0, 18.0, knobs.wild);
            st.velocity = (base + jitter).round().clamp(20.0, 127.0) as u8;

            // Accents live on the confident positions only.
            st.accent = lane.prior[s] >= 80 && cell01(seed, t as u32, s as u32, P_ACCENT) < 0.3;

            // MOTION (chunk-1 scope): non-identity hits become probabilistic —
            // the pattern breathes differently every pass. Anchors and identity
            // positions stay locked at 100 so the groove never loses its spine.
            st.probability = if lane.role == Role::Anchor || lane.prior[s] >= 80 {
                100
            } else {
                let depth = knobs.motion * cell01(seed, t as u32, s as u32, P_PROB);
                (100.0 - depth * 45.0).round() as u8
            };

            // Ratchets on motor/color lanes (their ramps arrive in apply_motion).
            st.ratchet = if matches!(lane.role, Role::Motor | Role::Color)
                && cell01(seed, t as u32, s as u32, P_RATCHET) < knobs.motion * 0.10
            {
                2 + (mix_seed(seed, t as u32, s as u32, P_RATCHET + 1) % 2) as u8
            } else {
                1
            };
        }
    }
    apply_motion(&mut p, terrain, knobs, seed);
    p
}

/// The MOTION layer (c5): write the LIVING payload — pre-wired FILL hits,
/// Ratio/NotFill conditions, ratchet ramps, p-locks, micro nudges — scaled by
/// the knob, role-aware, per-cell seeded (deterministic + monotonic: deeper
/// motion adds features to the same dig, never rerolls it). The anchor lanes
/// stay pure: the spine is sacred.
fn apply_motion(p: &mut Pattern, terrain: &Terrain, knobs: &DigKnobs, seed: u32) {
    let m = knobs.motion.clamp(0.0, 1.0);
    if m <= 0.0 {
        return;
    }
    for (t, lane) in terrain.lanes.iter().enumerate() {
        if lane.activity <= 0.0 {
            continue;
        }
        let tu = t as u32;
        for s in 0..DIG_STEPS {
            let su = s as u32;
            let st = &mut p.tracks[t].steps[s];
            if st.on {
                match lane.role {
                    Role::Anchor => {}
                    Role::Backbeat => {
                        // The big hits lay back a touch; ghosts push/pull.
                        st.micro = if lane.prior[s] >= 80 {
                            (m * 6.0 * cell01(seed, tu, su, P_MICRO)).round() as i16
                        } else {
                            ((cell01(seed, tu, su, P_MICRO) - 0.5) * 2.0 * m * 10.0).round() as i16
                        };
                        // Occasional dying drag-roll on a ghost (flam ramp).
                        if lane.prior[s] < 80
                            && st.ratchet == 1
                            && cell01(seed, tu, su, P_RAMP) < m * 0.12
                        {
                            st.ratchet = 2 + (cell01(seed, tu, su, P_RAMP + P_VAL_OFF) * 1.99) as u8;
                            st.ratchet_ramp =
                                (-(40.0 + cell01(seed, tu, su, P_RAMP + P_VAL_OFF) * 50.0)) as i8;
                        }
                        // A shortened/lengthened tail now and then.
                        if st.plock_count < 2 && cell01(seed, tu, su, P_PLOCK_DECAY) < m * 0.15 {
                            st.set_plock(
                                LockableParam::Decay.index(),
                                0.3 + cell01(seed, tu, su, P_PLOCK_DECAY + P_VAL_OFF) * 0.4,
                            );
                        }
                    }
                    Role::Motor => {
                        // The hat filter dance.
                        if st.plock_count < 2 && cell01(seed, tu, su, P_PLOCK_CUT) < m * 0.30 {
                            st.set_plock(
                                LockableParam::Cutoff.index(),
                                0.35 + cell01(seed, tu, su, P_PLOCK_CUT + P_VAL_OFF) * 0.6,
                            );
                        }
                        // Existing rolls become builds.
                        if st.ratchet > 1 && st.ratchet_ramp == 0 {
                            st.ratchet_ramp =
                                (40.0 + cell01(seed, tu, su, P_RAMP + P_VAL_OFF) * 50.0) as i8;
                        }
                        // Non-identity motor hits duck out during fills — space
                        // for the pre-wired fill to speak.
                        if lane.prior[s] < 80 && cell01(seed, tu, su, P_COND) < m * 0.12 {
                            st.condition = TrigCondition::NotFill;
                        }
                    }
                    Role::Color => {
                        st.micro =
                            ((cell01(seed, tu, su, P_MICRO) - 0.5) * 2.0 * m * 12.0).round() as i16;
                        // Every-other-bar evolution.
                        if cell01(seed, tu, su, P_COND) < m * 0.20 {
                            let a = if cell01(seed, tu, su, P_COND + P_VAL_OFF) < 0.5 { 1 } else { 2 };
                            st.condition = TrigCondition::Ratio { a, b: 2 };
                        }
                        // Melodic pitch stabs + occasional tail play.
                        if st.plock_count < 2 && cell01(seed, tu, su, P_PLOCK_PITCH) < m * 0.22 {
                            st.set_plock(
                                LockableParam::Pitch.index(),
                                0.5 + (cell01(seed, tu, su, P_PLOCK_PITCH + P_VAL_OFF) - 0.5) * 0.3,
                            );
                        }
                        if st.plock_count < 2 && cell01(seed, tu, su, P_PLOCK_DECAY) < m * 0.12 {
                            st.set_plock(
                                LockableParam::Decay.index(),
                                0.3 + cell01(seed, tu, su, P_PLOCK_DECAY + P_VAL_OFF) * 0.4,
                            );
                        }
                    }
                }
            } else if s >= 10 && matches!(lane.role, Role::Color | Role::Backbeat) {
                // PRE-WIRED FILLS: extra late-bar hits that sound ONLY while the
                // FILL button is held. The dig arrives with its fill built.
                if cell01(seed, tu, su, P_FILL) < m * 0.30 * lane.activity {
                    st.on = true;
                    st.condition = TrigCondition::Fill;
                    let conf = lane.prior[s].max(55) as f32 / 100.0;
                    let base = lerp(lane.vel_lo.max(60) as f32, lane.vel_hi.max(80) as f32, conf);
                    st.velocity = (base.round() as i32).clamp(40, 127) as u8;
                    st.probability = 100;
                    if cell01(seed, tu, su, P_FILL + P_VAL_OFF) < 0.35 {
                        st.ratchet = 2;
                        st.ratchet_ramp = 55; // fills build
                    }
                }
            }
        }
    }
}

/// A surfaced dig: the address, its interestingness, and the groove itself.
pub struct DigCandidate {
    pub seed: u32,
    pub score: f32,
    pub pattern: Pattern,
}

/// MUTATE configuration: which lanes are YOURS (locked, copied verbatim from
/// the source — notes, feel, p-locks and all) and how deep the variation cuts
/// on the rest.
pub struct MutateCfg {
    pub locks: [bool; MAX_TRACKS],
    /// `0..=1`: the per-cell probability that an unlocked cell is re-excavated
    /// from the terrain grammar instead of kept from the source. Per-cell S&H
    /// draws make it monotonic per seed: cells mutated at a shallow depth stay
    /// mutated (to the SAME value) as depth rises — the knob digs deeper into
    /// the same variation, it never rerolls it.
    pub depth: f32,
}

/// Excavate a VARIATION of `source`: locked lanes come through bit-identical
/// (the whole `Track` — polymeter length, level, every step payload); unlocked
/// lanes keep the source except where the per-cell mutate draw (< `depth`)
/// swaps in the terrain grammar's cell for this seed. The source's feel
/// (length, swing, humanize, groove, GROOVE-LOCK seed) is preserved, so locked
/// lanes also PLAY back identically. Deterministic given (source, terrain,
/// knobs, cfg, seed).
pub fn dig_mutate(
    terrain: &Terrain,
    knobs: &DigKnobs,
    source: &Pattern,
    cfg: &MutateCfg,
    seed: u32,
) -> Pattern {
    let gen = dig_one(terrain, knobs, seed);
    let mut p = *source;
    let depth = cfg.depth.clamp(0.0, 1.0);
    for t in 0..MAX_TRACKS {
        if cfg.locks[t] {
            continue;
        }
        for s in 0..DIG_STEPS {
            if cell01(seed, t as u32, s as u32, P_MUTATE) < depth {
                p.tracks[t].steps[s] = gen.tracks[t].steps[s];
            }
        }
    }
    p
}

/// Lane-level crossover: each track comes WHOLLY from `a` or `b` (a seeded
/// coin per lane), meta from `a`. Musically coherent by construction — you get
/// A's kick with B's hats, never a cell-salad.
pub fn splice(a: &Pattern, b: &Pattern, seed: u32) -> Pattern {
    let mut p = *a;
    for t in 0..MAX_TRACKS {
        if cell01(seed, t as u32, 0, P_SPLICE) < 0.5 {
            p.tracks[t] = b.tracks[t];
        }
    }
    p
}

/// MUTATE dig: roll variations of `source`, score, surface the best `k`.
/// A candidate's address reproduces it GIVEN the same source + cfg (mutations
/// are relative to your pattern at dig time).
pub fn dig_best_mutate(
    terrain: &Terrain,
    knobs: &DigKnobs,
    source: &Pattern,
    cfg: &MutateCfg,
    base_seed: u32,
    k: usize,
) -> Vec<DigCandidate> {
    let mut all: Vec<DigCandidate> = (0..ROLLS_PER_DIG)
        .map(|i| {
            let seed = mix_seed(base_seed, 0xD16, i as u32, 0x0A75);
            let pattern = dig_mutate(terrain, knobs, source, cfg, seed);
            let score = score(terrain, &pattern, knobs);
            DigCandidate { seed, score, pattern }
        })
        .collect();
    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(k.min(ROLLS_PER_DIG));
    all
}

/// BREED from a favorite: each candidate splices the parent with a FRESH
/// terrain roll (locked lanes force the parent's), scored, best `k` surfaced —
/// "offspring of the one you liked."
pub fn dig_best_breed(
    terrain: &Terrain,
    knobs: &DigKnobs,
    parent: &Pattern,
    locks: &[bool; MAX_TRACKS],
    base_seed: u32,
    k: usize,
) -> Vec<DigCandidate> {
    let mut all: Vec<DigCandidate> = (0..ROLLS_PER_DIG)
        .map(|i| {
            let seed = mix_seed(base_seed, 0xB4EE, i as u32, 0xD00D);
            let fresh = dig_one(terrain, knobs, seed);
            let mut pattern = splice(parent, &fresh, seed);
            for t in 0..MAX_TRACKS {
                if locks[t] {
                    pattern.tracks[t] = parent.tracks[t];
                }
            }
            let score = score(terrain, &pattern, knobs);
            DigCandidate { seed, score, pattern }
        })
        .collect();
    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(k.min(ROLLS_PER_DIG));
    all
}

/// Roll `ROLLS_PER_DIG` addresses derived from `base_seed`, score them, and
/// return the best `k` (sorted best-first). Each candidate is addressable by
/// its OWN seed: `dig_one(terrain, knobs, candidate.seed)` reproduces it.
/// Editor-thread only (allocates freely).
pub fn dig_best(terrain: &Terrain, knobs: &DigKnobs, base_seed: u32, k: usize) -> Vec<DigCandidate> {
    let mut all: Vec<DigCandidate> = (0..ROLLS_PER_DIG)
        .map(|i| {
            let seed = mix_seed(base_seed, 0xD16, i as u32, 0x5EED);
            let pattern = dig_one(terrain, knobs, seed);
            let score = score(terrain, &pattern, knobs);
            DigCandidate { seed, score, pattern }
        })
        .collect();
    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(k.min(ROLLS_PER_DIG));
    all
}

/// Score a candidate in the 16-step dig window: how much like MUSIC is it?
/// Deterministic heuristics, each `0..=1`, weighted to sum 1. This is the taste
/// filter that separates a dig from a dice roll. Terrain-aware where it must
/// be: the density term compares against the TERRAIN'S OWN expected hit count
/// at these knobs (a sparse cavern isn't punished for being sparse), and the
/// motor term follows whichever lane the terrain actually uses as its motor
/// (the cavern's motor is the ride, not the closed hat).
pub fn score(terrain: &Terrain, p: &Pattern, knobs: &DigKnobs) -> f32 {
    let mut total = 0u32;
    let mut off_grid = 0u32;
    let mut col = [0u32; DIG_STEPS];
    let mut vel_sum = 0.0f32;
    // Editor-side scoring scratch (a plain Vec; NOT audio-thread code).
    let mut vels: Vec<f32> = Vec::new();
    for t in 0..MAX_TRACKS {
        for (s, c) in col.iter_mut().enumerate() {
            let st = &p.tracks[t].steps[s];
            // Fill-only steps are silent on the normal pass — the score judges
            // what the groove SOUNDS like, not its fill reserve.
            if st.on && st.condition != TrigCondition::Fill {
                total += 1;
                *c += 1;
                if s % 4 != 0 {
                    off_grid += 1;
                }
                vel_sum += st.velocity as f32;
                vels.push(st.velocity as f32);
            }
        }
    }
    if total == 0 {
        return 0.0;
    }

    // Anchor: a kick on the downbeat is the spine; a kick lane in a sane band.
    let kick_hits = (0..DIG_STEPS).filter(|&s| p.tracks[0].steps[s].on).count() as f32;
    let a_anchor = (if p.tracks[0].steps[0].on { 1.0 } else { 0.25 }) * band(kick_hits, 2.0, 8.0);

    // Backbeat: snare or clap on 4/12 (either half). Terrains without a
    // backbeat identity give every candidate the same floor — within-terrain
    // ranking is unaffected.
    let bb = p.tracks[2].steps[4].on
        || p.tracks[2].steps[12].on
        || p.tracks[3].steps[4].on
        || p.tracks[3].steps[12].on;
    let a_backbeat = if bb { 1.0 } else { 0.35 };

    // Density: total hits near what THIS terrain's priors predict at these
    // knobs (the sum of every cell's on-probability) — self-calibrating for
    // sparse and dense terrains alike.
    let mut expected = 0.0_f32;
    for lane in terrain.lanes.iter() {
        if lane.activity > 0.0 {
            for s in 0..DIG_STEPS {
                expected += on_probability(lane, s, knobs);
            }
        }
    }
    let a_density = (1.0 - ((total as f32 - expected).abs() / expected.max(1.0))).max(0.0);

    // Syncopation balance: off-grid ratio near the knob's target.
    let r_off = off_grid as f32 / total as f32;
    let a_sync = (1.0 - (r_off - lerp(0.12, 0.55, knobs.sync)).abs() * 2.0).max(0.0);

    // Motor flow: the terrain's busiest motor lane shouldn't leave holes.
    // Neutral when the terrain has no committed motor (all-sparse terrains).
    let motor_lane = terrain
        .lanes
        .iter()
        .enumerate()
        .filter(|(_, l)| l.role == Role::Motor && l.activity >= 0.5)
        .max_by(|a, b| a.1.activity.partial_cmp(&b.1.activity).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i);
    let a_motor = match motor_lane {
        Some(lane) => motor_flow(p, lane),
        None => 0.75,
    };

    // Mud: penalize columns where too many voices pile up.
    let crowded = col.iter().filter(|&&c| c >= 6).count() as f32;
    let a_mud = (1.0 - crowded / 4.0).max(0.0);

    // Variety: a healthy velocity spread (contour + ghosts, not a flat wall).
    let mean = vel_sum / total as f32;
    let var = vels.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / total as f32;
    let a_variety = band(var.sqrt(), 6.0, 26.0);

    0.22 * a_anchor
        + 0.14 * a_backbeat
        + 0.18 * a_density
        + 0.14 * a_sync
        + 0.12 * a_motor
        + 0.12 * a_mud
        + 0.08 * a_variety
}

/// 1.0 inside `lo..=hi`, decaying linearly to 0 at half/double the band edges.
fn band(x: f32, lo: f32, hi: f32) -> f32 {
    if x >= lo && x <= hi {
        1.0
    } else if x < lo {
        (x / lo).max(0.0)
    } else {
        (1.0 - (x - hi) / hi).max(0.0)
    }
}

/// Longest cyclic gap between a motor lane's hits, mapped to `0..=1` (a motor
/// that stalls for more than a quarter note loses its flow score).
fn motor_flow(p: &Pattern, lane: usize) -> f32 {
    let hits: Vec<usize> = (0..DIG_STEPS).filter(|&s| p.tracks[lane].steps[s].on).collect();
    if hits.is_empty() {
        return 0.2;
    }
    let mut max_gap = 0usize;
    for (i, &h) in hits.iter().enumerate() {
        let next = if i + 1 < hits.len() { hits[i + 1] } else { hits[0] + DIG_STEPS };
        max_gap = max_gap.max(next - h);
    }
    (4.0 / max_gap as f32).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use percussion_core::{DrumKit, SeqState, Sequencer};

    #[test]
    fn a_dig_address_is_bit_exact_forever() {
        let k = DigKnobs::default();
        for t in TERRAINS {
            let a = dig_one(t, &k, 0xB1AD_5EED);
            let b = dig_one(t, &k, 0xB1AD_5EED);
            assert_eq!(a, b, "{}: same address must reproduce bit-exactly", t.id);
            let c = dig_one(t, &k, 0xB1AD_5EEE);
            assert_ne!(a, c, "{}: a different seed must be a different site", t.id);
        }
    }

    #[test]
    fn density_reveals_the_same_site_monotonically() {
        // The DENSITY guarantee: every hit present at a lower density is present
        // at a higher one (same seed) — the knob excavates, it never rerolls.
        for t in TERRAINS {
            for seed in [1u32, 0xC0FF_EE00, 0x0427_C0DE] {
                let mut prev: Option<Pattern> = None;
                for d in [0.15_f32, 0.4, 0.65, 0.9] {
                    let k = DigKnobs { density: d, ..DigKnobs::default() };
                    let cur = dig_one(t, &k, seed);
                    if let Some(lo) = &prev {
                        for tr in 0..MAX_TRACKS {
                            for s in 0..DIG_STEPS {
                                if lo.tracks[tr].steps[s].on {
                                    assert!(
                                        cur.tracks[tr].steps[s].on,
                                        "{}: hit ({tr},{s}) vanished when density rose to {d}",
                                        t.id
                                    );
                                }
                            }
                        }
                    }
                    prev = Some(cur);
                }
            }
        }
    }

    #[test]
    fn payload_is_valid_at_every_knob_corner() {
        let vals = [0.0_f32, 0.5, 1.0];
        for t in TERRAINS {
            for &density in &vals {
                for &sync in &vals {
                    for &motion in &vals {
                        for &wild in &vals {
                            let k = DigKnobs { density, sync, motion, wild };
                            for seed in [0u32, 7, 0xFFFF_FFFF] {
                                let p = dig_one(t, &k, seed);
                                assert_eq!(p.length, DIG_STEPS as u8);
                                assert_eq!(p.seed, seed);
                                assert!((50..=75).contains(&p.swing));
                                for tr in 0..MAX_TRACKS {
                                    for s in 0..64 {
                                        let st = &p.tracks[tr].steps[s];
                                        if s >= DIG_STEPS {
                                            assert!(!st.on, "no hits past the dig window");
                                        }
                                        if st.on {
                                            assert!((1..=127).contains(&st.velocity));
                                            assert!((1..=100).contains(&st.probability));
                                            assert!((1..=8).contains(&st.ratchet));
                                            assert!((-100..=100).contains(&st.ratchet_ramp));
                                            assert!(st.micro.abs() <= 48, "micro stays in slider range");
                                            assert!(st.plocks().len() <= 2, "at most 2 motion p-locks");
                                            for pl in st.plocks() {
                                                assert!(
                                                    LockableParam::from_index(pl.param).is_some(),
                                                    "p-lock param must decode"
                                                );
                                                assert!((0.0..=1.0).contains(&pl.value));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn dig_best_is_sorted_deterministic_and_anchored() {
        for t in TERRAINS {
            let k = DigKnobs::default();
            let a = dig_best(t, &k, 0x5EED_B347, N_CANDIDATES);
            let b = dig_best(t, &k, 0x5EED_B347, N_CANDIDATES);
            assert_eq!(a.len(), N_CANDIDATES);
            assert!(a.iter().zip(&b).all(|(x, y)| x.seed == y.seed), "{}: dig must be reproducible", t.id);
            assert!(a.windows(2).all(|w| w[0].score >= w[1].score), "{}: sorted best-first", t.id);
            // Every candidate is addressable by its own seed.
            for c in &a {
                assert_eq!(c.pattern, dig_one(t, &k, c.seed), "{}: candidate address round-trips", t.id);
            }
            // The winner has a spine: a non-empty kick lane.
            let kick_hits = (0..DIG_STEPS).filter(|&s| a[0].pattern.tracks[0].steps[s].on).count();
            assert!(kick_hits >= 2, "{}: the best dig must have an anchor (got {kick_hits} kicks)", t.id);
        }
    }

    #[test]
    fn scoring_prefers_music_over_silence_and_walls() {
        let k = DigKnobs::default();
        let empty = Pattern::default();
        let mut wall = Pattern::default();
        for tr in 0..MAX_TRACKS {
            for s in 0..DIG_STEPS {
                wall.tracks[tr].steps[s].on = true;
                wall.tracks[tr].steps[s].velocity = 100;
            }
        }
        for t in TERRAINS {
            let best = &dig_best(t, &k, 42, 1)[0];
            assert!(best.score > score(t, &empty, &k), "{}: a dig must beat silence", t.id);
            assert!(best.score > score(t, &wall, &k), "{}: a dig must beat a wall of hits", t.id);
        }
    }

    #[test]
    fn digs_render_finite_and_bounded() {
        // End-to-end sound safety: the best dig per terrain plays 2 bars through
        // a neutral kit (the same harness shape as the factory-world test) and
        // must stay finite, bus-limited, and audible.
        let sr = 48_000.0_f32;
        for t in TERRAINS {
            let dug = dig_best(t, &DigKnobs::default(), 0xD16_0001, 1).remove(0).pattern;
            let mut state = SeqState::default();
            state.patterns[0] = dug;
            state.current = 0;

            let mut kit = DrumKit::neutral(sr);
            let mut seq = Sequencer::new();
            seq.import(&state);
            seq.set_playing(true);

            let block = 512usize;
            let tempo = 128.0_f64;
            let qn_per_block = (block as f64 / sr as f64) * (tempo / 60.0);
            let mut pos = 0.0_f64;
            let mut peak = 0.0_f32;
            let mut made_sound = false;
            while pos < 8.0 {
                seq.process_block(pos, tempo, sr as f64, block);
                let pending = seq.pending();
                let mut ti = 0;
                for i in 0..block {
                    while ti < pending.len() {
                        let trg = pending[ti];
                        if trg.offset as usize > i {
                            break;
                        }
                        kit.trigger_seq(&trg);
                        ti += 1;
                    }
                    let (l, r) = kit.render();
                    assert!(l.is_finite() && r.is_finite(), "{}: dig rendered non-finite", t.id);
                    peak = peak.max(l.abs()).max(r.abs());
                    if l.abs() > 1e-3 {
                        made_sound = true;
                    }
                }
                pos += qn_per_block;
            }
            assert!(peak <= 1.02, "{}: dig exceeded the limiter: {peak}", t.id);
            assert!(made_sound, "{}: a dig must make sound", t.id);
        }
    }

    #[test]
    fn mutate_locks_and_depth_behave() {
        let t = terrain("breaks").unwrap();
        let k = DigKnobs::default();
        let source = Pattern::neutral_demo();

        // Depth 0, no locks: the variation IS the source, bit-exact.
        let cfg0 = MutateCfg { locks: [false; MAX_TRACKS], depth: 0.0 };
        assert_eq!(dig_mutate(t, &k, &source, &cfg0, 7), source, "depth 0 must be the source");

        // Locked lanes are bit-identical even at full depth; something else moves.
        let mut locks = [false; MAX_TRACKS];
        locks[0] = true; // my kick
        locks[2] = true; // my snare
        let cfg = MutateCfg { locks, depth: 1.0 };
        let m = dig_mutate(t, &k, &source, &cfg, 0xC0FF_EE00);
        assert_eq!(m.tracks[0], source.tracks[0], "locked kick must survive verbatim");
        assert_eq!(m.tracks[2], source.tracks[2], "locked snare must survive verbatim");
        assert_ne!(m, source, "full-depth mutation must change the unlocked lanes");
        // The source's feel is preserved (locked lanes also PLAY identically).
        assert_eq!(m.seed, source.seed);
        assert_eq!(m.swing, source.swing);
        assert_eq!(m.humanize, source.humanize);
        assert_eq!(m.length, source.length);

        // Depth accumulates, never rerolls: every cell mutated at 0.3 holds the
        // SAME value at 0.7 (same per-cell draw, same generated cell).
        let shallow = dig_mutate(t, &k, &source, &MutateCfg { locks: [false; MAX_TRACKS], depth: 0.3 }, 42);
        let deep = dig_mutate(t, &k, &source, &MutateCfg { locks: [false; MAX_TRACKS], depth: 0.7 }, 42);
        for tr in 0..MAX_TRACKS {
            for s in 0..DIG_STEPS {
                if shallow.tracks[tr].steps[s] != source.tracks[tr].steps[s] {
                    assert_eq!(
                        deep.tracks[tr].steps[s], shallow.tracks[tr].steps[s],
                        "deeper depth must keep shallow mutations at ({tr},{s})"
                    );
                }
            }
        }

        // dig_best_mutate: deterministic, sorted, and the locks hold on every candidate.
        let a = dig_best_mutate(t, &k, &source, &cfg, 9, N_CANDIDATES);
        let b = dig_best_mutate(t, &k, &source, &cfg, 9, N_CANDIDATES);
        assert!(a.iter().zip(&b).all(|(x, y)| x.seed == y.seed && x.pattern == y.pattern));
        assert!(a.windows(2).all(|w| w[0].score >= w[1].score));
        for c in &a {
            assert_eq!(c.pattern.tracks[0], source.tracks[0], "every candidate keeps the locked kick");
        }
    }

    #[test]
    fn motion_zero_is_a_static_pattern() {
        // With MOTION at 0 the dig is pure notes: no conditions, no p-locks, no
        // micro, no ramps, every probability at 100.
        for t in TERRAINS {
            let p = dig_one(t, &DigKnobs { motion: 0.0, ..DigKnobs::default() }, 0xCAFE);
            for tr in 0..MAX_TRACKS {
                for s in 0..DIG_STEPS {
                    let st = &p.tracks[tr].steps[s];
                    assert_eq!(st.condition, TrigCondition::Always, "{}: no conditions", t.id);
                    assert_eq!(st.plocks().len(), 0, "{}: no p-locks", t.id);
                    assert_eq!(st.micro, 0, "{}: no micro", t.id);
                    assert_eq!(st.ratchet_ramp, 0, "{}: no ramps", t.id);
                    if st.on {
                        assert_eq!(st.probability, 100, "{}: no chance", t.id);
                    }
                }
            }
        }
    }

    #[test]
    fn motion_one_writes_the_full_living_payload() {
        // At full MOTION, across a handful of seeds, every payload kind appears:
        // pre-wired FILL hits, evolving/ducking conditions, p-locks, micro,
        // ratchet ramps, and sub-100 probability. Deterministic scan.
        let t = terrain("disco").unwrap();
        let k = DigKnobs { motion: 1.0, ..DigKnobs::default() };
        let (mut fill, mut cond, mut plock, mut micro, mut ramp, mut prob) =
            (false, false, false, false, false, false);
        for seed in 0..30u32 {
            let p = dig_one(t, &k, seed);
            for tr in 0..MAX_TRACKS {
                for s in 0..DIG_STEPS {
                    let st = &p.tracks[tr].steps[s];
                    if !st.on {
                        continue;
                    }
                    match st.condition {
                        TrigCondition::Fill => fill = true,
                        TrigCondition::NotFill | TrigCondition::Ratio { .. } => cond = true,
                        _ => {}
                    }
                    if !st.plocks().is_empty() {
                        plock = true;
                    }
                    if st.micro != 0 {
                        micro = true;
                    }
                    if st.ratchet_ramp != 0 {
                        ramp = true;
                    }
                    if st.probability < 100 {
                        prob = true;
                    }
                }
            }
        }
        assert!(fill, "pre-wired FILL hits must appear");
        assert!(cond, "Ratio/NotFill conditions must appear");
        assert!(plock, "p-locks must appear");
        assert!(micro, "micro nudges must appear");
        assert!(ramp, "ratchet ramps must appear");
        assert!(prob, "sub-100 probability must appear");
    }

    #[test]
    fn fill_extras_sound_only_while_fill_is_held() {
        // The headline: a motion-heavy dig arrives with its fill built. Find a
        // dig carrying a Fill step, play one bar with and without FILL held —
        // the held pass must add hits on that lane.
        let t = terrain("disco").unwrap();
        let k = DigKnobs { motion: 1.0, ..DigKnobs::default() };
        let mut found = None;
        'scan: for seed in 0..40u32 {
            let p = dig_one(t, &k, seed);
            for tr in 0..MAX_TRACKS {
                for s in 0..DIG_STEPS {
                    let st = &p.tracks[tr].steps[s];
                    if st.on && st.condition == TrigCondition::Fill {
                        found = Some((p, tr));
                        break 'scan;
                    }
                }
            }
        }
        let (p, tr) = found.expect("full motion must write a fill extra within 40 seeds");
        let count = |fill: bool| {
            let mut state = SeqState::default();
            state.patterns[0] = p;
            state.current = 0;
            let mut seq = Sequencer::new();
            seq.import(&state);
            seq.set_fill(fill);
            seq.set_playing(true);
            let sr = 48_000.0;
            let bar = (4.0 * 0.5 * sr) as usize;
            seq.process_block(0.0, 120.0, sr, bar);
            seq.pending().iter().filter(|g| g.track as usize == tr).count()
        };
        assert!(
            count(true) > count(false),
            "holding FILL must add the pre-wired hits on lane {tr}"
        );
    }

    #[test]
    fn splice_and_breed_are_lane_coherent() {
        let t = terrain("techno").unwrap();
        let k = DigKnobs::default();
        let a = dig_one(t, &k, 1);
        let b = dig_one(t, &k, 2);
        let s = splice(&a, &b, 0x5EED);
        let mut from_a = 0;
        let mut from_b = 0;
        for tr in 0..MAX_TRACKS {
            if s.tracks[tr] == a.tracks[tr] {
                from_a += 1;
            } else {
                assert_eq!(s.tracks[tr], b.tracks[tr], "every lane must come wholly from a parent");
                from_b += 1;
            }
        }
        assert!(from_a > 0 && from_b > 0, "both parents should contribute (a={from_a} b={from_b})");

        // Breed: locked lanes force the parent's on every offspring; deterministic.
        let mut locks = [false; MAX_TRACKS];
        locks[0] = true;
        let kids = dig_best_breed(t, &k, &a, &locks, 0xFA_B1E5, N_CANDIDATES);
        let kids2 = dig_best_breed(t, &k, &a, &locks, 0xFA_B1E5, N_CANDIDATES);
        assert!(kids.iter().zip(&kids2).all(|(x, y)| x.seed == y.seed && x.pattern == y.pattern));
        for c in &kids {
            assert_eq!(c.pattern.tracks[0], a.tracks[0], "bred offspring keep the locked kick");
        }
    }

    #[test]
    fn terrains_speak_their_dialects() {
        // Character tests at WILD = 0 + MOTION = 0 (pure priors: a 0-prior cell
        // is a GUARANTEED rest and no fill extras appear, so dialect signatures
        // are structural, not luck). Deterministic: fixed base seeds, top-of-48.
        let k = DigKnobs { wild: 0.0, motion: 0.0, ..DigKnobs::default() };
        let top = |t: &Terrain, seed: u32| dig_best(t, &k, seed, 1).remove(0).pattern;

        // DISCO: four-on-the-floor + THE off-8th open hat.
        let p = top(&DISCO, 0x00D1_5C00);
        let quarters = [0, 4, 8, 12].iter().filter(|&&s| p.tracks[0].steps[s].on).count();
        assert!(quarters >= 3, "disco needs its four-on-the-floor (got {quarters})");
        let open_off8 = [2, 6, 10, 14].iter().filter(|&&s| p.tracks[6].steps[s].on).count();
        assert!(open_off8 >= 2, "disco needs the offbeat open hats (got {open_off8})");

        // HALFTIME: the backbeat lands on step 8 ONLY — 4 and 12 are silent by
        // authoring (prior 0 -> guaranteed at wild 0).
        let p = top(&HALFTIME, 0x000A_1F00);
        assert!(p.tracks[2].steps[8].on || p.tracks[3].steps[8].on, "halftime backbeat on beat 3");
        for s in [4, 12] {
            assert!(
                !p.tracks[2].steps[s].on && !p.tracks[3].steps[s].on,
                "halftime must NOT backbeat on step {s}"
            );
        }

        // FOOTWORK: the 3-3-2 kick lattice.
        let p = top(&FOOTWORK, 0x00F0_0700);
        assert!(p.tracks[0].steps[0].on, "footwork anchors the downbeat");
        let lattice = [3, 6, 10, 13].iter().filter(|&&s| p.tracks[0].steps[s].on).count();
        assert!(lattice >= 2, "footwork needs its syncopated kick lattice (got {lattice})");

        // CAVERN: sparse, toms carry it, no hat wall.
        let p = top(&CAVERN, 0x00CA_0E00);
        let total: usize =
            (0..MAX_TRACKS).map(|t| (0..DIG_STEPS).filter(|&s| p.tracks[t].steps[s].on).count()).sum();
        assert!(total <= 20, "cavern must stay sparse (got {total} hits)");
        let toms = (0..DIG_STEPS)
            .filter(|&s| p.tracks[8].steps[s].on || p.tracks[9].steps[s].on)
            .count();
        assert!(toms >= 2, "cavern toms carry the groove (got {toms})");
        let chat = (0..DIG_STEPS).filter(|&s| p.tracks[5].steps[s].on).count();
        assert!(chat <= 4, "cavern must not grow a hat wall (got {chat})");

        // OUTRUN: the big gated backbeat on 4 AND 12.
        let p = top(&OUTRUN_T, 0x0427_0000);
        assert!(
            p.tracks[2].steps[4].on && p.tracks[2].steps[12].on,
            "outrun needs the gated snare on both backbeats"
        );
    }

    #[test]
    fn every_factory_world_has_a_terrain_dialect() {
        // THIS WORLD must resolve every factory kit to a REGISTERED terrain.
        for kit in crate::kits::FACTORY_KITS {
            let t = terrain_for_world(kit.id);
            assert!(
                TERRAINS.iter().any(|x| std::ptr::eq(*x, t)),
                "world {} maps to an unregistered terrain",
                kit.id
            );
        }
        assert_eq!(terrain_for_world("discotheque").id, "disco");
        assert_eq!(terrain_for_world("marseille").id, "halftime");
        assert_eq!(terrain_for_world("bladerunner").id, "cavern");
        assert_eq!(terrain_for_world("outrun").id, "outrun");
        assert_eq!(terrain_for_world("neutral").id, "techno", "the bare machine speaks techno");
        assert_eq!(terrain_for_world("garbage").id, "techno", "unknown ids fall back safely");
    }

    #[test]
    fn terrain_registry_is_coherent() {
        let mut ids: Vec<&str> = TERRAINS.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), TERRAINS.len(), "terrain ids must be unique");
        assert!(terrain("techno").is_some());
        assert!(terrain("nope").is_none());
        for t in TERRAINS {
            assert!((50..=75).contains(&t.swing));
            assert!(t.humanize <= 100);
            assert!(t.lanes.iter().any(|l| l.role == Role::Anchor && l.activity > 0.5), "{}: needs an anchor lane", t.id);
        }
    }
}
