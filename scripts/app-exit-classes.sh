#!/usr/bin/env bash
# Classify failed domains by *why* curl gave up, not just that it did.
#
# A sweep records `000` for every domain curl could not turn into an HTTP status,
# and report code that stops there calls all of them "blocked". They are not the
# same thing:
#
#   exit 60  the certificate was rejected. The address answered, the handshake
#            ran, both directions carried bytes, and the client refused the
#            certificate because it does not cover the name. Over a tunnel this
#            is the structural limit: the client sent its own SNI and a
#            non-terminating relay cannot change it.
#   exit 28  the request timed out. Nothing usable came back in budget. This is
#            the only bucket that means "could not reach it".
#   exit 7   TCP connected and the host refused the connection.
#
# The distinction decides what to do next. A pile of exit-60 is a rule-quality
# and SNI problem that no amount of kernel work fixes; a pile of exit-28 on
# domains with hundreds of addresses is a candidate-selection problem inside the
# kernel. Reading them as one number points at the wrong file.
#
# Usage:
#   bash scripts/app-exit-classes.sh <domains-file> [mode]
#     domains-file  one domain per line (or a sweep TSV; column 1 is used)
#     mode          tunnel (default) | direct
set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG=dev.detour
WORK="${WORK:-tmp}"
LIST="${1:?usage: app-exit-classes.sh <domains-file> [tunnel|direct]}"
MODE="${2:-tunnel}"
TIMEOUT="${TIMEOUT:-12}"
JOBS="${JOBS:-4}"

export MSYS_NO_PATHCONV=1
ADB_ARGS=(-s "$DEVICE")

say() { printf '%s\n' "$*"; }

to_win() {
  local path="$1" abs
  abs="$(cd "$(dirname "$path")" 2>/dev/null && pwd)/$(basename "$path")"
  case "$abs" in
    /[a-zA-Z]/*) printf '%s' "$(echo "${abs:1:1}" | tr 'a-z' 'A-Z'):${abs:2}" ;;
    *) printf '%s' "$abs" ;;
  esac
}

# --- put the device where the caller asked ----------------------------------
if [ "$MODE" = "direct" ]; then
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd disconnect" >/dev/null 2>&1
  say "exit-classes: tunnel DOWN (direct mode)"
else
  "$ADB" "${ADB_ARGS[@]}" shell "am broadcast -a dev.detour.CONTROL -p $PKG --es cmd connect" >/dev/null 2>&1
  say "exit-classes: tunnel UP"
fi
sleep 4

# --- normalise the input to one domain per line -----------------------------
grep -v '^#' "$LIST" | awk -F'\t' 'NF>0 && $1!="" {print $1}' | sort -u > "$WORK/exit-domains.txt"
N="$(wc -l < "$WORK/exit-domains.txt" | tr -d ' ')"
say "exit-classes: $N domains, $JOBS at a time, ${TIMEOUT}s cap"

# --- device-side runner -----------------------------------------------------
#
# `%{exitcode}` is what curl actually exited with, and `-w` prints it even when
# the transfer failed, so one pass yields both the status and the reason.
cat > "$WORK/exit-runner.sh" <<'RUNNER'
#!/system/bin/sh
# $1 = list file, $2 = timeout, $3 = jobs
list="$1"
timeout_s="$2"
jobs="$3"
CMD='d="$1"; out="$(curl -sS -o /dev/null -w "%{http_code} %{exitcode} %{time_total}" --max-time "'"$timeout_s"'" "https://$d/" 2>/dev/null)"; rc=$?; [ -z "$out" ] && out="000 ${rc} 0"; printf "%s %s\n" "$d" "$out"'
xargs -P "$jobs" -n 1 sh -c "$CMD" _ <"$list"
RUNNER

MSYS_NO_PATHCONV=1 "$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/exit-domains.txt")" /data/local/tmp/watt/exit-domains.txt >/dev/null 2>&1
MSYS_NO_PATHCONV=1 "$ADB" "${ADB_ARGS[@]}" push "$(to_win "$WORK/exit-runner.sh")" /data/local/tmp/watt/exit-runner.sh >/dev/null 2>&1
"$ADB" "${ADB_ARGS[@]}" shell 'chmod 755 /data/local/tmp/watt/exit-runner.sh' >/dev/null 2>&1

"$ADB" "${ADB_ARGS[@]}" shell \
  "sh /data/local/tmp/watt/exit-runner.sh /data/local/tmp/watt/exit-domains.txt $TIMEOUT $JOBS" \
  2>/dev/null | tr -d '\r' > "$WORK/exit-classes.txt"

# --- tally ------------------------------------------------------------------
TOTAL="$(grep -c . "$WORK/exit-classes.txt" 2>/dev/null || echo 0)"
say "exit-classes: $TOTAL results"

say ""
say "  exit  meaning                              count"
say "  ----  -----------------------------------  -----"
awk '{
  # domain code exit secs
  rc = $3
  code = $2
  if (code != "000") { key = "http:" code; label = "HTTP " code " (reached)" }
  else if (rc == "60") { key = "60"; label = "certificate rejected" }
  else if (rc == "28") { key = "28"; label = "timed out" }
  else if (rc == "7")  { key = "7";  label = "connection refused" }
  else if (rc == "35") { key = "35"; label = "TLS handshake failed" }
  else if (rc == "6")  { key = "6";  label = "name not resolved" }
  else { key = "other:" rc; label = "other (exit " rc ")" }
  n[key]++; L[key] = label
} END {
  for (k in n) printf "%6s  %-37s  %5d\n", k, L[k], n[k]
}' "$WORK/exit-classes.txt" | sort -k3 -rn

say ""
say "exit-classes: full data in $WORK/exit-classes.txt (domain code exit secs)"
