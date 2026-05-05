#!/usr/bin/env bash
# Runs inside ubuntu:20.04 during GitHub Actions Linux cross-build so the binary stays on an old glibc.
# Host mounts repo at /work and vcpkg at /vcpkg; apt occasionally drops focal-updates/security — retry hard.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
export TZ=Etc/UTC
export CC=clang
export CXX=clang++
export VCPKG_ROOT=/vcpkg

if [[ -z "${VCPKG_CHAINLOAD_TOOLCHAIN_FILE:-}" ]]; then
  unset VCPKG_CHAINLOAD_TOOLCHAIN_FILE
fi

mkdir -p /etc/apt/apt.conf.d
printf '%s\n' \
  'Acquire::Retries "10";' \
  'Acquire::http::Timeout "120";' \
  'Acquire::ftp::Timeout "120";' \
  > /etc/apt/apt.conf.d/80-ci-retries

PKG=(
  curl zip unzip tar pkg-config build-essential cmake ninja-build clang
  gcc-aarch64-linux-gnu g++-aarch64-linux-gnu tzdata git python3 python3-venv
)

installed=0
for attempt in 1 2 3 4 5 6 7 8; do
  set +e
  apt-get clean
  apt-get update 2>&1 | tee /tmp/apt-update.log
  apt_rc="${PIPESTATUS[0]}"
  set -e
  if [[ "$apt_rc" -ne 0 ]] || grep -q "Failed to fetch" /tmp/apt-update.log; then
    echo "[apt] incomplete mirror (exit ${apt_rc} or Failed to fetch); retry..."
  elif apt-get install -y "${PKG[@]}"; then
    installed=1
    break
  fi
  echo "[apt] attempt ${attempt} failed; retry in 30s..."
  sleep 30
done

if [[ "$installed" -ne 1 ]]; then
  echo "[apt] all attempts exhausted"
  exit 1
fi

/vcpkg/vcpkg install \
  "--triplet=${VCPKG_DEFAULT_TRIPLET}" \
  --overlay-ports=/work/vcpkg-overlays

chmod -R 777 /work/vcpkg_installed
