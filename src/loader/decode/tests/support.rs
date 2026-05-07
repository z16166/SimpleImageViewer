//! Shared knobs for decode tests (`TILED_THRESHOLD` overrides).

use std::sync::{LazyLock, Mutex, MutexGuard};

static TILED_THRESHOLD_TEST_LOCK: LazyLock<Mutex<()>> =
    LazyLock::new(|| Mutex::new(()));

pub(crate) struct TiledThresholdOverride {
    old_threshold: u64,
}

impl TiledThresholdOverride {
    pub(crate) fn set(value: u64) -> Self {
        let old_threshold =
            crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
        crate::tile_cache::TILED_THRESHOLD.store(value, std::sync::atomic::Ordering::Relaxed);
        Self { old_threshold }
    }
}

impl Drop for TiledThresholdOverride {
    fn drop(&mut self) {
        crate::tile_cache::TILED_THRESHOLD
            .store(self.old_threshold, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) fn lock_tiled_threshold_for_test() -> MutexGuard<'static, ()> {
    TILED_THRESHOLD_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
