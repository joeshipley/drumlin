//! The drum voices and the `Voice` enum the kit dispatches over.
//!
//! Each voice is a self-contained one-shot engine with a uniform interface:
//! `trigger(vel, accent)`, `render() -> (l, r)`, `choke()`, `is_active()`,
//! `reset()`. Fixed-architecture (no `dyn`) so the kit is a plain array and the
//! audio path has no allocation or virtual dispatch.

pub mod clap;
pub mod hat;
pub mod kick;
pub mod snare;

pub use clap::ClapVoice;
pub use hat::HatVoice;
pub use kick::KickVoice;
pub use snare::SnareVoice;

/// One drum track's voice. `Silent` is the placeholder for the 7 tracks not yet
/// implemented in M2 (toms/perc/cymbal/FM/sample arrive at M4).
pub enum Voice {
    Kick(KickVoice),
    Snare(SnareVoice),
    Hat(HatVoice),
    Clap(ClapVoice),
    Silent,
}

impl Voice {
    pub fn set_sample_rate(&mut self, sr: f32) {
        match self {
            Voice::Kick(v) => v.set_sample_rate(sr),
            Voice::Snare(v) => v.set_sample_rate(sr),
            Voice::Hat(v) => v.set_sample_rate(sr),
            Voice::Clap(v) => v.set_sample_rate(sr),
            Voice::Silent => {}
        }
    }

    pub fn trigger(&mut self, velocity: f32, accent: bool) {
        match self {
            Voice::Kick(v) => v.trigger(velocity, accent),
            Voice::Snare(v) => v.trigger(velocity, accent),
            Voice::Hat(v) => v.trigger(velocity, accent),
            Voice::Clap(v) => v.trigger(velocity, accent),
            Voice::Silent => {}
        }
    }

    pub fn render(&mut self) -> (f32, f32) {
        match self {
            Voice::Kick(v) => v.render(),
            Voice::Snare(v) => v.render(),
            Voice::Hat(v) => v.render(),
            Voice::Clap(v) => v.render(),
            Voice::Silent => (0.0, 0.0),
        }
    }

    pub fn choke(&mut self) {
        match self {
            Voice::Kick(v) => v.choke(),
            Voice::Snare(v) => v.choke(),
            Voice::Hat(v) => v.choke(),
            Voice::Clap(v) => v.choke(),
            Voice::Silent => {}
        }
    }

    pub fn is_active(&self) -> bool {
        match self {
            Voice::Kick(v) => v.is_active(),
            Voice::Snare(v) => v.is_active(),
            Voice::Hat(v) => v.is_active(),
            Voice::Clap(v) => v.is_active(),
            Voice::Silent => false,
        }
    }

    pub fn reset(&mut self) {
        match self {
            Voice::Kick(v) => v.reset(),
            Voice::Snare(v) => v.reset(),
            Voice::Hat(v) => v.reset(),
            Voice::Clap(v) => v.reset(),
            Voice::Silent => {}
        }
    }
}
