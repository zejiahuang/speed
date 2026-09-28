#!/usr/bin/env bash
# Invoke a command inside the WSL distro.
#
# This exists because passing a command line through `wsl.exe -- bash -lc "..."`
# from Git Bash mangles it: `$` is eaten by the Windows argument layer, so
# `$f` arrives empty and `'...'` arrives truncated. Any command with a dollar
# sign must be written to a file first and invoked as a file.
#
# Usage: bash scripts/run-in-wsl.sh <command...>
set -euo pipefail

DISTRO="${WATT_WSL_DISTRO:-Ubuntu-22.04}"

exec wsl.exe -d "$DISTRO" -- bash -lc "$*"
