//! `DrumKit` — the 12-track voice rack with per-voice tails, choke groups, and
//! the shared dynamics bus (design §3.1/§3.2/§5.3). Signal flow per track:
//! voice engine → `VoiceTail` (drive → CS-80 filter → level → pan) → kit sum →
//! `DrumBus` (glue → true-peak limiter) → output. M3's Neutral kit voices Kick,
//! Snare, Clap and Closed/Open Hat at their design indices; the other 7 tracks
//! are `Silent` until M4.

use crate::bus::DrumBus;
use crate::tail::VoiceTail;
use crate::voice::{
    ClapVoice, CowbellVoice, HatVoice, KickVoice, RimVoice, SnareVoice, TomVoice, Voice, ZapVoice,
};
use crate::MAX_TRACKS;

/// Default track layout (design §3.2): index -> role.
/// 0 KICK · 1 KICK2 · 2 SNARE · 3 CLAP · 4 RIM · 5 CLHAT · 6 OPHAT ·
/// 7 RIDE · 8 TOM_LO · 9 TOM_HI · 10 PERC · 11 SAMPLE
pub struct DrumKit {
    voices: [Voice; MAX_TRACKS],
    tails: [VoiceTail; MAX_TRACKS],
    /// 0 = no group; 1..=4 = groups A..D. A trigger chokes other sounding
    /// voices sharing its group.
    choke_group: [u8; MAX_TRACKS],
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

        let mut choke_group = [0u8; MAX_TRACKS];
        choke_group[5] = 1; // closed hat -> group A
        choke_group[6] = 1; // open hat   -> group A (closed chokes open)

        Self { voices, tails, choke_group, bus: DrumBus::neutral(sr), sr }
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

    /// Trigger a track. Applies the choke broadcast (allocation-free, O(12)),
    /// then triggers the voice.
    pub fn trigger(&mut self, track: usize, velocity: f32, accent: bool) {
        if track >= MAX_TRACKS {
            return;
        }
        let g = self.choke_group[track];
        if g != 0 {
            for i in 0..MAX_TRACKS {
                if i != track && self.choke_group[i] == g {
                    self.voices[i].choke();
                }
            }
        }
        self.voices[track].trigger(velocity, accent);
    }

    /// Sum all voices through their tails, then the glue/limiter bus, to a stereo
    /// frame.
    pub fn render(&mut self) -> (f32, f32) {
        let mut l = 0.0;
        let mut r = 0.0;
        for (v, t) in self.voices.iter_mut().zip(self.tails.iter_mut()) {
            let (vl, vr) = v.render();
            let (tl, tr) = t.process(vl, vr);
            l += tl;
            r += tr;
        }
        self.bus.process(l, r)
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
        kit.trigger(0, 1.0, true); // kick
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
            kit.trigger(6, 1.0, false); // open hat
            for _ in 0..256 {
                kit.render();
            }
            if choke {
                kit.trigger(5, 1.0, false); // closed hat chokes open
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
            kit.trigger(t, 1.0, true);
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
}
