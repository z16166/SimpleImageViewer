# Single chainload for arm64-linux overlay triplet: include vcpkg Linux flags, then pin GCC 10.
if(NOT "$ENV{VCPKG_ROOT}x" STREQUAL "x" AND EXISTS "$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
  include("$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
endif()

set(CMAKE_C_COMPILER "/usr/bin/aarch64-linux-gnu-gcc-10" CACHE STRING "" FORCE)
set(CMAKE_CXX_COMPILER "/usr/bin/aarch64-linux-gnu-g++-10" CACHE STRING "" FORCE)
