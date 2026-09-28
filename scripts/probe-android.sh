#!/usr/bin/env bash
#
# What is actually available for building an Android target from this WSL distro.
#
# Nothing here changes the system: it only reports. Run it before deciding how to
# scaffold the Android side, because the answer decides whether the NDK needs to
# be installed first and whether a Rust cross build is possible at all.
#
#   scripts/probe-android.sh

set -uo pipefail

# rustup/cargo live in ~/.cargo/bin, which a non-interactive shell does not have on
# its PATH. Without this the probe reports "rustup NOT INSTALLED" on a machine that
# has been building Rust all day.
export PATH="$HOME/.cargo/bin:$PATH"

say() { printf '%-22s %s\n' "$1" "$2"; }

echo "=== host ==="
say "distro" "$(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME")"
say "kernel" "$(uname -r)"
say "arch" "$(uname -m)"
say "cpus" "$(nproc)"
say "memory" "$(awk '/MemTotal/ {printf "%.1f GiB", $2/1048576}' /proc/meminfo)"

echo
echo "=== java ==="
if command -v java >/dev/null 2>&1; then
  say "java" "$(java -version 2>&1 | head -1)"
  say "javac" "$(javac -version 2>&1 | head -1)"
  say "JAVA_HOME" "${JAVA_HOME:-(unset)}"
else
  say "java" "NOT INSTALLED"
fi

echo
echo "=== android sdk / ndk ==="
for candidate in \
  "$HOME/Android/Sdk" \
  "$HOME/android-sdk" \
  "/usr/lib/android-sdk" \
  "/opt/android-sdk" \
  "${ANDROID_HOME:-/nonexistent}" \
  "${ANDROID_SDK_ROOT:-/nonexistent}"; do
  if [ -d "$candidate" ]; then
    say "sdk dir" "$candidate"
    ls "$candidate" 2>/dev/null | tr '\n' ' '
    echo
  fi
done
say "ANDROID_HOME" "${ANDROID_HOME:-(unset)}"
say "ANDROID_SDK_ROOT" "${ANDROID_SDK_ROOT:-(unset)}"
say "ANDROID_NDK_HOME" "${ANDROID_NDK_HOME:-(unset)}"

for tool in sdkmanager avdmanager adb gradle kotlinc aapt2; do
  if command -v "$tool" >/dev/null 2>&1; then
    say "$tool" "$(command -v "$tool")"
  else
    say "$tool" "-"
  fi
done

# The NDK is what a Rust cross build actually needs: the clang wrappers, the
# per-API sysroot and the linker. An SDK without it cannot produce a .so.
ndk_hits="$(find "$HOME" /opt /usr/lib -maxdepth 4 -type d -name 'ndk*' 2>/dev/null | head -5)"
if [ -n "$ndk_hits" ]; then
  echo "ndk candidates:"
  echo "$ndk_hits" | sed 's/^/  /'
else
  say "ndk" "NOT FOUND"
fi

echo
echo "=== rust android targets ==="
if command -v rustup >/dev/null 2>&1; then
  say "rustc" "$(rustc --version 2>/dev/null || echo '-')"
  rustup target list --installed 2>/dev/null | sed 's/^/  /'
else
  say "rustup" "NOT INSTALLED"
fi

echo
echo "=== disk ==="
df -h "$HOME" /mnt/d 2>/dev/null | sed 's/^/  /'

echo
echo "probe-android: done"
