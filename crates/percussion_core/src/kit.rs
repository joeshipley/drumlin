//! `DrumKit` — the 12-track voice rack with per-voice tails, choke groups, and
//! the shared dynamics bus (design §3.1/§3.2/§5.3). Signal flow per track:
//! voice engine → `VoiceTail` (drive → CS-80 filter → level → pan) → kit sum →
//! `DrumBus` (glue → true-peak limiter) → output. M3's Neutral kit voices Kick,
//! Snare, Clap and Closed/Open Hat at their design indices; the other 7 tracks
//! are `Silent` until M4.

use crate::bus::{BusSends, DrumBus};
use crate::plock::{LockableParam, PLock};
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
    pub tracks: [[f32; LockableParam::COUNT]; MAX_TRACKS],
}

impl Default for VoicePatch {
    fn default() -> Self {
        // Derive from the Neutral kit so the defaults never drift from neutral().
        // (base[] is sample-rate independent, so any rate yields the same patch.)
        let mut p = Self { tracks: [[0.0; LockableParam::COUNT]; MAX_TRACKS] };
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
}

/// Below this, a send snaps to exactly 0.0 so the persisted value, the bus
/// engage gate, and the per-sample render tap all agree on "off" (no sub-
/// threshold send that routes nothing yet reports as engaged).
const SEND_FLOOR: f32 = 0.0001;

impl VoiceMixRow {
    /// Apply one MIX edit: `field` 0 = Send A, 1 = Send B, 2 = mute, 3 = solo,
    /// 4 = gated_verb (bools as `> 0.5`), 5 = choke_group (0..=4). The single
    /// source of this mapping — shared by the audio-thread kit update and the
    /// editor-thread persist write, so they can't drift.
    pub fn set(&mut self, field: u8, value: f32) {
        match field {
            0 => self.send_a = Self::snap_send(value),
            1 => self.send_b = Self::snap_send(value),
            2 => self.mute = value > 0.5,
            3 => self.solo = value > 0.5,
            4 => self.gated_verb = value > 0.5,
            5 => self.choke_group = value.clamp(0.0, 4.0).round() as u8,
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
    bus: DrumBus,
    sr: f32,
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

    /// Trigger a track. Applies any per-step parameter locks to the tail, the
    /// choke broadcast (allocation-free, O(12)), then triggers the voice.
    pub fn trigger(&mut self, track: usize, velocity: f32, accent: bool, plocks: &[PLock]) {
        if track >= MAX_TRACKS {
            return;
        }
        self.apply_plocks(track, plocks);
        let g = self.mix[track].choke_group;
        if g != 0 {
            for i in 0..MAX_TRACKS {
                if i != track && self.mix[i].choke_group == g {
                    self.voices[i].choke();
                }
            }
        }
        self.voices[track].trigger(velocity, accent);
    }

    /// Resolve (base + p-lock overrides) onto the track's tail. The dirty flag
    /// lets an unlocked hit on an unlocked tail skip all work — so default
    /// (no-p-lock) playback never touches the tail and stays bit-identical.
    fn apply_plocks(&mut self, track: usize, plocks: &[PLock]) {
        if plocks.is_empty() && !self.tail_dirty[track] {
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
                }
            }
        }
        let tail = &mut self.tails[track];
        tail.set_level(level);
        tail.set_pan(pan);
        tail.set_lp_cutoff(cutoff);
        tail.set_resonance(resonance);
        tail.set_drive_amount(drive_on, drive_amt);
        self.tail_dirty[track] = !plocks.is_empty();
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
    /// 3 = solo, 4 = gated_verb (bools as `> 0.5`). Sends/mute/solo/gated-verb,
    /// not Level/Pan (those are VOICE patch params via `set_voice_param`).
    pub fn set_voice_mix(&mut self, track: usize, field: u8, value: f32) {
        if track >= MAX_TRACKS {
            return;
        }
        self.mix[track].set(field, value);
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
        self.recompute_mix_flags();
    }

    /// Sum all voices through their tails (with mute/solo gain), accumulating the
    /// post-fader **Send A** (reverb) and **Send B** (delay) sums, then run the
    /// glue/limiter bus with those sends. At the default (gain 1.0, sends 0) this
    /// is bit-identical to a bare voice sum into the bus.
    pub fn render(&mut self) -> (f32, f32) {
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
            l += tl;
            r += tr;
            // post-fader/mute sends. Send A routes to the gated reverb when the
            // voice is flagged gated_verb, else the normal reverb.
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

    /// Live pump duck gain (1.0 = open) for the GUI pump meter.
    pub fn pump_envelope(&self) -> f32 {
        self.bus.pump_envelope()
    }

    /// Panic-reset: silence every voice and clear filter/tail state.
    pub fn reset(&mut self) {
        for v in &mut self.voices {
            v.reset();
        }
        for t in &mut self.tails {
            t.reset();
        }
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
        let mut exported = VoicePatch { tracks: [[0.0; LockableParam::COUNT]; MAX_TRACKS] };
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

        // Set a send + mute, round-trip, confirm survival.
        k.set_voice_mix(2, 0, 0.6); // snare Send A
        k.set_voice_mix(5, 2, 1.0); // closed-hat mute
        k.set_voice_mix(0, 3, 1.0); // kick solo
        let mut m = VoiceMix::default();
        k.export_mix_into(&mut m);
        let mut k2 = DrumKit::neutral(48_000.0);
        k2.import_mix(&m);
        assert!((k2.voice_mix(2, 0) - 0.6).abs() < 1e-6, "send A survives");
        assert_eq!(k2.voice_mix(5, 2), 1.0, "mute survives");
        assert_eq!(k2.voice_mix(0, 3), 1.0, "solo survives");
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
