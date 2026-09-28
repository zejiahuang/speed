#!/usr/bin/env bash
# Build the debug APK inside WSL, reusing the native library already built.
#
# SKIP_NATIVE=1 because scripts/wsl-native-build.sh has just run; rebuilding it
# here would only duplicate work and race the cargo target directory.
set -euo pipefail

cd /mnt/d/4
export SKIP_NATIVE=1
exec bash scripts/app-build.sh assembleDebug
