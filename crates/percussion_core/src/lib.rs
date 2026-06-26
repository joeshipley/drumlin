//! `percussion_core` — Drumlin's drum voices + sequencer.
//!
//! The percussion peer of Esker's `synth_core`: pure logic + DSP, no plugin or
//! host types, inline-tested, real-time-safe (fixed-size state, no audio-thread
//! allocation, `flush_denormal` on every recursive write). It builds on the
//! shared `synth_core` primitives (oscillator, noise, filter, drive) and adds the
//! percussion-specific generators (`DahdEnv`, `MetalCluster`, `ClapDiffuser`).
//!
//! M2 scope: Kick, Snare, Closed/Open Hat and Clap voices; choke groups; a
//! single 16-step host-synced sequencer. Toms/perc/cymbal/FM/sample voices,
//! p-locks and the full grid arrive at M4/M5.

pub mod bus;
pub mod clap_diffuser;
pub mod drift;
pub mod kit;
pub mod metal_cluster;
pub mod mod_matrix;
pub mod pitch_env;
pub mod plock;
pub mod resonator;
pub mod rng;
pub mod sequencer;
pub mod tail;
pub mod voice;

#[cfg(test)]
mod golden;

pub use bus::DrumBus;
pub use clap_diffuser::ClapDiffuser;
pub use kit::{track_for_note, DrumKit, VoiceMix, VoiceMixRow, VoicePatch, N_AUX};
pub use metal_cluster::MetalCluster;
pub use mod_matrix::{
    DrumModDest, DrumModMatrix, DrumModSlot, DrumModSource, ALL_VOICES, N_DRUM_DESTS, N_DRUM_SLOTS,
    N_DRUM_SOURCES,
};
pub use pitch_env::DahdEnv;
pub use plock::{LockableParam, PLock, LOCKABLE_PARAMS, MAX_PLOCKS};
pub use resonator::Resonator;
pub use rng::XorShift32;
pub use sequencer::{GrooveTemplate, Pattern, SeqState, Sequencer, Step, Track, TrigCondition};
pub use tail::VoiceTail;
pub use voice::Voice;

/// Fixed track count. Twelve matches the design's voice count and the MPK pad
/// feel (design §3.2); polymeter/golden fixtures bake this in, so it is frozen.
pub const MAX_TRACKS: usize = 12;

/// Max steps per track: 4 pages of 16.
pub const MAX_STEPS: usize = 64;

/// Sequencer master resolution (pulses per quarter note). Divisible by 16ths,
/// triplets and 32nds — room for swing/micro as exact integers (design §4.3).
pub const PPQN: u32 = 384;

/// A scheduled drum hit the sequencer emits and the kit consumes. `offset` is
/// the sample position within the current process block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trigger {
    pub offset: u32,
    pub track: u8,
    /// 0.0..=1.0 (already includes step velocity × track level × humanize).
    pub velocity: f32,
    pub accent: bool,
    /// Per-step parameter locks applied to this hit only.
    pub plocks: [plock::PLock; plock::MAX_PLOCKS],
    pub plock_count: u8,
    /// Seeded per-hit drift randoms, bipolar `-1..1` (pitch, level), from the
    /// sequencer's GROOVE-LOCK RNG. The kit scales them by the voice's DRIFT
    /// amount; `0.0` (the default / live hits) = no drift.
    pub rand_pitch: f32,
    pub rand_level: f32,
    /// Seeded per-hit S&H for the mod matrix's `RandomPerHit` source, bipolar
    /// `-1..1` (independent of the drift draws). `0.0` on live/non-seq hits.
    pub rand_mod: f32,
}

impl Trigger {
    /// The active p-locks for this hit.
    pub fn plocks(&self) -> &[plock::PLock] {
        &self.plocks[..(self.plock_count as usize).min(plock::MAX_PLOCKS)]
    }
}
