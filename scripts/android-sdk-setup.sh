#!/usr/bin/env bash
#
# One-time Android build environment for WSL.
#
# Installs a JDK, the Android command-line tools, and the SDK packages a Compose
# app needs. Everything lands under $ANDROID_HOME (~/android-sdk) and the JDK
# comes from apt, because Gradle needs a full JDK and the distro one is the
# least surprising place to get it.
#
#   bash scripts/android-sdk-setup.sh
#
# Idempotent: re-running it skips what is already there. Needs sudo for apt.
#
# Environment:
#   ANDROID_HOME   where the SDK goes        (default: $HOME/android-sdk)
#   CMDLINE_TOOLS  version of the tools zip  (default: 11076708, the 2024-04 r5)

set -uo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
CMDLINE_TOOLS="${CMDLINE_TOOLS:-11076708}"
TOOLS_ZIP="commandlinetools-linux-${CMDLINE_TOOLS}_latest.zip"
TOOLS_URL="https://dl.google.com/android/repository/${TOOLS_ZIP}"

# The platform the app compiles against. Material 3 Expressive lives in
# Compose BOM 2024.09+ and needs compileSdk 35; 34 is the floor for the
# adaptive APIs the layout uses.
PLATFORM="android-35"
BUILD_TOOLS="35.0.0"

say() { printf '\n=== %s\n' "$*"; }

# --- 1. JDK ----------------------------------------------------------------
#
# Gradle 8.x needs 17. `openjdk-17-jdk-headless` is enough — there is no display
# in WSL and the Android Gradle plugin does not need one.
if command -v javac >/dev/null 2>&1; then
  say "JDK already present: $(javac -version 2>&1)"
else
  say "installing the JDK (needs sudo)"
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openjdk-17-jdk-headless unzip
fi

# --- 2. Command-line tools -------------------------------------------------
#
# `sdkmanager` is the only thing downloaded by hand; it installs everything else.
if [ -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" ]; then
  say "command-line tools already present"
else
  say "downloading the command-line tools"
  mkdir -p "$ANDROID_HOME/cmdline-tools"
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/$TOOLS_ZIP" "$TOOLS_URL" || {
    echo "android-sdk-setup: could not download $TOOLS_URL" >&2
    exit 1
  }
  unzip -q "$tmp/$TOOLS_ZIP" -d "$tmp"
  # The zip unpacks to `cmdline-tools/`, but `sdkmanager` only looks for its
  # own files under a directory literally named `latest`. Renaming is required,
  # not cosmetic.
  rm -rf "$ANDROID_HOME/cmdline-tools/latest"
  mv "$tmp/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
  rm -rf "$tmp"
fi

SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"

# --- 3. SDK packages -------------------------------------------------------
say "installing platform-tools, $PLATFORM, build-tools $BUILD_TOOLS"
# The licences have to be accepted before anything installs, and `sdkmanager`
# reads them from stdin.
yes | "$SDKMANAGER" --sdk_root="$ANDROID_HOME" --licenses >/dev/null 2>&1
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" --install \
  "platform-tools" "platforms;$PLATFORM" "build-tools;$BUILD_TOOLS" 2>&1 | tail -3

# --- 4. Report -------------------------------------------------------------
#
# Printed rather than written into a profile: this script is run by hand, and a
# silent edit of ~/.bashrc is the kind of thing that surprises people later.
say "done"
cat <<EOF
Add these to your shell (or export them before building):

  export ANDROID_HOME="$ANDROID_HOME"
  export ANDROID_SDK_ROOT="\$ANDROID_HOME"
  export PATH="\$ANDROID_HOME/platform-tools:\$ANDROID_HOME/cmdline-tools/latest/bin:\$PATH"

The Gradle wrapper fetches Gradle itself on first build, so nothing else is
needed here. Verify with:

  bash scripts/android-sdk-verify.sh
EOF
