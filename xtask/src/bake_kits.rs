//! `cargo xtask bake-kits` (M12) — graduate library kits from JSON to Rust.
//!
//! Reads every `crates/drumlin/kits/*.kit.json` (the version-controlled library
//! source — keepers copied here from `~/Music/Drumlin/Kits/` after a listening
//! session) and regenerates `crates/drumlin/src/kits_baked.rs`: one `&'static
//! Kit` per file plus the `BAKED_KITS` slice the factory list chains in.
//!
//! This tool only TRANSCRIBES (with eager structural validation for good error
//! messages); the real guards are crate-side — the generated file must compile
//! against the actual `KitRow` enum, and the factory tests (row-decode, terrain
//! registration, id uniqueness, finite render) run over every baked kit.

use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, String>;

pub fn run() -> Result<()> {
    let root = workspace_root();
    let src_dir = root.join("crates/drumlin/kits");
    let out_path = root.join("crates/drumlin/src/kits_baked.rs");

    let mut files: Vec<PathBuf> = match std::fs::read_dir(&src_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".kit.json")))
            .collect(),
        Err(_) => Vec::new(),
    };
    // Stable output: sorted by filename.
    files.sort();

    let mut kits = Vec::new();
    let mut seen_ids = vec![
        // The hand-authored core; the crate-side uniqueness test is the real guard.
        "neutral".to_string(),
        "discotheque".to_string(),
        "marseille".to_string(),
        "bladerunner".to_string(),
        "outrun".to_string(),
    ];
    for path in &files {
        let kit = bake_one(path)?;
        if seen_ids.contains(&kit.id) {
            return Err(format!("{}: duplicate kit id `{}`", path.display(), kit.id));
        }
        seen_ids.push(kit.id.clone());
        kits.push(kit);
    }

    let generated = render(&kits);
    std::fs::write(&out_path, generated).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    println!(
        "baked {} kit(s) from {} -> {}",
        kits.len(),
        src_dir.display(),
        out_path.display()
    );
    Ok(())
}

fn workspace_root() -> PathBuf {
    // xtask/ lives directly under the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask has a parent").to_path_buf()
}

struct BakedKit {
    id: String,
    ident: String,
    name: String,
    blurb: String,
    terrain: String,
    macro_labels: [String; 8],
    rows: Vec<String>, // pre-rendered `KitRow::...` literals
}

fn bake_one(path: &Path) -> Result<BakedKit> {
    let ctx = |m: &str| format!("{}: {m}", path.display());
    let data = std::fs::read_to_string(path).map_err(|e| ctx(&e.to_string()))?;
    let v: Value = serde_json::from_str(&data).map_err(|e| ctx(&format!("bad JSON: {e}")))?;

    if v["format"].as_str() != Some("drumlin-kit-v1") {
        return Err(ctx("format must be \"drumlin-kit-v1\""));
    }
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".kit.json"))
        .ok_or_else(|| ctx("bad filename"))?;
    let id = kebab_id(stem);
    if id.is_empty() {
        return Err(ctx("filename yields an empty id"));
    }
    let name = v["name"].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| ctx("missing name"))?;
    let blurb = v["blurb"].as_str().unwrap_or("");
    let terrain = v["terrain"].as_str().unwrap_or("techno");

    let labels_v = v["macro_labels"].as_array().ok_or_else(|| ctx("missing macro_labels[8]"))?;
    if labels_v.len() != 8 {
        return Err(ctx("macro_labels must have exactly 8 entries"));
    }
    let mut macro_labels: [String; 8] = Default::default();
    for (i, l) in labels_v.iter().enumerate() {
        macro_labels[i] = l.as_str().ok_or_else(|| ctx("macro_labels entries must be strings"))?.to_string();
    }

    let rows_v = v["rows"].as_array().ok_or_else(|| ctx("missing rows[]"))?;
    let mut rows = Vec::new();
    for (i, row) in rows_v.iter().enumerate() {
        rows.push(bake_row(row).map_err(|m| ctx(&format!("rows[{i}]: {m}")))?);
    }

    Ok(BakedKit {
        ident: format!("BAKED_{}", id.to_uppercase().replace('-', "_")),
        id,
        name: name.to_string(),
        blurb: blurb.to_string(),
        terrain: terrain.to_string(),
        macro_labels,
        rows,
    })
}

/// Filename stem -> a factory-style kebab id (lowercase alnum + dashes).
fn kebab_id(stem: &str) -> String {
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn bake_row(row: &Value) -> Result<String> {
    let obj = row.as_object().ok_or("row must be an object")?;
    if obj.len() != 1 {
        return Err("row must have exactly one variant key".into());
    }
    let (key, body) = obj.iter().next().unwrap();
    let f = |name: &str| -> Result<f64> {
        body[name].as_f64().filter(|x| x.is_finite()).ok_or(format!("missing/bad `{name}`"))
    };
    let u = |name: &str| -> Result<u64> { body[name].as_u64().ok_or(format!("missing/bad `{name}`")) };
    let in_range = |v: f64, lo: f64, hi: f64, what: &str| -> Result<f64> {
        if (lo..=hi).contains(&v) { Ok(v) } else { Err(format!("`{what}` {v} outside {lo}..={hi}")) }
    };
    match key.as_str() {
        "voice" => {
            let (t, p) = (u("track")?, u("param")?);
            if t >= 12 { return Err(format!("voice track {t} >= 12")); }
            if p >= 5 { return Err(format!("voice param {p} >= 5 (tail patch subset)")); }
            let n = in_range(f("norm")?, 0.0, 1.0, "norm")?;
            Ok(format!("KitRow::Voice {{ track: {t}, param: {p}, norm: {:?} }}", n as f32))
        }
        "mix" => {
            let (t, fld) = (u("track")?, u("field")?);
            if t >= 12 { return Err(format!("mix track {t} >= 12")); }
            if fld > 9 { return Err(format!("mix field {fld} > 9")); }
            // choke_group (5) and output (8) carry raw counts 0..=4; the rest 0..=1.
            let hi = if fld == 5 || fld == 8 { 4.0 } else { 1.0 };
            let n = in_range(f("norm")?, 0.0, hi, "norm")?;
            Ok(format!("KitRow::Mix {{ track: {t}, field: {fld}, norm: {:?} }}", n as f32))
        }
        "mod-slot" => {
            let (slot, src, dst, voice) = (u("slot")?, u("src")?, u("dst")?, u("voice")?);
            if slot >= 16 { return Err(format!("mod-slot slot {slot} >= 16")); }
            if src == 0 || src >= 21 { return Err(format!("mod-slot src {src} must be 1..=20 (not Off)")); }
            if dst == 0 || dst >= 13 { return Err(format!("mod-slot dst {dst} must be 1..=12 (not Off)")); }
            if voice != 255 && voice >= 12 { return Err(format!("mod-slot voice {voice} must be 255 or < 12")); }
            let d = in_range(f("depth")?, -1.0, 1.0, "depth")?;
            Ok(format!(
                "KitRow::ModSlot {{ slot: {slot}, src: {src}, dst: {dst}, depth: {:?}, voice: {voice} }}",
                d as f32
            ))
        }
        "lfo" => {
            let idx = u("idx")?;
            if idx > 1 { return Err(format!("lfo idx {idx} > 1")); }
            let shape = u("shape")?.min(255);
            let rate = in_range(f("rate")?, 0.01, 100.0, "rate")?;
            let depth = in_range(f("depth")?, 0.0, 1.0, "depth")?;
            let retrig = body["retrig"].as_bool().ok_or("missing/bad `retrig`")?;
            Ok(format!(
                "KitRow::Lfo {{ idx: {idx}, shape: {shape}, rate: {:?}, depth: {:?}, retrig: {retrig} }}",
                rate as f32, depth as f32
            ))
        }
        "mod-env" => {
            let a = in_range(f("attack")?, 0.0, 30.0, "attack")?;
            let d = in_range(f("decay")?, 0.0, 30.0, "decay")?;
            Ok(format!("KitRow::ModEnv {{ attack: {:?}, decay: {:?} }}", a as f32, d as f32))
        }
        "bus" => {
            let id = u("id")?;
            if !(1..=9).contains(&id) { return Err(format!("bus id {id} must be 1..=9")); }
            let n = in_range(f("norm")?, 0.0, 1.0, "norm")?;
            Ok(format!("KitRow::Bus {{ id: {id}, norm: {:?} }}", n as f32))
        }
        "sidechain" => {
            let b = row["sidechain"].as_bool().ok_or("sidechain must be a bool")?;
            Ok(format!("KitRow::Sidechain({b})"))
        }
        other => Err(format!("unknown row kind `{other}`")),
    }
}

fn render(kits: &[BakedKit]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! GENERATED by `cargo xtask bake-kits` — DO NOT EDIT.\n\
         //!\n\
         //! Source of truth: `crates/drumlin/kits/*.kit.json` (the graduated\n\
         //! library; see that folder's README for the listening-session flow).\n\
         //! Re-run the bake after adding/culling kits. Validated crate-side by\n\
         //! the factory tests (row decode, terrain registration, id uniqueness,\n\
         //! finite render).\n"
    );
    if kits.is_empty() {
        let _ = writeln!(out, "use crate::kits::Kit;\n");
        let _ = writeln!(out, "pub static BAKED_KITS: &[&Kit] = &[];");
        return out;
    }
    let _ = writeln!(out, "use crate::kits::{{Kit, KitRow}};\n");
    for k in kits {
        let _ = writeln!(out, "pub static {}: Kit = Kit {{", k.ident);
        let _ = writeln!(out, "    id: {:?},", k.id);
        let _ = writeln!(out, "    name: {:?},", k.name);
        let _ = writeln!(out, "    blurb: {:?},", k.blurb);
        let _ = writeln!(out, "    terrain: {:?},", k.terrain);
        let _ = writeln!(out, "    rows: &[");
        for r in &k.rows {
            let _ = writeln!(out, "        {r},");
        }
        let _ = writeln!(out, "    ],");
        let labels = k.macro_labels.iter().map(|l| format!("{l:?}")).collect::<Vec<_>>().join(", ");
        let _ = writeln!(out, "    macro_labels: [{labels}],");
        let _ = writeln!(out, "    pattern: None,");
        let _ = writeln!(out, "}};\n");
    }
    let list = kits.iter().map(|k| format!("&{}", k.ident)).collect::<Vec<_>>().join(", ");
    let _ = writeln!(out, "pub static BAKED_KITS: &[&Kit] = &[{list}];");
    out
}
