//! User kit files (M12) — the library-authoring loop. A `.kit.json` in
//! `~/Music/Drumlin/Kits/` is a portable, human-editable KIT: the same
//! `KitRow` lens the factory speaks, plus a name, labels, and a DIG `terrain`
//! dialect. The KITS page lists the folder live (MY KITS), EXPORT KIT writes
//! one from the machine's current sound (the diff against Neutral), and a
//! graduated kit can later be baked into the factory `&'static` bank.
//!
//! Same discipline as `presets.rs`: editor-thread only, sanitized stems,
//! size-capped reads, dir-parameterized fns for tests.

use crate::kits::KitRow;
use crate::presets::sanitize_stem;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FORMAT_TAG: &str = "drumlin-kit-v1";
/// File suffix: `<stem>.kit.json`.
const EXT: &str = "kit.json";
/// A kit is a few KB of rows; refuse anything bloated/hostile.
const MAX_KIT_BYTES: u64 = 256 * 1024;

/// An owned kit — what a `.kit.json` deserializes to. Timbral only (no
/// pattern): grooves come from the DIG in the kit's `terrain` dialect.
#[derive(Clone, Serialize, Deserialize)]
pub struct KitDef {
    pub format: String,
    pub name: String,
    #[serde(default)]
    pub blurb: String,
    /// DIG dialect id (a registered terrain); unknown ids fall back to techno.
    #[serde(default = "default_terrain")]
    pub terrain: String,
    pub macro_labels: [String; 8],
    pub rows: Vec<KitRow>,
}

fn default_terrain() -> String {
    "techno".to_string()
}

/// `~/Music/Drumlin/Kits` (next to the MIDI exports — user-facing, draggable).
pub fn kits_dir() -> Option<PathBuf> {
    crate::midi_export::export_dir().map(|d| d.join("Kits"))
}

/// The on-disk path for a name in `dir`, re-validated to sit directly inside it.
fn safe_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(format!("{}.{}", sanitize_stem(name), EXT));
    if path.parent() != Some(dir) {
        return None;
    }
    Some(path)
}

pub(crate) fn save_in(dir: &Path, def: &KitDef) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = safe_path(dir, &def.name)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "bad kit name"))?;
    let json = serde_json::to_string_pretty(def)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

pub(crate) fn list_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let suffix = format!(".{EXT}");
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name().to_string_lossy().strip_suffix(&suffix).map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

pub(crate) fn load_in(dir: &Path, name: &str) -> Option<KitDef> {
    let path = safe_path(dir, name)?;
    if std::fs::metadata(&path).ok()?.len() > MAX_KIT_BYTES {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    let def: KitDef = serde_json::from_str(&data).ok()?;
    (def.format == FORMAT_TAG).then_some(def)
}

// --- thin wrappers over the real kits dir (used by the plugin) ---

pub(crate) fn save(def: &KitDef) -> std::io::Result<PathBuf> {
    save_in(
        &kits_dir().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no music dir"))?,
        def,
    )
}
pub(crate) fn list() -> Vec<String> {
    kits_dir().map(|d| list_in(&d)).unwrap_or_default()
}
pub(crate) fn load(name: &str) -> Option<KitDef> {
    load_in(&kits_dir()?, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_def() -> KitDef {
        KitDef {
            format: FORMAT_TAG.to_string(),
            name: "Test Cavern".to_string(),
            blurb: "unit test".to_string(),
            terrain: "cavern".to_string(),
            macro_labels: crate::default_macro_labels(),
            rows: vec![
                KitRow::Voice { track: 0, param: 2, norm: 0.3 },
                KitRow::Mix { track: 2, field: 0, norm: 0.6 },
                KitRow::ModSlot { slot: 0, src: 3, dst: 3, depth: 0.5, voice: 255 },
                KitRow::Lfo { idx: 0, shape: 2, rate: 8.0, depth: 1.0, retrig: false },
                KitRow::ModEnv { attack: 0.01, decay: 0.4 },
                KitRow::Bus { id: 3, norm: 0.4 },
                KitRow::Sidechain(true),
            ],
        }
    }

    #[test]
    fn kit_files_round_trip_every_row_kind() {
        let dir = std::env::temp_dir().join(format!("drumlin_kit_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let def = demo_def();
        let path = save_in(&dir, &def).expect("saves");
        assert!(path.ends_with("Test Cavern.kit.json"));
        assert_eq!(list_in(&dir), vec!["Test Cavern".to_string()]);
        let back = load_in(&dir, "Test Cavern").expect("loads");
        assert_eq!(back.terrain, "cavern");
        assert_eq!(back.rows.len(), def.rows.len());
        // Every row kind survives the JSON round trip with its payload.
        match (&back.rows[0], &def.rows[0]) {
            (KitRow::Voice { track: a, param: b, norm: c }, KitRow::Voice { track: x, param: y, norm: z }) => {
                assert_eq!((a, b), (x, y));
                assert!((c - z).abs() < 1e-6);
            }
            _ => panic!("row 0 changed kind"),
        }
        assert!(matches!(back.rows[4], KitRow::ModEnv { .. }));
        assert!(matches!(back.rows[6], KitRow::Sidechain(true)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_names_and_formats_are_refused() {
        let dir = std::env::temp_dir().join(format!("drumlin_kit_evil_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut def = demo_def();
        def.name = "../../escape".to_string();
        let path = save_in(&dir, &def).expect("sanitized save still lands");
        assert_eq!(path.parent(), Some(dir.as_path()), "must stay inside the kits dir");
        // A wrong format tag refuses to load.
        let mut bad = demo_def();
        bad.name = "Wrong".to_string();
        bad.format = "not-a-kit".to_string();
        save_in(&dir, &bad).unwrap();
        assert!(load_in(&dir, "Wrong").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
