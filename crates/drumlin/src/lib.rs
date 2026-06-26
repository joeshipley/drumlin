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

use nih_plug::prelude::*;
use nih_plug_webview::{HTMLSource, WebViewEditor};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU8, Ordering};
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
}

impl Default for PersistState {
    fn default() -> Self {
        Self {
            seq: SeqState::default(),
            seq_enabled: true,
            voices: VoicePatch::default(),
            mix: VoiceMix::default(),
            mod_state: ModState::default(),
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

    /// KIT/preset/pattern panic-reset flag, drained at block start.
    fx_reset_pending: Arc<AtomicBool>,

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
            fx_reset_pending: Arc::new(AtomicBool::new(false)),
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
            while let Ok(value) = ctx.next_event() {
                let Ok(action) = serde_json::from_value::<Action>(value) else {
                    continue;
                };
                match action {
                    Action::Init => {
                        // Seed the GUI from the persisted/live state so it shows
                        // exactly what the engine holds (incl. a restored project):
                        // the pattern bank, per-voice patch + mix, the bus-FX
                        // slider positions, and the sidechain toggle. (The bus-FX
                        // params + the SC toggle are host-persisted but the sliders
                        // are otherwise write-only.)
                        if let Ok(s) = params.state.lock() {
                            let mut msg = bank_json(&s.seq, &s.voices, &s.mix, &s.mod_state);
                            // Bus-FX slider values, normalized, in pget! id order 1..=9.
                            msg["busfx"] = json!([
                                params.pump.unmodulated_normalized_value(),
                                params.bus_drive.unmodulated_normalized_value(),
                                params.reverb.unmodulated_normalized_value(),
                                params.delay.unmodulated_normalized_value(),
                                params.pump_rate.unmodulated_normalized_value(),
                                params.pump_curve.unmodulated_normalized_value(),
                                params.parallel.unmodulated_normalized_value(),
                                params.punch.unmodulated_normalized_value(),
                                params.gate_time.unmodulated_normalized_value(),
                            ]);
                            msg["sidechain"] = json!(params.sidechain_key.value());
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
            if !self.seq_dirty {
                self.seq.import(&state.seq);
                self.seq_enabled.store(state.seq_enabled, Ordering::Relaxed);
                self.persisted_seq_enabled = state.seq_enabled;
            }
            self.kit.import_patch(&state.voices);
            self.kit.import_mix(&state.mix);
            // Seed the mod matrix + LFO/env config + macros from the project.
            let ms = &state.mod_state;
            for (i, sl) in ms.matrix.slots.iter().enumerate() {
                self.kit.set_mod_slot(i, sl.src, sl.dst, sl.depth, sl.target_voice);
            }
            self.mod_engine.set_lfo1(lfo_shape(ms.lfo1.shape), ms.lfo1.rate_hz, ms.lfo1.depth, ms.lfo1.retrig);
            self.mod_engine.set_lfo2(lfo_shape(ms.lfo2.shape), ms.lfo2.rate_hz, ms.lfo2.depth, ms.lfo2.retrig);
            self.mod_engine.set_mod_env(ms.env_attack, ms.env_decay);
            self.macros = ms.macros;
            self.kit.set_macros(self.macros);
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
        // Once per block: panic-reset on a pending KIT/pattern jump.
        if self.fx_reset_pending.swap(false, Ordering::Relaxed) {
            self.kit.reset();
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
                    // Mod-wheel (CC1) feeds the ModWheel mod source. Latched for
                    // the next block's fan-in (sources are sampled per hit).
                    NoteEvent::MidiCC { cc: 1, value, .. } => self.mod_wheel = value,
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
