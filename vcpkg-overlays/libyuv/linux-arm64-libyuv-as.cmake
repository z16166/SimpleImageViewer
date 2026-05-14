# Injected with -DCMAKE_PROJECT_YUV_INCLUDE=... for linux arm64 only.
# NEON stays enabled at armv8-a; DotProd/I8MM object code is compiled out when these are set
# (see arm64-disable-dotprod-i8mm.patch). SVE/SME stay off for Zig cortex_a53 baseline.
add_compile_definitions(
    LIBYUV_DISABLE_NEON_DOTPROD
    LIBYUV_DISABLE_NEON_I8MM
    LIBYUV_DISABLE_SVE
    LIBYUV_DISABLE_SME
)
