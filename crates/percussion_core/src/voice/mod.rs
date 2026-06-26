//! The drum voices and the `Voice` enum the kit dispatches over.
//!
//! Each voice is a self-contained one-shot engine with a uniform interface:
//! `trigger(vel, accent)`, `render() -> (l, r)`, `choke()`, `is_active()`,
//! `reset()`. Fixed-architecture (no `dyn`) so the kit is a plain array and the
//! audio path has no allocation or virtual dispatch.

pub mod clap;
pub mod cowbell;
pub mod hat;
pub mod kick;
pub mod rim;
pub mod snare;
pub mod tom;
pub mod zap;

pub use clap::ClapVoice;
pub use cowbell::CowbellVoice;
pub use hat::HatVoice;
pub use kick::KickVoice;
pub use rim::RimVoice;
pub use snare::SnareVoice;
pub use tom::TomVoice;
pub use zap::ZapVoice;

/// One drum track's voice. `Kick`/`Hat` cover several tracks via constructor
/// variants (sub kick, ride cymbal). `Silent` is the placeholder for any track
/// not yet voiced.
pub enum Voice {
    Kick(KickVoice),
    Snare(SnareVoice),
    Hat(HatVoice),
    Clap(ClapVoice),
    Tom(TomVoice),
    Rim(RimVoice),
    Cowbell(CowbellVoice),
    Zap(ZapVoice),
    Silent,
}

impl Voice {
    pub fn set_sample_rate(&mut self, sr: f32) {
        match self {
            Voice::Kick(v) => v.set_sample_rate(sr),
            Voice::Snare(v) => v.set_sample_rate(sr),
            Voice::Hat(v) => v.set_sample_rate(sr),
            Voice::Clap(v) => v.set_sample_rate(sr),
            Voice::Tom(v) => v.set_sample_rate(sr),
            Voice::Rim(v) => v.set_sample_rate(sr),
            Voice::Cowbell(v) => v.set_sample_rate(sr),
            Voice::Zap(v) => v.set_sample_rate(sr),
            Voice::Silent => {}
        }
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        match self {
            Voice::Kick(v) => v.trigger(velocity, accent),
            Voice::Snare(v) => v.trigger(velocity, accent),
            Voice::Hat(v) => v.trigger(velocity, accent),
            Voice::Clap(v) => v.trigger(velocity, accent),
            Voice::Tom(v) => v.trigger(velocity, accent),
            Voice::Rim(v) => v.trigger(velocity, accent),
            Voice::Cowbell(v) => v.trigger(velocity, accent),
            Voice::Zap(v) => v.trigger(velocity, accent),
            Voice::Silent => {}
        }
    }

    /// Per-hit pitch drift, in cents (set just before `trigger`). Pitched voices
    /// fold it into their frequency; clap (noise) and rim (fixed-partial, v1) are
    /// no-ops. `0.0` is a bit-exact no-op (ratio `2^(0/1200) = 1.0`).
    pub fn set_pitch_drift_cents(&mut self, cents: f32) {
        match self {
            Voice::Kick(v) => v.set_pitch_drift_cents(cents),
            Voice::Snare(v) => v.set_pitch_drift_cents(cents),
            Voice::Hat(v) => v.set_pitch_drift_cents(cents),
            Voice::Tom(v) => v.set_pitch_drift_cents(cents),
            Voice::Cowbell(v) => v.set_pitch_drift_cents(cents),
            Voice::Zap(v) => v.set_pitch_drift_cents(cents),
            Voice::Clap(_) | Voice::Rim(_) | Voice::Silent => {}
        }
    }

    pub fn render(&mut self) -> (f32, f32) {
        match self {
            Voice::Kick(v) => v.render(),
            Voice::Snare(v) => v.render(),
            Voice::Hat(v) => v.render(),
            Voice::Clap(v) => v.render(),
            Voice::Tom(v) => v.render(),
            Voice::Rim(v) => v.render(),
            Voice::Cowbell(v) => v.render(),
            Voice::Zap(v) => v.render(),
            Voice::Silent => (0.0, 0.0),
        }
    }

    pub fn choke(&mut self) {
        match self {
            Voice::Kick(v) => v.choke(),
            Voice::Snare(v) => v.choke(),
            Voice::Hat(v) => v.choke(),
            Voice::Clap(v) => v.choke(),
            Voice::Tom(v) => v.choke(),
            Voice::Rim(v) => v.choke(),
            Voice::Cowbell(v) => v.choke(),
            Voice::Zap(v) => v.choke(),
            Voice::Silent => {}
        }
    }

    pub fn is_active(&self) -> bool {
        match self {
            Voice::Kick(v) => v.is_active(),
            Voice::Snare(v) => v.is_active(),
            Voice::Hat(v) => v.is_active(),
            Voice::Clap(v) => v.is_active(),
            Voice::Tom(v) => v.is_active(),
            Voice::Rim(v) => v.is_active(),
            Voice::Cowbell(v) => v.is_active(),
            Voice::Zap(v) => v.is_active(),
            Voice::Silent => false,
        }
    }

    pub fn reset(&mut self) {
        match self {
            Voice::Kick(v) => v.reset(),
            Voice::Snare(v) => v.reset(),
            Voice::Hat(v) => v.reset(),
            Voice::Clap(v) => v.reset(),
            Voice::Tom(v) => v.reset(),
            Voice::Rim(v) => v.reset(),
            Voice::Cowbell(v) => v.reset(),
            Voice::Zap(v) => v.reset(),
            Voice::Silent => {}
        }
    }
}
