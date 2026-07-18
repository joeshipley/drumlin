# The kit library source (M12)

Every `*.kit.json` in this folder is a **graduated** factory kit — version-
controlled, reviewed by ears, and baked into compiled `&'static` data.

## The flow

1. **Draft / capture.** Kits are authored in the fast loop: design the sound in
   the plugin (in Logic, through the real bus) and hit **EXPORT KIT**, or draft
   JSON directly into `~/Music/Drumlin/Kits/`. That folder is the *audition*
   space — the KITS page lists it live under MY KITS.
2. **Listen.** Batches of five, one family per session. Recall each kit, dig a
   few grooves in its dialect (the `terrain` tag), keep or cull.
3. **Graduate.** Copy the keepers' `.kit.json` files into THIS folder, then:

   ```sh
   cargo xtask bake-kits
   ```

   which regenerates `../src/kits_baked.rs`. The baked kits join
   `factory_kits()` and are covered by the factory tests automatically
   (row decode, terrain registration, id uniqueness, finite render).

The kit **id** is the filename stem (kebab-cased) — rename the file to rename
the id. Baked kits are timbral-only (`pattern: None`): grooves come from the
DIG in the kit's terrain dialect.
