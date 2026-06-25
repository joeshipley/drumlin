//! `XorShift32` — the family's deterministic PRNG (the same algorithm Esker's
//! arp/Random-Lock uses; design §4.1). Drives per-step probability, humanize and
//! the GROOVE LOCK. Seeded and reproducible: a given seed reproduces the exact
//! same "random" performance every run, which is what lets a humanized pattern
//! still pass the bit-exact golden-trigger test.

/// Deterministically mix a seed + 3 coordinates into a 32-bit value (a
/// splitmix-style finalizer). Used to give every (track, step, purpose) cell its
/// OWN independent RNG, so editing one step never re-rolls its neighbours — yet
/// the whole pattern is still a pure function of `pattern.seed` (the GROOVE LOCK).
#[inline]
pub fn mix_seed(seed: u32, a: u32, b: u32, c: u32) -> u32 {
    let mut x = seed
        ^ a.wrapping_mul(0x9E37_79B1)
        ^ b.wrapping_mul(0x85EB_CA77)
        ^ c.wrapping_mul(0xC2B2_AE3D);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x
}

#[derive(Clone, Copy, Debug)]
pub struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    pub fn new(seed: u32) -> Self {
        // state must be non-zero
        Self { state: seed | 1 }
    }

    pub fn reseed(&mut self, seed: u32) {
        self.state = seed | 1;
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Uniform in `0..n` (returns 0 if n == 0).
    #[inline]
    pub fn next_below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.next_u32() % n
        }
    }

    /// Uniform in `0.0..1.0`.
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        // top 24 bits -> [0,1)
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniform in `-1.0..1.0`.
    #[inline]
    pub fn next_bipolar(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_sequence() {
        let mut a = XorShift32::new(0xC0FFEE);
        let mut b = XorShift32::new(0xC0FFEE);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn reseed_restores_sequence() {
        let mut r = XorShift32::new(42);
        let first: Vec<u32> = (0..16).map(|_| r.next_u32()).collect();
        for _ in 0..100 {
            r.next_u32();
        }
        r.reseed(42);
        let again: Vec<u32> = (0..16).map(|_| r.next_u32()).collect();
        assert_eq!(first, again);
    }

    #[test]
    fn next_below_is_in_range_and_varied() {
        let mut r = XorShift32::new(7);
        let mut seen_low = false;
        let mut seen_high = false;
        for _ in 0..1000 {
            let v = r.next_below(100);
            assert!(v < 100);
            if v < 10 {
                seen_low = true;
            }
            if v >= 90 {
                seen_high = true;
            }
        }
        assert!(seen_low && seen_high, "distribution should cover the range");
    }

    #[test]
    fn next_f32_is_unit_interval() {
        let mut r = XorShift32::new(123);
        for _ in 0..10_000 {
            let v = r.next_f32();
            assert!((0.0..1.0).contains(&v));
        }
    }
}
