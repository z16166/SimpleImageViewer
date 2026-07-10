use super::should_spawn_load_task;
use crate::loader::{
    DecodeProfile, ImageLoader, LoadIntent, MAX_IMG_LOADER_THREADS, decode_profile_stub,
};
use std::collections::HashMap;

#[test]
fn should_spawn_load_task_only_for_profile_upgrade() {
    let mut loading = HashMap::new();
    let base = decode_profile_stub();

    assert!(should_spawn_load_task(&mut loading, 7, base.clone()));
    assert_eq!(loading.get(&7).map(|e| &e.profile), Some(&base));

    // Same profile should not schedule duplicate load task.
    assert!(!should_spawn_load_task(&mut loading, 7, base.clone()));
    assert!(loading.contains_key(&7));

    let upgraded = DecodeProfile {
        load_intent: LoadIntent::Current,
        ..base.clone()
    };
    assert!(should_spawn_load_task(&mut loading, 7, upgraded));
    assert!(loading.contains_key(&7));
}

#[test]
fn should_spawn_load_task_supersedes_on_profile_downgrade() {
    use crate::loader::{DecodeProfile, ProfileSpawnRelation, profile_spawn_relation};

    let mut loading = HashMap::new();
    let hq = DecodeProfile {
        raw_high_quality: true,
        ..decode_profile_stub()
    };
    assert!(should_spawn_load_task(&mut loading, 3, hq.clone()));

    let sdr = DecodeProfile {
        raw_high_quality: false,
        ..decode_profile_stub()
    };
    assert_eq!(
        profile_spawn_relation(&hq, &sdr),
        ProfileSpawnRelation::Downgrade
    );
    assert!(should_spawn_load_task(&mut loading, 3, sdr.clone()));
    assert_eq!(
        loading.get(&3).map(|e| e.profile.raw_high_quality),
        Some(false)
    );
}

#[test]
fn should_spawn_load_task_rejects_neighbor_prefetch_at_soft_cap() {
    let mut loading = HashMap::new();
    let neighbor = || DecodeProfile {
        load_intent: LoadIntent::NeighborPrefetch,
        ..decode_profile_stub()
    };

    for index in 0..MAX_IMG_LOADER_THREADS {
        assert!(should_spawn_load_task(&mut loading, index, neighbor()));
    }
    assert_eq!(loading.len(), MAX_IMG_LOADER_THREADS);

    assert!(!should_spawn_load_task(
        &mut loading,
        MAX_IMG_LOADER_THREADS,
        neighbor()
    ));
    assert_eq!(loading.len(), MAX_IMG_LOADER_THREADS);

    let upgraded = DecodeProfile {
        load_intent: LoadIntent::Current,
        ..neighbor()
    };
    assert!(should_spawn_load_task(&mut loading, 0, upgraded));
    assert_eq!(loading.len(), MAX_IMG_LOADER_THREADS);
}

#[test]
fn try_note_capacity_requeue_rejects_fourth_attempt() {
    let mut loader = ImageLoader::new();
    let index = 4;
    assert!(loader.try_note_capacity_requeue(index));
    assert!(loader.try_note_capacity_requeue(index));
    assert!(loader.try_note_capacity_requeue(index));
    assert_eq!(loader.test_capacity_requeue_count(index), 3);
    assert!(!loader.try_note_capacity_requeue(index));
    assert_eq!(loader.test_capacity_requeue_count(index), 3);
    loader.clear_capacity_requeue(index);
    assert_eq!(loader.test_capacity_requeue_count(index), 0);
    assert!(loader.try_note_capacity_requeue(index));
}

#[test]
fn cancel_outside_prefetch_window_only_touches_inflight_outside_window() {
    let mut loader = ImageLoader::new();
    loader.test_register_inflight(0);
    loader.test_register_inflight(3);
    loader.test_register_inflight(50);
    loader.test_register_inflight(99);

    loader.cancel_outside_prefetch_window(50, 100, 2, &std::collections::HashSet::new());

    let loading = loader.loading.lock();
    assert_eq!(loading.len(), 1);
    assert!(loading.contains_key(&50));
    assert!(!loading.contains_key(&0));
    assert!(!loading.contains_key(&3));
    assert!(!loading.contains_key(&99));
}

#[test]
fn cancel_outside_prefetch_window_retains_requested_indices() {
    let mut loader = ImageLoader::new();
    loader.test_register_inflight(0);
    loader.test_register_inflight(3);
    loader.test_register_inflight(50);
    let retain = std::collections::HashSet::from([3usize]);

    loader.cancel_outside_prefetch_window(50, 100, 2, &retain);

    let loading = loader.loading.lock();
    assert_eq!(loading.len(), 2);
    assert!(loading.contains_key(&50));
    assert!(loading.contains_key(&3));
    assert!(!loading.contains_key(&0));
}

#[test]
fn cancel_indices_sets_decode_cancel_flag() {
    let mut loader = ImageLoader::new();
    loader.test_register_inflight(4);
    let flag = {
        let loading = loader.loading.lock();
        loading.get(&4).expect("registered").cancel.clone()
    };
    assert!(!flag.is_cancelled());
    loader.cancel_indices([4]);
    assert!(flag.is_cancelled());
    assert!(!loader.loading.lock().contains_key(&4));
}

#[test]
fn should_spawn_upgrade_cancels_previous_flag() {
    let mut loading = HashMap::new();
    let base = decode_profile_stub();
    assert!(should_spawn_load_task(&mut loading, 1, base.clone()));
    let old_flag = loading.get(&1).unwrap().cancel.clone();
    let upgraded = DecodeProfile {
        load_intent: LoadIntent::Current,
        ..base
    };
    assert!(should_spawn_load_task(&mut loading, 1, upgraded));
    assert!(old_flag.is_cancelled());
    assert!(!loading.get(&1).unwrap().cancel.is_cancelled());
}

#[test]
fn cancel_all_clears_capacity_requeue_counts() {
    let mut loader = ImageLoader::new();
    let index = 2;
    assert!(loader.try_note_capacity_requeue(index));
    assert!(loader.try_note_capacity_requeue(index));
    assert_eq!(loader.test_capacity_requeue_count(index), 2);
    loader.cancel_all();
    assert_eq!(loader.test_capacity_requeue_count(index), 0);
}

#[test]
fn request_tile_rejects_invalid_coords() {
    let loader = ImageLoader::new();
    let pixels = std::sync::Arc::new(vec![
        0;
        crate::tile_cache::get_tile_size() as usize
            * crate::tile_cache::get_tile_size() as usize
            * 4
    ]);
    let source: std::sync::Arc<dyn crate::loader::TiledImageSource> =
        std::sync::Arc::new(crate::loader::tiled_sources::MemoryImageSource::new(
            crate::tile_cache::get_tile_size(),
            crate::tile_cache::get_tile_size(),
            pixels,
        ));

    for (priority, col, row) in [(1, 1, 0), (1, 0, 1), (1, u32::MAX, 0), (1, 0, u32::MAX)] {
        assert!(!loader.request_tile(
            1,
            decode_profile_stub(),
            priority,
            crate::loader::TileDecodeSource::Sdr(std::sync::Arc::clone(&source)),
            col,
            row,
        ));
    }

    let (lock, _) = &*loader.tile_queue;
    assert!(lock.lock().is_empty());
}

#[test]
fn tile_worker_drops_stale_invalid_tile_request_without_reporting_ready() {
    let loader = ImageLoader::new();
    let source: std::sync::Arc<dyn crate::loader::TiledImageSource> = std::sync::Arc::new(
        crate::loader::tiled_sources::MemoryImageSource::new(1, 1, std::sync::Arc::new(vec![0; 4])),
    );
    let profile = decode_profile_stub();

    {
        let (lock, cvar) = &*loader.tile_queue;
        lock.lock().push(super::types::TileRequest {
            profile_epoch: profile.profile_epoch,
            priority: 2,
            index: 9,
            col: u32::MAX,
            row: u32::MAX,
            source: crate::loader::TileDecodeSource::Sdr(std::sync::Arc::clone(&source)),
        });
        cvar.notify_one();
    }

    assert!(loader.request_tile(
        9,
        profile,
        1,
        crate::loader::TileDecodeSource::Sdr(source),
        0,
        0,
    ));

    let output = loader
        .rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("valid tile ready result after stale invalid request");
    match output {
        crate::loader::LoaderOutput::Tile(tile) => {
            assert_eq!(tile.col, 0);
            assert_eq!(tile.row, 0);
        }
        _ => panic!("expected tile-ready output"),
    }
    assert!(
        loader
            .rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err()
    );
}
