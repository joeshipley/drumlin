mod bake_kits;

fn main() -> nih_plug_xtask::Result<()> {
    // `cargo xtask bake-kits` — the kit-library graduation step (M12): transcribe
    // crates/drumlin/kits/*.kit.json into generated &'static factory Rust.
    if std::env::args().nth(1).as_deref() == Some("bake-kits") {
        return bake_kits::run().map_err(|e| std::io::Error::other(e).into());
    }
    nih_plug_xtask::main()
}
