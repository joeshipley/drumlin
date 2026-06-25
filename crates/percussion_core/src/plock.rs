//! Per-step **parameter locks** — Drumlin's marquee feature (design §4.2). A
//! `PLock` overrides one lockable parameter for one hit only; the voice/tail
//! applies it for that trigger and restores the patch default on the next.
//!
//! `LockableParam` is a stable, index-addressed registry (the same discipline as
//! Esker's `ModDest`): variants keep their index, new ones append at the end, and
//! a reconcile test pins index ↔ id-string so a saved p-lock never points at the
//! wrong parameter. The `value` is normalized `0..1` — the SAME encoding the
//! scene/KIT system uses — so one shared denormalize path drives both.
//!
//! M5 part 1 scopes locks to the per-voice **tail** (level/pan/cutoff/resonance/
//! drive), which is uniform across all 12 voices. Voice-engine locks (pitch,
//! decay) arrive with the mod-matrix param infrastructure (M6).

/// Max parameter locks per step (MVP; design §4.2). Sizing the `Step` struct.
pub const MAX_PLOCKS: usize = 4;

/// One per-step parameter lock: `param` indexes [`LOCKABLE_PARAMS`], `value` is
/// normalized `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PLock {
    pub param: u16,
    pub value: f32,
}

impl Default for PLock {
    fn default() -> Self {
        Self { param: 0, value: 0.0 }
    }
}

/// The lockable per-voice tail parameters. Index = position; append-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockableParam {
    Level,
    Pan,
    Cutoff,
    Resonance,
    Drive,
}

impl LockableParam {
    pub const COUNT: usize = 5;

    pub fn index(self) -> u16 {
        match self {
            LockableParam::Level => 0,
            LockableParam::Pan => 1,
            LockableParam::Cutoff => 2,
            LockableParam::Resonance => 3,
            LockableParam::Drive => 4,
        }
    }

    pub fn from_index(i: u16) -> Option<Self> {
        match i {
            0 => Some(LockableParam::Level),
            1 => Some(LockableParam::Pan),
            2 => Some(LockableParam::Cutoff),
            3 => Some(LockableParam::Resonance),
            4 => Some(LockableParam::Drive),
            _ => None,
        }
    }

    /// Stable id string (matches the GUI / preset encoding).
    pub fn id(self) -> &'static str {
        match self {
            LockableParam::Level => "level",
            LockableParam::Pan => "pan",
            LockableParam::Cutoff => "cutoff",
            LockableParam::Resonance => "resonance",
            LockableParam::Drive => "drive",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "level" => Some(LockableParam::Level),
            "pan" => Some(LockableParam::Pan),
            "cutoff" => Some(LockableParam::Cutoff),
            "resonance" => Some(LockableParam::Resonance),
            "drive" => Some(LockableParam::Drive),
            _ => None,
        }
    }

    /// Map a normalized `0..1` value to the parameter's engine units.
    pub fn denormalize(self, norm: f32) -> f32 {
        let n = norm.clamp(0.0, 1.0);
        match self {
            LockableParam::Level => n * 2.0,                 // 0..2 (unity at 0.5)
            LockableParam::Pan => n * 2.0 - 1.0,             // -1..+1
            LockableParam::Cutoff => 20.0 * 1000.0_f32.powf(n), // 20 Hz..20 kHz, log
            LockableParam::Resonance => n,                   // 0..1
            LockableParam::Drive => n,                       // 0..1
        }
    }
}

/// The registry, in index order. A p-lock's `param` field indexes this.
pub const LOCKABLE_PARAMS: [LockableParam; LockableParam::COUNT] = [
    LockableParam::Level,
    LockableParam::Pan,
    LockableParam::Cutoff,
    LockableParam::Resonance,
    LockableParam::Drive,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_id_round_trip() {
        for (i, &p) in LOCKABLE_PARAMS.iter().enumerate() {
            assert_eq!(p.index() as usize, i, "index must match registry position");
            assert_eq!(LockableParam::from_index(i as u16), Some(p));
            assert_eq!(LockableParam::from_id(p.id()), Some(p), "id round-trip");
        }
        assert_eq!(LockableParam::from_index(999), None);
        assert_eq!(LockableParam::from_id("nope"), None);
        assert_eq!(LOCKABLE_PARAMS.len(), LockableParam::COUNT);
    }

    #[test]
    fn id_pins_are_literal_and_stable() {
        // Explicit literal pins (not derived from the match) so a reorder/rename
        // of the enum — which would silently corrupt saved p-locks — is caught.
        assert_eq!(LockableParam::from_index(0).unwrap().id(), "level");
        assert_eq!(LockableParam::from_index(1).unwrap().id(), "pan");
        assert_eq!(LockableParam::from_index(2).unwrap().id(), "cutoff");
        assert_eq!(LockableParam::from_index(3).unwrap().id(), "resonance");
        assert_eq!(LockableParam::from_index(4).unwrap().id(), "drive");
        assert_eq!(LockableParam::COUNT, 5);
    }

    #[test]
    fn denormalize_hits_expected_endpoints() {
        assert!((LockableParam::Pan.denormalize(0.5) - 0.0).abs() < 1e-6);
        assert!((LockableParam::Pan.denormalize(0.0) + 1.0).abs() < 1e-6);
        assert!((LockableParam::Level.denormalize(0.5) - 1.0).abs() < 1e-6);
        assert!((LockableParam::Cutoff.denormalize(0.0) - 20.0).abs() < 1e-3);
        assert!((LockableParam::Cutoff.denormalize(1.0) - 20_000.0).abs() < 1.0);
    }
}
