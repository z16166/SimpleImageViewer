# Injected with -DCMAKE_PROJECT_YUV_INCLUDE=... for linux arm64 only (vcpkg ubuntu:20.04 / old glibc CI).
# SVE2 needs newer GCC; NEON64 row_neon64 emits usdot/sudot (i8mm) that GNU as 2.34 (focal) cannot assemble even with -Wa.
add_compile_definitions(LIBYUV_DISABLE_SVE)
add_compile_definitions(LIBYUV_DISABLE_NEON)
