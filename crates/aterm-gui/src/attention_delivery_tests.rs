// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A parsed annotation can be waiting for its metadata lock when its caller's deadline ends.

use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::sync::mpsc::{Receiver, Sender};

use aterm_control::hangup::ServingFd;

use crate::control::{HUNG_UP_REPLY, meta_wakes_for_test};
use crate::session_store::{SessionHandle, Store};

thread_local! {
    static BEFORE_EFFECT: RefCell<Option<Preemption>> = const { RefCell::new(None) };
}

struct Preemption {
    arrived: Sender<bool>,
    resume: Receiver<()>,
    gone: bool,
}

/// Models preemption immediately before the real metadata mutex.
pub(super) fn before_effect() {
    if let Some(gate) = BEFORE_EFFECT.with(|slot| slot.borrow_mut().take()) {
        gate.arrived.send(false).expect("preemption observed");
        gate.resume
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("preempted lane resumed");
        if gate.gone {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the serving lane observes actual caller EOF",
                || crate::control::caller_hung_up().then_some(()),
            );
        }
        gate.arrived
            .send(crate::control::caller_hung_up())
            .expect("actual peer state observed on serving lane");
    }
}

#[derive(Clone, Copy, Debug)]
enum Peer {
    Gone,
    Live,
    HalfClosed,
    Internal,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    old: &'static str,
    initial: &'static str,
    latest: &'static str,
}

const CASES: &[Case] = &[
    Case {
        old: "set attention owner=codex-alab kind=ask old refusal",
        initial: "unset attention owner=codex-alab",
        latest: "unset attention owner=codex-alab",
    },
    Case {
        old: "unset attention owner=codex-alab",
        initial: "set attention owner=codex-alab kind=ask old refusal",
        latest: "set attention owner=codex-alab kind=ask fresh refusal",
    },
    Case {
        old: "set attention owner=codex-alab kind=wait fresh refusal",
        initial: "set attention owner=codex-alab kind=ask fresh refusal",
        latest: "set attention owner=codex-alab kind=ask fresh refusal",
    },
    Case {
        old: "set attention old refusal",
        initial: "unset attention",
        latest: "unset attention",
    },
    Case {
        old: "unset attention",
        initial: "set attention old refusal",
        latest: "set attention fresh refusal",
    },
    Case {
        old: "set description old work",
        initial: "unset description",
        latest: "unset description",
    },
    Case {
        old: "unset description",
        initial: "set description old work",
        latest: "set description fresh work",
    },
];

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    description: Option<String>,
    text: Option<String>,
    entries: Vec<(String, String, Option<aterm_types::needs::AttentionKind>)>,
    driver_writes: u8,
    task: Option<crate::task_record::TaskRecord>,
    events: Vec<(String, String)>,
}

fn snapshot(h: &SessionHandle) -> Snapshot {
    let meta = h.ctx.meta.lock().unwrap();
    let timeline = h.ctx.timeline.lock().unwrap();
    Snapshot {
        description: meta.description.clone(),
        text: meta.attention.clone(),
        entries: meta
            .attention_owners
            .in_write_order()
            .into_iter()
            .map(|(owner, text, kind)| (owner.into(), text.into(), kind))
            .collect(),
        driver_writes: meta.driver_writes,
        task: timeline.task().cloned(),
        events: timeline
            .since(None)
            .map(|event| (event.kind.to_string(), event.payload.to_string()))
            .collect(),
    }
}

fn write(h: &SessionHandle, store: &Store, command: &str, conn: u64) {
    let (reply, _) = meta_wakes_for_test(store, &h.term, 0, &h.ctx, command, Some(conn));
    assert_eq!(reply, "OK\n", "{command}");
}

fn exercise(case: Case, peer: Peer) {
    let h = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    h.ctx.timeline.lock().unwrap().mark_task(
        &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
        false,
        std::time::Instant::now(),
        1,
    );
    assert_eq!(
        super::claim_supervisor(&h.ctx, "supervisor", Some(7), None, 100),
        Ok(true)
    );
    write(&h, &store, case.initial, 7);
    let (client, server) = aterm_uds::CtlStream::pair().expect("serving socket pair");
    let (arrived_tx, arrived) = std::sync::mpsc::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel();
    let old_handle = h.clone();
    let old_store = store.clone();
    let writer = std::thread::spawn(move || {
        let _serving =
            (!matches!(peer, Peer::Internal)).then(|| ServingFd::enter(server.as_raw_fd()));
        BEFORE_EFFECT.with(|slot| {
            *slot.borrow_mut() = Some(Preemption {
                arrived: arrived_tx,
                resume: resume_rx,
                gone: matches!(peer, Peer::Gone),
            });
        });
        meta_wakes_for_test(
            &old_store,
            &old_handle.term,
            0,
            &old_handle.ctx,
            case.old,
            Some(7),
        )
    });
    arrived
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("old parsed request reached its effect boundary");
    assert_eq!(
        super::claim_supervisor(&h.ctx, "supervisor", Some(8), None, 100),
        Ok(false),
        "the same holder renews its claim onto the replacement connection"
    );
    write(&h, &store, case.latest, 8);
    write(
        &h,
        &store,
        "set attention owner=person kind=ask the person's current need",
        8,
    );
    let before = snapshot(&h);
    let held = h.ctx.meta.lock().unwrap();
    match peer {
        Peer::Gone => {
            assert!(aterm_uds::hangup::hang_up(&client));
            drop(client);
        }
        Peer::HalfClosed => client
            .shutdown(std::net::Shutdown::Write)
            .expect("supported request-side half-close"),
        Peer::Live | Peer::Internal => {}
    }
    resume.send(()).expect("resume old lane");
    let observed_gone = arrived
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("serving lane observes the caller before locking metadata");
    drop(held);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the metadata writer completes",
        || writer.is_finished().then_some(()),
    );
    let (reply, wakes) = writer.join().expect("metadata writer");
    let after = snapshot(&h);
    assert_eq!(observed_gone, matches!(peer, Peer::Gone), "{peer:?}");
    if matches!(peer, Peer::Gone) {
        assert_eq!(reply, HUNG_UP_REPLY, "{case:?}");
        assert!(wakes.is_empty(), "refusal wakes no GUI: {case:?}");
        assert_eq!(
            after, before,
            "no metadata, task or timeline change: {case:?}"
        );
    } else {
        assert_eq!(reply, "OK\n", "{peer:?} {case:?}");
        assert_ne!(after, before, "live effect applied: {peer:?} {case:?}");
        assert!(
            after.entries.iter().any(|(owner, text, _)| {
                owner == "person" && text == "the person's current need"
            })
        );
    }
}

#[test]
fn abandoned_keyed_attention_set_cannot_recreate_a_resolved_alert() {
    exercise(CASES[0], Peer::Gone);
}

#[test]
fn abandoned_keyed_attention_clear_cannot_remove_a_fresh_alert() {
    exercise(CASES[1], Peer::Gone);
}

#[test]
fn abandoned_attention_kind_cannot_replace_a_fresh_kind() {
    exercise(CASES[2], Peer::Gone);
}

#[test]
fn abandoned_bare_attention_set_cannot_recreate_a_resolved_alert() {
    exercise(CASES[3], Peer::Gone);
}

#[test]
fn abandoned_bare_attention_clear_cannot_remove_a_fresh_alert() {
    exercise(CASES[4], Peer::Gone);
}

#[test]
fn abandoned_description_set_cannot_recreate_a_retired_description() {
    exercise(CASES[5], Peer::Gone);
}

#[test]
fn abandoned_description_clear_cannot_remove_a_fresh_description() {
    exercise(CASES[6], Peer::Gone);
}

#[test]
fn live_half_closed_and_internal_attention_writers_remain_supported() {
    for peer in [Peer::Live, Peer::HalfClosed, Peer::Internal] {
        for &case in CASES {
            exercise(case, peer);
        }
    }
}
