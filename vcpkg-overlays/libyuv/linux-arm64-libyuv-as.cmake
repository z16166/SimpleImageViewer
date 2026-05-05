# Injected via -DCMAKE_PROJECT_YUV_INCLUDE=... (linux arm64 libyuv builds only).
# GCC emits udot/usdot/sudot from Neon intrinsics, but GNU as defaults to an -march
# that rejects those opcodes when cross-compiling. Give as the same ISA as the sources expect.
add_compile_options("-Wa,-march=armv8.2-a+dotprod+i8mm")
