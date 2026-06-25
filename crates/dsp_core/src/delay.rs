//! A stereo delay with two engines behind one struct: a vintage **TAPE /
//! Space-Echo** emulation and a clean **tempo-synced stereo** delay.
//!
//! Both engines share the same two pre-allocated ring buffers (one per channel)
//! and the same wet/dry mixer; the [`DelayMode`] param chooses which DSP runs.
//! The other engine is simply not evaluated, so switching modes is cheap and
//! never allocates.
//!
//! ## The two engines, in one paragraph each
//!
//! **Clean** is a textbook stereo delay: each channel has an *independent* tap
//! time, so you can offset left and right (or sync them to different note
//! divisions for that dotted-eighth / triplet interplay). `feedback` routes a
//! tap back into *its own* channel; `crossfeed` routes it into the *opposite*
//! channel, which is what turns two mono delays into a ping-pong that bounces
//! across the stereo field. A one-pole high-pass and one-pole low-pass sit in
//! the feedback loop so each repeat gets a little darker and thinner — the
//! classic "the echoes fade into the distance" colour without muddying the
//! first hit.
//!
//! **Tape** models a Roland Space-Echo-style tape loop. A real tape echo never
//! plays back at exactly the recorded speed: the motor and capstan introduce
//! slow *wow* (~0.6 Hz) and faster *flutter* (~6 Hz) that smear the pitch. We
//! reproduce that by wobbling the *read position* with two summed LFOs — moving
//! the read head is mathematically the same as varying playback speed, so you
//! get the seasick pitch warble that makes tape delays sound alive. The
//! feedback path also runs through a `tanh` saturator and a low-pass whose
//! darkness grows with `age`, so each generation of the echo gets warmer, more
//! compressed, and duller — exactly how a worn tape degrades. Because the
//! saturator bounds the loop, `feedback` is allowed to exceed 1.0 here: the echo
//! self-oscillates into a swelling drone instead of blowing up.
//!
//! ## Real-time safety
//!
//! The ring buffers are sized once in [`Delay::new`] for the longest delay we
//! support (2 seconds) at the highest sample rate we expect, and never grow.
//! [`Delay::process`] only reads, writes, and does float math — no allocation,
//! no locks. [`Delay::set_sample_rate`] is the one place buffers are touched
//! structurally, and it only *clears* (zero-fills) the existing storage; it
//! never reallocates on the audio thread's behalf.

use std::f32::consts::TAU;

/// Longest delay time we let either engine address, in seconds. Both the param
/// range (`delay_time` tops out at 2.0 s) and the tempo-synced divisions stay
/// under this, so a tap can never ask for a position older than the buffer
/// holds.
const MAX_DELAY_SECS: f32 = 2.0;

/// Highest sample rate we pre-allocate for. Sizing the ring at this rate means
/// a later `set_sample_rate` to anything `<=` this only ever *clears* the
/// buffer — it never needs to grow, so the audio thread never allocates.
const MAX_SAMPLE_RATE: f32 = 192_000.0;

/// Which delay engine is active. The unused engine's DSP is skipped entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelayMode {
    /// Roland Space-Echo-style modulated tape loop: wow/flutter pitch wobble,
    /// feedback-path saturation and high-frequency rolloff that ages each
    /// repeat. Self-oscillates safely at high feedback.
    Tape,
    /// Clean tempo-synced stereo delay with independent L/R tap times,
    /// per-channel feedback, ping-pong crossfeed, and a tone-shaping HP/LP in
    /// the feedback loop.
    Clean,
}

/// Stereo delay. Construct with [`Delay::new`], feed it engineering-unit
/// settings via [`Delay::set_params`] / [`Delay::set_sync`] at block rate, then
/// call [`Delay::process`] once per sample.
#[derive(Clone, Debug)]
pub struct Delay {
    sample_rate: f32,
    tempo_bpm: f32,

    /// Per-channel ring buffers, allocated once. We index them with
    /// `rem_euclid` (not a power-of-two mask) because the buffer length depends
    /// on the runtime sample rate, so it is not a clean power of two.
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,

    // --- block-rate parameters (already in engineering units) ---
    on: bool,
    mode: DelayMode,
    /// Left/right tap times in *seconds*. In Clean mode these are the two
    /// independent taps; in Tape mode only `time_l` is used (single head).
    time_l: f32,
    time_r: f32,
    feedback: f32,
    crossfeed: f32,
    wow: f32,
    age: f32,
    /// Feedback-path filter cutoffs (Clean mode HP/LP).
    hpf_hz: f32,
    lpf_hz: f32,
    mix: f32,

    // --- per-channel filter states (one-pole) ---
    /// Low-pass state: the running output of the one-pole LP.
    lp_l: f32,
    lp_r: f32,
    /// High-pass state: we build a HP as `x - lowpass(x)`, so we track the LP
    /// half here and subtract.
    hp_lp_l: f32,
    hp_lp_r: f32,

    // --- tape modulation state ---
    /// Slow "wow" LFO phase in `0.0..1.0` (~0.6 Hz).
    wow_phase: f32,
    /// Faster "flutter" LFO phase in `0.0..1.0` (~6 Hz).
    flutter_phase: f32,
}

impl Delay {
    /// Allocate the ring buffers (sized for [`MAX_DELAY_SECS`] at
    /// [`MAX_SAMPLE_RATE`]) and seed sane defaults. This is the only place that
    /// allocates.
    pub fn new(sample_rate: f32) -> Self {
        // +2 samples of headroom so linear interpolation reading the oldest
        // sample can always look one neighbour further back without wrapping
        // past the write head.
        let max_len = (MAX_DELAY_SECS * MAX_SAMPLE_RATE).ceil() as usize + 2;
        Self {
            sample_rate: sample_rate.max(1.0),
            tempo_bpm: 120.0,
            buf_l: vec![0.0; max_len],
            buf_r: vec![0.0; max_len],
            write_pos: 0,

            on: false,
            mode: DelayMode::Tape,
            time_l: 0.25,
            time_r: 0.25,
            feedback: 0.30,
            crossfeed: 0.0,
            wow: 0.25,
            age: 0.40,
            hpf_hz: 100.0,
            lpf_hz: 12_000.0,
            mix: 0.30,

            lp_l: 0.0,
            lp_r: 0.0,
            hp_lp_l: 0.0,
            hp_lp_r: 0.0,

            wow_phase: 0.0,
            flutter_phase: 0.0,
        }
    }

    /// Update the sample rate and flush all state. We clear (but never
    /// reallocate) the ring buffers so a rate change doesn't replay stale audio
    /// at the wrong pitch, and reset the filter and LFO states so there's no
    /// click or lingering DC.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.clear();
    }

    /// Zero the ring buffers and all filter/LFO state. Pure float writes over
    /// already-allocated storage — RT-safe. Public so the plugin can flush the
    /// delay tail on a scene/world switch (so stale or over-fed repeats don't
    /// bleed into the new world).
    pub fn clear(&mut self) {
        for s in &mut self.buf_l {
            *s = 0.0;
        }
        for s in &mut self.buf_r {
            *s = 0.0;
        }
        self.write_pos = 0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.hp_lp_l = 0.0;
        self.hp_lp_r = 0.0;
        self.wow_phase = 0.0;
        self.flutter_phase = 0.0;
    }

    /// Host tempo, used only if the plugin chooses *not* to resolve sync itself.
    /// In practice the plugin resolves division+tempo to seconds and calls
    /// [`Delay::set_sync`]; we keep `set_tempo` to mirror the arpeggiator's API
    /// (and so the module is self-sufficient for tests).
    pub fn set_tempo(&mut self, bpm: f32) {
        self.tempo_bpm = bpm.clamp(1.0, 1000.0);
    }

    /// Set the already-mapped engineering-unit parameters. Everything is clamped
    /// so out-of-range automation can never NaN the delay or address past the
    /// buffer.
    ///
    /// - `time_l_secs`, `time_r_secs`: tap times, seconds. Clean uses both;
    ///   Tape uses `time_l` as its single head position.
    /// - `feedback`: regeneration. `0..1.1`; values `> 1.0` are only meaningful
    ///   in Tape mode (saturation keeps it bounded) and are clamped to `< 1.0`
    ///   in Clean mode for stability.
    /// - `crossfeed`: `0..1`, ping-pong amount into the opposite channel.
    /// - `wow`: `0..1`, tape wow/flutter depth (Tape only).
    /// - `age`: `0..1`, tape wear — more feedback-path darkening + saturation.
    /// - `hpf_hz`, `lpf_hz`: feedback-loop high-/low-pass cutoffs.
    /// - `mix`: `0..1` wet/dry blend.
    #[allow(clippy::too_many_arguments)]
    pub fn set_params(
        &mut self,
        on: bool,
        mode: DelayMode,
        time_l_secs: f32,
        time_r_secs: f32,
        feedback: f32,
        crossfeed: f32,
        wow: f32,
        age: f32,
        hpf_hz: f32,
        lpf_hz: f32,
        mix: f32,
    ) {
        self.on = on;
        self.mode = mode;
        // Keep a small floor and a hard ceiling under the buffer length so the
        // interpolated read can never run off the end.
        let max_secs = self.max_delay_secs();
        self.time_l = sanitize(time_l_secs, 0.25).clamp(0.0005, max_secs);
        self.time_r = sanitize(time_r_secs, 0.25).clamp(0.0005, max_secs);
        self.feedback = sanitize(feedback, 0.0).clamp(0.0, 1.1);
        self.crossfeed = sanitize(crossfeed, 0.0).clamp(0.0, 1.0);
        self.wow = sanitize(wow, 0.0).clamp(0.0, 1.0);
        self.age = sanitize(age, 0.0).clamp(0.0, 1.0);
        // Keep cutoffs inside `(0, Nyquist)` so the one-pole coefficients stay
        // in `(0, 1)`.
        let nyquist = self.sample_rate * 0.5;
        self.hpf_hz = sanitize(hpf_hz, 100.0).clamp(10.0, nyquist * 0.99);
        self.lpf_hz = sanitize(lpf_hz, 12_000.0).clamp(20.0, nyquist * 0.99);
        self.mix = sanitize(mix, 0.0).clamp(0.0, 1.0);
    }

    /// Tempo-synced convenience: the plugin resolves a note division + host
    /// tempo to seconds and hands us the left/right tap times directly. This is
    /// just `set` for `time_l`/`time_r` with the same clamping as
    /// [`Delay::set_params`], so sync and free-time share one read path.
    pub fn set_sync(&mut self, div_secs_l: f32, div_secs_r: f32) {
        let max_secs = self.max_delay_secs();
        self.time_l = sanitize(div_secs_l, 0.25).clamp(0.0005, max_secs);
        self.time_r = sanitize(div_secs_r, 0.25).clamp(0.0005, max_secs);
    }

    /// The longest delay the *current* buffer can address, leaving 2 samples of
    /// interpolation headroom. Always `<= MAX_DELAY_SECS`.
    fn max_delay_secs(&self) -> f32 {
        let usable = (self.buf_l.len().saturating_sub(2)) as f32;
        (usable / self.sample_rate).min(MAX_DELAY_SECS)
    }

    /// Process one stereo sample. Returns the wet/dry-mixed `(left, right)`.
    ///
    /// When the delay is bypassed (`on == false`) we pass the input straight
    /// through *and* keep clearing the write head so the ring doesn't hold a
    /// stale tail to burst out when re-enabled.
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        if !self.on {
            // Decay the buffer toward silence so re-enabling is clean, but don't
            // pay for the full DSP.
            self.buf_l[self.write_pos] = 0.0;
            self.buf_r[self.write_pos] = 0.0;
            self.advance();
            return (l, r);
        }

        let (wet_l, wet_r) = match self.mode {
            DelayMode::Clean => self.process_clean(l, r),
            DelayMode::Tape => self.process_tape(l, r),
        };

        self.advance();

        let dry = 1.0 - self.mix;
        (dry * l + self.mix * wet_l, dry * r + self.mix * wet_r)
    }

    /// Advance the shared write head by one sample, wrapping at the buffer end.
    #[inline]
    fn advance(&mut self) {
        self.write_pos += 1;
        if self.write_pos >= self.buf_l.len() {
            self.write_pos = 0;
        }
    }

    // --- Clean stereo engine ---------------------------------------------

    /// Independent L/R taps, per-channel feedback + ping-pong crossfeed, with a
    /// HP/LP tone shaper in the feedback loop.
    fn process_clean(&mut self, l: f32, r: f32) -> (f32, f32) {
        // Read each channel's tap *before* writing this sample so the minimum
        // delay is a full tap, not zero.
        let wet_l = read_interp(&self.buf_l, self.write_pos, self.time_l * self.sample_rate);
        let wet_r = read_interp(&self.buf_r, self.write_pos, self.time_r * self.sample_rate);

        // Tone-shape the regenerated signal: low-pass then high-pass so repeats
        // lose both the sparkle and the rumble, leaving a focused midrange that
        // sits behind the dry hit.
        let lp_coeff = one_pole_lp_coeff(self.lpf_hz, self.sample_rate);
        let hp_coeff = one_pole_lp_coeff(self.hpf_hz, self.sample_rate);

        let shaped_l = self.feedback_tone(wet_l, false, lp_coeff, hp_coeff);
        let shaped_r = self.feedback_tone(wet_r, true, lp_coeff, hp_coeff);

        // Clamp feedback strictly below unity in Clean mode — there is no
        // saturator to catch runaway regeneration here.
        let fb = self.feedback.min(0.98);
        let xf = self.crossfeed;

        // Each channel writes: its dry input + its own feedback + the *other*
        // channel's shaped feedback (the ping-pong path). We scale the combined
        // regeneration so feedback + crossfeed together still can't exceed unity.
        let norm = 1.0 / (1.0 + xf);
        let into_l = l + fb * (shaped_l + xf * shaped_r) * norm;
        let into_r = r + fb * (shaped_r + xf * shaped_l) * norm;

        self.buf_l[self.write_pos] = flush_denormal(into_l);
        self.buf_r[self.write_pos] = flush_denormal(into_r);

        (wet_l, wet_r)
    }

    /// Run one channel of the Clean feedback signal through LP then HP. `right`
    /// selects which channel's filter state to use.
    #[inline]
    fn feedback_tone(&mut self, x: f32, right: bool, lp_coeff: f32, hp_coeff: f32) -> f32 {
        if right {
            // One-pole low-pass.
            self.lp_r += lp_coeff * (x - self.lp_r);
            let lp = self.lp_r;
            // One-pole high-pass = signal minus its own low-passed version.
            self.hp_lp_r += hp_coeff * (lp - self.hp_lp_r);
            lp - self.hp_lp_r
        } else {
            self.lp_l += lp_coeff * (x - self.lp_l);
            let lp = self.lp_l;
            self.hp_lp_l += hp_coeff * (lp - self.hp_lp_l);
            lp - self.hp_lp_l
        }
    }

    // --- Tape / Space-Echo engine ----------------------------------------

    /// Single modulated tape head. Wow + flutter wobble the read position
    /// (pitch warble); the feedback path saturates and darkens with `age`.
    fn process_tape(&mut self, l: f32, r: f32) -> (f32, f32) {
        // Tape delays are mono down the loop; sum to mono for the head, keep the
        // stereo image only in the wet read offset.
        let input = 0.5 * (l + r);

        // --- modulation: slow wow + faster flutter, both depth-scaled by `wow`.
        // Flutter is shallower than wow and runs ~10x faster, matching how a
        // capstan's fast jitter is smaller than the slow motor drift.
        let wow_lfo = (self.wow_phase * TAU).sin();
        let flutter_lfo = (self.flutter_phase * TAU).sin();
        self.wow_phase = wrap01(self.wow_phase + 0.6 / self.sample_rate);
        self.flutter_phase = wrap01(self.flutter_phase + 6.0 / self.sample_rate);

        // Depth in *samples*: at full `wow` the head drifts up to ~3 ms for wow
        // and ~0.8 ms for flutter — audible warble without sounding broken.
        let wow_depth = self.wow * 0.003 * self.sample_rate;
        let flutter_depth = self.wow * 0.0008 * self.sample_rate;
        let base = self.time_l * self.sample_rate;
        // Keep the modulated delay strictly positive so the read never crosses
        // the write head.
        let mod_delay = (base + wow_lfo * wow_depth + flutter_lfo * flutter_depth).max(1.0);

        // Read both channels at the same modulated position (mono loop), but
        // give the right side a tiny extra offset so the wet output keeps a hint
        // of stereo width.
        let wet_l = read_interp(&self.buf_l, self.write_pos, mod_delay);
        let wet_r = read_interp(&self.buf_r, self.write_pos, mod_delay * 1.0007 + 1.0);

        // Average the two reads into the single tape loop signal.
        let looped = 0.5 * (wet_l + wet_r);

        // --- ageing: older tape rolls off highs harder and saturates sooner.
        // Map `age` to a feedback-path low-pass that closes from ~12 kHz (fresh)
        // down to ~2.5 kHz (worn).
        let hi_cut_hz = 12_000.0 - self.age * 9_500.0;
        let lp_coeff = one_pole_lp_coeff(hi_cut_hz, self.sample_rate);
        // Reuse the left LP state as the single mono tape low-pass.
        self.lp_l += lp_coeff * (looped - self.lp_l);
        let darkened = self.lp_l;

        // Gentle `tanh` tape saturation, getting harder with age. Slope is
        // compensated out so quiet repeats stay at unity (only loud peaks
        // compress), which is what bounds self-oscillation when feedback > 1.
        let sat_drive = 1.0 + self.age * 2.5;
        let saturated = (darkened * sat_drive).tanh() / sat_drive;

        // Feed back into the loop. Tape mode allows feedback up to 1.1 — the
        // saturator above stops it from exploding and instead lets it bloom into
        // a self-oscillating drone.
        let regen = self.feedback * saturated;
        let into = flush_denormal(input + regen);
        self.buf_l[self.write_pos] = into;
        self.buf_r[self.write_pos] = into;

        (wet_l, wet_r)
    }
}

/// Read `delay_samples` in the past from `buf`, with linear interpolation
/// between the two nearest samples for a smooth, click-free fractional delay.
/// The read index wraps with `rem_euclid` because the buffer length tracks the
/// runtime sample rate and isn't a clean power of two.
#[inline]
fn read_interp(buf: &[f32], write_pos: usize, delay_samples: f32) -> f32 {
    let len = buf.len();
    let len_f = len as f32;
    // Clamp so even an absurd delay request stays a valid, in-bounds read.
    let d = delay_samples.clamp(1.0, len_f - 2.0);
    let read = (write_pos as f32 - d).rem_euclid(len_f);
    let base = read.floor();
    let i0 = base as usize % len;
    let i1 = (i0 + 1) % len;
    let frac = read - base;
    let a = buf[i0];
    let b = buf[i1];
    a + (b - a) * frac
}

/// One-pole low-pass coefficient for a given cutoff. Derived from the standard
/// `1 - exp(-2*pi*fc/fs)` mapping and clamped to `(0, 1)` so the filter is
/// always stable, even if a caller hands in a silly cutoff.
#[inline]
fn one_pole_lp_coeff(cutoff_hz: f32, sample_rate: f32) -> f32 {
    let fc = cutoff_hz.clamp(1.0, sample_rate * 0.5 - 1.0);
    let c = 1.0 - (-TAU * fc / sample_rate).exp();
    c.clamp(0.0001, 1.0)
}

/// Wrap an LFO phase back into `0.0..1.0`.
#[inline]
fn wrap01(phase: f32) -> f32 {
    let p = phase - phase.floor();
    if p.is_finite() {
        p
    } else {
        0.0
    }
}

/// Replace a NaN/inf with a fallback, otherwise pass through. Used to keep bad
/// automation values from poisoning state.
#[inline]
fn sanitize(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

// Denormal/NaN flushing for the feedback buffers lives in the shared
// `crate::util` module so every effect uses the identical guard.
use crate::util::flush_denormal;

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the delay with an impulse and report the sample index of the first
    /// non-trivial wet output. Used to confirm an echo actually lands at the
    /// expected delay time.
    fn first_echo_index(delay: &mut Delay, frames: usize) -> Option<usize> {
        let mut first = None;
        for n in 0..frames {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let (l, _r) = delay.process(x, x);
            // Skip the dry passthrough at n==0; look for the delayed copy.
            if n > 0 && l.abs() > 0.05 && first.is_none() {
                first = Some(n);
            }
        }
        first
    }

    #[test]
    fn clean_produces_a_delayed_echo_at_the_tap_time() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        // 100 ms tap, audible mix, some feedback.
        d.set_params(
            true,
            DelayMode::Clean,
            0.100,
            0.100,
            0.4,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        let idx = first_echo_index(&mut d, 48_000).expect("expected an echo");
        let expected = (0.100 * sr) as usize;
        // Allow a small window around the tap for filter group delay.
        let lo = expected.saturating_sub(50);
        let hi = expected + 200;
        assert!((lo..hi).contains(&idx), "echo at {idx}, expected ~{expected}");
    }

    #[test]
    fn clean_independent_lr_taps_differ() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        // Left 80 ms, right 160 ms — the right echo should arrive later.
        d.set_params(
            true,
            DelayMode::Clean,
            0.080,
            0.160,
            0.0,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        let mut first_l = None;
        let mut first_r = None;
        for n in 0..48_000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let (l, r) = d.process(x, x);
            if n > 0 && l.abs() > 0.05 && first_l.is_none() {
                first_l = Some(n);
            }
            if n > 0 && r.abs() > 0.05 && first_r.is_none() {
                first_r = Some(n);
            }
        }
        let fl = first_l.expect("left echo");
        let fr = first_r.expect("right echo");
        assert!(fr > fl, "right tap ({fr}) should be later than left ({fl})");
    }

    #[test]
    fn crossfeed_pingpongs_into_the_opposite_channel() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        // Feed a left-only impulse with full crossfeed and feedback; energy
        // should appear on the right channel via the ping-pong path.
        d.set_params(
            true,
            DelayMode::Clean,
            0.050,
            0.050,
            0.6,
            1.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        let mut right_energy = 0.0f32;
        for n in 0..24_000 {
            let xl = if n == 0 { 1.0 } else { 0.0 };
            let (_l, r) = d.process(xl, 0.0);
            right_energy += r.abs();
        }
        assert!(
            right_energy > 0.1,
            "crossfeed should put energy on the right, got {right_energy}"
        );
    }

    #[test]
    fn tape_mode_warbles_the_delay_time() {
        let sr = 48_000.0;
        // A steady tone through full-wow Tape should differ measurably from the
        // same tone through a zero-wow Tape run, because the read head wobbles.
        let mut wobble = Delay::new(sr);
        wobble.set_params(
            true,
            DelayMode::Tape,
            0.200,
            0.200,
            0.5,
            0.0,
            1.0, // full wow
            0.4,
            100.0,
            12_000.0,
            1.0,
        );

        let mut still = Delay::new(sr);
        still.set_params(
            true,
            DelayMode::Tape,
            0.200,
            0.200,
            0.5,
            0.0,
            0.0, // no wow
            0.4,
            100.0,
            12_000.0,
            1.0,
        );

        let mut max_diff = 0.0f32;
        let mut phase = 0.0f32;
        for _ in 0..48_000 {
            let x = (phase * TAU).sin();
            phase = (phase + 440.0 / sr).fract();
            let (wl, _) = wobble.process(x, x);
            let (sl, _) = still.process(x, x);
            max_diff = max_diff.max((wl - sl).abs());
        }
        assert!(
            max_diff > 0.01,
            "wow should move the tape read vs the un-modulated run, diff {max_diff}"
        );
    }

    #[test]
    fn tape_self_oscillation_stays_bounded() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        // Feedback above unity (1.1) with heavy age: the tanh saturator must
        // keep the loop bounded rather than blowing up.
        d.set_params(
            true,
            DelayMode::Tape,
            0.150,
            0.150,
            1.1,
            0.0,
            0.3,
            0.9,
            100.0,
            12_000.0,
            1.0,
        );
        let mut peak = 0.0f32;
        let mut phase = 0.0f32;
        // Run 5 seconds — plenty for runaway feedback to explode if it could.
        for n in 0..(5 * sr as usize) {
            // Excite for the first 0.5 s, then let it self-oscillate.
            let x = if n < sr as usize / 2 {
                (phase * TAU).sin() * 0.8
            } else {
                0.0
            };
            phase = (phase + 110.0 / sr).fract();
            let (l, r) = d.process(x, x);
            assert!(l.is_finite() && r.is_finite(), "NaN/inf at sample {n}");
            peak = peak.max(l.abs()).max(r.abs());
        }
        // The saturator clamps the loop; output should stay in a sane range.
        assert!(peak < 4.0, "self-oscillation should stay bounded, peak {peak}");
    }

    #[test]
    fn finite_and_bounded_across_param_sweeps() {
        let sr = 44_100.0;
        let mut d = Delay::new(sr);
        d.set_tempo(128.0);
        let modes = [DelayMode::Tape, DelayMode::Clean];
        let mut phase = 0.0f32;
        let mut counter = 0usize;
        for &mode in &modes {
            // Sweep every param across its range while feeding a tone.
            for step in 0..40 {
                let t = step as f32 / 39.0;
                d.set_params(
                    true,
                    mode,
                    0.001 + t * 1.9,         // time_l 1ms..1.9s
                    0.001 + (1.0 - t) * 1.9, // time_r counter-sweep
                    t * 1.1,                 // feedback up to 1.1
                    t,                       // crossfeed
                    t,                       // wow
                    t,                       // age
                    20.0 + t * 1900.0,       // hpf
                    1000.0 + t * 18000.0,    // lpf
                    t,                       // mix
                );
                for _ in 0..2_000 {
                    // Occasionally hit it with a loud transient.
                    let spike = if counter % 997 == 0 { 4.0 } else { 0.0 };
                    let x = (phase * TAU).sin() * 0.7 + spike;
                    phase = (phase + 330.0 / sr).fract();
                    let (l, r) = d.process(x, x);
                    assert!(l.is_finite() && r.is_finite(), "NaN/inf in sweep");
                    assert!(
                        l.abs() < 50.0 && r.abs() < 50.0,
                        "runaway output {l},{r} (mode {mode:?}, t {t})"
                    );
                    counter += 1;
                }
            }
        }
    }

    #[test]
    fn bypass_passes_dry_and_does_not_burst_on_reenable() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        // Run with feedback, then bypass, then re-enable: there must be no loud
        // stale tail when we turn it back on.
        d.set_params(
            true,
            DelayMode::Clean,
            0.050,
            0.050,
            0.8,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        for n in 0..10_000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            d.process(x, x);
        }
        // Bypass: input must pass through unchanged.
        d.set_params(
            false,
            DelayMode::Clean,
            0.050,
            0.050,
            0.8,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        for _ in 0..10_000 {
            let (l, r) = d.process(0.3, -0.2);
            assert!(
                (l - 0.3).abs() < 1e-6 && (r + 0.2).abs() < 1e-6,
                "bypass not transparent"
            );
        }
        // Re-enable: the first samples should not contain a loud buried tail.
        d.set_params(
            true,
            DelayMode::Clean,
            0.050,
            0.050,
            0.8,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        for _ in 0..100 {
            let (l, r) = d.process(0.0, 0.0);
            assert!(
                l.abs() < 0.01 && r.abs() < 0.01,
                "stale tail burst on re-enable: {l},{r}"
            );
        }
    }

    #[test]
    fn set_sample_rate_clears_without_panicking() {
        let mut d = Delay::new(48_000.0);
        d.set_params(
            true,
            DelayMode::Tape,
            0.3,
            0.3,
            0.6,
            0.2,
            0.5,
            0.5,
            100.0,
            10_000.0,
            0.5,
        );
        for _ in 0..1000 {
            d.process(0.5, 0.5);
        }
        // Switch rate; buffers clear, no realloc, no stale audio.
        d.set_sample_rate(96_000.0);
        let (l, r) = d.process(0.0, 0.0);
        assert_eq!((l, r), (0.0, 0.0), "buffer should be silent after clear");
    }

    #[test]
    fn dry_when_mix_is_zero() {
        let mut d = Delay::new(48_000.0);
        d.set_params(
            true,
            DelayMode::Clean,
            0.1,
            0.1,
            0.5,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            0.0, // mix = 0 -> fully dry
        );
        for _ in 0..5000 {
            let (l, r) = d.process(0.4, -0.3);
            assert!(
                (l - 0.4).abs() < 1e-6 && (r + 0.3).abs() < 1e-6,
                "mix=0 must be dry"
            );
        }
    }

    #[test]
    fn set_sync_resolves_tap_times() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        d.set_params(
            true,
            DelayMode::Clean,
            0.25,
            0.25,
            0.2,
            0.0,
            0.0,
            0.0,
            20.0,
            18_000.0,
            1.0,
        );
        // Plugin resolved a dotted-eighth (L) vs eighth-triplet (R) division.
        d.set_sync(0.1875, 0.0833);
        let mut first_l = None;
        let mut first_r = None;
        for n in 0..48_000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let (l, r) = d.process(x, x);
            if n > 0 && l.abs() > 0.05 && first_l.is_none() {
                first_l = Some(n);
            }
            if n > 0 && r.abs() > 0.05 && first_r.is_none() {
                first_r = Some(n);
            }
        }
        let fl = first_l.expect("left echo");
        let fr = first_r.expect("right echo");
        // The right (triplet, shorter) tap should fire before the left (dotted).
        assert!(fr < fl, "triplet R ({fr}) should precede dotted L ({fl})");
    }

    #[test]
    fn tape_tail_decays_below_unity_feedback() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        d.set_params(
            true,
            DelayMode::Tape,
            0.120,
            0.120,
            0.5, // < 1.0 so the tail must fade
            0.0,
            0.0,
            0.3,
            100.0,
            12_000.0,
            1.0,
        );
        // Impulse, then run long enough for many repeats.
        for n in 0..(4 * sr as usize) {
            let x = if n == 0 { 1.0 } else { 0.0 };
            d.process(x, x);
        }
        // After 4 s the tail should be essentially gone.
        let mut tail = 0.0f32;
        for _ in 0..sr as usize {
            let (l, r) = d.process(0.0, 0.0);
            tail = tail.max(l.abs()).max(r.abs());
        }
        assert!(tail < 1e-3, "tape tail should decay to silence, residual {tail}");
    }
}

