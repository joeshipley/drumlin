#!/usr/bin/env bash
#
# notarize.sh — Developer ID sign + notarize + staple the Drumlin artifacts.
#
# This is the SHIP path. It is distinct from build-au.sh, which ad-hoc signs and
# installs for LOCAL auval. Run build-au.sh first to produce the .component, then
# run this to produce distributable, Gatekeeper-passing artifacts.
#
# What it does:
#   1. Developer ID sign the CLAP, the .component (inside-out, hardened runtime),
#      and the standalone .app.
#   2. Zip each, submit to Apple via `notarytool` (waits for the verdict).
#   3. Staple the notarization ticket onto each bundle.
#   4. Verify with `codesign --verify` and `spctl`.
#
# ---------------------------------------------------------------------------
# WHAT YOU (the human) MUST PROVIDE — an agent cannot create these:
#
#   1. A "Developer ID Application" certificate + its private key, installed in
#      your login keychain. Requires a paid ($99/yr) Apple Developer account.
#      Create it at https://developer.apple.com/account/resources/certificates
#      (or via Xcode > Settings > Accounts > Manage Certificates > +). Verify:
#          security find-identity -v -p codesigning
#      You should see one line like:
#          1) ABCD1234... "Developer ID Application: Joe Shipley (TEAMID1234)"
#
#   2. notarytool credentials. The recommended form is a stored keychain
#      PROFILE created ONCE with an app-specific password:
#        a. Create an app-specific password at https://account.apple.com
#           (Sign-In and Security > App-Specific Passwords).
#        b. Find your Team ID at https://developer.apple.com/account (top right,
#           e.g. TEAMID1234) — it is the 10-char code in the cert name above.
#        c. Store a reusable profile (prompts for the app-specific password):
#             xcrun notarytool store-credentials "drumlin-notary" \
#               --apple-id "you@example.com" \
#               --team-id  "TEAMID1234"
#      Thereafter this script just references the profile name. No secrets live
#      in the repo or in env vars.
#
# Set SIGN_IDENTITY (full cert common name) and NOTARY_PROFILE below, or via env.
# ---------------------------------------------------------------------------

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE="${BUNDLE:-Drumlin}"
AU_OUTPUT_NAME="${AU_OUTPUT_NAME:-Drumlin}"

# REQUIRED — override via env or edit here:
SIGN_IDENTITY="${SIGN_IDENTITY:-Developer ID Application: Joe Shipley (TEAMID1234)}"
NOTARY_PROFILE="${NOTARY_PROFILE:-drumlin-notary}"

# Artifact locations.
COMPONENT_SRC="$REPO_ROOT/build/auv2-consumer/build/${AU_OUTPUT_NAME}.component"
CLAP_SRC="$REPO_ROOT/target/bundled/${BUNDLE}.clap"
APP_SRC="$REPO_ROOT/target/bundled/${BUNDLE}.app"

DIST_DIR="$REPO_ROOT/dist"
ENTITLEMENTS="$REPO_ROOT/scripts/drumlin.entitlements"

mkdir -p "$DIST_DIR"

# --- sanity: identity + profile exist -------------------------------------
echo "==> Preflight"
if ! security find-identity -v -p codesigning | grep -q "$SIGN_IDENTITY"; then
  echo "ERROR: signing identity not found in keychain:"
  echo "       \"$SIGN_IDENTITY\""
  echo "       Run: security find-identity -v -p codesigning"
  exit 1
fi
[ -d "$CLAP_SRC" ]      || { echo "ERROR: $CLAP_SRC not found (run scripts/build-au.sh)"; exit 1; }
[ -d "$COMPONENT_SRC" ] || { echo "ERROR: $COMPONENT_SRC not found (run scripts/build-au.sh)"; exit 1; }

# --- ensure entitlements file exists (hardened runtime needs JIT for the GUI) ---
if [ ! -f "$ENTITLEMENTS" ]; then
  echo "==> Writing default entitlements -> $ENTITLEMENTS"
  cat > "$ENTITLEMENTS" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <!-- nih_plug_webview embeds a WKWebView; the hardened runtime blocks JIT
       unless these are granted. Without them the GUI renders blank in a
       hardened host. -->
  <key>com.apple.security.cs.allow-jit</key><true/>
  <key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/>
</dict>
</plist>
PLIST
fi

# sign() — Developer ID, hardened runtime (--options runtime), with timestamp.
# $1 = path to bundle.
sign() {
  local target="$1"
  codesign --force --timestamp --options runtime \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGN_IDENTITY" "$target"
}

# --- 1. SIGN, inside-out ---------------------------------------------------
echo "==> 1/4 Signing (inside-out, hardened runtime)"
# CLAP standalone (sign nested binaries first, then the bundle).
sign "$CLAP_SRC"
# The .component embeds its own copy of the CLAP at Contents/PlugIns — sign the
# inner CLAP, then the outer .component last so the outer seal is valid.
sign "$COMPONENT_SRC/Contents/PlugIns/${BUNDLE}.clap"
sign "$COMPONENT_SRC"
# Standalone app (optional but ships).
if [ -d "$APP_SRC" ]; then
  sign "$APP_SRC"
fi

# --- 2. NOTARIZE -----------------------------------------------------------
# notarytool requires a zip (or dmg/pkg) — it does not take a bare .component.
notarize_bundle() {
  local bundle="$1" zip="$2"
  echo "    zipping $(basename "$bundle") -> $(basename "$zip")"
  ditto -c -k --keepParent "$bundle" "$zip"
  echo "    submitting $(basename "$zip") (waits for Apple's verdict)"
  xcrun notarytool submit "$zip" --keychain-profile "$NOTARY_PROFILE" --wait
}

echo "==> 2/4 Notarizing"
notarize_bundle "$COMPONENT_SRC" "$DIST_DIR/${AU_OUTPUT_NAME}.component.zip"
notarize_bundle "$CLAP_SRC"      "$DIST_DIR/${BUNDLE}.clap.zip"
[ -d "$APP_SRC" ] && notarize_bundle "$APP_SRC" "$DIST_DIR/${BUNDLE}.app.zip"

# --- 3. STAPLE -------------------------------------------------------------
# Staple the ticket onto the bundle itself (the zip is just a transport).
echo "==> 3/4 Stapling"
xcrun stapler staple "$COMPONENT_SRC"
xcrun stapler staple "$CLAP_SRC"
[ -d "$APP_SRC" ] && xcrun stapler staple "$APP_SRC"

# --- 4. VERIFY -------------------------------------------------------------
echo "==> 4/4 Verifying"
codesign --verify --deep --strict --verbose=2 "$COMPONENT_SRC"
codesign --verify --deep --strict --verbose=2 "$CLAP_SRC"
xcrun stapler validate "$COMPONENT_SRC"
xcrun stapler validate "$CLAP_SRC"
# spctl assesses Gatekeeper acceptance. Plug-ins are not "execute" type, so
# `spctl -a -t install` is the right assessment for a loadable bundle.
spctl -a -vvv -t install "$COMPONENT_SRC" || true

echo "==> Done. Distributable, notarized, stapled artifacts:"
echo "    $COMPONENT_SRC"
echo "    $CLAP_SRC"
[ -d "$APP_SRC" ] && echo "    $APP_SRC"
echo "    (re-zip the stapled bundles for distribution, or ship a signed+notarized .pkg/.dmg)"
