#!/usr/bin/env bash
# Run inside AlmaLinux 8 (glibc 2.28) with gcc-toolset-15 enabled — full native build for one Rust triple.
# Usage: alma8-linux-build.sh <x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu>
# Expects workspace mounted at /workspace, nuget.config at /workspace/nuget.config, vcpkg at /workspace/vcpkg (not bootstrapped on host).
set -euo pipefail

RUST_TRIPLE="${1:?usage: alma8-linux-build.sh RUST_TRIPLE}"
cd /workspace

# Minimal almalinux:8 image has no PowerTools repo; packages like ninja-build/nasm live there.
# (Upstream calls this CRB on EL9+; on Alma 8 the tree is still named PowerTools.)
dnf -y install dnf-plugins-core
CRB_ARCH="$(uname -m)"
cat >/etc/yum.repos.d/almalinux8-powertools-ci.repo <<EOF
[powertools-ci]
name=AlmaLinux 8 - PowerTools (CI)
baseurl=https://repo.almalinux.org/almalinux/8/PowerTools/${CRB_ARCH}/os/
enabled=1
gpgcheck=1
gpgkey=https://repo.almalinux.org/almalinux/RPM-GPG-KEY-AlmaLinux
EOF
dnf -y update

# Toolchain + deps for vcpkg ports, Rust -sys crates, and egui/wgpu Linux stack
PKGS=(
  gcc-toolset-15 gcc-toolset-15-gcc gcc-toolset-15-gcc-c++ gcc-toolset-15-gdb
  # 64-bit libstdc++.a for the toolset lives here on EL8 (no separate gcc-toolset-*-libstdc++-static RPM).
  gcc-toolset-15-libstdc++-devel
  gcc-toolset-15-libatomic-devel
  git curl ninja-build nasm make patch file which tar xz unzip python3 python3-pip python311
  pkgconf pkg-config glibc-devel libstdc++-devel libstdc++-static
  alsa-lib-devel libX11-devel libxcb-devel libxkbcommon-devel
  libXcursor-devel libXrandr-devel libXi-devel mesa-libGL-devel
  libwayland-client-devel libwayland-cursor-devel wayland-devel
  fontconfig-devel freetype-devel expat-devel zlib-devel gawk
  autoconf autoconf-archive automake libtool libtool-ltdl-devel gettext-devel m4 flex bison texinfo
  libatomic openssl-devel perl
  # g++ -fuse-ld=lld (see .cargo/config.toml); Alma 8 AppStream ships /usr/bin/ld.lld
  lld
)
dnf -y install "${PKGS[@]}"
git config --global --add safe.directory /workspace

# C++ archives are linked with -static-libstdc++, but some Rust/vcpkg link lines still record
# DT_NEEDED libstdc++.so.6. System `pkg-config` is not used by vcpkg-rs (it parses .pc itself),
# but other -sys crates may — filter -lstdc++ from --libs output when they do.
install -d /usr/local/bin
_real_pkg_config="$(command -v pkg-config)"
{
  echo '#!/usr/bin/env bash'
  echo 'set -euo pipefail'
  echo 'set -o pipefail'
  printf '_real=%q\n' "${_real_pkg_config}"
  cat <<'WRAPPER'
if [[ "$*" == *--libs* ]]; then
  "$_real" "$@" | perl -pe 's/(^|\s)-lstdc\+\+(?=\s|$)/$1/g'
else
  exec "$_real" "$@"
fi
WRAPPER
} > /usr/local/bin/pkg-config
chmod +x /usr/local/bin/pkg-config
export PATH="/usr/local/bin:${PATH}"

# Kitware CMake: Alma 8 ships CMake 3.18.x; libyuv uses hooks that are reliable on newer CMake.
# Cache under the workspace so bind-mounted .ci-cache survives across CI runs.
KITWARE_CMAKE_VERSION="${KITWARE_CMAKE_VERSION:-4.3.2}"
_sysarch="$(uname -m)"
case "${_sysarch}" in
  aarch64 | arm64) _cmake_plat=linux-aarch64 ;;
  x86_64) _cmake_plat=linux-x86_64 ;;
  *)
    echo "Unsupported machine for Kitware CMake bundle: ${_sysarch}" >&2
    exit 2
    ;;
esac
_KIT_CMAKE_DIR="cmake-${KITWARE_CMAKE_VERSION}-${_cmake_plat}"
_KIT_CMAKE_ROOT="/workspace/.ci-cache/kitware-cmake/${_KIT_CMAKE_DIR}"
if [[ ! -x "${_KIT_CMAKE_ROOT}/bin/cmake" ]]; then
  echo "Installing Kitware CMake ${KITWARE_CMAKE_VERSION} (${_cmake_plat}) into ${_KIT_CMAKE_ROOT}..."
  rm -rf "${_KIT_CMAKE_ROOT}.tmp"
  mkdir -p "${_KIT_CMAKE_ROOT}.tmp"
  curl -fsSL -o "/tmp/${_KIT_CMAKE_DIR}.tar.gz" \
    "https://github.com/Kitware/CMake/releases/download/v${KITWARE_CMAKE_VERSION}/${_KIT_CMAKE_DIR}.tar.gz"
  tar -xzf "/tmp/${_KIT_CMAKE_DIR}.tar.gz" -C "${_KIT_CMAKE_ROOT}.tmp" --strip-components=1
  rm -rf "${_KIT_CMAKE_ROOT}"
  mv "${_KIT_CMAKE_ROOT}.tmp" "${_KIT_CMAKE_ROOT}"
fi
export PATH="${_KIT_CMAKE_ROOT}/bin:${PATH}"
echo "Using cmake: $(command -v cmake)"
cmake --version | head -1

# /usr/bin/python3 on EL8 is 3.6; vcpkg-tool-meson requires Python >= 3.7.
mkdir -p /usr/local/bin
ln -sf /usr/bin/python3.11 /usr/local/bin/python3
export PATH="/usr/local/bin:${PATH}"
# Help aclocal find PowerTools autoconf-archive macros (vcpkg ports using autoreconf, e.g. alsa).
export ACLOCAL_PATH="/usr/share/aclocal"
if [[ -d /usr/share/autoconf-archive ]]; then
  export ACLOCAL_PATH="${ACLOCAL_PATH}:/usr/share/autoconf-archive"
fi

# EPEL: Mono for vcpkg NuGet binary cache (same pattern as Ubuntu CI).
# Do not use "mono-core mono-devel || mono-complete": the first succeeds but is too
# minimal for nuget.exe 7.x (missing WindowsBase / reference assemblies); need complete.
dnf -y install epel-release || true
dnf -y install mono-complete libgdiplus

source /opt/rh/gcc-toolset-15/enable
# Pin the link driver for Rust openexr/build.rs `-print-file-name=libstdc++.a` and the final link.
export CC="$(command -v gcc)"
export CXX="$(command -v g++)"
_toolstd_a="$("${CXX}" -print-file-name=libstdc++.a)"
if [[ -z "${_toolstd_a}" || "${_toolstd_a}" == libstdc++.a || ! -f "${_toolstd_a}" ]]; then
  echo "::error::${CXX} -print-file-name=libstdc++.a must resolve to an existing file (got: ${_toolstd_a:-empty}). On EL8 install gcc-toolset-15-libstdc++-devel (provides .../15/libstdc++.a)."
  exit 1
fi
echo "Toolchain libstdc++.a: ${_toolstd_a}"
if ! command -v ld.lld >/dev/null 2>&1; then
  echo "::error::ld.lld not on PATH after installing lld; LLD wrapper uses g++ -fuse-ld=lld (.cargo/g++-lld-wrap.sh)."
  exit 1
fi
_ld_lld="$(command -v ld.lld)"
echo "LLD: ${_ld_lld} ($("${_ld_lld}" --version 2>/dev/null | head -1 || true))"
_wrap="/workspace/.cargo/g++-lld-wrap.sh"
if [[ ! -x "${_wrap}" ]]; then
  chmod +x "${_wrap}" || true
fi
_tmp="$(mktemp)"
if ! printf 'int main(){}\n' | "${_wrap}" -x c++ - -o "${_tmp}"; then
  echo "::error::${_wrap} smoke link failed (wrapper should run g++ -fuse-ld=lld; see lld on PATH)."
  rm -f "${_tmp}"
  exit 1
fi
rm -f "${_tmp}"
# Do not set CARGO_TARGET_*_LINKER here: it overrides [target.*] linker in .cargo/config.toml and would skip the LLD wrapper.

case "${RUST_TRIPLE}" in
  aarch64-unknown-linux-gnu)
    export VCPKG_DEFAULT_TRIPLET=arm64-linux-v8a
    export CFLAGS="-march=armv8-a -mcpu=cortex-a53 -O2"
    export CXXFLAGS="-march=armv8-a -mcpu=cortex-a53 -O2"
    RUST_PROFILE=ci
    ;;
  x86_64-unknown-linux-gnu)
    export VCPKG_DEFAULT_TRIPLET=x64-linux
    export CFLAGS="-O2"
    export CXXFLAGS="-O2"
    RUST_PROFILE=ci
    ;;
  *)
    echo "Unsupported triple: ${RUST_TRIPLE}" >&2
    exit 2
    ;;
esac

export VCPKG_ROOT=/workspace/vcpkg
export PATH="${VCPKG_ROOT}:${PATH}"
export VCPKG_KEEP_ENV_VARS="${VCPKG_KEEP_ENV_VARS:-};PKG_CONFIG;M4;AUTOCONF;AUTOMAKE;LIBTOOL;GETTEXT;VCPKG_MAKE_BUILD_TRIPLET"

if [[ ! -d "${VCPKG_ROOT}/.git" ]]; then
  echo "ERROR: vcpkg git checkout missing at ${VCPKG_ROOT}" >&2
  exit 1
fi
if [[ ! -x "${VCPKG_ROOT}/vcpkg" ]]; then
  (cd "${VCPKG_ROOT}" && ./bootstrap-vcpkg.sh -disableMetrics)
fi

export NUGET_CONFIGFILE=/workspace/nuget.config

if [[ -n "${GITHUB_TOKEN:-}" && -n "${GITHUB_REPOSITORY_OWNER:-}" ]]; then
  mkdir -p /workspace/.nuget-tools
  NUGET_EXE=/workspace/.nuget-tools/nuget.exe
  if [[ ! -f "${NUGET_EXE}" ]]; then
    curl -fsSL -o "${NUGET_EXE}" "https://dist.nuget.org/win-x86-commandline/v7.3.1/nuget.exe"
  fi
  SOURCE="https://nuget.pkg.github.com/${GITHUB_REPOSITORY_OWNER}/index.json"
  mono "${NUGET_EXE}" setApiKey "${GITHUB_TOKEN}" -Source "${SOURCE}" || true
fi

export VCPKG_BINARY_SOURCES="${VCPKG_BINARY_SOURCES:-}"

set +e
vcpkg install \
  --triplet="${VCPKG_DEFAULT_TRIPLET}" \
  --overlay-ports=/workspace/vcpkg-overlays \
  --overlay-triplets=/workspace/triplets \
  2>&1 | tee /workspace/vcpkg_build.log
vc=$?
set -e
if [[ "${vc}" -ne 0 ]]; then
  echo "::error::vcpkg install failed" >&2
  exit "${vc}"
fi

# vcpkg manifest mode installs under <workspace>/vcpkg_installed, but vcpkg-rs scans
# $VCPKG_ROOT/installed/<triplet> and $VCPKG_ROOT/installed/vcpkg/status. Mirror both so
# find_package can emit full link lines instead of failing and falling back.
manifest_installed="/workspace/vcpkg_installed/${VCPKG_DEFAULT_TRIPLET}"
mkdir -p "${VCPKG_ROOT}/installed"
ln -sfn "${manifest_installed}" "${VCPKG_ROOT}/installed/${VCPKG_DEFAULT_TRIPLET}"
ln -sfn "/workspace/vcpkg_installed/vcpkg" "${VCPKG_ROOT}/installed/vcpkg"

# Pkg-config from vcpkg: strip bare -lstdc++ from installed *.pc (vcpkg-rs parses these; see vcpkg
# crate PcFile::from_str).
if command -v perl >/dev/null 2>&1; then
  find "${manifest_installed}" -name '*.pc' -type f -print0 |
    while IFS= read -r -d '' f; do
      # Drop explicit libstdc++ from *all* .pc lines (Libs / Libs.private / Weird spacing).
      perl -i -pe 's/(^|\s)-lstdc\+\+(?=\s|$)/$1/g' "$f"
      perl -i -pe 's/(^|\s)-l\s+stdc\+\+(?=\s|$)/$1/g' "$f"
      perl -i -pe 's/(^|\s)-l:libstdc\+\+\.so[^\s]*(?=\s|$)/$1/g' "$f"
    done
else
  echo "::warning::perl not found; skipping vcpkg .pc -lstdc++ strip (libstdc++.so may stay in NEEDED)"
fi

LIBYUV_A="/workspace/vcpkg_installed/${VCPKG_DEFAULT_TRIPLET}/lib/libyuv.a"
if [[ -f "${LIBYUV_A}" ]]; then
  echo "::group::Debug: libyuv.a symbols matching ABGRToYRow_NEON"
  echo "archive=${LIBYUV_A}"
  if command -v file >/dev/null 2>&1; then
    file "${LIBYUV_A}" || true
  fi
  echo "---- nm (all matches; T/t = defined, U = unresolved ref in some member) ----"
  nm -g "${LIBYUV_A}" 2>/dev/null | grep -F 'ABGRToYRow_NEON' || echo "(no nm matches)"
  echo "---- nm defined-only ----"
  nm -g --defined-only "${LIBYUV_A}" 2>/dev/null | grep -F 'ABGRToYRow_NEON' || echo "(no defined symbols matched)"
  echo "---- archive members with neon in name (first 30) ----"
  ar -t "${LIBYUV_A}" 2>/dev/null | grep -i neon | head -30 || true
  _row_m="$(ar -t "${LIBYUV_A}" 2>/dev/null | grep -F 'row_neon64' | head -1 || true)"
  if [[ -n "${_row_m}" ]]; then
    echo "::group::Debug: nm defined-only on archive member ${_row_m}"
    _td="$(mktemp -d)"
    (cd "${_td}" && ar x "${LIBYUV_A}" "${_row_m}" 2>/dev/null) || true
    if [[ -f "${_td}/${_row_m}" ]]; then
      nm -g --defined-only "${_td}/${_row_m}" 2>/dev/null | grep -F 'ABGRToYRow_NEON' || echo "(no ABGRToYRow_NEON T/t in ${_row_m})"
    fi
    rm -rf "${_td}"
    echo "::endgroup::"
  fi
  echo "::endgroup::"
else
  echo "::warning::Debug: missing ${LIBYUV_A} (cannot probe ABGRToYRow_NEON)"
fi

VCPKG_PC="/workspace/vcpkg_installed/${VCPKG_DEFAULT_TRIPLET}/lib/pkgconfig"
if [[ ! -d "${VCPKG_PC}" ]]; then
  echo "ERROR: missing ${VCPKG_PC}" >&2
  ls -la /workspace/vcpkg_installed >&2 || true
  exit 1
fi

SYS_PC="/usr/lib64/pkgconfig:/usr/share/pkgconfig"
export PKG_CONFIG_PATH="${VCPKG_PC}:${SYS_PC}"
export PKG_CONFIG_LIBDIR="${PKG_CONFIG_PATH}"
export PKG_CONFIG_ALLOW_CROSS=1
# Helps pkg-config emit static-friendly lines on x64 (aarch64: enable only after verifying all ports ship .a).
if [[ "${RUST_TRIPLE}" == x86_64-* ]]; then
  export PKG_CONFIG_ALL_STATIC=1
fi

# Monkey's Audio SDK (non-Windows path — was host step before)
MONKEY_VERSION="${MONKEY_SDK_VERSION:-1293}"
MONKEY_DIR="${MONKEY_SDK_DIR:-3rdparty/monkey-sdk}"
if [[ ! -f "${MONKEY_DIR}/Shared/Common/MACLib.h" ]]; then
  mkdir -p "${MONKEY_DIR}"
  curl -fsSL -o /tmp/monkey_sdk.zip "https://monkeysaudio.com/files/MAC_${MONKEY_VERSION}_SDK.zip"
  rm -rf /tmp/monkey_temp
  mkdir -p /tmp/monkey_temp "${MONKEY_DIR}"
  unzip -q -o /tmp/monkey_sdk.zip -d /tmp/monkey_temp
  shopt -s dotglob nullglob
  _m=(/tmp/monkey_temp/*)
  if ((${#_m[@]})); then cp -a "${_m[@]}" "${MONKEY_DIR}/"; fi
  shopt -u dotglob nullglob
fi

export RUSTUP_HOME=/workspace/.ci-rustup
export CARGO_HOME=/workspace/.ci-cargo
export CARGO_INCREMENTAL=0

if [[ ! -x "${CARGO_HOME}/bin/cargo" ]]; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path
fi
source "${CARGO_HOME}/env"
rustup default stable

# CMake / bindgen helpers that read PKG_CONFIG paths
export CMAKE_PREFIX_PATH="/workspace/vcpkg_installed/${VCPKG_DEFAULT_TRIPLET}${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"

# Belt-and-suspenders: merges with `.cargo/config.toml` `[target.*-linux-gnu] rustflags`.
# Avoid `-Wl,-Bstatic -lstdc++ -Wl,-Bdynamic`: it can switch back to shared libstdc++ for symbols
# from late `.a` objects (e.g. libde265 C++) after the static pass. `-static-libstdc++` is enough here;
# root `build.rs` / vcpkg metadata handle archive order.
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-static-libstdc++ -C link-arg=-static-libgcc"

# Optional: override `profile.ci` LTO (off|thin|fat|true|false). If unset, Cargo.toml default applies.
# Note: `fat` on Alma + gcc-toolset + static C++ deps has recorded NEEDED libstdc++.so.6 despite
# -static-libstdc++; keep unset unless you verify `readelf -d` on the binary.
if [[ -n "${SIV_ALMA_RELEASE_LTO:-}" ]]; then
  export CARGO_PROFILE_CI_LTO="${SIV_ALMA_RELEASE_LTO}"
fi

# CI: main-crate build.rs emits `-Wl,-Map=target/simple-image-viewer-final.link.map` for Alma debugging.
export SIV_CI_LINK_MAP=1

cargo build --locked --profile "${RUST_PROFILE}" --target "${RUST_TRIPLE}"

LINK_MAP="/workspace/target/simple-image-viewer-final.link.map"
LINK_MAP_SNAP="/workspace/target/link.map.after-cargo-build"
if [[ -f "${LINK_MAP}" ]]; then
  cp -f "${LINK_MAP}" "${LINK_MAP_SNAP}"
  echo "Saved link map snapshot (before cargo test may relink): ${LINK_MAP_SNAP}"
fi
cargo test --locked --profile "${RUST_PROFILE}" --target "${RUST_TRIPLE}" simd_swizzle

BIN=SimpleImageViewer
SRC=""
for cand in "/workspace/target/${RUST_TRIPLE}/${RUST_PROFILE}/${BIN}" "/workspace/target/${RUST_TRIPLE}/ci/${BIN}" "/workspace/target/${RUST_TRIPLE}/release/${BIN}"; do
  if [[ -f "${cand}" ]]; then
    SRC="${cand}"
    break
  fi
done
if [[ -z "${SRC}" ]]; then
  echo "ERROR: could not find ${BIN} under target/" >&2
  find /workspace/target -type f -name "${BIN}" 2>/dev/null | head -20 >&2 || true
  exit 1
fi
DEST="/workspace/target/${RUST_TRIPLE}/release/${BIN}"
mkdir -p "/workspace/target/${RUST_TRIPLE}/release"
if [[ "${SRC}" != "${DEST}" ]]; then
  cp -f "${SRC}" "${DEST}"
fi
echo "Built and staged: ${DEST}"

siv_dump_link_map_stdc() {
  local primary="/workspace/target/simple-image-viewer-final.link.map"
  local snap="/workspace/target/link.map.after-cargo-build"
  local map=""
  if [[ -f "${primary}" ]]; then
    map="${primary}"
  elif [[ -f "${snap}" ]]; then
    map="${snap}"
  fi
  if [[ -z "${map}" ]]; then
    echo "::warning::link map missing: ${primary} (and no snapshot ${snap})"
    find /workspace/target -maxdepth 4 \( -name 'simple-image-viewer-final.link.map' -o -name 'link.map.after-cargo-build' \) -type f 2>/dev/null | head -20 || true
    return 0
  fi
  echo "::group::link.map: libstdc++ / load hints (${map}, $(wc -c < "${map}" | tr -d ' ') bytes)"
  echo "---- grep: libstdc | stdc++ (first 800 lines) ----"
  grep -nE 'libstdc|stdc\+\+' "${map}" | head -800 || echo "(no lines matched libstdc|stdc++ pattern)"
  echo "---- grep: .so paths mentioning stdc++ ----"
  grep -nF '.so' "${map}" | grep -F 'stdc' | head -400 || echo "(no .so path with stdc in name)"
  echo "---- grep: '.a' archive lines mentioning stdc++ ----"
  grep -nE '\.a\)' "${map}" | grep -F 'stdc' | head -200 || true
  echo "---- grep: 'LOAD ' lines with stdc ----"
  grep -nE '^LOAD |libstdc' "${map}" | head -300 || true
  echo "::endgroup::"
}

siv_dump_link_map_stdc

if readelf -d "${DEST}" 2>/dev/null | grep -qF 'libstdc++.so'; then
  echo "::error::${DEST} lists libstdc++.so in NEEDED."
  echo "Diagnostics: full PT_DYNAMIC (NEEDED):"
  readelf -d "${DEST}" >&2 || true
  echo "To find the source on a dev machine:"
  echo "  - Run: cargo build -vv --locked --profile \"${RUST_PROFILE}\" --target \"${RUST_TRIPLE}\" 2>&1 | tee /tmp/link.log"
  echo "        then grep -E 'stdc\\+\\+|Running .*g\\+\\+|cc ' /tmp/link.log"
  echo "  - GNU ld: add to RUSTFLAGS: -C link-arg=-Wl,--verbose (shows which libstdc++ file was chosen)"
  echo "  - GNU ld: link map from CI: /workspace/target/simple-image-viewer-final.link.map (or link.map.after-cargo-build)"
  siv_dump_link_map_stdc >&2 || true
  echo "Note: there is no standard -Wl,debug; use --verbose or a link map instead."
  echo "Expected fixes: g++ as linker, -static-libstdc++ (see .cargo/config.toml), patched vcpkg emit static= for .a,"
  echo "  Alma: do not set SIV_ALMA_RELEASE_LTO unless you need fat/thin LTO; verify readelf -d if enabling."
  echo "  If NEEDED libstdc++.so appears, try leaving LTO off (default) or OOM/SIGILL overrides as documented."
  exit 1
fi
