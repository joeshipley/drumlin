#!/usr/bin/env bash
#
# build-au.sh — Build, sign, install and validate the Drumlin Audio Unit (AUv2).
#
# Cloned from Esker's scripts/build-au.sh; only the plugin identity defaults
# differ (PACKAGE=drumlin, BUNDLE=Drumlin, AU_SUBTYPE_CODE=Drml, bundle id).
#
# Pipeline:
#   1. cargo xtask bundle drumlin --release   (produces Drumlin.clap via bundler.toml)
#   2. clap-wrapper (CMake) wraps that prebuilt .clap into a .component
#   3. ad-hoc codesign --deep the .component
#   4. install to ~/Library/Audio/Plug-Ins/Components/
#   5. auval -v aumu <subtype> <manuf>
#
# clap-wrapper is cloned on demand into build/clap-wrapper (gitignored, not vendored).
# The CLAP SDK and Apple AudioUnitSDK are auto-downloaded by clap-wrapper via CPM.
#
# Requirements: cmake, a C++ toolchain (Xcode/CLT), cargo, git. macOS only.
#
# Override the AU identity / plugin name via environment variables (see below).

set -euo pipefail

# ---- Configuration (override via env) --------------------------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE="${PACKAGE:-drumlin}"                       # cargo package/crate name
BUNDLE="${BUNDLE:-Drumlin}"                         # .clap basename (set by bundler.toml)
AU_OUTPUT_NAME="${AU_OUTPUT_NAME:-Drumlin}"         # .component display/file name
AU_MANUF_NAME="${AU_MANUF_NAME:-Joe Shipley}"
AU_MANUF_CODE="${AU_MANUF_CODE:-JShp}"              # 4-char AU manufacturer code (shared family code)
AU_SUBTYPE_CODE="${AU_SUBTYPE_CODE:-Drml}"          # 4-char AU subtype code (distinct from Esker's Eskr)
AU_INSTRUMENT_TYPE="${AU_INSTRUMENT_TYPE:-aumu}"    # aumu = instrument/music device
AU_BUNDLE_VERSION="${AU_BUNDLE_VERSION:-0.1.0}"
AU_BUNDLE_ID="${AU_BUNDLE_ID:-com.joeshipley.drumlin}"
CLAP_WRAPPER_TAG="${CLAP_WRAPPER_TAG:-v0.12.1}"     # pin clap-wrapper for reproducibility
ARCH="${ARCH:-$(uname -m)}"                         # arm64 or x86_64
# clap-wrapper's shared_prologue.cmake does `if (${CMAKE_OSX_DEPLOYMENT_TARGET}
# VERSION_GREATER_EQUAL "10.15")`, which errors out if the var is empty. Pass an
# explicit target (>=10.15 -> uses std::filesystem, no gulrak CPM stub).
MACOS_DEPLOYMENT_TARGET="${MACOS_DEPLOYMENT_TARGET:-11.0}"

BUILD_DIR="$REPO_ROOT/build"
WRAPPER_DIR="$BUILD_DIR/clap-wrapper"
CONSUMER_DIR="$BUILD_DIR/auv2-consumer"
CLAP_PATH="$REPO_ROOT/target/bundled/${BUNDLE}.clap"
COMPONENTS_DIR="$HOME/Library/Audio/Plug-Ins/Components"

echo "==> 1/5 Building CLAP: cargo xtask bundle $PACKAGE --release (-> ${BUNDLE}.clap)"
( cd "$REPO_ROOT" && cargo xtask bundle "$PACKAGE" --release )
[ -d "$CLAP_PATH" ] || { echo "ERROR: $CLAP_PATH not found"; exit 1; }

echo "==> 2/5 Preparing clap-wrapper ($CLAP_WRAPPER_TAG)"
mkdir -p "$BUILD_DIR"
if [ ! -d "$WRAPPER_DIR/.git" ]; then
  git clone --depth 1 --branch "$CLAP_WRAPPER_TAG" --recurse-submodules \
    https://github.com/free-audio/clap-wrapper.git "$WRAPPER_DIR"
fi

# Generate the tiny consumer CMake project that wraps our prebuilt CLAP.
mkdir -p "$CONSUMER_DIR/src"
cat > "$CONSUMER_DIR/src/empty.cpp" <<'EOF'
// Intentionally empty. clap-wrapper attaches the AUv2 sources to this MODULE
// target; the wrapper loads the CLAP embedded in the .component at runtime.
EOF
cat > "$CONSUMER_DIR/CMakeLists.txt" <<EOF
cmake_minimum_required(VERSION 3.21)
project(drumlin_auv2 LANGUAGES C CXX)
if (APPLE)
  enable_language(OBJC)
  enable_language(OBJCXX)
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES TRUE CACHE BOOL "" FORCE)
set(CLAP_WRAPPER_BUILD_TESTS OFF CACHE BOOL "" FORCE)
add_subdirectory(${WRAPPER_DIR} clap-wrapper-build EXCLUDE_FROM_ALL)
add_library(drumlin_auv2 MODULE src/empty.cpp)
target_add_auv2_wrapper(
  TARGET drumlin_auv2
  OUTPUT_NAME "${AU_OUTPUT_NAME}"
  BUNDLE_IDENTIFIER "${AU_BUNDLE_ID}"
  BUNDLE_VERSION "${AU_BUNDLE_VERSION}"
  MANUFACTURER_NAME "${AU_MANUF_NAME}"
  MANUFACTURER_CODE "${AU_MANUF_CODE}"
  SUBTYPE_CODE "${AU_SUBTYPE_CODE}"
  INSTRUMENT_TYPE "${AU_INSTRUMENT_TYPE}"
  MACOSX_EMBEDDED_CLAP_LOCATION "${CLAP_PATH}"
)
EOF

echo "==> 3/5 Configuring + building the .component (arch=$ARCH)"
cmake -S "$CONSUMER_DIR" -B "$CONSUMER_DIR/build" -G "Unix Makefiles" \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES="$ARCH" \
  -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOS_DEPLOYMENT_TARGET"
cmake --build "$CONSUMER_DIR/build" --config Release -j"$(sysctl -n hw.ncpu)"

COMPONENT="$CONSUMER_DIR/build/${AU_OUTPUT_NAME}.component"
[ -d "$COMPONENT" ] || { echo "ERROR: $COMPONENT not built"; exit 1; }

echo "==> 4/5 Ad-hoc codesigning + installing"
codesign -s - --force --deep "$COMPONENT/Contents/PlugIns/${BUNDLE}.clap" 2>/dev/null || true
codesign -s - --force --deep "$COMPONENT"
mkdir -p "$COMPONENTS_DIR"
rm -rf "$COMPONENTS_DIR/${AU_OUTPUT_NAME}.component"
cp -R "$COMPONENT" "$COMPONENTS_DIR/"
echo "    installed -> $COMPONENTS_DIR/${AU_OUTPUT_NAME}.component"

echo "==> 5/5 Validating"
killall -9 AudioComponentRegistrar 2>/dev/null || true
# IMPORTANT: when clap-wrapper embeds a PREBUILT .clap (the
# MACOSX_EMBEDDED_CLAP_LOCATION path used here), it DERIVES the AU subtype from
# the CLAP id and IGNORES $AU_SUBTYPE_CODE — that code is only honored in
# clap-wrapper's separate "--explicit" path (which builds the clap as a CMake
# target instead of embedding a prebuilt one). The derived code is deterministic
# from the CLAP id (com.joeshipley.drumlin -> 'KV6m'). So we read the REAL
# subtype back from the installed component and validate against it. The
# manufacturer code IS honored ($AU_MANUF_CODE = JShp).
PLIST="$COMPONENTS_DIR/${AU_OUTPUT_NAME}.component/Contents/Info.plist"
ACTUAL_SUBTYPE="$(plutil -extract 'AudioComponents.0.subtype' raw -o - "$PLIST")"
echo "    intended subtype '$AU_SUBTYPE_CODE' (informational); clap-wrapper derived '$ACTUAL_SUBTYPE' from the CLAP id"
echo "    auval -v $AU_INSTRUMENT_TYPE $ACTUAL_SUBTYPE $AU_MANUF_CODE"
auval -v "$AU_INSTRUMENT_TYPE" "$ACTUAL_SUBTYPE" "$AU_MANUF_CODE"
