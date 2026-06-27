# Shipping Drumlin (M10)

The release runbook: how to validate, sign, notarize, and verify a distributable
build. Drumlin **v1.0.0**, **arm64-only** (Apple Silicon, macOS 11+).

There are two scripts, and they are different:

| Script | Purpose | Signing |
|---|---|---|
| `scripts/build-au.sh` | Build the `.component` / `.clap` and `auval` it **locally** | ad-hoc (`codesign -s -`) |
| `scripts/notarize.sh` | Produce **distributable**, Gatekeeper-passing artifacts | Developer ID + notarized + stapled |

`build-au.sh` is the inner loop; `notarize.sh` is the ship path. Run `build-au.sh`
first to produce the bundles, then `notarize.sh` to sign + notarize them.

---

## 1. One-time setup (human-only — an agent cannot create these)

A paid Apple Developer account ($99/yr — the same one Esker uses) is required.

1. **Developer ID Application certificate.** Create it at
   <https://developer.apple.com/account/resources/certificates> (or Xcode ›
   Settings › Accounts › Manage Certificates › +) and install it in your login
   keychain. Verify:
   ```sh
   security find-identity -v -p codesigning
   # -> 1) ABCD… "Developer ID Application: Joe Shipley (TEAMID1234)"
   ```
   Until this returns a valid identity, signing is blocked.

2. **App-specific password** at <https://account.apple.com> (Sign-In and
   Security › App-Specific Passwords). Note your 10-char **Team ID** (the code in
   the cert's common name).

3. **Store the notarytool profile once** (no secret then lives in the repo):
   ```sh
   xcrun notarytool store-credentials "drumlin-notary" \
     --apple-id you@example.com --team-id TEAMID1234
   # prompts for the app-specific password
   ```

4. **Point `notarize.sh` at your identity** — set `SIGN_IDENTITY` (the full cert
   common name) and `NOTARY_PROFILE` (`drumlin-notary`) at the top of
   `scripts/notarize.sh`, or pass them via env.

---

## 2. Pre-ship validation gate (M10)

Run before every release. All of this is automated except the final manual RT run.

```sh
# Unit + property + golden suite (must be all green; the 13 goldens are byte-exact)
cargo test -p percussion_core
cargo test -p drumlin
```

What the suite proves:
- **Goldens** (`percussion_core::golden`) — the 12 voices + default pattern are
  bit-for-bit unchanged. A diff here is a regression **unless the sound change was
  intentional** — then regenerate (see §4).
- **Finiteness + silence** (`kit::*`) — every voice at param extremes, the
  saturated mod matrix, and every factory GROOVE WORLD render finite + bus-limited
  (peak ≤ 1.02); an idle kit is *exact* silence and a fed bus flushes to true zero.
- **NaN/denormal folds** (`drift`, `resonator`, `pitch_env`, voices) — a poisoned
  intermediate never escapes as NaN/inf.
- **RT-safety** — `audio_hot_path_is_alloc_free` proves the per-block audio work
  makes **zero heap allocations**; `process_takes_only_try_lock` pins the
  no-blocking-lock contract.
- **Scheduler headroom** — `dense_ratchets_dont_overflow_large_blocks`: the
  densest pattern keeps ring headroom up to **8192-frame** host blocks.

Then the AU validation gate + the manual RT sign-off:

```sh
# Builds the bundles and runs `auval -v aumu Drml JShp` — must end "AU VALIDATION SUCCEEDED."
bash scripts/build-au.sh

# Manual RT sign-off (needs a live audio device + display — not CI-able):
# the DEBUG standalone runs the SHIPPED process() under nih-plug's
# assert_process_allocs, which panics if process() ever allocates.
cargo run --bin drumlin
#   …then play a dense pattern, recall a KIT, sweep the macros. No panic = pass.
#   (The automated audio_hot_path_is_alloc_free test is the CI-able proxy for this.)
```

---

## 3. Build + notarize + verify

```sh
bash scripts/build-au.sh     # build + local auval (ad-hoc signed)
bash scripts/notarize.sh     # Developer ID sign (hardened runtime + JIT entitlements
                             # for the WKWebView GUI), notarytool submit --wait, staple
```

`notarize.sh` finishes by verifying each artifact:
```sh
codesign --verify --deep --strict --verbose=2 <bundle>   # valid on disk
codesign -dvv <bundle>                                    # Developer ID + TeamIdentifier + runtime flag
xcrun stapler validate <bundle>                           # ticket stapled
spctl -a -t install -vvv <bundle>                         # accepted / "source=Notarized Developer ID"
```

**Final end-to-end Gatekeeper check (do this on a *second* Mac** — one that never
built Drumlin): copy the stapled `.component`, load it in Logic, and confirm it
both instantiates **and** the webview GUI renders. A blank GUI under the hardened
runtime means the JIT entitlements need attention. Confirm `spctl` reports
`accepted` / `source=Notarized Developer ID` there too.

---

## 4. The golden rule

`percussion_core`'s golden renders are bit-exact regression anchors. **On an
intentional sound change**, regenerate them and commit the new `golden/*.bin`:

```sh
cargo test -p percussion_core regenerate_goldens -- --ignored
```

Never regenerate to "make a failing test pass" — an unexplained golden diff is a
regression signal. M10's hardening was deliberately golden-safe (every NaN-fold is
a no-op at the defaults), so it required no regen.

---

## 5. Version bump checklist

Bump in lockstep:
- `Cargo.toml` › `[workspace.package]` › `version`
- `scripts/build-au.sh` › `AU_BUNDLE_VERSION` (host caching + Gatekeeper key off it)
