# Injected with -DCMAKE_PROJECT_YUV_INCLUDE=... for linux arm64 only.
# After dropping +i8mm from -march (focal g++-9/10), cc1 can still emit udot; align GNU as.
add_compile_options("-Wa,-march=armv8.2-a+dotprod")
