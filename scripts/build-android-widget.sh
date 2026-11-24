#!/usr/bin/env bash
# Builds the Rhythm APK with the home-screen widget included.
#
# Requires a recent cargo-apk2 (1.2+) with Kotlin and manifest_override support:
#   cargo install cargo-apk2 --force
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo-apk2 >/dev/null 2>&1 && ! cargo apk2 --version >/dev/null 2>&1; then
    echo "cargo-apk2 is required. Install it with: cargo install cargo-apk2 --force" >&2
    exit 1
fi

# cargo-apk2 expects $KOTLIN_HOME/bin/kotlinc and $KOTLIN_HOME/lib/kotlin-stdlib.jar.
# Some distros install kotlinc to /usr/bin but don't set a matching KOTLIN_HOME.
if [[ ! -x "${KOTLIN_HOME:-}/bin/kotlinc" || ! -f "${KOTLIN_HOME:-}/lib/kotlin-stdlib.jar" ]]; then
    command -v kotlinc >/dev/null 2>&1 || {
        echo "kotlinc is required to build the widget." >&2
        exit 1
    }
    STDLIB_JAR="${KOTLIN_STDLIB_JAR:-}"
    if [[ -z "$STDLIB_JAR" ]]; then
        STDLIB_JAR="$(find /usr/share /usr/local "${KOTLIN_HOME:-/nonexistent}" \
            2>/dev/null -name kotlin-stdlib.jar -print | head -n 1)"
    fi
    [[ -n "$STDLIB_JAR" ]] || {
        echo "Could not find kotlin-stdlib.jar; set KOTLIN_STDLIB_JAR." >&2
        exit 1
    }

    SHIM="${XDG_DATA_HOME:-$HOME/.local/share}/rhythm-kotlin-home"
    mkdir -p "$SHIM/bin" "$SHIM/lib"
    ln -sf "$(command -v kotlinc)" "$SHIM/bin/kotlinc"
    ln -sf "$STDLIB_JAR" "$SHIM/lib/kotlin-stdlib.jar"
    export KOTLIN_HOME="$SHIM"
    echo "==> Using Kotlin shim at $KOTLIN_HOME"
fi

# cargo-apk2 requires an explicit release signing key. Default to the Android
# debug keystore; override these variables for a real release keystore.
export CARGO_APK_RELEASE_KEYSTORE="${CARGO_APK_RELEASE_KEYSTORE:-${ANDROID_USER_HOME:-$HOME/.android}/debug.keystore}"
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:-android}"

echo "==> Building APK with cargo-apk2 (widget manifest + Kotlin sources)"
# This crate has both a cdylib lib and a bin; apk2 must package the cdylib.
cargo apk2 build --lib "$@"

echo "The APK was written under target/android-artifacts/. Install it with:"
echo "  adb install -r <path-to-apk>"
