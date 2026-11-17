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

/// A PUBLISHED SNAPSHOT RINGS THE FOOTER WHEN IT CHANGES (the footer cause design's step C8,
/// [`crate::host_facts::Bell::Snapshot`]): the restart offer and the tap's room a footer read
/// shows are judged by a tab's published snapshot, so a first publish, a changed one and a
/// withdrawal each ring the footer's snapshot bell once, and a publish of the snapshot already
/// held, or a withdrawal of none, rings nothing.
///
/// NEGATIVE CONTROL (run 2026-11-16): with the ring in `publish_snapshot_from` removed, the test
/// fails at "a first publish rings" with `left: [] right: [Snapshot]`.
#[test]
fn a_published_snapshot_rings_the_footer_when_it_changes() {
    use crate::host_facts::{Bell, take_rung};
    const SID: &str = "test-snapshot-bell-3c1e0b7a";
    publish_snapshot(SID, None);
    let _ = take_rung();
    let first = snap(SID);
    publish_snapshot(SID, Some(&first));
    assert_eq!(take_rung(), [Bell::Snapshot], "a first publish rings");
    publish_snapshot(SID, Some(&first));
    assert_eq!(
        take_rung(),
        Vec::<Bell>::new(),
        "the same snapshot again rings nothing"
    );
    let mut moved = first.clone();
    moved.pid += 1;
    publish_snapshot(SID, Some(&moved));
    assert_eq!(take_rung(), [Bell::Snapshot], "a changed snapshot rings");
    publish_snapshot(SID, None);
    assert_eq!(take_rung(), [Bell::Snapshot], "a withdrawal rings");
    publish_snapshot(SID, None);
    assert_eq!(
        take_rung(),
        Vec::<Bell>::new(),
        "withdrawing none rings nothing"
    );
}
