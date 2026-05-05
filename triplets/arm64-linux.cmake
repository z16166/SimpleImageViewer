# Overlay for cross arm64-from-x86_64 Docker (ubuntu:20.04): vcpkg's linux.cmake resolves
# CMAKE_CXX_COMPILER=aarch64-linux-gnu-g++. With only gcc-10-aarch64-linux-gnu packages,
# that basename is missing unless we symlink (see scripts/ci/vcpkg_inner_focal.sh).
set(VCPKG_TARGET_ARCHITECTURE arm64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_CMAKE_SYSTEM_NAME Linux)
set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "${CMAKE_CURRENT_LIST_DIR}/chainload-aarch64-linux.cmake")
