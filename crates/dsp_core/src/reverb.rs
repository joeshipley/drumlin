//! A feedback-delay-network (FDN) reverb — the plate/hall "send" effect.
//!
//! This is the lush, blooming tail you ride up under a pad with a console aux
//! send. It is built as a **send / return**, not a serial insert: the dry signal
//! always passes through untouched, and a *scaled copy* of the wet tail is added
//! on top. Turning the send up makes the room bloom without ever thinning the
//! dry — exactly how a hardware aux send behaves, and what makes it a musical
//! "ride" (the signature Plate-224 send move).
//!
//! # What an FDN actually is
//!
//! A single delay line fed back on itself is an echo: one repeat, then a quieter
//! repeat, then quieter still. That is too sparse to sound like a *room* — real
//! reverb is thousands of overlapping reflections per second, so dense they blur
//! into a smooth wash.
//!
//! A **feedback delay network** gets that density cheaply. Take `N` delay lines
//! of different lengths, and instead of feeding each one straight back into
//! itself, feed *every* line's output into *every* line's input through a mixing
//! matrix. After a few trips around the loop, one input impulse has scattered
//! into a thicket of echoes whose count multiplies each pass — the exponential
//! build-up that gives a real room its dense, grainless tail.
//!
//! The trick that makes it stable is the choice of mixing matrix. If the matrix
//! is **orthogonal** (its rows are unit vectors at right angles to each other) it
//! preserves the total energy of the signal vector as it mixes — it only *rotates*
//! energy between the lines, never amplifies it. That means the decay rate is set
//! *entirely* by the per-line feedback gains, cleanly and predictably, instead of
//! the matrix itself ringing or blowing up. We use a normalized **Hadamard**
//! matrix: an 8x8 grid of `±1/sqrt(8)` whose rows are mutually orthogonal, which
//! we apply with a fast in-place butterfly (no matrix multiply, no `Vec`, no
//! deps).
//!
//! ```text
//!   in ─► [predelay] ─►(+)─►[delay 1]─[damp]─►┐
//!                       (+)─►[delay 2]─[damp]─►│
//!                       ...                    ├─► Hadamard mix ─► feedback ──┐
//!                       (+)─►[delay 8]─[damp]─►┘            │                 │
//!                        ▲                                  └─► sum ─► [hi/lo cut] ─► wet
//!                        └──────────────────────────────────────────────────┘
//! ```
//!
//! # Setting the decay time (RT60)
//!
//! "Decay" is given as an **RT60**: the time, in seconds, for the tail to fall by
//! 60 dB. Each delay line of length `D` samples is tapped `SR/D` times per second,
//! and each pass multiplies by its feedback gain `g`. For the level to reach
//! `-60 dB = 10^-3` after `RT60` seconds, we need:
//!
//! ```text
//!     g ^ (RT60 * SR / D) = 10^-3
//!  => g = 10 ^ ( -3 * D / (RT60 * SR) )
//! ```
//!
//! Short lines (small `D`) recirculate more often, so they need a gain closer to
//! 1 to take the same wall-clock time to decay; long lines need less. Computing
//! `g` *per line* from this formula is what keeps every line decaying in lockstep
//! so the whole tail fades as one smooth event rather than a pile-up of mismatched
//! echoes.
//!
//! # Damping, size, and brightness
//!
//! * **Damping** puts a one-pole low-pass *inside* each feedback line, so high
//!   frequencies lose energy faster than lows on every trip — air and soft
//!   furnishings absorbing treble, the natural "darkening as it decays" of a real
//!   space.
//! * **Size** scales every delay length together. Longer lines = a bigger room
//!   with later, sparser early reflections; shorter = a tight plate.
//! * **Hi-cut / Lo-cut** are one-pole filters on the *wet output* (not in the
//!   loop): lo-cut keeps the tail from muddying the low end, hi-cut sets the
//!   overall brightness/air of the return.
//! * **Modulation** slowly wobbles a couple of the line lengths with a slow LFO.
//!   A static FDN can develop a faint metallic "ring" where the fixed line lengths
//!   beat against each other; gently chorusing the delays smears those resonances
//!   into a shimmer (the EMT-224 "modulated plate" trick) and kills the ring.
//! * **Width** scales the stereo *side* component of the wet, narrowing or
//!   widening the return without touching the dry.
//! * **Freeze** drives every feedback gain to ~1.0 and mutes the input: the tail
//!   already circulating can neither grow nor decay, so it sustains forever — an
//!   instant infinite pad.
//!
//! # Real-time safety
//!
//! Every delay line is a `Vec<f32>` allocated **once** in [`Reverb::new`], sized
//! for the largest room at the highest sample rate. [`Reverb::process_send`] only
//! reads, writes, and does float math on fields that already exist — no
//! allocation, no locks. All gains and filter states are guarded so no parameter
//! value (including NaN, infinities, or a zero RT60) can produce a NaN or let the
//! tail run away. In addition, every write into the recirculating state (the FDN
//! line buffers and the per-line damping one-pole) is passed through
//! [`flush_denormal`], so even a non-finite *signal* injected into the loop is
//! purged within one sample rather than latched forever.

use crate::util::flush_denormal;
use std::f32::consts::TAU;

/// The number of delay lines in the network. Eight is the classic sweet spot:
/// dense enough that the tail is smooth and grainless, small enough that the
/// Hadamard butterfly is a handful of adds per sample. It is a power of two,
/// which the in-place Hadamard transform requires.
const NUM_LINES: usize = 8;

/// Base delay-line lengths in **samples at 44.1 kHz**, before the `size` scale.
///
/// These are mutually *coprime* (no common factors) so the lines never line up
/// and reinforce into a periodic, comb-filtered "boing". They are the lengths for
/// the default Plate algorithm; other algorithms scale these (see
/// [`ReverbAlgo::length_scale`]). Re-scaled to the running sample rate in
/// [`Reverb::rebuild_delays`].
const BASE_LENGTHS_44K: [usize; NUM_LINES] = [1153, 1361, 1733, 2069, 2503, 2861, 3209, 3539];

/// Hard ceiling on a single line's length in samples, used to size the ring
/// buffers in [`Reverb::new`]. The longest base line (~3539 at 44.1 kHz) times
/// the biggest `size` scale (~2.2x for a Hall) times the highest sample-rate
/// ratio we expect (192 / 44.1 ≈ 4.35x) plus modulation headroom is comfortably
/// under this. Sizing once for the worst case is what lets `process_send` never
/// allocate.
const MAX_LINE_LEN: usize = 1 << 16; // 65536 samples ≈ 0.34 s @ 192 kHz

/// Maximum predelay in seconds — the size of the predelay ring buffer.
const MAX_PREDELAY_SECS: f32 = 0.25;

/// The two delay lines we modulate for the anti-ring shimmer, and the (slow) LFO
/// rates in Hz for each. Different, slow, incommensurate rates keep the
/// modulation from itself becoming periodic.
const MOD_LINES: [usize; 2] = [2, 5];
const MOD_RATES_HZ: [f32; 2] = [0.7, 1.1];

/// The largest peak feedback gain we will ever apply to a line, even in freeze.
/// Kept a hair below 1.0 so a frozen tail holds essentially forever yet can never
/// integrate up to infinity through accumulated round-off.
const MAX_FEEDBACK: f32 = 0.9995;

/// The reverb algorithm flavour. Each picks a different line-length scale and a
/// damping bias so the four presets feel like genuinely different spaces while
/// sharing one FDN engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReverbAlgo {
    /// EMT-224-style bright, dense plate: short lines, light extra damping.
    Plate224,
    /// A large hall: long lines, late and sparse, warmer.
    Hall,
    /// A spring-tank twang: fewer-feeling, dispersive, short and resonant.
    Spring,
    /// A small room: short dark lines, quick decay character.
    Room,
}

impl ReverbAlgo {
    /// Multiplier applied to every base line length for this algorithm. Bigger =
    /// a physically bigger space (later reflections).
    fn length_scale(self) -> f32 {
        match self {
            ReverbAlgo::Plate224 => 1.0,
            ReverbAlgo::Hall => 2.2,
            ReverbAlgo::Spring => 0.6,
            ReverbAlgo::Room => 0.75,
        }
    }

    /// Extra damping bias added to the user's `damping` knob (clamped to 1.0).
    /// Plates are bright (little extra), halls and rooms are warmer/darker.
    fn damping_bias(self) -> f32 {
        match self {
            ReverbAlgo::Plate224 => 0.0,
            ReverbAlgo::Hall => 0.15,
            ReverbAlgo::Spring => 0.1,
            ReverbAlgo::Room => 0.25,
        }
    }
}

/// One delay line in the network: a ring buffer plus the one-pole damping state
/// that low-passes the signal on its way around the loop.
#[derive(Clone, Debug)]
struct DelayLine {
    buffer: Vec<f32>,
    write_pos: usize,
    /// The line's *effective* length in samples (after size scaling). Always
    /// `>= 1` and `< buffer.len()`.
    len: f32,
    /// One-pole low-pass state for in-loop damping.
    damp_state: f32,
}

impl DelayLine {
    fn new() -> Self {
        Self {
            buffer: vec![0.0; MAX_LINE_LEN],
            write_pos: 0,
            len: 1.0,
            damp_state: 0.0,
        }
    }

    fn clear(&mut self) {
        for s in &mut self.buffer {
            *s = 0.0;
        }
        self.write_pos = 0;
        self.damp_state = 0.0;
    }

    /// Read the line `delay` samples in the past, linearly interpolated so a
    /// fractional (modulated) delay glides smoothly with no zipper.
    #[inline]
    fn read(&self, delay: f32) -> f32 {
        let cap = self.buffer.len();
        // Clamp the requested delay into the valid window so a wild modulation
        // value can never index out of bounds or read the sample we are about to
        // overwrite.
        let d = delay.max(1.0).min(cap as f32 - 2.0);
        let read = (self.write_pos as f32 - d).rem_euclid(cap as f32);
        let i0 = read.floor() as usize;
        let frac = read - i0 as f32;
        let a = self.buffer[i0 % cap];
        let b = self.buffer[(i0 + 1) % cap];
        a + (b - a) * frac
    }

    /// Write a sample at the head and advance the write position.
    #[inline]
    fn write(&mut self, x: f32) {
        let cap = self.buffer.len();
        self.buffer[self.write_pos] = x;
        self.write_pos = (self.write_pos + 1) % cap;
    }
}

/// A simple pre-delay: one ring buffer that delays the input by a few
/// milliseconds before it enters the network, so the first reflection arrives
/// after the dry note rather than smeared on top of it.
#[derive(Clone, Debug)]
struct PreDelay {
    buffer: Vec<f32>,
    write_pos: usize,
    len: usize,
}

impl PreDelay {
    fn new(max_len: usize) -> Self {
        Self {
            buffer: vec![0.0; max_len.max(1)],
            write_pos: 0,
            len: 1,
        }
    }

    fn clear(&mut self) {
        for s in &mut self.buffer {
            *s = 0.0;
        }
        self.write_pos = 0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let cap = self.buffer.len();
        let read = (self.write_pos + cap - self.len) % cap;
        let out = self.buffer[read];
        self.buffer[self.write_pos] = x;
        self.write_pos = (self.write_pos + 1) % cap;
        out
    }
}

/// A feedback-delay-network plate/hall reverb, used as a stereo aux **send**.
///
/// Construct once per plugin instance, set the sample rate, push block-rate
/// parameters through the setters, and call [`Reverb::process_send`] per sample.
/// The FDN runs continuously (even at send 0) so changing the send level never
/// restarts the tail — a swell mid-note is seamless.
///
/// Real-time safe: all buffers are allocated in [`Reverb::new`]; the process path
/// only does float math on existing fields.
#[derive(Clone, Debug)]
pub struct Reverb {
    sample_rate: f32,

    // --- The network ---
    lines: Vec<DelayLine>,
    predelay_l: PreDelay,
    predelay_r: PreDelay,

    // --- Block-rate parameters (already in engineering units) ---
    on: bool,
    algo: ReverbAlgo,
    decay_secs: f32,
    size: f32,
    damping: f32,
    locut_hz: f32,
    hicut_hz: f32,
    modulation: f32,
    width: f32,
    freeze: bool,
    /// Predelay length in samples (applied to both channels).
    predelay_samples: usize,

    // --- Send / return levels (passed in each block, smoothed at param level) ---
    send: f32,
    return_gain: f32,

    // --- Derived per-line state ---
    /// Per-line feedback gain derived from the RT60 and each line's length.
    feedback: [f32; NUM_LINES],
    /// One-pole damping coefficient shared by every line (0 = no damping).
    damp_coeff: f32,

    // --- Modulation LFOs ---
    mod_phase: [f32; 2],

    // --- Output filters (on the wet, one pole each, per channel) ---
    locut_state_l: f32,
    locut_state_r: f32,
    hicut_state_l: f32,
    hicut_state_r: f32,
    locut_coeff: f32,
    hicut_coeff: f32,
}

impl Reverb {
    /// Create a reverb at the given sample rate with sensible plate defaults
    /// (the contract's defaults: 2.5 s decay, size 0.6, light damping, a touch of
    /// modulation, full width). The FDN starts silent.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let max_predelay = (MAX_PREDELAY_SECS * sr).ceil() as usize + 2;

        let mut lines = Vec::with_capacity(NUM_LINES);
        for _ in 0..NUM_LINES {
            lines.push(DelayLine::new());
        }

        let mut reverb = Self {
            sample_rate: sr,
            lines,
            predelay_l: PreDelay::new(max_predelay),
            predelay_r: PreDelay::new(max_predelay),
            on: true,
            algo: ReverbAlgo::Plate224,
            decay_secs: 2.5,
            size: 0.6,
            damping: 0.4,
            locut_hz: 80.0,
            hicut_hz: 9_000.0,
            modulation: 0.2,
            width: 1.0,
            freeze: false,
            predelay_samples: (0.020 * sr) as usize,
            send: 0.0,
            return_gain: 1.0,
            feedback: [0.0; NUM_LINES],
            damp_coeff: 0.0,
            mod_phase: [0.0; 2],
            locut_state_l: 0.0,
            locut_state_r: 0.0,
            hicut_state_l: 0.0,
            hicut_state_r: 0.0,
            locut_coeff: 0.0,
            hicut_coeff: 0.0,
        };
        reverb.rebuild_delays();
        reverb.update_feedback();
        reverb.update_damping();
        reverb.update_output_filters();
        reverb
    }

    /// Change the sample rate. Clears every buffer (so stale, mis-scaled audio
    /// cannot replay as a click) and recomputes all length- and time-derived
    /// coefficients against the new rate.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.clear();
        self.predelay_samples = self
            .predelay_samples
            .min(self.predelay_l.buffer.len().saturating_sub(2));
        self.rebuild_delays();
        self.update_feedback();
        self.update_damping();
        self.update_output_filters();
    }

    /// Clear all audio memory (delay lines, predelay, filter states, LFO phases)
    /// without touching the parameters. Use on transport stop or sample-rate
    /// change so the tail starts from silence.
    pub fn clear(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        self.predelay_l.clear();
        self.predelay_r.clear();
        self.locut_state_l = 0.0;
        self.locut_state_r = 0.0;
        self.hicut_state_l = 0.0;
        self.hicut_state_r = 0.0;
        self.mod_phase = [0.0; 2];
    }

    /// Set all block-rate parameters at once. Values are expected already mapped
    /// to engineering units by the plugin layer; everything here is clamped so no
    /// input can NaN the tail.
    ///
    /// * `decay_secs` — RT60, the 60 dB decay time in seconds.
    /// * `size` — `0..1` room-size scale.
    /// * `predelay_secs` — gap before the tail starts (`0..0.25 s`).
    /// * `damping` — `0..1`, high-frequency absorption in the loop.
    /// * `locut_hz` / `hicut_hz` — output high-pass / low-pass corners.
    /// * `modulation` — `0..1`, anti-ring shimmer depth.
    /// * `width` — `0..1` stereo width of the return.
    /// * `freeze` — latch the tail to infinite sustain and mute the input.
    #[allow(clippy::too_many_arguments)]
    pub fn set_params(
        &mut self,
        on: bool,
        algo: ReverbAlgo,
        decay_secs: f32,
        size: f32,
        predelay_secs: f32,
        damping: f32,
        locut_hz: f32,
        hicut_hz: f32,
        modulation: f32,
        width: f32,
        freeze: bool,
    ) {
        self.on = on;

        // Re-scale the lines only when the algorithm or size actually changes.
        let size = clamp01(size);
        if algo != self.algo || (size - self.size).abs() > 1.0e-6 {
            self.algo = algo;
            self.size = size;
            self.rebuild_delays();
            // Feedback gains depend on the (now changed) line lengths.
            self.update_feedback();
        }

        let decay = guard(decay_secs, 2.5).clamp(0.05, 120.0);
        if (decay - self.decay_secs).abs() > 1.0e-6 || freeze != self.freeze {
            self.decay_secs = decay;
            self.freeze = freeze;
            self.update_feedback();
        }
        self.freeze = freeze;

        let damping = clamp01(damping);
        if (damping - self.damping).abs() > 1.0e-6 {
            self.damping = damping;
            self.update_damping();
        }

        let locut = guard(locut_hz, 80.0).max(10.0).min(self.nyquist());
        let hicut = guard(hicut_hz, 9_000.0).max(100.0).min(self.nyquist());
        if (locut - self.locut_hz).abs() > 1.0e-3 || (hicut - self.hicut_hz).abs() > 1.0e-3 {
            self.locut_hz = locut;
            self.hicut_hz = hicut;
            self.update_output_filters();
        }

        self.modulation = clamp01(modulation);
        self.width = clamp01(width);

        let pd = guard(predelay_secs, 0.0).clamp(0.0, MAX_PREDELAY_SECS);
        self.predelay_samples = ((pd * self.sample_rate) as usize)
            .max(1)
            .min(self.predelay_l.buffer.len().saturating_sub(2));
        self.predelay_l.len = self.predelay_samples;
        self.predelay_r.len = self.predelay_samples;
    }

    /// Set the aux **send** and **return** levels for this block. The send is the
    /// rideable headline control (smoothed at the param level for zipper-free
    /// rides); the return is a fixed make-up gain on the wet. Both are guarded.
    pub fn set_send(&mut self, send: f32, return_gain: f32) {
        self.send = guard(send, 0.0).clamp(0.0, 4.0);
        self.return_gain = guard(return_gain, 1.0).clamp(0.0, 8.0);
    }

    /// Process one stereo sample as a **send**: returns the full
    /// `dry + send * return_gain * wet` signal.
    ///
    /// The dry pair passes through at unity (a console aux send never attenuates
    /// the channel), and a scaled copy of the FDN's wet tail is *added* on top.
    /// `send == 0` yields exactly the dry input, yet the network keeps running so
    /// a send swell mid-note is seamless. `on == false` is a true bypass for CPU.
    ///
    /// Real-time safe: no allocation, no locks.
    #[inline]
    pub fn process_send(&mut self, l: f32, r: f32) -> (f32, f32) {
        if !self.on {
            return (l, r);
        }

        let (wet_l, wet_r) = self.process_wet(l, r);
        let g = self.send * self.return_gain;
        (l + g * wet_l, r + g * wet_r)
    }

    /// Run the FDN for one stereo sample and return *only* the wet output. Kept
    /// separate so the network advances every sample regardless of the send level
    /// (so the tail and the GUI "ride halo" stay live even at send 0).
    #[inline]
    fn process_wet(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        // In freeze we mute the input so the circulating tail neither grows nor
        // shrinks — an instant infinite pad.
        let (drive_l, drive_r) = if self.freeze {
            (0.0, 0.0)
        } else {
            (in_l, in_r)
        };

        // Predelay: a short gap before the tail begins, per channel.
        let pre_l = self.predelay_l.process(drive_l);
        let pre_r = self.predelay_r.process(drive_r);
        // The network is mono-summed at the input; stereo emerges from the
        // different lines being routed to L/R at the output.
        let input = (pre_l + pre_r) * 0.5;

        // Advance the two modulation LFOs (cheap; only used if modulation > 0).
        let mod_depth = self.modulation * 12.0; // up to ±12 samples of wobble
        for (phase, &rate) in self.mod_phase.iter_mut().zip(&MOD_RATES_HZ) {
            *phase += rate / self.sample_rate;
            if *phase >= 1.0 {
                *phase -= 1.0;
            }
        }

        // 1) Read each line at its (optionally modulated) length, applying the
        //    in-loop damping low-pass as we read.
        let mut node = [0.0f32; NUM_LINES];
        for (i, line) in self.lines.iter_mut().enumerate() {
            let mut delay = line.len;
            // Modulate two of the lines for the anti-ring shimmer.
            if mod_depth > 0.0 {
                if i == MOD_LINES[0] {
                    delay += mod_depth * (self.mod_phase[0] * TAU).sin();
                } else if i == MOD_LINES[1] {
                    delay += mod_depth * (self.mod_phase[1] * TAU).sin();
                }
            }
            let raw = line.read(delay);
            // One-pole low-pass: state += coeff*(raw - state); damping=0 -> bypass.
            // Flush so a non-finite `raw` (or denormal decay) can't latch into the
            // recursive damping state and ring forever.
            line.damp_state =
                flush_denormal(line.damp_state + self.damp_coeff * (raw - line.damp_state));
            node[i] = if self.damp_coeff > 0.0 {
                line.damp_state
            } else {
                raw
            };
        }

        // 2) Mix the line outputs through the orthogonal Hadamard matrix. This is
        //    the energy-preserving rotation that scatters echoes between lines.
        let mixed = hadamard8(node);

        // 3) Feed back: each line is re-driven by its mixed value (scaled by its
        //    per-line feedback gain) plus the fresh input, then written.
        for (i, line) in self.lines.iter_mut().enumerate() {
            let fb = mixed[i] * self.feedback[i] + input;
            // A safety clamp keeps a hot line bounded, but `NaN.clamp(-32, 32)`
            // still returns NaN — so we *also* flush, which folds any non-finite
            // (or denormal-tiny) value to 0.0. Without this a single injected NaN
            // would be re-read (`line.read`), spread to all 8 lines through the
            // Hadamard mix, and re-written every sample, latching the whole FDN
            // dead permanently.
            line.write(flush_denormal(fb.clamp(-32.0, 32.0)));
        }

        // 4) Build the stereo wet from the line outputs. Even lines lean left,
        //    odd lines lean right, so the eight decorrelated taps spread into a
        //    wide stereo image. Scale by 1/sqrt(lines/2) to keep the level sane.
        let mut sum_l = 0.0f32;
        let mut sum_r = 0.0f32;
        for (i, &n) in node.iter().enumerate() {
            if i % 2 == 0 {
                sum_l += n;
            } else {
                sum_r += n;
            }
        }
        let norm = 1.0 / ((NUM_LINES / 2) as f32).sqrt();
        let mut wet_l = sum_l * norm;
        let mut wet_r = sum_r * norm;

        // 5) Output filters: lo-cut (high-pass) then hi-cut (low-pass), one pole
        //    each, per channel. These shape the *return*, not the loop.
        wet_l = self.apply_output_filters(wet_l, false);
        wet_r = self.apply_output_filters(wet_r, true);

        // 6) Width: mid/side scaling of the side component only.
        let mid = (wet_l + wet_r) * 0.5;
        let side = (wet_l - wet_r) * 0.5 * self.width;
        let out_l = mid + side;
        let out_r = mid - side;

        // Final NaN guard: if anything slipped through, emit silence rather than
        // poison the bus.
        (sanitize(out_l), sanitize(out_r))
    }

    /// Apply the lo-cut (HP) and hi-cut (LP) one-pole filters to one wet sample.
    /// `right` selects which channel's filter state to use.
    #[inline]
    fn apply_output_filters(&mut self, x: f32, right: bool) -> f32 {
        // Lo-cut: a one-pole high-pass is (input − low-passed input).
        let (locut_state, hicut_state) = if right {
            (&mut self.locut_state_r, &mut self.hicut_state_r)
        } else {
            (&mut self.locut_state_l, &mut self.hicut_state_l)
        };

        // Both one-poles are recursive: flush each state so a denormal can't
        // trickle through the output forever (and a NaN can't latch). Folding the
        // state to 0.0 lets the wet output reach bit-exact silence on a dead tail.
        *locut_state = flush_denormal(*locut_state + self.locut_coeff * (x - *locut_state));
        let hp = x - *locut_state;

        // Hi-cut: a one-pole low-pass.
        *hicut_state = flush_denormal(*hicut_state + self.hicut_coeff * (hp - *hicut_state));
        *hicut_state
    }

    // ----- Coefficient derivation -------------------------------------------

    fn nyquist(&self) -> f32 {
        (self.sample_rate * 0.49).max(100.0)
    }

    /// Re-scale every delay line's effective length for the current algorithm +
    /// size, and clamp it inside its buffer. Called only when algo/size/sample
    /// rate change — never on the hot path.
    fn rebuild_delays(&mut self) {
        let sr_ratio = self.sample_rate / 44_100.0;
        let algo_scale = self.algo.length_scale();
        // `size` 0..1 maps to a 0.5x..1.5x length scale around the algo base.
        let size_scale = 0.5 + self.size;
        for (i, line) in self.lines.iter_mut().enumerate() {
            let base = BASE_LENGTHS_44K[i] as f32;
            let len = base * sr_ratio * algo_scale * size_scale;
            // Leave headroom for modulation and interpolation at both ends.
            line.len = len.max(4.0).min(line.buffer.len() as f32 - 32.0);
        }
    }

    /// Recompute the per-line feedback gain from the RT60 and each line's length:
    /// `g = 10^(-3 * len / (RT60 * SR))`. In freeze we override to ~1.0 so the
    /// tail sustains. All gains are clamped strictly below 1 for stability.
    fn update_feedback(&mut self) {
        if self.freeze {
            for g in &mut self.feedback {
                *g = MAX_FEEDBACK;
            }
            return;
        }
        let rt60 = self.decay_secs.max(0.05);
        for (i, line) in self.lines.iter().enumerate() {
            let exponent = -3.0 * line.len / (rt60 * self.sample_rate);
            let g = 10.0f32.powf(exponent);
            self.feedback[i] = g.clamp(0.0, MAX_FEEDBACK);
        }
    }

    /// Recompute the shared in-loop damping coefficient from the `damping` knob
    /// plus the algorithm's bias. The coefficient is the one-pole low-pass blend
    /// factor: 0 = no damping (full bypass), approaching 1 = heavy treble loss.
    fn update_damping(&mut self) {
        let amount = clamp01(self.damping + self.algo.damping_bias());
        // Map 0..1 to a one-pole coefficient. We keep it comfortably below 1 so a
        // damped line still passes *some* signal (a coeff of exactly 1 would freeze
        // the low-pass state and silence the line).
        self.damp_coeff = amount * 0.95;
    }

    /// Recompute the output lo-cut / hi-cut one-pole coefficients from their
    /// corner frequencies. Standard one-pole RC mapping:
    /// `coeff = 1 − exp(−2π·fc/SR)`.
    fn update_output_filters(&mut self) {
        self.locut_coeff = one_pole_coeff(self.locut_hz, self.sample_rate);
        self.hicut_coeff = one_pole_coeff(self.hicut_hz, self.sample_rate);
    }

    // ----- Introspection (tests / UI) ---------------------------------------

    /// The per-line feedback gains currently in effect (for tests / debugging).
    pub fn feedback_gains(&self) -> [f32; NUM_LINES] {
        self.feedback
    }

    /// The effective length, in samples, of line `i` (for tests / debugging).
    pub fn line_len(&self, i: usize) -> f32 {
        self.lines[i].len
    }

    /// Whether the network is currently frozen.
    pub fn is_frozen(&self) -> bool {
        self.freeze
    }
}

/// Apply the normalized 8-point Hadamard transform in place via the fast
/// butterfly (3 stages of pairwise add/subtract), then scale by `1/sqrt(8)` so
/// the transform is *orthonormal* — it preserves the energy of the input vector.
/// That energy-preservation is exactly what makes the FDN's decay rate depend
/// only on the per-line feedback gains, never on the matrix.
#[inline]
fn hadamard8(mut v: [f32; 8]) -> [f32; 8] {
    // Stage 1: pairs (0,1) (2,3) (4,5) (6,7)
    let mut step = 1;
    while step < 8 {
        let mut i = 0;
        while i < 8 {
            for j in i..i + step {
                let a = v[j];
                let b = v[j + step];
                v[j] = a + b;
                v[j + step] = a - b;
            }
            i += step * 2;
        }
        step *= 2;
    }
    let norm = 1.0 / (8.0f32).sqrt();
    for x in &mut v {
        *x *= norm;
    }
    v
}

/// One-pole low-pass blend coefficient for corner `fc` at sample rate `sr`:
/// `1 − exp(−2π·fc/sr)`, clamped to `(0, 1)` so it is always a stable, usable
/// blend factor.
#[inline]
fn one_pole_coeff(fc: f32, sr: f32) -> f32 {
    let c = 1.0 - (-TAU * fc / sr).exp();
    c.clamp(1.0e-5, 0.9999)
}

/// Clamp to `0..=1`, mapping NaN to 0.0 (so a bad automation value is harmless).
#[inline]
fn clamp01(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Replace a non-finite value with a fallback (NaN/inf hardening for setters).
#[inline]
fn guard(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

/// Final NaN/inf guard on an audio sample: any non-finite value becomes silence.
#[inline]
fn sanitize(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// Build a reverb with the contract's default plate settings and a non-zero
    /// send so the wet is actually audible in the output.
    fn default_reverb() -> Reverb {
        let mut rev = Reverb::new(SR);
        rev.set_params(
            true,
            ReverbAlgo::Plate224,
            2.5,   // decay
            0.6,   // size
            0.020, // predelay
            0.4,   // damping
            80.0,  // locut
            9_000.0, // hicut
            0.2,   // modulation
            1.0,   // width
            false, // freeze
        );
        rev.set_send(1.0, 1.0);
        rev
    }

    /// The Hadamard transform must be orthonormal: it preserves the L2 norm of
    /// the input vector (energy in == energy out). This is the property the whole
    /// stability argument rests on.
    #[test]
    fn hadamard_preserves_energy() {
        let inputs = [
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [1.0, -2.0, 3.0, -4.0, 5.0, -6.0, 7.0, -8.0],
            [0.3, 0.3, 0.3, 0.3, 0.3, 0.3, 0.3, 0.3],
        ];
        for v in inputs {
            let before: f32 = v.iter().map(|x| x * x).sum();
            let out = hadamard8(v);
            let after: f32 = out.iter().map(|x| x * x).sum();
            assert!(
                (before - after).abs() < 1.0e-4,
                "Hadamard must preserve energy: {before} vs {after}"
            );
        }
    }

    /// Across a full sweep of every parameter and a range of pathological inputs,
    /// the output must stay finite and bounded — never NaN, never runaway.
    #[test]
    fn output_finite_and_bounded_across_param_sweep() {
        let algos = [
            ReverbAlgo::Plate224,
            ReverbAlgo::Hall,
            ReverbAlgo::Spring,
            ReverbAlgo::Room,
        ];
        for algo in algos {
            for &decay in &[0.05f32, 0.5, 2.5, 12.0, 100.0] {
                for &size in &[0.0f32, 0.5, 1.0] {
                    for &damping in &[0.0f32, 0.5, 1.0] {
                        let mut rev = Reverb::new(SR);
                        rev.set_params(
                            true, algo, decay, size, 0.02, damping, 80.0, 9_000.0, 0.5, 1.0,
                            false,
                        );
                        rev.set_send(2.0, 2.0); // hot send to stress the level
                        let mut phase = 0.0f32;
                        for i in 0..8_000 {
                            // A mix of tone, impulses, and extreme spikes.
                            let drive = if i % 1000 == 0 { 1.0e6 } else { (phase * TAU).sin() };
                            phase = (phase + 220.0 / SR).fract();
                            let (l, r) = rev.process_send(drive, -drive);
                            assert!(
                                l.is_finite() && r.is_finite(),
                                "non-finite at algo={algo:?} decay={decay} size={size} damp={damping} i={i}: {l},{r}"
                            );
                            assert!(
                                l.abs() < 1.0e7 && r.abs() < 1.0e7,
                                "unbounded output at algo={algo:?} i={i}: {l},{r}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// `send == 0` must yield *exactly* the dry input (a true wet bypass), even
    /// while a tail is decaying internally — the seamless-swell guarantee.
    #[test]
    fn send_zero_passes_dry_untouched() {
        let mut rev = default_reverb();
        // Build up a tail first.
        for _ in 0..2_000 {
            rev.process_send(1.0, 1.0);
        }
        // Now drop the send to zero; output must equal the dry input bit-for-bit.
        rev.set_send(0.0, 1.0);
        for i in 0..1_000 {
            let dry_l = (i as f32 * 0.001).sin();
            let dry_r = (i as f32 * 0.002).cos();
            let (l, r) = rev.process_send(dry_l, dry_r);
            assert_eq!(l, dry_l, "send=0 must pass left dry untouched");
            assert_eq!(r, dry_r, "send=0 must pass right dry untouched");
        }
    }

    /// `on == false` is a true bypass: output is exactly the dry input.
    #[test]
    fn disabled_is_true_bypass() {
        let mut rev = default_reverb();
        rev.set_params(
            false,
            ReverbAlgo::Plate224,
            2.5,
            0.6,
            0.02,
            0.4,
            80.0,
            9_000.0,
            0.2,
            1.0,
            false,
        );
        for i in 0..500 {
            let dry = (i as f32 * 0.01).sin();
            let (l, r) = rev.process_send(dry, dry * 0.5);
            assert_eq!(l, dry);
            assert_eq!(r, dry * 0.5);
        }
    }

    /// Feeding an impulse and then silence must produce a wet tail that actually
    /// rings (non-trivial energy) and then decays toward zero over time.
    #[test]
    fn tail_rings_then_decays() {
        let mut rev = default_reverb();

        // Kick it with an impulse.
        let (l0, r0) = rev.process_send(1.0, 1.0);
        let _ = (l0, r0);

        // Measure wet energy in an early window vs a late window, feeding silence.
        let mut early = 0.0f32;
        let mut late = 0.0f32;
        for i in 0..SR as usize {
            // process_send adds dry(=0) to the wet, so the output IS the wet here.
            let (l, r) = rev.process_send(0.0, 0.0);
            let e = l * l + r * r;
            if i < 4_000 {
                early += e;
            } else if i >= (SR as usize - 4_000) {
                late += e;
            }
        }
        assert!(early > 1.0e-6, "tail should ring with real energy, got {early}");
        assert!(
            late < early * 0.5,
            "tail must decay: early energy {early}, late energy {late}"
        );
    }

    /// A longer decay (RT60) must sustain the tail longer than a short decay: the
    /// energy remaining after a fixed time should be greater for the long setting.
    #[test]
    fn longer_decay_sustains_longer() {
        fn remaining_energy(decay: f32) -> f32 {
            let mut rev = Reverb::new(SR);
            rev.set_params(
                true,
                ReverbAlgo::Plate224,
                decay,
                0.6,
                0.0,
                0.0, // no damping so we isolate the decay-time effect
                20.0,
                18_000.0,
                0.0, // no modulation for a clean comparison
                1.0,
                false,
            );
            rev.set_send(1.0, 1.0);
            rev.process_send(1.0, 1.0); // impulse
            let mut tail = 0.0f32;
            for i in 0..SR as usize {
                let (l, r) = rev.process_send(0.0, 0.0);
                if i >= SR as usize / 2 {
                    tail += l * l + r * r;
                }
            }
            tail
        }
        let short = remaining_energy(0.5);
        let long = remaining_energy(8.0);
        assert!(
            long > short * 4.0,
            "long decay should retain far more late energy: short={short}, long={long}"
        );
    }

    /// Freeze must sustain a tail essentially forever (it should not decay away),
    /// and the muted input means new dry energy does not keep building it up
    /// without bound.
    #[test]
    fn freeze_sustains_and_stays_bounded() {
        let mut rev = default_reverb();
        // Excite the network, then freeze.
        for _ in 0..4_000 {
            rev.process_send((rand_like() - 0.5) * 0.5, (rand_like() - 0.5) * 0.5);
        }
        rev.set_params(
            true,
            ReverbAlgo::Plate224,
            2.5,
            0.6,
            0.02,
            0.4,
            80.0,
            9_000.0,
            0.2,
            1.0,
            true, // freeze on
        );

        // Energy now should hold roughly steady, not decay to nothing.
        let mut first = 0.0f32;
        let mut last = 0.0f32;
        let mut peak = 0.0f32;
        for i in 0..(SR as usize * 4) {
            let (l, r) = rev.process_send(0.0, 0.0);
            let e = l * l + r * r;
            peak = peak.max(l.abs()).max(r.abs());
            if i < 4_000 {
                first += e;
            } else if i >= (SR as usize * 4 - 4_000) {
                last += e;
            }
        }
        assert!(first > 1.0e-6, "frozen tail should have energy, got {first}");
        // After 4 seconds a non-frozen 2.5 s RT60 tail would be ~−40 dB; frozen it
        // must still be a substantial fraction of where it started.
        assert!(
            last > first * 0.1,
            "freeze must sustain (not decay): first={first}, last={last}"
        );
        assert!(peak.is_finite() && peak < 100.0, "frozen tail unbounded: {peak}");
    }

    /// Modulation should change the tail: with modulation on, the output of an
    /// otherwise-identical impulse response must differ from modulation off (it
    /// choruses the lines), proving the anti-ring shimmer is actually moving.
    #[test]
    fn modulation_moves_the_tail() {
        fn tail(modulation: f32) -> Vec<f32> {
            let mut rev = Reverb::new(SR);
            rev.set_params(
                true,
                ReverbAlgo::Plate224,
                3.0,
                0.6,
                0.0,
                0.2,
                80.0,
                9_000.0,
                modulation,
                1.0,
                false,
            );
            rev.set_send(1.0, 1.0);
            rev.process_send(1.0, 1.0);
            let mut out = Vec::with_capacity(8_000);
            for _ in 0..8_000 {
                let (l, _) = rev.process_send(0.0, 0.0);
                out.push(l);
            }
            out
        }
        let dry_tail = tail(0.0);
        let mod_tail = tail(1.0);
        let diff: f32 = dry_tail
            .iter()
            .zip(&mod_tail)
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            diff > 0.01,
            "modulation should perceptibly alter the tail, total diff {diff}"
        );
    }

    /// Width = 0 should collapse the wet return toward mono (left ≈ right), while
    /// width = 1 leaves a genuine stereo difference. The dry is unaffected either
    /// way (it is added outside the width stage), so we test with send-only wet.
    #[test]
    fn width_controls_stereo_spread() {
        fn side_energy(width: f32) -> f32 {
            let mut rev = Reverb::new(SR);
            rev.set_params(
                true,
                ReverbAlgo::Plate224,
                3.0,
                0.6,
                0.0,
                0.2,
                80.0,
                9_000.0,
                0.0,
                width,
                false,
            );
            rev.set_send(1.0, 1.0);
            rev.process_send(1.0, -1.0);
            let mut side = 0.0f32;
            for _ in 0..8_000 {
                let (l, r) = rev.process_send(0.0, 0.0);
                let s = (l - r) * 0.5;
                side += s * s;
            }
            side
        }
        let mono = side_energy(0.0);
        let wide = side_energy(1.0);
        assert!(
            mono < wide * 0.25 + 1.0e-9,
            "width=0 should collapse the side channel: mono={mono}, wide={wide}"
        );
    }

    /// Higher damping must darken the tail: the late-tail high-frequency content
    /// should be lower with heavy damping than with none. We measure it crudely
    /// via the energy of the sample-to-sample difference (a high-pass proxy).
    #[test]
    fn damping_darkens_the_tail() {
        fn hf_energy(damping: f32) -> f32 {
            let mut rev = Reverb::new(SR);
            rev.set_params(
                true,
                ReverbAlgo::Plate224,
                4.0,
                0.6,
                0.0,
                damping,
                20.0,
                18_000.0, // hi-cut wide open so damping is the only HF control
                0.0,
                1.0,
                false,
            );
            rev.set_send(1.0, 1.0);
            // Excite with broadband noise so there is HF energy to absorb.
            for _ in 0..2_000 {
                rev.process_send(rand_like() - 0.5, rand_like() - 0.5);
            }
            let mut prev = 0.0f32;
            let mut hf = 0.0f32;
            for i in 0..(SR as usize) {
                let (l, _) = rev.process_send(0.0, 0.0);
                if i > SR as usize / 2 {
                    let d = l - prev;
                    hf += d * d;
                }
                prev = l;
            }
            hf
        }
        let bright = hf_energy(0.0);
        let dark = hf_energy(1.0);
        assert!(
            dark < bright,
            "more damping should reduce late high-frequency energy: bright={bright}, dark={dark}"
        );
    }

    /// Sample-rate changes must not allocate-or-panic and must keep the reverb
    /// finite. Also confirms clearing on rate change (no stale click).
    #[test]
    fn sample_rate_change_is_safe() {
        let mut rev = default_reverb();
        for _ in 0..1_000 {
            rev.process_send(0.3, -0.3);
        }
        for &sr in &[44_100.0, 96_000.0, 192_000.0, 22_050.0] {
            rev.set_sample_rate(sr);
            for _ in 0..1_000 {
                let (l, r) = rev.process_send(0.5, 0.5);
                assert!(l.is_finite() && r.is_finite(), "non-finite after SR change to {sr}");
            }
        }
    }

    /// Pathological setter inputs (NaN, infinities, negatives, zero decay) must be
    /// clamped so the reverb still produces finite audio.
    #[test]
    fn setters_are_hardened_against_bad_values() {
        let mut rev = Reverb::new(SR);
        rev.set_params(
            true,
            ReverbAlgo::Hall,
            f32::NAN,       // decay
            f32::INFINITY,  // size
            -1.0,           // predelay
            f32::NAN,       // damping
            -100.0,         // locut
            1.0e12,         // hicut
            f32::INFINITY,  // modulation
            -5.0,           // width
            false,
        );
        rev.set_send(f32::NAN, f32::INFINITY);
        for _ in 0..2_000 {
            let (l, r) = rev.process_send(1.0, -1.0);
            assert!(l.is_finite() && r.is_finite(), "bad params produced non-finite output");
        }
    }

    /// Feedback gains must all be strictly below 1.0 (the stability requirement)
    /// for every reasonable decay, and exactly the freeze ceiling when frozen.
    #[test]
    fn feedback_gains_are_stable() {
        let mut rev = Reverb::new(SR);
        for &decay in &[0.1f32, 1.0, 10.0, 60.0] {
            rev.set_params(
                true, ReverbAlgo::Hall, decay, 0.5, 0.02, 0.3, 80.0, 9_000.0, 0.2, 1.0, false,
            );
            for g in rev.feedback_gains() {
                assert!(g >= 0.0 && g < 1.0, "feedback gain out of [0,1): {g}");
            }
        }
        // Frozen: gains pinned to the (sub-unity) freeze ceiling.
        rev.set_params(
            true, ReverbAlgo::Hall, 2.5, 0.5, 0.02, 0.3, 80.0, 9_000.0, 0.2, 1.0, true,
        );
        for g in rev.feedback_gains() {
            assert!(g <= MAX_FEEDBACK && g > 0.99, "frozen gain wrong: {g}");
        }
    }

    /// REGRESSION: a single non-finite sample injected into the loop must NOT
    /// latch. Before the flush, `f32::NAN.clamp(-32, 32)` left NaN in the FDN ring,
    /// the Hadamard mix spread it to all 8 lines, and the tail stayed dead forever.
    /// After the fix the reverb must return to finite output within a short window.
    #[test]
    fn recovers_from_injected_nan_and_inf() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut rev = default_reverb();
            // Warm up a normal tail.
            let mut phase = 0.0f32;
            for _ in 0..2_000 {
                let x = (phase * TAU).sin();
                phase = (phase + 220.0 / SR).fract();
                let _ = rev.process_send(x, x);
            }
            // Inject the poison sample into the loop.
            let _ = rev.process_send(bad, bad);

            // Feed silence and confirm we are finite again within a short window.
            // The injected sample has to flush out of the lines; allow a few
            // buffer-length's worth of samples.
            let mut recovered_at = None;
            for i in 0..20_000 {
                let (l, r) = rev.process_send(0.0, 0.0);
                if l.is_finite() && r.is_finite() {
                    if recovered_at.is_none() {
                        recovered_at = Some(i);
                    }
                } else {
                    // A later non-finite means it re-poisoned — fail.
                    recovered_at = None;
                }
            }
            assert!(
                recovered_at.is_some(),
                "reverb never recovered to finite output after injecting {bad}"
            );
            // And the steady state must be exactly finite (not just non-NaN).
            for _ in 0..1_000 {
                let (l, r) = rev.process_send(0.0, 0.0);
                assert!(l.is_finite() && r.is_finite(), "reverb re-poisoned at steady state");
            }
        }
    }

    /// After excitation then SILENCE, the tail must decay to *exactly* 0.0 (no
    /// lingering denormal trickling through the feedback loop forever). The
    /// `flush_denormal` on the line/damp writes is what guarantees this.
    #[test]
    fn tail_decays_to_exactly_zero_on_silence() {
        // A short RT60 so the exponential tail crosses the denormal-flush floor
        // (1e-25, ~−500 dB) within the test window. The point of this test is that
        // once the tail drops below that floor it snaps to *exactly* 0.0 rather
        // than trickling as a denormal forever — which is the `flush_denormal`
        // guard on the FDN line/damp writes doing its job.
        let mut rev = Reverb::new(SR);
        rev.set_params(
            true,
            ReverbAlgo::Room,
            0.25,    // short decay
            0.3,     // small size
            0.0,     // no predelay
            0.8,     // heavy damping speeds the high-frequency die-off
            80.0,    // locut
            9_000.0, // hicut
            0.0,     // no modulation
            1.0,     // width
            false,   // freeze
        );
        rev.set_send(1.0, 1.0);

        // Excite with an impulse + a short tone burst.
        let _ = rev.process_send(1.0, 1.0);
        let mut phase = 0.0f32;
        for _ in 0..500 {
            let x = (phase * TAU).sin();
            phase = (phase + 330.0 / SR).fract();
            let _ = rev.process_send(x, x);
        }
        // Feed pure silence long enough for the short tail to cross the flush
        // floor, then assert bit-exact zero (no lingering denormal, no NaN).
        let mut last = (1.0f32, 1.0f32);
        for _ in 0..(SR as usize * 10) {
            last = rev.process_send(0.0, 0.0);
        }
        assert_eq!(last.0, 0.0, "reverb left tail did not reach exactly 0: {}", last.0);
        assert_eq!(last.1, 0.0, "reverb right tail did not reach exactly 0: {}", last.1);
    }

    /// A tiny, fast, dependency-free pseudo-random source for noise excitation in
    /// tests (we are std-only, so no `rand`). A simple xorshift over a
    /// thread-local cell — safe, no `unsafe`, deterministic enough for a proxy.
    fn rand_like() -> f32 {
        use std::cell::Cell;
        thread_local! {
            static STATE: Cell<u32> = const { Cell::new(0x1234_5678) };
        }
        STATE.with(|s| {
            let mut x = s.get();
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            s.set(x);
            (x as f32 / u32::MAX as f32).clamp(0.0, 1.0)
        })
    }
}
