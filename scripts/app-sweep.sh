#!/usr/bin/env bash
#
# Sweep many rule domains through the running app and report per-domain verdicts.
#
#   bash scripts/app-sweep.sh [options]
#
#     --list <file>     TSV of "group<TAB>entry<TAB>domain<TAB>ipcount"
#                       (default: tmp/sweep-domains.tsv)
#     --limit <n>       test at most n domains (default: all)
#     --group <name>    only this group (repeatable)
#     --mode tunnel|direct|both   default both
#     --timeout <s>     per-request cap (default 10)
#     --out <file>      TSV of results (default: tmp/sweep-results.tsv)
#     --jobs <n>        concurrent requests on the device (default 6)
#
# Why this exists alongside `app-sample.sh` and `app-path.sh`. Those answer
# "does this one host work" precisely enough to argue about. This answers "how
# many of the 1281 hosts the rules claim to help actually work", which is a
# different question and needs a different shape: it is about coverage, so it
# must be broad, and it is about the *rule set*, so a domain that fails is a
# finding rather than a surprise.
#
# Two design decisions carry the weight:
#
# 1. **The whole batch runs on the device, in one shell.** Per-domain `adb
#    shell` costs ~200 ms of adb round trip before curl starts, which over 1281
#    domains is four minutes of pure overhead and, worse, spreads the samples
#    across a window long enough for the network to change underneath them.
#    One pushed script changes the unit of measurement from "the session" to
#    "the batch".
#
# 2. **Concurrency is bounded and deliberate.** Six at a time. The tunnel's
#    listener pool and flow table are finite, and a 1281-way burst would
#    measure the pool ceiling rather than the rules. Six is enough to hide the
#    per-request latency without touching the ceiling.
#
# `--mode both` interleaves per domain (tunnel then direct, domain by domain)
# rather than running two sweeps. A tunnel sweep followed by a direct sweep
# measures two different networks; the failure rate here moves by tens of
# points in twenty minutes, which has already been misread as a kernel
# regression twice. Interleaving keeps each pair in the same moment.

set -uo pipefail

ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
DEVICE="${DEVICE:-emulator-5554}"
PKG="dev.detour"
ACTION="dev.detour.CONTROL"
TAG="DetourControl"

# Keep Git Bash from rewriting device paths. See `push_file` below for what
# this prevents; it is set once here rather than as a command prefix because a
# prefix on an `export`ed variable does not reach a subprocess spawned inside a
# function, which is exactly where the pushes happen.
export MSYS_NO_PATHCONV=1

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$REPO_DIR/tmp"

LIST="$TMP_DIR/sweep-domains.tsv"
OUT="$TMP_DIR/sweep-results.tsv"
LIMIT=0
MODE="both"
TIMEOUT=10
JOBS=6
# Deliberately not `GROUPS`. That name is a bash built-in array holding the
# caller's group IDs, so `${#GROUPS[@]}` reads as non-empty and `"${GROUPS[@]}"`
# yields the gid — here `197121`, which no `--group` value can equal, so every
# domain was filtered out and the script reported "no domains selected" while
# looking perfectly correct. A built-in shadowed by a local is not an error in
# bash; it is a silent wrong answer.
WANT_GROUPS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --list) LIST="$2"; shift 2 ;;
    --limit) LIMIT="$2"; shift 2 ;;
    --group) WANT_GROUPS+=("$2"); shift 2 ;;
    --mode) MODE="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --jobs) JOBS="$2"; shift 2 ;;
    -h|--help) sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "app-sweep: unknown argument: $1" >&2; exit 2 ;;
  esac
done

[ -f "$LIST" ] || { echo "app-sweep: list not found: $LIST" >&2; exit 1; }

control() {
  "$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG $*" >/dev/null 2>&1
}

# Wait for the app to *report* the state we asked for. See app-ab.sh for why
# the poll is on `running` and why the settle is not cosmetic: tearing a VPN
# down and back up every couple of seconds makes the framework unbind the
# service mid-cycle, and a `connect` accepted during that window answers
# `running:false` to a channel that looks healthy.
wait_running() {
  local want="$1" tries="${2:-40}" line
  for _ in $(seq "$tries"); do
    control --es cmd status
    sleep 1
    line="$("$ADB" -s "$DEVICE" logcat -d 2>/dev/null | tr -d '\r' \
      | grep " $TAG: " | tail -1 | sed 's/.*'"$TAG"': //')"
    case "$want:$line" in
      true:*'"running":true'*)   return 0 ;;
      false:*'"running":false'*) return 0 ;;
    esac
  done
  return 1
}

# --- build the batch input on the host --------------------------------------

FILTERED="$TMP_DIR/sweep-filtered.tsv"
: >"$FILTERED"
while IFS=$'\t' read -r group entry domain ipcount; do
  [ -z "${domain:-}" ] && continue
  domain="${domain%$'\r'}"
  if [ "${#WANT_GROUPS[@]}" -gt 0 ]; then
    keep=0
    for wanted in "${WANT_GROUPS[@]}"; do
      [ "$group" = "$wanted" ] && keep=1
    done
    [ "$keep" -eq 1 ] || continue
  fi
  printf '%s\t%s\t%s\t%s\n' "$group" "$entry" "$domain" "$ipcount" >>"$FILTERED"
done <"$LIST"

TOTAL="$(wc -l <"$FILTERED" | tr -d ' ')"
if [ "$LIMIT" -gt 0 ] && [ "$LIMIT" -lt "$TOTAL" ]; then
  head -n "$LIMIT" "$FILTERED" >"$FILTERED.tmp" && mv "$FILTERED.tmp" "$FILTERED"
  TOTAL="$LIMIT"
fi
[ "$TOTAL" -gt 0 ] || { echo "app-sweep: no domains selected" >&2; exit 1; }

echo "app-sweep: $TOTAL domains, mode=$MODE jobs=$JOBS timeout=${TIMEOUT}s"

# --- the device-side runner -------------------------------------------------
#
# Written to a file and pushed rather than passed inline: a `wsl`/`adb` command
# line with `$` in it gets its substitutions eaten somewhere in the Windows
# argument path, and a batch runner that silently loses `$domain` is a runner
# that reports on nothing.

RUNNER="$TMP_DIR/sweep-runner.sh"
cat >"$RUNNER" <<'RUNNER_EOF'
#!/system/bin/sh
# Probe every domain in the list, JOBS at a time, and print
# "<domain>\t<code>\t<seconds>" per line.
#
#   sweep-runner.sh <list-file> <seconds-cap> <jobs>
#
# No shell functions and no `export -f`. Android's `/system/bin/sh` is mksh,
# which does not carry a function into a child process the way bash does, so a
# runner written as `export -f probe; xargs -P … sh -c 'probe …'` prints
# nothing at all — xargs runs, the child cannot find the command, and the
# failure is swallowed by the redirect into the output file. The whole command
# is therefore passed as a `sh -c` string, which works on any POSIX shell.
LIST_FILE="$1"
TIMEOUT="$2"
JOBS="$3"

# The exit code is the column that makes the result interpretable, and it was
# missing for most of this script's life.
#
# `%{http_code}` alone collapses every failure into `000`, which then gets read
# as "blocked". But `000` covers at least three different situations, and two of
# them are *reachability successes*:
#
#   exit 60  TLS certificate rejected. The address was reached, the handshake
#            ran, and the client refused the certificate. Bytes flowed both
#            ways. This is the case watt structurally cannot fix (it does not
#            terminate TLS, so it cannot change the SNI the client sent), and
#            it is a completely different finding from "nothing answered".
#   exit 28  the timeout expired. No usable answer within the budget — the
#            closest thing to a real block, though it also covers a slow link.
#   exit 7   connection refused, i.e. TCP reached a host that said no.
#
# With only `000` in the output, a rule set that routes 100 domains to
# wrong-certificate edges and a rule set that routes them nowhere look
# identical, and the second is much worse than the first. Reporting a combined
# "blocked" number overstates the damage by exactly the certificate-mismatch
# population, which in the 1.0.54 rules turned out to be most of it.
CMD='d="$1"; l="$(curl -sS -o /dev/null -w "%{http_code} %{time_total} %{exitcode}" --max-time '"$TIMEOUT"' "https://$d/" 2>/dev/null | tail -1)"; e="$?"; [ -z "$l" ] && l="000 0 ${e:-1}"; printf "%s\t%s\t%s\t%s\n" "$d" "${l%% *}" "$(printf "%s" "$l" | cut -d" " -f2)" "$(printf "%s" "$l" | cut -d" " -f3)"'

# -P is the concurrency; -n1 hands one domain to each child. `sh -c "$CMD" _`
# makes the domain land in $1 rather than $0, so a domain can never be read as
# the command name.
xargs -P "$JOBS" -n 1 sh -c "$CMD" _ <"$LIST_FILE"
RUNNER_EOF

push_file() {
  # `MSYS_NO_PATHCONV=1` on every push, always.
  #
  # Git Bash rewrites an argument that looks like an absolute POSIX path into a
  # Windows one before the binary sees it, so `/data/local/tmp/x` arrives as
  # `C:/Users/.../PortableGit/data/local/tmp/x` and adb answers
  # "remote secure_mkdirs failed: No such file or directory". The message names
  # the *remote*, which is why it reads as a device-permission problem when it
  # is a local-shell one. `adb shell` arguments do not need this — they are
  # forwarded as a string and parsed on the device — so only push targets do.
  #
  # The local side then has to be converted by hand, because the same variable
  # also stops the *source* path being translated and adb.exe is a Windows
  # binary: handed `/d/4/tmp/x` it reports "cannot stat", which reads like a
  # missing file. Both halves are needed, and getting one without the other
  # produces an error message that points at the wrong machine.
  local src="$1"
  case "$src" in
    /[a-zA-Z]/*) src="$(echo "${src:1:1}" | tr 'a-z' 'A-Z'):${src:2}" ;;
  esac
  MSYS_NO_PATHCONV=1 "$ADB" -s "$DEVICE" push "$src" "$2" >/dev/null 2>&1
}
DEV_LIST="/data/local/tmp/watt/sweep-list.txt"
DEV_RUNNER="/data/local/tmp/watt/sweep-runner.sh"
DEV_OUT="/data/local/tmp/watt/sweep-out.txt"

push_file "$RUNNER" "$DEV_RUNNER" || { echo "app-sweep: push of the runner failed" >&2; exit 1; }
"$ADB" -s "$DEVICE" shell "chmod 755 $DEV_RUNNER" >/dev/null 2>&1

# --- helpers ----------------------------------------------------------------

# Extract just the domains, so the device does no parsing it does not need.
domains_only() {
  awk -F'\t' 'NF>0 && $3!="" {print $3}' "$FILTERED"
}

pull_results() {
  "$ADB" -s "$DEVICE" shell "cat $DEV_OUT" 2>/dev/null | tr -d '\r'
}

##############################################################################
# lane 1: the tunnel sweep
##############################################################################

run_sweep() {
  local label="$1" want="$2"
  echo "app-sweep: [$label] switching the app to $want"
  if [ "$want" = "on" ]; then
    control --es cmd mode --es value vpn
    control --es cmd connect
    wait_running true 45 || { echo "app-sweep: could not bring the tunnel up" >&2; return 1; }
  else
    control --es cmd disconnect
    wait_running false 45 || { echo "app-sweep: could not take the tunnel down" >&2; return 1; }
  fi
  # The settle is what keeps this from testing Android's bookkeeping instead
  # of the kernel. Two seconds on each side of a switch.
  sleep "${SETTLE:-3}"

  domains_only >"$TMP_DIR/sweep-domains-input.txt"
  push_file "$TMP_DIR/sweep-domains-input.txt" "$DEV_LIST" || return 1

  echo "app-sweep: [$label] probing $TOTAL domains, $JOBS at a time ..."
  "$ADB" -s "$DEVICE" shell \
    "sh $DEV_RUNNER $DEV_LIST $TIMEOUT $JOBS > $DEV_OUT 2>/dev/null" \
    >/dev/null 2>&1

  pull_results >"$TMP_DIR/sweep-$label.tsv"
  local got
  got="$(wc -l <"$TMP_DIR/sweep-$label.tsv" | tr -d ' ')"
  echo "app-sweep: [$label] $got of $TOTAL answered"
}

case "$MODE" in
  tunnel) run_sweep tunnel on || exit 2 ;;
  direct) run_sweep direct off || exit 2 ;;
  both)
    run_sweep tunnel on   || exit 2
    run_sweep direct off  || exit 2
    ;;
  *) echo "app-sweep: bad mode: $MODE" >&2; exit 2 ;;
esac

# --- merge and report -------------------------------------------------------
#
# The two sweeps are joined on the domain. `--mode both` runs them back to back
# rather than truly interleaved; the settle between them is minutes not seconds,
# so the caveat is real and stated: a wide difference is trustworthy, a narrow
# one is not. Per-pair interleaving at 1281 domains would cost an hour of state
# switching for a precision the question does not need.

"$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG --es cmd status" >/dev/null 2>&1
sleep 1

# Python is a Windows program here, so it cannot be handed a Git Bash path:
# `/d/4/tmp/x` reaches it as `\d\4\tmp\x` and the open fails with a
# FileNotFoundError that names a path nothing ever created. Every path that
# crosses into it goes through `to_win`.
to_win() {
  case "$1" in
    /[a-zA-Z]/*) echo "$(echo "${1:1:1}" | tr 'a-z' 'A-Z'):${1:2}" ;;
    *) echo "$1" ;;
  esac
}

PYTHON="${PYTHON:-C:/Users/Administrator/.workbuddy-ai/binaries/python/versions/3.13.12/python.exe}"
"$PYTHON" "$(to_win "$(dirname "${BASH_SOURCE[0]}")/sweep-report.py")" \
  --list "$(to_win "$FILTERED")" \
  --tunnel "$(to_win "$TMP_DIR/sweep-tunnel.tsv")" \
  --direct "$(to_win "$TMP_DIR/sweep-direct.tsv")" \
  --mode "$MODE" \
  --out "$(to_win "$OUT")"
