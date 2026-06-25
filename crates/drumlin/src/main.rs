//! Standalone build of Drumlin — runs as its own app over CoreAudio, no DAW.
//!
//! It needs a MIDI input to trigger pads. Useful invocations:
//!   # list available MIDI inputs (pass an invalid name):
//!   cargo run --release --bin drumlin -- --midi-input ""
//!   # then run it pointed at your controller:
//!   cargo run --release --bin drumlin -- --midi-input "Your Controller"
//!
//! Audio goes to the default output device via the auto backend (CoreAudio on
//! macOS).

use nih_plug::prelude::*;

use drumlin::Drumlin;

fn main() {
    nih_export_standalone::<Drumlin>();
}
