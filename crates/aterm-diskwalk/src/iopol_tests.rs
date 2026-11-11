// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn thread_policy_round_trips_through_the_kernel() {
    std::thread::spawn(|| {
        set_thread_materialize_policy(MaterializePolicy::On).unwrap();
        assert_eq!(thread_materialize_policy(), Some(MaterializePolicy::On));
        set_thread_materialize_policy(MaterializePolicy::Off).unwrap();
        assert_eq!(thread_materialize_policy(), Some(MaterializePolicy::Off));
        main_thread_never_materializes().unwrap();
        assert_eq!(thread_materialize_policy(), Some(MaterializePolicy::Off));
    })
    .join()
    .unwrap();
}

#[test]
fn a_thread_scoped_policy_does_not_leak_into_a_sibling_thread() {
    let sibling = std::thread::spawn(|| {
        set_thread_materialize_policy(MaterializePolicy::On).unwrap();
        thread_materialize_policy()
    })
    .join()
    .unwrap();
    assert_eq!(sibling, Some(MaterializePolicy::On));
    let fresh = std::thread::spawn(thread_materialize_policy)
        .join()
        .unwrap();
    assert_ne!(
        fresh,
        Some(MaterializePolicy::On),
        "a sibling's thread-scoped ON must not reach a thread that never set it"
    );
}
