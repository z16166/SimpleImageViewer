// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Dedicated rayon pool for PSD/PSB per-layer channel decode.
//!
//! Kept separate from the img-loader pool, [`crate::loader::REFINEMENT_POOL`], and
//! [`crate::loader::DIRECTORY_TREE_STRIP_POOL`] so nested `par_iter` during
//! composite does not steal those workers or inherit their saturation.

use std::sync::LazyLock;

/// Minimum workers: enough overlap for multi-layer ZIP/PackBits without a
/// thread-per-core spike while a large PSB is open.
const PSD_LAYER_DECODE_POOL_MIN_THREADS: usize = 2;
/// Hard cap on concurrent layer-decode workers (and thus peak decode RSS).
const PSD_LAYER_DECODE_POOL_MAX_THREADS: usize = 4;

/// Use this pool when the layer stack has at least this many records.
pub(crate) const PARALLEL_LAYER_DECODE_MIN: usize = 2;

/// Bounded pool for `decode_one_layer` parallelism during composite.
pub(crate) static PSD_LAYER_DECODE_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    let n = std::thread::available_parallelism()
        .map(|cores| {
            cores.get().div_ceil(4).clamp(
                PSD_LAYER_DECODE_POOL_MIN_THREADS,
                PSD_LAYER_DECODE_POOL_MAX_THREADS,
            )
        })
        .unwrap_or(PSD_LAYER_DECODE_POOL_MIN_THREADS);
    match rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .thread_name(|i| format!("psd-layer-decode-{i}"))
        .build()
    {
        Ok(pool) => pool,
        Err(e) => {
            log::error!(
                "[PsdLayerDecode] Failed to create pool ({n} threads): {e}. \
                 Falling back to 1-thread pool."
            );
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|i| format!("psd-layer-decode-fallback-{i}"))
                .build()
                .unwrap_or_else(|final_err| {
                    log::error!(
                        "[PsdLayerDecode] Fallback 1-thread pool failed: {final_err}; \
                         using default builder"
                    );
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(1)
                        .build()
                        .expect("rayon single-thread pool")
                })
        }
    }
});
