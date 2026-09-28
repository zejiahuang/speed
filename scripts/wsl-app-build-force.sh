#!/usr/bin/env bash
# Build the debug APK with the configuration cache disabled.
#
# Why: with `--configuration-cache` active (Gradle 8 enables it by default for
# this project's setup), an edit made while a previous build still holds the
# entry can leave Gradle reporting every task UP-TO-DATE against a stale graph,
# so `touch` on the changed source does not move anything. The visible symptom is
# a dex directory older than the Kotlin file that supposedly produced it, which
# is how a "successful" build ships without the change. Slower, and honest.
set -euo pipefail

cd /mnt/d/4
export SKIP_NATIVE=1
exec bash scripts/app-build.sh assembleDebug --no-configuration-cache
