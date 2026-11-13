// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A ring wakes every subscription on its list and no other, and N rings
/// between two waits are one wake.
#[test]
fn a_ring_wakes_its_lists_watchers_once_and_no_others() {
    let (a, b) = (Arc::<Watchers>::default(), Arc::<Watchers>::default());
    let on_a = Subscription::single(&a);
    let on_b = Subscription::single(&b);
    for _ in 0..3 {
        a.ring_watchers();
    }
    assert!(on_a.wait(Duration::ZERO), "a's watcher is woken");
    assert!(!on_a.wait(Duration::ZERO), "three rings are one wake");
    assert!(!on_b.wait(Duration::ZERO), "b's watcher is not woken by a");
}

/// One subscription on two lists is woken by either, and leaves exactly the
/// list it unwatches; a drop leaves the rest.
#[test]
fn a_subscription_watches_many_lists_and_leaves_exactly_its_own() {
    let (a, b) = (Arc::<Watchers>::default(), Arc::<Watchers>::default());
    let other = Subscription::single(&a);
    let mut sub = Subscription::new();
    assert!(!sub.wait(Duration::ZERO), "registered on nothing");
    sub.watch(&a);
    sub.watch(&a);
    sub.watch(&b);
    assert_eq!((a.count(), b.count()), (2, 1), "a second watch is a no-op");
    b.ring_watchers();
    assert!(sub.wait(Duration::ZERO), "woken by either list");
    sub.unwatch(&a);
    assert_eq!(a.count(), 1, "only its own slot left a");
    a.ring_watchers();
    assert!(!sub.wait(Duration::ZERO), "a no longer wakes it");
    assert!(other.wait(Duration::ZERO), "a still wakes the other");
    drop(sub);
    assert_eq!(b.count(), 0, "the drop left b");
    b.ring_watchers();
}

/// A FULL SLOT NEVER LOSES A DIRTY FLAG. A ring into a slot that already
/// holds a wake sends nothing, so the flag is the only record of which list
/// rang: it is set whether or not the send lands, and the one pending wake
/// finds both lists' flags.
#[test]
fn a_full_slot_never_loses_a_dirty_flag() {
    let (a, b, c) = (
        Arc::<Watchers>::default(),
        Arc::<Watchers>::default(),
        Arc::<Watchers>::default(),
    );
    let mut sub = Subscription::new();
    let flags = [sub.watch(&a), sub.watch(&b), sub.watch(&c)];
    assert!(
        flags.iter().all(|f| f.swap(false, Ordering::AcqRel)),
        "a registration's flag starts set"
    );
    assert!(Arc::ptr_eq(&sub.watch(&a), &flags[0]), "one flag per list");
    a.ring_watchers();
    b.ring_watchers();
    assert!(sub.wait(Duration::ZERO), "one wake is pending");
    assert!(
        !sub.wait(Duration::ZERO),
        "b's ring found the slot full and sent nothing"
    );
    let rang: Vec<bool> = flags
        .iter()
        .map(|f| f.swap(false, Ordering::AcqRel))
        .collect();
    assert_eq!(
        rang,
        [true, true, false],
        "both rings are on their flags, and c's is not"
    );
}

/// A HELD LIST RINGS ONCE, AT ITS LAST HOLD'S RELEASE. Rings made under a
/// hold wake nobody and set no flag; the inner hold's release rings
/// nothing while the outer one stands; the outer one's rings the watchers
/// once, each flag set.
#[test]
fn a_held_list_rings_once_at_its_last_holds_release() {
    let list = Arc::<Watchers>::default();
    let mut sub = Subscription::new();
    let dirty = sub.watch(&list);
    dirty.store(false, Ordering::Release);
    let mut screen = Subscription::new();
    let frame_dirty = screen.watch_frame(&list);
    frame_dirty.store(false, Ordering::Release);

    list.ring_watchers();
    assert!(
        sub.wait(Duration::ZERO),
        "the negative control: no hold, a wake"
    );
    assert!(screen.wait(Duration::ZERO));
    dirty.store(false, Ordering::Release);
    frame_dirty.store(false, Ordering::Release);

    let outer = list.hold_rings();
    let inner = list.hold_rings();
    list.ring_watchers();
    list.ring_watchers();
    assert!(!sub.wait(Duration::ZERO), "a held ring wakes nobody");
    assert!(!dirty.load(Ordering::Acquire), "and sets no flag");
    drop(inner);
    assert!(!sub.wait(Duration::ZERO), "the outer hold still stands");
    drop(outer);
    assert!(sub.wait(Duration::ZERO), "the last release rings");
    assert!(!sub.wait(Duration::ZERO), "once");
    assert!(dirty.swap(false, Ordering::AcqRel), "with the flag set");
    assert!(screen.wait(Duration::ZERO), "every watcher");
    frame_dirty.store(false, Ordering::Release);

    let held = list.hold_rings();
    list.ring_frame_watchers();
    assert!(
        !screen.wait(Duration::ZERO),
        "a held frame ring wakes nobody"
    );
    drop(held);
    assert!(
        screen.wait(Duration::ZERO),
        "its release rings the frame watcher"
    );
    assert!(frame_dirty.load(Ordering::Acquire));
    assert!(!sub.wait(Duration::ZERO), "and no other");
    assert!(!dirty.load(Ordering::Acquire));
}

/// A ring never blocks on a watcher that stopped reading: its slot holds
/// one pending wake and the rest are dropped.
#[test]
fn a_ring_never_blocks_on_a_watcher_that_stopped_reading() {
    let list = Arc::<Watchers>::default();
    let sub = Subscription::single(&list);
    for _ in 0..10_000 {
        list.ring_watchers();
    }
    assert!(sub.wait(Duration::ZERO));
    assert!(!sub.wait(Duration::ZERO));
}

/// A RECORD WAKES ITS SESSION'S WATCHERS AND NO OTHER'S, AND IS COUNTED:
/// the timeline and the turn ledger of one [`Observed`] ring the list it
/// names as records, each stepping its record count once, and a plain ring
/// (the PTY reader's) steps no count.
#[test]
fn a_record_wakes_its_sessions_watchers_and_no_others() {
    let (one, two) = (Observed::default(), Observed::default());
    let on_one = Subscription::single(&one.watchers);
    let on_two = Subscription::single(&two.watchers);
    one.timeline.lock().unwrap().record("cwd-change");
    assert!(on_one.wait(Duration::ZERO), "a timeline record wakes it");
    assert!(!on_two.wait(Duration::ZERO), "another session's does not");
    assert_eq!(
        (one.watchers.records_rung(), two.watchers.records_rung()),
        (1, 0)
    );
    one.turns
        .lock()
        .unwrap()
        .record(crate::subscribe::digest_fixture::turn_record(1), || 2);
    assert!(on_one.wait(Duration::ZERO), "a turn record wakes it");
    assert!(!on_two.wait(Duration::ZERO));
    assert_eq!(one.watchers.records_rung(), 2, "a turn record is counted");
    one.watchers.ring_watchers();
    assert!(on_one.wait(Duration::ZERO), "an output ring wakes it");
    assert_eq!(one.watchers.records_rung(), 2, "and is no record");
}
