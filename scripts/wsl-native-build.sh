#!/usr/bin/env bash
# Build the Android native library for the emulator's ABI, inside WSL.
#
# A file rather than a one-liner because the command crosses wsl.exe, which
# strips `$` from the command line. See scripts/run-in-wsl.sh.
set -euo pipefail

cd /mnt/d/4
exec bash scripts/app-native-build.sh x86_64-linux-android
