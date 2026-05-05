# Injected with -DCMAKE_PROJECT_YUV_INCLUDE=... for linux arm64 only.
# GCC 10: no SVE2 in -march — row_sve.cc is empty when LIBYUV_DISABLE_SVE is set.
add_compile_definitions(LIBYUV_DISABLE_SVE)
# cc1 uses -march=armv8.2-a+dotprod (no +i8mm); pass +i8mm to GNU as for usdot/sudot in row_neon64.
add_compile_options("-Wa,-march=armv8.2-a+dotprod+i8mm")
