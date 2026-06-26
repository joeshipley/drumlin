//! The step sequencer (design §4) — the soul of the instrument.
//!
//! M5 part 1 lands the **per-step performance engine**: every step carries a full
//! Elektron-style payload (velocity, accent, micro-timing, ratchets, probability,
//! conditional trig, and up to `MAX_PLOCKS` parameter locks), evaluated in a
//! deterministic, RNG-driven, reproducible order. A seeded `XorShift32`
//! (`GROOVE LOCK`) is re-seeded each pattern loop so a humanized / probabilistic
//! groove is frozen and bit-reproducible. Swing, micro and ratchets shift a
//! trigger off its step boundary; a fixed-capacity **carry queue** schedules
//! those shifted triggers even across process-block boundaries.
//!
//! Pattern bank + song chaining + Euclid + FILL button + live record arrive in
//! M5 part 2. The audio thread never allocates; pattern state is `Copy` POD.

use crate::plock::{PLock, MAX_PLOCKS};
use crate::drift::{PURPOSE_DRIFT_LEVEL, PURPOSE_DRIFT_PITCH, PURPOSE_MOD_RANDOM};
use crate::rng::{mix_seed, XorShift32};
use crate::{Trigger, MAX_STEPS, MAX_TRACKS};

/// Conditional trig (design §4.3). `Pre`/`Neighbor` are deferred ("Later").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TrigCondition {
    Always,
    Fill,
    NotFill,
    First,
    NotFirst,
    /// Play on the `a`-th loop of every `b` loops (1-indexed), e.g. 1:2, 3:4.
    Ratio { a: u8, b: u8 },
}

impl Default for TrigCondition {
    fn default() -> Self {
        TrigCondition::Always
    }
}

impl TrigCondition {
    /// Compact code for the GUI/edit protocol.
    pub fn code(self) -> u8 {
        match self {
            TrigCondition::Always => 0,
            TrigCondition::Fill => 1,
            TrigCondition::NotFill => 2,
            TrigCondition::First => 3,
            TrigCondition::NotFirst => 4,
            TrigCondition::Ratio { .. } => 5,
        }
    }

    pub fn from_code(code: u8, a: u8, b: u8) -> Self {
        match code {
            1 => TrigCondition::Fill,
            2 => TrigCondition::NotFill,
            3 => TrigCondition::First,
            4 => TrigCondition::NotFirst,
            5 => TrigCondition::Ratio { a: a.max(1), b: b.max(1) },
            _ => TrigCondition::Always,
        }
    }

    fn passes(self, loop_index: u64, fill_active: bool) -> bool {
        match self {
            TrigCondition::Always => true,
            TrigCondition::Fill => fill_active,
            TrigCondition::NotFill => !fill_active,
            TrigCondition::First => loop_index == 0,
            TrigCondition::NotFirst => loop_index != 0,
            TrigCondition::Ratio { a, b } => {
                let b = b.max(1) as u64;
                let a = (a.max(1) as u64).min(b);
                (loop_index % b) + 1 == a
            }
        }
    }
}

/// Microtiming + accent feel layered over swing (design §4.2). Minimal set for
/// M5 part 1; the full library lands in part 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GrooveTemplate {
    Straight,
    Mpc16,
    Lazy,
}

impl Default for GrooveTemplate {
    fn default() -> Self {
        GrooveTemplate::Straight
    }
}

impl GrooveTemplate {
    /// Per-step micro push as a fraction of one step (0..~0.2), before the
    /// `groove_amount` blend.
    fn offset_frac(self, step: usize) -> f32 {
        match self {
            GrooveTemplate::Straight => 0.0,
            GrooveTemplate::Mpc16 => match step % 4 {
                1 => 0.10,
                3 => 0.14,
                _ => 0.0,
            },
            GrooveTemplate::Lazy => {
                if step % 2 == 1 {
                    0.18
                } else {
                    0.02
                }
            }
        }
    }
}

/// One step on one track. Fixed-size, `Copy`, no heap (design §4.2).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Step {
    pub on: bool,
    pub velocity: u8, // 0..=127
    pub accent: bool,
    pub micro: i16,        // micro-timing nudge in 1/384-beat ticks, signed
    pub ratchet: u8,       // 1 = normal, 2..=8 = roll
    pub ratchet_ramp: i8,  // -100..+100: flam (down) .. build (up)
    pub probability: u8,   // 0..=100; drawn from the pattern RNG each pass
    pub condition: TrigCondition,
    pub plocks: [PLock; MAX_PLOCKS],
    pub plock_count: u8,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            on: false,
            velocity: 100,
            accent: false,
            micro: 0,
            ratchet: 1,
            ratchet_ramp: 0,
            probability: 100,
            condition: TrigCondition::Always,
            plocks: [PLock::default(); MAX_PLOCKS],
            plock_count: 0,
        }
    }
}

impl Step {
    pub fn plocks(&self) -> &[PLock] {
        &self.plocks[..(self.plock_count as usize).min(MAX_PLOCKS)]
    }

    /// Add a parameter lock (replacing one for the same param, or appending).
    pub fn set_plock(&mut self, param: u16, value: f32) {
        for i in 0..self.plock_count as usize {
            if self.plocks[i].param == param {
                self.plocks[i].value = value;
                return;
            }
        }
        if (self.plock_count as usize) < MAX_PLOCKS {
            self.plocks[self.plock_count as usize] = PLock { param, value };
            self.plock_count += 1;
        }
    }

    pub fn clear_plock(&mut self, param: u16) {
        let mut i = 0;
        while i < self.plock_count as usize {
            if self.plocks[i].param == param {
                for j in i..(self.plock_count as usize - 1) {
                    self.plocks[j] = self.plocks[j + 1];
                }
                self.plock_count -= 1;
            } else {
                i += 1;
            }
        }
    }
}

/// One drum track's lane. `length` < pattern length gives polymeter (design §4.2).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
    // serde's native array impls stop at 32; BigArray covers [Step; 64].
    #[cfg_attr(feature = "serde", serde(with = "serde_big_array::BigArray"))]
    pub steps: [Step; MAX_STEPS],
    pub length: u8,
    pub muted: bool,
    pub swing: i8, // -1 = use pattern swing; else 50..=75
    pub level: u8, // 0..=127, scales velocity
}

impl Default for Track {
    fn default() -> Self {
        Self {
            steps: [Step::default(); MAX_STEPS],
            length: 16,
            muted: false,
            swing: -1,
            level: 127,
        }
    }
}

/// A pattern binds the tracks plus global feel. `Copy`, so an undo snapshot is
/// one memcpy (design §4.4).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pattern {
    pub tracks: [Track; MAX_TRACKS],
    pub length: u8,        // active steps, 1..=64
    pub resolution: u8,    // steps per bar (16 = 16ths)
    pub swing: u8,         // 50..=75 (50 straight, 66 ≈ triplet feel)
    pub groove: GrooveTemplate,
    pub groove_amount: u8, // 0..=100
    pub humanize: u8,      // 0..=100 seeded timing + velocity jitter
    pub seed: u32,         // per-pattern PRNG seed -> GROOVE LOCK
    pub fill_active: bool,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            tracks: [Track::default(); MAX_TRACKS],
            length: 16,
            resolution: 16,
            swing: 50,
            groove: GrooveTemplate::Straight,
            groove_amount: 0,
            humanize: 0,
            seed: 0x1234_5678,
            fill_active: false,
        }
    }
}

impl Pattern {
    /// The clean, punchy 909-leaning demo groove on the Neutral kit:
    /// four-on-the-floor kick, snare+clap backbeat, straight-8th closed hats,
    /// and a couple of open-hat offbeats that the closed hat chokes.
    pub fn neutral_demo() -> Self {
        let mut p = Pattern::default();
        let set = |p: &mut Pattern, track: usize, steps: &[usize], accent: &[usize]| {
            for &s in steps {
                p.tracks[track].steps[s].on = true;
                p.tracks[track].steps[s].velocity = 105;
            }
            for &s in accent {
                p.tracks[track].steps[s].accent = true;
                p.tracks[track].steps[s].velocity = 120;
            }
        };
        set(&mut p, 0, &[0, 4, 8, 12], &[0]); // KICK four-on-the-floor, accent downbeat
        set(&mut p, 2, &[4, 12], &[]); // SNARE backbeat
        set(&mut p, 3, &[4, 12], &[]); // CLAP layered with snare
        set(&mut p, 5, &[0, 2, 4, 6, 8, 10, 12, 14], &[]); // CLOSED HAT straight 8ths
        set(&mut p, 6, &[7, 15], &[]); // OPEN HAT offbeats (choked by closed)
        p
    }
}

/// Shape a ratchet sub-hit's velocity by the ramp (-100 flam .. +100 build).
fn ratchet_velocity(base: f32, k: u8, n: u8, ramp: i8) -> f32 {
    if n <= 1 {
        return base;
    }
    let frac = k as f32 / (n - 1) as f32; // 0..1
    let r = ramp as f32 / 100.0; // -1..1
    let lo = (1.0 - r.abs() * 0.6).max(0.1);
    let mult = if r >= 0.0 {
        lo + (1.0 - lo) * frac // build: quiet -> loud
    } else {
        1.0 - (1.0 - lo) * frac // flam: loud -> quiet
    };
    (base * mult).clamp(0.0, 1.0)
}

const MAX_PENDING: usize = 256;
const MAX_CARRY: usize = 128;

/// Pattern bank size (design §4.2 "16 slots"). Song chaining queues the next.
pub const N_PATTERNS: usize = 16;

/// The persistable slice of sequencer state: the full 16-pattern bank plus the
/// selected slot. The plugin serializes this into its `#[persist]` field so a
/// saved host project restores every programmed step, p-lock and groove.
/// Playback state (playhead, carry, queued switch) is transient and excluded.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SeqState {
    pub patterns: Vec<Pattern>,
    pub current: u8,
}

impl Default for SeqState {
    fn default() -> Self {
        // A fresh instance carries the Neutral demo groove, matching `Sequencer::new`,
        // so an un-saved plugin still boots with a playable pattern.
        let mut patterns = vec![Pattern::default(); N_PATTERNS];
        patterns[0] = Pattern::neutral_demo();
        Self { patterns, current: 0 }
    }
}

#[derive(Clone, Copy)]
struct Carried {
    /// Samples until fire, measured from the start of the next block processed.
    samples: u32,
    trig: Trigger,
}

/// The per-hit modulation values a step computes once and stamps onto every
/// Trigger it emits (shared across a ratchet's sub-hits). Seeded randoms + the
/// timing sources; the global sources (LFOs/env/macros) come from kit state.
#[derive(Clone, Copy)]
struct HitMods {
    rand_pitch: f32,
    rand_level: f32,
    rand_mod: f32,
    bar_phase: f32,
    step_pos: f32,
}

/// The runtime sequencer. Lives on the audio thread; edits arrive from the GUI
/// via the plugin's lock-free edit ring.
pub struct Sequencer {
    /// The live, currently-playing pattern (the hot path reads this directly).
    pub pattern: Pattern,
    /// The 16-slot pattern bank (heap-backed: a `Pattern` is ~37 KB, so the bank
    /// would blow the stack inline). Allocated once at construction; the audio
    /// thread only indexes/copies into it (no allocation), so it stays RT-safe.
    bank: Vec<Pattern>,
    current: usize,
    /// Pattern queued to switch in at the next loop boundary (song chaining).
    queued: Option<usize>,
    playing: bool,
    /// Continuous absolute step position (re-anchored to the host each block).
    playhead_steps: f64,
    prev_floor: i64,
    pending: [Trigger; MAX_PENDING],
    pending_len: usize,
    carry: [Carried; MAX_CARRY],
    carry_len: usize,
}

impl Default for Sequencer {
    fn default() -> Self {
        Self::new()
    }
}

impl Sequencer {
    pub fn new() -> Self {
        let pattern = Pattern::neutral_demo();
        // vec! builds directly on the heap (no giant stack temporary).
        let mut bank = vec![Pattern::default(); N_PATTERNS];
        bank[0] = pattern;
        let empty = Trigger {
            offset: 0,
            track: 0,
            velocity: 0.0,
            accent: false,
            plocks: [PLock::default(); MAX_PLOCKS],
            plock_count: 0,
            rand_pitch: 0.0,
            rand_level: 0.0,
            rand_mod: 0.0,
            bar_phase: 0.0,
            step_pos: 0.0,
        };
        Self {
            pattern,
            bank,
            current: 0,
            queued: None,
            playing: false,
            playhead_steps: 0.0,
            prev_floor: -1,
            pending: [empty; MAX_PENDING],
            pending_len: 0,
            carry: [Carried { samples: 0, trig: empty }; MAX_CARRY],
            carry_len: 0,
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_fill(&mut self, on: bool) {
        self.pattern.fill_active = on;
    }

    /// Current step index under the playhead (for the GUI moving column), or
    /// `None` when stopped.
    pub fn current_step(&self) -> Option<usize> {
        if !self.playing || self.prev_floor < 0 {
            // not playing, or playing but no boundary crossed yet this run
            return None;
        }
        let len = self.pattern.length.max(1) as i64;
        Some(self.prev_floor.rem_euclid(len) as usize)
    }

    pub fn toggle_step(&mut self, track: usize, step: usize) -> bool {
        if track < MAX_TRACKS && step < MAX_STEPS {
            let s = &mut self.pattern.tracks[track].steps[step];
            s.on = !s.on;
            s.on
        } else {
            false
        }
    }

    pub fn set_step(&mut self, track: usize, step: usize, on: bool) {
        if track < MAX_TRACKS && step < MAX_STEPS {
            self.pattern.tracks[track].steps[step].on = on;
        }
    }

    pub fn step_on(&self, track: usize, step: usize) -> bool {
        track < MAX_TRACKS && step < MAX_STEPS && self.pattern.tracks[track].steps[step].on
    }

    /// Read a step (for the GUI to populate its inspector / cell visuals).
    pub fn step(&self, track: usize, step: usize) -> Option<&Step> {
        if track < MAX_TRACKS && step < MAX_STEPS {
            Some(&self.pattern.tracks[track].steps[step])
        } else {
            None
        }
    }

    /// Set a step's full performance payload (from the GUI step inspector).
    #[allow(clippy::too_many_arguments)]
    pub fn set_step_params(
        &mut self,
        track: usize,
        step: usize,
        on: bool,
        velocity: u8,
        accent: bool,
        probability: u8,
        ratchet: u8,
        micro: i16,
        condition: TrigCondition,
    ) {
        if track >= MAX_TRACKS || step >= MAX_STEPS {
            return;
        }
        let s = &mut self.pattern.tracks[track].steps[step];
        s.on = on;
        s.velocity = velocity.min(127);
        s.accent = accent;
        s.probability = probability.min(100);
        s.ratchet = ratchet.clamp(1, 8);
        s.micro = micro;
        s.condition = condition;
    }

    pub fn set_plock(&mut self, track: usize, step: usize, param: u16, value: f32) {
        if track < MAX_TRACKS && step < MAX_STEPS {
            self.pattern.tracks[track].steps[step].set_plock(param, value);
        }
    }

    pub fn clear_plock(&mut self, track: usize, step: usize, param: u16) {
        if track < MAX_TRACKS && step < MAX_STEPS {
            self.pattern.tracks[track].steps[step].clear_plock(param);
        }
    }

    pub fn clear_lane(&mut self, track: usize) {
        if track < MAX_TRACKS {
            let length = self.pattern.tracks[track].length;
            self.pattern.tracks[track] = Track { length, ..Track::default() };
        }
    }

    /// Generate a Euclidean rhythm on a lane: `pulses` hits spread as evenly as
    /// possible over the lane length, rotated by `rotate` (design §4.4).
    pub fn euclid(&mut self, track: usize, pulses: u8, rotate: u8) {
        if track >= MAX_TRACKS {
            return;
        }
        let len = self.pattern.tracks[track].length.max(1) as usize;
        for s in 0..MAX_STEPS {
            self.pattern.tracks[track].steps[s].on = false;
        }
        let k = (pulses as usize).min(len);
        if k == 0 {
            return;
        }
        for i in 0..k {
            let pos = (i * len) / k; // even Euclidean distribution
            let pos = (pos + rotate as usize) % len;
            let s = &mut self.pattern.tracks[track].steps[pos];
            s.on = true;
            if s.velocity == 0 {
                s.velocity = 105;
            }
        }
    }

    pub fn set_swing(&mut self, swing: u8) {
        self.pattern.swing = swing.clamp(50, 75);
    }

    pub fn set_humanize(&mut self, humanize: u8) {
        self.pattern.humanize = humanize.min(100);
    }

    pub fn set_groove_amount(&mut self, amount: u8) {
        self.pattern.groove_amount = amount.min(100);
    }

    // --- pattern bank / song chaining ---
    pub fn current_pattern(&self) -> usize {
        self.current
    }

    pub fn queued_pattern(&self) -> Option<usize> {
        self.queued
    }

    /// Which bank slots have any active step (for the GUI selector dots).
    pub fn pattern_used(&self, idx: usize) -> bool {
        let p = if idx == self.current {
            &self.pattern
        } else if idx < N_PATTERNS {
            &self.bank[idx]
        } else {
            return false;
        };
        p.tracks
            .iter()
            .any(|t| t.steps[..(t.length as usize).min(MAX_STEPS)].iter().any(|s| s.on))
    }

    /// Select a pattern: queued to switch at the next loop while playing, or
    /// immediately when stopped (so the GUI updates at once).
    pub fn select_pattern(&mut self, idx: usize) {
        if idx >= N_PATTERNS || idx == self.current {
            self.queued = None;
            return;
        }
        if self.playing {
            self.queued = Some(idx);
        } else {
            self.bank[self.current] = self.pattern;
            self.current = idx;
            self.pattern = self.bank[self.current];
            self.queued = None;
        }
    }

    pub fn reset_playhead(&mut self) {
        self.playhead_steps = 0.0;
        self.prev_floor = -1;
        self.carry_len = 0;
    }

    /// Snapshot the live bank into `state` for persistence. Allocation-free when
    /// `state` is already sized to the bank (the `Default`), so it is safe to
    /// call from the audio thread under a non-blocking `try_lock`.
    pub fn export_into(&self, state: &mut SeqState) {
        if state.patterns.len() != N_PATTERNS {
            return; // never reallocate on the audio thread
        }
        state.patterns[..N_PATTERNS].copy_from_slice(&self.bank[..N_PATTERNS]);
        // the edited slot lives in `self.pattern`, not yet flushed to the bank.
        state.patterns[self.current] = self.pattern;
        state.current = self.current as u8;
    }

    /// Adopt a persisted bank (host project load) and rewind to the top.
    pub fn import(&mut self, state: &SeqState) {
        let n = state.patterns.len().min(N_PATTERNS);
        self.bank[..n].copy_from_slice(&state.patterns[..n]);
        self.current = (state.current as usize).min(N_PATTERNS - 1);
        self.pattern = self.bank[self.current];
        self.queued = None;
        self.reset_playhead();
    }

    /// Walk one process block. `pos_qn` is the transport position (quarter notes)
    /// at the block's first sample. Fills `pending` (sorted by offset) with the
    /// triggers that fire in this block. Allocation-free.
    pub fn process_block(&mut self, pos_qn: f64, tempo: f64, sr: f64, block_len: usize) {
        self.pending_len = 0;

        // Stopped / invalid block: cancel any scheduled (swing/ratchet) sub-hits
        // so nothing sounds after the transport stops.
        if !self.playing || block_len == 0 || tempo <= 0.0 || sr <= 0.0 {
            self.carry_len = 0;
            return;
        }

        let steps_per_qn = self.pattern.resolution.max(1) as f64 / 4.0;
        let host_step = pos_qn * steps_per_qn;
        let discontinuity = host_step + 1.0e-6 < self.playhead_steps
            || (host_step - self.playhead_steps).abs() > 0.5;
        self.playhead_steps = host_step;
        if discontinuity {
            self.prev_floor = host_step.floor() as i64 - 1;
            // A transport relocate / loop-wrap invalidates sub-hits carried from
            // the old position — flush them so they can't ghost at the new spot.
            self.carry_len = 0;
        }

        // Fire carried sub-hits due this block; carry the rest forward.
        let mut nc = 0;
        for k in 0..self.carry_len {
            let c = self.carry[k];
            if (c.samples as usize) < block_len {
                self.push_pending(Trigger { offset: c.samples, ..c.trig });
            } else {
                self.carry[nc] = Carried { samples: c.samples - block_len as u32, trig: c.trig };
                nc += 1;
            }
        }
        self.carry_len = nc;

        let steps_per_sample = (tempo / 60.0 / sr) * steps_per_qn;
        let samples_per_step = if steps_per_sample > 0.0 { 1.0 / steps_per_sample } else { 0.0 };
        let samples_per_micro_tick = (60.0 / tempo) * sr / 384.0;

        for i in 0..block_len {
            let fl = self.playhead_steps.floor() as i64;
            if fl > self.prev_floor {
                self.prev_floor = fl;
                self.eval_boundary(fl, i, block_len, samples_per_step, samples_per_micro_tick);
            }
            self.playhead_steps += steps_per_sample;
        }
        self.sort_pending();
    }

    fn eval_boundary(
        &mut self,
        fl: i64,
        i: usize,
        block_len: usize,
        samples_per_step: f64,
        samples_per_micro_tick: f64,
    ) {
        // At a loop boundary, switch in any queued pattern (song chaining).
        let plen0 = self.pattern.length.max(1) as i64;
        if fl.rem_euclid(plen0) == 0 {
            if let Some(q) = self.queued.take() {
                self.bank[self.current] = self.pattern;
                self.current = q.min(N_PATTERNS - 1);
                self.pattern = self.bank[self.current];
            }
        }
        let plen = self.pattern.length.max(1) as i64;
        // Conditions evaluate against the pattern-loop epoch (the song loop),
        // even for polymeter tracks that wrap on their own length. The GROOVE
        // LOCK itself needs no per-loop reseed: each cell's randomness is a pure
        // function of (seed, track, step) — see eval_track — so it is frozen and
        // identical every loop by construction, and editing one step never
        // re-rolls another.
        let loop_u = fl.div_euclid(plen).max(0) as u64;
        for t in 0..MAX_TRACKS {
            self.eval_track(t, fl, loop_u, i, block_len, samples_per_step, samples_per_micro_tick);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_track(
        &mut self,
        t: usize,
        fl: i64,
        loop_u: u64,
        i: usize,
        block_len: usize,
        samples_per_step: f64,
        samples_per_micro_tick: f64,
    ) {
        let track = self.pattern.tracks[t];
        if track.muted {
            return;
        }
        let slen = (track.length.max(1) as i64).min(MAX_STEPS as i64);
        let step_idx = fl.rem_euclid(slen) as usize;
        let step = track.steps[step_idx];
        if !step.on {
            return;
        }
        // 1. condition gate
        if !step.condition.passes(loop_u, self.pattern.fill_active) {
            return;
        }
        // 2. probability — each cell rolls from its OWN seed (a pure function of
        //    seed+coordinates), so editing one step never re-rolls another.
        if step.probability < 100 {
            let mut r = XorShift32::new(mix_seed(self.pattern.seed, t as u32, step_idx as u32, 0));
            if r.next_below(100) >= step.probability as u32 {
                return;
            }
        }
        // 3. velocity = step × track level × seeded humanize (own per-cell seed)
        let mut vel = (step.velocity as f32 / 127.0) * (track.level as f32 / 127.0);
        let h = self.pattern.humanize as f32 / 100.0;
        if self.pattern.humanize > 0 {
            let mut rv = XorShift32::new(mix_seed(self.pattern.seed, t as u32, step_idx as u32, 1));
            vel = (vel * (1.0 + rv.next_bipolar() * h * 0.3)).clamp(0.0, 1.0);
        }
        // 4. timing: swing + groove + micro (+ humanize), in samples
        let swing = if track.swing >= 0 {
            track.swing as f32
        } else {
            self.pattern.swing as f32
        };
        // swing P (50..75): the off-16th of each pair is delayed by (P-50)/50 of
        // a step (0 at 50, half a step at 75 = max). Swing/groove follow the
        // ABSOLUTE grid position, not the per-track (polymeter) step, so a swung
        // groove doesn't "walk" on odd-length lanes.
        let grid_pos = fl.rem_euclid(self.pattern.length.max(1) as i64) as usize;
        let swing_frac = (swing - 50.0).max(0.0) / 50.0;
        let swing_off = if grid_pos % 2 == 1 { swing_frac } else { 0.0 };
        let groove_off =
            self.pattern.groove.offset_frac(grid_pos) * (self.pattern.groove_amount as f32 / 100.0);
        let mut off = (swing_off + groove_off) as f64 * samples_per_step;
        off += step.micro as f64 * samples_per_micro_tick;
        if self.pattern.humanize > 0 {
            let mut rt = XorShift32::new(mix_seed(self.pattern.seed, t as u32, step_idx as u32, 2));
            off += (rt.next_bipolar() * h * 0.04) as f64 * samples_per_step;
        }
        let base_off = off.max(0.0); // M5 part 1 clamps early nudges to the boundary

        // 5. per-hit analog-drift randoms — own per-cell seeds (purposes 3/4), pure
        //    normalized bipolar values; the kit scales them by the voice's DRIFT
        //    amount. Independent of the existing draws, so they never re-roll them.
        let rand_pitch =
            XorShift32::new(mix_seed(self.pattern.seed, t as u32, step_idx as u32, PURPOSE_DRIFT_PITCH)).next_bipolar();
        let rand_level =
            XorShift32::new(mix_seed(self.pattern.seed, t as u32, step_idx as u32, PURPOSE_DRIFT_LEVEL)).next_bipolar();
        // The mod matrix's RandomPerHit source — a SEPARATE per-cell S&H (purpose
        // 6), so it never perturbs the drift draws above.
        let rand_mod =
            XorShift32::new(mix_seed(self.pattern.seed, t as u32, step_idx as u32, PURPOSE_MOD_RANDOM)).next_bipolar();
        // Per-hit mod timing sources: bar-phase is GLOBAL (position within the
        // bar, a filter-opens-across-the-bar staple); step-position is PER-TRACK
        // (the hit's step within its own length — meaningful on polymetric lanes).
        let mods = HitMods {
            rand_pitch,
            rand_level,
            rand_mod,
            bar_phase: grid_pos as f32 / self.pattern.length.max(1) as f32,
            step_pos: step_idx as f32 / slen.max(1) as f32,
        };

        // 6. ratchets — spread the sub-hits over the space remaining between the
        //    (swung) start and the next step boundary, so a roll can't spill past it.
        let ratchet = step.ratchet.clamp(1, 8);
        if ratchet <= 1 {
            self.schedule(i as f64 + base_off, block_len, t, vel, mods, &step);
        } else {
            let span = (samples_per_step - base_off).max(samples_per_step * 0.25);
            let spacing = span / ratchet as f64;
            for k in 0..ratchet {
                let kv = ratchet_velocity(vel, k, ratchet, step.ratchet_ramp);
                self.schedule(i as f64 + base_off + k as f64 * spacing, block_len, t, kv, mods, &step);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn schedule(
        &mut self,
        off_from_block_start: f64,
        block_len: usize,
        track: usize,
        vel: f32,
        mods: HitMods,
        step: &Step,
    ) {
        let off = off_from_block_start.round().max(0.0) as u32;
        let trig = Trigger {
            offset: off,
            track: track as u8,
            velocity: vel.clamp(0.0, 1.0),
            accent: step.accent,
            plocks: step.plocks,
            plock_count: step.plock_count,
            rand_pitch: mods.rand_pitch,
            rand_level: mods.rand_level,
            rand_mod: mods.rand_mod,
            bar_phase: mods.bar_phase,
            step_pos: mods.step_pos,
        };
        if (off as usize) < block_len {
            self.push_pending(trig);
        } else {
            debug_assert!(
                self.carry_len < self.carry.len(),
                "sequencer carry overflow — block too large; sub-block chunking is a later refinement"
            );
            if self.carry_len < self.carry.len() {
                self.carry[self.carry_len] = Carried { samples: off - block_len as u32, trig };
                self.carry_len += 1;
            }
        }
    }

    fn push_pending(&mut self, trig: Trigger) {
        debug_assert!(
            self.pending_len < self.pending.len(),
            "sequencer pending overflow — block too large; sub-block chunking is a later refinement"
        );
        if self.pending_len < self.pending.len() {
            self.pending[self.pending_len] = trig;
            self.pending_len += 1;
        }
    }

    /// Insertion sort by offset (stable, allocation-free) — the plugin's dispatch
    /// relies on ascending offsets, and swing/ratchets/carry can emit out of order.
    fn sort_pending(&mut self) {
        for i in 1..self.pending_len {
            let mut j = i;
            while j > 0 && self.pending[j - 1].offset > self.pending[j].offset {
                self.pending.swap(j - 1, j);
                j -= 1;
            }
        }
    }

    /// Triggers emitted by the most recent `process_block`, ascending by offset.
    pub fn pending(&self) -> &[Trigger] {
        &self.pending[..self.pending_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_import_round_trips_the_bank() {
        let mut seq = Sequencer::new();
        // edit slot 2 (a p-locked step), then a step on the default slot 0.
        seq.select_pattern(2);
        seq.set_step(0, 5, true);
        seq.set_plock(0, 5, crate::LockableParam::Cutoff.index(), 0.42);
        seq.select_pattern(0);
        seq.set_step(3, 7, true);

        let mut state = SeqState::default();
        seq.export_into(&mut state);
        assert_eq!(state.current, 0);

        let mut restored = Sequencer::new();
        restored.import(&state);
        assert!(restored.step_on(3, 7), "slot 0 edit must survive");

        restored.select_pattern(2);
        assert!(restored.step_on(0, 5), "slot 2 edit must survive");
        assert_eq!(restored.pattern.tracks[0].steps[5].plock_count, 1, "p-lock must survive");
    }

    #[test]
    fn export_into_is_allocation_free_and_skips_a_mis_sized_buffer() {
        let seq = Sequencer::new();
        let mut wrong = SeqState { patterns: vec![Pattern::default(); 4], current: 0 };
        seq.export_into(&mut wrong); // must not panic / reallocate
        assert_eq!(wrong.patterns.len(), 4, "a mis-sized buffer is left untouched");
    }

    fn collect(seq: &mut Sequencer, tempo: f64, sr: f64, block: usize, blocks: usize) -> Vec<Trigger> {
        let mut all = Vec::new();
        let mut qn = 0.0;
        for _ in 0..blocks {
            seq.process_block(qn, tempo, sr, block);
            all.extend_from_slice(seq.pending());
            qn += (tempo / 60.0) * (block as f64 / sr);
        }
        all
    }

    #[test]
    fn stopped_emits_nothing() {
        let mut seq = Sequencer::new();
        seq.process_block(0.0, 120.0, 48_000.0, 512);
        assert!(seq.pending().is_empty());
    }

    #[test]
    fn first_downbeat_fires_kick_at_offset_zero() {
        let mut seq = Sequencer::new();
        seq.set_playing(true);
        seq.process_block(0.0, 120.0, 48_000.0, 64);
        assert!(seq.pending().iter().any(|t| t.track == 0 && t.offset == 0));
    }

    #[test]
    fn default_pattern_one_bar_kick_count() {
        let mut seq = Sequencer::new();
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar);
        assert_eq!(seq.pending().iter().filter(|t| t.track == 0).count(), 4);
    }

    #[test]
    fn determinism_same_transport_same_triggers() {
        let run = || {
            let mut seq = Sequencer::new();
            seq.set_playing(true);
            collect(&mut seq, 120.0, 48_000.0, 512, 16)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn probability_is_reproducible_and_thins_hits() {
        let make = || {
            let mut seq = Sequencer::new();
            // a busy 50%-probability hat lane on track 7
            for s in 0..16 {
                seq.pattern.tracks[7].steps[s] = Step { on: true, probability: 50, ..Default::default() };
            }
            seq.set_playing(true);
            seq
        };
        let mut a = make();
        let mut b = make();
        let ta = collect(&mut a, 120.0, 48_000.0, 4096, 8);
        let tb = collect(&mut b, 120.0, 48_000.0, 4096, 8);
        assert_eq!(ta, tb, "GROOVE LOCK: same seed -> identical probabilistic performance");
        // and it should drop some of the 16-per-bar hits
        let hits7 = ta.iter().filter(|t| t.track == 7).count();
        assert!(hits7 > 0 && hits7 < 8 * 16, "probability should thin hits, got {hits7}");
    }

    #[test]
    fn probability_groove_lock_repeats_each_loop() {
        // With a frozen seed, loop 2 must fire the same probabilistic steps as loop 1.
        let mut seq = Sequencer::new();
        for s in 0..16 {
            seq.pattern.tracks[7].steps[s] = Step { on: true, probability: 50, ..Default::default() };
        }
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar);
        let loop0: Vec<u32> = seq.pending().iter().filter(|t| t.track == 7).map(|t| t.offset).collect();
        seq.process_block(4.0, 120.0, sr, bar); // next loop (16 steps = 4 quarter notes)
        let loop1: Vec<u32> = seq.pending().iter().filter(|t| t.track == 7).map(|t| t.offset).collect();
        // GROOVE LOCK guarantees the same probabilistic DECISIONS each loop (same
        // steps fire); the per-sample offset can differ by ~1 sample of f64
        // accumulation drift between loops, which is musically irrelevant.
        assert_eq!(loop0.len(), loop1.len(), "GROOVE LOCK must fire the same hits each loop");
        for (a, b) in loop0.iter().zip(loop1.iter()) {
            assert!(
                (*a as i64 - *b as i64).abs() <= 2,
                "groove-locked hits should align within a sample or two: {a} vs {b}"
            );
        }
    }

    #[test]
    fn ratchet_emits_n_subhits() {
        let mut seq = Sequencer::new();
        seq.pattern = Pattern::default();
        seq.pattern.tracks[0].steps[0] = Step { on: true, ratchet: 4, ..Default::default() };
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar);
        let hits = seq.pending().iter().filter(|t| t.track == 0).count();
        assert_eq!(hits, 4, "ratchet 4 should emit 4 sub-hits, got {hits}");
        // sub-hits are spread across the step (distinct ascending offsets)
        let offs: Vec<u32> = seq.pending().iter().filter(|t| t.track == 0).map(|t| t.offset).collect();
        assert!(offs.windows(2).all(|w| w[0] < w[1]), "ratchet offsets must be spread + sorted: {offs:?}");
    }

    #[test]
    fn swing_delays_odd_steps() {
        let mut seq = Sequencer::new();
        seq.pattern = Pattern::default();
        seq.pattern.swing = 66;
        // hat on every 16th
        for s in 0..16 {
            seq.pattern.tracks[5].steps[s].on = true;
        }
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar);
        let offs: Vec<u32> = seq.pending().iter().filter(|t| t.track == 5).map(|t| t.offset).collect();
        let samples_per_step = bar as f32 / 16.0;
        // even step 0 lands on the grid; odd step 1 is pushed late
        assert!((offs[0] as f32) < samples_per_step * 0.25, "step 0 on the grid");
        let step1_pos = offs[1] as f32 - samples_per_step;
        assert!(step1_pos > samples_per_step * 0.2, "swing should push the odd 16th late: {step1_pos}");
    }

    #[test]
    fn conditional_first_only_fires_on_loop_zero() {
        let mut seq = Sequencer::new();
        seq.pattern = Pattern::default();
        seq.pattern.tracks[3].steps[0] = Step { on: true, condition: TrigCondition::First, ..Default::default() };
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar);
        assert_eq!(seq.pending().iter().filter(|t| t.track == 3).count(), 1, "fires on first loop");
        seq.process_block(2.0, 120.0, sr, bar);
        assert_eq!(seq.pending().iter().filter(|t| t.track == 3).count(), 0, "silent after first loop");
    }

    #[test]
    fn ratio_condition_fires_every_other_loop() {
        let mut seq = Sequencer::new();
        seq.pattern = Pattern::default();
        seq.pattern.tracks[3].steps[0] = Step { on: true, condition: TrigCondition::Ratio { a: 1, b: 2 }, ..Default::default() };
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        // one pattern loop = 16 steps = 4 quarter notes
        let fires = |seq: &mut Sequencer, loop_i: f64| {
            seq.process_block(loop_i * 4.0, 120.0, sr, bar);
            seq.pending().iter().filter(|t| t.track == 3).count()
        };
        assert_eq!(fires(&mut seq, 0.0), 1); // loop 0 -> 1:2 fires
        assert_eq!(fires(&mut seq, 1.0), 0); // loop 1 -> silent
        assert_eq!(fires(&mut seq, 2.0), 1); // loop 2 -> fires
    }

    #[test]
    fn polymeter_track_wraps_by_its_own_length() {
        let mut seq = Sequencer::new();
        seq.pattern.tracks[11] = Track::default();
        seq.pattern.tracks[11].length = 3;
        seq.pattern.tracks[11].steps[0].on = true;
        seq.set_playing(true);
        let sr = 48_000.0;
        let samples = (12.0 / 4.0 * (60.0 / 120.0) * sr) as usize;
        seq.process_block(0.0, 120.0, sr, samples);
        assert_eq!(seq.pending().iter().filter(|t| t.track == 11).count(), 4);
    }

    #[test]
    fn pending_is_sorted_by_offset() {
        // a ratcheted + swung pattern emits out of order; pending() must be sorted
        let mut seq = Sequencer::new();
        seq.pattern.swing = 66;
        seq.pattern.tracks[0].steps[0] = Step { on: true, ratchet: 3, ..Default::default() };
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar);
        let offs: Vec<u32> = seq.pending().iter().map(|t| t.offset).collect();
        assert!(offs.windows(2).all(|w| w[0] <= w[1]), "pending must be ascending: {offs:?}");
    }

    #[test]
    fn plocks_ride_along_on_triggers() {
        let mut seq = Sequencer::new();
        seq.pattern = Pattern::default();
        let mut step = Step { on: true, ..Default::default() };
        step.set_plock(crate::plock::LockableParam::Cutoff.index(), 0.3);
        seq.pattern.tracks[0].steps[0] = step;
        seq.set_playing(true);
        seq.process_block(0.0, 120.0, 48_000.0, 256);
        let t = seq.pending().iter().find(|t| t.track == 0).unwrap();
        assert_eq!(t.plock_count, 1);
        assert_eq!(t.plocks()[0].param, crate::plock::LockableParam::Cutoff.index());
    }

    #[test]
    fn editing_one_step_does_not_reroll_others() {
        // Per-cell RNG: toggling an unrelated probabilistic step must NOT change
        // which other probabilistic steps fire (the Elektron-edit-one-step rule).
        let track5_groove = |extra_on: bool| {
            let mut seq = Sequencer::new();
            seq.pattern = Pattern::default();
            for s in 0..16 {
                seq.pattern.tracks[5].steps[s] = Step { on: true, probability: 50, ..Default::default() };
            }
            // a single edit on a different track/step
            seq.pattern.tracks[9].steps[3] = Step { on: extra_on, probability: 50, ..Default::default() };
            seq.set_playing(true);
            let sr = 48_000.0;
            let bar = (4.0 * 0.5 * sr) as usize;
            seq.process_block(0.0, 120.0, sr, bar);
            let spb = bar as f32 / 16.0;
            seq.pending()
                .iter()
                .filter(|t| t.track == 5)
                .map(|t| (t.offset as f32 / spb).round() as i32)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            track5_groove(false),
            track5_groove(true),
            "track 5's groove must be independent of an edit on track 9"
        );
    }

    #[test]
    fn euclid_distributes_and_rotates() {
        let mut seq = Sequencer::new();
        seq.pattern = Pattern::default();
        seq.euclid(5, 4, 0);
        let on: Vec<usize> = (0..16).filter(|&s| seq.step_on(5, s)).collect();
        assert_eq!(on, vec![0, 4, 8, 12], "4 pulses over 16, evenly spread");
        seq.euclid(5, 4, 2);
        let rot: Vec<usize> = (0..16).filter(|&s| seq.step_on(5, s)).collect();
        assert_eq!(rot, vec![2, 6, 10, 14], "rotated by 2");
    }

    #[test]
    fn pattern_switch_stopped_is_immediate_and_preserves_slots() {
        let mut seq = Sequencer::new();
        seq.select_pattern(3);
        assert_eq!(seq.current_pattern(), 3, "stopped switch is immediate");
        seq.set_step(0, 1, true); // edit slot 3
        seq.select_pattern(0);
        assert!(seq.step_on(0, 0), "slot 0 (neutral_demo) preserved");
        assert!(!seq.step_on(0, 1), "slot 3's edit didn't leak into slot 0");
        seq.select_pattern(3);
        assert!(seq.step_on(0, 1), "slot 3's edit was preserved");
    }

    #[test]
    fn pattern_switch_playing_is_queued_to_loop_boundary() {
        let mut seq = Sequencer::new();
        seq.set_playing(true);
        let sr = 48_000.0;
        let bar = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar / 2); // half a loop
        seq.select_pattern(2);
        assert_eq!(seq.current_pattern(), 0, "still on 0 mid-loop");
        assert_eq!(seq.queued_pattern(), Some(2));
        seq.process_block(2.0, 120.0, sr, bar); // crosses the loop boundary
        assert_eq!(seq.current_pattern(), 2, "switched at the loop boundary");
    }

    #[test]
    fn editing_toggles_steps() {
        let mut seq = Sequencer::new();
        assert!(!seq.step_on(8, 1));
        assert!(seq.toggle_step(8, 1));
        assert!(seq.step_on(8, 1));
        seq.set_step(8, 1, false);
        assert!(!seq.step_on(8, 1));
    }
}
