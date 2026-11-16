// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet retry remains live while a real repository item is held at its injected effect.

use super::*;
use crate::init::{CloneOwedOutcome, OwedClone};
use crate::sync::fleet_retry::{Event, Wait, Woke};
use std::sync::mpsc::{self, Receiver, Sender};

const HANG: Duration = Duration::from_secs(60);

type Next<'a> = dyn FnMut(&Receiver<Event>, Option<Duration>, &mut u64) -> Woke + 'a;

struct Clock<'a> {
    now: u64,
    next: Box<Next<'a>>,
}

impl Wait for Clock<'_> {
    fn now_ms(&self) -> u64 {
        self.now
    }

    fn next(&mut self, events: &Receiver<Event>, pause: Option<Duration>) -> Woke {
        (self.next)(events, pause, &mut self.now)
    }
}

fn progress(events: &Receiver<Event>) -> Woke {
    Woke::Event(
        events
            .recv_timeout(HANG)
            .expect("repository lane made no progress"),
    )
}

struct HeldClone {
    started: Sender<()>,
    entered: Mutex<Receiver<()>>,
    release: Sender<()>,
    held: Mutex<Receiver<()>>,
    calls: AtomicU32,
    finished: AtomicBool,
}

impl HeldClone {
    fn new() -> Self {
        let (started, entered) = mpsc::channel();
        let (release, held) = mpsc::channel();
        Self {
            started,
            entered: Mutex::new(entered),
            release,
            held: Mutex::new(held),
            calls: AtomicU32::new(0),
            finished: AtomicBool::new(false),
        }
    }

    fn owed(&self) -> Result<OwedClone, String> {
        Ok(if self.finished.load(Ordering::SeqCst) {
            OwedClone::Verified(crate::init_observe::RepositoryCounts {
                ready: 1,
                ..Default::default()
            })
        } else {
            OwedClone::Due("the private inventory owes one clone".into())
        })
    }

    fn run(&self) -> Result<CloneOwedOutcome, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.send(()).unwrap();
        self.held
            .lock()
            .unwrap()
            .recv_timeout(HANG)
            .expect("the admitted clone was never released");
        self.finished.store(true, Ordering::SeqCst);
        Ok(CloneOwedOutcome {
            said: "cloned".into(),
            readback: None,
        })
    }
}

enum Fault {
    Fleet,
    FleetThenFabric,
    Fabric,
}

fn coordination_before_clone_finishes(fault: Fault) {
    let repair_failure = matches!(fault, Fault::FleetThenFabric);
    let fleet_current = matches!(fault, Fault::Fabric);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let clone = HeldClone::new();
    let held = || None;
    let owed = |_: Option<&crate::init_github::RepositoryReadback>| clone.owed();
    let effect = || clone.run();
    let ports = owed_clone::Hooked {
        inner: Inert::new(scratch.path()),
        held: &held,
        owed: &owed,
        clone: &effect,
        control: None,
        forget: &|| Ok(()),
    };
    *ports.inner.node.lock().unwrap() = NodeRead::Due {
        from: "the node is missing".into(),
        to: "the node runs".into(),
    };
    if fleet_current {
        *ports.inner.fleet.lock().unwrap() = FleetRead::Current("already joined".into());
        *ports.inner.renewed.lock().unwrap() = Err("the broker was briefly busy".into());
    } else {
        *ports.inner.fleet_done.lock().unwrap() = Err("the issuer was briefly full".into());
    }
    let mut waits = 0;
    let mut clock = Clock {
        now: 0,
        next: Box::new(|events, pause, now| {
            if waits > usize::from(repair_failure) {
                assert_eq!(pause, None, "a successful fleet is never retried again");
                return progress(events);
            }
            if waits == 1 {
                waits += 1;
                assert_eq!(pause, Some(Duration::from_secs(10)));
                assert!(!clone.finished.load(Ordering::SeqCst));
                *ports.inner.renewed.lock().unwrap() = Ok("the broker now serves the fleet".into());
                *now += 10_000;
                return Woke::Retry;
            }
            waits += 1;
            assert_eq!(pause, Some(Duration::from_secs(5)));
            clone.entered.lock().unwrap().recv_timeout(HANG).unwrap();
            assert_eq!(clone.calls.load(Ordering::SeqCst), 1);
            assert!(!clone.finished.load(Ordering::SeqCst));
            assert!(
                try_lock(scratch.path()).unwrap().is_none(),
                "same admitted pass"
            );
            assert!(!ports.inner.applies().contains(&"requests"));
            *ports.inner.fleet_done.lock().unwrap() = Ok(FleetDone::Synced("joined".into()));
            *ports.inner.node.lock().unwrap() = NodeRead::Due {
                from: "the node lacks the new fleet".into(),
                to: "the node serves the new fleet".into(),
            };
            *ports.inner.fabric.lock().unwrap() = FabricRead::Due("the fleet changed".into());
            if repair_failure {
                *ports.inner.renewed.lock().unwrap() = Err("the broker was briefly busy".into());
            } else {
                *ports.inner.renewed.lock().unwrap() = Ok("the broker now serves the fleet".into());
            }
            *now += 5_000;
            Woke::Retry
        }),
    };
    let mut fleet_events = Vec::new();
    let mut released = false;
    let pass = ran(run_pass_waiting(
        &ports,
        Trigger::Subscriber,
        Mode::Apply,
        aterm_secrets::converge::RepositoryWork::Refresh.into(),
        &mut |row| {
            if row.id == FLEET {
                fleet_events.push(row.outcome);
            }
            if row.id == FABRIC && clone.calls.load(Ordering::SeqCst) > 0 && !released {
                if row.outcome == Outcome::Failed {
                    assert!(repair_failure);
                    assert!(!clone.finished.load(Ordering::SeqCst));
                    return;
                }
                assert_eq!(row.outcome, Outcome::Changed);
                assert_eq!(clone.calls.load(Ordering::SeqCst), 1);
                assert!(
                    !clone.finished.load(Ordering::SeqCst),
                    "fleet repair preceded Git"
                );
                assert_eq!(
                    ports
                        .inner
                        .applies()
                        .iter()
                        .filter(|&&k| k == "node")
                        .count(),
                    2
                );
                assert!(try_lock(scratch.path()).unwrap().is_none());
                released = true;
                clone.release.send(()).unwrap();
            }
            if row.id == REQUESTS {
                assert!(released && clone.finished.load(Ordering::SeqCst));
            }
        },
        &mut clock,
    ));
    assert!(released);
    assert_eq!(
        fleet_events,
        if fleet_current {
            vec![Outcome::Ok, Outcome::Ok]
        } else if repair_failure {
            vec![Outcome::Failed, Outcome::Changed, Outcome::Ok]
        } else {
            vec![Outcome::Failed, Outcome::Changed]
        }
    );
    assert_eq!(
        ports
            .inner
            .applies()
            .iter()
            .filter(|&&k| k == "fleet")
            .count(),
        if fleet_current { 0 } else { 2 }
    );
    for id in [FLEET, NODE, FABRIC, CLONE] {
        let expected =
            if (repair_failure && matches!(id, FLEET | NODE)) || (fleet_current && id == FLEET) {
                Outcome::Ok
            } else {
                Outcome::Changed
            };
        assert_eq!(row(&pass.rows, id).outcome, expected, "{id}");
        assert_eq!(pass.rows.iter().filter(|row| row.id == id).count(), 1);
    }
    assert_eq!(clone.calls.load(Ordering::SeqCst), 1);
    assert!(try_lock(scratch.path()).unwrap().is_some());
    let record = read_record(scratch.path()).unwrap();
    assert_eq!(
        record
            .rows
            .iter()
            .find(|row| row.id == FLEET)
            .unwrap()
            .outcome,
        if repair_failure || fleet_current {
            Outcome::Ok
        } else {
            Outcome::Changed
        }
    );
    let history = std::fs::read_to_string(FileJournal::under(scratch.path()).path()).unwrap();
    assert!(history.lines().any(
        |line| line.contains(if fleet_current { FABRIC } else { FLEET }) && line.contains("failed")
    ));
}

#[test]
fn fleet_and_its_node_and_fabric_finish_before_a_long_clone_is_released() {
    coordination_before_clone_finishes(Fault::Fleet);
}

#[test]
fn a_transient_dependent_repair_also_retries_before_the_clone_finishes() {
    coordination_before_clone_finishes(Fault::FleetThenFabric);
}

#[test]
fn a_fabric_fault_retries_beside_a_clone_when_the_fleet_is_already_current() {
    coordination_before_clone_finishes(Fault::Fabric);
}

#[test]
fn a_current_stop_or_off_prevents_another_fleet_attempt_beside_the_clone() {
    for choice in ["vault hold", "swarm off", "fleet leave", "other fleet"] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let clone = HeldClone::new();
        let hold = Mutex::new(None);
        let held = || hold.lock().unwrap().clone();
        let owed = |_: Option<&crate::init_github::RepositoryReadback>| clone.owed();
        let effect = || clone.run();
        let ports = owed_clone::Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &owed,
            clone: &effect,
            control: None,
            forget: &|| Ok(()),
        };
        *ports.inner.fleet_done.lock().unwrap() = Err("issuer busy".into());
        let mut first = true;
        let mut clock = Clock {
            now: 0,
            next: Box::new(|events, pause, now| {
                if !first {
                    assert_eq!(pause, None, "a current choice ends in-pass retries");
                    return progress(events);
                }
                first = false;
                clone.entered.lock().unwrap().recv_timeout(HANG).unwrap();
                match choice {
                    "vault hold" => *hold.lock().unwrap() = Some("a Stop holds the vault".into()),
                    "swarm off" => *ports.inner.swarm.lock().unwrap() = Ok(false),
                    "fleet leave" => {
                        *ports.inner.fleet.lock().unwrap() =
                            FleetRead::Idle("fleet sync is off after leave".into())
                    }
                    "other fleet" => {
                        *ports.inner.fleet.lock().unwrap() =
                            FleetRead::Blocked("this Mac joined another fleet by hand".into())
                    }
                    _ => unreachable!(),
                }
                *now += u64::try_from(pause.unwrap().as_millis()).unwrap();
                Woke::Retry
            }),
        };
        let mut released = false;
        let pass = ran(run_pass_waiting(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh.into(),
            &mut |row| {
                if row.id == FLEET
                    && matches!(row.outcome, Outcome::NotApplicable | Outcome::Blocked)
                {
                    assert!(!released);
                    released = true;
                    clone.release.send(()).unwrap();
                }
            },
            &mut clock,
        ));
        assert!(released, "{choice}");
        assert_eq!(
            row(&pass.rows, FLEET).outcome,
            if choice == "other fleet" {
                Outcome::Blocked
            } else {
                Outcome::NotApplicable
            }
        );
        assert_eq!(
            ports
                .inner
                .applies()
                .iter()
                .filter(|&&k| k == "fleet")
                .count(),
            1,
            "{choice}"
        );
        assert_eq!(
            clone.calls.load(Ordering::SeqCst),
            1,
            "no duplicate repository helper"
        );
        assert!(try_lock(scratch.path()).unwrap().is_some());
    }
}

#[test]
fn repository_completion_ends_in_pass_retries_without_delaying_acquisition() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let clone = HeldClone::new();
    let held = || None;
    let owed = |_: Option<&crate::init_github::RepositoryReadback>| clone.owed();
    let effect = || clone.run();
    let ports = owed_clone::Hooked {
        inner: Inert::new(scratch.path()),
        held: &held,
        owed: &owed,
        clone: &effect,
        control: None,
        forget: &|| Ok(()),
    };
    *ports.inner.fleet_done.lock().unwrap() = Err("issuer busy".into());
    let mut pauses = Vec::new();
    let mut clock = Clock {
        now: 0,
        next: Box::new(|events, pause, now| {
            let pause = pause.unwrap();
            if pauses.len() < 2 {
                if pauses.is_empty() {
                    clone.entered.lock().unwrap().recv_timeout(HANG).unwrap();
                }
                pauses.push(pause);
                *now += u64::try_from(pause.as_millis()).unwrap();
                return Woke::Retry;
            }
            if pauses.len() == 2 {
                pauses.push(pause);
                clone.release.send(()).unwrap();
            }
            progress(events)
        }),
    };
    let pass = ran(run_pass_waiting(
        &ports,
        Trigger::Subscriber,
        Mode::Apply,
        aterm_secrets::converge::RepositoryWork::Refresh.into(),
        &mut |_| {},
        &mut clock,
    ));
    drop(clock);
    assert_eq!(
        pauses,
        [
            Duration::from_secs(5),
            Duration::from_secs(10),
            Duration::from_secs(20)
        ]
    );
    assert_eq!(
        ports
            .inner
            .applies()
            .iter()
            .filter(|&&k| k == "fleet")
            .count(),
        3
    );
    assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Failed);
    assert_eq!(row(&pass.rows, CLONE).outcome, Outcome::Changed);
    assert!(clone.finished.load(Ordering::SeqCst));
    assert_eq!(clone.calls.load(Ordering::SeqCst), 1);
    assert!(try_lock(scratch.path()).unwrap().is_some());
}

#[test]
fn an_interrupted_repository_helper_is_joined_without_another_fleet_attempt() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let held = || None;
    let owed = |_: Option<&crate::init_github::RepositoryReadback>| {
        Ok(OwedClone::Due("one missing clone".into()))
    };
    let effect =
        || -> Result<CloneOwedOutcome, String> { panic!("injected repository interruption") };
    let ports = owed_clone::Hooked {
        inner: Inert::new(scratch.path()),
        held: &held,
        owed: &owed,
        clone: &effect,
        control: None,
        forget: &|| Ok(()),
    };
    *ports.inner.fleet_done.lock().unwrap() = Err("issuer busy".into());
    let mut clock = Clock {
        now: 0,
        next: Box::new(|events, _, _| progress(events)),
    };
    let stopped = run_pass_waiting(
        &ports,
        Trigger::Subscriber,
        Mode::Apply,
        aterm_secrets::converge::RepositoryWork::Refresh.into(),
        &mut |_| {},
        &mut clock,
    );
    assert!(
        matches!(stopped, Err(Stop::Refused(ref why)) if why.contains("repository lane stopped unexpectedly")),
        "{stopped:?}"
    );
    assert_eq!(
        ports
            .inner
            .applies()
            .iter()
            .filter(|&&k| k == "fleet")
            .count(),
        1
    );
    assert!(!ports.inner.applies().contains(&"requests"));
    assert!(try_lock(scratch.path()).unwrap().is_some());
}
