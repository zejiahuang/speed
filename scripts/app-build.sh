#!/usr/bin/env bash
#
# Build the Android app.
#
#   bash scripts/app-build.sh [gradle tasks...]     (default: assembleDebug)
#
# Gradle is fetched on first run rather than committed as a wrapper jar: a binary
# blob in the repository that nothing here can regenerate is worse than a
# download. The distribution lands under ~/.cache and is reused afterwards.
#
# The native library has to exist first — the app loads it at startup and reports
# its absence rather than crashing, but a build without it produces an APK that
# can only say "the kernel is unavailable".
#
# Environment:
#   ANDROID_HOME   SDK location      (default: ~/android-sdk)
#   GRADLE_VERSION distribution      (default: 8.14.3)
#   SKIP_NATIVE    1 to reuse the existing .so

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$REPO_DIR/android"
ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
GRADLE_VERSION="${GRADLE_VERSION:-8.14.3}"
GRADLE_HOME="$HOME/.cache/gradle-$GRADLE_VERSION"

if [ ! -d "$ANDROID_HOME/platforms" ]; then
  echo "app-build: no SDK at $ANDROID_HOME — install the Android SDK (platform 36 +" >&2
  echo "app-build: build-tools 36.0.0) and point ANDROID_HOME at it" >&2
  exit 1
fi

# --- native -----------------------------------------------------------------
if [ "${SKIP_NATIVE:-0}" != "1" ]; then
  echo "app-build: building libwatt_ffi.so"
  bash "$REPO_DIR/scripts/app-native-build.sh" x86_64-linux-android || exit 1
fi

# --- gradle -----------------------------------------------------------------
if [ ! -x "$GRADLE_HOME/bin/gradle" ]; then
  echo "app-build: fetching Gradle $GRADLE_VERSION"
  mkdir -p "$HOME/.cache"
  tmp="$(mktemp -d)"
  url="https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip"
  curl -fsSL -o "$tmp/gradle.zip" "$url" || {
    echo "app-build: could not download $url" >&2
    exit 1
  }
  unzip -q "$tmp/gradle.zip" -d "$HOME/.cache"
  rm -rf "$tmp"
fi

if [ $# -eq 0 ]; then
  set -- assembleDebug
fi

export ANDROID_HOME ANDROID_SDK_ROOT="$ANDROID_HOME"
export JAVA_HOME="${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
export PATH="$GRADLE_HOME/bin:$ANDROID_HOME/platform-tools:$PATH"

echo "app-build: JAVA_HOME=$JAVA_HOME"
echo "app-build: ANDROID_HOME=$ANDROID_HOME"
echo "app-build: tasks=$*"

cd "$ANDROID_DIR" || exit 1
exec gradle --no-daemon --console=plain "$@"
