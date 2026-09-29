#!/usr/bin/env bash
#
# Build the kernel as a shared library and put it where the app expects it.
#
#   bash scripts/app-native-build.sh [ABI...]      (default: x86_64 arm64-v8a's arm64)
#
# The app loads `libwatt_ffi.so` from `android/app/src/main/jniLibs/<abi>/`, so
# this script is the bridge between the cargo build and the Gradle build. It is
# deliberately separate from Gradle: Gradle has no business driving cargo, and
# keeping them apart means the Rust build can be run and debugged on its own.
#
# The `jni-bridge` feature is what adds the `Java_dev_detour_core_Kernel_*`
# entry points. Without it the library only has the plain C ABI and the app's
# `System.loadLibrary` succeeds while every call throws `UnsatisfiedLinkError`.
#
# Environment:
#   NDK_DIR        path to the Android NDK   (default: ~/android/android-ndk-r30)
#   PROFILE        debug | release           (default: debug)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JNI_LIBS="$REPO_DIR/android/app/src/main/jniLibs"
PROFILE="${PROFILE:-debug}"

# The ABI names differ between cargo and Android's packaging: cargo calls it
# `aarch64-linux-android`, the APK wants a directory called `arm64-v8a`. Getting
# this wrong produces a library that builds fine and is never loaded.
ABIS=("$@")
if [ ${#ABIS[@]} -eq 0 ]; then
  ABIS=("x86_64-linux-android")
fi

abi_to_dir() {
  case "$1" in
    x86_64-linux-android)   echo "x86_64" ;;
    aarch64-linux-android)  echo "arm64-v8a" ;;
    armv7-linux-androideabi) echo "armeabi-v7a" ;;
    i686-linux-android)     echo "x86" ;;
    *) echo "" ;;
  esac
}

CARGO_ARGS=(build -p watt-ffi --features jni-bridge)
if [ "$PROFILE" = "release" ]; then
  CARGO_ARGS+=(--release)
fi

for abi in "${ABIS[@]}"; do
  dir="$(abi_to_dir "$abi")"
  if [ -z "$dir" ]; then
    echo "app-native-build: unknown ABI $abi" >&2
    exit 1
  fi

  echo "app-native-build: $abi -> jniLibs/$dir"
  # `android-build.sh` chooses the NDK clang linker from its own `ANDROID_ABI`,
  # which defaults to x86_64, and then appends `--target "$ANDROID_ABI"` to the
  # cargo line. Naming the ABI only as a cargo `--target` therefore compiles the
  # right objects but links them with the wrong toolchain: for arm64 it falls
  # back to the host `cc` and dies with `Relocations in generic ELF (EM: 183)`.
  # Exporting ANDROID_ABI keeps the linker and the target in agreement, and lets
  # android-build.sh be the one place that names the target.
  export ANDROID_ABI="$abi"
  if ! bash "$REPO_DIR/scripts/android-build.sh" \
      "${CARGO_ARGS[@]}" >/dev/null; then
    echo "app-native-build: cargo failed for $abi" >&2
    exit 1
  fi

  target_dir="${CARGO_TARGET_DIR:-$HOME/.cache/watt-target}/$abi/$PROFILE"
  source_lib="$target_dir/libwatt_ffi.so"
  if [ ! -f "$source_lib" ]; then
    echo "app-native-build: no library at $source_lib" >&2
    ls "$target_dir" 2>/dev/null | head -10 >&2
    exit 1
  fi

  mkdir -p "$JNI_LIBS/$dir"
  cp "$source_lib" "$JNI_LIBS/$dir/libwatt_ffi.so"
  echo "app-native-build: $(du -h "$JNI_LIBS/$dir/libwatt_ffi.so" | cut -f1) -> $JNI_LIBS/$dir/"
done

echo "app-native-build: done"
