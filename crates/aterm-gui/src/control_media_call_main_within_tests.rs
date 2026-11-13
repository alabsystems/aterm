// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;

use super::recv_within;

#[test]
fn a_scheduled_main_thread_supplies_its_required_reply() {
    let (tx, rx) = mpsc::channel::<u8>();
    tx.send(7).unwrap();
    let reply = super::recv_main_at(&rx, std::time::Instant::now() - Duration::from_secs(31));
    assert_eq!(reply, Ok(7));
    let (tx, rx) = mpsc::channel::<u8>();
    tx.send(9).unwrap();
    let expired = super::recv_main_at(&rx, std::time::Instant::now() - Duration::from_secs(61))
        .expect_err("the whole hang detector expired");
    assert_eq!(rx.try_recv(), Ok(9), "expiry must not consume a late reply");
    let (tx, rx) = mpsc::channel::<u8>();
    drop(tx);
    let dropped = super::recv_main_at(&rx, std::time::Instant::now()).unwrap_err();
    assert_ne!(expired, dropped);
}

#[test]
fn recv_within_tells_a_timeout_from_a_dropped_reply() {
    let (_tx, rx) = mpsc::channel::<u8>();
    let timed_out = recv_within(&rx, Duration::from_millis(20));
    let (tx, rx) = mpsc::channel::<u8>();
    drop(tx);
    let dropped = recv_within(&rx, Duration::from_secs(60));
    assert!(timed_out.is_err() && dropped.is_err());
    assert_ne!(timed_out, dropped);
}
#[test]
fn a_main_thread_verb_is_refused_while_a_dialog_stands() {
    use crate::watchdog::Breadcrumb;
    assert!(super::dialog_refusal_for(Breadcrumb::Modal).is_some());
    for other in [
        Breadcrumb::Startup,
        Breadcrumb::AboutToWait,
        Breadcrumb::WindowEvent,
        Breadcrumb::UserEvent,
        Breadcrumb::NewEvents,
        Breadcrumb::ResizeSettle,
        Breadcrumb::UpdateHandoff,
        Breadcrumb::AboutToWaitWork,
    ] {
        assert_eq!(super::dialog_refusal_for(other), None, "{other:?}");
    }
}

#[test]
fn the_hop_count_keeps_when_its_run_began_until_the_last_hop_ends() {
    let hops = super::Hops::new();
    assert_eq!(hops.since_ns(), None, "nothing outstanding");
    let first = hops.post(10);
    assert_eq!(hops.since_ns(), Some(10));
    let second = hops.post(20);
    assert_eq!(hops.since_ns(), Some(10), "the run began with the first");
    drop(first);
    assert_eq!(
        hops.since_ns(),
        Some(10),
        "a hop still waits: the run goes on"
    );
    drop(second);
    assert_eq!(hops.since_ns(), None, "every guard counted itself out");
    let next = hops.post(30);
    assert_eq!(hops.since_ns(), Some(30), "a new run, a new start");
    drop(next);
    assert_eq!(hops.since_ns(), None);
    let at_zero = hops.post(0);
    assert!(hops.since_ns().is_some());
    drop(at_zero);
}

#[test]
fn a_hop_stops_counting_when_the_main_thread_takes_it() {
    let hops: &'static super::Hops = Box::leak(Box::new(super::Hops::new()));
    let parked = hops.post(10);
    let request = super::take_hop(crate::Wake::Hop {
        mark: parked.mark(),
        wake: Box::new(crate::Wake::WarmupResult),
    });
    assert!(
        matches!(request, crate::Wake::WarmupResult),
        "the main thread handles the request itself: {request:?}"
    );
    assert_eq!(
        hops.since_ns(),
        None,
        "taken, its reply queued: its worker still waits, and it is not counted"
    );
    let waiting = hops.post(20);
    assert_eq!(hops.since_ns(), Some(20));
    drop(parked);
    assert_eq!(
        hops.since_ns(),
        Some(20),
        "a reply after the take counts out nothing more"
    );
    let late = waiting.mark();
    drop(waiting);
    assert_eq!(hops.since_ns(), None);
    let next = hops.post(30);
    late.taken();
    assert_eq!(
        hops.since_ns(),
        Some(30),
        "a take after the give-up counts out nothing"
    );
    drop(next);
    assert_eq!(hops.since_ns(), None);

    let first = hops.post(40);
    let second = hops.post(50);
    first.mark().taken();
    assert_eq!(hops.since_ns(), Some(40));
    drop(first);
    assert_eq!(hops.since_ns(), Some(40), "the second still waits");
    drop(second);
    assert_eq!(hops.since_ns(), None);
}

#[test]
fn a_stalled_main_thread_answers_a_named_retryable_err() {
    use crate::watchdog::{Breadcrumb, MainStall};
    let stall = MainStall {
        stalled: Duration::from_secs(12),
        root: Breadcrumb::NewEvents,
        returned: true,
    };
    assert_eq!(
        format!("ERR {}\n", super::MainHopError::Stalled(stall)),
        "ERR main thread stalled 12s; retry\n"
    );
}

fn image_request(
    reply: mpsc::Sender<crate::control::ImageReply>,
    frame_metadata: &Arc<OnceLock<crate::control::ImageFrameMetadata>>,
) -> crate::Wake {
    crate::Wake::Image(Box::new(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::in_memory(),
        clean: false,
        session: None,
        want_bytes: true,
        want_metadata: false,
        frame_metadata: Arc::clone(frame_metadata),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    }))
}

#[test]
fn capture_hops_are_counted_until_taken() {
    let hops: &'static super::Hops = Box::leak(Box::new(super::Hops::new()));
    let metadata = Arc::new(OnceLock::new());
    let mut posted = vec![image_request(mpsc::channel().0, &metadata)];
    assert_eq!(hops.since_ns(), None, "a raw post is not counted");
    posted.clear();

    let (rx, guard) = super::post_counted(
        hops,
        None,
        |wake| {
            posted.push(wake);
            true
        },
        |reply| image_request(reply, &metadata),
    )
    .expect("an unrefused capture is posted");
    assert!(
        hops.since_ns().is_some(),
        "the posted capture waits on the main thread"
    );
    let [wake] = <[crate::Wake; 1]>::try_from(posted).expect("one wake posted");
    let crate::Wake::Image(request) = super::take_hop(wake) else {
        panic!("the main thread handles the image request itself");
    };
    assert_eq!(hops.since_ns(), None, "taken: the main thread has it");
    request
        .reply
        .send(Err("answered".to_string()))
        .expect("the worker still waits");
    let reply = rx.recv().expect("the main thread's answer");
    assert_eq!(reply.err().as_deref(), Some("answered"));
    drop(guard);
    assert_eq!(hops.since_ns(), None, "the take counted it out once");
}

#[test]
fn post_main_refuses_before_posting() {
    use crate::watchdog::{Breadcrumb, MainStall};
    let hops: &'static super::Hops = Box::leak(Box::new(super::Hops::new()));
    let stalled = super::MainHopError::Stalled(MainStall {
        stalled: Duration::from_secs(6),
        root: Breadcrumb::NewEvents,
        returned: false,
    });
    let (mut built, mut sent) = (0, 0);
    let refused = super::post_counted::<u8>(
        hops,
        Some(stalled),
        |_| {
            sent += 1;
            true
        },
        |_| {
            built += 1;
            crate::Wake::WarmupResult
        },
    );
    assert_eq!(refused.err(), Some(stalled));
    assert_eq!((built, sent), (0, 0), "never built, never posted");
    assert_eq!(hops.since_ns(), None, "never counted");

    let metadata = Arc::new(OnceLock::new());
    let gone = super::post_counted(
        hops,
        None,
        |_| false,
        |reply| image_request(reply, &metadata),
    );
    assert_eq!(
        gone.err(),
        Some(super::MainHopError::Reason("event loop gone"))
    );
    assert_eq!(
        Arc::strong_count(&metadata),
        1,
        "the unsent request dropped with its guards"
    );
    assert_eq!(hops.since_ns(), None, "its hop counted itself out");

    let mut posted = Vec::new();
    let (_rx, guard) = super::post_counted::<u8>(
        hops,
        None,
        |wake| {
            posted.push(wake);
            true
        },
        |_| {
            built += 1;
            crate::Wake::WarmupResult
        },
    )
    .expect("unrefused, the hop is posted");
    assert_eq!(built, 1, "built once");
    assert!(
        matches!(posted.as_slice(), [crate::Wake::Hop { .. }]),
        "one counted hop posted: {posted:?}"
    );
    assert!(hops.since_ns().is_some(), "and counted");
    drop(guard);
    assert_eq!(hops.since_ns(), None);
}
