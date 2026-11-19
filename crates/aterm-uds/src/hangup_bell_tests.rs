// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The hang-up bell's arms. A ring that must come is waited for under the one-minute hang
//! detector; a ring that must not come is judged after a later arm's ring, which the bell's one
//! thread sends after any ring an event heard earlier would have sent (both kernels hand events
//! back in the order they came).

use super::Bell;
use crate::CtlStream;
use crate::hangup::fd_peer_closed;
use aterm_spec::wait::MUST_HAPPEN;
use std::os::fd::AsRawFd as _;
use std::sync::mpsc::{Receiver, sync_channel};

/// A ring channel as a park holds one: a single slot, so rings not yet read coalesce.
fn ring() -> (std::sync::mpsc::SyncSender<()>, Receiver<()>) {
    sync_channel(1)
}

#[track_caller]
fn rung(rings: &Receiver<()>, what: &str) {
    rings
        .recv_timeout(MUST_HAPPEN)
        .unwrap_or_else(|e| panic!("{what}: no ring within the hang detector ({e:?})"));
}

/// How many rings wait unread.
fn unread(rings: &Receiver<()>) -> usize {
    rings.try_iter().count()
}

/// A bell of the test's own, started as a serving lane starts the process's before any park arms
/// it.
#[track_caller]
fn started() -> Bell {
    let bell = Bell::new();
    assert!(bell.start(), "the hang-up bell starts on this platform");
    bell
}

/// A CLOSE RINGS, and a stream already hung up when armed rings at once; the predicate a woken
/// park asks agrees with the ring.
/// NEGATIVE CONTROL: with the queue's wait hearing no hang-up, neither ring comes within the hang
/// detector.
#[test]
fn a_peer_that_closes_rings_its_arm() {
    let bell = started();
    let (near, far) = CtlStream::pair().expect("pair");
    let (tx, rx) = ring();
    let armed = bell.arm(&near, tx).expect("arm");
    assert!(armed.ringing(), "a started bell serves its arm");
    drop(far);
    rung(&rx, "the closed peer's arm");
    assert!(fd_peer_closed(near.as_raw_fd()), "the ring means hung up");
    drop(armed);

    let (near, far) = CtlStream::pair().expect("pair");
    drop(far);
    let (tx, rx) = ring();
    let _armed = bell.arm(&near, tx).expect("arm");
    rung(&rx, "an arm of a stream already hung up");
}

/// A PEER'S WRITE-SIDE SHUTDOWN IS NO HANG-UP: A's peer shuts its write half, then B's peer
/// closes; once B rings, A has rung nothing, and the predicate agrees.
/// NEGATIVE CONTROL: ringing on every write-filter event, not on `EV_EOF` alone, rings A as soon
/// as it is armed (its socket is writable), and A's count is 1.
#[test]
fn a_write_side_shutdown_never_rings_where_a_close_does() {
    let bell = started();
    let (a, a_peer) = CtlStream::pair().expect("pair");
    let (b, b_peer) = CtlStream::pair().expect("pair");
    let (a_tx, a_rx) = ring();
    let (b_tx, b_rx) = ring();
    let _a_armed = bell.arm(&a, a_tx).expect("arm A");
    let _b_armed = bell.arm(&b, b_tx).expect("arm B");
    a_peer
        .shutdown(std::net::Shutdown::Write)
        .expect("A's peer half-closes");
    drop(b_peer);
    rung(&b_rx, "B, whose peer closed");
    assert_eq!(unread(&a_rx), 0, "A's peer only shut its write half");
    assert!(
        !fd_peer_closed(a.as_raw_fd()),
        "nor does the predicate call it hung up"
    );
    drop(a_peer);
}

/// AN ARM DROPPED BEFORE THE CLOSE NEVER RINGS, and stands armed until the drop.
/// NEGATIVE CONTROL: a drop that leaves its registration in the queue and its arm standing still
/// shows the descriptor armed, and rings the dropped arm's channel at the close.
#[test]
fn an_arm_dropped_before_the_close_never_rings() {
    let bell = started();
    let (near, far) = CtlStream::pair().expect("pair");
    let (tx, rx) = ring();
    let armed = bell.arm(&near, tx).expect("arm");
    assert!(bell.armed_for_test(near.as_raw_fd()), "the arm stands");
    drop(armed);
    assert!(
        !bell.armed_for_test(near.as_raw_fd()),
        "the drop took the arm out"
    );
    drop(far);
    let (later, later_peer) = CtlStream::pair().expect("pair");
    let (later_tx, later_rx) = ring();
    let _later = bell.arm(&later, later_tx).expect("arm the later stream");
    drop(later_peer);
    rung(&later_rx, "the later stream's arm");
    assert_eq!(unread(&rx), 0, "the dropped arm rang");
}

/// A DESCRIPTOR NUMBER REUSED AFTER A DROP RINGS ITS NEW ARM: `dup2` puts a new socket at the
/// dropped arm's number (closing the old one there), the new arm at that number rings when the
/// new peer closes, and the old arm, its peer closed too, never rings. A number armed twice at
/// once is refused, and the first arm still rings.
/// NEGATIVE CONTROL: a drop that leaves its arm standing keeps the number armed, and the new arm
/// is refused.
#[test]
fn a_number_reused_after_a_drop_rings_its_new_arm() {
    let bell = started();
    let (reused, old_peer) = CtlStream::pair().expect("pair");
    let (old_tx, old_rx) = ring();
    drop(bell.arm(&reused, old_tx).expect("arm the old socket"));
    let (new, new_peer) = CtlStream::pair().expect("pair");
    // SAFETY: both descriptors are open and owned here; `dup2` closes the old socket at
    // `reused`'s number and puts the new one there, which `reused` then owns.
    let placed = unsafe { libc::dup2(new.as_raw_fd(), reused.as_raw_fd()) };
    assert_eq!(
        placed,
        reused.as_raw_fd(),
        "the new socket takes the old number"
    );
    drop(new);
    drop(old_peer);
    let (new_tx, new_rx) = ring();
    let armed = bell
        .arm(&reused, new_tx)
        .expect("arm the new socket at the reused number");
    let (twice_tx, _twice_rx) = ring();
    assert_eq!(
        bell.arm(&reused, twice_tx).err().map(|e| e.kind()),
        Some(std::io::ErrorKind::AlreadyExists),
        "a number armed twice at once is refused"
    );
    drop(new_peer);
    rung(&new_rx, "the new arm at the reused number");
    assert_eq!(unread(&old_rx), 0, "the dropped arm rang");
    drop(armed);
}

/// A HUNG-UP ARM RINGS ONCE, not at every wait of the bell's queue while it stays armed: after A
/// rang and its ring was read, B's peer closes; once B rings, A has rung no more.
/// NEGATIVE CONTROL: removing the per-arm notification guard lets repeated hang-up events ring
/// again after the first wake was consumed; the explicit duplicate-event test exercises this.
#[test]
fn a_hung_up_arm_rings_once() {
    let bell = started();
    let (a, a_peer) = CtlStream::pair().expect("pair");
    let (a_tx, a_rx) = ring();
    let a_armed = bell.arm(&a, a_tx).expect("arm A");
    drop(a_peer);
    rung(&a_rx, "A, whose peer closed");
    let (b, b_peer) = CtlStream::pair().expect("pair");
    let (b_tx, b_rx) = ring();
    let b_armed = bell.arm(&b, b_tx).expect("arm B");
    drop(b_peer);
    rung(&b_rx, "B, whose peer closed");
    let _arms = a_armed.shared.lock_arms();
    assert!(
        a_armed.ringing() && b_armed.ringing(),
        "the bell still serves both arms; these are hang-up rings, not thread-end wakes"
    );
    assert_eq!(unread(&a_rx), 0, "A rang again while it stayed armed");
}

/// Repeated kernel delivery for one id cannot ring again after its first wake was consumed.
/// A fresh registration can ring, an old id cannot reach it, and ending the thread still wakes
/// the already-notified arm. These deliveries are explicit, with no scheduling absence window.
#[test]
fn duplicate_hangup_events_notify_each_registration_once() {
    let bell = started();
    let (near, _peer) = CtlStream::pair().expect("pair");
    let (tx, rx) = ring();
    let armed = bell.arm(&near, tx).expect("arm");
    let first_id = armed.id;
    armed.shared.ring(first_id);
    rung(&rx, "the first hang-up event");
    armed.shared.ring(first_id);
    assert_eq!(unread(&rx), 0, "a repeated event rang the same arm twice");
    assert!(armed.ringing(), "the thread is still serving this bell");
    drop(armed);

    let (next_tx, next_rx) = ring();
    let next = bell.arm(&near, next_tx).expect("rearm the same descriptor");
    assert_ne!(next.id, first_id, "a new registration has a fresh identity");
    next.shared.ring(first_id);
    assert_eq!(unread(&next_rx), 0, "a retired id reached the new arm");
    next.shared.ring(next.id);
    rung(&next_rx, "the fresh arm's first event");
    bell.end_thread_for_test();
    rung(&next_rx, "thread end after a consumed hang-up notification");
    assert!(!next.ringing(), "the final wake reports the ended thread");
}

/// A BELL WHOSE THREAD ENDED rings every standing arm as it ends, says so to each
/// ([`super::Armed::ringing`]), and arms nothing more, so its callers fall back to asking for
/// themselves.
/// NEGATIVE CONTROL: an end that leaves the bell marked live arms the later stream.
#[test]
fn a_bell_whose_thread_ended_rings_its_arms_and_arms_no_more() {
    let bell = started();
    let (near, _far) = CtlStream::pair().expect("pair");
    let (tx, rx) = ring();
    let armed = bell.arm(&near, tx).expect("arm");
    bell.end_thread_for_test();
    rung(&rx, "the arm standing as the thread ended");
    assert!(!armed.ringing(), "the arm hears its bell has ended");
    let (later, _later_peer) = CtlStream::pair().expect("pair");
    let (later_tx, _later_rx) = ring();
    assert!(
        bell.arm(&later, later_tx).is_err(),
        "an ended bell arms nothing"
    );
    drop(armed);
}

/// AN ARM NEVER STARTS ITS BELL, SO IT NEVER WAITS FOR A SPAWN IN FLIGHT: a park arms under the
/// lock of the state it guards (a session's Inbox), and a start opens its kernel queue under the
/// spawn lock, which on macOS waits for every spawn in flight (seconds under load, for good while
/// `syspolicyd` is stuck). With a spawn held in flight on the test's thread, an arm of a started
/// bell answers Ok and an arm of a bell never started answers Err, both within the hang
/// detector, and the bell never started still arms nothing after the spawn ends.
/// NEGATIVE CONTROL: an arm that starts its bell at the first arm (the lazy start) arms the bell
/// never started; on macOS it first waits for the held spawn, past the hang detector.
#[test]
fn an_arm_never_starts_its_bell_nor_waits_for_a_spawn_in_flight() {
    let live = started();
    let unstarted = Bell::new();
    let (a, _a_peer) = CtlStream::pair().expect("pair");
    let (b, _b_peer) = CtlStream::pair().expect("pair");
    let (tx, _rx) = ring();
    let (answer_tx, answer) = sync_channel(1);
    std::thread::scope(|scope| {
        let in_flight = crate::spawn_lock::spawning();
        scope.spawn(|| {
            let armed = (
                live.arm(&a, tx.clone()).is_ok(),
                unstarted.arm(&b, tx.clone()).is_ok(),
            );
            let _ = answer_tx.send(armed);
        });
        let armed = answer
            .recv_timeout(MUST_HAPPEN)
            .expect("the arms answer while a spawn is in flight");
        drop(in_flight);
        assert_eq!(
            armed,
            (true, false),
            "a started bell arms; one never started arms nothing"
        );
    });
    assert!(
        unstarted.arm(&b, tx).is_err(),
        "an arm left the bell never started unstarted"
    );
}
