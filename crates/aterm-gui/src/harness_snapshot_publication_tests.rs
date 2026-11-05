// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_observer_sees_only_a_landed_snapshot_after_both_publication_locks_are_released() {
    const SID: &str = "test-observer-after-landed-snapshot-f7da9286";
    struct PublishedEntry;
    impl Drop for PublishedEntry {
        fn drop(&mut self) {
            publish_snapshot(SID, None);
        }
    }
    let _entry = PublishedEntry;
    publish_snapshot(SID, None);
    let world = Arc::new(World::default());
    let mut hooks = hooks(&world, parking_body(&world));
    let kept = Arc::new(Mutex::new(Kept::default()));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured_kept = Arc::clone(&kept);
    let captured_calls = Arc::clone(&calls);
    let captured_observed = Arc::clone(&observed);
    hooks.acts.observe_snapshot = Arc::new(move |snapshot| {
        assert_eq!(snapshot.tab, SID);
        assert_eq!(published_snapshot(SID).as_ref(), Some(snapshot));
        let actual = captured_kept.try_lock().expect("Kept lock is released");
        assert_eq!(actual.snapshot.as_ref(), Some(snapshot));
        captured_calls.fetch_add(1, Ordering::SeqCst);
        captured_observed.lock().unwrap().push(snapshot.clone());
    });
    let notified_calls = Arc::clone(&calls);
    hooks.note = Arc::new(move |note| {
        if let HostNote::AgentPublished { sid } = note {
            assert_eq!(sid, SID);
            assert_eq!(
                notified_calls.load(Ordering::SeqCst),
                1,
                "the observer runs before the publication notification"
            );
        }
    });
    let landed = snap(SID);
    assert!(land_snapshot(SID, &kept, &hooks, landed.clone(), |_| false));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*observed.lock().unwrap(), vec![landed.clone()]);

    let mut stale = landed.clone();
    stale.pid += 1;
    assert!(!land_snapshot(SID, &kept, &hooks, stale, |_| true));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a rejected stale snapshot supplies no observation authority"
    );
    assert_eq!(published_snapshot(SID), Some(landed));
}
