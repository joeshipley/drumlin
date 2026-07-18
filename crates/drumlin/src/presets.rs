//! Disk presets (M9, design §8) — save/load the whole machine to
//! `~/Library/Application Support/Drumlin/presets/*.drumlin.json` (or the
//! platform equivalent via the `directories` crate). A preset is a full snapshot
//! — the serde `PersistState` (voices/mix/mod/pattern bank/macro labels) plus the
//! 9 bus-FX params + the sidechain toggle. Loading routes through the same atomic
//! recall path a KIT uses.
//!
//! All I/O is editor-thread only (never `process`). The core fns take an explicit
//! `dir` so they're unit-testable against a temp directory; thin public wrappers
//! resolve the real presets dir. Path handling is defensive: names are sanitized
//! to a safe stem, the final path is re-validated to live directly in the presets
//! dir, and reads are size-capped.

use crate::ModState;
use directories::ProjectDirs;
use percussion_core::{VoiceMix, VoicePatch};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FORMAT_TAG: &str = "drumlin-preset-v1";
/// File suffix: `<stem>.drumlin.json`.
const EXT: &str = "drumlin.json";
/// Refuse to read anything larger than this (a corrupt/hostile file guard).
const MAX_PRESET_BYTES: u64 = 1024 * 1024; // 1 MiB (a sound preset is a few KB)

/// A portable SOUND snapshot: the per-voice patch + mix + mod state + macro
/// labels + the bus-FX chain + sidechain. NOT the pattern bank — that's project
/// state the host already persists; a preset loads a sound onto your groove.
#[derive(Serialize, Deserialize)]
pub(crate) struct DiskPreset {
    pub format: String,
    pub name: String,
    pub voices: VoicePatch,
    pub mix: VoiceMix,
    pub mod_state: ModState,
    pub macro_labels: [String; 8],
    /// Bus-FX normalized values, `pget!` ids 1..=9 (index = id - 1).
    pub bus: [f32; 9],
    pub sidechain: bool,
}

/// `~/Library/Application Support/Drumlin/presets` (or the platform equivalent).
pub(crate) fn presets_dir() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("com", "joeshipley", "Drumlin")?;
    Some(dirs.config_dir().join("presets"))
}

/// Sanitize a user name into a safe filename stem: alphanumerics + space / - / _
/// only (anything else -> `_`), trimmed + length-capped, never empty and never a
/// path separator. Prevents traversal and odd filenames.
pub(crate) fn sanitize_stem(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect();
    s = s.trim().to_string();
    if s.is_empty() {
        "untitled".to_string()
    } else {
        s
    }
}

/// The on-disk path for a name in `dir`, re-validated to sit directly inside
/// `dir` (defense in depth on top of `sanitize_stem`).
fn safe_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(format!("{}.{}", sanitize_stem(name), EXT));
    if path.parent() != Some(dir) {
        return None;
    }
    Some(path)
}

pub(crate) fn save_in(dir: &Path, preset: &DiskPreset) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = safe_path(dir, &preset.name).ok_or_else(|| io_err("bad preset name"))?;
    let json = serde_json::to_string_pretty(preset).map_err(|e| io_err(&e.to_string()))?;
    std::fs::write(path, json)
}

pub(crate) fn list_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let suffix = format!(".{EXT}");
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_suffix(&suffix)
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

pub(crate) fn load_in(dir: &Path, name: &str) -> Option<DiskPreset> {
    let path = safe_path(dir, name)?;
    if std::fs::metadata(&path).ok()?.len() > MAX_PRESET_BYTES {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    let preset: DiskPreset = serde_json::from_str(&data).ok()?;
    (preset.format == FORMAT_TAG).then_some(preset)
}

pub(crate) fn delete_in(dir: &Path, name: &str) -> std::io::Result<()> {
    let path = safe_path(dir, name).ok_or_else(|| io_err("bad preset name"))?;
    std::fs::remove_file(path)
}

// --- thin wrappers over the real presets dir (used by the plugin) ---

pub(crate) fn save(preset: &DiskPreset) -> std::io::Result<()> {
    save_in(&presets_dir().ok_or_else(|| io_err("no config dir"))?, preset)
}
pub(crate) fn list() -> Vec<String> {
    presets_dir().map(|d| list_in(&d)).unwrap_or_default()
}
pub(crate) fn load(name: &str) -> Option<DiskPreset> {
    load_in(&presets_dir()?, name)
}
pub(crate) fn delete(name: &str) -> std::io::Result<()> {
    delete_in(&presets_dir().ok_or_else(|| io_err("no config dir"))?, name)
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_blocks_traversal_and_separators() {
        assert_eq!(sanitize_stem("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_stem("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_stem("  "), "untitled");
        assert_eq!(sanitize_stem(""), "untitled");
        assert_eq!(sanitize_stem("My Kit-2"), "My Kit-2"); // friendly names kept
        // safe_path keeps it inside the dir (no escape).
        let dir = Path::new("/tmp/drumlin_presets");
        let p = safe_path(dir, "../evil").unwrap();
        assert_eq!(p.parent(), Some(dir), "path must stay in the presets dir");
    }

    #[test]
    fn save_list_load_delete_round_trips() {
        // A unique temp dir so parallel tests don't collide.
        let dir = std::env::temp_dir().join(format!("drumlin_preset_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let preset = DiskPreset {
            format: FORMAT_TAG.to_string(),
            name: "Test World".to_string(),
            voices: VoicePatch::default(),
            mix: VoiceMix::default(),
            mod_state: ModState::default(),
            macro_labels: crate::default_macro_labels(),
            bus: [0.1, 0.2, 0.3, 0.4, 0.5, 0.5, 0.6, 0.7, 0.8],
            sidechain: true,
        };
        save_in(&dir, &preset).unwrap();
        assert_eq!(list_in(&dir), vec!["Test World".to_string()]);
        let loaded = load_in(&dir, "Test World").expect("loads back");
        assert_eq!(loaded.name, "Test World");
        assert!(loaded.sidechain);
        assert!((loaded.bus[0] - 0.1).abs() < 1e-6);
        delete_in(&dir, "Test World").unwrap();
        assert!(list_in(&dir).is_empty(), "delete removes it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
