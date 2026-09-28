#!/usr/bin/env bash
#
# Drive the app through every feature it has, from adb alone.
#
#   bash scripts/app-test.sh
#
# Every assertion is a statement about what the app reported, not about what it
# was asked to do. A broadcast that is accepted and then ignored looks identical
# to a successful one from the outside, so each step reads the answer back before
# claiming anything.
#
# Environment:
#   DEVICE   adb serial        (default: 127.0.0.1:5555)
#   ADB      path to adb       (default: the LDPlayer one)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEVICE="${DEVICE:-127.0.0.1:5555}"
ADB="${ADB:-/d/program/LDPlayer14/adb.exe}"
PKG="dev.detour"
ACTION="dev.detour.CONTROL"
TAG="DetourControl"

PASS=0
FAIL=0

# Every command writes one line of JSON to logcat. This reads the newest.
latest() {
  "$ADB" -s "$DEVICE" logcat -d 2>/dev/null | tr -d '\r' | grep " $TAG: " | tail -1 \
    | sed 's/.*'"$TAG"': //'
}

# Run one control command and return its answer.
control() {
  "$ADB" -s "$DEVICE" logcat -c >/dev/null 2>&1
  "$ADB" -s "$DEVICE" shell "am broadcast -a $ACTION -p $PKG $*" >/dev/null 2>&1
  sleep "${CONTROL_WAIT:-4}"
  latest
}

check() {
  local what="$1" json="$2" want="$3"
  if printf '%s' "$json" | grep -q -- "$want"; then
    echo "  PASS  $what"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $what"
    echo "        wanted: $want"
    echo "        got:    $json"
    FAIL=$((FAIL + 1))
  fi
}

section() { printf '\n== %s\n' "$*"; }

# Assert that a curl result line carries a usable HTTP status.
#
# Kept separate from `check` because the assertion is not a substring: `000` is
# a transport failure and every non-zero curl exit is one too, while any 2xx or
# 3xx is a real answer from the site.
check_reachable() {
  local what="$1" result="$2"
  local code exitcode
  code=$(printf '%s' "$result" | awk '{print $1}')
  exitcode=$(printf '%s' "$result" | awk '{print $2}')
  case "$code" in
    2*|3*)
      echo "  PASS  $what"
      PASS=$((PASS + 1))
      ;;
    *)
      echo "  FAIL  $what"
      echo "        http=$code curl-exit=$exitcode  (full: '$result')"
      FAIL=$((FAIL + 1))
      ;;
  esac
}

echo "device: $DEVICE"
"$ADB" -s "$DEVICE" wait-for-device 2>/dev/null

# --- startup ----------------------------------------------------------------

section "启动"
"$ADB" -s "$DEVICE" shell "am force-stop $PKG" >/dev/null 2>&1
"$ADB" -s "$DEVICE" shell "am start -n $PKG/.MainActivity" >/dev/null 2>&1
sleep 6

"$ADB" -s "$DEVICE" shell "ps -A | grep $PKG" >/dev/null 2>&1
if "$ADB" -s "$DEVICE" shell "ps -A | grep $PKG" 2>/dev/null | grep -q "$PKG"; then
  echo "  PASS  进程存活"
  PASS=$((PASS + 1))
else
  echo "  FAIL  进程存活"
  FAIL=$((FAIL + 1))
fi

# A crash on the first screen would show up here rather than as a mysterious
# failure three steps later.
CRASH=$("$ADB" -s "$DEVICE" logcat -d 2>/dev/null | tr -d '\r' | grep -c "AndroidRuntime.*$PKG")
check "启动无崩溃" "crashes=$CRASH" "crashes=0"

STATUS=$(control "--es cmd status")
check "内核库已加载" "$STATUS" '"kernel_loaded":true'
check "初始为未连接" "$STATUS" '"running":false'

# --- rules ------------------------------------------------------------------

section "规则"
RULES=$(control "--es cmd rules --es value count")
check "规则文档已缓存" "$RULES" '"bytes"'

# The filter is the part that had a hole: the switches used to be decorative.
section "规则开关真正生效"
BEFORE=$(printf '%s' "$RULES" | sed 's/.*"bytes":\([0-9]*\).*/\1/')
SET=$(control "--es cmd set --es key developer_view --es value true")
check "设置项可写" "$SET" '"developer_view":"true"'

# Switching a group off has to shrink the document that reaches the kernel.
DISABLED=$(control "--es cmd rules --es value disable --es key 'g:developer'")
check "可禁用分组" "$DISABLED" '"bytes"'
AFTER=$(printf '%s' "$DISABLED" | sed 's/.*"bytes":\([0-9]*\).*/\1/')
if [ -n "$BEFORE" ] && [ -n "$AFTER" ] && [ "$AFTER" -lt "$BEFORE" ]; then
  echo "  PASS  禁用分组后文档变小（$BEFORE -> $AFTER 字节）"
  PASS=$((PASS + 1))
else
  echo "  FAIL  禁用分组后文档变小（$BEFORE -> $AFTER）"
  FAIL=$((FAIL + 1))
fi

RESET=$(control "--es cmd rules --es value enable-all")
check "可全部恢复" "$RESET" '"bytes"'

# --- proxy mode -------------------------------------------------------------

section "代理模式"
MODE=$(control "--es cmd mode --es value proxy")
check "切到代理模式" "$MODE" '"mode":"proxy"'

STARTED=$(control "--es cmd connect")
check "代理已启动" "$STARTED" '"started":"proxy"'

sleep 4
STATUS=$(control "--es cmd status")
check "状态为已连接" "$STATUS" '"running":true'
check "端口已回报" "$STATUS" '"proxy_port":'

PORT=$(printf '%s' "$STATUS" | sed 's/.*"proxy_port":\([0-9]*\).*/\1/')
echo "  端口: $PORT"

# The real test: a request through the app's own proxy, out through the kernel.
#
# One site is not a test. `github.com` alone passing has twice been read as "the
# feature works" and twice been wrong: the sites a rule set covers fail for
# different reasons — a stale single address, a wildcard that does not cover the
# name, an address that answers with someone else's certificate — and a fix for
# one of those does nothing for the others. So the list is deliberately mixed.
if [ -n "$PORT" ] && [ "$PORT" != "0" ]; then
  section "代理模式连通性"
  SITES="${SITES:-github.com raw.githubusercontent.com}"
  for site in $SITES; do
    # Three attempts, because the first connection to a domain pays for the
    # certificate probe and a single sample cannot tell a broken domain from a
    # cold cache. Success is "any attempt returned 2xx/3xx", which is the
    # question being asked: can this device reach the site at all.
    best=""
    for attempt in 1 2 3; do
      R=$("$ADB" -s "$DEVICE" shell \
        "curl -s -o /dev/null --max-time 35 -x 127.0.0.1:$PORT -w '%{http_code} %{exitcode} %{time_total}' https://$site/" \
        2>/dev/null | tr -d '\r')
      code=$(printf '%s' "$R" | awk '{print $1}')
      case "$code" in
        2*|3*) best="$R"; break ;;
      esac
      [ -n "$best" ] || best="$R"
    done
    check "可访问 $site" "$best" " "
    printf '        %s -> %s\n' "$site" "$best"
  done
fi

STOPPED=$(control "--es cmd disconnect")
sleep 3
STATUS=$(control "--es cmd status")
check "断开后不再运行" "$STATUS" '"running":false'

# --- settings ---------------------------------------------------------------

section "设置"
for pair in "proxy_port 1081" "dark_mode always" "offline true" "max_candidates 8"; do
  set -- $pair
  OUT=$(control "--es cmd set --es key $1 --es value $2")
  check "可写 $1" "$OUT" "\"$1\""
done

DUMP=$(control "--es cmd dump")
check "快照可导出" "$DUMP" '"written"'
check "快照含设置" "$DUMP" '"settings"'

# --- logs -------------------------------------------------------------------

section "日志"
LOGS=$(control "--es cmd log")
check "日志可读" "$LOGS" '"entries"'

CLEARED=$(control "--es cmd log --es value clear")
check "日志可清空" "$CLEARED" '"cleared":true'

# --- summary ----------------------------------------------------------------

printf '\n== 结果\n  %d 通过, %d 失败\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
