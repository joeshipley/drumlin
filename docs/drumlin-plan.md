# Drumlin — Build Plan

*Companion to [`drumlin-design.md`](drumlin-design.md). This document is the executable plan: it adapts the design to the agreed workspace constraint, resolves the design's open questions, fixes the concrete file/crate layout, and defines the per-milestone deliverables and verification gates.*

Last updated: 2026-06-24.

---

## 0. The one structural deviation from the design doc

The design doc (§7.2) assumes Drumlin is built **inside** the `customvst` monorepo: it would *move* `dsp_core` out of Esker's `synth_core`, add a `drumlin` crate to the customvst workspace, and have one `xtask` bundle both plugins.

**We are not doing that.** Per explicit instruction, Drumlin is a **standalone Cargo workspace at `/Users/joes/drumlin`**, and **Esker (`/Users/joes/customvst`) is reference-only and never modified.**

The adaptation is mechanical and faithful:

| Design doc says | We do instead |
|---|---|
| *Move* `dsp_core` out of `synth_core` (M1) | **Depend on the published `synth_core` crate** (git, pinned `rev`). See the update below. |
| Esker's golden test proves the move was zero-change | We keep our **own** golden coverage in Drumlin; `synth_core` carries Esker's bit-exact guard. |
| `xtask` bundles `customvst` + `drumlin` | Drumlin's own `xtask` bundles `drumlin`. |
| `nih_plug_webview` shared from `customvst/vendor` | **Copy** the vendored fork into `drumlin/vendor/` so Drumlin is fully self-contained. |

**Update (synth_core adopted — the byte-copy discipline is retired):** The DSP core is now a standalone published crate, `git@github.com:joeshipley/synth_core.git`, consumed by both Esker and Drumlin. `percussion_core` depends on it as a **pinned git `rev`** (`eba0e714…`, `[net] git-fetch-with-cli = true` for SSH). The local `crates/dsp_core` copy was deleted and `use dsp_core::…` renamed to `use synth_core::…`. The switch was bit-transparent (all 12 voice + default-pattern goldens unchanged) — including the now-upstream triangle-oscillator denormal fix (a no-op for Drumlin's continuously-driven oscillators). **Collaboration:** never edit a local copy; change shared DSP in the `synth_core` repo (its golden guards Esker's default voice), push, then bump the `rev` in both Esker and Drumlin. Keep `synth_core` dependency-free + general-purpose; instrument-specific DSP (drum voices, the sequencer) stays in `percussion_core`.

---

## 1. Open questions (design §11) — resolved for the build

The design doc commits to most of these; here are the frozen answers we build against.

1. **Track count:** **12 fixed tracks.** `MAX_TRACKS = 12`. (Design committed.)
2. **`dsp_core` scope:** Copy these 13 modules from `synth_core/src` (all confirmed dependency-free, `std`-only, no synth-specific coupling): `filter, drive, delay, reverb, dynamics, phaser, chorus, lfo, envelope, mod_matrix, oscillator (carries `Noise`/`NoiseType`), wavetable, util`. Do **not** copy `voice, synth, arpeggiator, golden_default` (synth-specific). `mod_matrix` is copied verbatim now and gains percussion sources + voice-targeting in M6.
3. **P-lock budget:** `MAX_PLOCKS = 4` per step for v1. Revisit later.
4. **KIT vs GROOVE WORLD:** Keep **distinct** (KIT = sound+bus+macros; GROOVE WORLD = KIT + pattern + groove feel). Reuse Esker's `Scene`/`MacroDef` for both.
5. **Multi-out:** Defer to **M8**. MVP is stereo main out only.
6. **Neutral golden anchor:** Defined in M2/M3 by the default Kick/Snare/CHat/OHat patches; frozen *before* the M3 anchor render lands. Documented in `percussion_core/golden/README`.
7. **Sample layer in MVP:** Built-in **transient/click bank** (baked into the binary) ships at M2 for the kick "knock." User-sample drag-in stays in "Later."
8. **Sidechain PUMP keying:** Internal kick-trigger by default at v1; host sidechain aux input deferred (declared alongside multi-out in M8).
9. **Standalone preview clock:** Standalone falls back to a fixed default tempo (120 BPM) and an internal play clock; when hosted, the host transport always wins and the internal `>` PLAY is inert.
10. **AU codes:** `AU_MANUF_CODE=JShp` (shared author code — honored), `AU_INSTRUMENT_TYPE=aumu`, `AU_BUNDLE_ID=com.joeshipley.drumlin`. **Subtype reality (corrects the design doc):** clap-wrapper, when embedding a *prebuilt* `.clap` (the `MACOSX_EMBEDDED_CLAP_LOCATION` path we use), **derives** the AU subtype from the CLAP id and **ignores** `AU_SUBTYPE_CODE`. `com.joeshipley.drumlin` deterministically yields **`KV6m`**. So the registered AU is `aumu KV6m JShp`; `Drml` is informational only (clap-wrapper's `--explicit` path that honors it is incompatible with embedding a cargo-built clap, and forcing it would require nih-plug to expose the `clapwrapper/auv2` extension, which it doesn't). `scripts/build-au.sh` reads the real subtype back from the installed component and validates that. **`auval -v aumu KV6m JShp` → AU VALIDATION SUCCEEDED.**

---

## 2. Target workspace layout

```
/Users/joes/drumlin/
├─ Cargo.toml                  workspace: resolver=2, members, release profile, MSRV 1.87
├─ Cargo.lock
├─ bundler.toml               [drumlin] name = "Drumlin"
├─ rust-toolchain / MSRV      rust-version = "1.87" (workspace.package)
├─ .cargo/config.toml         xtask alias + resolver fallback (cloned from Esker)
├─ .gitignore                 /target, *.clap, *.component, /build, .DS_Store
├─ README.md
├─ LICENSE                    MIT (Joe Shipley)
├─ docs/
│  ├─ drumlin-design.md        (the design doc — already present)
│  └─ drumlin-plan.md          (this file)
├─ scripts/
│  └─ build-au.sh              cloned from Esker; Drumlin AU identity defaults
├─ vendor/
│  └─ nih-plug-webview/        copied from customvst/vendor (self-contained)
├─ xtask/
│  ├─ Cargo.toml               nih_plug_xtask (git)
│  └─ src/main.rs              delegates to nih_plug_xtask::main()
└─ crates/
   ├─ dsp_core/               NEW — zero-dependency shared primitives (copied from Esker)
   │  ├─ Cargo.toml            no dependencies
   │  └─ src/
   │     ├─ lib.rs             module decls + re-exports (DSP subset only)
   │     ├─ filter.rs drive.rs delay.rs reverb.rs dynamics.rs
   │     ├─ phaser.rs chorus.rs lfo.rs envelope.rs mod_matrix.rs
   │     ├─ oscillator.rs wavetable.rs util.rs
   │     └─ transient.rs       NEW (M3/M7) — bus transient shaper
   ├─ percussion_core/        NEW — drum voices + sequencer + kit model (zero-dep)
   │  ├─ Cargo.toml            depends only on dsp_core
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ pitch_env.rs resonator.rs metal_cluster.rs one_shot.rs clap_diffuser.rs
   │     ├─ voice/{kick,snare,hat,tom,cymbal,fm_perc,noise}.rs
   │     ├─ kit.rs             12 voices + bus assembly + choke groups
   │     ├─ sequencer.rs       steps, p-locks, probability, ratchet, euclid, swing, song
   │     ├─ bus.rs             drum-bus FX chain (re-points dsp_core modules)
   │     ├─ transient_bank.rs  baked click/tick one-shots
   │     └─ golden/            golden one-shots + default-pattern render fixtures
   └─ drumlin/                NEW — nih-plug plugin shell (sibling of customvst)
      ├─ Cargo.toml            crate-type ["cdylib","lib"]; nih_plug + webview + rtrb + serde + directories
      └─ src/
         ├─ lib.rs             Plugin impl, rings, viz, fx_reset, Action enum, editor()
         ├─ main.rs            standalone entry (nih_export_standalone)
         ├─ kits.rs            KITS (Scene/MacroDef reuse)
         ├─ worlds.rs          GROOVE WORLDS (kit + pattern + groove + macros)
         ├─ presets.rs         disk preset browser (directories crate)
         └─ gui/index.html     PRISM, restyled for the step-sequencer
```

### Pinned dependency facts (cloned from Esker so the family stays bit-compatible)
- Workspace `[profile.release]`: `lto = "thin"`, `codegen-units = 1`, `opt-level = 3`. MSRV `1.87`.
- `nih_plug` = git `https://github.com/robbert-vdh/nih-plug.git`, **rev `f36931f7af4646065488a9845d8f8c2f95252c23`** (pin in `Cargo.lock`), `default-features = false`, features `["assert_process_allocs","standalone"]` (drops GPL `vst3-sys`).
- `nih_plug_webview` = path `../../vendor/nih-plug-webview` (wry 0.35.1, baseview git).
- `rtrb = "0.2"`, `serde = "1" (derive)`, `serde_json = "1"`, `directories = "5"`.
- `.cargo/config.toml`: alias `xtask = "run --package xtask --release --"`; `[resolver] incompatible-rust-versions = "fallback"`.

---

## 3. Engineering invariants (inherited from Esker, enforced on every milestone)

1. **No allocations on the audio thread.** All buffers sized in `initialize()`/`set_sample_rate()`. `assert_process_allocs` feature on. Pre-build the wavetable bank in `initialize()`.
2. **Lock-free GUI↔audio via `rtrb` SPSC rings.** `kbd` ring (`u16` packed note), `cc` ring (`u64` packed CC-apply), plus audio→editor viz atomics (`VizState`). Pad audition is local-only, **never** host-recorded.
3. **`flush_denormal` on every recursive state write.** New percussion generators (pitch-env state, resonator poles, clap-diffuser taps, transient envelope followers) follow the same rule; each gets the `NaN.clamp` pitfall test.
4. **Panic-reset on KIT/pattern jumps** via an `fx_reset_pending: Arc<AtomicBool>`, drained once at block start to `clear()` every FX feedback buffer + silence voices.
5. **Sample-accurate sequencer timing** off the host transport PPQ; 384 PPQN master resolution; swing/micro offsets as exact sample deltas; phase-locked across loop boundaries.
6. **Inline tests everywhere** (`#[cfg(test)] mod tests { use super::* }`), pure `#[test]`/`assert_eq!`, golden constants checked in.

---

## 4. Milestones

**Status (2026-06-24):** M0, M1, M2, M3 (MVP), **M4 complete and verified.** Plus a usability add: a **SEQ enable toggle** (groovebox vs. pure MIDI-region/pad control) and host-transport play/stop sync. Workspace builds, `cargo test --workspace` green (139 `dsp_core` + 41 `percussion_core`), `Drumlin.clap`/`Drumlin.app` bundle, AU passes `auval`. M2 shipped Kick/Snare/Clap/Closed+Open-Hat synthesis, choke groups, the 16-step host-synced sequencer, GM note map, local pad audition, and a live editable PRISM grid (then an adversarial multi-agent review — 17 confirmed findings, real ones fixed). **M3** added the uniform per-voice tail (drive → CS-80 dual filter → level → "full-at-center" pan), the **glue → true-peak-limiter bus** ("glue is the headline"), and **froze the Neutral kit as the bit-exact golden anchor** (5 per-voice one-shots + a 1-bar default-pattern render under `crates/percussion_core/golden/`, regenerated only on intended sonic changes). Next: **M4** (the remaining 7 voices: toms, perc/rim/cowbell, ride/crash, FM-perc, noise/FX, sample) — which will deliberately re-freeze the golden.

**Deferred from the M2 review (intentional):**
- *dsp_core triangle-oscillator denormal flush* — the leaky integrator in `oscillator.rs` isn't `flush_denormal`d. It lives in `dsp_core`, which we keep **byte-identical to Esker** (§0); fixing it here would fork the family DSP. Track as an **upstream Esker item** instead.
- *Sequencer `MAX_PENDING` cap* — guarded by a debug-assert + documented; the real fix (sub-block chunking in the plugin) lands at **M5** with the full grid.
- *Per-sample `2.0.powf` in the kick* — negligible for one voice; revisit only if profiling flags it.

Each milestone lists **deliverables**, the **files** it touches, and its **verification gate** (must pass before the next milestone). Build/test commands in §5.

### MVP — a beat you can play in Logic

#### M0 — Scaffold ✅ gate: `cargo build --workspace` green; `cargo xtask bundle drumlin --release` produces `Drumlin.clap`; (stretch) `auval` passes
- Standalone workspace skeleton: root `Cargo.toml`, `.cargo/config.toml`, `.gitignore`, `bundler.toml`, `LICENSE`, `README.md`, `xtask/`, `scripts/build-au.sh` (Drumlin identity), `vendor/nih-plug-webview` (copied).
- `crates/drumlin`: minimal nih-plug Plugin that emits silence, returns a placeholder PRISM webview, declares stereo out + `MidiConfig::MidiCCs`, `nih_export_clap!` + `nih_export_standalone` in `main.rs`.
- A minimal `gui/index.html` with the PRISM shell (brand bar + empty grid) so the editor loads.

#### M1 — Port `dsp_core` (front-loaded; everything depends on it) ✅ — gate: `cargo test -p dsp_core` green
- Create `crates/dsp_core` and copy the 13 modules **verbatim** from Esker `synth_core/src`. Author `lib.rs` declaring exactly those modules and re-exporting their public API (DSP subset only — no `voice`/`synth`/`arp`).
- Bring every module's **inline tests** along; they are the proof the copy is faithful (the same `flush_denormal` NaN-pitfall test, the `mod_matrix` index-reconcile test, etc., must pass unchanged).
- `dsp_core/Cargo.toml` has **no dependencies**.

#### M2 — Core voices + a sequencer ✅ — gate: voices produce signal; choke test passes; sequencer locks to host transport; pad audition works
- `percussion_core` crate. New generators needed for these voices: `pitch_env.rs` (DAHD exp), `resonator.rs`, `metal_cluster.rs`, `one_shot.rs` + `transient_bank.rs` (baked click bank).
- Voices: **Kick, Snare, Closed Hat, Open Hat** (synthesis). Choke groups (CLHAT+OPHAT in group A). Mono-per-track retrigger.
- `sequencer.rs`: single 16-step pattern, `Step`/`Track`/`Pattern` POD structs (fixed-capacity, `Copy`), 384 PPQN clock off host transport, `Trigger` ring drained by the engine.
- Plugin shell: GM note map (C1 kick, D1 snare, F#1 CH, A#1 OH), on-screen 4×4 pad bank → `kbd` ring local audition.

#### M3 — Per-voice tail + minimal bus + **the Neutral golden anchor** ✅ — gate: per-voice golden one-shots + default-pattern golden render checked in and bit-exact
- Uniform per-voice tail: Level, Pan, Pitch, Decay, CS-80 dual filter, Drive, Send A/B stubs.
- Minimal bus chain reused from `dsp_core`: glue compressor + true-peak limiter (`Dynamics`). `transient.rs` shaper stub.
- **Neutral kit** defined and frozen. Land per-voice golden one-shots (16k stereo samples, `to_bits` compare) and the **default-pattern golden render** (Neutral plays pattern A1, 1 bar, seed fixed, 120 BPM, 48 kHz). These become the regression anchors every later refactor is measured against.

### v1 — the shipping instrument
- **M4** ✅ — Full 12-voice kit: sub kick (1), rim (4), ride (7), toms LO/HI (8/9), cowbell/perc (10), FM "zap"/FX on the sample track (11). New `Resonator` generator (modal body/ring). Per-voice golden one-shots for all 12 + extended GM note map + all-12 pad bank. **Purely additive: the M3 goldens for the original 5 voices + the default pattern stayed byte-identical** (the new voices are silent unless triggered, and the original tracks' tail config was untouched). The SAMPLE track hosts a synth zap until user-sample loading lands (Later). `cargo test --workspace` green (139 + 55); AU passes `auval`.
- **M5** — Full step grid. Split into two passes:
  - **M5 part 1 ✅ (engine):** the per-step performance engine — every `Step` carries velocity, accent, micro-timing, ratchets (+ramp), probability, conditional trig (`Always/Fill/First/Ratio`), and up to `MAX_PLOCKS` parameter locks. Deterministic, RNG-driven (`XorShift32`), reproducible via a seeded **GROOVE LOCK** (reseeded per loop). Swing/micro/ratchets shift triggers off the grid via a cross-block **carry queue**; `pending` is offset-sorted. **P-locks** target the per-voice tail (level/pan/cutoff/resonance/drive) via the `LockableParam` registry (+ reconcile test); the kit's dirty-flag fast path keeps unlocked playback **byte-identical** (all 12 voice + default-pattern goldens unchanged). 14 new tests (determinism, GROOVE LOCK, probability, ratchets, swing, conditions, polymeter, p-locks).
  - **M5 part 2 ✅:** the **step-editing GUI** (cell visuals — velocity brightness, probability pip, p-lock + ratchet stripes; a step inspector for velocity/probability/ratchet/micro/accent/condition + **per-step p-lock editing**), a **16-slot pattern bank** with song chaining (queue-to-loop), a per-lane **Euclid** generator + clear, a momentary **FILL** button, and editable swing/humanize. Typed lock-free `SeqEdit` ring; the GUI mirrors the bank and edits optimistically. (Still open for a later pass: live record/quantize, groove-template library, per-track speed.) Plus the part-1 review's deferred refinements: negative-micro/flam via one-step look-back scheduling, sub-block chunking to remove the carry/pending caps, and a filter "snap" path so per-step Cutoff locks step rather than glide (the tail's smoother currently slews them over ~5 ms).
- **M6** — Full mod system: 2 LFOs + mod-env + the 16-slot matrix with **voice-targeted destinations** (`ModDest` widened with `target_voice: u8`), K1–K8 macros. Mirror Esker's reconcile test for the widened enum.
- **M7** — Full bus FX: transient shaper, drive/bitcrush, glue + parallel/NY comp, **sidechain PUMP** (big transport knob), tape/stereo delay, reverb send (+ snare gated-verb path), true-peak limiter.
- **M8** — Mixing: per-voice channel strips, sends, **multi-out** routing (aux output ports in the audio-IO layout + AU/CLAP) and optional host sidechain aux input.
- **M9** — KITS + GROOVE WORLDS as lenses (Scene/MacroDef reuse): flagship worlds (Discothèque, Marseille, Bladerunner/Glacier, Outrun/Strobe, Neutral) + a 50+ factory library; A/B compare; disk presets; MIDI-learn; live visualizers; undo/redo (one-memcpy `Pattern` snapshots).
- **M10** — Validation + polish: full golden suite, factory finiteness/silence smoke tests, `auval`, RT-safety audit (`assert_process_allocs` clean), denormal/NaN audit, signing/notarization.

### Later — depth
Per design §9 "Later": full Elektron conditionals, slides/glides, song-mode arranger, finer grids, user-sample drag-in, MPE/microtuning, **MIDI-out so Drumlin can sequence Esker** + a shared session bus (the real payoff of one `dsp_core`).

**Shared DSP package (deferred, by design):** the copy in §0 is the deliberate for-now choice (don't bloat Esker). Once both projects exist, factor `dsp_core` into **one shared package both Esker and Drumlin depend on**, so a DSP fix lands once. Keeping `dsp_core` byte-identical to Esker's modules now is what makes that a mechanical lift later rather than a reconciliation.

---

## 5. Build & test commands

```sh
cd /Users/joes/drumlin

# Fast inner loop — the dependency-free cores test in milliseconds:
cargo test -p dsp_core
cargo test -p percussion_core
cargo test --workspace

# Build everything (plugin is a cdylib; standalone is a bin):
cargo build --workspace

# Bundle the CLAP (+ standalone) -> target/bundled/Drumlin.clap
cargo xtask bundle drumlin --release

# Build the AU (.component) for Logic and validate (macOS, needs cmake + CLT):
./scripts/build-au.sh
# (defaults baked into the script: PACKAGE=drumlin, BUNDLE=Drumlin,
#  AU_SUBTYPE_CODE=Drml, AU_BUNDLE_ID=com.joeshipley.drumlin)

# Run the standalone for quick listening:
cargo run --release --bin drumlin -- --midi-input ""        # lists MIDI inputs
cargo run --release --bin drumlin -- --midi-input "Your Controller"
```

### Golden-render strategy (per design §7.4)
- **Per-voice golden one-shots** — each factory voice, default patch, one hit, first ~16k stereo samples as checked-in `(l_bits, r_bits)`, asserted exact.
- **Default-pattern golden render** — Neutral kit, pattern A1, 1 bar, fixed seed, 120 BPM / 48 kHz. Top-level anchor exercising sequencer + choke + voices + bus.
- **Golden-trigger fixture** — `(pattern, transport) → Vec<(sample_offset, track, vel)>` pinning tick/swing/ratchet math.
- **Determinism**, **choke-group**, **polymeter-convergence**, **`LOCKABLE_PARAMS` reconcile**, and **finiteness/silence smoke** tests.

---

## 6. Execution order for this first session

1. **M0 scaffold** — workspace builds; `Drumlin.clap` bundles; plugin loads (silent) with the PRISM shell.
2. **M1 port `dsp_core`** — all copied inline tests green (proof the copy is faithful and bit-compatible with Esker).

These two are the foundation; M2+ (voices, sequencer, golden anchor) build on a workspace that is already real-time-safe, bundle-able, and regression-tooled. Subsequent sessions pick up at M2.
