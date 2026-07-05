use super::should_spawn_load_task;
use crate::loader::{
    DecodeProfile, ImageLoader, InFlightLoad, LoadIntent, MAX_IMG_LOADER_THREADS,
    decode_profile_stub,
};
use std::collections::HashMap;

#[test]
fn should_spawn_load_task_only_for_profile_upgrade() {
    let mut loading = HashMap::new();
    let base = decode_profile_stub();

    assert!(should_spawn_load_task(&mut loading, 7, base.clone()));
    assert_eq!(
        loading.get(&7),
        Some(&InFlightLoad {
            profile: base.clone(),
        })
    );

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
        assert!(should_spawn_load_task(
            &mut loading,
            index,
            neighbor()
        ));
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

    loader.cancel_outside_prefetch_window(50, 100, 2);

    let loading = loader.loading.lock();
    assert_eq!(loading.len(), 1);
    assert!(loading.contains_key(&50));
    assert!(!loading.contains_key(&0));
    assert!(!loading.contains_key(&3));
    assert!(!loading.contains_key(&99));
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
