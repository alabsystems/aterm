// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::vitals_host::Reader;
use std::sync::mpsc::{self, TryRecvError};

#[path = "../../aterm-vitals/tests/support/world.rs"]
mod world;

const HANG: std::time::Duration = std::time::Duration::from_secs(60);

fn shared(lane: &Lane) -> Arc<Shared> {
    match lane {
        Lane::Worker(worker) => Arc::clone(&worker.shared),
        Lane::Direct(_) => panic!("threaded lane required"),
    }
}

fn wait_state(shared: &Shared, why: &str, ready: impl Fn(&State) -> bool) {
    let until = Instant::now() + HANG;
    let mut state = shared.lock();
    while !ready(&state) {
        let left = until.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{why}");
        state = shared.changed.wait_timeout(state, left).unwrap().0;
    }
}

fn wait_ready(shared: &Shared, serial: u64) {
    wait_state(shared, "assessment result did not arrive", |state| {
        state.ready.as_ref().is_some_and(|out| out.serial == serial)
    });
}

fn wait_dead(shared: &Shared) {
    wait_state(shared, "assessment thread did not end", |_| {
        !shared.alive.load(Ordering::Acquire)
    });
}

fn input(epoch: Instant, observed: Instant, generation: u64) -> Input {
    let req = Request {
        self_pid: std::process::id(),
        jobs_now: true,
        ..Request::default()
    };
    let reads = req.tab_reads(epoch, || observed);
    Input::new(
        reads,
        Some(vec![ProcessReading {
            pid: 4242,
            start: Some(generation),
            ..ProcessReading::default()
        }]),
        true,
        &req,
        generation,
        false,
    )
}

/// Hold the real assessment boundary, not the machine source. Two whole machine readings
/// must return while the assessment is still held, and no job result is invented for them.
#[test]
fn machine_readings_continue_while_assessment_is_held() {
    let (entered, at_gate) = mpsc::sync_channel(1);
    let (release, go) = mpsc::sync_channel(1);
    let mut first = true;
    let lane = Lane::threaded(move |core, input| {
        if std::mem::take(&mut first) {
            entered.send(()).unwrap();
            go.recv_timeout(HANG).unwrap();
        }
        core.read(input)
    });
    let shared = shared(&lane);
    let (ask, requests) = mpsc::sync_channel::<()>(1);
    let (sampled, readings) = mpsc::channel();
    let (ended, done) = mpsc::sync_channel(1);
    let sampler = std::thread::spawn(move || {
        let mut reader = Reader::new(Arc::default());
        reader.assessment = lane;
        let req = Request {
            self_pid: std::process::id(),
            ..Request::default()
        };
        while requests.recv().is_ok() {
            let snap = reader.read(&req);
            sampled
                .send((snap.seq, snap.at_ns, snap.jobs.is_some()))
                .unwrap();
        }
        reader.assessment.at_stop(&[], 0);
        drop(reader);
        ended.send(()).unwrap();
    });
    ask.send(()).unwrap();
    let before = readings.recv_timeout(HANG).expect("first machine sample");
    at_gate
        .recv_timeout(HANG)
        .expect("assessment is actually held");
    ask.send(()).unwrap();
    let during = readings
        .recv_timeout(HANG)
        .expect("machine sample while assessment stays held");
    assert!(during.0 > before.0 && during.1 > before.1);
    assert!(
        !before.2 && !during.2,
        "a pending assessment is not fresh job data"
    );
    release.send(()).unwrap();
    drop(ask);
    done.recv_timeout(HANG).expect("machine sampler stopped");
    sampler.join().unwrap();
    wait_dead(&shared);
}

/// A completed assessment cannot be overtaken before publication. Pending generations may
/// coalesce, but their replacement keeps its identity and its original observation time.
#[test]
fn assessments_publish_in_order_and_only_unstarted_observations_coalesce() {
    for serial_seed in [0, u64::MAX - 1] {
        ordered_assessment_publication(serial_seed);
    }
}

fn ordered_assessment_publication(serial_seed: u64) {
    let (entered, at_gate) = mpsc::sync_channel(1);
    let (release, go) = mpsc::sync_channel(1);
    let (saw, seen) = mpsc::channel();
    let mut first = true;
    let mut lane = Lane::threaded(move |core, input| {
        if std::mem::take(&mut first) {
            entered.send(()).unwrap();
            go.recv_timeout(HANG).unwrap();
        }
        let result = core.read(input);
        saw.send((
            input.serial,
            input.sweep.as_ref().unwrap()[0].start,
            input.coalesced,
        ))
        .unwrap();
        result
    });
    match &mut lane {
        Lane::Worker(worker) => worker.next = serial_seed,
        Lane::Direct(_) => unreachable!(),
    }
    let serial = |offset| serial_seed.saturating_add(offset);
    let shared = shared(&lane);
    let now = Instant::now();
    let epoch = now - std::time::Duration::from_secs(10);
    let first_at = now - std::time::Duration::from_secs(3);
    let newest = now - std::time::Duration::from_secs(1);
    lane.read(input(epoch, first_at, 11), true);
    at_gate.recv_timeout(HANG).unwrap();
    lane.read(
        input(epoch, now - std::time::Duration::from_secs(2), 22),
        true,
    );
    lane.read(input(epoch, newest, 33), true);
    release.send(()).unwrap();
    wait_ready(&shared, serial(1));
    assert_eq!(seen.recv_timeout(HANG).unwrap(), (serial(1), Some(11), 0));
    assert!(
        matches!(seen.try_recv(), Err(TryRecvError::Empty)),
        "the next generation waits for publication"
    );
    let first = lane.carried();
    assert_eq!(first.jobs_at, Some(first_at));
    assert!(
        !lane.continuous(),
        "a delayed initial result is not an indefinitely current view"
    );
    lane.publish_names(Vec::new());
    wait_ready(&shared, serial(3));
    assert_eq!(seen.recv_timeout(HANG).unwrap(), (serial(3), Some(33), 1));
    {
        let state = shared.lock();
        assert!(
            !Arc::ptr_eq(
                state.acknowledged.as_ref().unwrap(),
                &state.ready.as_ref().unwrap().publication,
            ),
            "the previous acknowledgement cannot publish a new result, even at counter saturation"
        );
    }
    let last = lane.carried();
    assert_eq!((last.jobs_at, last.left_at), (Some(newest), Some(newest)));
    assert!(
        lane.continuous(),
        "a timely reassessment can maintain continuity"
    );
    lane.read(input(epoch, Instant::now(), 44), true);
    assert!(
        !lane.continuous(),
        "a changed process birth revokes the old witness immediately"
    );
    lane.read(input(epoch, Instant::now(), 33), true);
    assert!(
        !lane.continuous(),
        "returning to old evidence does not restore an invalidated witness"
    );
    lane.publish_names(Vec::new());
    wait_ready(&shared, serial(5));
    assert_eq!(seen.recv_timeout(HANG).unwrap(), (serial(5), Some(33), 1));
    lane.carried();
    assert!(
        lane.continuous(),
        "only another actual assessment restores the witness"
    );
    // Stop while this result still awaits its acknowledgement.
    lane.at_stop(&[], 0);
    wait_dead(&shared);
}

/// A full notification channel cannot lose pending work, and the assessment holds only a
/// weak sender. The real sampler drain must service that work and end after its owner drops.
#[test]
fn a_full_wake_channel_keeps_work_visible_and_does_not_retain_the_sampler() {
    use crate::sys_naming::{Error, Operation, Request as NameRequest};
    use aterm_vitals::jobs::NamingError;
    use aterm_vitals::sys::query::NameTarget;
    let mut lane = Lane::threaded(Core::read);
    let shared = shared(&lane);
    let (tx, rx) = mpsc::sync_channel(2);
    tx.send(Msg::Told(Vec::new())).unwrap();
    tx.send(Msg::Told(Vec::new())).unwrap();
    let owner = Arc::new(tx);
    lane.install_wake(Arc::downgrade(&owner));
    let (reply, answer) = mpsc::sync_channel(1);
    assert!(
        lane.schedule_names(vec![NameRequest::new(
            Operation::Capture {
                session: 7,
                target: NameTarget::Job("absent".into())
            },
            Arc::new(Mutex::new(true)),
            reply,
        )])
        .is_empty()
    );
    assert_eq!(lane.work(), Work::Input);
    let latest = Arc::new(Mutex::new(super::super::Handover::default()));
    let published = Arc::new(AtomicBool::new(false));
    let (ended, done) = mpsc::sync_channel(1);
    let held = Arc::clone(&latest);
    let marked = Arc::clone(&published);
    let sampler = std::thread::spawn(move || {
        let mut reader = Reader::new(Arc::default());
        reader.assessment = lane;
        super::super::run(
            &rx,
            &held,
            &|| {
                assert!(held.lock().unwrap().snap.is_some());
                marked.store(true, Ordering::Release);
                true
            },
            reader,
            None,
            None,
            &AtomicBool::new(false),
            super::super::PriorRun {
                handed_off: false,
                crashed_at: None,
                killed_since: None,
            },
        );
        ended.send(()).unwrap();
    });
    assert!(matches!(
        answer.recv_timeout(HANG).unwrap(),
        Err(Error::Engine(NamingError::Gone))
    ));
    assert!(
        published.load(Ordering::Acquire),
        "the reply follows publication"
    );
    assert!(latest.lock().unwrap().snap.as_ref().unwrap().seq >= 2);
    drop(owner);
    done.recv_timeout(HANG)
        .expect("no assessment-owned sender keeps the sampler alive");
    sampler.join().unwrap();
    wait_dead(&shared);
}

/// Exercise the real naming engine through the actor: cancellation, a later probe for apply,
/// publication acknowledgements, and a reused pid rejecting its predecessor's ticket.
#[test]
fn naming_keeps_fresh_probe_publication_and_generation_custody() {
    use crate::sys_naming::{Answer, Error, Operation, Request as NameRequest};
    use aterm_vitals::jobs::{NamingError, TierB};
    use aterm_vitals::sys::query::NameTarget;
    let mut world = world::World::new();
    world.at = world::T0 - 480_000;
    let mut leaf = world::Proc::new(7700, 0, "cargo");
    leaf.out = Some((1, 99_000));
    world.files.insert(
        (1, 99_000),
        world::OutFile {
            size: 0,
            per_s: 0,
            lf: Some(true),
        },
    );
    world.run_fg(1, 77, "cargo build", leaf);
    let mut first = true;
    let (saw, seen) = mpsc::channel();
    let mut lane = Lane::threaded(move |core, input| {
        if input.req.session_ids.iter().any(|(_, s)| s == "reuse") {
            let leaf = world.procs.get_mut(&7700).unwrap();
            leaf.start += 1;
            leaf.exec += 1;
        }
        let view = core.jobs.scripted(input.reads.observed_at, |engine| {
            std::mem::swap(engine, &mut world.jobs);
            if std::mem::take(&mut first) {
                for _ in 0..239 {
                    let _ = world.tick(2_000);
                }
            }
            let dt = input.reads.at_ms.saturating_sub(world.at);
            let view = world.tick(dt);
            std::mem::swap(engine, &mut world.jobs);
            view
        });
        saw.send((input.serial, input.req.jobs_now)).unwrap();
        Reading {
            jobs: Some(view),
            jobs_at: core.jobs.observed_at(),
            ..Reading::default()
        }
    });
    let shared = shared(&lane);
    let epoch = Instant::now() - std::time::Duration::from_millis(world::T0);
    let make = || {
        let mut i = input(epoch, Instant::now(), 0);
        i.req.jobs_now = false;
        i
    };
    let capture = |live, reply| {
        NameRequest::new(
            Operation::Capture {
                session: 1,
                target: NameTarget::Job("b77".into()),
            },
            live,
            reply,
        )
    };
    let canceled = Arc::new(Mutex::new(true));
    let (reply, answer) = mpsc::sync_channel(1);
    lane.schedule_names(vec![capture(Arc::clone(&canceled), reply)]);
    lane.read(make(), true);
    wait_ready(&shared, 1);
    lane.carried();
    assert!(matches!(answer.try_recv(), Err(TryRecvError::Empty)));
    *canceled.lock().unwrap() = false;
    lane.publish_names(Vec::new());
    assert_eq!(
        answer.recv_timeout(HANG).unwrap().unwrap_err(),
        Error::Canceled
    );
    let live = Arc::new(Mutex::new(true));
    let (reply, answer) = mpsc::sync_channel(1);
    lane.schedule_names(vec![capture(Arc::clone(&live), reply)]);
    lane.read(make(), true);
    wait_ready(&shared, 2);
    lane.carried();
    lane.publish_names(Vec::new());
    let Answer::Captured(ticket) = answer.recv_timeout(HANG).unwrap().unwrap() else {
        panic!("capture");
    };
    assert_eq!(ticket.waiter().pid, 7700);
    let stale_input = make();
    let (reply, answer) = mpsc::sync_channel(1);
    lane.schedule_names(vec![NameRequest::new(
        Operation::Apply {
            ticket: ticket.clone(),
            word: TierB::Lock,
        },
        Arc::clone(&live),
        reply,
    )]);
    lane.read(make(), true);
    wait_ready(&shared, 3);
    lane.carried();
    lane.publish_names(Vec::new());
    assert!(
        matches!(answer.try_recv(), Err(TryRecvError::Empty)),
        "application needs a later probe"
    );
    lane.read(stale_input, true);
    wait_state(
        &shared,
        "the pre-apply observation was not rejected",
        |state| state.discarded > 0 && state.input.is_none(),
    );
    assert_eq!(
        (
            seen.recv_timeout(HANG).unwrap(),
            seen.recv_timeout(HANG).unwrap(),
            seen.recv_timeout(HANG).unwrap()
        ),
        ((1, true), (2, true), (3, true))
    );
    assert!(
        matches!(seen.try_recv(), Err(TryRecvError::Empty)),
        "an old observation is not a later probe"
    );
    lane.read(make(), true);
    wait_ready(&shared, 5);
    let applied = lane.carried();
    assert!(
        applied
            .jobs
            .as_deref()
            .unwrap()
            .rows
            .iter()
            .any(|r| r.named.as_ref().is_some_and(|n| n.word == TierB::Lock))
    );
    let mut reuse = make();
    reuse.req.jobs_now = true;
    reuse.req.session_ids.push((1, "reuse".into()));
    lane.read(reuse, true);
    assert!(
        matches!(answer.try_recv(), Err(TryRecvError::Empty)),
        "application waits for publication"
    );
    lane.publish_names(Vec::new());
    assert!(matches!(
        answer.recv_timeout(HANG).unwrap().unwrap(),
        Answer::Applied { .. }
    ));
    wait_ready(&shared, 6);
    let changed = lane.carried();
    assert!(
        changed
            .jobs
            .as_deref()
            .unwrap()
            .rows
            .iter()
            .all(|r| r.named.is_none())
    );
    lane.publish_names(Vec::new());
    let (reply, answer) = mpsc::sync_channel(1);
    lane.schedule_names(vec![NameRequest::new(
        Operation::Apply {
            ticket,
            word: TierB::Lock,
        },
        live,
        reply,
    )]);
    lane.read(make(), true);
    wait_ready(&shared, 7);
    lane.carried();
    lane.publish_names(Vec::new());
    assert_eq!(
        answer.recv_timeout(HANG).unwrap().unwrap_err(),
        Error::Engine(NamingError::Stale)
    );
    lane.at_stop(&[], 0);
    wait_dead(&shared);
}

/// Losing an assessment must not restart its history or stop the machine reader. Requests
/// extracted by the failed operation disconnect; still-queued requests get unavailable.
#[test]
fn a_failed_assessment_leaves_machine_readings_available_and_names_unavailable() {
    use crate::sys_naming::{Error, Operation, Request as NameRequest};
    use aterm_vitals::sys::query::NameTarget;
    let (entered, at_gate) = mpsc::sync_channel(1);
    let (release, go) = mpsc::sync_channel(1);
    let mut lane = Lane::threaded(move |_, _| {
        entered.send(()).unwrap();
        go.recv_timeout(HANG).unwrap();
        panic!("injected assessment failure");
    });
    let shared = shared(&lane);
    let request = |reply| {
        NameRequest::new(
            Operation::Capture {
                session: 1,
                target: NameTarget::Job("absent".into()),
            },
            Arc::new(Mutex::new(true)),
            reply,
        )
    };
    let (first, first_answer) = mpsc::sync_channel(1);
    lane.schedule_names(vec![request(first)]);
    let epoch = Instant::now() - std::time::Duration::from_secs(10);
    lane.read(input(epoch, Instant::now(), 1), true);
    at_gate.recv_timeout(HANG).unwrap();
    let (second, second_answer) = mpsc::sync_channel(1);
    lane.schedule_names(vec![request(second)]);
    release.send(()).unwrap();
    wait_dead(&shared);
    assert!(matches!(
        first_answer.recv_timeout(HANG),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(
        second_answer.recv_timeout(HANG).unwrap().unwrap_err(),
        Error::Unavailable
    );
    let mut reader = Reader::new(Arc::default());
    reader.assessment = lane;
    let before = reader.read(&Request::default());
    let after = reader.read(&Request::default());
    assert!(after.seq > before.seq && after.at_ns > before.at_ns);
    assert!(before.jobs.is_none() && after.jobs.is_none());
    assert!(!reader.assessment.continuous());
    assert_eq!(
        reader.assessment.work(),
        Work::None,
        "the failure is observed once"
    );
    reader.assessment.at_stop(&[], 0);
}
