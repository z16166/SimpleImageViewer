#!/usr/bin/env bash
# Runs inside ubuntu:22.04 for GitHub Actions Linux arm64 overlay so glibc aligns with GCC 12 cross packages.
# Host mounts repo at /work and vcpkg at /vcpkg; apt occasionally drops updates/security repos — retry hard.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
export TZ=Etc/UTC
export VCPKG_ROOT=/vcpkg

if [[ "${VCPKG_DEFAULT_TRIPLET:-}" != "arm64-linux" ]]; then
  export CC=clang
  export CXX=clang++
fi

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
  curl wget zip unzip tar pkg-config build-essential cmake ninja-build clang nasm
  tzdata git python3 python3-venv
)

if [[ "${VCPKG_DEFAULT_TRIPLET:-}" == "arm64-linux" ]]; then
  PKG+=( gcc-12-aarch64-linux-gnu g++-12-aarch64-linux-gnu binutils-aarch64-linux-gnu )
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

# vcpkg scripts/toolchains/linux.cmake uses aarch64-linux-gnu-gcc/g++ (unversioned).
# We only install gcc-12-aarch64-linux-gnu, so those basenames are missing.
if [[ "${VCPKG_DEFAULT_TRIPLET:-}" == "arm64-linux" ]]; then
  mkdir -p /usr/local/bin
  ln -sf /usr/bin/aarch64-linux-gnu-gcc-12 /usr/local/bin/aarch64-linux-gnu-gcc
  ln -sf /usr/bin/aarch64-linux-gnu-g++-12 /usr/local/bin/aarch64-linux-gnu-g++
  export PATH="/usr/local/bin:${PATH}"
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

if [[ "${VCPKG_DEFAULT_TRIPLET:-}" == "arm64-linux" ]]; then
  unset CC CXX CPP CFLAGS CXXFLAGS LDFLAGS 2>/dev/null || true
fi

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
