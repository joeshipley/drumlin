//! MIDI export (M11 c6) — a pattern as a Standard MIDI File, TR-909-software
//! style: click, the file is revealed in Finder, drag it onto a DAW track.
//!
//! The export bakes ONE pass of the pattern exactly as the engine plays loop 0:
//! swing (pattern or per-track override), groove-template offsets, micro
//! nudges, seeded humanize (timing + velocity — deterministic per the GROOVE
//! LOCK, so "as heard" IS reproducible), conditions gated at loop 0 / fill off,
//! the per-cell probability rolls, and ratchets expanded into real notes with
//! their velocity ramps. Accents are folded into velocity (MIDI has no accent
//! lane). What cannot ride a static region: p-locks, choke behavior, per-seed
//! drift — the notes are portable, the SOUND stays in the instrument.
//!
//! Notes land on the General MIDI drum map Drumlin already speaks
//! ([`percussion_core::track_for_note`]) — so a region drives Drumlin 1:1, and
//! any other GM-mapped drum instrument too. No tempo meta is written: ticks are
//! musical time, so the region conforms to the host project's tempo.

use percussion_core::rng::{mix_seed, XorShift32};
use percussion_core::{Pattern, MAX_STEPS, MAX_TRACKS};
use std::path::PathBuf;

/// Ticks per quarter note in the exported file.
pub const PPQ: u32 = 480;
/// One 16th step, in ticks.
const STEP_TICKS: f64 = PPQ as f64 / 4.0;
/// Note gate for plain hits (drums ignore length; 1/32 keeps regions tidy).
const GATE_TICKS: u32 = PPQ / 8;

/// Track -> GM note, the inverse of the kit's `track_for_note` map (pinned by
/// test): KICK SUB SNARE CLAP RIM CLHAT OPHAT RIDE TOMLO TOMHI COWBELL ZAP.
pub const TRACK_NOTE: [u8; MAX_TRACKS] = [36, 35, 38, 39, 37, 42, 46, 51, 45, 50, 56, 60];

/// `~/Music/Drumlin` (falling back to the home dir if no Music folder).
pub fn export_dir() -> Option<PathBuf> {
    let u = directories::UserDirs::new()?;
    let music = u.audio_dir().map(|d| d.to_path_buf()).unwrap_or_else(|| u.home_dir().join("Music"));
    Some(music.join("Drumlin"))
}

/// Render one baked pass of `p` as a complete SMF (format 0, one track).
pub fn pattern_to_midi(p: &Pattern) -> Vec<u8> {
    // (tick, on, note, vel); offs sort before ons at the same tick so a ratchet
    // can never overlap two sounding copies of one note.
    let mut ev: Vec<(u32, bool, u8, u8)> = Vec::new();

    let plen = (p.length.max(1) as usize).min(MAX_STEPS);
    let h = p.humanize as f32 / 100.0;
    for t in 0..MAX_TRACKS {
        let track = &p.tracks[t];
        if track.muted {
            continue;
        }
        let note = TRACK_NOTE[t];
        let slen = (track.length.max(1) as usize).min(MAX_STEPS);
        let swing = if track.swing >= 0 { track.swing as f32 } else { p.swing as f32 };
        let swing_frac = (swing - 50.0).max(0.0) / 50.0;
        for g in 0..plen {
            let s = g % slen; // polymeter: the lane wraps inside the pattern bar
            let st = &track.steps[s];
            if !st.on {
                continue;
            }
            // The engine's loop-0 gates, verbatim: condition at (loop 0, fill
            // off) and the frozen per-cell probability roll.
            if !st.condition.passes(0, false) {
                continue;
            }
            if st.probability < 100 {
                let mut r = XorShift32::new(mix_seed(p.seed, t as u32, s as u32, 0));
                if r.next_below(100) >= st.probability as u32 {
                    continue;
                }
            }
            // Velocity: step x lane level x seeded humanize, accent folded in
            // (MIDI has no accent lane; the boost mirrors the voices' feel).
            let mut vel = (st.velocity as f32 / 127.0) * (track.level as f32 / 127.0);
            if p.humanize > 0 {
                let mut rv = XorShift32::new(mix_seed(p.seed, t as u32, s as u32, 1));
                vel = (vel * (1.0 + rv.next_bipolar() * h * 0.3)).clamp(0.0, 1.0);
            }
            if st.accent {
                vel *= 1.25;
            }
            let vel = ((vel * 127.0).round() as i32).clamp(1, 127) as u8;

            // Timing: swing on odd grid positions + groove template + micro
            // (1/384-beat ticks, signed) + seeded humanize jitter, all in ticks.
            let swing_off = if g % 2 == 1 { swing_frac as f64 } else { 0.0 };
            let groove_off =
                (p.groove.offset_frac(g) * (p.groove_amount as f32 / 100.0)) as f64;
            let mut off = (swing_off + groove_off) * STEP_TICKS;
            off += st.micro as f64 * (PPQ as f64 / 384.0);
            if p.humanize > 0 {
                let mut rt = XorShift32::new(mix_seed(p.seed, t as u32, s as u32, 2));
                off += (rt.next_bipolar() * h * 0.04) as f64 * STEP_TICKS;
            }
            let base = g as f64 * STEP_TICKS;

            let ratchet = st.ratchet.clamp(1, 8) as u32;
            if ratchet <= 1 {
                let tick = (base + off).round().max(0.0) as u32;
                ev.push((tick, true, note, vel));
                ev.push((tick + GATE_TICKS, false, note, 0));
            } else {
                // The engine's roll: sub-hits spread over the room left between
                // the (swung) start and the next boundary, ramped velocities.
                let span = (STEP_TICKS - off).max(STEP_TICKS * 0.25);
                let spacing = span / ratchet as f64;
                let gate = ((spacing as u32).saturating_sub(1)).max(1).min(GATE_TICKS);
                for k in 0..ratchet {
                    let kv = ratchet_vel(vel, k, ratchet, st.ratchet_ramp);
                    let tick = (base + off + k as f64 * spacing).round().max(0.0) as u32;
                    ev.push((tick, true, note, kv));
                    ev.push((tick + gate, false, note, 0));
                }
            }
        }
    }
    // Offs before ons at the same tick.
    ev.sort_by_key(|&(tick, on, note, _)| (tick, on, note));

    // ---- serialize: MThd + one MTrk on channel 10 (index 9, the GM drum ch) ----
    let mut trk: Vec<u8> = Vec::new();
    trk.extend_from_slice(&[0x00, 0xFF, 0x03]); // track name
    let name = b"Drumlin";
    trk.push(name.len() as u8);
    trk.extend_from_slice(name);
    let mut last = 0u32;
    for &(tick, on, note, vel) in &ev {
        vlq(&mut trk, tick - last);
        last = tick;
        trk.push(if on { 0x99 } else { 0x89 });
        trk.push(note & 0x7F);
        trk.push(if on { vel & 0x7F } else { 0 });
    }
    // End the bar cleanly so consecutive drags tile: pad to the pattern length.
    let bar_end = (plen as f64 * STEP_TICKS).round() as u32;
    vlq(&mut trk, bar_end.saturating_sub(last));
    trk.extend_from_slice(&[0xFF, 0x2F, 0x00]); // end of track

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // format 0
    out.extend_from_slice(&1u16.to_be_bytes()); // one track
    out.extend_from_slice(&(PPQ as u16).to_be_bytes());
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(trk.len() as u32).to_be_bytes());
    out.extend_from_slice(&trk);
    out
}

/// The engine's ratchet velocity ramp (-100 flam .. +100 build), in MIDI units.
fn ratchet_vel(base: u8, k: u32, n: u32, ramp: i8) -> u8 {
    if n <= 1 {
        return base;
    }
    let pos = k as f32 / (n - 1) as f32; // 0..1 across the roll
    let shape = ramp as f32 / 100.0; // -1 flam .. +1 build
    let gain = 1.0 + shape * (pos - 0.5) * 1.2;
    ((base as f32 * gain).round() as i32).clamp(1, 127) as u8
}

/// MIDI variable-length quantity.
fn vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut stack = [0u8; 4];
    let mut n = 0;
    loop {
        stack[n] = (v & 0x7F) as u8;
        v >>= 7;
        n += 1;
        if v == 0 {
            break;
        }
    }
    while n > 1 {
        n -= 1;
        out.push(stack[n] | 0x80);
    }
    out.push(stack[0]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use percussion_core::track_for_note;

    #[test]
    fn track_note_map_matches_the_kit() {
        // The export map must stay the exact inverse of the kit's MIDI-in map,
        // or a dragged region wouldn't drive Drumlin 1:1.
        for (t, &note) in TRACK_NOTE.iter().enumerate() {
            assert_eq!(track_for_note(note), Some(t), "note {note} must map back to track {t}");
        }
    }

    #[test]
    fn vlq_known_vectors() {
        let enc = |v: u32| {
            let mut o = Vec::new();
            vlq(&mut o, v);
            o
        };
        assert_eq!(enc(0x00), vec![0x00]);
        assert_eq!(enc(0x40), vec![0x40]);
        assert_eq!(enc(0x7F), vec![0x7F]);
        assert_eq!(enc(0x80), vec![0x81, 0x00]);
        assert_eq!(enc(0x2000), vec![0xC0, 0x00]);
        assert_eq!(enc(0x0FFF_FFFF), vec![0xFF, 0xFF, 0xFF, 0x7F]);
    }

    /// Minimal SMF structural parse + note extraction for the tests.
    fn parse(bytes: &[u8]) -> Vec<(u32, u8, u8)> {
        assert_eq!(&bytes[0..4], b"MThd");
        assert_eq!(u32::from_be_bytes(bytes[4..8].try_into().unwrap()), 6);
        assert_eq!(u16::from_be_bytes(bytes[8..10].try_into().unwrap()), 0, "format 0");
        assert_eq!(u16::from_be_bytes(bytes[10..12].try_into().unwrap()), 1, "one track");
        assert_eq!(u16::from_be_bytes(bytes[12..14].try_into().unwrap()) as u32, PPQ);
        assert_eq!(&bytes[14..18], b"MTrk");
        let len = u32::from_be_bytes(bytes[18..22].try_into().unwrap()) as usize;
        let trk = &bytes[22..];
        assert_eq!(trk.len(), len, "track length must cover exactly the payload");
        assert_eq!(&trk[len - 3..], &[0xFF, 0x2F, 0x00], "end-of-track meta");
        // Walk events, collecting NoteOns as (tick, note, vel).
        let mut ons = Vec::new();
        let (mut i, mut tick) = (0usize, 0u32);
        while i < len {
            let mut delta = 0u32;
            loop {
                let b = trk[i];
                i += 1;
                delta = (delta << 7) | (b & 0x7F) as u32;
                if b & 0x80 == 0 {
                    break;
                }
            }
            tick += delta;
            match trk[i] {
                0xFF => {
                    let (ty, l) = (trk[i + 1], trk[i + 2] as usize);
                    i += 3 + l;
                    if ty == 0x2F {
                        break;
                    }
                }
                0x99 => {
                    ons.push((tick, trk[i + 1], trk[i + 2]));
                    i += 3;
                }
                0x89 => i += 3,
                other => panic!("unexpected event byte {other:#x}"),
            }
        }
        ons
    }

    #[test]
    fn neutral_demo_exports_exactly_its_hits() {
        let p = Pattern::neutral_demo();
        let ons = parse(&pattern_to_midi(&p));
        // Demo: kick x5(4 + accent dup? no: 4 steps) — count on-steps directly.
        let expected: usize = (0..MAX_TRACKS)
            .map(|t| (0..16).filter(|&s| p.tracks[t].steps[s].on).count())
            .sum();
        assert_eq!(ons.len(), expected, "every sounding step exports once");
        // The downbeat kick lands at tick 0 on GM 36; the snare backbeat at
        // step 4 = one quarter = PPQ ticks.
        assert!(ons.contains(&(0, 36, {
            // vel: 120 accented -> (120/127)*1.25*127 clamped
            let v = ((120.0 / 127.0) * 1.25 * 127.0_f32).round() as i32;
            v.clamp(1, 127) as u8
        })));
        assert!(ons.iter().any(|&(tick, note, _)| tick == PPQ && note == 38));
    }

    #[test]
    fn swing_micro_and_ratchets_bake_into_ticks() {
        let mut p = Pattern::default();
        p.swing = 66; // (66-50)/50 = 0.32 of a step on odd grid positions
        p.tracks[5].steps[1].on = true; // odd position -> swung
        p.tracks[5].steps[1].velocity = 100;
        p.tracks[0].steps[0].on = true;
        p.tracks[0].steps[0].velocity = 100;
        p.tracks[0].steps[0].micro = -24; // pulls a quarter-step early, clamps at 0
        p.tracks[2].steps[4].on = true;
        p.tracks[2].steps[4].velocity = 100;
        p.tracks[2].steps[4].ratchet = 4;
        let ons = parse(&pattern_to_midi(&p));
        // Swung hat: 120 + 0.32*120 = 158.4 -> 158.
        assert!(ons.iter().any(|&(t, n, _)| n == 42 && t == 158), "swing must bake: {ons:?}");
        // Negative micro at step 0 clamps to tick 0 (nothing before the region).
        assert!(ons.iter().any(|&(t, n, _)| n == 36 && t == 0));
        // Ratchet 4 on the snare: four 38s inside step 4's span, evenly spaced.
        let snares: Vec<u32> =
            ons.iter().filter(|&&(_, n, _)| n == 38).map(|&(t, _, _)| t).collect();
        assert_eq!(snares.len(), 4, "ratchet expands into real notes");
        assert_eq!(snares, vec![480, 510, 540, 570]);
    }

    #[test]
    fn conditions_and_probability_bake_loop_zero() {
        use percussion_core::TrigCondition;
        let mut p = Pattern::default();
        for s in 0..4 {
            p.tracks[0].steps[s].on = true;
            p.tracks[0].steps[s].velocity = 100;
        }
        p.tracks[0].steps[1].condition = TrigCondition::NotFirst; // silent on loop 0
        p.tracks[0].steps[2].condition = TrigCondition::Fill; // fill off -> silent
        p.tracks[0].steps[3].probability = 0; // never fires
        let ons = parse(&pattern_to_midi(&p));
        assert_eq!(ons.len(), 1, "only the unconditional step survives loop 0: {ons:?}");
        assert_eq!(ons[0].0, 0);
    }
}
