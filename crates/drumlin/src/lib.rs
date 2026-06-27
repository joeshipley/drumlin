//! Drumlin — a characterful analog drum-machine plugin; the rhythm-section
//! sibling to Esker.
//!
//! M2: a real-time-safe CLAP/AU instrument that grooves. It drives a
//! `percussion_core` step sequencer off the host transport (or an internal
//! preview clock in standalone), triggers the Kick/Snare/Clap/Hat voices,
//! accepts host MIDI (GM note map) and local on-screen pad audition, and shows
//! a live, editable step grid in the PRISM webview. The per-voice tail, bus FX,
//! mod matrix and KITS arrive at M3+. See `docs/drumlin-plan.md`.

mod kits;
mod presets;
mod worlds;

use nih_plug::prelude::*;
use nih_plug_webview::{HTMLSource, WebViewEditor};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicI32, AtomicU16, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use percussion_core::{
    track_for_note, DrumKit, DrumModDest, DrumModMatrix, DrumModSlot, DrumModSource, LockableParam,
    ModEngine, ModLfoShape, Pattern, SeqState, Sequencer, TrigCondition, VoiceMix, VoicePatch,
    MAX_STEPS, MAX_TRACKS, N_AUX, N_TAIL_PARAMS,
};

const EDITOR_WIDTH: u32 = 1100;
const EDITOR_HEIGHT: u32 = 800;

/// On-screen pad-bank note ring capacity (editor producer -> audio consumer).
const KBD_QUEUE_CAP: usize = 256;
/// Grid step-edit ring capacity (editor producer -> audio consumer).
const EDIT_QUEUE_CAP: usize = 512;

/// Default velocity for on-screen pad hits.
const PAD_VELOCITY: f32 = 0.9;

/// Max undo/redo depth (whole-pattern snapshots).
const UNDO_CAP: usize = 64;

/// Editor -> audio sequencer edits, sent over a lock-free ring and applied at
/// block start. POD/`Copy` so the ring never allocates.
#[derive(Clone, Copy)]
enum SeqEdit {
    SetStep { track: u8, step: u8, on: bool },
    StepParams { track: u8, step: u8, on: bool, vel: u8, accent: bool, prob: u8, rat: u8, micro: i16, cond: u8, ra: u8, rb: u8 },
    SetPlock { track: u8, step: u8, param: u16, value: f32 },
    ClearPlock { track: u8, step: u8, param: u16 },
    ClearLane { track: u8 },
    Euclid { track: u8, pulses: u8, rotate: u8 },
    Fill { on: bool },
    SelectPattern { idx: u8 },
    Swing { value: u8 },
    Humanize { value: u8 },
    /// Per-voice patch default (VOICE editor): `param` indexes `LockableParam`,
    /// `value` is normalized 0..1.
    SetVoiceParam { track: u8, param: u16, value: f32 },
    /// Per-voice MIX (channel-strip) value: `field` 0=Send A, 1=Send B, 2=mute,
    /// 3=solo, 4=gated_verb (bools as 0.0/1.0).
    SetVoiceMix { track: u8, field: u8, value: f32 },
    /// Mod-matrix slot: `src`/`dst` are the source/dest indices, `depth` -1..1,
    /// `voice` is 0xFF (all) or a track index.
    SetModSlot { slot: u8, src: u16, dst: u16, depth: f32, voice: u8 },
    /// LFO config (`idx` 0=LFO1, 1=LFO2): shape discriminant, rate Hz, depth, retrig.
    SetLfo { idx: u8, shape: u8, rate: f32, depth: f32, retrig: bool },
    /// Mod-env attack + decay (seconds).
    SetModEnv { attack: f32, decay: f32 },
    /// One macro knob (`idx` 0..7, `value` 0..1).
    SetMacro { idx: u8, value: f32 },
}

/// JS -> Rust messages from the webview.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Action {
    Init,
    Note { on: bool, note: u8 },
    Step { track: u8, step: u8, on: bool },
    StepParams { track: u8, step: u8, on: bool, vel: u8, accent: bool, prob: u8, rat: u8, micro: i16, cond: u8, ra: u8, rb: u8 },
    SetPlock { track: u8, step: u8, param: u16, value: f32 },
    ClearPlock { track: u8, step: u8, param: u16 },
    ClearLane { track: u8 },
    Euclid { track: u8, pulses: u8, rotate: u8 },
    Fill { on: bool },
    SelectPattern { idx: u8 },
    Swing { value: u8 },
    Humanize { value: u8 },
    SetVoiceParam { track: u8, param: u16, value: f32 },
    SetVoiceMix { track: u8, field: u8, value: f32 },
    SetModSlot { slot: u8, src: u16, dst: u16, depth: f32, voice: u8 },
    SetLfo { idx: u8, shape: u8, rate: f32, depth: f32, retrig: bool },
    SetModEnv { attack: f32, decay: f32 },
    SetMacro { idx: u8, value: f32 },
    RecallKit { id: String },
    PresetSave { name: String },
    PresetLoad { name: String },
    PresetDelete { name: String },
    AbStore { slot: u8 },
    AbRecall { slot: u8 },
    AbCopy,
    /// Arm MIDI-learn for a macro (the next CC binds to it).
    MidiLearn { macro_idx: u8 },
    /// Clear any CC bound to a macro (and disarm if it was learning).
    MidiClear { macro_idx: u8 },
    Undo,
    Redo,
    Transport { play: bool },
    SeqEnable { on: bool },
    SidechainEnable { on: bool },
    // Automatable param gestures (id: 0=gain, 1=pump, 2=bus_drive).
    ParamBegin { id: u8 },
    ParamSet { id: u8, value: f32 },
    ParamEnd { id: u8 },
}

#[derive(Params)]
struct DrumlinParams {
    #[id = "master_gain"]
    gain: FloatParam,
    /// Sidechain PUMP depth — the headline French-house duck.
    #[id = "pump"]
    pump: FloatParam,
    /// Lo-fi bus drive.
    #[id = "bus_drive"]
    bus_drive: FloatParam,
    /// Plate reverb send (the "space").
    #[id = "reverb"]
    reverb: FloatParam,
    /// Tape/stereo delay mix.
    #[id = "delay"]
    delay: FloatParam,
    /// Pump rate (note division); factory center reproduces the 1/4-note duck.
    #[id = "pump_rate"]
    pump_rate: FloatParam,
    /// Pump duck curve/shape.
    #[id = "pump_curve"]
    pump_curve: FloatParam,
    /// Parallel/NY compression blend.
    #[id = "parallel"]
    parallel: FloatParam,
    /// Transient PUNCH (attack emphasis) on the bus.
    #[id = "punch"]
    punch: FloatParam,
    /// Gated-verb gate length (hold), ms.
    #[id = "gate_time"]
    gate_time: FloatParam,
    /// Use the host sidechain (aux input) as the PUMP key instead of the internal
    /// kick. Default off -> internal kick, so the bus is unchanged.
    #[id = "sidechain"]
    sidechain_key: BoolParam,

    /// Out-of-band state the host can't reach through plain params: the full
    /// pattern bank (steps, p-locks, grooves) and the SEQ master-enable.
    /// `nih-plug` serializes this `Arc<Mutex<_>>` with the plugin state, so a
    /// saved project restores the user's programming. (The FX macros — pump,
    /// drive, reverb, delay, gain — are ordinary params the host persists itself.)
    #[persist = "drumlin_state"]
    state: Arc<Mutex<PersistState>>,
}

/// The host-persisted song state: the pattern bank, the SEQ enable, and the
/// per-voice patch (the VOICE editor's defaults).
/// One LFO's persisted config. `shape` is the `ModLfoShape` discriminant
/// (0=Sine, 1=Triangle, 2=Saw, 3=Square, 4=SampleHold).
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
struct LfoCfg {
    shape: u8,
    rate_hz: f32,
    depth: f32,
    retrig: bool,
}

/// The persisted M6 mod state: the 16-slot matrix + the 2 LFO configs + the
/// mod-env (attack/decay) + the 8 macro knob values. Default = an all-Off matrix
/// + the `ModEngine`'s default LFOs + macros 0 → fully inert (no modulation).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct ModState {
    #[serde(default)]
    matrix: DrumModMatrix,
    #[serde(default = "ModState::default_lfo1")]
    lfo1: LfoCfg,
    #[serde(default = "ModState::default_lfo2")]
    lfo2: LfoCfg,
    #[serde(default = "ModState::default_env_attack")]
    env_attack: f32,
    #[serde(default = "ModState::default_env_decay")]
    env_decay: f32,
    #[serde(default)]
    macros: [f32; 8],
}

impl ModState {
    fn default_lfo1() -> LfoCfg {
        LfoCfg { shape: 0, rate_hz: 2.0, depth: 1.0, retrig: false }
    }
    fn default_lfo2() -> LfoCfg {
        LfoCfg { shape: 1, rate_hz: 5.0, depth: 1.0, retrig: false }
    }
    fn default_env_attack() -> f32 {
        0.005
    }
    fn default_env_decay() -> f32 {
        0.5
    }
}

impl Default for ModState {
    fn default() -> Self {
        Self {
            matrix: DrumModMatrix::default(),
            lfo1: Self::default_lfo1(),
            lfo2: Self::default_lfo2(),
            env_attack: Self::default_env_attack(),
            env_decay: Self::default_env_decay(),
            macros: [0.0; 8],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PersistState {
    seq: SeqState,
    seq_enabled: bool,
    #[serde(default)]
    voices: VoicePatch,
    #[serde(default)]
    mix: VoiceMix,
    #[serde(default)]
    mod_state: ModState,
    /// The K1–K8 macro labels for the recalled world (display-only — relabels the
    /// MOD page knobs). Persisted so a reloaded project keeps the world's names.
    #[serde(default = "default_macro_labels")]
    macro_labels: [String; 8],
    /// MIDI-learn map: `(cc, macro)` pairs binding a CC to a K1–K8 macro. Empty
    /// by default; rebuilt into the lock-free lookup on load.
    #[serde(default)]
    midi_cc: Vec<(u8, u8)>,
}

fn default_macro_labels() -> [String; 8] {
    kits::DEFAULT_MACRO_LABELS.map(String::from)
}

impl Default for PersistState {
    fn default() -> Self {
        Self {
            seq: SeqState::default(),
            seq_enabled: true,
            voices: VoicePatch::default(),
            mix: VoiceMix::default(),
            mod_state: ModState::default(),
            macro_labels: default_macro_labels(),
            midi_cc: Vec::new(),
        }
    }
}

impl Default for DrumlinParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                "Master",
                util::db_to_gain(-3.0),
                FloatRange::Skewed {
                    min: util::db_to_gain(-60.0),
                    max: util::db_to_gain(6.0),
                    factor: FloatRange::gain_skew_factor(-60.0, 6.0),
                },
            )
            .with_unit(" dB")
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_value_to_string(formatters::v2s_f32_gain_to_db(2))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            pump: FloatParam::new("Pump", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            bus_drive: FloatParam::new("Bus Drive", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            reverb: FloatParam::new("Reverb", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            delay: FloatParam::new("Delay", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            // Factory center 0.5 selects the 1/4-note division — the original duck.
            pump_rate: FloatParam::new("Pump Rate", 0.5, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_value_to_string(Arc::new(|v| {
                    ["1/1", "1/2", "1/4", "1/8", "1/16"][((v * 5.0) as usize).min(4)].to_string()
                })),
            pump_curve: FloatParam::new("Pump Curve", 0.5, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            parallel: FloatParam::new("Parallel Comp", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            punch: FloatParam::new("Punch", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            gate_time: FloatParam::new("Gate Time", 120.0, FloatRange::Linear { min: 20.0, max: 400.0 })
                .with_unit(" ms")
                .with_value_to_string(formatters::v2s_f32_rounded(0)),
            sidechain_key: BoolParam::new("Sidechain Key", false),
            state: Arc::new(Mutex::new(PersistState::default())),
        }
    }
}

pub struct Drumlin {
    params: Arc<DrumlinParams>,
    sample_rate: f32,

    kit: DrumKit,
    seq: Sequencer,
    /// The global mod sources (2 LFOs + mod-env). Advanced once per block on the
    /// host thread; its values are pushed into the kit via `set_mod_globals`.
    mod_engine: ModEngine,
    /// Latched mod-wheel (CC1) value, `0..1`; pushed to the kit each block.
    mod_wheel: f32,
    /// Current macro-knob values (K1–K8); mirrored from the persisted state and
    /// pushed to the kit on edit + load.
    macros: [f32; 8],
    /// Rising-edge tracker for `run` (sequencer playing), to fire the mod-env /
    /// retrigger LFOs on transport start.
    was_running: bool,
    /// Internal preview transport position (quarter notes); used in standalone
    /// or when the host is stopped but the GUI PLAY is engaged.
    internal_pos_qn: f64,
    was_internal_playing: bool,

    /// On-screen pad ring (editor -> audio). Local audition only; never written
    /// to the host's recorded MIDI stream (the Esker lesson).
    kbd_tx: Option<Producer<u16>>,
    kbd_rx: Consumer<u16>,
    /// Sequencer edit ring (editor -> audio).
    edit_tx: Option<Producer<SeqEdit>>,
    edit_rx: Consumer<SeqEdit>,

    /// GUI PLAY toggle for the internal preview clock.
    internal_play: Arc<AtomicBool>,
    /// Current sequencer step (-1 when stopped), published for the moving column.
    playhead: Arc<AtomicI32>,
    /// Per-track "sounding" bitmap (bit t = track t active), for trigger LEDs.
    voice_active: Arc<AtomicU16>,
    /// Current pattern slot, published for the GUI (auto-changes on a queued
    /// song-chain switch).
    cur_pattern: Arc<AtomicU8>,
    /// Live pump duck gain (f32 bits, 1.0 = open), published for the GUI meter.
    pump_meter: Arc<AtomicU32>,
    /// Effective playing state (host transport OR internal clock), published so
    /// the GUI PLAY indicator reflects host-driven playback too.
    effective_playing: Arc<AtomicBool>,
    /// Master enable for the internal step sequencer. On = groovebox (the grid
    /// runs, locked to the host transport / standalone PLAY). Off = Drumlin is
    /// purely MIDI/pad-driven and the grid stays silent, so a host MIDI region
    /// drives it cleanly without the internal pattern doubling it.
    seq_enabled: Arc<AtomicBool>,

    /// KIT recall signal (M9): the editor stages the new state into `params.state`
    /// then sets this; the audio thread re-adopts (and cuts tails) at block start.
    /// Cleared only after a SUCCESSFUL try_lock+adopt, so a recall is never dropped
    /// on lock contention (it retries next block). Must default false so an
    /// auval/headless render never spuriously recalls.
    recall_pending: Arc<AtomicBool>,

    /// MIDI-learn (M9): lock-free CC->macro lookup (index = CC 0..127; value =
    /// macro 0..7, or -1 = unbound). The audio thread reads it on every MidiCC;
    /// the editor writes it on learn/clear/load. `learn_arm` holds the macro
    /// currently being learned (-1 = idle); the audio thread binds the next CC.
    cc_to_macro: Arc<[AtomicI8; 128]>,
    learn_arm: Arc<AtomicI8>,

    /// Set when a sequencer edit lands; the next block snapshots the bank into
    /// the persisted state via a non-blocking `try_lock`, so the audio thread
    /// never stalls on the host's save.
    seq_dirty: bool,
    /// Last SEQ-enable value mirrored into the persisted state, so a toggle (no
    /// step edit) still re-snapshots.
    persisted_seq_enabled: bool,

    /// Preallocated per-sample aux stem accumulator (one stereo pair per aux
    /// output bus), filled by `kit.render_into` — keeps `process` alloc-free.
    aux_scratch: [(f32, f32); N_AUX],
}

impl Default for Drumlin {
    fn default() -> Self {
        let (kbd_tx, kbd_rx) = RingBuffer::<u16>::new(KBD_QUEUE_CAP);
        let (edit_tx, edit_rx) = RingBuffer::<SeqEdit>::new(EDIT_QUEUE_CAP);
        Self {
            params: Arc::new(DrumlinParams::default()),
            sample_rate: 48_000.0,
            kit: DrumKit::neutral(48_000.0),
            seq: Sequencer::new(),
            mod_engine: ModEngine::new(48_000.0),
            mod_wheel: 0.0,
            macros: [0.0; 8],
            was_running: false,
            internal_pos_qn: 0.0,
            was_internal_playing: false,
            kbd_tx: Some(kbd_tx),
            kbd_rx,
            edit_tx: Some(edit_tx),
            edit_rx,
            internal_play: Arc::new(AtomicBool::new(false)),
            playhead: Arc::new(AtomicI32::new(-1)),
            voice_active: Arc::new(AtomicU16::new(0)),
            cur_pattern: Arc::new(AtomicU8::new(0)),
            pump_meter: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            effective_playing: Arc::new(AtomicBool::new(false)),
            seq_enabled: Arc::new(AtomicBool::new(true)),
            recall_pending: Arc::new(AtomicBool::new(false)),
            cc_to_macro: Arc::new(core::array::from_fn(|_| AtomicI8::new(-1))),
            learn_arm: Arc::new(AtomicI8::new(-1)),
            seq_dirty: false,
            persisted_seq_enabled: true,
            aux_scratch: [(0.0, 0.0); N_AUX],
        }
    }
}

/// JSON of the initial Neutral demo grid (slot 0). The GUI mirrors the bank
/// locally from here + applies its own edits, so it stays in sync without the
/// audio thread pushing 16 patterns back.
/// Sparse cells for one pattern: only steps that sound or carry a p-lock, each
/// tagged with its `(t, s)`. The GUI blanks its bank then applies these, so the
/// payload scales with content, not with 12×64 empty cells per pattern.
fn pattern_cells(p: &Pattern) -> Vec<serde_json::Value> {
    let len = (p.length as usize).min(MAX_STEPS);
    let mut out = Vec::new();
    for t in 0..MAX_TRACKS {
        for s in 0..len {
            let st = &p.tracks[t].steps[s];
            if !st.on && st.plock_count == 0 {
                continue;
            }
            let (cond, ra, rb) = match st.condition {
                TrigCondition::Ratio { a, b } => (5u8, a, b),
                other => (other.code(), 1u8, 2u8),
            };
            let plocks: Vec<serde_json::Value> = st
                .plocks()
                .iter()
                .map(|pl| json!({ "param": pl.param, "value": pl.value }))
                .collect();
            out.push(json!({
                "t": t, "s": s,
                "on": st.on, "vel": st.velocity, "accent": st.accent,
                "prob": st.probability, "rat": st.ratchet, "ramp": st.ratchet_ramp,
                "micro": st.micro, "cond": cond, "ra": ra, "rb": rb, "plocks": plocks,
            }));
        }
    }
    out
}

/// The full bank + per-voice patch, for seeding the GUI on open (and after a
/// host project load, since the editor refetches via `Init`). Reads the
/// persisted/live state, so the grid and VOICE editor the user sees always match
/// what the engine will play. The patch is emitted **normalized** (0..1, the
/// slider encoding) even though it's stored in engine units.
// The AUDIO_IO_LAYOUTS aux_output_ports + aux_outputs literals must stay the same
// length as the kit's N_AUX (the per-voice OUT picker routes 1..=N_AUX).
const _: () = assert!(N_AUX == 4, "aux output port count must equal percussion_core::N_AUX");

/// Re-seat the live engine from a (staged) `PersistState`. The single source of
/// truth for both project load (`initialize`) and KIT recall (the audio-thread
/// drain). Takes disjoint `&mut` field refs rather than `&mut self` so the caller
/// can hold the `params.state` lock guard (which borrows `self.params`) at the
/// same time. `adopt_seq` gates the pattern/seq import: `initialize` passes
/// `!seq_dirty` (protect unsnapshotted edits); recall passes `true` (a recalled
/// GROOVE WORLD must load its pattern unconditionally). Alloc-free — every call
/// is a field write / fixed-count setter / one `copy_from_slice`.
#[allow(clippy::too_many_arguments)]
fn adopt_state(
    kit: &mut DrumKit,
    seq: &mut Sequencer,
    mod_engine: &mut ModEngine,
    macros: &mut [f32; 8],
    state: &PersistState,
    adopt_seq: bool,
) {
    if adopt_seq {
        seq.import(&state.seq);
    }
    kit.import_patch(&state.voices);
    kit.import_mix(&state.mix);
    let ms = &state.mod_state;
    for (i, sl) in ms.matrix.slots.iter().enumerate() {
        kit.set_mod_slot(i, sl.src, sl.dst, sl.depth, sl.target_voice);
    }
    mod_engine.set_lfo1(lfo_shape(ms.lfo1.shape), ms.lfo1.rate_hz, ms.lfo1.depth, ms.lfo1.retrig);
    mod_engine.set_lfo2(lfo_shape(ms.lfo2.shape), ms.lfo2.rate_hz, ms.lfo2.depth, ms.lfo2.retrig);
    mod_engine.set_mod_env(ms.env_attack, ms.env_decay);
    *macros = ms.macros;
    kit.set_macros(*macros);
}

/// The bus-FX host-param values a recalled kit drives (by `pget!` id 1..9) +
/// the sidechain toggle. `None` = "the kit doesn't set it" -> reset to default.
struct StagedBus {
    bus: [Option<f32>; 10],
    sidechain: Option<bool>,
}

/// Decode a `Kit` into the staging `PersistState` (the percussion_core half:
/// voices/mix/mod/pattern), returning the bus-FX + sidechain to drive via the
/// host-param gesture bridge. The kit is a COMPLETE lens, so the lens-controlled
/// state is reset to its defaults first, then the kit's curated rows applied —
/// recall is deterministic regardless of prior state, and Neutral (empty rows,
/// no pattern) reproduces the defaults exactly. Editor-thread only.
fn stage_kit(state: &mut PersistState, kit: &kits::Kit) -> StagedBus {
    use kits::KitRow;
    state.voices = VoicePatch::default();
    state.mix = VoiceMix::default();
    state.mod_state = ModState::default();
    state.macro_labels = kit.macro_labels.map(String::from);
    let mut bus: [Option<f32>; 10] = [None; 10];
    let mut sidechain = None;
    for row in kit.rows {
        match *row {
            KitRow::Voice { track, param, norm } => {
                if let Some(p) = LockableParam::from_index(param as u16) {
                    if (track as usize) < MAX_TRACKS && (param as usize) < N_TAIL_PARAMS {
                        state.voices.tracks[track as usize][param as usize] = p.denormalize(norm);
                    }
                }
            }
            KitRow::Mix { track, field, norm } => {
                if let Some(r) = state.mix.tracks.get_mut(track as usize) {
                    r.set(field, norm);
                }
            }
            KitRow::ModSlot { slot, src, dst, depth, voice } => {
                if let Some(sl) = state.mod_state.matrix.slots.get_mut(slot as usize) {
                    *sl = DrumModSlot {
                        src: DrumModSource::from_index(src as usize),
                        dst: DrumModDest::from_index(dst as usize),
                        depth: depth.clamp(-1.0, 1.0),
                        target_voice: voice,
                    };
                }
            }
            KitRow::Lfo { idx, shape, rate, depth, retrig } => {
                let cfg = LfoCfg { shape, rate_hz: rate, depth, retrig };
                if idx == 0 {
                    state.mod_state.lfo1 = cfg;
                } else {
                    state.mod_state.lfo2 = cfg;
                }
            }
            KitRow::ModEnv { attack, decay } => {
                state.mod_state.env_attack = attack;
                state.mod_state.env_decay = decay;
            }
            KitRow::Bus { id, norm } => {
                if (id as usize) < bus.len() {
                    bus[id as usize] = Some(norm.clamp(0.0, 1.0));
                }
            }
            KitRow::Sidechain(b) => sidechain = Some(b),
        }
    }
    // GROOVE WORLD: build the embedded groove (editor-thread) and load it into
    // the selected pattern slot.
    if let Some(build) = kit.pattern {
        let cur = state.seq.current as usize;
        if let Some(slot) = state.seq.patterns.get_mut(cur) {
            *slot = build();
        }
    }
    StagedBus { bus, sidechain }
}

/// Map a persisted LFO shape discriminant to the engine enum (unknown -> Sine).
fn lfo_shape(i: u8) -> ModLfoShape {
    match i {
        1 => ModLfoShape::Triangle,
        2 => ModLfoShape::Saw,
        3 => ModLfoShape::Square,
        4 => ModLfoShape::SampleHold,
        _ => ModLfoShape::Sine,
    }
}

fn bank_json(seq: &SeqState, voices: &VoicePatch, mix: &VoiceMix, mod_state: &ModState) -> serde_json::Value {
    let patterns: Vec<serde_json::Value> = seq
        .patterns
        .iter()
        .map(|p| {
            json!({
                "length": p.length,
                "swing": p.swing,
                "humanize": p.humanize,
                "cells": pattern_cells(p),
            })
        })
        .collect();
    // 12 tracks x 5 tail params, normalized for the VOICE sliders. (Pitch/Decay
    // are p-lock-only, not per-voice defaults, so they're not in voice_rows.)
    let voice_rows: Vec<Vec<f32>> = (0..MAX_TRACKS)
        .map(|t| {
            (0..N_TAIL_PARAMS)
                .map(|i| LockableParam::from_index(i as u16).unwrap().normalize(voices.tracks[t][i]))
                .collect()
        })
        .collect();
    // 12 tracks x [sendA, sendB, mute, solo, gatedVerb, chokeGroup, eqLow, eqHigh].
    // EQ is emitted normalized (0.5 = flat), matching the slider encoding.
    let mix_rows: Vec<Vec<f32>> = mix
        .tracks
        .iter()
        .map(|m| {
            vec![
                m.send_a,
                m.send_b,
                f32::from(m.mute),
                f32::from(m.solo),
                f32::from(m.gated_verb),
                f32::from(m.choke_group),
                m.eq_low_norm(),
                m.eq_high_norm(),
                f32::from(m.output),
                m.drift,
            ]
        })
        .collect();
    // The mod matrix: 16 slots as [src_index, dst_index, depth, target_voice].
    let mod_slots: Vec<Vec<f32>> = mod_state
        .matrix
        .slots
        .iter()
        .map(|s| vec![s.src.index() as f32, s.dst.index() as f32, s.depth, f32::from(s.target_voice)])
        .collect();
    let lfo_json = |l: &LfoCfg| json!({ "shape": l.shape, "rate": l.rate_hz, "depth": l.depth, "retrig": l.retrig });
    json!({
        "type": "grid",
        "tracks": MAX_TRACKS,
        "current": seq.current,
        "patterns": patterns,
        "voicepatch": voice_rows,
        "voicemix": mix_rows,
        "mod": {
            "slots": mod_slots,
            "lfo1": lfo_json(&mod_state.lfo1),
            "lfo2": lfo_json(&mod_state.lfo2),
            "env": { "attack": mod_state.env_attack, "decay": mod_state.env_decay },
            "macros": mod_state.macros,
        },
        // The factory KITS / GROOVE WORLDS for the KITS page (id + name + blurb +
        // whether it carries a pattern). Recall is wired in chunk 2.
        "kits": kits::FACTORY_KITS.iter().map(|k| json!({
            "id": k.id, "name": k.name, "blurb": k.blurb, "world": k.pattern.is_some(),
        })).collect::<Vec<_>>(),
    })
}

/// The 9 bus-FX params (pget! ids 1..=9) as normalized values, for the GUI
/// sliders + preset snapshots.
fn bus_values(params: &DrumlinParams) -> [f32; 9] {
    [
        params.pump.unmodulated_normalized_value(),
        params.bus_drive.unmodulated_normalized_value(),
        params.reverb.unmodulated_normalized_value(),
        params.delay.unmodulated_normalized_value(),
        params.pump_rate.unmodulated_normalized_value(),
        params.pump_curve.unmodulated_normalized_value(),
        params.parallel.unmodulated_normalized_value(),
        params.punch.unmodulated_normalized_value(),
        params.gate_time.unmodulated_normalized_value(),
    ]
}

/// The complete GUI seed payload: the bank + voice/mix/mod + the bus sliders,
/// sidechain, macro labels, and the user-preset list. Used by Init AND after a
/// preset load (so the whole GUI refreshes to the loaded state).
#[allow(clippy::too_many_arguments)]
fn full_json(
    seq: &SeqState,
    voices: &VoicePatch,
    mix: &VoiceMix,
    mod_state: &ModState,
    macro_labels: &[String; 8],
    bus: &[f32; 9],
    sidechain: bool,
    user_presets: &[String],
) -> serde_json::Value {
    let mut msg = bank_json(seq, voices, mix, mod_state);
    msg["busfx"] = json!(bus);
    msg["sidechain"] = json!(sidechain);
    msg["macro_labels"] = json!(macro_labels);
    msg["presets"] = json!(user_presets);
    msg
}

/// True for grid edits that change the current pattern's content (snapshotted for
/// undo). Selection / transport / sound / continuous-feel edits are excluded.
fn action_mutates_pattern(a: &Action) -> bool {
    matches!(
        a,
        Action::Step { .. }
            | Action::StepParams { .. }
            | Action::SetPlock { .. }
            | Action::ClearPlock { .. }
            | Action::ClearLane { .. }
            | Action::Euclid { .. }
    )
}

/// Derive the per-macro bound CC (`-1` = unbound) from the lock-free lookup, for
/// the GUI display (the last CC bound to each macro wins).
fn macro_cc_map(cc_to_macro: &[AtomicI8; 128]) -> [i32; 8] {
    let mut m = [-1i32; 8];
    for (cc, slot) in cc_to_macro.iter().enumerate() {
        let v = slot.load(Ordering::Relaxed);
        if (0..8).contains(&v) {
            m[v as usize] = cc as i32;
        }
    }
    m
}

/// A portable SOUND snapshot (the same fields a disk preset stores). The carrier
/// for preset load + A/B compare: capture the current sound, or apply one.
#[derive(Clone)]
struct SoundSnapshot {
    voices: VoicePatch,
    mix: VoiceMix,
    mod_state: ModState,
    macro_labels: [String; 8],
    bus: [f32; 9],
    sidechain: bool,
}

/// Snapshot the current sound (state + bus params + sidechain).
fn snapshot_current(s: &PersistState, params: &DrumlinParams) -> SoundSnapshot {
    SoundSnapshot {
        voices: s.voices.clone(),
        mix: s.mix.clone(),
        mod_state: s.mod_state.clone(),
        macro_labels: s.macro_labels.clone(),
        bus: bus_values(params),
        sidechain: params.sidechain_key.value(),
    }
}

/// Replace the SOUND fields of `state` from a snapshot (keeping the pattern bank)
/// and return the full GUI re-seed. The bus + sidechain are driven by the caller
/// via the param gesture bridge.
fn apply_sound(state: &mut PersistState, snap: &SoundSnapshot, user_presets: &[String]) -> serde_json::Value {
    state.voices = snap.voices.clone();
    state.mix = snap.mix.clone();
    state.mod_state = snap.mod_state.clone();
    state.macro_labels = snap.macro_labels.clone();
    full_json(
        &state.seq, &state.voices, &state.mix, &state.mod_state, &state.macro_labels,
        &snap.bus, snap.sidechain, user_presets,
    )
}

/// The two A/B compare registers + which is active (editor-local; not persisted).
#[derive(Default)]
struct AbState {
    a: Option<SoundSnapshot>,
    b: Option<SoundSnapshot>,
    active: u8, // 0 = A, 1 = B
}

impl Plugin for Drumlin {
    const NAME: &'static str = "Drumlin";
    const VENDOR: &'static str = "Joe Shipley";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        // One stereo aux input: the host sidechain key for the PUMP (auval-validated).
        aux_input_ports: &[new_nonzero_u32(2)],
        // Four stereo aux pairs for per-voice stems (each voice's MIX "output"
        // picker targets Main or one of these). auval-validated (5 output buses).
        aux_output_ports: &[
            new_nonzero_u32(2),
            new_nonzero_u32(2),
            new_nonzero_u32(2),
            new_nonzero_u32(2),
        ],
        names: PortNames {
            layout: None,
            main_input: None,
            main_output: Some("Output"),
            aux_inputs: &["Sidechain"],
            aux_outputs: &["Aux 1", "Aux 2", "Aux 3", "Aux 4"],
        },
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let params = self.params.clone();
        let kbd_tx = Mutex::new(self.kbd_tx.take());
        let edit_tx = Mutex::new(self.edit_tx.take());
        let internal_play = self.internal_play.clone();
        let playhead = self.playhead.clone();
        let voice_active = self.voice_active.clone();
        let cur_pattern = self.cur_pattern.clone();
        let pump_meter = self.pump_meter.clone();
        let effective_playing = self.effective_playing.clone();
        let seq_enabled = self.seq_enabled.clone();
        let recall_pending = self.recall_pending.clone();
        // A/B compare registers, editor-local (Arc<Mutex> like last_status, since
        // the event-loop closure is Send). Not persisted — a session tool.
        let ab = Arc::new(Mutex::new(AbState::default()));
        // Undo/redo of grid edits: editor-local stacks of whole-pattern snapshots
        // (Pattern is Copy). Restore reuses the recall path. Bounded to UNDO_CAP.
        let undo_stack: Arc<Mutex<Vec<Pattern>>> = Arc::new(Mutex::new(Vec::new()));
        let redo_stack: Arc<Mutex<Vec<Pattern>>> = Arc::new(Mutex::new(Vec::new()));
        let cc_to_macro = self.cc_to_macro.clone();
        let learn_arm = self.learn_arm.clone();
        // Last macro->CC map sent to the GUI (suppress unchanged sends).
        let last_macro_cc = Arc::new(Mutex::new([-1i32; 8]));
        // Set when the MIDI-learn map changes; the persist is retried each tick
        // until try_lock succeeds (so a bind isn't lost on lock contention).
        let midi_persist_pending = Arc::new(AtomicBool::new(false));
        // Pack (seq<<21 | playing<<20 | voices<<8 | playhead+1) to suppress
        // unchanged status sends.
        let last_status = Arc::new(AtomicU32::new(u32::MAX));

        let editor = WebViewEditor::new(
            HTMLSource::String(include_str!("gui/index.html")),
            (EDITOR_WIDTH, EDITOR_HEIGHT),
        )
        .with_background_color((0x0E, 0x0F, 0x12, 0xFF))
        .with_developer_mode(cfg!(debug_assertions))
        .with_keyboard_handler(|event| {
            event.state == nih_plug_webview::KeyState::Down
                && event.key == nih_plug_webview::Key::Escape
        })
        .with_event_loop(move |ctx, setter, _window| {
            // All three params are FloatParam, so one macro maps id -> &param.
            macro_rules! pget {
                ($id:expr) => {{
                    match $id {
                        1u8 => &params.pump,
                        2u8 => &params.bus_drive,
                        3u8 => &params.reverb,
                        4u8 => &params.delay,
                        5u8 => &params.pump_rate,
                        6u8 => &params.pump_curve,
                        7u8 => &params.parallel,
                        8u8 => &params.punch,
                        9u8 => &params.gate_time,
                        _ => &params.gain,
                    }
                }};
            }
            macro_rules! push_edit {
                ($e:expr) => {{
                    if let Ok(mut tx) = edit_tx.lock() {
                        if let Some(tx) = tx.as_mut() {
                            let _ = tx.push($e);
                        }
                    }
                }};
            }
            // Apply a SoundSnapshot: replace the sound (keep the bank), drive the
            // bus + sidechain host params, signal the audio thread, re-seed the GUI.
            // Shared by preset load + A/B recall (captures setter/ctx/params here).
            macro_rules! apply_snap {
                ($snap:expr) => {{
                    let snap = $snap;
                    let plist = presets::list();
                    let msg = params.state.lock().ok().map(|mut s| apply_sound(&mut s, &snap, &plist));
                    if let Some(msg) = msg {
                        for bid in 1u8..=9 {
                            let p = pget!(bid);
                            setter.begin_set_parameter(p);
                            setter.set_parameter_normalized(p, snap.bus[(bid - 1) as usize]);
                            setter.end_set_parameter(p);
                        }
                        setter.begin_set_parameter(&params.sidechain_key);
                        setter.set_parameter(&params.sidechain_key, snap.sidechain);
                        setter.end_set_parameter(&params.sidechain_key);
                        recall_pending.store(true, Ordering::Relaxed);
                        ctx.send_json(msg);
                    }
                }};
            }
            // Undo/redo: pop a pattern snapshot from $src, push the current onto
            // $dst, restore it into the selected slot, and re-seed via the recall
            // path. Reuses the GUI re-seed + recall drain (the playhead resets).
            macro_rules! restore_from {
                ($src:expr, $dst:expr) => {{
                    let restored = $src.lock().ok().and_then(|mut s| s.pop());
                    if let Some(p) = restored {
                        let msg = if let Ok(mut s) = params.state.lock() {
                            let cur = s.seq.current as usize;
                            if let Some(slot) = s.seq.patterns.get_mut(cur) {
                                let current = *slot;
                                if let Ok(mut d) = $dst.lock() {
                                    d.push(current);
                                    if d.len() > UNDO_CAP {
                                        d.remove(0);
                                    }
                                }
                                *slot = p;
                            }
                            let mut m = full_json(
                                &s.seq, &s.voices, &s.mix, &s.mod_state, &s.macro_labels,
                                &bus_values(&params), params.sidechain_key.value(), &presets::list(),
                            );
                            m["macro_cc"] = json!(macro_cc_map(&cc_to_macro));
                            Some(m)
                        } else {
                            None
                        };
                        if let Some(msg) = msg {
                            recall_pending.store(true, Ordering::Relaxed);
                            ctx.send_json(msg);
                        }
                    }
                }};
            }
            while let Ok(value) = ctx.next_event() {
                let Ok(action) = serde_json::from_value::<Action>(value) else {
                    continue;
                };
                // Before a grid edit, snapshot the current pattern for undo (and
                // drop the redo branch). At human edit speed the persisted pattern
                // already reflects the previous edit, so the snapshot is accurate.
                if action_mutates_pattern(&action) {
                    if let Ok(s) = params.state.lock() {
                        if let Some(p) = s.seq.patterns.get(s.seq.current as usize).copied() {
                            if let Ok(mut u) = undo_stack.lock() {
                                u.push(p);
                                if u.len() > UNDO_CAP {
                                    u.remove(0);
                                }
                            }
                            if let Ok(mut r) = redo_stack.lock() {
                                r.clear();
                            }
                        }
                    }
                }
                match action {
                    Action::Init => {
                        // Seed the GUI from the persisted/live state so it shows
                        // exactly what the engine holds (incl. a restored project):
                        // the pattern bank, per-voice patch + mix, the bus-FX
                        // slider positions, and the sidechain toggle. (The bus-FX
                        // params + the SC toggle are host-persisted but the sliders
                        // are otherwise write-only.)
                        if let Ok(s) = params.state.lock() {
                            let mut msg = full_json(
                                &s.seq, &s.voices, &s.mix, &s.mod_state, &s.macro_labels,
                                &bus_values(&params), params.sidechain_key.value(), &presets::list(),
                            );
                            msg["macro_cc"] = json!(macro_cc_map(&cc_to_macro));
                            ctx.send_json(msg);
                        }
                    }
                    Action::Note { on, note } => {
                        if let Ok(mut tx) = kbd_tx.lock() {
                            if let Some(tx) = tx.as_mut() {
                                let ev = (note as u16) | (if on { 0x100 } else { 0x000 });
                                let _ = tx.push(ev);
                            }
                        }
                    }
                    Action::Step { track, step, on } => push_edit!(SeqEdit::SetStep { track, step, on }),
                    Action::StepParams { track, step, on, vel, accent, prob, rat, micro, cond, ra, rb } => {
                        push_edit!(SeqEdit::StepParams { track, step, on, vel, accent, prob, rat, micro, cond, ra, rb })
                    }
                    Action::SetPlock { track, step, param, value } => {
                        push_edit!(SeqEdit::SetPlock { track, step, param, value })
                    }
                    Action::ClearPlock { track, step, param } => {
                        push_edit!(SeqEdit::ClearPlock { track, step, param })
                    }
                    Action::ClearLane { track } => push_edit!(SeqEdit::ClearLane { track }),
                    Action::Euclid { track, pulses, rotate } => {
                        push_edit!(SeqEdit::Euclid { track, pulses, rotate })
                    }
                    Action::Fill { on } => push_edit!(SeqEdit::Fill { on }),
                    Action::SelectPattern { idx } => push_edit!(SeqEdit::SelectPattern { idx }),
                    Action::Swing { value } => push_edit!(SeqEdit::Swing { value }),
                    Action::Humanize { value } => push_edit!(SeqEdit::Humanize { value }),
                    Action::SetVoiceParam { track, param, value } => {
                        // Apply to the live kit (sound) via the ring, AND persist
                        // immediately on the editor thread. The editor is the sole
                        // source of voice edits, so writing the patch here — rather
                        // than waiting for an audio-thread snapshot — means a tone
                        // tweak made with the transport stopped (when some hosts
                        // stop calling process()) can never be lost before a save.
                        push_edit!(SeqEdit::SetVoiceParam { track, param, value });
                        if let Some(p) = LockableParam::from_index(param) {
                            if let Ok(mut s) = params.state.lock() {
                                // voices is the per-voice TAIL patch (N_TAIL_PARAMS
                                // wide); Pitch/Decay are p-lock-only, not stored here.
                                if (track as usize) < MAX_TRACKS && (param as usize) < N_TAIL_PARAMS {
                                    s.voices.tracks[track as usize][param as usize] = p.denormalize(value);
                                }
                            }
                        }
                    }
                    Action::SetVoiceMix { track, field, value } => {
                        // Apply to the live kit AND persist on the editor thread
                        // (transport-independent), mirroring SetVoiceParam.
                        push_edit!(SeqEdit::SetVoiceMix { track, field, value });
                        if let Ok(mut s) = params.state.lock() {
                            if let Some(row) = s.mix.tracks.get_mut(track as usize) {
                                row.set(field, value); // same mapping as the live kit
                            }
                        }
                    }
                    Action::SetModSlot { slot, src, dst, depth, voice } => {
                        push_edit!(SeqEdit::SetModSlot { slot, src, dst, depth, voice });
                        if let Ok(mut s) = params.state.lock() {
                            if let Some(sl) = s.mod_state.matrix.slots.get_mut(slot as usize) {
                                *sl = DrumModSlot {
                                    src: DrumModSource::from_index(src as usize),
                                    dst: DrumModDest::from_index(dst as usize),
                                    depth: depth.clamp(-1.0, 1.0),
                                    target_voice: voice,
                                };
                            }
                        }
                    }
                    Action::SetLfo { idx, shape, rate, depth, retrig } => {
                        push_edit!(SeqEdit::SetLfo { idx, shape, rate, depth, retrig });
                        if let Ok(mut s) = params.state.lock() {
                            let cfg = LfoCfg { shape, rate_hz: rate, depth, retrig };
                            if idx == 0 {
                                s.mod_state.lfo1 = cfg;
                            } else {
                                s.mod_state.lfo2 = cfg;
                            }
                        }
                    }
                    Action::SetModEnv { attack, decay } => {
                        push_edit!(SeqEdit::SetModEnv { attack, decay });
                        if let Ok(mut s) = params.state.lock() {
                            s.mod_state.env_attack = attack;
                            s.mod_state.env_decay = decay;
                        }
                    }
                    Action::SetMacro { idx, value } => {
                        push_edit!(SeqEdit::SetMacro { idx, value });
                        if let Ok(mut s) = params.state.lock() {
                            if let Some(m) = s.mod_state.macros.get_mut(idx as usize) {
                                *m = value.clamp(0.0, 1.0);
                            }
                        }
                    }
                    Action::RecallKit { id } => {
                        if let Some(kit) = kits::FACTORY_KITS.iter().find(|k| k.id == id.as_str()) {
                            // Stage the percussion_core half under the lock, then
                            // release BEFORE driving params / signaling (so the
                            // audio thread's try_lock isn't contended and the flag
                            // is set only once staging is complete).
                            let staged = params.state.lock().ok().map(|mut s| stage_kit(&mut s, kit));
                            if let Some(staged) = staged {
                                // The bus FX chain is part of the lens: drive ids
                                // 1..9 to the kit's value or their default (master
                                // gain, id 0, is left alone). Host-recordable.
                                for bid in 1u8..=9 {
                                    let p = pget!(bid);
                                    let v = staged.bus[bid as usize].unwrap_or_else(|| p.default_normalized_value());
                                    setter.begin_set_parameter(p);
                                    setter.set_parameter_normalized(p, v);
                                    setter.end_set_parameter(p);
                                }
                                let sc = staged.sidechain.unwrap_or(false);
                                setter.begin_set_parameter(&params.sidechain_key);
                                setter.set_parameter(&params.sidechain_key, sc);
                                setter.end_set_parameter(&params.sidechain_key);
                                // Tell the audio thread to adopt the staged state +
                                // cut tails at block start.
                                recall_pending.store(true, Ordering::Relaxed);
                                // Relabel the MOD-page macro knobs for this world.
                                ctx.send_json(json!({ "type": "macro-labels", "labels": kit.macro_labels }));
                            }
                        }
                    }
                    Action::PresetSave { name } => {
                        // Snapshot the whole machine (state + bus + sidechain) to
                        // disk on the editor thread, then refresh the browser list.
                        let snap = params.state.lock().ok().map(|s| presets::DiskPreset {
                            format: "drumlin-preset-v1".to_string(),
                            name: name.clone(),
                            voices: s.voices.clone(),
                            mix: s.mix.clone(),
                            mod_state: s.mod_state.clone(),
                            macro_labels: s.macro_labels.clone(),
                            bus: bus_values(&params),
                            sidechain: params.sidechain_key.value(),
                        });
                        if let Some(snap) = snap {
                            let _ = presets::save(&snap);
                            ctx.send_json(json!({ "type": "presets", "presets": presets::list() }));
                        }
                    }
                    Action::PresetDelete { name } => {
                        let _ = presets::delete(&name);
                        ctx.send_json(json!({ "type": "presets", "presets": presets::list() }));
                    }
                    Action::PresetLoad { name } => {
                        // A preset replaces the SOUND only (keeps the pattern bank).
                        if let Some(p) = presets::load(&name) {
                            apply_snap!(SoundSnapshot {
                                voices: p.voices,
                                mix: p.mix,
                                mod_state: p.mod_state,
                                macro_labels: p.macro_labels,
                                bus: p.bus,
                                sidechain: p.sidechain,
                            });
                        }
                    }
                    Action::AbStore { slot } => {
                        // Capture the current sound into register A (0) or B (1).
                        if let Ok(s) = params.state.lock() {
                            let snap = snapshot_current(&s, &params);
                            if let Ok(mut ab) = ab.lock() {
                                if slot == 0 {
                                    ab.a = Some(snap);
                                } else {
                                    ab.b = Some(snap);
                                }
                                ab.active = slot;
                            }
                        }
                    }
                    Action::AbRecall { slot } => {
                        // Apply register A/B if it has been captured.
                        let snap = ab.lock().ok().and_then(|mut ab| {
                            ab.active = slot;
                            if slot == 0 { ab.a.clone() } else { ab.b.clone() }
                        });
                        if let Some(snap) = snap {
                            apply_snap!(snap);
                        }
                    }
                    Action::AbCopy => {
                        // Copy the active register into the other (A<->B).
                        if let Ok(mut ab) = ab.lock() {
                            if ab.active == 0 {
                                ab.b = ab.a.clone();
                            } else {
                                ab.a = ab.b.clone();
                            }
                        }
                    }
                    Action::MidiLearn { macro_idx } => {
                        if macro_idx < 8 {
                            learn_arm.store(macro_idx as i8, Ordering::Relaxed);
                        }
                    }
                    Action::MidiClear { macro_idx } => {
                        // Disarm BEFORE scanning: once the audio thread can't bind a
                        // new CC to this macro, the scan is guaranteed to clear any
                        // slot (closes the clear-vs-bind race).
                        if learn_arm.load(Ordering::Relaxed) == macro_idx as i8 {
                            learn_arm.store(-1, Ordering::Relaxed);
                        }
                        for slot in cc_to_macro.iter() {
                            if slot.load(Ordering::Relaxed) == macro_idx as i8 {
                                slot.store(-1, Ordering::Relaxed);
                            }
                        }
                    }
                    Action::Undo => restore_from!(undo_stack, redo_stack),
                    Action::Redo => restore_from!(redo_stack, undo_stack),
                    Action::Transport { play } => internal_play.store(play, Ordering::Relaxed),
                    Action::SeqEnable { on } => seq_enabled.store(on, Ordering::Relaxed),
                    Action::SidechainEnable { on } => {
                        setter.begin_set_parameter(&params.sidechain_key);
                        setter.set_parameter(&params.sidechain_key, on);
                        setter.end_set_parameter(&params.sidechain_key);
                    }
                    Action::ParamBegin { id } => setter.begin_set_parameter(pget!(id)),
                    Action::ParamSet { id, value } => {
                        setter.set_parameter_normalized(pget!(id), value.clamp(0.0, 1.0))
                    }
                    Action::ParamEnd { id } => setter.end_set_parameter(pget!(id)),
                }
            }

            // Publish playhead + voice LEDs + transport + current pattern, on change.
            let ph = playhead.load(Ordering::Relaxed);
            let va = voice_active.load(Ordering::Relaxed);
            let playing = effective_playing.load(Ordering::Relaxed);
            let seq_on = seq_enabled.load(Ordering::Relaxed);
            let pat = cur_pattern.load(Ordering::Relaxed);
            let pump_env = f32::from_bits(pump_meter.load(Ordering::Relaxed));
            // Quantize the duck depth to a nibble so the meter sends ~at the duck
            // rate (not every frame) yet stays still when the pump is open.
            let duck = ((1.0 - pump_env).clamp(0.0, 1.0) * 15.0) as u32;
            let packed = ((duck & 0xF) << 28)
                | (((pat as u32) & 0xF) << 24)
                | ((seq_on as u32) << 21)
                | ((playing as u32) << 20)
                | (((va as u32) & 0xFFF) << 8)
                | (((ph + 1) as u32) & 0xFF);
            if last_status.swap(packed, Ordering::Relaxed) != packed {
                ctx.send_json(json!({
                    "type": "status",
                    "playhead": ph,
                    "voices": va,
                    "playing": playing,
                    "seq": seq_on,
                    "pattern": pat,
                    "pump": pump_env,
                }));
            }

            // MIDI-learn: when the bound map changes (a learn completed on the
            // audio thread, or a clear), push it to the GUI + flag a persist.
            // Diffed each tick so the editor needn't be notified explicitly.
            let mcc = macro_cc_map(&cc_to_macro);
            let changed = match last_macro_cc.lock() {
                Ok(mut l) if *l != mcc => {
                    *l = mcc;
                    true
                }
                _ => false,
            };
            if changed {
                ctx.send_json(json!({ "type": "macro-cc", "macro_cc": mcc }));
                midi_persist_pending.store(true, Ordering::Relaxed);
            }
            // Persist the map, retrying across ticks until try_lock wins (so a
            // bind made the same tick the host serializes isn't lost on reload).
            if midi_persist_pending.load(Ordering::Relaxed) {
                if let Ok(mut s) = params.state.try_lock() {
                    s.midi_cc = cc_to_macro
                        .iter()
                        .enumerate()
                        .filter_map(|(cc, slot)| {
                            let m = slot.load(Ordering::Relaxed);
                            (m >= 0).then_some((cc as u8, m as u8))
                        })
                        .collect();
                    midi_persist_pending.store(false, Ordering::Relaxed);
                }
            }
        });

        Some(Box::new(editor))
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.sample_rate = buffer_config.sample_rate;
        self.kit.set_sample_rate(buffer_config.sample_rate);
        self.mod_engine.set_sample_rate(buffer_config.sample_rate);
        // Adopt any host-restored project state (the pattern bank, SEQ enable,
        // and the per-voice patch). The bank import is skipped if a step edit is
        // still un-snapshotted (`seq_dirty`), so a mid-session sample-rate
        // re-init can't clobber unsaved programming. The patch is always current
        // in `state.voices` (the editor writes it directly), so importing it is
        // always safe — and a Default patch reproduces the Neutral kit exactly
        // (engine-unit storage is lossless).
        if let Ok(state) = self.params.state.try_lock() {
            let adopt_seq = !self.seq_dirty;
            if adopt_seq {
                self.seq_enabled.store(state.seq_enabled, Ordering::Relaxed);
                self.persisted_seq_enabled = state.seq_enabled;
            }
            adopt_state(
                &mut self.kit,
                &mut self.seq,
                &mut self.mod_engine,
                &mut self.macros,
                &state,
                adopt_seq,
            );
            // Rebuild the lock-free MIDI-learn lookup from the persisted map.
            for slot in self.cc_to_macro.iter() {
                slot.store(-1, Ordering::Relaxed);
            }
            for &(cc, macro_idx) in &state.midi_cc {
                if (cc as usize) < 128 && macro_idx < 8 {
                    self.cc_to_macro[cc as usize].store(macro_idx as i8, Ordering::Relaxed);
                }
            }
        }
        true
    }

    fn reset(&mut self) {
        self.kit.reset();
        self.seq.reset_playhead();
        self.internal_pos_qn = 0.0;
        self.was_internal_playing = false;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // Once per block: adopt a pending KIT recall. The editor has staged the
        // new state into params.state; re-seat the live engine from it and cut
        // tails on the jump. Clear the flag ONLY on a successful try_lock+adopt,
        // so a recall can't be dropped on lock contention (it retries next block),
        // and the tail-cut is coupled to the actual adoption (never a block early).
        // seq_dirty is cleared so the post-edit snapshot doesn't re-export over
        // the just-recalled state.
        if self.recall_pending.load(Ordering::Relaxed) {
            if let Ok(state) = self.params.state.try_lock() {
                adopt_state(
                    &mut self.kit,
                    &mut self.seq,
                    &mut self.mod_engine,
                    &mut self.macros,
                    &state,
                    true,
                );
                self.seq_enabled.store(state.seq_enabled, Ordering::Relaxed);
                self.persisted_seq_enabled = state.seq_enabled;
                self.seq_dirty = false;
                self.kit.reset(); // cut tails at the jump
                self.recall_pending.store(false, Ordering::Relaxed);
            }
        }

        // Once per block: apply queued edits from the GUI.
        while let Ok(ed) = self.edit_rx.pop() {
            // Per-voice patch edits dirty the kit; everything else dirties the
            // pattern bank. Keeping these separate avoids re-exporting the whole
            // bank when only a VOICE slider moved.
            if let SeqEdit::SetVoiceParam { track, param, value } = ed {
                // Apply to the live kit for sound; persistence is handled on the
                // editor thread (see Action::SetVoiceParam), not from here.
                self.kit.set_voice_param(track as usize, param as u16, value);
                continue;
            }
            if let SeqEdit::SetVoiceMix { track, field, value } = ed {
                // Same split: live kit here, persistence on the editor thread.
                self.kit.set_voice_mix(track as usize, field, value);
                continue;
            }
            // Mod edits: apply to the live kit / mod-engine (sound); persistence
            // is on the editor thread (Action::*). None of these dirty the seq.
            match ed {
                SeqEdit::SetModSlot { slot, src, dst, depth, voice } => {
                    self.kit.set_mod_slot(
                        slot as usize,
                        DrumModSource::from_index(src as usize),
                        DrumModDest::from_index(dst as usize),
                        depth,
                        voice,
                    );
                    continue;
                }
                SeqEdit::SetLfo { idx, shape, rate, depth, retrig } => {
                    if idx == 0 {
                        self.mod_engine.set_lfo1(lfo_shape(shape), rate, depth, retrig);
                    } else {
                        self.mod_engine.set_lfo2(lfo_shape(shape), rate, depth, retrig);
                    }
                    continue;
                }
                SeqEdit::SetModEnv { attack, decay } => {
                    self.mod_engine.set_mod_env(attack, decay);
                    continue;
                }
                SeqEdit::SetMacro { idx, value } => {
                    if let Some(m) = self.macros.get_mut(idx as usize) {
                        *m = value;
                    }
                    self.kit.set_macros(self.macros);
                    continue;
                }
                _ => {}
            }
            self.seq_dirty = true;
            match ed {
                SeqEdit::SetStep { track, step, on } => {
                    self.seq.set_step(track as usize, step as usize, on)
                }
                SeqEdit::StepParams { track, step, on, vel, accent, prob, rat, micro, cond, ra, rb } => {
                    self.seq.set_step_params(
                        track as usize,
                        step as usize,
                        on,
                        vel,
                        accent,
                        prob,
                        rat,
                        micro,
                        TrigCondition::from_code(cond, ra, rb),
                    )
                }
                SeqEdit::SetPlock { track, step, param, value } => {
                    self.seq.set_plock(track as usize, step as usize, param, value)
                }
                SeqEdit::ClearPlock { track, step, param } => {
                    self.seq.clear_plock(track as usize, step as usize, param)
                }
                SeqEdit::ClearLane { track } => self.seq.clear_lane(track as usize),
                SeqEdit::Euclid { track, pulses, rotate } => {
                    self.seq.euclid(track as usize, pulses, rotate)
                }
                SeqEdit::Fill { on } => self.seq.set_fill(on),
                SeqEdit::SelectPattern { idx } => self.seq.select_pattern(idx as usize),
                SeqEdit::Swing { value } => self.seq.set_swing(value),
                SeqEdit::Humanize { value } => self.seq.set_humanize(value),
                // All handled before this match (they `continue`); listed for
                // exhaustiveness.
                SeqEdit::SetVoiceParam { .. }
                | SeqEdit::SetVoiceMix { .. }
                | SeqEdit::SetModSlot { .. }
                | SeqEdit::SetLfo { .. }
                | SeqEdit::SetModEnv { .. }
                | SeqEdit::SetMacro { .. } => {}
            }
        }

        // Snapshot the bank / kit patch into the host-persisted state whenever
        // they (or the SEQ enable) changed. `try_lock` is non-blocking and the
        // exports are allocation-free, so this stays real-time safe; if the host
        // is serializing for a save right now, we simply retry next block.
        let seq_on = self.seq_enabled.load(Ordering::Relaxed);
        if self.seq_dirty || seq_on != self.persisted_seq_enabled {
            if let Ok(mut state) = self.params.state.try_lock() {
                self.seq.export_into(&mut state.seq);
                state.seq_enabled = seq_on;
                self.seq_dirty = false;
                self.persisted_seq_enabled = seq_on;
            }
        }

        // Once per block, at sample 0: local pad audition (drums are one-shots,
        // so note-off is ignored).
        while let Ok(ev) = self.kbd_rx.pop() {
            let note = (ev & 0x7F) as u8;
            let on = ev & 0x100 != 0;
            if on {
                if let Some(t) = track_for_note(note) {
                    self.kit.trigger(t, PAD_VELOCITY, false, &[]);
                }
            }
        }

        // Resolve the transport: host wins; otherwise the GUI PLAY drives an
        // internal preview clock.
        let transport = context.transport();
        let tempo = transport.tempo.unwrap_or(120.0).max(1.0);
        let sr = self.sample_rate as f64;
        let block_len = buffer.samples();

        // Once per block: push the bus FX params (the headline PUMP, lo-fi drive)
        // and the tempo (for the beat-synced duck).
        self.kit.set_bus_tempo(tempo as f32);
        self.kit.set_pump(self.params.pump.value());
        self.kit.set_bus_drive(self.params.bus_drive.value());
        self.kit.set_bus_reverb(self.params.reverb.value());
        self.kit.set_bus_delay(self.params.delay.value());
        self.kit.set_pump_rate(self.params.pump_rate.value());
        self.kit.set_pump_curve(self.params.pump_curve.value());
        self.kit.set_bus_parallel(self.params.parallel.value());
        self.kit.set_bus_transient(self.params.punch.value());
        self.kit.set_gate_time(self.params.gate_time.value());
        let sidechain = self.params.sidechain_key.value();
        self.kit.set_pump_source_external(sidechain);
        let host_playing = transport.playing;
        let internal_playing = self.internal_play.load(Ordering::Relaxed);
        let seq_on = self.seq_enabled.load(Ordering::Relaxed);

        // Reset the preview playhead on a fresh standalone PLAY.
        if internal_playing && !self.was_internal_playing && !host_playing {
            self.internal_pos_qn = 0.0;
            self.seq.reset_playhead();
        }
        self.was_internal_playing = internal_playing;

        // The grid runs only when SEQ is enabled, and then follows the host
        // transport (or the standalone PLAY clock). MIDI/pad triggering below is
        // independent of this, so SEQ-off gives clean MIDI-region control.
        let run = seq_on && (host_playing || internal_playing);
        let pos_qn = if host_playing {
            transport.pos_beats().unwrap_or(self.internal_pos_qn)
        } else {
            self.internal_pos_qn
        };

        self.seq.set_playing(run);
        self.effective_playing.store(run, Ordering::Relaxed);

        // Mod sources (M6): on the transport play-start edge fire the mod-env +
        // reset retrigger LFOs; then advance the engine over this block and push
        // the latched LFO/env + mod-wheel values into the kit. The matrix reads
        // them per hit (an all-Off matrix ignores them, so this is inert until a
        // route is wired). LFO config / macros arrive with the MOD page.
        if run && !self.was_running {
            self.mod_engine.retrigger();
        }
        self.was_running = run;
        self.mod_engine.advance(block_len);
        self.kit.set_mod_globals(self.mod_engine.lfo1(), self.mod_engine.lfo2(), self.mod_engine.mod_env());
        self.kit.set_mod_wheel(self.mod_wheel);
        // Push the macro values every block so a learned-CC twist (written into
        // self.macros in the MidiCC handler) actually reaches the kit.
        self.kit.set_macros(self.macros);

        self.seq.process_block(pos_qn, tempo, sr, block_len);

        // Advance the internal preview clock to the block END (where the
        // sequencer playhead now is), so a later host-stop -> internal handoff
        // continues seamlessly instead of rewinding to this block's start.
        let block_advance = (tempo / 60.0) * (block_len as f64 / sr);
        if host_playing {
            self.internal_pos_qn = pos_qn + block_advance;
        } else if internal_playing {
            self.internal_pos_qn += block_advance;
        }

        // Per-sample render: interleave host MIDI, sequencer triggers, and the
        // kit, scheduling each at its exact sample offset.
        let mut ti = 0usize;
        let pending_len = self.seq.pending().len();
        let mut next_midi = context.next_event();

        for (i, channel_samples) in buffer.iter_samples().enumerate() {
            // Host MIDI notes at this sample (GM note map -> track).
            while let Some(event) = next_midi {
                if event.timing() as usize > i {
                    break;
                }
                match event {
                    NoteEvent::NoteOn { note, velocity, .. } => {
                        if let Some(t) = track_for_note(note) {
                            self.kit.trigger(t, velocity, false, &[]);
                        }
                    }
                    NoteEvent::MidiCC { cc, value, .. } => {
                        let cc = cc as usize;
                        if cc < 128 {
                            // MIDI-learn: if armed, bind this CC to the macro + disarm
                            // (the editor picks the change up + persists it).
                            let arm = self.learn_arm.load(Ordering::Relaxed);
                            if arm >= 0 {
                                self.cc_to_macro[cc].store(arm, Ordering::Relaxed);
                                self.learn_arm.store(-1, Ordering::Relaxed);
                            }
                            // Apply a bound CC to its macro (pushed via set_macros).
                            let m = self.cc_to_macro[cc].load(Ordering::Relaxed);
                            if (0..8).contains(&m) {
                                self.macros[m as usize] = value;
                            }
                        }
                        // CC1 also feeds the dedicated mod-wheel source.
                        if cc == 1 {
                            self.mod_wheel = value;
                        }
                    }
                    _ => {}
                }
                next_midi = context.next_event();
            }

            // Sequencer triggers scheduled at or before this sample (offsets are
            // emitted in ascending order; `> i` is robust if one ever lands early).
            while ti < pending_len {
                let trg = self.seq.pending()[ti];
                if trg.offset as usize > i {
                    break;
                }
                // Sequencer hits carry seeded per-hit drift + mod sources on the
                // Trigger (live pad/MIDI hits above use plain `trigger`).
                self.kit.trigger_seq(&trg);
                ti += 1;
            }

            // Feed the host sidechain key to the PUMP (only when enabled; an
            // unconnected input reads as silence -> no duck).
            if sidechain {
                let key = aux.inputs.first().map_or(0.0, |b| {
                    let ch = b.as_slice_immutable();
                    let kl = ch.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                    let kr = ch.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                    kl.abs().max(kr.abs())
                });
                self.kit.set_pump_key(key);
            }

            let (l, r) = self.kit.render_into(&mut self.aux_scratch);
            let g = self.params.gain.smoothed.next();
            for (ch, sample) in channel_samples.into_iter().enumerate() {
                *sample = if ch == 0 { l * g } else { r * g };
            }
            // Per-voice stems -> aux output buses (master gain applied; buses with
            // no voice routed stay wrapper-zeroed silence).
            let n = aux.outputs.len().min(N_AUX);
            for k in 0..n {
                let chans = aux.outputs[k].as_slice();
                let (al, ar) = self.aux_scratch[k];
                if !chans.is_empty() {
                    chans[0][i] = al * g;
                }
                if chans.len() > 1 {
                    chans[1][i] = ar * g;
                }
            }
        }

        // Every scheduled trigger must have fired within the block (offsets are
        // always < block_len); a leftover would mean a stranded hit.
        debug_assert_eq!(ti, pending_len, "stranded sequencer triggers");

        // Publish playhead + per-track activity for the GUI.
        self.playhead.store(
            self.seq.current_step().map(|s| s as i32).unwrap_or(-1),
            Ordering::Relaxed,
        );
        let mut mask = 0u16;
        for t in 0..MAX_TRACKS {
            if self.kit.track_active(t) {
                mask |= 1 << t;
            }
        }
        self.voice_active.store(mask, Ordering::Relaxed);
        self.cur_pattern.store(self.seq.current_pattern() as u8, Ordering::Relaxed);
        self.pump_meter.store(self.kit.pump_envelope().to_bits(), Ordering::Relaxed);

        ProcessStatus::KeepAlive
    }
}

impl ClapPlugin for Drumlin {
    const CLAP_ID: &'static str = "com.joeshipley.drumlin";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Drumlin — a characterful analog drum machine");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Drum,
        ClapFeature::Stereo,
    ];
}

// VST3 intentionally NOT exported (GPL vst3-sys). CLAP + AU only. See plan §7.1.
nih_export_clap!(Drumlin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_kit_resets_then_applies_rows() {
        use kits::{Kit, KitRow, DEFAULT_MACRO_LABELS};
        let kit = Kit {
            id: "t",
            name: "T",
            blurb: "",
            rows: &[
                KitRow::Voice { track: 0, param: 2, norm: 0.3 }, // kick Cutoff
                KitRow::Mix { track: 2, field: 0, norm: 0.6 },    // snare Send A
                KitRow::ModSlot { slot: 0, src: 3, dst: 3, depth: 0.5, voice: 255 }, // LFO1->Cutoff
                KitRow::Lfo { idx: 0, shape: 2, rate: 8.0, depth: 1.0, retrig: false },
                KitRow::ModEnv { attack: 0.01, decay: 0.4 },
                KitRow::Bus { id: 1, norm: 0.8 },                 // pump
                KitRow::Sidechain(true),
            ],
            macro_labels: DEFAULT_MACRO_LABELS,
            pattern: None,
        };
        let mut state = PersistState::default();
        state.voices.tracks[0][0] = 9.9; // dirty a param the kit DOESN'T set
        let staged = stage_kit(&mut state, &kit);

        // The kit is a complete lens: the un-listed dirtied param reset to default.
        assert_eq!(state.voices.tracks[0][0], VoicePatch::default().tracks[0][0], "reset before apply");
        // Listed rows applied.
        assert!((state.voices.tracks[0][2] - LockableParam::Cutoff.denormalize(0.3)).abs() < 1e-3);
        assert!((state.mix.tracks[2].send_a - 0.6).abs() < 1e-6);
        assert_eq!(state.mod_state.matrix.slots[0].src, DrumModSource::Lfo1);
        assert_eq!(state.mod_state.matrix.slots[0].dst, DrumModDest::Cutoff);
        assert_eq!(state.mod_state.lfo1.shape, 2);
        assert!((state.mod_state.lfo1.rate_hz - 8.0).abs() < 1e-6);
        assert!((state.mod_state.env_decay - 0.4).abs() < 1e-6);
        // Bus pump staged; an unlisted bus param is None (the driver resets it).
        assert_eq!(staged.bus[1], Some(0.8));
        assert_eq!(staged.bus[3], None);
        assert_eq!(staged.sidechain, Some(true));
    }

    #[test]
    fn all_factory_kits_stage_cleanly() {
        for kit in kits::FACTORY_KITS {
            let mut state = PersistState::default();
            let _ = stage_kit(&mut state, kit);
            // A GROOVE WORLD must have loaded a non-empty groove into the slot.
            if kit.pattern.is_some() {
                let cur = state.seq.current as usize;
                let any_on = state.seq.patterns[cur].tracks.iter().any(|t| t.steps.iter().any(|s| s.on));
                assert!(any_on, "GROOVE WORLD {} must load a non-empty groove", kit.id);
            }
        }
    }

    #[test]
    fn factory_world_rows_decode_to_active_targets() {
        // A static guard: a mis-numbered factory row (a src/dst that decodes to
        // Off, an out-of-range track/field/bus id) would silently do nothing.
        // Catch it before ship.
        use kits::KitRow;
        for kit in kits::FACTORY_KITS {
            for row in kit.rows {
                match *row {
                    KitRow::Voice { track, param, .. } => {
                        assert!((track as usize) < MAX_TRACKS, "{}: Voice track OOB", kit.id);
                        assert!((param as usize) < N_TAIL_PARAMS, "{}: Voice param OOB", kit.id);
                    }
                    KitRow::Mix { track, field, .. } => {
                        assert!((track as usize) < MAX_TRACKS, "{}: Mix track OOB", kit.id);
                        assert!(field <= 9, "{}: Mix field OOB", kit.id);
                    }
                    KitRow::ModSlot { slot, src, dst, voice, .. } => {
                        assert!((slot as usize) < 16, "{}: ModSlot index OOB", kit.id);
                        assert_ne!(
                            DrumModSource::from_index(src as usize),
                            DrumModSource::Off,
                            "{}: ModSlot src decodes to Off (inert route)",
                            kit.id
                        );
                        assert_ne!(
                            DrumModDest::from_index(dst as usize),
                            DrumModDest::Off,
                            "{}: ModSlot dst decodes to Off (inert route)",
                            kit.id
                        );
                        assert!(
                            voice == 0xFF || (voice as usize) < MAX_TRACKS,
                            "{}: ModSlot target_voice OOB",
                            kit.id
                        );
                    }
                    KitRow::Lfo { idx, .. } => assert!(idx <= 1, "{}: Lfo idx OOB", kit.id),
                    KitRow::ModEnv { attack, decay } => assert!(
                        attack.is_finite() && attack >= 0.0 && decay.is_finite() && decay >= 0.0,
                        "{}: ModEnv times must be finite + non-negative",
                        kit.id
                    ),
                    KitRow::Bus { id, .. } => {
                        assert!((1..=9).contains(&id), "{}: Bus id {id} out of 1..=9", kit.id)
                    }
                    KitRow::Sidechain(_) => {}
                }
            }
        }
    }

    #[test]
    fn every_factory_world_renders_finite_and_bounded() {
        // Recall each factory kit exactly as the audio driver does (stage_kit ->
        // adopt_state -> bus rows -> mod fan-in -> sequencer -> render) and run a
        // couple of bars with the macros at both rails. Every sample must be
        // finite + bus-limited, and a GROOVE WORLD must actually make sound.
        let sr = 48_000.0_f32;
        for kit in kits::FACTORY_KITS {
            for macros in [[0.0_f32; 8], [1.0_f32; 8]] {
                let mut state = PersistState::default();
                let staged = stage_kit(&mut state, kit);

                let mut dk = DrumKit::neutral(sr);
                let mut seq = Sequencer::new();
                let mut me = ModEngine::new(sr);
                let mut macros = macros;
                adopt_state(&mut dk, &mut seq, &mut me, &mut macros, &state, true);

                // Drive the staged bus FX through the kit setters (the audio driver
                // routes these via host params; here applied directly).
                if let Some(v) = staged.bus[1] { dk.set_pump(v); }
                if let Some(v) = staged.bus[2] { dk.set_bus_drive(v); }
                if let Some(v) = staged.bus[3] { dk.set_bus_reverb(v); }
                if let Some(v) = staged.bus[4] { dk.set_bus_delay(v); }
                if let Some(v) = staged.bus[5] { dk.set_pump_rate(v); }
                if let Some(v) = staged.bus[6] { dk.set_pump_curve(v); }
                if let Some(v) = staged.bus[7] { dk.set_bus_parallel(v); }
                if let Some(v) = staged.bus[8] { dk.set_bus_transient(v); }
                if let Some(v) = staged.bus[9] { dk.set_gate_time(v * 200.0); }
                dk.set_bus_tempo(120.0);

                seq.set_playing(true);
                let block = 512usize;
                let tempo = 120.0_f64;
                let qn_per_block = (block as f64 / sr as f64) * (tempo / 60.0);
                let mut pos_qn = 0.0_f64;
                let mut peak = 0.0_f32;
                let mut made_sound = false;
                while pos_qn < 8.0 {
                    // 2 bars
                    me.advance(block);
                    dk.set_mod_globals(me.lfo1(), me.lfo2(), me.mod_env());
                    dk.set_mod_wheel(1.0);
                    dk.set_macros(macros);
                    seq.process_block(pos_qn, tempo, sr as f64, block);
                    let pending = seq.pending();
                    let mut ti = 0;
                    for i in 0..block {
                        while ti < pending.len() {
                            let trg = pending[ti];
                            if trg.offset as usize > i {
                                break;
                            }
                            dk.trigger_seq(&trg);
                            ti += 1;
                        }
                        let (l, r) = dk.render();
                        assert!(
                            l.is_finite() && r.is_finite(),
                            "world {} rendered non-finite (macros={:?})",
                            kit.id,
                            macros[0]
                        );
                        peak = peak.max(l.abs()).max(r.abs());
                        if l.abs() > 1e-3 || r.abs() > 1e-3 {
                            made_sound = true;
                        }
                    }
                    pos_qn += qn_per_block;
                }
                assert!(peak <= 1.02, "world {} exceeded the limiter: peak={peak}", kit.id);
                if kit.pattern.is_some() {
                    assert!(made_sound, "GROOVE WORLD {} produced no sound", kit.id);
                }
            }
        }
    }

    /// The real persistence path: program a bank, JSON round-trip the persisted
    /// blob (exactly what `nih-plug` serializes into the host project), and
    /// confirm every kind of programming survives — including the `[Step; 64]`
    /// arrays that go through `serde_big_array`.
    #[test]
    fn persist_state_round_trips_through_json() {
        let mut seq = Sequencer::new();
        seq.select_pattern(5);
        seq.set_step(7, 13, true);
        seq.set_plock(7, 13, percussion_core::LockableParam::Resonance.index(), 0.77);
        seq.set_step(7, 63, true); // last step in the 64-wide lane (the BigArray edge)
        seq.select_pattern(0);
        seq.set_step(1, 2, true);

        let mut persisted = PersistState::default();
        persisted.seq_enabled = false;
        seq.export_into(&mut persisted.seq);
        // ...a per-voice patch edit (snare cutoff) AND a mix edit (snare Send A + mute).
        let mut kit = DrumKit::neutral(48_000.0);
        kit.set_voice_param(2, LockableParam::Cutoff.index(), 0.3);
        kit.set_voice_mix(2, 0, 0.5); // snare reverb send
        kit.set_voice_mix(3, 2, 1.0); // clap mute
        kit.export_patch_into(&mut persisted.voices);
        kit.export_mix_into(&mut persisted.mix);

        let json = serde_json::to_string(&persisted).expect("serialize");
        let back: PersistState = serde_json::from_str(&json).expect("deserialize");
        assert!(!back.seq_enabled, "SEQ-enable must survive");

        let mut restored = Sequencer::new();
        restored.import(&back.seq);
        assert!(restored.step_on(1, 2), "slot 0 edit survives");
        restored.select_pattern(5);
        assert!(restored.step_on(7, 13), "slot 5 edit survives");
        assert!(restored.step_on(7, 63), "step 63 (BigArray edge) survives");
        assert_eq!(restored.pattern.tracks[7].steps[13].plock_count, 1, "p-lock survives");

        // The voice patch survives the JSON round-trip (engine-unit, lossless).
        let mut rkit = DrumKit::neutral(48_000.0);
        rkit.import_patch(&back.voices);
        rkit.import_mix(&back.mix);
        assert!(
            (rkit.voice_param(2, LockableParam::Cutoff.index()) - 0.3).abs() < 1e-4,
            "per-voice patch edit must survive persistence"
        );
        assert!(
            (rkit.voice_param(0, LockableParam::Cutoff.index()) - kit.voice_param(0, LockableParam::Cutoff.index())).abs() < 1e-6,
            "untouched tracks keep their neutral patch"
        );
        assert!((rkit.voice_mix(2, 0) - 0.5).abs() < 1e-6, "Send A must survive persistence");
        assert_eq!(rkit.voice_mix(3, 2), 1.0, "mute must survive persistence");

        // Old projects (no `voices`/`mix` fields) still load (serde default = Neutral).
        let legacy = r#"{"seq":{"patterns":[],"current":0},"seq_enabled":true}"#;
        let _: PersistState = serde_json::from_str(legacy).expect("legacy state without voices/mix must load");
    }
}
