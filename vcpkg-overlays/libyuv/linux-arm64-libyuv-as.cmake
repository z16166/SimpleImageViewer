# Injected via -DCMAKE_PROJECT_YUV_INCLUDE=... (linux arm64 libyuv builds only).
# -Wa aligns GNU as with dot-product Neon (udot) from intrinsics.
# GCC 9 (ubuntu focal cross) does not accept +i8mm in -march — use dotprod only (i8mm CMake lines stripped in portfile).
add_compile_options("-Wa,-march=armv8.2-a+dotprod")
