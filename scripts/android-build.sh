#!/usr/bin/env bash
#
# Build the kernel for an Android target in this workspace.
#
# Cargo needs to be told which linker to use for a target it has no default for,
# and an Android build needs the NDK's clang rather than the host's: the sysroot
# and the C runtime have to be the ones the device actually has. `cargo check`
# hides this because it never links, which is why a target can look buildable
# until something tries to produce a binary.
#
#   scripts/android-build.sh [cargo args...]      (default: build -p watt-daemon)
#
# Environment:
#   NDK_DIR        path to the Android NDK            (default: ~/android/android-ndk-r30)
#   ANDROID_ABI    target ABI                          (default: x86_64-linux-android)
#   ANDROID_API    API level to link against           (default: 34)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

NDK_DIR="${NDK_DIR:-$HOME/android/android-ndk-r30}"
ANDROID_ABI="${ANDROID_ABI:-x86_64-linux-android}"
ANDROID_API="${ANDROID_API:-34}"

if [ ! -d "$NDK_DIR" ]; then
  echo "android-build: NDK not found at $NDK_DIR" >&2
  echo "android-build: set NDK_DIR, or download one:" >&2
  echo "  curl -o ndk.zip https://googledownloads.cn/android/repository/android-ndk-r30-linux.zip" >&2
  exit 1
fi

TOOLCHAIN="$NDK_DIR/toolchains/llvm/prebuilt/linux-x86_64"
CC="$TOOLCHAIN/bin/${ANDROID_ABI}${ANDROID_API}-clang"
if [ ! -x "$CC" ]; then
  echo "android-build: no compiler at $CC" >&2
  ls "$TOOLCHAIN/bin" | grep -E "^${ANDROID_ABI}[0-9]+-clang$" | head -5 >&2
  exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"
export "CARGO_TARGET_$(echo "$ANDROID_ABI" | tr 'a-z-' 'A-Z_')_LINKER=$CC"
export AR="$TOOLCHAIN/bin/llvm-ar"

echo "android-build: target=$ANDROID_ABI api=$ANDROID_API"
echo "android-build: linker=$CC"

if [ $# -eq 0 ]; then
  set -- build -p watt-daemon
fi

cd "$REPO_DIR/core-rs" || exit 1
exec cargo "$@" --target "$ANDROID_ABI"
