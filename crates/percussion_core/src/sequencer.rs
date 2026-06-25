//! The step sequencer (design §4). M2 scope: a single pattern of fixed-capacity
//! `Copy` POD structs, a sample-accurate clock driven off the host transport's
//! quarter-note position, and a trigger list the kit drains. Per-step p-locks,
//! probability, ratchets, conditionals, swing and polymeter arrive at M5 — the
//! data model already reserves room for them.

use crate::{Trigger, MAX_STEPS, MAX_TRACKS};

/// One step on one track. Fixed-size, `Copy`, no heap. (M2 uses on/velocity/
/// accent; the richer Elektron fields land at M5.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub on: bool,
    pub velocity: u8, // 0..=127
    pub accent: bool,
}

impl Default for Step {
    fn default() -> Self {
        Self { on: false, velocity: 100, accent: false }
    }
}

/// One drum track's lane. `length` < pattern length seeds polymeter (M5); for
/// M2 every track is the pattern length.
#[derive(Clone, Copy, Debug)]
pub struct Track {
    pub steps: [Step; MAX_STEPS],
    pub length: u8,
    pub muted: bool,
}

impl Default for Track {
    fn default() -> Self {
        Self { steps: [Step::default(); MAX_STEPS], length: 16, muted: false }
    }
}

/// A pattern binds the tracks plus global feel. `Copy`, so an undo snapshot is
/// one memcpy (design §4.4).
#[derive(Clone, Copy, Debug)]
pub struct Pattern {
    pub tracks: [Track; MAX_TRACKS],
    pub length: u8,     // active steps, 1..=64
    pub resolution: u8, // steps per bar (16 = 16ths)
}

impl Default for Pattern {
    fn default() -> Self {
        Self { tracks: [Track::default(); MAX_TRACKS], length: 16, resolution: 16 }
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

/// Max triggers the sequencer can emit in one block (fixed, allocation-free).
/// Ample for any sane host block (12 tracks × ~21 step boundaries). Pathological
/// offline-bounce blocks (>128k samples) could exceed it; a debug build asserts,
/// and sub-block chunking in the plugin lands at M5 to remove the cap entirely.
const MAX_PENDING: usize = 256;

/// The runtime sequencer: holds a pattern, the continuous playhead, and a
/// per-block list of emitted triggers. Lives on the audio thread; edits arrive
/// from the GUI via the plugin's lock-free edit ring.
pub struct Sequencer {
    pub pattern: Pattern,
    playing: bool,
    /// Continuous absolute step position (re-anchored to the host each block).
    playhead_steps: f64,
    /// Floor of the last step boundary we emitted (for crossing detection).
    prev_floor: i64,
    pending: [Trigger; MAX_PENDING],
    pending_len: usize,
}

impl Default for Sequencer {
    fn default() -> Self {
        Self::new()
    }
}

impl Sequencer {
    pub fn new() -> Self {
        Self {
            pattern: Pattern::neutral_demo(),
            playing: false,
            playhead_steps: 0.0,
            prev_floor: -1,
            pending: [Trigger { offset: 0, track: 0, velocity: 0.0, accent: false }; MAX_PENDING],
            pending_len: 0,
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Current step index under the playhead (for the GUI moving column), or
    /// `None` when stopped.
    pub fn current_step(&self) -> Option<usize> {
        if !self.playing {
            return None;
        }
        let len = self.pattern.length.max(1) as i64;
        Some((((self.prev_floor % len) + len) % len) as usize)
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

    pub fn reset_playhead(&mut self) {
        self.playhead_steps = 0.0;
        self.prev_floor = -1;
    }

    /// Walk one process block. `pos_qn` is the transport position (quarter notes)
    /// at the block's first sample. Fills `pending` with the triggers that fire
    /// in this block, each tagged with its sample offset. Allocation-free.
    pub fn process_block(&mut self, pos_qn: f64, tempo: f64, sr: f64, block_len: usize) {
        self.pending_len = 0;
        if !self.playing || block_len == 0 || tempo <= 0.0 || sr <= 0.0 {
            return;
        }
        let steps_per_qn = self.pattern.resolution.max(1) as f64 / 4.0;
        let host_step = pos_qn * steps_per_qn;

        // The transport position is authoritative: re-anchor the playhead to it
        // every block, which eliminates inter-block drift. A backward move (loop)
        // or a jump larger than half a step is a relocate / loop / first block —
        // also reset prev_floor so the landing step re-fires.
        let discontinuity = host_step + 1.0e-6 < self.playhead_steps
            || (host_step - self.playhead_steps).abs() > 0.5;
        self.playhead_steps = host_step;
        if discontinuity {
            self.prev_floor = host_step.floor() as i64 - 1;
        }

        let steps_per_sample = (tempo / 60.0 / sr) * steps_per_qn;

        // Test the step boundary at the START of each sample, then advance — so a
        // boundary landing exactly on sample N fires at offset N (in the block
        // that contains sample N), with no off-by-one at the block edge.
        for i in 0..block_len {
            let fl = self.playhead_steps.floor() as i64;
            if fl > self.prev_floor {
                self.prev_floor = fl;
                self.emit_step(fl, i as u32);
            }
            self.playhead_steps += steps_per_sample;
        }
    }

    fn emit_step(&mut self, fl: i64, offset: u32) {
        for t in 0..MAX_TRACKS {
            let track = &self.pattern.tracks[t];
            if track.muted {
                continue;
            }
            // Each track wraps by its OWN length, derived from the absolute step
            // position — true polymeter (design §4.2), correct even when the
            // track length does not divide the pattern length.
            let slen = (track.length.max(1) as i64).min(MAX_STEPS as i64);
            let step = (((fl % slen) + slen) % slen) as usize;
            let s = track.steps[step];
            if s.on {
                debug_assert!(
                    self.pending_len < self.pending.len(),
                    "sequencer pending overflow (>{} triggers in one block) — block too \
                     large; sub-block chunking lands at M5",
                    self.pending.len()
                );
                if self.pending_len < self.pending.len() {
                    self.pending[self.pending_len] = Trigger {
                        offset,
                        track: t as u8,
                        velocity: (s.velocity as f32 / 127.0).clamp(0.0, 1.0),
                        accent: s.accent,
                    };
                    self.pending_len += 1;
                }
            }
        }
    }

    /// Triggers emitted by the most recent `process_block`, ordered by offset.
    pub fn pending(&self) -> &[Trigger] {
        &self.pending[..self.pending_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let trigs = seq.pending();
        assert!(
            trigs.iter().any(|t| t.track == 0 && t.offset == 0),
            "kick should fire on the downbeat at offset 0: {trigs:?}"
        );
    }

    #[test]
    fn one_bar_emits_the_expected_hit_count() {
        // 120 BPM, 16th resolution: one bar = 4 quarters = 0.5 step per 16th...
        // walk a whole bar in one big block and count kick hits (4-on-the-floor).
        let mut seq = Sequencer::new();
        seq.set_playing(true);
        let sr = 48_000.0;
        // samples in one bar at 120 BPM = 4 beats * (60/120) s * sr = 2.0s * 48k
        let bar_samples = (4.0 * 0.5 * sr) as usize;
        seq.process_block(0.0, 120.0, sr, bar_samples);
        let kicks = seq.pending().iter().filter(|t| t.track == 0).count();
        assert_eq!(kicks, 4, "four-on-the-floor = 4 kicks per bar, got {kicks}");
    }

    #[test]
    fn determinism_same_transport_same_triggers() {
        let run = || {
            let mut seq = Sequencer::new();
            seq.set_playing(true);
            let mut all = Vec::new();
            let mut qn = 0.0;
            for _ in 0..16 {
                seq.process_block(qn, 120.0, 48_000.0, 512);
                all.extend_from_slice(seq.pending());
                qn += 120.0 / 60.0 * (512.0 / 48_000.0); // advance transport
            }
            all
        };
        assert_eq!(run(), run(), "same transport must yield identical triggers");
    }

    #[test]
    fn polymeter_track_wraps_by_its_own_length() {
        // A length-3 track with only step 0 on must fire every 3 absolute steps,
        // independent of the 16-step pattern length.
        let mut seq = Sequencer::new();
        seq.pattern.tracks[11] = Track::default();
        seq.pattern.tracks[11].length = 3;
        seq.pattern.tracks[11].steps[0].on = true;
        seq.set_playing(true);
        let sr = 48_000.0;
        // 12 absolute steps at 120 BPM / 16ths = 3 quarter notes.
        let samples = (12.0 / 4.0 * (60.0 / 120.0) * sr) as usize;
        seq.process_block(0.0, 120.0, sr, samples);
        let hits = seq.pending().iter().filter(|t| t.track == 11).count();
        // step 0 fires at absolute steps 0, 3, 6, 9 within [0, 12).
        assert_eq!(hits, 4, "length-3 track should fire every 3 steps, got {hits}");
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
