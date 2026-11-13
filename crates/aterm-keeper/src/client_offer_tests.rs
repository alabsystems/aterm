// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_full_queue_hands_an_offered_op_back_and_a_gone_worker_does_too() {
    let (link, far) = KeeperLink::stalled();
    for rdev in 0..LINK_QUEUE as u64 {
        assert!(link.offer(LinkOp::Release { rdev }).is_ok(), "{rdev} fits");
    }
    let marked = u64::MAX;
    match link.offer(LinkOp::CompanionRelease {
        rdev: marked,
        receipt: None,
    }) {
        Err(Refused::Full(LinkOp::CompanionRelease { rdev, .. })) => assert_eq!(rdev, marked),
        other => panic!("a full queue hands the op back: {other:?}"),
    }
    assert!(
        !link.enqueue(LinkOp::CompanionRelease {
            rdev: marked - 1,
            receipt: None
        }),
        "control: enqueue drops it"
    );
    let drained = far.drain();
    assert_eq!(drained.len(), LINK_QUEUE, "only what fit");
    assert!(
        !drained
            .iter()
            .any(|op| matches!(op, LinkOp::CompanionRelease { .. })),
        "neither the refused offer nor the dropped enqueue reached the keeper"
    );
    assert!(
        link.offer(LinkOp::CompanionRelease {
            rdev: marked,
            receipt: None
        })
        .is_ok(),
        "drained, the queue takes it"
    );
    assert!(matches!(
        far.drain().as_slice(),
        [LinkOp::CompanionRelease { rdev, .. }] if *rdev == marked
    ));
    drop(far);
    assert!(matches!(
        link.offer(LinkOp::Release { rdev: 1 }),
        Err(Refused::Gone(LinkOp::Release { rdev: 1 }))
    ));
    link.shutdown();

    let observer = std::sync::Arc::new(ReadObserver::default());
    let receipt = std::sync::Arc::new(CompanionReceipt::reporting_confirmation(
        std::sync::Arc::clone(&observer),
    ));
    let mut read = Some(CompanionRead {
        receipt: std::sync::Arc::clone(&receipt),
        carried: Vec::new(),
        fence: None,
    });
    confirm_companion(&mut read, u64::MAX);
    assert!(!receipt.confirmed() && read.is_some());
    let refused_sent = std::time::Instant::now();
    read.as_mut().expect("pending").fence = Some(CompanionFence {
        after: 6,
        until: 7,
        sent: refused_sent,
        refused: false,
    });
    note_companion_refusal(&mut read, 5);
    note_companion_refusal(&mut read, 7);
    confirm_companion(&mut read, 6);
    assert!(!receipt.confirmed() && read.is_some());
    note_companion_refusal(&mut read, 6);
    confirm_companion(&mut read, 7);
    assert!(!observer.confirmed() && observer.confirmation().is_none());
    assert!(
        !receipt.confirmed() && read.is_some(),
        "a refusal is not acceptance"
    );
    unconfirm_companion(&mut read);
    confirm_companion(&mut read, 7);
    assert!(!observer.confirmed());
    assert!(
        !receipt.confirmed() && read.is_some(),
        "the disconnected op is unsent"
    );
    let accepted_sent = refused_sent + Duration::from_nanos(1);
    read.as_mut().expect("pending").fence = Some(CompanionFence {
        after: 0,
        until: 1,
        sent: accepted_sent,
        refused: false,
    });
    confirm_companion(&mut read, 0);
    assert!(
        !receipt.confirmed() && read.is_some(),
        "reconnected but unanswered"
    );
    confirm_companion(&mut read, 1);
    assert!(receipt.confirmed() && read.is_none());
    let confirmed = observer
        .confirmation()
        .expect("the observer heard the accepted read");
    assert_eq!(
        confirmed.sync_sent, accepted_sent,
        "the observer times the SYNC whose answer confirmed the read, not a refused one's"
    );
    assert!(
        confirmed.answer_read >= refused_sent,
        "the answer is read after its SYNC"
    );
    assert!(observer.confirmed());

    let now = std::time::Instant::now();
    let make = || {
        let (answer, read) = mpsc::sync_channel(1);
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED));
        let waited = Waited {
            answer,
            state: std::sync::Arc::clone(&state),
            until: Some(now + Duration::from_secs(60)),
        };
        ((LinkOp::Pending { pid: 7 }, Some(waited)), state, read)
    };
    let (queued, state, answer) = make();
    Waited::cancel(&state);
    assert!(claim_queued(queued, now).is_none());
    assert!(!answer.try_recv().expect("cancelled wait answered"));
    let (queued, _, answer) = make();
    assert!(claim_queued(queued, now + Duration::from_secs(60)).is_none());
    assert!(!answer.try_recv().expect("expired wait answered"));
    let (queued, state, answer) = make();
    let (op, waited) = claim_queued(queued, now).expect("live wait claimed");
    assert!(matches!(op, LinkOp::Pending { pid: 7 }));
    Waited::cancel(&state);
    assert_eq!(
        state.load(std::sync::atomic::Ordering::Acquire),
        Waited::CLAIMED,
        "a timeout cannot retract an already-running op"
    );
    waited
        .expect("waiter")
        .try_send(true)
        .expect("written answer");
    assert!(answer.try_recv().expect("live wait answered"));
}

#[test]
fn a_waited_op_finds_room_in_a_queue_its_ordinary_ops_filled() {
    let (link, far) = KeeperLink::stalled();
    for rdev in 0..LINK_QUEUE as u64 {
        assert!(link.offer(LinkOp::Release { rdev }).is_ok(), "{rdev} fits");
    }
    assert!(
        matches!(
            link.offer(LinkOp::CompanionRelease {
                rdev: 1,
                receipt: None
            }),
            Err(Refused::Full(_))
        ),
        "control: an ordinary op finds the queue full"
    );
    let now = std::time::Instant::now();
    let waited = u32::try_from(WAITED_ROOM).expect("a few places");
    for pid in 0..=waited {
        let (answer, _read) = mpsc::sync_channel(1);
        let request = Waited {
            answer,
            state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED)),
            until: Some(now + Duration::from_secs(60)),
        };
        let queued = link
            .tx
            .try_send((LinkOp::Pending { pid }, Some(request)))
            .is_ok();
        assert_eq!(queued, pid < waited, "only the reserved capacity fits");
    }
    assert!(
        !link.send_and_wait(LinkOp::Pending { pid: 99 }, Duration::ZERO),
        "an already-expired nonterminal wait is never admitted"
    );
    let drained = far.drain_at(now);
    assert_eq!(drained.len(), LINK_QUEUE + WAITED_ROOM);
    assert!(
        drained[..LINK_QUEUE]
            .iter()
            .all(|op| matches!(op, LinkOp::Release { .. }))
    );
    let pids: Vec<u32> = drained[LINK_QUEUE..]
        .iter()
        .map(|op| match op {
            LinkOp::Pending { pid } => *pid,
            other => panic!("only waited ops follow: {other:?}"),
        })
        .collect();
    assert_eq!(pids, (0..waited).collect::<Vec<_>>());
    assert!(
        link.offer(LinkOp::Release { rdev: 1 }).is_ok(),
        "read, the queue takes ordinary ops again"
    );

    let (link, far) = KeeperLink::stalled();
    while link.enqueue(LinkOp::Release { rdev: 1 }) {}
    drop(far);
    assert!(
        matches!(
            link.offer(LinkOp::Release { rdev: 2 }),
            Err(Refused::Gone(_))
        ),
        "an ended worker is gone, not full"
    );
}

/// STATUS CHECKS THE KEEPER'S IDENTITY FLOOR AND EMPTY WELCOME: a `Designated` client
/// calling `status()` refuses a `SameUid` or legacy untagged `WELCOME` with `NotThisBuild`
/// and rejects a non-zero `offers` count as out of protocol.
/// NEGATIVE CONTROL: a `Designated` `WELCOME` with `offers: 0` succeeds and records `caps`,
/// and a `SameUid` client accepts a `SameUid` `WELCOME`.
#[cfg(unix)]
#[test]
fn status_enforces_the_clients_identity_floor_and_empty_welcome() {
    let run_case = |policy: IdentityPolicy,
                    offers: u16,
                    served: Option<IdentityPolicy>,
                    caps: Option<u8>,
                    reply_status: bool| {
        let (stream, peer_stream) = CtlStream::pair().expect("pair");
        let peer = std::thread::spawn(move || {
            peer_stream
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .expect("timeout");
            let hello = read_frame(&peer_stream)
                .expect("read")
                .expect("hello")
                .frame;
            assert!(
                matches!(
                    hello,
                    Frame::Hello {
                        class: PeerClass::Status,
                        ..
                    }
                ),
                "{hello:?}"
            );
            let welcome = Frame::Welcome {
                proto: PROTO,
                offers,
                version: "test".into(),
                identity: served,
                caps,
            };
            write_frame(&peer_stream, &welcome, None).expect("welcome");
            if reply_status {
                let ask = read_frame(&peer_stream)
                    .expect("read")
                    .expect("status")
                    .frame;
                assert!(matches!(ask, Frame::Status { .. }), "{ask:?}");
                write_frame(
                    &peer_stream,
                    &Frame::Status {
                        text: "keeper=running".into(),
                    },
                    None,
                )
                .expect("status reply");
                Some(peer_stream)
            } else {
                None
            }
        });
        // This peer always answers, so its frames wait out only the hang detector.
        let wait = aterm_spec::wait::MUST_HAPPEN;
        stream.set_read_timeout(Some(wait)).expect("read timeout");
        stream.set_write_timeout(Some(wait)).expect("write timeout");
        let client = KeeperClient {
            stream,
            frame_wait: wait,
            policy,
            keeper_caps: Cell::new(0),
            syncs: Cell::new(0),
            synced: Cell::new(0),
            retiring: RefCell::new(Vec::new()),
        };
        let got = client.status("t");
        let _held = peer.join().expect("peer");
        (client, got)
    };

    let (_, uid_floor) = run_case(
        IdentityPolicy::Designated,
        0,
        Some(IdentityPolicy::SameUid),
        Some(0),
        false,
    );
    let err = uid_floor.expect_err("Designated status must refuse --identity uid keeper");
    assert!(refused_identity(&err), "{err}");
    assert!(err.to_string().contains("--identity uid"), "{err}");

    let (_, untagged) = run_case(IdentityPolicy::Designated, 0, None, None, false);
    let err = untagged.expect_err("Designated status must refuse untagged legacy keeper");
    assert!(refused_identity(&err), "{err}");

    let (_, non_empty) = run_case(
        IdentityPolicy::Designated,
        1,
        Some(IdentityPolicy::Designated),
        Some(0),
        false,
    );
    let err = non_empty.expect_err("status WELCOME with offers != 0 is out of protocol");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");

    let (designated_client, ok) = run_case(
        IdentityPolicy::Designated,
        0,
        Some(IdentityPolicy::Designated),
        Some(CAP_JOURNAL | CAP_SYNC),
        true,
    );
    assert_eq!(ok.expect("Designated keeper accepted"), "keeper=running");
    assert!(designated_client.keeper_journals() && designated_client.keeper_syncs());

    let (_, same_uid_ok) = run_case(
        IdentityPolicy::SameUid,
        0,
        Some(IdentityPolicy::SameUid),
        Some(0),
        true,
    );
    assert_eq!(
        same_uid_ok.expect("SameUid client accepted"),
        "keeper=running"
    );
}
