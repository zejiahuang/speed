#!/usr/bin/env bash
# Build the kernel as an optimised shared library for the emulator's ABI.
#
# Release, not debug, because this is the build the throughput numbers come from.
# An unoptimised relay loop is a plausible cause of a slow tunnel all by itself,
# and shipping one while measuring would make it impossible to tell "the loop
# reads one chunk per tick" from "the loop is compiled without optimisation".
set -euo pipefail

cd /mnt/d/4
export PROFILE=release
exec bash scripts/app-native-build.sh x86_64-linux-android
