//! `DrumKit` — the 12-track voice rack with per-voice tails, choke groups, and
//! the shared dynamics bus (design §3.1/§3.2/§5.3). Signal flow per track:
//! voice engine → `VoiceTail` (drive → CS-80 filter → level → pan) → kit sum →
//! `DrumBus` (glue → true-peak limiter) → output. M3's Neutral kit voices Kick,
//! Snare, Clap and Closed/Open Hat at their design indices; the other 7 tracks
//! are `Silent` until M4.

use crate::bus::{BusSends, DrumBus};
use crate::drift;
use crate::mod_matrix::{DrumModDest, DrumModMatrix, DrumModSource, ModGlobals, N_DRUM_DESTS, N_DRUM_SOURCES};
use crate::plock::{LockableParam, PLock, N_TAIL_PARAMS};
use crate::tail::VoiceTail;
use crate::voice::{
    ClapVoice, CowbellVoice, HatVoice, KickVoice, RimVoice, SnareVoice, TomVoice, Voice, ZapVoice,
};
use crate::MAX_TRACKS;

/// A track's default (patch) values for the lockable tail params. A p-lock
/// overrides one of these for a single hit; the next unlocked hit restores them.
#[derive(Clone, Copy)]
struct TrackBase {
    level: f32,
    pan: f32,
    cutoff: f32,
    resonance: f32,
    drive_amt: f32,
    drive_on: bool,
}

/// A kit's per-voice **patch**: each track's default values for the lockable
/// tail params (Level, Pan, Cutoff, Resonance, Drive), in `LockableParam` index
/// order. This is what the GUI VOICE editor edits and what the host project
/// persists. Stored in **engine units** (a lossless `f32` copy of `base[]`), so
/// `import_patch` of a `Default` patch reproduces `DrumKit::neutral`'s base
/// exactly — the default render stays byte-identical with no special-casing.
/// (The GUI wire protocol stays normalized `0..1`; the plugin normalizes on the
/// way out and `set_voice_param` denormalizes on the way in.)
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VoicePatch {
    pub tracks: [[f32; N_TAIL_PARAMS]; MAX_TRACKS],
}

impl Default for VoicePatch {
    fn default() -> Self {
        // Derive from the Neutral kit so the defaults never drift from neutral().
        // (base[] is sample-rate independent, so any rate yields the same patch.)
        let mut p = Self { tracks: [[0.0; N_TAIL_PARAMS]; MAX_TRACKS] };
        DrumKit::neutral(48_000.0).export_patch_into(&mut p);
        p
    }
}

/// One track's MIX-strip state: the two aux sends + mute/solo. Level/Pan are NOT
/// here — they live in the VOICE patch (`base[]`), shared so the MIX fader/pan
/// and the VOICE editor edit one source of truth. `Default` is fully neutral
/// (no sends, unmuted, unsoloed), so importing a default mix is a no-op.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VoiceMixRow {
    pub send_a: f32, // -> reverb send, 0..1
    pub send_b: f32, // -> delay send, 0..1
    pub mute: bool,
    pub solo: bool,
    /// Route this voice's Send A to the GATED reverb (80s gated-verb) instead of
    /// the normal reverb return.
    #[cfg_attr(feature = "serde", serde(default))]
    pub gated_verb: bool,
    /// Choke group: 0 = none, 1..=4 = groups A..D. Triggering a voice chokes
    /// other sounding voices sharing its group (e.g. closed hat chokes open hat).
    #[cfg_attr(feature = "serde", serde(default))]
    pub choke_group: u8,
    /// 2-band trim EQ shelf gains, dB (0 = flat). Low shelf + high shelf.
    #[cfg_attr(feature = "serde", serde(default))]
    pub eq_low_db: f32,
    #[cfg_attr(feature = "serde", serde(default))]
    pub eq_high_db: f32,
    /// Output routing: 0 = Main bus (default), 1..=N_AUX = an aux stem pair.
    #[cfg_attr(feature = "serde", serde(default))]
    pub output: u8,
    /// Analog-drift amount, 0..1 (0 = off): scales the seeded per-hit pitch +
    /// level wander applied at trigger.
    #[cfg_attr(feature = "serde", serde(default))]
    pub drift: f32,
}

/// Number of auxiliary stereo stem outputs (the per-voice "output" picker targets
/// Main or one of these). Kept small + host/auval-friendly; matches the plugin's
/// declared aux output ports.
pub const N_AUX: usize = 4;

/// Trim-EQ shelf gain range: a normalized `0..1` slider maps to ±`EQ_DB_RANGE`.
const EQ_DB_RANGE: f32 = 12.0;

/// Below this, a send snaps to exactly 0.0 so the persisted value, the bus
/// engage gate, and the per-sample render tap all agree on "off" (no sub-
/// threshold send that routes nothing yet reports as engaged).
const SEND_FLOOR: f32 = 0.0001;

impl VoiceMixRow {
    /// Apply one MIX edit: `field` 0 = Send A, 1 = Send B, 2 = mute, 3 = solo,
    /// 4 = gated_verb (bools as `> 0.5`), 5 = choke_group (0..=4), 6/7 = EQ low/
    /// high shelf (normalized 0..1, 0.5 = flat). The single source of this mapping
    /// — shared by the audio-thread kit update and the editor-thread persist
    /// write, so they can't drift.
    pub fn set(&mut self, field: u8, value: f32) {
        match field {
            0 => self.send_a = Self::snap_send(value),
            1 => self.send_b = Self::snap_send(value),
            2 => self.mute = value > 0.5,
            3 => self.solo = value > 0.5,
            4 => self.gated_verb = value > 0.5,
            5 => self.choke_group = value.clamp(0.0, 4.0).round() as u8,
            // EQ: normalized 0..1 slider (0.5 = flat) -> ±EQ_DB_RANGE dB.
            6 => self.eq_low_db = (value.clamp(0.0, 1.0) - 0.5) * 2.0 * EQ_DB_RANGE,
            7 => self.eq_high_db = (value.clamp(0.0, 1.0) - 0.5) * 2.0 * EQ_DB_RANGE,
            8 => self.output = value.clamp(0.0, N_AUX as f32).round() as u8,
            9 => self.drift = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn snap_send(value: f32) -> f32 {
        let v = value.clamp(0.0, 1.0);
        if v > SEND_FLOOR {
            v
        } else {
            0.0
        }
    }

    /// EQ shelf gains as normalized `0..1` (0.5 = flat) — the inverse of `set`'s
    /// fields 6/7. The single source for both the GUI seed and `voice_mix`, so
    /// the encode/decode can't drift if `EQ_DB_RANGE` is retuned.
    pub fn eq_low_norm(&self) -> f32 {
        self.eq_low_db / (2.0 * EQ_DB_RANGE) + 0.5
    }
    pub fn eq_high_norm(&self) -> f32 {
        self.eq_high_db / (2.0 * EQ_DB_RANGE) + 0.5
    }
}

/// The whole kit's MIX state, persisted with the project (mirrors `VoicePatch`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VoiceMix {
    pub tracks: [VoiceMixRow; MAX_TRACKS],
}

impl Default for VoiceMix {
    fn default() -> Self {
        // Derive from the Neutral kit so the default mix carries neutral()'s
        // choke groups (closed+open hat = group 1). Otherwise an un-edited
        // reload (which imports the default mix) would drop the hat choke.
        let mut m = Self { tracks: [VoiceMixRow::default(); MAX_TRACKS] };
        DrumKit::neutral(48_000.0).export_mix_into(&mut m);
        m
    }
}

/// Default track layout (design §3.2): index -> role.
/// 0 KICK · 1 KICK2 · 2 SNARE · 3 CLAP · 4 RIM · 5 CLHAT · 6 OPHAT ·
/// 7 RIDE · 8 TOM_LO · 9 TOM_HI · 10 PERC · 11 SAMPLE
pub struct DrumKit {
    voices: [Voice; MAX_TRACKS],
    tails: [VoiceTail; MAX_TRACKS],
    /// Per-track default tail params (the p-lock restore target).
    base: [TrackBase; MAX_TRACKS],
    /// Whether a track's tail currently holds a p-lock override (so an unlocked
    /// hit knows to restore the base). Keeps unlocked playback byte-identical.
    tail_dirty: [bool; MAX_TRACKS],
    /// Per-track MIX state (sends, mute/solo, gated-verb, choke group). Level/Pan
    /// stay in `base[]`; the choke group lives here so it's editable + persisted.
    mix: [VoiceMixRow; MAX_TRACKS],
    /// Cached "is any track soloed?" so `render` needn't scan 12 tracks/sample.
    any_solo: bool,
    /// The per-voice mod matrix (M6). Default all-Off → zero modulation → every
    /// golden render is byte-identical. Evaluated once per hit in `trigger_impl`.
    mod_matrix: DrumModMatrix,
    /// Block-rate global mod-source values (LFOs/env/wheel/macros) the plugin
    /// pushes in. Default 0; the per-hit sources come from the `Trigger`.
    mod_globals: ModGlobals,
    bus: DrumBus,
    sr: f32,
}

/// Everything about one hit that varies per-trigger and feeds the voice, the
/// analog drift, and the mod matrix's per-hit sources. A live `trigger` fills
/// only velocity/accent (the rest default to 0); the sequencer fills all of it
/// from the `Trigger`.
#[derive(Clone, Copy, Default)]
struct HitParams {
    velocity: f32,
    accent: bool,
    rand_pitch: f32,
    rand_level: f32,
    rand_mod: f32,
    bar_phase: f32,
    step_pos: f32,
}

/// The mod contribution to the per-voice tail params, already scaled into
/// engineering units. Folded into `apply_plocks` on top of base + p-lock. All
/// zero (the `Default`) when nothing routes to the tail — the byte-exact path.
#[derive(Clone, Copy, Default)]
struct ModTailOffsets {
    level: f32,      // linear gain offset
    pan: f32,        // -1..+1 offset
    cutoff_oct: f32, // octaves (multiplies the Hz cutoff)
    resonance: f32,  // knob-unit offset
    drive: f32,      // 0..1 offset
}

impl ModTailOffsets {
    /// Extract the tail destinations from the matrix accumulator (scaled).
    fn from_acc(acc: [f32; N_DRUM_DESTS]) -> Self {
        Self {
            level: acc[DrumModDest::Level.index()] * DrumModDest::Level.scale(),
            pan: acc[DrumModDest::Pan.index()] * DrumModDest::Pan.scale(),
            cutoff_oct: acc[DrumModDest::Cutoff.index()] * DrumModDest::Cutoff.scale(),
            resonance: acc[DrumModDest::Resonance.index()] * DrumModDest::Resonance.scale(),
            drive: acc[DrumModDest::Drive.index()] * DrumModDest::Drive.scale(),
        }
    }

    fn any(&self) -> bool {
        self.level != 0.0
            || self.pan != 0.0
            || self.cutoff_oct != 0.0
            || self.resonance != 0.0
            || self.drive != 0.0
    }
}

/// Scan a step's p-locks for the voice-engine locks (Pitch, Decay) and return
/// `(pitch cents, decay scale)`. Absent or centered -> `(0.0, 1.0)`, a no-op.
/// These route to the voice hooks, not the tail, so `apply_plocks` skips them.
fn pitch_decay_plocks(plocks: &[PLock]) -> (f32, f32) {
    let mut cents = 0.0;
    let mut decay = 1.0;
    for pl in plocks {
        match LockableParam::from_index(pl.param) {
            Some(LockableParam::Pitch) => cents = LockableParam::Pitch.denormalize(pl.value),
            Some(LockableParam::Decay) => decay = LockableParam::Decay.denormalize(pl.value),
            _ => {}
        }
    }
    (cents, decay)
}

impl DrumKit {
    /// The Neutral kit — the bit-exact golden anchor (frozen at M3).
    pub fn neutral(sr: f32) -> Self {
        let mut voices: [Voice; MAX_TRACKS] = core::array::from_fn(|_| Voice::Silent);
        voices[0] = Voice::Kick(KickVoice::neutral(sr)); // KICK
        voices[1] = Voice::Kick(KickVoice::sub(sr)); // KICK2 / SUB
        voices[2] = Voice::Snare(SnareVoice::neutral(sr)); // SNARE
        voices[3] = Voice::Clap(ClapVoice::neutral(sr)); // CLAP
        voices[4] = Voice::Rim(RimVoice::neutral(sr)); // RIM
        voices[5] = Voice::Hat(HatVoice::closed(sr)); // CLHAT
        voices[6] = Voice::Hat(HatVoice::open(sr)); // OPHAT
        voices[7] = Voice::Hat(HatVoice::ride(sr)); // RIDE
        voices[8] = Voice::Tom(TomVoice::low(sr)); // TOM_LO
        voices[9] = Voice::Tom(TomVoice::high(sr)); // TOM_HI
        voices[10] = Voice::Cowbell(CowbellVoice::neutral(sr)); // PERC / COWBELL
        voices[11] = Voice::Zap(ZapVoice::neutral(sr)); // SAMPLE / FX (zap until samples land)

        let mut tails: [VoiceTail; MAX_TRACKS] = core::array::from_fn(|_| VoiceTail::new(sr));
        // Per-voice Neutral balance + a touch of tail character. Filters default
        // wide (transparent) except the kicks, which get a gentle HP. Levels
        // balance the 12-track kit before the glue bus.
        tails[0].set_level(1.00); // KICK
        tails[0].set_filter(true, 28.0, 20_000.0, 0.0);
        tails[1].set_level(0.85); // KICK2 / SUB
        tails[1].set_filter(true, 22.0, 8_000.0, 0.0);
        tails[2].set_level(0.92); // SNARE
        tails[3].set_level(0.80); // CLAP
        tails[4].set_level(1.20); // RIM — a present side-stick accent
        tails[4].set_pan(-0.25);
        tails[5].set_level(0.62); // CLHAT
        tails[6].set_level(0.58); // OPHAT
        tails[7].set_level(0.42); // RIDE
        tails[7].set_pan(0.25);
        tails[8].set_level(0.78); // TOM_LO
        tails[8].set_pan(-0.4);
        tails[9].set_level(0.78); // TOM_HI
        tails[9].set_pan(0.4);
        tails[10].set_level(0.55); // PERC / COWBELL
        tails[10].set_pan(0.3);
        tails[11].set_level(0.6); // SAMPLE / FX

        // Choke groups live in the MIX state now (editable + persisted).
        let mut mix = [VoiceMixRow::default(); MAX_TRACKS];
        mix[5].choke_group = 1; // closed hat -> group A
        mix[6].choke_group = 1; // open hat   -> group A (closed chokes open)

        // Capture each track's configured tail values as the p-lock restore base.
        let base: [TrackBase; MAX_TRACKS] = core::array::from_fn(|t| TrackBase {
            level: tails[t].level(),
            pan: tails[t].pan(),
            cutoff: tails[t].lp_cutoff(),
            resonance: tails[t].resonance(),
            drive_amt: tails[t].drive_amount(),
            drive_on: tails[t].drive_on(),
        });

        Self {
            voices,
            tails,
            base,
            tail_dirty: [false; MAX_TRACKS],
            mix,
            any_solo: false,
            mod_matrix: DrumModMatrix::new(),
            mod_globals: ModGlobals::default(),
            bus: DrumBus::neutral(sr),
            sr,
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sr
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        for v in &mut self.voices {
            v.set_sample_rate(sr);
        }
        for t in &mut self.tails {
            t.set_sample_rate(sr);
        }
        self.bus.set_sample_rate(sr);
    }

    /// Trigger a track for a live hit (pad/MIDI, tests, goldens): no drift and no
    /// seeded mod sources, but global mod (LFOs/macros) + Velocity/Accent routes
    /// still apply if the matrix is wired.
    pub fn trigger(&mut self, track: usize, velocity: f32, accent: bool, plocks: &[PLock]) {
        self.trigger_impl(track, plocks, HitParams { velocity, accent, ..HitParams::default() });
    }

    /// Trigger a sequencer hit from its `Trigger`, applying seeded per-hit analog
    /// **drift** + the mod matrix (the Trigger carries the per-hit mod sources:
    /// drift randoms, RandomPerHit, bar-phase, step-position).
    pub fn trigger_seq(&mut self, trg: &crate::Trigger) {
        self.trigger_impl(
            trg.track as usize,
            trg.plocks(),
            HitParams {
                velocity: trg.velocity,
                accent: trg.accent,
                rand_pitch: trg.rand_pitch,
                rand_level: trg.rand_level,
                rand_mod: trg.rand_mod,
                bar_phase: trg.bar_phase,
                step_pos: trg.step_pos,
            },
        );
    }

    /// Applies the mod matrix + per-step p-locks to the tail, the choke broadcast
    /// (allocation-free, O(12)), drift, then triggers the voice.
    fn trigger_impl(&mut self, track: usize, plocks: &[PLock], hit: HitParams) {
        if track >= MAX_TRACKS {
            return;
        }
        // Mod matrix (M6): evaluate once per hit, but ONLY if any route is wired.
        // An all-Off matrix returns None, so the tail offsets stay zero and the
        // p-lock fast path / pitch hook run the identical float ops as before —
        // every golden render is byte-identical at the default.
        let mod_acc = self.compute_mod_acc(track, &hit);
        let mod_tail = mod_acc.map(ModTailOffsets::from_acc).unwrap_or_default();
        self.apply_plocks(track, plocks, mod_tail);

        let g = self.mix[track].choke_group;
        if g != 0 {
            for i in 0..MAX_TRACKS {
                if i != track && self.mix[i].choke_group == g {
                    self.voices[i].choke();
                }
            }
        }

        // Pitch: drift cents + the matrix Pitch dest, summed into the one hook
        // (set EVERY trigger so no stale detune lingers; 0 cents -> 2^0 = 1.0, a
        // bit-exact no-op). Level: drift's velocity scale (the matrix Level dest
        // is a tail VCA, already folded in apply_plocks).
        // Pitch/Decay p-locks resolve to a per-hit voice offset (not the tail):
        // Pitch in cents (summed with drift + the matrix), Decay a multiplicative
        // scale (multiplied with the matrix AmpDecay). Centered locks (0 cents /
        // unity) are no-ops, so an unlocked hit is bit-identical.
        let (plock_cents, plock_decay) = pitch_decay_plocks(plocks);

        let drift = self.mix[track].drift;
        let drift_cents = hit.rand_pitch * drift * drift::PITCH_CENTS_FULL;
        let mod_cents = mod_acc
            .map(|a| a[DrumModDest::Pitch.index()] * DrumModDest::Pitch.scale())
            .unwrap_or(0.0);
        self.voices[track].set_pitch_drift_cents(drift_cents + mod_cents + plock_cents);

        // AmpDecay: a multiplicative decay scale (octaves), `1.0` when unmodded.
        // Set every hit so a cleared route restores; the engine no-ops at 1.0.
        // Clamp the octaves to the documented +/-2 (0.25x..4x) so stacked routes
        // can't drive the decay to a runaway / stuck tail (every other dest is
        // range-clamped too — Cutoff at its Hz bounds, Level/Pan/Res/Drive below).
        let decay_scale = mod_acc
            .map(|a| {
                let oct = (a[DrumModDest::AmpDecay.index()] * DrumModDest::AmpDecay.scale()).clamp(-2.0, 2.0);
                2.0_f32.powf(oct)
            })
            .unwrap_or(1.0);
        self.voices[track].set_decay_mod(decay_scale * plock_decay);

        let mut velocity = hit.velocity;
        if drift != 0.0 {
            let level_mult = 1.0 + hit.rand_level * drift * drift::LEVEL_PCT_FULL;
            velocity = (velocity * level_mult).clamp(0.0, 1.0);
        }
        self.voices[track].trigger(velocity, hit.accent);
    }

    /// Build the source array (per-hit values from `hit`, globals from kit state)
    /// and accumulate the matrix for this track. `None` when nothing is wired —
    /// the caller then runs the exact pre-M6 path.
    fn compute_mod_acc(&self, track: usize, hit: &HitParams) -> Option<[f32; N_DRUM_DESTS]> {
        if !self.mod_matrix.any_active() {
            return None;
        }
        let mut s = [0.0f32; N_DRUM_SOURCES];
        s[DrumModSource::Velocity.index()] = hit.velocity;
        s[DrumModSource::Accent.index()] = if hit.accent { 1.0 } else { 0.0 };
        s[DrumModSource::Lfo1.index()] = self.mod_globals.lfo1;
        s[DrumModSource::Lfo2.index()] = self.mod_globals.lfo2;
        s[DrumModSource::ModEnv.index()] = self.mod_globals.mod_env;
        // PitchEnv/AmpEnv (indices reserved) are not yet produced -> 0.
        s[DrumModSource::Trigger.index()] = 1.0; // a "this hit happened" gate.
        s[DrumModSource::RandomPerHit.index()] = hit.rand_mod;
        s[DrumModSource::BarPhase.index()] = hit.bar_phase;
        s[DrumModSource::StepPosition.index()] = hit.step_pos;
        s[DrumModSource::ModWheel.index()] = self.mod_globals.mod_wheel;
        for (k, &m) in self.mod_globals.macros.iter().enumerate() {
            s[DrumModSource::Macro1.index() + k] = m;
        }
        let mut acc = [0.0f32; N_DRUM_DESTS];
        self.mod_matrix.accumulate(track, &s, &mut acc);
        Some(acc)
    }

    /// Set one mod-matrix slot's routing (the GUI / preset pushes these). `i` is
    /// `0..16`; depth clamps to `-1..+1`; `target_voice` is `ALL_VOICES` or a
    /// track index. An all-Off matrix is the byte-exact default.
    pub fn set_mod_slot(&mut self, i: usize, src: DrumModSource, dst: DrumModDest, depth: f32, target_voice: u8) {
        self.mod_matrix.set_slot(i, src, dst, depth, target_voice);
    }

    /// Push the block-rate global mod-source values (LFO1/LFO2/mod-env outputs).
    pub fn set_mod_globals(&mut self, lfo1: f32, lfo2: f32, mod_env: f32) {
        self.mod_globals.lfo1 = lfo1;
        self.mod_globals.lfo2 = lfo2;
        self.mod_globals.mod_env = mod_env;
    }

    /// Push the mod-wheel (CC1, `0..1`) value.
    pub fn set_mod_wheel(&mut self, v: f32) {
        self.mod_globals.mod_wheel = v;
    }

    /// Push the 8 macro-knob (K1–K8, `0..1`) values.
    pub fn set_macros(&mut self, macros: [f32; 8]) {
        self.mod_globals.macros = macros;
    }

    /// Resolve (base + p-lock overrides) onto the track's tail. The dirty flag
    /// lets an unlocked hit on an unlocked tail skip all work — so default
    /// (no-p-lock) playback never touches the tail and stays bit-identical.
    fn apply_plocks(&mut self, track: usize, plocks: &[PLock], mod_tail: ModTailOffsets) {
        // Fast path: unlocked hit, clean tail, no tail modulation -> the tail
        // already equals the patch default, so touch nothing (byte-identical).
        let modded = mod_tail.any();
        if plocks.is_empty() && !self.tail_dirty[track] && !modded {
            return;
        }
        let b = self.base[track];
        let mut level = b.level;
        let mut pan = b.pan;
        let mut cutoff = b.cutoff;
        let mut resonance = b.resonance;
        let mut drive_amt = b.drive_amt;
        let mut drive_on = b.drive_on;
        for pl in plocks {
            if let Some(param) = LockableParam::from_index(pl.param) {
                let v = param.denormalize(pl.value);
                match param {
                    LockableParam::Level => level = v,
                    LockableParam::Pan => pan = v,
                    LockableParam::Cutoff => cutoff = v,
                    LockableParam::Resonance => resonance = v,
                    LockableParam::Drive => {
                        drive_amt = v;
                        drive_on = v > 0.001;
                    }
                    // Pitch/Decay are voice-engine locks, not tail params — they
                    // route to the voice hooks in trigger_impl, not here.
                    LockableParam::Pitch | LockableParam::Decay => {}
                }
            }
        }
        // Fold the matrix tail offsets on top of base + p-lock (additive, then
        // clamped to each param's valid range). Cutoff modulates in octaves. The
        // Drive on/off gate follows the modulated amount only when Drive is
        // actually routed, so an unmodulated Drive keeps its p-lock/base state.
        if modded {
            level = (level + mod_tail.level).max(0.0);
            pan = (pan + mod_tail.pan).clamp(-1.0, 1.0);
            cutoff = (cutoff * 2.0_f32.powf(mod_tail.cutoff_oct)).clamp(20.0, 20_000.0);
            resonance = (resonance + mod_tail.resonance).clamp(0.0, 1.0);
            if mod_tail.drive != 0.0 {
                drive_amt = (drive_amt + mod_tail.drive).clamp(0.0, 1.0);
                drive_on = drive_amt > 0.001;
            }
        }
        let tail = &mut self.tails[track];
        tail.set_level(level);
        tail.set_pan(pan);
        tail.set_lp_cutoff(cutoff);
        tail.set_resonance(resonance);
        tail.set_drive_amount(drive_on, drive_amt);
        // Tail now holds an override (p-lock or mod), so the next plain hit must
        // restore base — mirror the p-lock dirty contract for the mod case too.
        self.tail_dirty[track] = !plocks.is_empty() || modded;
    }

    /// Write a track's tail from its current `base` defaults and clear its dirty
    /// flag — the tail now equals the patch default, so the next unlocked hit
    /// needs no restore and the `apply_plocks` fast path stays valid.
    fn seed_tail_from_base(&mut self, track: usize) {
        let b = self.base[track];
        let tail = &mut self.tails[track];
        tail.set_level(b.level);
        tail.set_pan(b.pan);
        tail.set_lp_cutoff(b.cutoff);
        tail.set_resonance(b.resonance);
        tail.set_drive_amount(b.drive_on, b.drive_amt);
        self.tail_dirty[track] = false;
    }

    /// Set a track's DEFAULT value for one lockable tail param — the patch the
    /// GUI VOICE editor edits. `norm` is the same normalized `0..1` the p-lock
    /// layer uses. Updates the restore base and re-seeds the tail so the change
    /// is audible immediately (and `base == tail`, so no p-lock dirtying).
    pub fn set_voice_param(&mut self, track: usize, param: u16, norm: f32) {
        if track >= MAX_TRACKS {
            return;
        }
        let Some(p) = LockableParam::from_index(param) else {
            return;
        };
        let v = p.denormalize(norm);
        let b = &mut self.base[track];
        match p {
            LockableParam::Level => b.level = v,
            LockableParam::Pan => b.pan = v,
            LockableParam::Cutoff => b.cutoff = v,
            LockableParam::Resonance => b.resonance = v,
            LockableParam::Drive => {
                b.drive_amt = v;
                b.drive_on = v > 0.001; // same threshold as apply_plocks
            }
            // Pitch/Decay have no per-voice tail default (p-lock-only in v1).
            LockableParam::Pitch | LockableParam::Decay => return,
        }
        self.seed_tail_from_base(track);
    }

    /// Snapshot every track's patch defaults (engine units) for persistence.
    /// Allocation-free — writes into the caller's array.
    pub fn export_patch_into(&self, patch: &mut VoicePatch) {
        for t in 0..MAX_TRACKS {
            let b = self.base[t];
            patch.tracks[t] = [b.level, b.pan, b.cutoff, b.resonance, b.drive_amt];
        }
    }

    /// Adopt a persisted patch (host project load): write every track's defaults
    /// and re-seed its tail. A `Default` patch reproduces `neutral()` exactly.
    pub fn import_patch(&mut self, patch: &VoicePatch) {
        for t in 0..MAX_TRACKS {
            let v = patch.tracks[t];
            let b = &mut self.base[t];
            b.level = v[0];
            b.pan = v[1];
            b.cutoff = v[2];
            b.resonance = v[3];
            b.drive_amt = v[4];
            b.drive_on = v[4] > 0.001;
            self.seed_tail_from_base(t);
        }
    }

    /// A track's current patch default for one lockable param, normalized `0..1`
    /// (for seeding the GUI VOICE editor).
    pub fn voice_param(&self, track: usize, param: u16) -> f32 {
        if track >= MAX_TRACKS {
            return 0.0;
        }
        let Some(p) = LockableParam::from_index(param) else {
            return 0.0;
        };
        let b = self.base[track];
        let eng = match p {
            LockableParam::Level => b.level,
            LockableParam::Pan => b.pan,
            LockableParam::Cutoff => b.cutoff,
            LockableParam::Resonance => b.resonance,
            LockableParam::Drive => b.drive_amt,
            // Pitch/Decay have no per-voice default; report the unity center.
            LockableParam::Pitch | LockableParam::Decay => return 0.5,
        };
        p.normalize(eng)
    }

    /// Recompute cached mix flags + tell the bus which sends are in use (so the
    /// reverb/delay returns run + tail while any voice routes to them).
    fn recompute_mix_flags(&mut self) {
        self.any_solo = self.mix.iter().any(|m| m.solo);
        // Sends are snapped to 0 below SEND_FLOOR, so `> 0.0` matches the gate.
        // Send A splits by gated_verb: normal reverb vs the gated reverb return.
        let any_a = self.mix.iter().any(|m| !m.gated_verb && m.send_a > 0.0);
        let any_gated = self.mix.iter().any(|m| m.gated_verb && m.send_a > 0.0);
        let any_b = self.mix.iter().any(|m| m.send_b > 0.0);
        self.bus.set_send_active(any_a, any_gated, any_b);
    }

    /// Set a track's MIX-strip value. `field`: 0 = Send A, 1 = Send B, 2 = mute,
    /// 3 = solo, 4 = gated_verb, 5 = choke_group, 6 = EQ low, 7 = EQ high.
    /// Sends/mute/solo/gated-verb/choke/EQ, not Level/Pan (those are VOICE patch
    /// params via `set_voice_param`).
    pub fn set_voice_mix(&mut self, track: usize, field: u8, value: f32) {
        if track >= MAX_TRACKS {
            return;
        }
        self.mix[track].set(field, value);
        if field == 6 || field == 7 {
            // EQ lives on the tail; push the new shelf gains onto it.
            let m = self.mix[track];
            self.tails[track].set_eq(m.eq_low_db, m.eq_high_db);
        }
        self.recompute_mix_flags();
    }

    /// Read a track's MIX value (for seeding the GUI). Bools come back as 0/1.
    pub fn voice_mix(&self, track: usize, field: u8) -> f32 {
        if track >= MAX_TRACKS {
            return 0.0;
        }
        let m = self.mix[track];
        match field {
            0 => m.send_a,
            1 => m.send_b,
            2 => f32::from(m.mute),
            3 => f32::from(m.solo),
            4 => f32::from(m.gated_verb),
            5 => f32::from(m.choke_group),
            6 => m.eq_low_norm(),
            7 => m.eq_high_norm(),
            8 => f32::from(m.output),
            9 => m.drift,
            _ => 0.0,
        }
    }

    /// Snapshot the kit's MIX state for persistence (alloc-free).
    pub fn export_mix_into(&self, mix: &mut VoiceMix) {
        mix.tracks = self.mix;
    }

    /// Adopt a persisted MIX state (host project load).
    pub fn import_mix(&mut self, mix: &VoiceMix) {
        self.mix = mix.tracks;
        // EQ lives on the tails — push each track's persisted shelves onto them.
        for t in 0..MAX_TRACKS {
            self.tails[t].set_eq(self.mix[t].eq_low_db, self.mix[t].eq_high_db);
        }
        self.recompute_mix_flags();
    }

    /// Render the Main stereo bus with no aux stems (the golden + bare path).
    pub fn render(&mut self) -> (f32, f32) {
        self.render_into(&mut [])
    }

    /// Sum the voices through their tails (with mute/solo gain) into the Main bus
    /// + the reverb/delay send sums, OR — for a voice whose `output` targets an
    /// aux pair — into `aux[output-1]` (a RAW post-fader stem that bypasses the
    /// shared glue/limiter and leaves the Main mix + its sends). Returns the Main
    /// bus frame. At the default (all `output == 0`, gain 1.0, sends 0) this is
    /// bit-identical to a bare voice sum into the bus, and the aux slice stays 0.
    pub fn render_into(&mut self, aux: &mut [(f32, f32)]) -> (f32, f32) {
        for a in aux.iter_mut() {
            *a = (0.0, 0.0);
        }
        let mut l = 0.0;
        let mut r = 0.0;
        let mut s = BusSends::default();
        for (i, (v, t)) in self.voices.iter_mut().zip(self.tails.iter_mut()).enumerate() {
            let (vl, vr) = v.render();
            let (tl, tr) = t.process(vl, vr);
            // mute/solo (cheap gate; default 1.0)
            let m = self.mix[i];
            let g = if m.mute || (self.any_solo && !m.solo) { 0.0 } else { 1.0 };
            let tl = tl * g;
            let tr = tr * g;
            // output 0 = Main; 1..=N routes to aux[output-1] IF that stem exists.
            // An out-of-range index (fewer aux buses than expected) falls back to
            // Main rather than dropping the voice.
            let stem_idx = if m.output == 0 { usize::MAX } else { (m.output - 1) as usize };
            if let Some(stem) = aux.get_mut(stem_idx) {
                // Stem: raw post-fader, out of the Main mix + its sends.
                stem.0 += tl;
                stem.1 += tr;
            } else {
                // Main bus: dry sum + the post-fader sends. Send A routes to the
                // gated reverb when flagged gated_verb, else the normal reverb.
                l += tl;
                r += tr;
                let (sa_l, sa_r) = (tl * m.send_a, tr * m.send_a);
                if m.gated_verb {
                    s.gated_l += sa_l;
                    s.gated_r += sa_r;
                } else {
                    s.reverb_l += sa_l;
                    s.reverb_r += sa_r;
                }
                s.delay_l += tl * m.send_b;
                s.delay_r += tr * m.send_b;
            }
        }
        self.bus.process_with_sends(l, r, s)
    }

    /// Number of currently-sounding voices (for the GUI voice meter).
    pub fn active_voices(&self) -> u32 {
        self.voices.iter().filter(|v| v.is_active()).count() as u32
    }

    /// Whether a given track is currently sounding (for trigger LEDs).
    pub fn track_active(&self, track: usize) -> bool {
        track < MAX_TRACKS && self.voices[track].is_active()
    }

    /// Live bus gain reduction (dB), for the GUI glue meter (wired in M9).
    pub fn bus_gain_reduction_db(&self) -> f32 {
        self.bus.gain_reduction_db()
    }

    // --- bus FX (M7) ---
    /// Push the host tempo to the bus (for the beat-synced PUMP). Once per block.
    pub fn set_bus_tempo(&mut self, bpm: f32) {
        self.bus.set_tempo(bpm);
    }

    /// Sidechain PUMP depth, 0..1.
    pub fn set_pump(&mut self, amount: f32) {
        self.bus.set_pump(amount);
    }

    /// Lo-fi bus drive, 0..1.
    pub fn set_bus_drive(&mut self, amount: f32) {
        self.bus.set_drive(amount);
    }

    /// Plate reverb send, 0..1.
    pub fn set_bus_reverb(&mut self, amount: f32) {
        self.bus.set_reverb(amount);
    }

    /// Tape/stereo delay mix, 0..1.
    pub fn set_bus_delay(&mut self, amount: f32) {
        self.bus.set_delay(amount);
    }

    /// Pump rate (normalized -> note division).
    pub fn set_pump_rate(&mut self, norm: f32) {
        self.bus.set_pump_rate(norm);
    }

    /// Pump duck curve/shape, 0..1.
    pub fn set_pump_curve(&mut self, curve: f32) {
        self.bus.set_pump_curve(curve);
    }

    /// Parallel/NY compression blend, 0..1.
    pub fn set_bus_parallel(&mut self, amount: f32) {
        self.bus.set_parallel(amount);
    }

    /// Transient PUNCH (attack emphasis), 0..1.
    pub fn set_bus_transient(&mut self, amount: f32) {
        self.bus.set_transient(amount);
    }

    /// Gated-verb gate length (hold), 20..400 ms.
    pub fn set_gate_time(&mut self, ms: f32) {
        self.bus.set_gate_time(ms);
    }

    /// Pump source: `true` = host sidechain key, `false` = internal kick.
    pub fn set_pump_source_external(&mut self, external: bool) {
        self.bus.set_pump_source_external(external);
    }

    /// Feed the external sidechain key level for this sample.
    pub fn set_pump_key(&mut self, level: f32) {
        self.bus.set_pump_key(level);
    }

    /// Live pump duck gain (1.0 = open) for the GUI pump meter.
    pub fn pump_envelope(&self) -> f32 {
        self.bus.pump_envelope()
    }

    /// Panic-reset: silence every voice, clear filter/tail state, and flush the
    /// bus's audio memory (reverb/delay tails, gate) — the KIT-recall tail-cut
    /// and host reset both rely on this actually cutting EVERYTHING.
    pub fn reset(&mut self) {
        for v in &mut self.voices {
            v.reset();
        }
        for t in &mut self.tails {
            t.reset();
        }
        self.bus.reset();
    }
}

/// GM-ish note map (design §6.3): standard General MIDI drum pitches so existing
/// MIDI drum clips and host drummers "just work." Returns the track index for a
/// MIDI note, if mapped.
pub fn track_for_note(note: u8) -> Option<usize> {
    match note {
        36 => Some(0),  // C1  bass drum -> kick
        35 => Some(1),  // B0  acoustic bass drum -> sub kick
        38 => Some(2),  // D1  acoustic snare
        39 => Some(3),  // D#1 hand clap
        37 => Some(4),  // C#1 side stick -> rim
        42 => Some(5),  // F#1 closed hi-hat
        46 => Some(6),  // A#1 open hi-hat
        51 => Some(7),  // D#2 ride cymbal -> ride
        45 => Some(8),  // A1  low tom
        50 => Some(9),  // D2  high tom
        56 => Some(10), // G#2 cowbell
        60 => Some(11), // C3  hi bongo -> perc / zap
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_patch_round_trips_and_default_import_is_a_no_op() {
        // A fresh Neutral kit's exported patch equals VoicePatch::default()...
        let kit = DrumKit::neutral(48_000.0);
        let mut exported = VoicePatch { tracks: [[0.0; N_TAIL_PARAMS]; MAX_TRACKS] };
        kit.export_patch_into(&mut exported);
        assert_eq!(exported, VoicePatch::default(), "neutral export must equal the default patch");

        // ...and importing the default patch leaves base[] byte-for-byte intact
        // (the golden guard: a fresh/un-edited project changes nothing).
        let mut k1 = DrumKit::neutral(48_000.0);
        let before: Vec<_> = (0..MAX_TRACKS).map(|t| k1.base[t]).map(|b| (b.level, b.pan, b.cutoff, b.resonance, b.drive_amt, b.drive_on)).collect();
        k1.import_patch(&VoicePatch::default());
        let after: Vec<_> = (0..MAX_TRACKS).map(|t| k1.base[t]).map(|b| (b.level, b.pan, b.cutoff, b.resonance, b.drive_amt, b.drive_on)).collect();
        assert_eq!(before, after, "default-patch import must not move any base value");
    }

    #[test]
    fn voice_mix_round_trips_and_default_import_is_a_no_op() {
        // Default mix import changes nothing (the golden guard).
        let mut k = DrumKit::neutral(48_000.0);
        k.import_mix(&VoiceMix::default());
        let mut exported = VoiceMix::default();
        k.export_mix_into(&mut exported);
        assert_eq!(exported, VoiceMix::default(), "default mix must round-trip to default");

        // Set a send + mute + solo + output routing, round-trip, confirm survival.
        k.set_voice_mix(2, 0, 0.6); // snare Send A
        k.set_voice_mix(5, 2, 1.0); // closed-hat mute
        k.set_voice_mix(0, 3, 1.0); // kick solo
        k.set_voice_mix(2, 8, 3.0); // snare -> Aux 3 (output routing)
        k.set_voice_mix(0, 9, 0.7); // kick drift amount
        let mut m = VoiceMix::default();
        k.export_mix_into(&mut m);
        let mut k2 = DrumKit::neutral(48_000.0);
        k2.import_mix(&m);
        assert!((k2.voice_mix(2, 0) - 0.6).abs() < 1e-6, "send A survives");
        assert_eq!(k2.voice_mix(5, 2), 1.0, "mute survives");
        assert_eq!(k2.voice_mix(0, 3), 1.0, "solo survives");
        assert_eq!(k2.voice_mix(2, 8), 3.0, "output routing survives");
        assert!((k2.voice_mix(0, 9) - 0.7).abs() < 1e-6, "drift amount survives");
    }

    /// Build a sequencer Trigger for a drift hit (no plocks, no mod sources).
    fn seq_trig(track: u8, velocity: f32, rand_pitch: f32, rand_level: f32) -> crate::Trigger {
        crate::Trigger {
            offset: 0,
            track,
            velocity,
            accent: false,
            plocks: [PLock::default(); crate::plock::MAX_PLOCKS],
            plock_count: 0,
            rand_pitch,
            rand_level,
            rand_mod: 0.0,
            bar_phase: 0.0,
            step_pos: 0.0,
        }
    }

    #[test]
    fn drift_changes_the_hit_but_is_a_no_op_at_zero() {
        // Render N samples of a freshly-triggered voice.
        fn render_kick(k: &mut DrumKit, rand_pitch: f32, rand_level: f32) -> Vec<f32> {
            k.trigger_seq(&seq_trig(0, 1.0, rand_pitch, rand_level));
            (0..2_000).map(|_| k.render().0).collect()
        }

        // drift = 0: the seeded randoms are ignored — identical to no drift at all.
        let mut undrifted = DrumKit::neutral(48_000.0);
        let baseline = render_kick(&mut undrifted, 0.9, -0.8);
        let mut plain = DrumKit::neutral(48_000.0);
        plain.trigger(0, 1.0, false, &[]);
        let plain_buf: Vec<f32> = (0..2_000).map(|_| plain.render().0).collect();
        assert_eq!(baseline, plain_buf, "drift=0 must ignore the randoms (byte-identical to no drift)");

        // drift > 0 with a nonzero pitch random: the hit must actually change.
        let mut drifted = DrumKit::neutral(48_000.0);
        drifted.set_voice_mix(0, 9, 1.0);
        let moved = render_kick(&mut drifted, 0.9, 0.0);
        assert!(moved != baseline, "drift>0 with a pitch random must change the hit");

        // Seeded => deterministic: same amount + same randoms => bit-identical.
        let mut again = DrumKit::neutral(48_000.0);
        again.set_voice_mix(0, 9, 1.0);
        let moved2 = render_kick(&mut again, 0.9, 0.0);
        assert_eq!(moved, moved2, "same drift + same randoms must reproduce exactly");
    }

    #[test]
    fn drift_clears_when_turned_back_to_zero() {
        // After a drifted hit, turning DRIFT to 0 must NOT leave a stale detune.
        // Both kits share an IDENTICAL drifted hit 1 (so identical bus/tail state
        // — the confound), and differ only in how hit 2 clears: `k` sets DRIFT=0,
        // the reference keeps DRIFT>0 but feeds zero randoms (a clearing path
        // immune to any future re-gating of the pitch setter). A lingering stale
        // detune would make hit 2 differ.
        fn drifted_hit1(k: &mut DrumKit) {
            k.set_voice_mix(0, 9, 1.0);
            k.trigger_seq(&seq_trig(0, 1.0, 0.9, 0.0));
            for _ in 0..2_000 {
                k.render();
            }
        }
        let mut k = DrumKit::neutral(48_000.0);
        drifted_hit1(&mut k);
        k.set_voice_mix(0, 9, 0.0); // DRIFT off
        k.trigger_seq(&seq_trig(0, 1.0, 0.9, 0.0)); // big random, but amount 0
        let after: Vec<f32> = (0..2_000).map(|_| k.render().0).collect();

        let mut reference = DrumKit::neutral(48_000.0);
        drifted_hit1(&mut reference);
        reference.trigger_seq(&seq_trig(0, 1.0, 0.0, 0.0)); // amount 1, zero randoms => undrifted
        let ref_after: Vec<f32> = (0..2_000).map(|_| reference.render().0).collect();

        assert_eq!(after, ref_after, "a stale drift detune must not linger after DRIFT=0");
    }

    #[test]
    fn mod_routes_modulate_the_voice() {
        use crate::mod_matrix::{DrumModDest, DrumModSource, ALL_VOICES};
        // Each render is a FRESH kit's first hit, so the bus state at trigger time
        // is identical — any difference is the modulation, not residual state.
        fn render_kick(setup: impl FnOnce(&mut DrumKit)) -> Vec<f32> {
            let mut k = DrumKit::neutral(48_000.0);
            setup(&mut k);
            k.trigger(0, 1.0, false, &[]);
            (0..2_000).map(|_| k.render().0).collect()
        }
        let baseline = render_kick(|_| {});

        // A tail destination (Level, the VCA): Trigger source (= 1.0 at the hit)
        // -> Level, depth -0.5, all voices. A constant source -> deterministic.
        let level_mod = render_kick(|k| k.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::Level, -0.5, ALL_VOICES));
        assert!(level_mod != baseline, "a wired Level route must change the hit");
        let level_mod2 = render_kick(|k| k.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::Level, -0.5, ALL_VOICES));
        assert_eq!(level_mod, level_mod2, "a constant route must reproduce exactly");

        // The pitch destination (folded into the drift hook).
        let pitch_mod = render_kick(|k| k.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::Pitch, 0.5, ALL_VOICES));
        assert!(pitch_mod != baseline, "a wired Pitch route must change the hit");

        // A cutoff route (tail filter, octaves).
        let cut_mod = render_kick(|k| k.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::Cutoff, -1.0, ALL_VOICES));
        assert!(cut_mod != baseline, "a wired Cutoff route must change the hit");
    }

    #[test]
    fn mod_route_targeting_another_voice_leaves_this_one_byte_identical() {
        use crate::mod_matrix::{DrumModDest, DrumModSource};
        // A route scoped to voice 5 must not perturb voice 0 — byte-for-byte the
        // same as an empty matrix (the target_voice filter zeroes the accumulator,
        // so the p-lock fast path runs unchanged).
        let mut scoped = DrumKit::neutral(48_000.0);
        scoped.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::Level, -0.5, 5);
        scoped.trigger(0, 1.0, false, &[]);
        let a: Vec<f32> = (0..2_000).map(|_| scoped.render().0).collect();

        let mut empty = DrumKit::neutral(48_000.0);
        empty.trigger(0, 1.0, false, &[]);
        let b: Vec<f32> = (0..2_000).map(|_| empty.render().0).collect();
        assert_eq!(a, b, "a route targeting voice 5 must leave voice 0 byte-identical");
    }

    #[test]
    fn ampdecay_route_shortens_and_lengthens_the_tail() {
        use crate::mod_matrix::{DrumModDest, DrumModSource, ALL_VOICES};
        // Energy in the tail of a hit (samples well after onset), per voice.
        fn tail_energy(setup: impl FnOnce(&mut DrumKit), track: usize) -> f64 {
            let mut k = DrumKit::neutral(48_000.0);
            setup(&mut k);
            k.trigger(track, 1.0, false, &[]);
            let buf: Vec<f32> = (0..6_000).map(|_| k.render().0).collect();
            buf[3_000..].iter().map(|&s| (s as f64) * (s as f64)).sum()
        }
        // Trigger source (= 1.0) -> AmpDecay. +depth lengthens (4x), -depth shortens.
        let base = tail_energy(|_| {}, 0);
        let longer = tail_energy(|k| k.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::AmpDecay, 1.0, ALL_VOICES), 0);
        let shorter = tail_energy(|k| k.set_mod_slot(0, DrumModSource::Trigger, DrumModDest::AmpDecay, -1.0, ALL_VOICES), 0);
        assert!(longer > base * 1.5, "a +AmpDecay route must lengthen the tail (more late energy)");
        assert!(shorter < base * 0.5, "a -AmpDecay route must shorten the tail (less late energy)");
    }

    #[test]
    fn pitch_and_decay_plocks_change_the_hit_and_center_is_a_no_op() {
        use crate::plock::{LockableParam, PLock};
        fn render_kick(plocks: &[PLock]) -> Vec<f32> {
            let mut k = DrumKit::neutral(48_000.0);
            k.trigger(0, 1.0, false, plocks);
            (0..6_000).map(|_| k.render().0).collect()
        }
        let baseline = render_kick(&[]);

        // Centered locks (norm 0.5) denormalize to unity -> byte-identical no-op.
        let centered = render_kick(&[
            PLock { param: LockableParam::Pitch.index(), value: 0.5 },
            PLock { param: LockableParam::Decay.index(), value: 0.5 },
        ]);
        assert_eq!(centered, baseline, "centered pitch + decay locks must be a bit-exact no-op");

        // A pitch lock shifts the pitch (changes the rendered hit).
        let pitched = render_kick(&[PLock { param: LockableParam::Pitch.index(), value: 0.85 }]);
        assert!(pitched != baseline, "a pitch p-lock must change the hit");

        // A short decay lock collapses the tail.
        let decayed = render_kick(&[PLock { param: LockableParam::Decay.index(), value: 0.0 }]);
        let tail = |b: &[f32]| b[3_000..].iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
        assert!(tail(&decayed) < tail(&baseline) * 0.5, "a short decay lock must shorten the tail");
    }

    #[test]
    fn global_lfo_source_reaches_a_dest_via_set_mod_globals() {
        use crate::mod_matrix::{DrumModDest, DrumModSource, ALL_VOICES};
        // The plugin's block-rate fan-in path: set_mod_globals pushes the LFO
        // values, which the matrix reads as the Lfo1 source. Two different LFO1
        // values routed to Cutoff must produce two different hits.
        fn render_kick(lfo1: f32) -> Vec<f32> {
            let mut k = DrumKit::neutral(48_000.0);
            k.set_mod_slot(0, DrumModSource::Lfo1, DrumModDest::Cutoff, 1.0, ALL_VOICES);
            k.set_mod_globals(lfo1, 0.0, 0.0);
            k.trigger(0, 1.0, false, &[]);
            (0..2_000).map(|_| k.render().0).collect()
        }
        assert!(render_kick(0.8) != render_kick(-0.8), "the global LFO1 source must reach Cutoff via set_mod_globals");
    }

    #[test]
    fn sub_threshold_send_snaps_to_zero() {
        // Below SEND_FLOOR a send snaps to exactly 0 so the engage gate and the
        // render tap agree (no sub-threshold send that routes nothing yet engages).
        let mut k = DrumKit::neutral(48_000.0);
        k.set_voice_mix(0, 0, 0.000_05);
        assert_eq!(k.voice_mix(0, 0), 0.0, "a sub-threshold send must snap to exactly 0");
        k.set_voice_mix(0, 0, 0.5);
        assert!((k.voice_mix(0, 0) - 0.5).abs() < 1e-6, "a real send is kept");
    }

    #[test]
    fn routed_voice_goes_to_its_aux_stem_and_leaves_main() {
        let mut k = DrumKit::neutral(48_000.0);
        k.set_voice_mix(0, 8, 1.0); // kick -> Aux 1
        k.trigger(0, 1.0, true, &[]);
        let mut aux = [(0.0_f32, 0.0_f32); N_AUX];
        let mut main_e = 0.0_f32;
        let mut aux_e = 0.0_f32;
        for _ in 0..2_000 {
            let (l, r) = k.render_into(&mut aux);
            main_e += l.abs() + r.abs();
            aux_e += aux[0].0.abs() + aux[0].1.abs();
        }
        assert!(aux_e > 0.01, "a routed voice must appear on its aux stem, got {aux_e}");
        assert!(main_e < aux_e * 0.1, "a routed voice must leave the Main mix ({main_e} vs {aux_e})");
    }

    #[test]
    fn voice_eq_is_flat_by_default_and_active_when_set() {
        let render = |setup: &dyn Fn(&mut DrumKit)| {
            let mut k = DrumKit::neutral(48_000.0);
            setup(&mut k);
            k.trigger(0, 1.0, true, &[]); // kick (rich lows for the low shelf)
            (0..2_000).map(|_| k.render().0).collect::<Vec<f32>>()
        };
        let flat = render(&|_k| {});
        let boosted = render(&|k| k.set_voice_mix(0, 6, 1.0)); // +12 dB low shelf
        let diff: f32 = flat.iter().zip(&boosted).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 0.01, "engaging the EQ must change the kick output, diff={diff}");

        // EQ value round-trips through the normalized encoding.
        let mut k = DrumKit::neutral(48_000.0);
        k.set_voice_mix(2, 7, 0.75); // snare high shelf
        assert!((k.voice_mix(2, 7) - 0.75).abs() < 1e-3);
        assert_eq!(k.voice_mix(0, 6), 0.5, "an un-set EQ reads back as flat (0.5)");
    }

    #[test]
    fn choke_group_assignment_round_trips_and_keeps_neutral_hats() {
        let mut k = DrumKit::neutral(48_000.0);
        // The neutral hat choke (closed+open = group 1) is now MIX state.
        assert_eq!(k.voice_mix(5, 5), 1.0, "closed hat in group 1");
        assert_eq!(k.voice_mix(6, 5), 1.0, "open hat in group 1");
        assert_eq!(k.voice_mix(0, 5), 0.0, "kick ungrouped by default");
        // Assigning a group works and exports for persistence.
        k.set_voice_mix(0, 5, 2.0);
        assert_eq!(k.voice_mix(0, 5), 2.0);
        let mut m = VoiceMix::default();
        k.export_mix_into(&mut m);
        assert_eq!(m.tracks[0].choke_group, 2);
        assert_eq!(m.tracks[5].choke_group, 1, "neutral hat choke preserved on export");
        // The default mix carries the neutral hat choke (so an un-edited reload keeps it).
        assert_eq!(VoiceMix::default().tracks[5].choke_group, 1);
    }

    #[test]
    fn mute_and_solo_gate_the_mix() {
        // Energy assertions use silence (unambiguous) rather than relative levels,
        // since the bus glue/limiter compensates when one of two voices drops.
        let render_energy = |setup: &dyn Fn(&mut DrumKit)| {
            let mut k = DrumKit::neutral(48_000.0);
            setup(&mut k);
            k.trigger(0, 1.0, true, &[]); // kick
            k.trigger(2, 1.0, true, &[]); // snare
            let mut e = 0.0_f32;
            for _ in 0..4_000 {
                let (l, r) = k.render();
                e += l.abs() + r.abs();
            }
            e
        };
        let normal = render_energy(&|_k| {});
        assert!(normal > 0.1, "the kit sounds normally");
        // Muting every track -> silence.
        let all_muted = render_energy(&|k| {
            for t in 0..MAX_TRACKS {
                k.set_voice_mix(t, 2, 1.0);
            }
        });
        assert!(all_muted < normal * 0.02, "muting all tracks must silence the kit, got {all_muted}");
        // Soloing an untriggered track -> the triggered kick/snare are gated -> silence.
        let solo_silent = render_energy(&|k| k.set_voice_mix(7, 3, 1.0));
        assert!(solo_silent < normal * 0.02, "solo on a silent track must gate the rest, got {solo_silent}");
        // Soloing the kick keeps it clearly audible.
        let solo_kick = render_energy(&|k| k.set_voice_mix(0, 3, 1.0));
        assert!(solo_kick > normal * 0.1, "the soloed kick stays audible, got {solo_kick}");
    }

    #[test]
    fn set_voice_param_edits_the_default_and_round_trips() {
        let mut kit = DrumKit::neutral(48_000.0);
        // Edit track 2 (snare) cutoff to ~mid.
        kit.set_voice_param(2, LockableParam::Cutoff.index(), 0.6);
        let got = kit.voice_param(2, LockableParam::Cutoff.index());
        assert!((got - 0.6).abs() < 1e-4, "voice param must read back what was set, got {got}");
        // Export -> import (engine units) is lossless.
        let mut p = VoicePatch::default();
        kit.export_patch_into(&mut p);
        let mut k2 = DrumKit::neutral(48_000.0);
        k2.import_patch(&p);
        assert!((k2.voice_param(2, LockableParam::Cutoff.index()) - got).abs() < 1e-6, "patch round-trip lossless");
    }

    #[test]
    fn note_map_matches_layout() {
        assert_eq!(track_for_note(36), Some(0)); // kick
        assert_eq!(track_for_note(35), Some(1)); // sub
        assert_eq!(track_for_note(38), Some(2)); // snare
        assert_eq!(track_for_note(39), Some(3)); // clap
        assert_eq!(track_for_note(37), Some(4)); // rim
        assert_eq!(track_for_note(42), Some(5)); // closed hat
        assert_eq!(track_for_note(46), Some(6)); // open hat
        assert_eq!(track_for_note(51), Some(7)); // ride
        assert_eq!(track_for_note(45), Some(8)); // tom lo
        assert_eq!(track_for_note(50), Some(9)); // tom hi
        assert_eq!(track_for_note(56), Some(10)); // cowbell
        assert_eq!(track_for_note(60), Some(11)); // zap
        // all 12 distinct
        let mapped: std::collections::HashSet<_> =
            (0..=127).filter_map(track_for_note).collect();
        assert_eq!(mapped.len(), 12, "every track should have a distinct note");
        assert_eq!(track_for_note(100), None);
    }

    #[test]
    fn triggering_a_voice_produces_sound() {
        let mut kit = DrumKit::neutral(48_000.0);
        kit.trigger(0, 1.0, true, &[]); // kick
        let mut peak = 0.0_f32;
        for _ in 0..4_000 {
            let (l, _) = kit.render();
            peak = peak.max(l.abs());
        }
        assert!(peak > 0.2, "kick via kit+bus should be audible, peak={peak}");
        assert!(kit.active_voices() >= 1);
    }

    #[test]
    fn closed_hat_chokes_open_hat() {
        let tail_len = |choke: bool| {
            let mut kit = DrumKit::neutral(48_000.0);
            kit.trigger(6, 1.0, false, &[]); // open hat
            for _ in 0..256 {
                kit.render();
            }
            if choke {
                kit.trigger(5, 1.0, false, &[]); // closed hat chokes open
            }
            let mut n = 0;
            while kit.track_active(6) {
                kit.render();
                n += 1;
                if n > 96_000 {
                    break;
                }
            }
            n
        };
        let natural = tail_len(false);
        let choked = tail_len(true);
        assert!(
            choked < natural / 2,
            "closed hat must choke the open hat: choked={choked} natural={natural}"
        );
    }

    #[test]
    fn whole_kit_stays_finite_and_limited() {
        let mut kit = DrumKit::neutral(48_000.0);
        for t in [0usize, 2, 3, 5, 6] {
            kit.trigger(t, 1.0, true, &[]);
        }
        let mut peak = 0.0_f32;
        for _ in 0..48_000 {
            let (l, r) = kit.render();
            assert!(l.is_finite() && r.is_finite());
            peak = peak.max(l.abs()).max(r.abs());
        }
        // the true-peak limiter must keep the summed kit under ~0 dBFS
        assert!(peak <= 1.02, "bus limiter should hold the kit at ~0 dBFS, peak={peak}");
    }

    #[test]
    fn audio_hot_path_is_alloc_free() {
        use crate::{Sequencer, MAX_STEPS};
        // The exact per-block work the plugin's process() does, minus the nih-plug
        // scaffolding (~99% of the hot path): advance the sequencer, dispatch its
        // triggers, render each sample via render_into. Warm up FIRST (any lazy
        // allocation happens here), then assert ZERO heap allocations across many
        // blocks — a heap touch on the audio thread fails this.
        fn run_block(
            kit: &mut DrumKit,
            seq: &mut Sequencer,
            scratch: &mut [(f32, f32)],
            pos: f64,
            sr: f64,
        ) {
            seq.process_block(pos, 120.0, sr, 512);
            let pending = seq.pending();
            let mut ti = 0;
            for i in 0..512 {
                while ti < pending.len() {
                    let trg = pending[ti];
                    if trg.offset as usize > i {
                        break;
                    }
                    kit.trigger_seq(&trg);
                    ti += 1;
                }
                kit.render_into(scratch);
            }
        }

        let sr = 48_000.0_f64;
        let mut kit = DrumKit::neutral(sr as f32);
        let mut seq = Sequencer::new();
        for t in 0..MAX_TRACKS {
            for step in (0..MAX_STEPS).step_by(4) {
                seq.set_step(t, step, true);
            }
        }
        seq.set_playing(true);
        let mut scratch = vec![(0.0_f32, 0.0_f32); N_AUX];
        let qn_per_block = (512.0 / sr) * (120.0 / 60.0);

        // Warm-up (outside the armed region): absorb any one-time lazy allocation.
        let mut pos = 0.0;
        for _ in 0..4 {
            run_block(&mut kit, &mut seq, &mut scratch, pos, sr);
            pos += qn_per_block;
        }

        // Armed: every allocation here is a real-time-safety regression.
        let allocs = crate::rt_guard::count_allocs(|| {
            for _ in 0..128 {
                run_block(&mut kit, &mut seq, &mut scratch, pos, sr);
                pos += qn_per_block;
            }
        });
        assert_eq!(allocs, 0, "audio hot path allocated {allocs} time(s) — not real-time-safe");
    }

    #[test]
    fn choke_does_not_corrupt_the_next_hit() {
        // Regression: DahdEnv::choke permanently overwrote the decay coefficient,
        // so after the FIRST closed hat, every open hat rendered as an ~8ms tick
        // (Neutral puts them in choke group 1; the demo groove hits closed hat
        // first). A choked voice's NEXT hit must be byte-identical to a fresh one.
        fn open_hat_render(kit: &mut DrumKit) -> Vec<f32> {
            kit.voices[6].trigger(1.0, false);
            let mut out = Vec::new();
            while kit.voices[6].is_active() {
                out.push(kit.voices[6].render().0);
                assert!(out.len() < 480_000, "open hat must reach idle");
            }
            out
        }
        let mut fresh = DrumKit::neutral(48_000.0);
        let baseline = open_hat_render(&mut fresh);
        assert!(baseline.len() > 10_000, "the open hat should ring (got {} samples)", baseline.len());

        // Idle-choke: the closed hat's choke broadcast hits the never-played open hat.
        let mut k = DrumKit::neutral(48_000.0);
        k.trigger(5, 1.0, false, &[]);
        assert_eq!(open_hat_render(&mut k), baseline, "idle-choked open hat must be unharmed");

        // Live choke: open hat rings, closed hat chokes it, next open hat rings
        // its full natural length. (Ring length, not bytes: the HP filter keeps
        // its tiny residual memory across hits by design — analog-style — but the
        // env decay TIME, which the bug corrupted, is deterministic and exact.)
        let mut k = DrumKit::neutral(48_000.0);
        k.trigger(6, 1.0, false, &[]);
        for _ in 0..2_000 {
            k.voices[6].render();
        }
        k.trigger(5, 1.0, false, &[]); // chokes the ringing open hat
        while k.voices[6].is_active() {
            k.voices[6].render();
        }
        assert_eq!(
            open_hat_render(&mut k).len(),
            baseline.len(),
            "post-choke open hat must ring the full natural decay"
        );
    }

    #[test]
    fn nonfinite_drift_cents_stays_finite_per_voice() {
        // A poisoned per-hit pitch offset (NaN/inf cents — the worst a future
        // mod/drift source could feed `set_pitch_drift_cents`) must never make a
        // voice render non-finite. Renders each voice in isolation so the bus
        // limiter can't mask a NaN; the safe_hz / cents_to_ratio / safe_inc folds
        // are what hold the line.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut kit = DrumKit::neutral(48_000.0);
            for t in 0..MAX_TRACKS {
                kit.voices[t].set_pitch_drift_cents(bad);
                kit.voices[t].trigger(1.0, true);
                for _ in 0..4_000 {
                    let (l, r) = kit.voices[t].render();
                    assert!(
                        l.is_finite() && r.is_finite(),
                        "track {t} rendered non-finite with drift cents = {bad}"
                    );
                }
            }
        }
    }

    // --- M10 finiteness + exact-silence smoke suite (property tests, never
    // goldens): the engine must stay finite + bus-limited at every extreme, and
    // return to *exact* zero when idle (no denormal floor). ---

    /// The worst-case p-lock corners to drive a voice to its rails.
    fn extreme_plock_cases() -> Vec<Vec<PLock>> {
        use LockableParam::*;
        let one = |p: LockableParam, v: f32| vec![PLock { param: p.index() as u16, value: v }];
        vec![
            vec![],                 // unlocked baseline
            one(Cutoff, 0.0),       // filter slammed shut
            one(Cutoff, 1.0),       // filter wide open
            one(Resonance, 1.0),    // self-oscillation territory
            one(Drive, 1.0),        // max saturation
            one(Level, 1.0),        // max VCA
            one(Decay, 0.0),        // shortest tail
            one(Decay, 1.0),        // longest tail
            one(Pitch, 0.0),        // pitch floor
            one(Pitch, 1.0),        // pitch ceiling
            // a 4-lock worst case stacking the loud/long corners
            vec![
                PLock { param: Cutoff.index() as u16, value: 1.0 },
                PLock { param: Resonance.index() as u16, value: 1.0 },
                PLock { param: Drive.index() as u16, value: 1.0 },
                PLock { param: Decay.index() as u16, value: 1.0 },
            ],
        ]
    }

    fn sweep_voice_finite(sr: f32, samples: usize, full: bool) {
        let cases = extreme_plock_cases();
        // Full sweep at the primary rate; just the 4-lock worst case at the others
        // (the sub-Nyquist clamps are the only sr-dependent part).
        let cases: &[Vec<PLock>] =
            if full { &cases } else { std::slice::from_ref(cases.last().unwrap()) };
        for track in 0..MAX_TRACKS {
            for &vel in &[0.0_f32, 0.01, 0.5, 1.0] {
                for &accent in &[false, true] {
                    for plocks in cases {
                        let mut kit = DrumKit::neutral(sr);
                        kit.trigger(track, vel, accent, plocks);
                        let mut peak = 0.0_f32;
                        for _ in 0..samples {
                            let (l, r) = kit.render();
                            assert!(
                                l.is_finite() && r.is_finite(),
                                "track {track} sr {sr} vel {vel} accent {accent} rendered non-finite"
                            );
                            peak = peak.max(l.abs()).max(r.abs());
                        }
                        assert!(peak <= 1.02, "track {track} sr {sr} exceeded the limiter: peak={peak}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_voice_is_finite_and_bounded_at_extremes() {
        sweep_voice_finite(48_000.0, 3_000, true);
        // the sub-Nyquist clamps depend on sr — sweep the rates too (worst case).
        sweep_voice_finite(44_100.0, 2_000, false);
        sweep_voice_finite(96_000.0, 2_000, false);
    }

    #[test]
    fn mod_matrix_saturated_stays_finite() {
        // Every one of the 16 slots active to track 0, cycling all dests at +/-1
        // depth, with every global/macro/wheel at 1.0 and 4 stacked p-locks —
        // the most the mod matrix can throw at a voice. Must stay finite + limited.
        let mut kit = DrumKit::neutral(48_000.0);
        for i in 0..16 {
            let dst = DrumModDest::from_index(1 + (i % (N_DRUM_DESTS - 1))); // skip Off=0
            let src = DrumModSource::from_index(1 + (i % (N_DRUM_SOURCES - 1)));
            let depth = if i % 2 == 0 { 1.0 } else { -1.0 };
            kit.set_mod_slot(i, src, dst, depth, 0);
        }
        kit.set_mod_globals(1.0, 1.0, 1.0);
        kit.set_mod_wheel(1.0);
        kit.set_macros([1.0; 8]);
        let plocks = [
            PLock { param: LockableParam::Cutoff.index() as u16, value: 1.0 },
            PLock { param: LockableParam::Resonance.index() as u16, value: 1.0 },
            PLock { param: LockableParam::Pitch.index() as u16, value: 1.0 },
            PLock { param: LockableParam::Decay.index() as u16, value: 1.0 },
        ];
        let mut peak = 0.0_f32;
        for _ in 0..8 {
            kit.trigger(0, 1.0, true, &plocks);
            for _ in 0..4_000 {
                let (l, r) = kit.render();
                assert!(l.is_finite() && r.is_finite(), "saturated mod went non-finite");
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        assert!(peak <= 1.02, "saturated mod exceeded the limiter: peak={peak}");
    }

    #[test]
    fn stacked_ampdecay_routes_stay_clamped() {
        // 16 AmpDecay routes at max depth must NOT drive the decay to a runaway/
        // stuck tail — the +/-2 oct clamp bounds the scale to [0.25x, 4x], so even
        // the longest voice goes quiet well within a few seconds.
        let mut kit = DrumKit::neutral(48_000.0);
        for i in 0..16 {
            kit.set_mod_slot(i, DrumModSource::from_index(1), DrumModDest::AmpDecay, 1.0, 0);
        }
        kit.set_macros([1.0; 8]);
        kit.set_mod_globals(1.0, 1.0, 1.0);
        kit.set_mod_wheel(1.0);
        kit.trigger(0, 1.0, true, &[]);
        let total = 48_000 * 8;
        let mut last_loud = 0;
        for n in 0..total {
            let (l, r) = kit.render();
            assert!(l.is_finite() && r.is_finite());
            if l.abs() > 1e-4 || r.abs() > 1e-4 {
                last_loud = n;
            }
        }
        assert!(last_loud < total - 1, "tail must end within 8s even with stacked AmpDecay (clamp holds)");
    }

    #[test]
    fn idle_kit_is_exact_silence() {
        // No triggers => the bus must emit EXACT zero (no denormal floor).
        let mut kit = DrumKit::neutral(48_000.0);
        for _ in 0..96_000 {
            let (l, r) = kit.render();
            assert_eq!(l, 0.0, "idle kit must be exact zero");
            assert_eq!(r, 0.0, "idle kit must be exact zero");
        }
    }

    #[test]
    fn idle_kit_with_live_sends_is_exact_silence() {
        // Reverb/delay sends up + a live mod route, but nothing triggered: an
        // unfed FX tail must not leak a denormal. Still exact zero.
        let mut kit = DrumKit::neutral(48_000.0);
        kit.set_voice_mix(0, 0, 0.8); // send A (reverb)
        kit.set_voice_mix(0, 1, 0.8); // send B (delay)
        kit.set_mod_slot(0, DrumModSource::from_index(3), DrumModDest::Cutoff, 1.0, 0xFF);
        kit.set_mod_globals(1.0, 1.0, 1.0);
        for _ in 0..96_000 {
            let (l, r) = kit.render();
            assert_eq!(l, 0.0, "live-but-unfed sends must stay exact zero");
            assert_eq!(r, 0.0, "live-but-unfed sends must stay exact zero");
        }
    }

    #[test]
    fn bus_silence_decays_to_exact_zero() {
        // Excite the whole kit with sends up, then run on silence: every tail +
        // the reverb/delay feedback must decay and FLUSH to a true zero (anything
        // below 1e-25 snaps to 0), never settle on a stuck denormal/DC floor.
        let mut kit = DrumKit::neutral(48_000.0);
        for t in 0..MAX_TRACKS {
            kit.set_voice_mix(t, 0, 0.6);
            kit.set_voice_mix(t, 1, 0.6);
            kit.trigger(t, 1.0, true, &[]);
        }
        let cap = 48_000 * 30; // generous; deterministic (neutral kit, fixed params)
        let mut zero_at = None;
        for n in 0..cap {
            let (l, r) = kit.render();
            assert!(l.is_finite() && r.is_finite());
            if l == 0.0 && r == 0.0 {
                zero_at = Some(n);
                break;
            }
        }
        assert!(zero_at.is_some(), "bus must flush to exact zero within 30s of silence");
    }

    fn kick_peak(kit: &mut DrumKit) -> f32 {
        let mut p = 0.0_f32;
        for _ in 0..4_000 {
            let (l, _) = kit.render();
            p = p.max(l.abs());
        }
        p
    }

    #[test]
    fn plock_changes_the_hit() {
        // A Level lock at 0.0 (normalized) silences the hit vs an unlocked hit.
        let mut unlocked = DrumKit::neutral(48_000.0);
        unlocked.trigger(0, 1.0, false, &[]);
        let unlocked_peak = kick_peak(&mut unlocked);

        let mut locked = DrumKit::neutral(48_000.0);
        locked.trigger(0, 1.0, false, &[PLock { param: LockableParam::Level.index(), value: 0.0 }]);
        let locked_peak = kick_peak(&mut locked);

        assert!(unlocked_peak > 0.2, "unlocked kick should be audible");
        assert!(
            locked_peak < unlocked_peak * 0.2,
            "a Level=0 p-lock should silence the hit: locked={locked_peak} unlocked={unlocked_peak}"
        );
    }

    #[test]
    fn plock_restores_base_on_next_unlocked_hit() {
        // hit 2 (unlocked) must come out the same whether hit 1 was p-locked or
        // not — i.e. the dirty-flag restore puts the tail back to base. (Level
        // scaling is post-filter, so hit 1 leaves the filter in the same state
        // either way; the only variable under test is the restore.)
        let second_hit_peak = |first_plocks: &[PLock]| {
            let mut kit = DrumKit::neutral(48_000.0);
            kit.trigger(0, 1.0, false, first_plocks);
            for _ in 0..48_000 {
                kit.render();
            }
            kit.trigger(0, 1.0, false, &[]); // hit 2: unlocked
            kick_peak(&mut kit)
        };
        let after_locked =
            second_hit_peak(&[PLock { param: LockableParam::Level.index(), value: 0.0 }]);
        let after_unlocked = second_hit_peak(&[]);
        assert!(
            (after_locked - after_unlocked).abs() < after_unlocked * 0.02,
            "unlocked hit must restore base regardless of the prior hit's p-lock: \
             {after_locked} vs {after_unlocked}"
        );
    }
}
