//! A simple linear ADSR envelope generator.
//!
//! Linear segments are not the analog-curve shapes we ultimately want (those
//! come with the mod-matrix work in M4), but they are easy to reason about and
//! easy to test: attack ramps to 1.0, decay falls to `sustain`, release falls
//! back to 0.0.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Debug)]
pub struct Adsr {
    sample_rate: f32,
    stage: Stage,
    level: f32,
    /// Level captured at the moment `release()` is called, so the release time
    /// is honored regardless of where in the envelope the note was let go.
    release_start: f32,

    pub attack_secs: f32,
    pub decay_secs: f32,
    pub sustain: f32,
    pub release_secs: f32,
}

impl Adsr {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            stage: Stage::Idle,
            level: 0.0,
            release_start: 0.0,
            attack_secs: 0.01,
            decay_secs: 0.10,
            sustain: 0.8,
            release_secs: 0.30,
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// The current envelope level in `0.0..=1.0`, **without** advancing. Used by
    /// the mod matrix to read an envelope as a modulation source (the amp and
    /// filter envelopes already advance once per sample in the voice's signal
    /// path, so re-reading their value here must not tick them again).
    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn trigger(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.release_start = self.level;
            self.stage = Stage::Release;
        }
    }

    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }

    /// Advance one sample and return the current level in `0.0..=1.0`.
    pub fn next(&mut self) -> f32 {
        let per_sample = |secs: f32, sr: f32| -> f32 {
            if secs <= 0.0 {
                1.0
            } else {
                1.0 / (secs * sr)
            }
        };

        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += per_sample(self.attack_secs, self.sample_rate);
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let span = 1.0 - self.sustain;
                self.level -= per_sample(self.decay_secs, self.sample_rate) * span;
                if self.level <= self.sustain {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {
                self.level = self.sustain;
            }
            Stage::Release => {
                self.level -= per_sample(self.release_secs, self.sample_rate) * self.release_start;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }

        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attack_reaches_peak() {
        let mut env = Adsr::new(48_000.0);
        env.attack_secs = 0.01; // 480 samples
        env.trigger();
        for _ in 0..480 {
            env.next();
        }
        assert!(env.next() >= 0.99, "attack should reach ~1.0");
    }

    #[test]
    fn release_returns_to_silence_and_idle() {
        let mut env = Adsr::new(48_000.0);
        env.attack_secs = 0.001;
        env.decay_secs = 0.001;
        env.sustain = 0.5;
        env.release_secs = 0.01;
        env.trigger();
        for _ in 0..200 {
            env.next(); // settle into the sustain stage
        }
        env.release();
        for _ in 0..1000 {
            env.next();
        }
        assert_eq!(env.next(), 0.0);
        assert!(!env.is_active(), "envelope should be idle after release");
    }
}
