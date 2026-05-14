"""Apply arm64 DotProd/I8MM preprocessor guards to upstream libyuv source/row_neon64.cc.

Usage:
  python scripts/dev/apply_libyuv_arm64_row_neon64.py <path/to/row_neon64.cc>
"""
from __future__ import annotations

import sys
from pathlib import Path


def main() -> None:
    path = Path(sys.argv[1])
    t = path.read_text(encoding="utf-8")

    def rep(old: str, new: str) -> None:
        nonlocal t
        if old not in t:
            raise SystemExit(f"substring not found:\n{old[:200]!r}...")
        t = t.replace(old, new, 1)

    # I8MM: narrow guards - a single #if across baseline *UVRow_NEON strips those symbols
    # when LIBYUV_DISABLE_NEON_I8MM is set (linker U without T).
    rep(
        "}\n\nstatic void ARGBToUV444MatrixRow_NEON_I8MM(",
        "}\n\n#if !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "static void ARGBToUV444MatrixRow_NEON_I8MM(",
    )
    rep(
        '        "v29");\n}\n\n// RGB to BT601 coefficients\n',
        '        "v29");\n}\n\n#endif  // !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n'
        '// RGB to BT601 coefficients\n',
    )
    rep(
        "  ARGBToUV444MatrixRow_NEON(src_argb, dst_u, dst_v, width,\n"
        "                            &kARGBI601UVConstants);\n"
        "}\n\n"
        "void ARGBToUV444Row_NEON_I8MM(",
        "  ARGBToUV444MatrixRow_NEON(src_argb, dst_u, dst_v, width,\n"
        "                            &kARGBI601UVConstants);\n"
        "}\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "void ARGBToUV444Row_NEON_I8MM(",
    )
    rep(
        "  ARGBToUV444MatrixRow_NEON_I8MM(src_argb, dst_u, dst_v, width,\n"
        "                                 &kARGBI601UVConstants);\n"
        "}\n\n"
        "// RGB to JPEG coefficients\n",
        "  ARGBToUV444MatrixRow_NEON_I8MM(src_argb, dst_u, dst_v, width,\n"
        "                                 &kARGBI601UVConstants);\n"
        "}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "// RGB to JPEG coefficients\n",
    )
    rep(
        "  ARGBToUV444MatrixRow_NEON(src_argb, dst_u, dst_v, width,\n"
        "                            &kARGBJPEGUVConstants);\n"
        "}\n\n"
        "void ARGBToUVJ444Row_NEON_I8MM(",
        "  ARGBToUV444MatrixRow_NEON(src_argb, dst_u, dst_v, width,\n"
        "                            &kARGBJPEGUVConstants);\n"
        "}\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "void ARGBToUVJ444Row_NEON_I8MM(",
    )
    rep(
        "  ARGBToUV444MatrixRow_NEON_I8MM(src_argb, dst_u, dst_v, width,\n"
        "                                 &kARGBJPEGUVConstants);\n"
        "}\n\n"
        "#define RGBTOUV_SETUP_REG",
        "  ARGBToUV444MatrixRow_NEON_I8MM(src_argb, dst_u, dst_v, width,\n"
        "                                 &kARGBJPEGUVConstants);\n"
        "}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "#define RGBTOUV_SETUP_REG",
    )
    rep(
        '        "v28"\n\n'
        "  );\n}\n\n"
        "// Process any of ARGB, ABGR, BGRA, RGBA, by adjusting the uvconstants layout.\n"
        "static void ABCDToUVMatrixRow_NEON_I8MM(",
        '        "v28"\n\n'
        "  );\n}\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "// Process any of ARGB, ABGR, BGRA, RGBA, by adjusting the uvconstants layout.\n"
        "static void ABCDToUVMatrixRow_NEON_I8MM(",
    )
    rep(
        "                              kABGRToUVJCoefficients);\n}\n\nvoid RGB565ToYRow_NEON(",
        "                              kABGRToUVJCoefficients);\n}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "void RGB565ToYRow_NEON(",
    )
    rep(
        "        \"v17\");\n}\n\nstatic void ARGBToYMatrixRow_NEON_DotProd(",
        "        \"v17\");\n}\n\n#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\nstatic void ARGBToYMatrixRow_NEON_DotProd(",
    )
    rep(
        "        \"v17\");\n}\n\n// RGB to JPeg coefficients",
        "        \"v17\");\n}\n\n#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n// RGB to JPeg coefficients",
    )
    rep(
        "static const struct RgbConstants kRgb24JPEGConstants = {{29, 150, 77, 0},\n"
        "                                                        0x0080};\n"
        "static const struct RgbConstants kRgb24JPEGDotProdConstants",
        "static const struct RgbConstants kRgb24JPEGConstants = {{29, 150, 77, 0},\n"
        "                                                        0x0080};\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n"
        "static const struct RgbConstants kRgb24JPEGDotProdConstants",
    )
    rep(
        "kRgb24JPEGDotProdConstants = {{0, 29, 150, 77},\n"
        "                                                               0x0080};\n\n"
        "static const struct RgbConstants kRawJPEGConstants",
        "kRgb24JPEGDotProdConstants = {{0, 29, 150, 77},\n"
        "                                                               0x0080};\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "static const struct RgbConstants kRawJPEGConstants",
    )
    rep(
        "static const struct RgbConstants kRgb24I601Constants = {{25, 129, 66, 0},\n"
        "                                                        0x1080};\n"
        "static const struct RgbConstants kRgb24I601DotProdConstants",
        "static const struct RgbConstants kRgb24I601Constants = {{25, 129, 66, 0},\n"
        "                                                        0x1080};\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n"
        "static const struct RgbConstants kRgb24I601DotProdConstants",
    )
    rep(
        "kRgb24I601DotProdConstants = {{0, 25, 129, 66},\n"
        "                                                               0x1080};\n\n"
        "static const struct RgbConstants kRawI601Constants",
        "kRgb24I601DotProdConstants = {{0, 25, 129, 66},\n"
        "                                                               0x1080};\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "static const struct RgbConstants kRawI601Constants",
    )
    rep(
        "static const struct RgbConstants kRawI601Constants = {{66, 129, 25, 0}, 0x1080};\n"
        "static const struct RgbConstants kRawI601DotProdConstants",
        "static const struct RgbConstants kRawI601Constants = {{66, 129, 25, 0}, 0x1080};\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n"
        "static const struct RgbConstants kRawI601DotProdConstants",
    )
    rep(
        "kRawI601DotProdConstants = {{0, 66, 129, 25},\n"
        "                                                             0x1080};\n\n"
        "void ARGBToYRow_NEON(",
        "kRawI601DotProdConstants = {{0, 66, 129, 25},\n"
        "                                                             0x1080};\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "void ARGBToYRow_NEON(",
    )
    rep(
        "  ARGBToYMatrixRow_NEON(src_abgr, dst_yj, width, &kRawJPEGConstants);\n}\n\n"
        "void ARGBToYRow_NEON_DotProd(",
        "  ARGBToYMatrixRow_NEON(src_abgr, dst_yj, width, &kRawJPEGConstants);\n}\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "void ARGBToYRow_NEON_DotProd(",
    )
    rep(
        "  ARGBToYMatrixRow_NEON_DotProd(src_abgr, dst_yj, width, &kRawJPEGConstants);\n}\n\n"
        "// RGBA expects first value to be A and ignored, then 3 values to contain RGB.",
        "  ARGBToYMatrixRow_NEON_DotProd(src_abgr, dst_yj, width, &kRawJPEGConstants);\n}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "// RGBA expects first value to be A and ignored, then 3 values to contain RGB.",
    )
    rep(
        "  RGBAToYMatrixRow_NEON(src_bgra, dst_y, width, &kRawI601Constants);\n}\n\n"
        "void RGBAToYRow_NEON_DotProd(",
        "  RGBAToYMatrixRow_NEON(src_bgra, dst_y, width, &kRawI601Constants);\n}\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "void RGBAToYRow_NEON_DotProd(",
    )
    rep(
        "  ARGBToYMatrixRow_NEON_DotProd(src_bgra, dst_y, width,\n"
        "                                &kRawI601DotProdConstants);\n}\n\n"
        "static void RGBToYMatrixRow_NEON(",
        "  ARGBToYMatrixRow_NEON_DotProd(src_bgra, dst_y, width,\n"
        "                                &kRawI601DotProdConstants);\n}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "static void RGBToYMatrixRow_NEON(",
    )
    rep(
        "                                          4, 4, 4, 27, 6, 6, 6, 31};\n\n"
        "void ARGBGrayRow_NEON_DotProd(",
        "                                          4, 4, 4, 27, 6, 6, 6, 31};\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "void ARGBGrayRow_NEON_DotProd(",
    )
    rep(
        "      : \"cc\", \"memory\", \"v0\", \"v1\", \"v2\", \"v3\", \"v24\", \"v25\");\n}\n\n"
        "// Convert 8 ARGB pixels (32 bytes) to 8 Sepia ARGB pixels.",
        "      : \"cc\", \"memory\", \"v0\", \"v1\", \"v2\", \"v3\", \"v24\", \"v25\");\n}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "// Convert 8 ARGB pixels (32 bytes) to 8 Sepia ARGB pixels.",
    )
    rep(
        "static const uvec8 kARGBSepiaRowAlphaIndices = {3, 7, 11, 15, 19, 23, 27, 31};\n\n"
        "void ARGBSepiaRow_NEON_DotProd(",
        "static const uvec8 kARGBSepiaRowAlphaIndices = {3, 7, 11, 15, 19, 23, 27, 31};\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "void ARGBSepiaRow_NEON_DotProd(",
    )
    rep(
        "        \"v21\", \"v22\", \"v24\", \"v25\", \"v26\", \"v28\", \"v29\", \"v30\");\n}\n\n"
        "// Tranform 8 ARGB pixels (32 bytes) with color matrix.",
        "        \"v21\", \"v22\", \"v24\", \"v25\", \"v26\", \"v28\", \"v29\", \"v30\");\n}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_DOTPROD)\n\n"
        "// Tranform 8 ARGB pixels (32 bytes) with color matrix.",
    )
    rep(
        "        \"v17\", \"v18\", \"v19\", \"v22\", \"v23\", \"v24\", \"v25\");\n}\n\n"
        "void ARGBColorMatrixRow_NEON_I8MM(",
        "        \"v17\", \"v18\", \"v19\", \"v22\", \"v23\", \"v24\", \"v25\");\n}\n\n"
        "#if !defined(LIBYUV_DISABLE_NEON_I8MM)\n"
        "void ARGBColorMatrixRow_NEON_I8MM(",
    )
    rep(
        "        \"v22\", \"v23\", \"v31\");\n}\n\n"
        "// Multiply 2 rows of ARGB pixels together, 8 pixels at a time.",
        "        \"v22\", \"v23\", \"v31\");\n}\n\n"
        "#endif  // !defined(LIBYUV_DISABLE_NEON_I8MM)\n\n"
        "// Multiply 2 rows of ARGB pixels together, 8 pixels at a time.",
    )

    path.write_text(t, encoding="utf-8", newline="\n")
    print("wrote", path)


if __name__ == "__main__":
    main()
