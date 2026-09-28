#!/usr/bin/env bash
# Bootstrap the WSL2 Ubuntu build environment for the Watt Android full-traffic kernel.
# Safe to re-run: every step is idempotent.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

# WSL images ship unattended-upgrades, which can hold the dpkg frontend lock for
# several minutes. Ask apt to wait instead of failing, and try to get out of the
# way first when the service exists.
APT_LOCK_OPTS=(-o DPkg::Lock::Timeout=900)

log() { printf '\n==> %s\n' "$*"; }

log "apt: releasing background maintenance jobs"
sudo systemctl stop unattended-upgrades.service 2>/dev/null || true
sudo systemctl stop apt-daily.service 2>/dev/null || true
sudo systemctl stop apt-daily-upgrade.service 2>/dev/null || true

log "apt: base build + network tooling"
sudo apt-get "${APT_LOCK_OPTS[@]}" update -y
sudo apt-get "${APT_LOCK_OPTS[@]}" install -y --no-install-recommends \
  build-essential \
  pkg-config \
  curl \
  wget \
  git \
  ca-certificates \
  unzip \
  xz-utils \
  file \
  iproute2 \
  iptables \
  iputils-ping \
  dnsutils \
  netcat-openbsd \
  python3 \
  jq

log "rustup: minimal stable toolchain"
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
  sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable --no-modify-path
else
  echo "cargo already present, skipping rustup-init"
fi

# shellcheck disable=SC1091
. "$HOME/.cargo/env"

log "rust components"
rustup component add clippy rustfmt || true

log "android rust targets"
rustup target add aarch64-linux-android x86_64-linux-android || true

log "versions"
rustc --version
cargo --version
rustup target list --installed

log "kernel capabilities"
printf 'unprivileged_userns_clone=%s\n' "$(cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null || echo n/a)"
printf 'ip_forward=%s\n' "$(sysctl -n net.ipv4.ip_forward 2>/dev/null || echo n/a)"
ls -l /dev/net/tun

log "done"
