# Plan: Split `loader.rs` into a `loader/` module tree

## Context

`src/loader.rs` is the largest Rust source file in the application (on the order of **4.8k lines**). The `ImageLoader` implementation alone is ~**1k lines**. This plan splits the file by **responsibility** and **abstraction layer**, while keeping **`crate::loader::*` public API stable** via `loader/mod.rs` re-exports.

**Branch for this work:** `refactor/split-loader-modules` (based on `main`).

---

## Current rough map (conceptual)

| Block | Role |
|-------|------|
| Preview caps + refinement pool | `PREVIEW_*`, `MONITOR_PREVIEW_CAP`, `refresh_hq_preview_monitor_cap`, `hq_preview_max_side`, `REFINEMENT_POOL` |
| Domain types | `DecodedImage`, `ImageData`, `LoadResult`, preview/tile result types, `LoaderOutput`, `RefinementRequest`, … |
| Orchestration | `ImageLoader` + impl: workers, tile queue, delayed fallback, generation, poll/cancel |
| HDR load helpers | `hdr_display_requests_sdr_preview`, placeholders, SDR fallback glue |
| Decode routing | `load_image_file`, per-format `load_*`, magic detection, `load_detected_exr` |
| HDR/SDR assembly | `make_image_data`, `make_hdr_image_data*`, capacity/tiled routing |
| Metadata | `extract_exif_thumbnail` |
| UI texture cache | `TextureCache` |
| Tiled sources | `RawImageSource`, `MemoryImageSource`, `HdrSdrTiledFallbackSource`, `TiledImageSource` impls |
| Tests | Large `#[cfg(test)] mod tests` |

---

## Target layout (`src/loader/`)

Replace monolithic `loader.rs` with **`loader/mod.rs`** and submodules. `main.rs` keeps `mod loader;` (resolves to `loader/mod.rs`).

| File / directory | Contents |
|------------------|----------|
| `loader/mod.rs` | Submodules, **re-exports** matching today’s `crate::loader` surface, thin glue only |
| `loader/preview_caps.rs` | Preview monitor cap, `PREVIEW_LIMIT`, `MONITOR_PREVIEW_CAP`, `hq_preview_max_side`, `REFINEMENT_POOL` |
| `loader/types.rs` | Decoded/tile/preview/load result types, `TiledImageSource`, `LoaderOutput`, `RefinementRequest`, … |
| `loader/orchestrator.rs` | `ImageLoader` only; private queue types (`TileRequest`, `DelayedFallbackJob`, …) colocated or `orchestrator_types.rs` |
| `loader/hdr_fallback.rs` | SDR fallback / placeholder helpers around `hdr::decode` |
| `loader/orientation.rs` | `apply_exif_orientation_*`, gain-map decode capacity helpers |
| `loader/metadata.rs` | `extract_exif_thumbnail` |
| `loader/texture_cache.rs` | `TextureCache` |
| `loader/tiled_sources.rs` | `RawImageSource`, `MemoryImageSource`, `HdrSdrTiledFallbackSource` + `TiledImageSource` impls (split further if needed) |
| `loader/decode/mod.rs` | `load_image_file`, format dispatch, `load_by_image_format`, content detection |
| `loader/decode/jpeg.rs` | JPEG + Ultra HDR / capacity path |
| `loader/decode/modern.rs` | AVIF, JXL, HEIF entry points |
| `loader/decode/hdr_formats.rs` | Radiance `.hdr`, EXR paths, deep EXR, tiled probing |
| `loader/decode/raster.rs` | PNG, WebP, GIF, PSD, static `image` paths |
| `loader/decode/detect.rs` | Magic-byte / content sniffing |
| `loader/tests/` (optional) | Split `mod tests` by topic |

**Dependency rule:** `decode/*` must **not** depend on `orchestrator`; `orchestrator` calls into `decode` as free functions.

---

## Phased execution

### Phase A — Mechanical, low risk

1. Add `docs/loader-split-plan.md` (this file).
2. Introduce `src/loader/preview_caps.rs`; move preview caps + `REFINEMENT_POOL` there.
3. Rename monolith: **`loader.rs` → `loader/mod.rs`** (delete `loader.rs`), wire `mod preview_caps` + `pub use`.
4. **`cargo check`** + **`cargo test loader::`** after each step.

### Phase B — Types and cross-cutting helpers

1. `types.rs` for all user-visible / channel payload types.
2. `orientation.rs`, `metadata.rs`, `hdr_fallback.rs`.

### Phase C — Orchestrator vs decode

1. Move **`ImageLoader`** to `orchestrator.rs`.
2. Move decode functions into `decode/` submodules; keep `load_image_file` as the single entry from orchestrator paths.

### Phase D — Polish

1. Split `tests` into `loader/tests/*.rs` or multiple `#[cfg(test)]` modules.
2. Audit `pub` / `pub(crate)` visibility; ensure Windows COM spawn stays in worker setup only.

---

## Risks

- **Visibility:** New submodules need consistent `pub(crate)` for helpers shared between decode and orchestrator.
- **`#[cfg]`:** Preserve `target_os`, `feature`, and `cfg(test)` gates (e.g. test-only JPEG helper).
- **Merge conflicts:** Prefer small PRs per phase against `refactor/split-loader-modules`.

---

## Status

| Phase | Status |
|-------|--------|
| A.1 Plan doc (`docs/loader-split-plan.md`) | Done |
| A.2 `preview_caps.rs` + `loader/mod.rs` (monolith moved under `loader/`) | Done |
| B–D | Pending |

_Update this table as work lands._
