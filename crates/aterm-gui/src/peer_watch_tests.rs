// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use aterm_spec::wait::MUST_HAPPEN;

use super::*;
use crate::session_watch::Subscription;

fn started() -> Arc<PeerWatch> {
    PeerWatch::start().expect("the peer watch thread starts")
}

/// Whether `sub` has a wake pending, without waiting.
fn rang(sub: &Subscription) -> bool {
    sub.wait(Duration::ZERO)
}

/// A QUIET OPEN CLIENT IS KEPT; ONE THAT GOES IS A WAKE. The client's
/// hang-up, its half-close and a stray byte each settle the stream
/// `Gone` and ring the list a push loop parks on, with no timer: the
/// wait is a hang detector, and the ring is the only thing that can end
/// it. Negative control: the quiet open client is still `Watching`, with
/// no ring, after the others' polls have run past it.
#[test]
fn the_peer_watch_wakes_a_quiet_subscription_on_hangup() {
    let watch = started();
    let (quiet_server, _quiet_client) = CtlStream::pair().expect("pair");
    let quiet = watch.watch(&quiet_server).expect("watched");
    let quiet_sub = Subscription::single(quiet.wakes());
    type End = (&'static str, fn(&CtlStream));
    let ends: [End; 3] = [
        ("hang-up", |client| {
            aterm_uds::hangup::hang_up(client);
        }),
        ("half-close", |client| {
            client
                .shutdown(std::net::Shutdown::Write)
                .expect("half-close");
        }),
        ("a stray byte", |client| {
            (&*client).write_all(b"x").expect("byte");
        }),
    ];
    for (how, end) in ends {
        let (server, client) = CtlStream::pair().expect("pair");
        let guard = watch.watch(&server).expect("watched");
        let sub = Subscription::single(guard.wakes());
        assert_eq!(guard.peer(), Peer::Watching, "{how}: there before it");
        end(&client);
        assert!(sub.wait(MUST_HAPPEN), "{how}: the watch rang its list");
        assert_eq!(guard.peer(), Peer::Gone, "{how}");
    }
    assert_eq!(quiet.peer(), Peer::Watching, "a quiet open client stays");
    assert!(!rang(&quiet_sub), "and its list never rang");
}

/// A RELEASED DESCRIPTOR IS NEVER POLLED: the guard's drop returns only
/// once the thread has taken the entry out, so a stream that closes (and
/// a descriptor number reused) after it is a stranger's. Here the
/// released stream turns readable and the poll runs past it: a barrier
/// stream's hang-up must be seen, by a poll that reports every ready
/// descriptor it holds and stores all its verdicts before any ring.
/// Negative control: a stream still watched, made readable the same way,
/// is settled no later than that poll. Counterfactual, run: a release
/// that leaves the entry in settles the released stream too.
#[test]
fn peer_watch_never_polls_a_released_descriptor() {
    let watch = started();
    let (released_server, mut released_client) = CtlStream::pair().expect("pair");
    let released = watch.watch(&released_server).expect("watched");
    let released_verdict = Arc::clone(&released.watched);
    drop(released);
    let (kept_server, mut kept_client) = CtlStream::pair().expect("pair");
    let kept = watch.watch(&kept_server).expect("watched");

    released_client.write_all(b"x").expect("byte");
    kept_client.write_all(b"x").expect("byte");
    let (barrier_server, barrier_client) = CtlStream::pair().expect("pair");
    let barrier = watch.watch(&barrier_server).expect("watched");
    let sub = Subscription::single(barrier.wakes());
    crate::test_socket::hang_up(barrier_client);
    assert!(sub.wait(MUST_HAPPEN), "the barrier's hang-up rang");

    assert_eq!(
        kept.peer(),
        Peer::Gone,
        "a watched readable stream is settled"
    );
    assert_eq!(
        released_verdict.peer(),
        Peer::Watching,
        "a released stream is not polled, readable or not"
    );
}

/// A STOPPED WATCH HANDS ITS STREAMS BACK: each reads `Unwatched` and is
/// rung, so its push loop wakes and probes on a timer again, and a later
/// watch answers `None`. A client that went before the stop stays
/// `Gone`. Negative control: before the stop the open client reads
/// `Watching` with no ring.
#[test]
fn a_stopped_peer_watch_hands_its_streams_back() {
    let watch = started();
    let (open_server, _open_client) = CtlStream::pair().expect("pair");
    let open = watch.watch(&open_server).expect("watched");
    let open_sub = Subscription::single(open.wakes());
    let (gone_server, gone_client) = CtlStream::pair().expect("pair");
    let gone = watch.watch(&gone_server).expect("watched");
    let gone_sub = Subscription::single(gone.wakes());
    crate::test_socket::hang_up(gone_client);
    assert!(gone_sub.wait(MUST_HAPPEN), "the hang-up rang");
    assert_eq!(open.peer(), Peer::Watching);
    assert!(!rang(&open_sub), "nothing rang the open client's list yet");

    watch.fail_for_test();
    assert!(
        open_sub.wait(MUST_HAPPEN),
        "the stop rang the open client's list"
    );
    assert_eq!(open.peer(), Peer::Unwatched, "handed back to the probe");
    assert_eq!(gone.peer(), Peer::Gone, "a settled verdict stays");
    let (late_server, _late_client) = CtlStream::pair().expect("pair");
    assert!(
        watch.watch(&late_server).is_none(),
        "a stopped watch refuses"
    );
    drop((open, gone));
}
