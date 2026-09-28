#!/usr/bin/env bash
# Inspect the resolved material3 AAR to find out how ShortNavigationBar lays out
# its children.
#
# The bug: only one of four destinations renders, and it spans the whole bar.
# Whether that is "the caller must supply Modifier.weight(1f)" or "the component
# is broken on this device" decides between a one-line fix and a rewrite, so the
# question is answered from the artifact rather than by guessing.
set -uo pipefail

BASE=~/.gradle/caches/modules-2/files-2.1/androidx.compose.material3
echo "=== cached artifacts ==="
find "$BASE" -name '*.aar' -o -name '*sources*.jar' 2>/dev/null | sort

SRC=$(find "$BASE" -name '*sources*.jar' 2>/dev/null | head -1)
if [ -n "$SRC" ]; then
  echo "=== sources jar found: $SRC ==="
  TMP=$(mktemp -d)
  unzip -o -q "$SRC" -d "$TMP"
  find "$TMP" -name 'ShortNavigationBar*.kt' | head
  for f in $(find "$TMP" -name 'ShortNavigationBar*.kt'); do
    echo "----- $f -----"
    sed -n '1,200p' "$f"
  done
  rm -rf "$TMP"
  exit 0
fi

AAR=$(find "$BASE" -name '*.aar' 2>/dev/null | head -1)
echo "=== no sources; inspecting bytecode of $AAR ==="
TMP=$(mktemp -d)
unzip -o -q "$AAR" classes.jar -d "$TMP"
echo "--- classes matching ShortNavigation ---"
unzip -l "$TMP/classes.jar" | grep -i 'shortnavigation' | head -40
for C in androidx.compose.material3.ShortNavigationBarKt \
         androidx.compose.material3.ShortNavigationBarItemKt \
         androidx.compose.material3.ShortNavigationBarDefaults \
         androidx.compose.material3.ShortNavigationBarArrangement; do
  echo "===== $C ====="
  javap -p -classpath "$TMP/classes.jar" "$C" 2>&1 | head -30
done
rm -rf "$TMP"
