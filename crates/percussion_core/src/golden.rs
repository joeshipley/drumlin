//! Bit-exact golden-render regression anchors (design §7.4). The **Neutral kit**
//! is the frozen reference: any *unintended* change to the voice / sequencer /
//! tail / bus math reproduces these renders byte-for-byte, so a refactor that
//! shifts a single sample fails loudly.
//!
//! The goldens are checked-in binary blobs under `crates/percussion_core/golden/`
//! (interleaved L/R `f32` bit patterns, little-endian). They are loaded at test
//! runtime (not `include_bytes!`) so the crate always compiles. Regenerate them
//! **deliberately** — only when an intended sonic change lands (new voices at M4,
//! tuning, etc.) — with:
//!
//! ```text
//! cargo test -p percussion_core regenerate_goldens -- --ignored --nocapture
//! ```
//!
//! then commit the updated `golden/*.bin`. Bit-exactness is pinned to this
//! toolchain/opt-level (debug `cargo test`), exactly like Esker's GOLDEN_DEFAULT.

use crate::{DrumKit, Sequencer};

const SR: f32 = 48_000.0;
const ONESHOT_SAMPLES: usize = 16_000;
/// One bar at 120 BPM = 2.0 s.
const BAR_SAMPLES: usize = 96_000;

fn push_frame(bytes: &mut Vec<u8>, l: f32, r: f32) {
    bytes.extend_from_slice(&l.to_bits().to_le_bytes());
    bytes.extend_from_slice(&r.to_bits().to_le_bytes());
}

/// One voice's default one-shot through the full Neutral signal path (tail + bus),
/// from a freshly-built kit.
fn render_voice_oneshot(track: usize) -> Vec<u8> {
    let mut kit = DrumKit::neutral(SR);
    kit.trigger(track, 1.0, false, &[]);
    let mut bytes = Vec::with_capacity(ONESHOT_SAMPLES * 8);
    for _ in 0..ONESHOT_SAMPLES {
        let (l, r) = kit.render();
        push_frame(&mut bytes, l, r);
    }
    bytes
}

/// The Neutral kit playing pattern A1 (the demo groove) for one bar at 120 BPM /
/// 48 kHz — the top-level anchor exercising the sequencer, choke groups, every
/// voice and the full bus chain together. Mirrors the plugin's process loop.
fn render_pattern_oneshot() -> Vec<u8> {
    let mut kit = DrumKit::neutral(SR);
    let mut seq = Sequencer::new();
    seq.set_playing(true);
    let tempo = 120.0_f64;
    let block = 512usize;
    let mut pos_qn = 0.0_f64;
    let mut produced = 0usize;
    let mut bytes = Vec::with_capacity(BAR_SAMPLES * 8);
    while produced < BAR_SAMPLES {
        let n = block.min(BAR_SAMPLES - produced);
        seq.process_block(pos_qn, tempo, SR as f64, n);
        let trigs: Vec<_> = seq.pending().to_vec();
        let mut ti = 0;
        for i in 0..n {
            while ti < trigs.len() && trigs[ti].offset as usize <= i {
                // Render through the exact production sequencer path (the seeded
                // drift randoms + the mod sources on the Trigger). All tracks
                // default to drift = 0 and the matrix is all-Off, so this is
                // byte-identical to a bare trigger — which is the point: the
                // byte-exact golden now also guards the drift=0 / empty-matrix
                // no-op of the full trigger_seq path.
                kit.trigger_seq(&trigs[ti]);
                ti += 1;
            }
            let (l, r) = kit.render();
            push_frame(&mut bytes, l, r);
        }
        pos_qn += (tempo / 60.0) * (n as f64 / SR as f64);
        produced += n;
    }
    bytes
}

const GOLDENS: &[(&str, fn() -> Vec<u8>)] = &[
    ("kick.bin", || render_voice_oneshot(0)),
    ("sub.bin", || render_voice_oneshot(1)),
    ("snare.bin", || render_voice_oneshot(2)),
    ("clap.bin", || render_voice_oneshot(3)),
    ("rim.bin", || render_voice_oneshot(4)),
    ("chat.bin", || render_voice_oneshot(5)),
    ("ohat.bin", || render_voice_oneshot(6)),
    ("ride.bin", || render_voice_oneshot(7)),
    ("tomlo.bin", || render_voice_oneshot(8)),
    ("tomhi.bin", || render_voice_oneshot(9)),
    ("cowbell.bin", || render_voice_oneshot(10)),
    ("zap.bin", || render_voice_oneshot(11)),
    ("pattern_a1.bin", render_pattern_oneshot),
];

fn golden_path(name: &str) -> String {
    format!("{}/golden/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn assert_matches_golden(name: &str, fresh: &[u8]) {
    let path = golden_path(name);
    let golden = std::fs::read(&path).unwrap_or_else(|_| {
        panic!("missing golden {path}; run `cargo test -p percussion_core regenerate_goldens -- --ignored`")
    });
    assert_eq!(
        golden.len(),
        fresh.len(),
        "golden {name} length changed ({} -> {})",
        golden.len(),
        fresh.len()
    );
    for (i, (g, f)) in golden.chunks_exact(4).zip(fresh.chunks_exact(4)).enumerate() {
        if g != f {
            let gb = f32::from_bits(u32::from_le_bytes(g.try_into().unwrap()));
            let fb = f32::from_bits(u32::from_le_bytes(f.try_into().unwrap()));
            panic!(
                "golden {name} diverged at f32 #{i} (frame {}, {} ch): expected {gb} got {fb}.\n\
                 If this change is INTENTIONAL, regenerate: \
                 cargo test -p percussion_core regenerate_goldens -- --ignored",
                i / 2,
                if i % 2 == 0 { "L" } else { "R" }
            );
        }
    }
}

#[test]
#[ignore = "regenerates golden/*.bin; run only after an intended sonic change"]
fn regenerate_goldens() {
    let dir = format!("{}/golden", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, render) in GOLDENS {
        let data = render();
        let path = format!("{dir}/{name}");
        std::fs::write(&path, &data).unwrap();
        println!("wrote {path} ({} bytes)", data.len());
    }
}

#[test]
fn kick_matches_golden() {
    assert_matches_golden("kick.bin", &render_voice_oneshot(0));
}
#[test]
fn sub_matches_golden() {
    assert_matches_golden("sub.bin", &render_voice_oneshot(1));
}
#[test]
fn snare_matches_golden() {
    assert_matches_golden("snare.bin", &render_voice_oneshot(2));
}
#[test]
fn clap_matches_golden() {
    assert_matches_golden("clap.bin", &render_voice_oneshot(3));
}
#[test]
fn rim_matches_golden() {
    assert_matches_golden("rim.bin", &render_voice_oneshot(4));
}
#[test]
fn closed_hat_matches_golden() {
    assert_matches_golden("chat.bin", &render_voice_oneshot(5));
}
#[test]
fn open_hat_matches_golden() {
    assert_matches_golden("ohat.bin", &render_voice_oneshot(6));
}
#[test]
fn ride_matches_golden() {
    assert_matches_golden("ride.bin", &render_voice_oneshot(7));
}
#[test]
fn tom_lo_matches_golden() {
    assert_matches_golden("tomlo.bin", &render_voice_oneshot(8));
}
#[test]
fn tom_hi_matches_golden() {
    assert_matches_golden("tomhi.bin", &render_voice_oneshot(9));
}
#[test]
fn cowbell_matches_golden() {
    assert_matches_golden("cowbell.bin", &render_voice_oneshot(10));
}
#[test]
fn zap_matches_golden() {
    assert_matches_golden("zap.bin", &render_voice_oneshot(11));
}
#[test]
fn default_pattern_matches_golden() {
    assert_matches_golden("pattern_a1.bin", &render_pattern_oneshot());
}

#[test]
fn pattern_render_is_finite_and_limited() {
    // Robust (non-bit-exact) smoke guard: the anchor render must be finite,
    // audible, and held under the true-peak ceiling by the bus limiter.
    let bytes = render_pattern_oneshot();
    let mut peak = 0.0_f32;
    for c in bytes.chunks_exact(4) {
        let v = f32::from_bits(u32::from_le_bytes(c.try_into().unwrap()));
        assert!(v.is_finite(), "NaN/inf in the default pattern render");
        peak = peak.max(v.abs());
    }
    assert!(peak > 0.1, "default pattern should be audible, peak={peak}");
    assert!(peak <= 1.02, "bus limiter should hold ~0 dBFS, peak={peak}");
}
