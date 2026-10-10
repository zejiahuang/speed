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

# Cargo and the NDK spell one target differently, and only one. Cargo's target is
# `armv7-linux-androideabi`; the NDK's clang wrapper is
# `armv7a-linux-androideabi<api>-clang` — an extra `a` after `armv7`. Every other
# Android target agrees between the two (aarch64, x86_64, i686), so this is the
# only translation needed; without it the script looks for a compiler that does
# not exist and blames the NDK for a file it never had. The prefix is used for
# both the linker and the `cc` compiler variable, because a linker that does not
# exist is the same failure reported under a different name.
case "$ANDROID_ABI" in
  armv7-linux-androideabi) NDK_ABI="armv7a-linux-androideabi" ;;
  *)                       NDK_ABI="$ANDROID_ABI" ;;
esac

TOOLCHAIN="$NDK_DIR/toolchains/llvm/prebuilt/linux-x86_64"
CC="$TOOLCHAIN/bin/${NDK_ABI}${ANDROID_API}-clang"
if [ ! -x "$CC" ]; then
  echo "android-build: no compiler at $CC" >&2
  ls "$TOOLCHAIN/bin" | grep -E "^${NDK_ABI}[0-9]+-clang$" | head -5 >&2
  exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}"

# Two spellings of the same target, and they are not interchangeable:
#
#   * cargo's linker var is upper-cased  -- CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER
#   * the `cc` crate's compiler var keeps the target as written, with `-` -> `_`
#     -- CC_aarch64_linux_android
#
# `cc` is what a dependency with C sources needs; `ring`, behind the upstream
# resolver's TLS, is the first such dependency this kernel has had. Exporting
# only the upper-cased form leaves `cc` with nothing, and it then fails with
# `failed to find tool "aarch64-linux-android-clang"` — a missing *variable*
# reported as a missing *compiler*.
KEY="$(echo "$ANDROID_ABI" | tr '-' '_')"
KEY_UP="$(echo "$ANDROID_ABI" | tr 'a-z-' 'A-Z_')"
export "CARGO_TARGET_${KEY_UP}_LINKER=$CC"
export "CC_${KEY}=$CC"
export "CXX_${KEY}=$TOOLCHAIN/bin/${NDK_ABI}${ANDROID_API}-clang++"
export "AR_${KEY}=$TOOLCHAIN/bin/llvm-ar"
# The bare names as well: some build scripts read `AR` before `AR_<target>`.
export AR="$TOOLCHAIN/bin/llvm-ar"

echo "android-build: target=$ANDROID_ABI api=$ANDROID_API"
echo "android-build: linker=$CC"

if [ $# -eq 0 ]; then
  set -- build -p watt-daemon
fi

cd "$REPO_DIR/core-rs" || exit 1
exec cargo "$@" --target "$ANDROID_ABI"
