use super::should_spawn_load_task;
use crate::loader::{decode_profile_stub, DecodeProfile, InFlightLoad, LoadIntent};
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
