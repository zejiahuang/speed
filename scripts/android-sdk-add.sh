#!/usr/bin/env bash
#
# Install extra SDK packages after the fact.
#
#   bash scripts/android-sdk-add.sh "platforms;android-37" "build-tools;37.0.0"
#
# Split out from android-sdk-setup.sh because the packages a project needs change
# as its plugins do: Compose BOM 2026.09 wants compileSdk 37, which did not exist
# when the first set was chosen.

set -uo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"

if [ ! -x "$SDKMANAGER" ]; then
  echo "android-sdk-add: no sdkmanager at $SDKMANAGER" >&2
  exit 1
fi

if [ $# -eq 0 ]; then
  echo "usage: android-sdk-add.sh <package>...   |   android-sdk-add.sh --list" >&2
  exit 2
fi

if [ "$1" = "--list" ]; then
  # What the repository actually offers. Worth having as a mode of its own: the
  # answer is not guessable, and asking for a platform that does not exist fails
  # with "Failed to find package" rather than telling you the ceiling.
  "$SDKMANAGER" --sdk_root="$ANDROID_HOME" --list 2>/dev/null \
    | grep -E "^\s+(platforms;android-[0-9]+|build-tools;[0-9.]+)\s" \
    | sort -u
  exit 0
fi

echo "android-sdk-add: installing $*"
yes | "$SDKMANAGER" --sdk_root="$ANDROID_HOME" --licenses >/dev/null 2>&1
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" --install "$@" 2>&1 | tail -3

echo "android-sdk-add: platforms now:"
ls "$ANDROID_HOME/platforms" 2>/dev/null
echo "android-sdk-add: build-tools now:"
ls "$ANDROID_HOME/build-tools" 2>/dev/null
