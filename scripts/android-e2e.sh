#!/usr/bin/env bash
#
# Run the kernel on a real Android device or emulator and report what happens.
#
# This is the test the WSL harness cannot do: it proves the Rust core actually
# runs on Android — creating a TUN there, matching the real rule document, and
# carrying real HTTPS with real certificate verification.
#
# Android needs its own setup, which is why the device side is a separate file
# (`android-e2e-device.sh`): its routing rules have no `main` table lookup, so
# tunnel routes must go into a table reached by a rule of their own.
#
#   scripts/android-e2e.sh
#
# Environment:
#   ADB          path to adb                    (default: auto-detected)
#   DEVICE       adb device selector            (default: -s 127.0.0.1:5555)
#   SITES_FILE   "<domain> <address>" lines     (default: tmp/rule-sites.txt)
#   SKIP_BUILD   1 to reuse the existing binary
#   ABI          Android target ABI             (default: x86_64-linux-android)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# adb and python are Windows programs here, and neither understands a Git Bash
# path like /d/4. Everything they are handed has to be rewritten to D:/4 first,
# or the failure looks like a missing file rather than a wrong path.
to_win() {
  case "$1" in
    /[a-z]/*) printf '%s:%s' "$(printf '%s' "${1:1:1}" | tr '[:lower:]' '[:upper:]')" "${1:2}" ;;
    *) printf '%s' "$1" ;;
  esac
}

WIN_REPO="$(to_win "$REPO_DIR")"
SCRIPTS_DIR="$REPO_DIR/scripts"
WIN_SCRIPTS="$WIN_REPO/scripts"
TMP_DIR="$REPO_DIR/tmp"
WIN_TMP="$WIN_REPO/tmp"

ADB="${ADB:-}"
DEVICE="${DEVICE:--s 127.0.0.1:5555}"
SITES_FILE="${SITES_FILE:-$TMP_DIR/rule-sites.txt}"
SKIP_BUILD="${SKIP_BUILD:-0}"
ABI="${ABI:-x86_64-linux-android}"

if [ -z "$ADB" ]; then
  for candidate in \
    "/c/Users/Administrator/Desktop/platform-tools/adb.exe" \
    "/d/program/LDPlayer14/adb.exe"; do
    [ -x "$candidate" ] && ADB="$candidate" && break
  done
fi
[ -n "$ADB" ] || { echo "android-e2e: no adb found; set ADB" >&2; exit 1; }
[ -f "$SITES_FILE" ] || { echo "android-e2e: no sites file: $SITES_FILE" >&2; exit 1; }

WORK="$TMP_DIR/android"
# Python here is a Windows program, so it cannot be handed a Git Bash path:
# `/d/4/tmp/...` reaches it as `\d\4\tmp\...` and the file is reported missing.
# Every path crossing into Python has to be Windows-form first.
WIN_WORK="$WIN_TMP/android"
rm -rf "$WORK"
mkdir -p "$WORK"

# --- 1. Build for the device. ------------------------------------------------
if [ "$SKIP_BUILD" != "1" ]; then
  echo "android-e2e: building for $ABI"
  wsl.exe -d Ubuntu-22.04 -- bash /mnt/d/4/scripts/android-build.sh build -p watt-daemon \
    >"$WORK/build.log" 2>&1 || { cat "$WORK/build.log" >&2; exit 1; }
fi
BINARY="$(wsl.exe -d Ubuntu-22.04 -- bash -c \
  'echo "$HOME/.cache/watt-target/x86_64-linux-android/debug/watt-daemon"' | tr -d '\r')"

# --- 2. Build one DNS query per site, on the host. ---------------------------
#
# The device's curl has no --dns-servers and there is no dig, so the query is
# constructed here and fired with netcat; the reply comes back to be parsed.
python3 - "$(to_win "$SITES_FILE")" "$WIN_SCRIPTS" "$WIN_WORK" <<'PY'
import sys, pathlib
sites_path, scripts_dir, work = sys.argv[1:4]
sys.path.insert(0, scripts_dir)
from tun_smoke_client import build_query  # noqa: E402

count = 0
for line in pathlib.Path(sites_path).read_text(encoding="utf-8").splitlines():
    parts = line.split()
    if not parts:
        continue
    domain = parts[0]
    pathlib.Path(f"{work}/query-{domain}.bin").write_bytes(build_query(domain, 0x5741))
    count += 1
print(f"android-e2e: built {count} DNS queries")
PY

# --- 3. Push everything the device needs. ------------------------------------
echo "android-e2e: pushing to the device"
"$ADB" $DEVICE shell "mkdir -p /data/local/tmp/watt" >/dev/null 2>&1
"$ADB" $DEVICE push "$WIN_TMP/android/query-"*.bin /data/local/tmp/watt/ >/dev/null 2>&1
cp "$SITES_FILE" "$WORK/sites.txt"
"$ADB" $DEVICE push "$WIN_TMP/android/sites.txt" /data/local/tmp/watt/sites.txt >/dev/null 2>&1
"$ADB" $DEVICE push "$WIN_SCRIPTS/android-e2e-device.sh" /data/local/tmp/watt/ >/dev/null 2>&1
"$ADB" $DEVICE push "$WIN_TMP/rules.json" /data/local/tmp/watt/rules.json >/dev/null 2>&1
"$ADB" $DEVICE push "//wsl.localhost/Ubuntu-22.04/${BINARY#/}" /data/local/tmp/watt/watt-daemon \
  >"$WORK/push.log" 2>&1 || { cat "$WORK/push.log" >&2; exit 1; }
"$ADB" $DEVICE shell "chmod 755 /data/local/tmp/watt/watt-daemon /data/local/tmp/watt/android-e2e-device.sh"

# --- 4. Run it. ---------------------------------------------------------------
echo
# The device side creates a TUN and edits routing tables, both of which need
# root. `adb shell` gives the `shell` user, so the whole script is elevated once
# here rather than sprinkling `su` through it — a partially elevated run would
# fail halfway with a confusing message instead of at the start.
"$ADB" $DEVICE shell "su -c 'sh /data/local/tmp/watt/android-e2e-device.sh'" 2>&1 | tr -d '\r'

# --- 5. Read the DNS replies that came back. --------------------------------
echo
echo "--- leg 1 read back: what the rule set answered ---"
replies=0
while read -r domain _rest; do
  [ -z "${domain:-}" ] && continue
  "$ADB" $DEVICE pull "/data/local/tmp/watt/reply-$domain.bin" "$WIN_TMP/android/" >/dev/null 2>&1 \
    && replies=$((replies + 1))
done <"$WORK/sites.txt"
if [ "$replies" -gt 0 ]; then
  python3 "$WIN_SCRIPTS/dns_parse.py" "$WIN_TMP/android/reply-"*.bin 2>&1 |
    sed "s#.*reply-##; s#\.bin##"
else
  echo "  no DNS replies were captured"
fi

echo
echo "android-e2e: done; device artefacts left in /data/local/tmp/watt"
