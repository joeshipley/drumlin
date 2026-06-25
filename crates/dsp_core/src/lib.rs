//! `dsp_core` — the family's shared, dependency-free DSP primitives.
//!
//! These modules are **copied verbatim from Esker's `synth_core`** (see
//! `docs/drumlin-plan.md` §0). They are kept byte-for-byte identical to Esker's
//! so a fix in either repo can be upstreamed/merged into the other and the
//! family's sound stays bit-compatible. Do not edit them to add Drumlin-specific
//! behavior — percussion-specific code lives in `percussion_core`.
//!
//! Only the **pure-DSP subset** of `synth_core` is copied here. The
//! synth-specific modules (`voice`, `synth`, `arpeggiator`, `golden_default`)
//! are intentionally left behind in Esker — Drumlin doesn't want a polyphonic
//! pitched-voice allocator or an arpeggiator.
//!
//! Everything here is plain Rust that can be exercised with `cargo test` in
//! milliseconds; each module carries its own inline `#[cfg(test)]` tests, and
//! those passing unchanged is the proof that this copy is faithful.

pub mod chorus;
pub mod delay;
pub mod drive;
pub mod dynamics;
pub mod envelope;
pub mod filter;
pub mod lfo;
pub mod mod_matrix;
pub mod oscillator;
pub mod phaser;
pub mod reverb;
pub mod util;
pub mod wavetable;

pub use chorus::Chorus;
pub use delay::{Delay, DelayMode};
pub use drive::{Drive, DriveKind};
pub use dynamics::{Dynamics, LimiterStyle, PumpSource};
pub use envelope::Adsr;
pub use filter::Filter;
pub use lfo::{Lfo, LfoShape};
pub use mod_matrix::{ModDest, ModMatrix, ModSlot, ModSource, N_DESTS, N_SLOTS, N_SOURCES};
pub use oscillator::{Noise, NoiseType, Oscillator, Waveform};
pub use phaser::Phaser;
pub use reverb::{Reverb, ReverbAlgo};
pub use util::flush_denormal;
pub use wavetable::{bank as wavetable_bank, WavetableBank, MIP_LEVELS, N_TABLES, TABLE_LEN};
