use super::should_spawn_load_task;
use crate::loader::{DecodeProfile, InFlightLoad, LoadIntent, decode_profile_stub};
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
    use crate::settings::RawDemosaicMode;

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
