# Drumlin

*The rhythm-section sibling to Esker.*

---

## 1. Pitch

**Drumlin** is a characterful analog **drum-machine** plugin, and the percussion sibling to Esker. Where Esker is the synth that *sings*, Drumlin is the box that *moves the floor*: every drum voice is **synthesized, not sampled** — a small, opinionated rack of modeled analog percussion engines (an 808/909-lineage kick, snare, hats, clap, toms, rim, cowbell, plus an FM/zap utility voice and a low-fi "dust" texture layer) that you tune, drive, sidechain and re-pitch like an instrument rather than trigger like a media player. Underneath those voices is a **step sequencer with Elektron-grade per-step parameter locks**, reproducible randomness, swing/groove, probability, ratchets and polymeter — the surface that turns a static loop into a living, evolving phrase. Drumlin carries Esker's exact engineering posture (a dependency-free, unit-tested Rust DSP core; hard real-time safety; CLAP + AUv2 only — no GPL VST3; MIT-licensed) and Esker's exact UX language (the dark, neon-tinged PRISM webview, one-touch sound-world scenes, eight relabeling macros, MIDI-learn, undo/redo, live visualizers). The result is a drum machine that sounds like the rhythm section of a Bangalter record, the gated snare of a synthwave anthem, or the dusty pulse of a Vangelis cue — punchy, analog-flavored, cinematic-yet-danceable — and that a producer can *ride live* on an MPK mini 3, not just program with a mouse.

The mental model is one sentence: **Esker is the melody; Drumlin is the groove; they share a soul, a look, and a codebase discipline.**

---

## 2. Vision & Sonic Identity

### A sibling, not a spin-off

Drumlin is deliberately a **member of a family**, named — like Esker (a winding glacial meltwater ridge) — after a glacial/geological landform. A *drumlin* is a glacial hill, and it literally contains the word **drum**, which makes it the obvious flagship name for the percussion instrument. The family relationship is not cosmetic; it is a set of commitments inherited verbatim from Esker:

- **Same sonic lineage.** Vangelis (cinematic Blade Runner warmth), French 79 / Simon Henner (modern French synthwave), Ratatat (bright, detuned, melodic) and Thomas Bangalter / Daft Punk (French house — punchy, filtered, compressed, groovy). Esker reads this lineage through *pitched* voices; Drumlin reads the **same four artists through their rhythm sections** — Bangalter's compressed filtered-disco kick-and-hat, French 79's TR-808 and gated reverb, Ratatat's bright detuned percussive blips, Vangelis's cavernous, slightly-detuned toms and gongs.
- **Same engineering rigor.** A tested, dependency-free DSP core; a **bit-exact golden-render regression guard**; no allocations on the audio thread (asserted in debug); lock-free SPSC rings for GUI↔audio; denormal/NaN flushing on every feedback path (the same class of bug that caused Esker's reverb crackle). Reproducible build scripts; an ad-hoc-signed AU that passes `auval`.
- **Same UI language.** The PRISM webview, restyled but instantly recognizable: dark canvas, one neon accent per scene, arc knobs with value rings, glanceable visualizers, an on-screen trigger surface (pads instead of a keyboard), MIDI-learn, undo/redo, disk preset browser + curated factory library.
- **Same business posture.** MIT, CLAP + AUv2 only (so the tree stays permissive and it loads in Logic), self-contained, no DRM — "more capable than the drum machines I've paid real money for." Positioned and owned as Esker's companion.

### Sonic north star — the family drum sound

Drumlin's voices are modeled on the canonical analog drum-machine lineage, then pushed through the family's French-house / synthwave aesthetic. The target is not "an 808 clone" — it is a *characterful, tunable, drivable* take on each archetype, voiced for this family's records.

| Voice | Analog lineage | Family voicing target |
|---|---|---|
| **Kick** | TR-808 (long sine + pitch-env), TR-909 (punchy, clicky transient) | Tunable sine/triangle body with a pitch-envelope "boom," a separate click/transient layer, and a saturation/drive stage — the Bangalter French-house kick: punchy, filtered, glued, sidechain-able against itself. |
| **Snare** | LinnDrum (snap), 909 (noise + tone), Simmons SDS (synthetic) | A tuned tone-pair + noise burst with its own decay and a built-in **gated-reverb** path — the synthwave anthem snare. Dial it dusty (LinnDrum) or wide and gated (outrun). |
| **Hi-hats** | 808/909 metallic hats (band-passed noise / 6-osc metal), CR-78 | Closed/open pair with a shared choke, metallic-vs-airy tilt and tight HP filtering — the disco/house tick that rides the off-beats. |
| **Clap** | 808 clap (multi-tap noise + reverb tail) | The classic stacked-burst clap with adjustable spread and a room tail — the outrun clap that smears wide in stereo. |
| **Toms** | LinnDrum toms, Simmons electronic toms | Pitched sine/triangle toms with pitch-env and drive — dusty LinnDrum fills at one extreme, big detuned Vangelis cinematic toms at the other. |
| **Rim / Cowbell / Perc** | 808 rimshot, 808 cowbell, CR-78 perc | Short metallic/woody utility voices for groove glue and synthwave flavor. |
| **Zap / FX voice** | Simmons sweeps, CR-78 oddities, noise | A modular "anything" percussive voice — pitch-swept zaps, noise sweeps, reverse hits — for cinematic accents and transitions. |
| **Dust layer** | (Drumlin original) | A low-fi character voice: tape hiss, vinyl crackle, room tone and a small bank of one-shot textures that sit *under* the kit for lo-fi and scoring glue. |

Three sonic fingerprints define "the Drumlin sound" across all voices:

1. **Punch and glue.** Every kit runs through the family dynamics chain — a sidechain pump, a glue compressor and a true-peak limiter — so the kit hits as a *unit*, compressed and danceable, the way a Daft Punk drum bus does.
2. **Analog wander.** Per-hit pitch/timing/level drift (Esker's "Vintage Drift," reborn as humanize) so no two hits are bit-identical — the opposite of a static sample loop. Crucially, the drift is **seeded and reproducible** (the family signature).
3. **Drive and space.** Shared saturation/bit-crush per voice plus a plate/hall/gated-reverb send, so a clean 909 kit becomes a dusty lo-fi kit or a vast cinematic one with a single scene change.

### Use-case target

Drumlin is aimed squarely at the records this family makes: **cinematic synthwave and outrun** (gated snares, 808 booms, night-drive momentum), **French house** (filtered, pumping, compressed four-on-the-floor), **lo-fi** (dusty LinnDrum toms, vinyl-flecked hats, swung and humanized) and **scoring** (toms, zaps and noise sweeps as cinematic percussion under a cue). It should feel equally at home laying down a Bangalter floor-filler, a French 79 verse pulse or a Vangelis tom motif — and it should be *played*: ridden live with pads and macro knobs, with the same "performance-first, always-visible" ethos that governs Esker.

### Guiding principles

1. **Synthesize, don't sample.** Every core voice is a modeled analog engine with real, automatable, modulatable parameters. Samples (the dust layer, optional one-shots) are *seasoning*, never the substance. This is the line that keeps Drumlin honest: if a feature could be replaced by "just load a better sample," it doesn't belong; if it makes the *engine* more expressive, tunable or characterful, it does.
2. **Characterful by default, surgical on demand.** Out of the box a kit drifts, drives, glues and breathes — it has a *vibe* before you touch anything. But every bit of that character is a parameter you can pull back to clinical precision.
3. **Performance-first, always-visible.** The things you ride live — pad triggers, scene macros, the sidechain pump, master, swing/groove, the reverb send, the FILL button — frame every screen as persistent chrome and map 1:1 to the MPK's pads and knobs. Drumlin is a ridable instrument, not a mouse-only sequencer.
4. **Same family, same rigor.** It looks like Esker, builds like Esker and is held to Esker's standards: dependency-free tested DSP core, bit-exact golden render, hard real-time safety, CLAP + AU, MIT. A user who owns both should feel one design mind behind both.
5. **Glue is the headline.** The kit is mixed and compressed as a unit — drive, sidechain pump, glue comp, true-peak limiter — because the family sound (Bangalter, French 79) is defined as much by *how the drums sit together* as by how each one sounds alone.

### Where it sits in the market

The drum-plugin world splits into sample ROMplers (realism, but you rent someone else's WAVs), pure drum *synths* (alive and tunable, but usually single-drum or a shallow fixed bank with weak sequencing) and groove boxes (synthesis plus a sequencer, but either thin voices or huge, sample-leaning, ecosystem-locked machines). The wedge Drumlin occupies: **a character-forward hybrid drum machine with synth-grade voice architecture *and* deep per-step parameter-lock sequencing, built to be ridden, not just programmed** — Sonic Charge Microtonic's p-lock depth with Esker's voice and finish, in the same permissive, self-contained, opinionated package. And it brings the *family voice* nobody else in the drum-synth tier owns.

---

## 3. Sound Engine & Per-Voice Synthesis

Where Esker is a polyphonic *pitched* instrument (one dual-layer voice cloned across notes), Drumlin is a **fixed-architecture rhythm engine**: a small number of dedicated, hand-tuned drum tracks, each a self-contained one-shot voice. The unifying decision is to **reuse Esker's DSP primitives verbatim where they fit** (`Oscillator`, `Noise`, the CS-80 dual `Filter`, `Drive`, the `flush_denormal` discipline, the `ModMatrix`) and add a thin set of percussion-specific generators on top. Same crate idiom, same real-time safety contract, same bit-exact golden-render guard.

### 3.1 Architecture decision: HYBRID, dual-layer per voice

**Recommendation: hybrid — analog-modeled synthesis as the primary layer, with an optional sample/transient layer blended in.** This is the literal percussion translation of Esker's signature dual-layer voice, and it's the right call for three reasons:

1. **The family sound is analog-modeled, not sample-library.** CR-78 / TR-808 / TR-909 / LinnDrum territory: synthesized kicks (pitched sine + pitch-envelope), synthesized snares (tone + noise), metallic-FM hats. Pure synthesis nails the lineage, is infinitely tweakable, costs zero memory, and is trivially `Clone`-able and unit-testable — exactly the Esker ethos of a dependency-free DSP core.
2. **Synthesis alone can't do everything.** A real clap's stereo splatter, a vinyl-crackle texture, a found-sound transient or a sampled 909 ride are hard to synthesize convincingly. So each voice gets an **optional sample layer**: a one-shot player blended by a per-voice `layer_mix` — mirroring Esker's Layer A / Layer B crossfade exactly.
3. **The transient trick.** Even on a fully-synth kick, a 2–6 ms sampled *click/transient* layered over the synth body is what gives modern kicks their "knock." So the sample layer doubles as a **transient injector**: a tiny built-in click bank that fires only during the attack window. One mechanism, two uses (full sample OR attack-only transient), governed by a `sample_role` switch.

**Per-voice top-level signal flow** (identical skeleton for every track; only the engine block differs):

```
            ┌─────────────────────────────────────────────┐
trigger ───▶│ SYNTH ENGINE (kick/snare/hat/... generator)  │──┐
(note,vel)  └─────────────────────────────────────────────┘  │
            ┌─────────────────────────────────────────────┐  ├─▶ layer_mix
       ───▶│ SAMPLE LAYER (one-shot / transient injector)  │──┘   (xfade)
            └─────────────────────────────────────────────┘        │
                                                                    ▼
   ┌──────────┐   ┌──────────────────┐   ┌──────────┐   ┌──────────────┐   ┌─────┐
   │ per-voice│──▶│ CS-80 dual filter │──▶│ post     │──▶│ amp VCA      │──▶│ pan │──▶ track bus
   │  DRIVE   │   │  (HP→LP, SVF/Moog)│   │  drive   │   │ (env × vel/  │   │     │
   └──────────┘   └──────────────────┘   └──────────┘   │  accent)     │   └─────┘
        ▲ shared per-voice MOD MATRIX (pitch-env, amp-env, 2 LFOs, velocity, accent, random) ▲
```

Every box except the engine generator is a **reused Esker primitive** instantiated per drum voice instead of per pitched voice. That reuse is what makes Drumlin a sibling and not a cousin.

### 3.2 Track count, allocation, choke groups, accent

- **12 fixed tracks.** Eight is too few for the family (house needs kick + clap + two hats + perc + ride + two toms minimum); 16 invites bloat. Twelve is the sweet spot and matches the 8-pad-×-bank feel of the MPK with headroom. Default layout: `KICK, KICK2/SUB, SNARE, CLAP, RIM, CLHAT, OPHAT, RIDE/CRASH, TOM_LO, TOM_HI, PERC/COWBELL, SAMPLE/USER`.
- **Fixed engine TYPE, fully editable.** Track 1 is "a kick" — its engine is the Kick generator — but every parameter is exposed. The SAMPLE/USER track defaults to the one-shot engine but can host any engine, so users aren't boxed in.
- **Mono-per-track with configurable retrigger.** A new trigger on the same track restarts that voice. Two modes: `Retrig` (hard restart, the 909 way — phase/envelopes reset; default for kick/snare/clap) and `Poly(1..4)` (let the old tail ring while the new hit starts; default `Poly(2)` for toms/ride).
- **Choke groups.** Four groups (`Off, A, B, C, D`). A trigger on a voice in a group sends a fast `choke()` (5–20 ms release override) to every *other* sounding voice in that group. Canonical use: CLHAT + OPHAT in group A so closed chokes open. The broadcast is hard-real-time and allocation-free — the engine holds `[Option<ChokeGroup>; 12]` and iterates the 12 voices on trigger.
- **Accent.** A global **accent** signal (per-step, from the sequencer) is a first-class mod source alongside velocity. It applies a configurable boost to amp gain, pitch-envelope depth, filter cutoff and drive — per voice via mod-matrix depths. This is the 808/909 accent rail and the heart of "groove."

### 3.3 Reused Esker primitives (the shared toolbox)

| Primitive | Role in Drumlin | Notes |
|---|---|---|
| `Oscillator` (sine/tri/saw/pulse/FM/wavetable + supersaw) | Kick body, snare tone, tom body, cowbell. FM mode drives metallic hats/ride. | Reused verbatim; FM ratio/index is the metallic-cluster knob. |
| `Noise` (white/pink) | Snare/clap/hat/cymbal noise source. | Pink for body, white for sizzle. |
| `Filter` (ZDF SVF + Moog ladder, HP+LP taps) | Per-voice CS-80 dual filter (resonant HP → LP) on every track. | HP tap bandpasses hat/snare noise; LP shapes kick/tom body. A real character edge sample machines lack. |
| `Drive` (tube/diode/RAT/compound + bitcrush) | Per-voice saturation/dirt, pre- *and* post-filter slots. | Kick distortion, snare crunch, lo-fi grit. |
| `Adsr` | VCA amp envelope; reused as a contour generator. | |
| `ModMatrix` (16-slot) | Per-voice modulation routing. | New sources `PitchEnv`, `Accent`, `Trigger`, `RandomPerHit`. |
| `flush_denormal` | Mandatory on every feedback write. | Non-negotiable RT-safety guard. |

### 3.4 New percussion-specific generators (the only genuinely new DSP)

Each is a small, inline-tested struct with a per-sample `next_sample()` API matching the Esker idiom, no audio-thread allocation, `flush_denormal` on any recursive state.

- **`PitchEnvelope` (DAHD, exponential).** The single most important primitive Esker lacks. A fast Decay-only or Attack-Hold-Decay contour with selectable exponential curve (true percussion needs exponential, not the linear `Adsr`), sub-millisecond resolution. Params: `start_amount` (semitones/Hz offset), `time` (ms), `curve` (lin↔exp). Drives kick/tom/snare-tone pitch sweep; doubles as a fast amp-decay shaper.
- **`Resonator` (modal / tuned bandpass bank).** 1–4 tuned, damped two-pole resonators (high-Q biquad bandpass, or a Karplus-ish damped delay for metal). The *body* of a snare, the *ring* of a rim/cowbell, the *shell* tone of a tom. Per partial: `freq`, `q/decay`, `gain`. This is what separates a real-sounding snare from "noise + a click."
- **`MetalCluster` (six-square-oscillator metallic source).** The classic 808 cymbal/hat/cowbell recipe: 6 detuned squares at fixed inharmonic ratios, summed and ring-mod-able, then bandpassed (a percussion analog of the supersaw's satellite table). Drives CLHAT/OPHAT/RIDE/CRASH and COWBELL (2 of the 6 = the famous 808 cowbell). FM mode on a single `Oscillator` is the brighter, more digital alternate path.
- **`OneShotSample` (sample layer / transient injector).** Linear-interp playback, start/end/loop points, `sample_role ∈ {Full, TransientOnly}`, gain, and a tiny built-in transient bank (click/tick/noise-burst one-shots baked into the binary like the wavetable bank). User samples load on the GUI thread into a double-buffered `Arc<[f32]>` swapped via the lock-free ring — **no file I/O or allocation on the audio thread.**
- **`ClapDiffuser` (multi-tap burst + stereo spread).** 3–4 rapid noise bursts (~8–12 ms apart) plus a longer diffuse tail and a stereo all-pass diffuser for the "room" splatter. Params: burst timing/count, `spread`.

### 3.5 Per-voice mod matrix

Same 16-slot table and `ModSlot { source, dest, depth }` shape as Esker. Sources extended for percussion: `Velocity`, `Accent` (the 808/909 rail, first-class), `PitchEnv`, `AmpEnv`, `Lfo1`, `Lfo2`, `Trigger` (a one-sample impulse at note-on, for click injection) and `RandomPerHit` (a fresh sample-and-hold each trigger — seeded per pattern for repeatability, reusing Esker's Random-Lock ethos). Dests: `Pitch`, `PitchEnvDepth`, `Cutoff`, `Resonance`, `Drive`, `NoiseLevel`, `ToneLevel`, `AmpDecay`, `Pan`, `SampleStart`, `LayerMix`. The two LFOs are per-voice and tempo-syncable. This is what lets one kick patch breathe: e.g. `RandomPerHit → Pitch (±15 cents)` and `Accent → Cutoff`. Mod destinations are **voice-addressable** — a slot routes "LFO1 → Hat Decay" or "Velocity → Kick Pitch" — handled by widening `ModDest` with a small `target_voice: u8` (`0xFF` = all voices).

### 3.6 Per-voice engines (synthesis method + role)

- **KICK** — `Oscillator(Sine|Triangle)` body + `PitchEnvelope` (fast downward sweep, the boom→thud) + `Trigger`/`OneShotSample(TransientOnly)` click + `Drive` + LP filter. Octave-down sine sub option for 808 sub.
- **SNARE** — two detuned `Oscillator(Triangle)` tones (~180 + 330 Hz, the 909 dual-tone) + `Resonator` body (2–3 partials) + bandpassed `Noise` with its own decay + optional **gated-reverb** send (gate handled by a fast amp-gate on the reverb return at the FX-rack stage). Tone/noise balance is the defining knob.
- **HATS / CYMBALS** — `MetalCluster` (or `Oscillator(Fm)` for brighter metal) into a steep HP, short (closed) or long (open) decay. CLHAT + OPHAT share a choke group. RIDE/CRASH = same engine, longer decay, lower HP, a bell-emphasis resonator partial.
- **CLAP** — `ClapDiffuser` bursts bandpassed (~900–1200 Hz) with stereo `spread`; optional short room from the reverb send.
- **TOMS (LO/HI)** — `Oscillator(Sine|Triangle)` + `PitchEnvelope` (gentler than kick) + `Resonator` shell + slight `Noise` attack. `Poly(2)` so toms ring through fills.
- **PERC / RIM / COWBELL** — RIM = very short `Resonator` tick + click transient. COWBELL = `MetalCluster` with 2 oscillators (the 808 recipe). General PERC can host any engine (wood, clave, shaker via filtered noise burst).
- **SAMPLE / USER** — `OneShotSample(Full)` wrapped in the full per-voice chain (filter/drive/amp/pan/mod), so even a raw sample gets the analog-flavored Drumlin treatment. Also where vinyl-texture / found-sound layers live.

### 3.7 KICK — concrete parameter list

| Param | Range / unit | Default | Role |
|---|---|---|---|
| `tune` | 30–120 Hz (or MIDI note) | 50 Hz (G1) | Base body pitch |
| `body_wave` | Sine / Triangle | Sine | Body waveform |
| `pitch_env_amount` | 0–48 st | +24 st | Pitch-sweep start offset |
| `pitch_env_time` | 1–500 ms | 35 ms | Sweep duration (DAHD decay) |
| `pitch_env_curve` | 0–1 (lin↔exp) | 0.85 | Sweep curve |
| `amp_decay` | 20–2000 ms | 350 ms | Body amplitude decay (exp) |
| `amp_hold` | 0–100 ms | 4 ms | Pre-decay hold (punch) |
| `click_level` | 0–1 | 0.4 | Transient layer gain |
| `click_type` | Tick / Noise / Sample idx | Tick | Which transient one-shot |
| `sub_level` | 0–1 | 0.0 | Octave-down sine sub |
| `drive_amount` | 0–1 | 0.25 | Saturation depth |
| `drive_kind` | Tube/Diode/RAT/Compound | Tube | Saturation flavor |
| `lp_cutoff` | 20 Hz–20 kHz | 8 kHz | Per-voice LP |
| `hp_cutoff` | 20–500 Hz | 25 Hz | DC/sub cleanup |
| `filter_model` | SVF / Moog | Moog | Filter character |
| `pan` | −1..+1 | 0 | Stereo position |
| `accent_amount` | 0–1 | 0.5 | Accent boost depth (gain+pitch+drive) |
| `layer_mix` | 0–1 | 0 (synth) | Synth ↔ sample crossfade |
| `choke_group` | Off/A/B/C/D | Off | Mute group |

### 3.8 SNARE — concrete parameter list

| Param | Range / unit | Default | Role |
|---|---|---|---|
| `tune` | 100–400 Hz | 180 Hz | Tone-osc 1 pitch |
| `tone2_ratio` | 1.0–2.5 | 1.83 | Tone-osc 2 ratio (dual-tone beat) |
| `tone_decay` | 20–800 ms | 120 ms | Tonal-body decay |
| `tone_level` | 0–1 | 0.5 | Tone-side balance |
| `body_partials` | 1–3 | 2 | `Resonator` partial count |
| `body_q` | 1–40 | 12 | Resonator Q / ring length |
| `noise_type` | White / Pink | White | Snare-wire color |
| `noise_decay` | 20–1000 ms | 200 ms | Noise (buzz) decay |
| `noise_level` | 0–1 | 0.6 | Noise amount |
| `noise_hp` | 200 Hz–6 kHz | 1.8 kHz | Noise high-pass |
| `noise_lp` | 2–18 kHz | 9 kHz | Noise low-pass |
| `snappy` | 0–1 | 0.5 | Macro: skews noise decay + HP for "snap" |
| `drive_amount` | 0–1 | 0.2 | Crunch |
| `gated_verb` | bool | false | Route to reverb with a fast gate (80s snare) |
| `gate_time` | 20–400 ms | 120 ms | Gated-reverb gate length |
| `pan` | −1..+1 | 0 | Stereo position |
| `accent_amount` | 0–1 | 0.5 | Accent boost depth |
| `layer_mix` | 0–1 | 0 (synth) | Synth ↔ sample crossfade |
| `choke_group` | Off/A/B/C/D | Off | Mute group |

---

## 4. Sequencer & Performance

The soul of the instrument. Drumlin's voice engine is the sibling to Esker's synth core; the sequencer is the thing that makes it *groove*. The intent: the immediacy and tactility of a TR-style step grid fused with the deep, per-step sound-design power of an Elektron sequencer — pure-data patterns, reproducible randomness, bit-exact regression guards, and a one-touch scene concept that mirrors Esker's sound-worlds. Sequencer state lives in a new dependency-free crate (sibling to the DSP core); it **runs sample-accurate on the audio thread** but is **edited on the GUI thread**, with edits crossing via a lock-free SPSC ring of `SeqEdit` messages and a playhead bitmap returned over a second ring — exactly Esker's editor↔processor pattern. The audio thread never allocates and never blocks.

### 4.1 How it stays a sibling to Esker

| Esker pattern | Drumlin sequencer echo |
|---|---|
| Dependency-free, inline-tested DSP core | Dependency-free, inline-tested sequencer core; pure logic, no DSP, no host types |
| Fixed-capacity, allocation-free state | Every pattern/track/step is a fixed-size `Copy` POD struct; no `Vec` on the audio thread |
| Deterministic `XorShift32` PRNG (Random / RandomLock) | The **same** PRNG drives per-step probability, humanization and a **GROOVE LOCK** (reproducible "drunk" feel) — the exact `XorShift32` lifted from the arp into the shared core |
| Golden-render bit-identity test | A **golden-trigger** fixture: a canonical pattern + transport renders a checked-in `(sample_offset, track, velocity)` list; timing-math changes must reproduce it bit-for-bit |
| Normalized 0..1 params; scene-as-pure-data | Global sequencer params (swing, tempo-mult, fill amount) are nih-plug params; the *pattern grid itself* is `#[persist]` JSON, not thousands of automatable params |
| Sound-worlds = a LENS (patch + macro remap) | **GROOVE WORLDS** = a LENS (kit + pattern + groove feel + macro remap) |

### 4.2 The data model

Three nested POD structs, all `Copy`, all fixed-capacity.

**A step** is *not* just on/off. Following Elektron, every step carries a full performance payload plus an optional bank of parameter locks:

```rust
/// One step on one track. Fixed-size, Copy, no heap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub on: bool,
    pub velocity: u8,            // 0..=127
    pub accent: bool,           // TR-style accent rail
    pub micro: i16,             // micro-timing nudge in 1/384-beat ticks, signed
    pub length: u8,             // gate as fraction of a step; 255 == tie to next
    pub ratchet: u8,            // 1 = normal, 2..=8 = roll
    pub ratchet_ramp: i8,       // -100..+100: flam (down) .. build (up)
    pub probability: u8,        // 0..=100; drawn from the pattern RNG each pass
    pub condition: TrigCondition,
    pub plocks: [PLock; MAX_PLOCKS],
    pub plock_count: u8,
}
```

**P-locks are the marquee feature** — a per-step override of *any* lockable engine or FX parameter:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PLock {
    pub param: u16,   // index into a stable, regression-guarded LOCKABLE_PARAMS table
    pub value: f32,   // normalized 0..1 — the SAME encoding Esker scenes use
}
pub const MAX_PLOCKS: usize = 4;   // per step; MVP. Raise later.
```

When the playhead hits a step that locks `filter_cutoff`, the engine substitutes that normalized value for that one hit and restores the live value after. Because `PLock.value` uses the exact normalized encoding as Esker's scene system, the engine's "apply a normalized value to param X" code is **shared** — no parallel path to drift. A `LOCKABLE_PARAMS` `u16` reconcile test (mirroring Esker's `ModDest` gate) keeps the registry in lockstep with the engine param id-strings so a p-lock never points at the wrong param.

**A track** is one drum voice; per-track length and rate give **polymeter** for free:

```rust
pub const MAX_STEPS: usize = 64;     // 4 pages of 16
pub const MAX_TRACKS: usize = 12;    // matches the fixed voice count

#[derive(Clone, Copy, Debug)]
pub struct Track {
    pub steps: [Step; MAX_STEPS],
    pub length: u8,            // 1..=64; unequal lengths across tracks == POLYMETER
    pub speed: TrackSpeed,     // x2 / /2 etc. relative to the pattern clock
    pub swing: i8,             // per-track swing override (-1 = use pattern swing)
    pub voice: u8,
    pub muted: bool,
    pub level: u8,
    pub accent_amt: u8,        // how much the accent flag adds
    pub default_velocity: u8,
}
```

Polymeter is a *data* property (just unequal `length`s) — no special engine mode; each track owns its own playhead modulo its own length. The cheapest way to get the off-kilter, hypnotic motion the Ratatat/French-79 side of the family loves.

**A pattern** binds the tracks plus global feel:

```rust
#[derive(Clone, Copy, Debug)]
pub struct Pattern {
    pub tracks: [Track; MAX_TRACKS],
    pub n_tracks: u8,
    pub swing: u8,                  // 50..=75 (50 straight, 66 ≈ triplet feel)
    pub groove: GrooveTemplate,     // microtiming + accent curve layered over swing
    pub groove_amount: u8,          // 0..=100 blend
    pub length: u8,
    pub resolution: u8,             // steps-per-bar (16 = 16ths)
    pub seed: u32,                  // per-pattern PRNG seed -> GROOVE LOCK
    pub fill_active: bool,
}
```

A **Song** is an ordered list of pattern ids with repeat counts — `[(pattern_id, repeats)]` — held in the persisted blob, off the audio thread's hot path. Pattern *chaining* (queue the next pattern, switch on the bar boundary) is a single `next_pattern: Option<u8>` the GUI sets and the audio thread consumes at the loop point, quantized.

### 4.3 Timing engine

Master resolution is **384 PPQN** — divisible by 16ths, triplets and 32nds, with room for `micro` nudges and swing as exact integers. The clock is host-transport-driven (Esker already reads tempo for the arp; Drumlin additionally reads the host's musical position so the grid locks to the DAW timeline and survives loop/locate). Each block: convert the block's start PPQ + tempo into a tick window; for each track, walk the ticks its playhead crosses; at each step boundary evaluate the step and, if it fires, compute the **exact sample offset within the block** and push a `Trigger { sample_offset, track, velocity, plocks, ratchet }` into a fixed ring the voice engine drains.

**Per-step evaluation order** (deterministic, RNG-driven, reproducible):

1. `condition` gate — fill/A:B/first-loop logic (uses a per-track pass counter, not RNG).
2. `probability` roll — `rng.next_below(100) < probability`, advanced once per evaluated step in a fixed order; a given seed → the exact same "random" performance every time. **GROOVE LOCK** freezes this seed.
3. Resolve **swing + groove template + micro nudge** → signed tick offset → sample offset.
4. Resolve **velocity** — base, + accent rail if set, × seeded humanize jitter, × track level.
5. Resolve **ratchets** — emit `ratchet` sub-triggers at even subdivisions, velocities shaped by `ratchet_ramp` (down = flam, up = build/roll).
6. Resolve **p-locks** — attach `[PLock; MAX_PLOCKS]`; the voice applies them for this hit only.

**Conditional triggers** (`TrigCondition`): `Fill`/`NotFill` (the performance roll), `Pre`/`NotPre` (depends on the previous conditional step), `First`/`NotFirst` (great for one-shot crashes), `A:B` ratios (`1:2, 2:2, 1:4, 3:4…` — "play every Nth loop," the backbone of long-form evolution from a 16-step grid) and `Neighbor` (fire only if the same step on the track to the left fired).

### 4.4 Performance layer

- **FILL / performance-roll button** — a momentary toggle that flips `fill_active` (engaging all `Fill`-conditional steps) and optionally force-ratchets the held track for a roll. The family's French-house build-ups live here.
- **Live record + quantize** — incoming MIDI/pad hits are timestamped against the PPQ clock and written to the nearest step (quantize on) or the nearest `micro`-tick (quantize off = capture the feel). Overdub by default; per-track record-arm latch.
- **Mute / solo + scene mutes** — instant, pattern-savable, snapshot-able as part of a GROOVE WORLD.
- **Pattern chain / step jump** — queue next pattern, switch on the bar; hold-to-audition vs click-to-commit, reusing Esker's sound-world audition gesture.
- **Humanize** — a global 0–100 adding *seeded* jitter to timing and velocity, so "humanized" still passes the golden-trigger test when the seed is pinned. Reproducible humanization is the family signature.
- **Undo/redo** — each `Pattern` is `Copy`, so an undo step is one memcpy onto a snapshot stack.
- **Per-lane EUCLID generator** (fill *k* of *n*, rotate) seeds patterns fast — very on-brand for French-house and synthwave hi-hat patterns.

### 4.5 The 8 macros and GROOVE WORLDS

The **8 MACRO knobs** relabel per GROOVE WORLD exactly as in Esker, routed through matrix slots 13–16 — but some macros now drive *sequencer* params (swing, probability scaler, fill amount) in addition to voice/FX. Drum-flavored default labels: *Punch, Swing Feel, Filter Sweep, Drive/Glue, Decay, Space, Stereo Width, Lo-Fi* ("Punch" → transient-shaper amount + pump depth; "Filter Sweep" rides the bus filter across the bar).

A **GROOVE WORLD** is a LENS over a *whole groove*: **kit + pattern + groove feel + macro remap**, recalled in one touch and encoded as pure data (a normalized `(param_id, value)` table + an embedded `Pattern` + the 8 macro defs) exactly like an Esker `Scene`. Factory worlds:

- **Discothèque** (Bangalter/Daft Punk) — four-on-the-floor kick, filtered disco hats with heavy swing+groove, sidechain-pumped, compressed; macros = FILTER / PUMP / SWING / FILL.
- **Marseille** (French 79 / Simon Henner) — 808-flavored kit, half-time snare, tape-delay'd rim, lock-frozen perc fills; macros = TAPE / HUMANIZE / DENSITY / ROOM.
- **Bladerunner** (Vangelis) — slow, cavernous, reverbed toms and gated hits, sparse probability-driven evolution; macros = SPACE / DECAY / EVOLVE / DRIVE.
- **Outrun** (Ratatat-bright, 80s gated) — detuned synthy percussion, polymeter hats, snappy gated reverb; macros = BRIGHT / DETUNE / POLY / SNAP.

A **VARY** button mutates a world within taste (reseed probability/humanize, nudge fills) — the percussion analog of Esker's sound-world VARY.

---

## 5. Modulation, Macros, FX & Mixing

### 5.1 Modulation

Reuse Esker's `ModMatrix` (16 slots) almost wholesale. Sources: 2 tempo-syncable LFOs, a global mod-envelope, **velocity** (per-hit), **per-step random (S&H)**, **step-position / bar-phase** (so a filter opens across the bar — a French-house staple), mod-wheel/CC and the **8 macros (K1–K8)**. Destinations are the per-voice tail params (pitch, decay, cutoff/res, drive, pan, level, sends) plus bus-FX destinations — all **voice-addressable** via the `target_voice` widening (§3.5). The one structural extension over Esker is exactly this voice-scoping; everything else is reuse. **Per-step parameter locks** (§4.2) are a sequencer-native modulation source that complements the matrix: a locked step is a one-step override written into the param before the voice triggers.

### 5.2 Per-voice mixer

One channel strip per lane (MIX rail): fader, pan, mute/solo, Send A (reverb), Send B (delay), a 2-band trim EQ, choke-group selector, and an **output routing** picker — *Main bus* (default) or *Multi-out* (declared as auxiliary output ports so Logic/CLAP hosts can split kick/snare/hats to separate tracks). **As shipped (M8):** rather than one dedicated stereo pair per voice (12 ports — heavy for AU/auval), the OUT picker assigns each voice to *Main* or one of **4 shared aux stereo pairs** (M / 1–4). Raising the pool is a one-line change (`N_AUX` + the `aux_output_ports` literal).

### 5.3 The drum-BUS FX chain

The order below is a percussion-tuned reuse of Esker's FX modules. Every Esker FX module is already a stateless-about-voice stereo processor (`Drive::process`, `Delay::process(l,r)->(l,r)`, `Reverb::process_send`, `Phaser`, `Chorus`, and `Dynamics::process` with `set_pump/set_glue/set_limiter` + gain-reduction/pump-envelope meters) and already calls `flush_denormal` on its feedback paths — so the bus chain is **literally Esker's post-voice signal chain re-pointed at the summed drum bus.** That reuse is the single biggest payoff of sharing the core.

1. **Transient shaper** — the one genuinely new bus module (~80 lines; Esker has no transient designer). Attack/Sustain via fast/slow envelope difference. Lives in the shared core.
2. **Drive / bit-crush** — `Drive` verbatim. Lo-fi crunch is core to Daft Punk and lo-fi house.
3. **Glue compression** — `Dynamics::set_glue` verbatim (SSL-style mix glue).
4. **Parallel / NY compression** — a second `Dynamics` instance hard-squashed in parallel + a dry/wet knob; no new DSP.
5. **Sidechain PUMP** — `Dynamics::set_pump` keyed off the internal kick trigger or a host sidechain. The single most genre-defining control in the plugin — give it a big dedicated knob on the transport bar.
6. **Tape + stereo delay** — `Delay` (tempo-sync, link/dual-tap, tape mode).
7. **Reverb send** — `Reverb` FDN plate/hall on Send A. The snare's gated-verb path lives here, gated by a fast amp-gate on the return.
8. **True-peak limiter** — `Dynamics::set_limiter` on the master, with a true-peak clip LED.

---

## 6. UI/UX & MIDI

### 6.1 The screen (PRISM, adapted for a beatbox)

Same chrome as Esker — top brand/transport bar, left rail, a main band, a bottom strip — but the **center of gravity moves from a voice editor to a step-sequencer grid.** Reuse every PRISM token verbatim (`--accent #E0A33E` amber over `--bg #0E0F12`, `--panel #15171C`, `--raise #1B1E24`, IBM Plex Mono/Sans, the `.fx-tile`/`.rail-btn`/`.knob`/`.toggle`/`.ab-btn`/`.lvl`/`.clip-led` component CSS) so the two plugins are unmistakably siblings.

1. **Brand/transport bar.** "DRUMLIN" wordmark (amber). KIT name + A/B compare. Pattern selector (16 slots, chainable into a 16-step song row). Host-synced tempo (standalone falls back to a default like Esker). Swing knob, pattern length (1–64), the big **PUMP** knob, a `>` PLAY that runs the internal preview clock only in standalone. MIDI-learn dot, undo/redo, settings.
2. **Left rail.** `GRID` · `VOICE` · `MIX` · `MOD` · `BUS FX` · `KITS` — selecting one swaps the main band, exactly like Esker's `data-section` rail swap.
3. **Main band — the step grid (the hero).** 12 voice lanes × N steps (default 16, up to 64 with horizontal paging), collapsible to the 8 most-used.
4. **Bottom strip.** An on-screen **4×4 pad bank** (local audition — see §6.3), a master meter + true-peak clip LED, and the live-visualizer dock.

### 6.2 The step grid (hero surface)

Each lane row shows a color-coded name chip, mute/solo (`M`/`S`), a mini output meter, a velocity-accent ribbon and the step cells. A click toggles a hit; the cell is not binary — its fill brightness encodes velocity (amber intensity), a corner pip encodes probability, a stripe flags a p-lock present, and a held cell opens a popover with the full per-step locks (velocity, micro-timing, probability, ratchet, conditional trig, parameter locks). Outrun touches consistent with PRISM: a moving amber glow sweep on the active playhead column, a brighter hairline on downbeats (every 4), dimmer amber for ghost hits. Editing ergonomics: shift-drag paints a run, alt-drag sets a velocity ramp, right-drag on a lane clears it, and a per-lane EUCLID generator seeds fills fast.

### 6.3 Selected-voice editor + MIDI

Clicking a lane name (or rail → VOICE) opens the per-voice editor, laid out on Esker's `.pane`/`.card`/`.knob` grid. Each voice exposes its type-specific knobs (§3.6) plus a **uniform tail** so the UI and mod matrix stay regular: Level, Pan, Pitch, Decay, the CS-80 dual filter, Drive, Send A (reverb), Send B (delay), Choke group, Output routing.

**MIDI design:**

- **GM-ish note map** so existing MIDI drum clips and host drummers "just work": C1(36) Kick, D1(38) Snare, F#1(42) Closed Hat, A#1(46) Open Hat, etc., each per-voice editable, with an optional **chromatic mode** where notes pitch a single voice for tuned-tom runs. Velocity is a per-hit mod source.
- **The on-screen 4×4 pad bank auditions LOCALLY and is NOT host-recorded** — the explicit Esker lesson (the on-screen keyboard drains a lock-free ring and injects directly into the engine at a default velocity, never into the host's recorded MIDI stream). Drumlin's pads use the identical pattern: editor pushes a packed note into an `rtrb` SPSC ring, `process` drains it once per block and triggers the voice. Pads are for auditioning / finger-drumming-into-the-grid-while-stopped.
- **MIDI-learn** (CC→param) reused from Esker's lock-free CC-map bridge; K1–K8 are the obvious learn targets for the MPK's 8 knobs.
- **Pattern-change via MIDI** — an optional note range or program-change selects patterns, so Drumlin can be sequenced live.

### 6.4 Live visualizers

GUI-side, in the bottom dock: per-voice trigger LEDs, a master scope + spectrum (JS FFT of the scope ring), gain-reduction meters for glue/pump/limiter (fed by the `Dynamics` meter accessors), the **pump-envelope curve drawn live** (so you can *see* the Daft Punk ducking) and the per-voice filter-curve editor reused from Esker (draggable HP/LP handles).

---

## 7. Technical Architecture & Stack

This design plugs into the actual Esker codebase. Concrete anchors:

- Workspace `/Users/joes/customvst/Cargo.toml`: members `crates/synth_core`, `crates/customvst`, `xtask`; `exclude=["vendor"]`; release profile `lto="thin"`, `codegen-units=1`, `opt-level=3`; MSRV `1.87`.
- FX modules in `/Users/joes/customvst/crates/synth_core/src/dynamics.rs` are stateless-about-voice stereo processors reusable on a drum bus verbatim.
- `flush_denormal` in `/Users/joes/customvst/crates/synth_core/src/util.rs` — the canonical NaN/inf/denormal → 0.0 guard, with its documented `NaN.clamp()` pitfall test.
- The plugin shell `/Users/joes/customvst/crates/customvst/src/lib.rs` already implements the bridge Drumlin needs: lock-free `rtrb` SPSC rings (`kbd_tx/rx`, `cc_tx/rx`), `assert_process_allocs`, once-per-block param push, an `fx_reset_pending` AtomicBool panic-reset, and the on-screen keyboard that auditions locally and is **not** host-recorded (`lib.rs:5039`).
- PRISM palette/components in `/Users/joes/customvst/crates/customvst/src/gui/index.html`.
- The golden guard `default_voice_is_bit_identical_to_golden` (`voice.rs:3137`) comparing `f32::to_bits()` against a checked-in `GOLDEN_DEFAULT`.

### 7.1 Stack & licensing (inherit Esker's exactly)

Rust + nih-plug with `default-features=false` to drop nih-plug's `vst3` feature (the GPL `vst3-sys` linkage); features `["assert_process_allocs", "standalone"]`. **Ships CLAP + AUv2 only. MIT-licensed.** Same trade-off Esker accepted: loses VST3-only hosts (Ableton/Cubase/FL) but keeps Logic via the AU and keeps the license clean. Same vendored `nih_plug_webview` (pinned rev), `rtrb`, `serde`/`serde_json`, `directories`; `bundler.toml` `name="Drumlin"`.

### 7.2 Crate / workspace structure — the DSP-sharing decision

**Decision: extract a shared, dependency-free `dsp_core` crate, and keep two thin per-instrument cores on top of it.** Do *not* make Drumlin depend on `synth_core` directly (that couples a drum machine to a polyphonic voice allocator and arpeggiator it doesn't want), and do *not* copy-paste the FX (that forks the bit-exact behavior the family relies on).

```
customvst/                      (the "landform" monorepo)
└─ crates/
   ├─ dsp_core/         NEW — shared, zero-dependency primitives
   │   ├─ filter.rs  drive.rs  delay.rs  reverb.rs  dynamics.rs   (moved from synth_core)
   │   ├─ phaser.rs  chorus.rs  lfo.rs  envelope.rs               (moved)
   │   ├─ mod_matrix.rs        (moved + voice-target widening)
   │   ├─ oscillator.rs  wavetable.rs                            (moved)
   │   ├─ transient.rs  NEW — bus transient shaper
   │   └─ util.rs              (moved — flush_denormal)
   ├─ synth_core/       Esker's voice/synth/arp, now `use dsp_core::*` (thin)
   ├─ percussion_core/  NEW — drum voices, sequencer engine, kit model
   │   ├─ voice/{kick,snare,hat,tom,cymbal,fm_perc,noise}.rs
   │   ├─ pitch_env.rs  resonator.rs  metal_cluster.rs  one_shot.rs  clap_diffuser.rs
   │   ├─ kit.rs        12 voices + bus-chain assembly + choke groups
   │   ├─ sequencer.rs  steps, p-locks, probability, ratchet, euclid, swing, song
   │   ├─ bus.rs        the drum-bus FX chain (re-points dsp_core modules)
   │   └─ golden_default.rs + golden tests
   ├─ customvst/        Esker plugin shell (unchanged)
   └─ drumlin/          NEW — Drumlin nih-plug shell (sibling of customvst)
       ├─ lib.rs  scenes.rs (KITS)  presets.rs  gui/index.html
   xtask/               bundles both customvst + drumlin
```

`dsp_core` is the **family's shared sound** — extracting it once means a reverb/drive/glue fix lands in both instruments and stays bit-compatible. `synth_core` shrinks to synth-specific (voice allocator, arp, harmonizer); `percussion_core` is its drum peer. Both depend only on `dsp_core`, and every crate stays dependency-free except the plugin shells.

**Migration safety:** moving modules out of `synth_core` must preserve Esker's `GOLDEN_DEFAULT`. The extraction is a pure move (re-export from `synth_core` for back-compat), and Esker's existing `default_voice_is_bit_identical_to_golden` test is the proof it didn't change a single sample. Do the extraction as its own phase, gated on that test staying green.

### 7.3 Real-time-safety rules (identical discipline to Esker)

1. **No allocations on the audio thread** — `assert_process_allocs` on in debug; all buffers (delay lines, reverb FDN, sequencer scratch, per-voice state) sized at `initialize()`/`set_sample_rate()`, never in `process`.
2. **Lock-free GUI↔audio messaging via `rtrb` SPSC rings** — editor→audio (pad notes + pattern/p-lock edits as compact packed messages applied at block start), audio→editor (trigger LEDs, GR meters, scope ring, playhead bitmap), and the CC-apply ring for MIDI-learn. User samples cross as `Arc<[f32]>` swaps.
3. **`flush_denormal` on every recursive write** — inherited free in all moved FX; new drum voices guard their filter, any resonant body and the transient shaper's envelope followers.
4. **Panic-reset on KIT/pattern jumps** — reuse `fx_reset_pending`: on KIT apply or hard pattern jump, drain once at block start and `clear()` every FX feedback buffer + silence voices, so a hotter kit can't feed stale reverb/delay state (the exact crackle bug Esker root-caused).
5. **Sample-accurate sequencer timing** off the host transport's PPQ position; swing/micro offsets applied as sample deltas; phase-locked across loop boundaries.

### 7.4 Golden-render test strategy

- **Per-voice golden one-shots** — each factory voice at its default patch, one hit, first ~4000 samples captured as a checked-in `(l_bits, r_bits)` table asserted with exact `==` (catches reordering of pitch-env/VCA math — the drum analog of Esker's VCA-placement guard).
- **Default-pattern golden render** — the **Neutral kit playing pattern A1 for one bar** (fixed seed, tempo 120, 48 kHz) into a golden buffer. The top-level anchor: it exercises the sequencer, choke groups, all voices and the full bus chain together. Any refactor (including the `dsp_core` extraction) must keep it bit-identical.
- **Golden-trigger fixture** — a `(pattern, transport)` → `Vec<(sample_offset, track, vel)>` fixture pinning the tick/swing/ratchet math.
- **Determinism test** — same `seed` ⇒ identical probability/humanize sequence across runs.
- **Choke-group test** — an open-hat hit is bit-silenced at the sample the closed-hat fires.
- **Polymeter convergence test** — tracks of length 12 vs 16 re-align after their LCM.
- **`LOCKABLE_PARAMS` reconcile test** — the `u16` registry stays in lockstep with engine param id-strings.
- **Finiteness/silence smoke test** over every factory kit + pattern (no NaN, min-peak > threshold).

### 7.5 Reproducible AU build

Clone Esker's `/Users/joes/customvst/scripts/build-au.sh` to `build-drumlin-au.sh`, changing only identity vars: `cargo xtask bundle drumlin --release` → `Drumlin.clap`; clap-wrapper pinned to the same tag (`v0.12.1`) wraps it into `Drumlin.component`; AU codes `AU_MANUF_CODE=JShp`, `AU_SUBTYPE_CODE=Drml`, `AU_INSTRUMENT_TYPE=aumu`, `AU_BUNDLE_ID=com.joeshipley.drumlin`. Ad-hoc codesign `--deep`, install to `~/Library/Audio/Plug-Ins/Components/`, validate with `auval -v aumu Drml JShp`. The clap-wrapper and SDKs auto-download into a gitignored `build/`, so the build is reproducible from a clean checkout with `cmake + CLT + cargo`. The **multi-out** ports must be declared as auxiliary outputs in the audio-IO layout so the AU/CLAP exposes them to Logic.

---

## 8. Kits & Preset System (the sound-world analog)

A **KIT is Esker's sound-world, for drums** — a LENS that recalls the *whole machine* in one touch: all 12 voice patches, the bus FX chain, the macro relabeling and a default pattern. Reuse the `Scene`/`MacroDef` structure from `scenes.rs` almost verbatim (curated normalized params + 8 macro routings + per-kit accent). A **preset is a saved snapshot on a KIT lens** — identical mental model to Esker. Same press-hold-to-audition vs click-to-commit, same A/B compare, same disk preset browser (`directories` crate, `~/Library/Application Support/Drumlin/`), same in-memory factory list mirrored into the GUI.

Note the two scene concepts and keep them distinct: a **KIT** recalls the sound + bus + macros (the timbral lens), while a **GROOVE WORLD** (§4.5) additionally embeds a *pattern + groove feel*. A KIT is "how the drums sound"; a GROOVE WORLD is "the whole groove." Factory content shares the family names:

- **Glacier / Bladerunner** — Vangelis cinematic.
- **Marseille** — French 79 / Simon Henner synthwave.
- **Discothèque** — Bangalter French house.
- **Strobe / Outrun** — Ratatat-bright / 80s gated.
- **Neutral** — the bit-identical default anchor (the golden-render reference).

Ship ~6 flagship KITs at v1 and grow a 50+ factory pattern/kit library.

---

## 9. Phased Roadmap

### MVP — the smallest fun, shippable drum machine (a beat you can play in Logic)

- **M0 — Scaffold.** The `drumlin` crate as a sibling of `customvst`; minimal PRISM GUI shell; CLAP + AU build green via the cloned script; `auval` passes.
- **M1 — Extract `dsp_core`.** Pure move + re-export out of `synth_core`. **Esker's golden test stays green** (proof of zero behavior change). Front-load this; everything depends on it.
- **M2 — Core voices + a sequencer.** Kick + Snare + Closed/Open Hat (synthesis) with choke groups; a 16-step single-pattern sequencer locked to host transport; GM note map; the on-screen pad bank (local audition, lock-free ring).
- **M3 — Per-voice tail + minimal bus + the anchor.** Level/Pan/Pitch/Decay/dual-filter/sends per voice; a minimal bus chain (glue + limiter) reused from `dsp_core`. Land the **Neutral default-pattern golden render** as the regression anchor.

That MVP is genuinely scoped: four classic voices, one pattern, host-synced playback, local-audition pads, glue + limiter, and a golden anchor — a self-contained box that already grooves in Logic. The flagship p-lock depth and the full kit come next, on a foundation that's already real-time-safe and regression-guarded.

### v1 — the shipping instrument

- **M4** — Full 12-voice kit (toms, perc, cymbal/ride, FM-perc, noise/FX).
- **M5** — Full step grid: per-step velocity, micro-timing, probability, ratchet/roll, conditional trigs, **per-step p-locks**, per-lane length (polymeter), Euclid generator, swing + groove templates, 16 patterns + song chaining, FILL button, live record.
- **M6** — Full mod system: 2 LFOs + mod-env + the 16-slot matrix with **voice-targeted destinations**, K1–K8 macros.
- **M7** — Full bus FX: transient shaper, drive/bitcrush, glue + parallel/NY comp, **sidechain PUMP**, tape/stereo delay, reverb send, true-peak limiter.
- **M8** — Mixing: per-voice channel strips, sends, **multi-out** routing/ports.
- **M9** — KITS + GROOVE WORLDS as lenses (Scene/MacroDef reuse): the flagship worlds + a 50+ factory library; A/B; disk presets; MIDI-learn; live visualizers; undo/redo.
- **M10** — Validation + polish: per-voice + pattern golden tests, factory smoke tests, `auval`, RT-safety audit (`assert_process_allocs` clean), denormal/NaN audit, signing/notarization (the same $99 Apple Developer route Esker uses).

### Later — depth

- Full Elektron conditional set (`Pre/NotPre`, `Neighbor`), per-step parameter slides/glides between consecutive locks.
- Song-mode arranger UI beyond simple chaining; per-pattern time signature; finer grids (24/32 PPB); per-track groove templates.
- Euclidean fill regenerator + pattern morph/interpolate; LFO-per-step; parameter-locking the *kit assignment* (sound-swap per step); > 4 p-locks/step.
- User sample drag-in layered under the synth voices (kept optional so the bit-reproducible default stays intact); per-step automation lanes.
- MPE / poly-aftertouch and microtuning for tuned toms/perc.
- **MIDI-out so Drumlin can sequence Esker**, and a shared Esker+Drumlin "session" bus so a track's pump/glue can key off Drumlin's kick into Esker — the real payoff of one shared `dsp_core`, and the two siblings in a rig.

---

## 10. Naming — recommendation up front

**Recommendation: `Drumlin`.** A glacial hill (a streamlined ridge of glacial till) — perfectly on-theme as Esker's sibling, and it literally contains the word **drum**. Short, brandable, instantly legible as a drum instrument while still reading as a real landform. No drum-machine or drum-synth plugin uses the name (nearest hit is Spitfire LABS *"Drumline,"* a different word and a sample-based marching kit — not a meaningful collision).

| Name | Theme fit | Percussion resonance | Conflict |
|---|---|---|---|
| **Drumlin** ✅ | Glacial hill (ridge of till) | Contains "drum"; Irish *droimnín*, "little ridge" | None in audio synthesis; only the unrelated "Drumline" sample kit |
| **Kettle** | Kettle-hole (glacial depression) | *Kettledrum*; struck vessel — most percussion-literal | Timpani sample VSTs named "Kettle Drum"; "Kettle" is an everyday word — noisier mark |
| **Moraine** | Ridge of glacial debris | "Debris field" of impacts | None found in audio software |
| **Cairn** | Stacked-stone landform | Stacked/struck layers | None found in audio software |
| **Scree** | Slope of loose rock | Rattling/skittering = hats/perc texture | None found (secondary candidate) |
| **Till** | Unsorted glacial sediment | Short percussive monosyllable | Common English word — weak/hard to protect |
| **Tor** | Granite outcrop | Hard struck stone; ultra-short | Likely generic/trademark collisions; thin hook |

After Drumlin, **Moraine** and **Cairn** are the cleanest fallbacks (on-theme, brandable) but neither *contains* a percussion hook. **Kettle** is the most percussion-literal and tonally a beautiful match for Esker, but carries the most semantic noise. Drumlin wins on all three axes at once.

---

## 11. Open Questions for the First Session

1. **Track count — 12 vs 16?** This document commits to **12 fixed tracks** (the MPK feel, less bloat). Sequencer structs use `MAX_TRACKS = 12`. Confirm before the data model is frozen, since polymeter/golden fixtures bake it in.
2. **`dsp_core` extraction scope.** Exactly which modules move vs stay in `synth_core`? Confirm the re-export-for-back-compat plan and that this lands as its own phase (M1) gated on Esker's golden test. Does Esker's `oscillator`/`wavetable` move wholesale, or do we split a smaller "shared osc" subset?
3. **P-lock budget.** `MAX_PLOCKS = 4` per step for the MVP — enough for the wedge, or do early factory patterns want more? It directly sizes the `Step` struct (and therefore undo-snapshot memcpy cost).
4. **KIT vs GROOVE WORLD as separate concepts** — ship both at v1, or collapse into one "world" that always embeds a pattern? (Leaning: keep distinct, but it affects scene encoding and the browser UI.)
5. **Multi-out at MVP or v1?** Declaring aux output ports touches the audio-IO layout and the AU build. Document defers it to M8 — confirm Logic users don't need it sooner.
6. **Default voice for the Neutral golden anchor.** The bit-exact regression target must be chosen and frozen *before* M2, since every later refactor is measured against it. Which exact kick/snare/hat patches define "Neutral"?
7. **Sample layer in MVP?** Document defers user-sample drag-in to "Later" to protect the dependency-free, bit-reproducible default — but the *built-in transient/click bank* (baked into the binary) is needed at M2 for the kick "knock." Confirm that split.
8. **Sidechain PUMP keying.** Internal kick-trigger by default; do we also expose a host sidechain input at v1, and how is it declared in the CLAP/AU IO layout?
9. **Standalone preview clock.** Confirm the standalone fallback tempo/behavior and that the internal `>` PLAY never fights the host transport when hosted.
10. **AU codes.** Confirm `AU_SUBTYPE_CODE=Drml` / `AU_MANUF_CODE=JShp` / `com.joeshipley.drumlin` are free of collisions with Esker's registered codes before the build script is cloned.
