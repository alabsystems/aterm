// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the mailbox the walker's publications reach the loop through (`diskwalk-mailbox`).

use super::*;

/// THE LOOP'S ABSORB NEVER WAITS ON THE WALKER: while the walker holds the mailbox part-way
/// through a store, `absorb` returns at once with nothing and leaves the board on the earlier
/// publication, and the held store arrives at the next absorb, once. The absorb runs on its own
/// thread and its answer is waited for by a one-minute hang detector, which an absorb that
/// waits for the mailbox's lock (as a receive from std's channel waits on a send part-way
/// through its slot) fails.
#[test]
fn absorb_returns_at_once_while_the_walker_holds_the_mailbox() {
    const MUST_HAPPEN: Duration = Duration::from_secs(60);
    let shared: &'static Shared = Box::leak(Box::new(Shared::new()));
    let mailbox = Arc::new(Mailbox::default());
    let earlier = Arc::new(Published::default());
    mailbox.store(Arc::clone(&earlier), || {});
    let first = shared.absorb(&mailbox).expect("the earlier store");
    assert!(Arc::ptr_eq(&first, &earlier));
    let held = Arc::new(Published::default());
    let (took_lock, holding) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = {
        let (mailbox, held) = (Arc::clone(&mailbox), Arc::clone(&held));
        std::thread::spawn(move || {
            let mut slot = mailbox.0.lock_or_recover();
            took_lock.send(()).expect("the test waits for the lock");
            let _ = released.recv_timeout(MUST_HAPPEN);
            *slot = Some(held);
        })
    };
    holding
        .recv_timeout(MUST_HAPPEN)
        .expect("the holder takes the mailbox");
    assert!(
        matches!(mailbox.0.try_lock(), Err(TryLockError::WouldBlock)),
        "the control: the walker holds the mailbox"
    );
    let (answer, answered) = std::sync::mpsc::channel();
    {
        let mailbox = Arc::clone(&mailbox);
        std::thread::spawn(move || {
            let _ = answer.send(shared.absorb(&mailbox));
        });
    }
    let took = answered
        .recv_timeout(MUST_HAPPEN)
        .expect("absorb returns while the walker holds the mailbox");
    assert!(took.is_none(), "a held mailbox gives nothing");
    let board = shared.board.lock_or_recover().clone();
    assert!(
        board.is_some_and(|b| Arc::ptr_eq(&b, &earlier)),
        "the board keeps the earlier publication"
    );
    release.send(()).expect("the holder waits for the release");
    holder.join().expect("the holder stores");
    let next = shared.absorb(&mailbox).expect("the held store arrives");
    assert!(Arc::ptr_eq(&next, &held));
    let board = shared.board.lock_or_recover().clone();
    assert!(board.is_some_and(|b| Arc::ptr_eq(&b, &held)));
    assert!(
        shared.absorb(&mailbox).is_none(),
        "each store is taken once"
    );
}

/// THE MAILBOX KEEPS ONLY THE NEWEST: of two stores before an absorb the loop takes the second,
/// the second lets the first go, and the board is the second.
#[test]
fn absorb_keeps_only_the_newest() {
    let shared = Shared::new();
    let mailbox = Mailbox::default();
    let first = Arc::new(Published::default());
    let second = Arc::new(Published::default());
    mailbox.store(Arc::clone(&first), || {});
    mailbox.store(Arc::clone(&second), || {});
    assert_eq!(
        Arc::strong_count(&first),
        1,
        "the second store lets the first go"
    );
    let took = shared.absorb(&mailbox).expect("a publication");
    assert!(Arc::ptr_eq(&took, &second));
    let board = shared.board.lock_or_recover().clone();
    assert!(board.is_some_and(|b| Arc::ptr_eq(&b, &second)));
    assert!(
        shared.absorb(&mailbox).is_none(),
        "each store is taken once"
    );
}

/// THE ABSORB A STORE'S WAKE BRINGS TAKES THAT STORE: the wake runs once the store is in the
/// mailbox and the walker has let the mailbox go, so the loop's absorb, run here inside the
/// wake, takes it. A wake before the store, or one sent while the walker still holds the
/// mailbox, brings an absorb that takes nothing, and no later wake comes for that store.
#[test]
fn the_absorb_a_stores_wake_brings_takes_that_store() {
    let shared = Shared::new();
    let mailbox = Mailbox::default();
    let published = Arc::new(Published::default());
    let mut woken = None;
    mailbox.store(Arc::clone(&published), || {
        woken = Some(shared.absorb(&mailbox));
    });
    let took = woken.expect("the store wakes the loop");
    assert!(
        took.is_some_and(|took| Arc::ptr_eq(&took, &published)),
        "the absorb the wake brings takes the store"
    );
}

/// A POISONED MAILBOX STILL DELIVERS: a thread that panics while it holds the mailbox, after
/// its store, poisons the lock, and the next absorb takes that store, once, rather than nothing
/// from then on.
#[test]
fn absorb_takes_a_store_from_a_poisoned_mailbox() {
    let shared = Shared::new();
    let mailbox = Arc::new(Mailbox::default());
    let published = Arc::new(Published::default());
    let poisoner = {
        let (mailbox, published) = (Arc::clone(&mailbox), Arc::clone(&published));
        std::thread::spawn(move || {
            let mut slot = mailbox.0.lock_or_recover();
            *slot = Some(published);
            panic!("a panic while the walker holds the mailbox");
        })
    };
    assert!(poisoner.join().is_err(), "the poisoner panics");
    assert!(
        mailbox.0.is_poisoned(),
        "the control: the mailbox is poisoned"
    );
    let took = shared
        .absorb(&mailbox)
        .expect("the poisoned mailbox's store");
    assert!(Arc::ptr_eq(&took, &published));
    assert!(
        shared.absorb(&mailbox).is_none(),
        "each store is taken once"
    );
}
