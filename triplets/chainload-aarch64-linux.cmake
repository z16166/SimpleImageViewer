# Include vcpkg's Linux toolchain (cross flags). Do not FORCE gcc-* here — that overrides
# per-port compiler overrides. Docker symlinks aarch64-linux-gnu-* → *-12 (see vcpkg_inner_jammy.sh).
if(NOT "$ENV{VCPKG_ROOT}x" STREQUAL "x" AND EXISTS "$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
  include("$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
endif()
