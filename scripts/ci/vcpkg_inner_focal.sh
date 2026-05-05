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

# dav1d / libavif stack expects NASM; vcpkg runs only inside this image, not on the GitHub host.
PKG=(
  curl zip unzip tar pkg-config build-essential cmake ninja-build clang nasm
  gcc-aarch64-linux-gnu g++-aarch64-linux-gnu tzdata git python3 python3-venv
)

# arm64-linux vcpkg: libyuv needs gcc>=10 for +i8mm / SVE march tokens; focal's default cross is gcc 9.
if [[ "${VCPKG_DEFAULT_TRIPLET:-}" == "arm64-linux" ]]; then
  PKG+=( gcc-10-aarch64-linux-gnu g++-10-aarch64-linux-gnu )
fi

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

dump_vcpkg_build_logs() {
  echo "::group::vcpkg build log tails (under /vcpkg/buildtrees)"
  mapfile -t _logs < <(find /vcpkg/buildtrees -type f \( \
    -name 'install-*-out.log' -o -name 'install-*-err.log' \
    -o -name 'config-*-out.log' -o -name 'config-*-err.log' \
    \) 2>/dev/null | sort -u | head -n 80) || true
  if [[ "${#_logs[@]}" -eq 0 ]]; then
    echo "(no matching *.log files found)"
  else
    for f in "${_logs[@]}"; do
      echo "----- $f (last 200 lines) -----"
      tail -n 200 "$f" 2>/dev/null || true
    done
  fi
  echo "::endgroup::"
}

set +e
/vcpkg/vcpkg install \
  "--triplet=${VCPKG_DEFAULT_TRIPLET}" \
  --overlay-ports=/work/vcpkg-overlays
_vcpkg_rc=$?
set -e
if [[ "$_vcpkg_rc" -ne 0 ]]; then
  dump_vcpkg_build_logs
  exit "$_vcpkg_rc"
fi

chmod -R 777 /work/vcpkg_installed
