// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::Path;
use std::time::Duration;

struct Subject {
    child: spawnfd::Child,
    identity: Identity,
}

impl Subject {
    fn new() -> Self {
        let mut command = spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/cat")));
        command
            .process_group(0)
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::null())
            .stderr(spawnfd::Stdio::null());
        let child = command.spawn().expect("original subject");
        let identity = read_identity(child.id() as i32).expect("original birth");
        Self { child, identity }
    }
}

impl Drop for Subject {
    fn drop(&mut self) {
        aterm_uds::deadline::settle_child(&mut self.child);
    }
}

#[test]
fn a_functional_wait_keeps_native_progress_without_inventing_a_take_budget() {
    fn take(timed: bool, moving: bool) -> Result<usize, CouldNotRun> {
        let start = Instant::now();
        let now = Cell::new(start);
        let probes = Cell::new(0);
        let generation = Cell::new(0_u64);
        let budget = timed.then_some(Budget {
            ends: start + Duration::from_secs(20),
            secs: 20,
        });
        working(
            budget,
            "the functional result",
            POLL_GAP,
            Progress {
                now: &mut || now.get(),
                runs: &mut || true,
                read: &mut || {
                    if moving {
                        generation.set(generation.get() + 1);
                    }
                    Some(generation.get())
                },
                pause: &mut |_| now.set(now.get() + Duration::from_secs(75)),
            },
            || {
                probes.set(probes.get() + 1);
                (probes.get() == 4).then_some(probes.get())
            },
        )
    }
    assert_eq!(take(false, true).expect("working beyond a finite take"), 4);
    assert!(
        take(true, true).is_err(),
        "the existing finite take still expires"
    );
    assert!(
        take(false, false).is_err(),
        "functional does not admit a stalled subject"
    );
}

#[test]
fn native_observation_cost_is_not_mistaken_for_a_subjects_idle_window() {
    let start = Instant::now();
    let now = Cell::new(start);
    let first = Cell::new(true);
    let probes = Cell::new(0);
    let result = working(
        None,
        "the observed result",
        POLL_GAP,
        Progress {
            now: &mut || now.get(),
            runs: &mut || true,
            read: &mut || {
                if first.replace(false) {
                    now.set(now.get() + Duration::from_secs(120));
                }
                Some(1)
            },
            pause: &mut |_| now.set(now.get() + Duration::from_secs(10)),
        },
        || {
            probes.set(probes.get() + 1);
            (probes.get() == 4).then_some(())
        },
    );
    result.expect("the minute starts after the native observation");
}

#[test]
fn functional_control_collects_complete_output_and_rejects_overflow() {
    let subject = Subject::new();
    let mut echo = spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/echo")));
    echo.arg("complete reply");
    let reply = output(&mut echo, subject.identity).expect("complete output");
    assert!(reply.status.success());
    assert_eq!(reply.stdout, b"complete reply\n");

    let mut large = spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/usr/bin/head")));
    large.args(["-c", &(OUTPUT_LIMIT + 1).to_string(), "/dev/zero"]);
    let error = output(&mut large, subject.identity).expect_err("a retained suffix is no receipt");
    assert!(
        error.to_string().contains("complete capture limit"),
        "{error}"
    );
    assert_eq!(
        read_identity(subject.identity.pid),
        Some(subject.identity),
        "the server is not the disposable client"
    );
}

#[test]
fn a_wrong_subject_birth_starts_no_control_client() {
    let subject = Subject::new();
    let mut wrong = subject.identity;
    wrong.birth.0 = wrong.birth.0.wrapping_add(1);
    let root = aterm_tempfile::tempdir().expect("private marker");
    let marker = root.path().join("client-started");
    let mut touch = spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/usr/bin/touch")));
    touch.arg(&marker);
    assert!(output(&mut touch, wrong).is_err());
    assert!(
        !marker.exists(),
        "identity refusal precedes any client side effect"
    );
}

#[test]
fn checked_finish_reaps_the_original_and_never_turns_unread_inventory_into_empty() {
    let mut subject = Subject::new();
    let result = finish_with(
        &mut subject.child,
        |_| Inventory::refused("unread inventory"),
        |_| panic!("no known helper"),
    );
    assert!(result.is_err());
    assert!(
        subject.child.is_reaped(),
        "failure still retires the original child"
    );

    let mut subject = Subject::new();
    let expected = subject.child.id();
    let listed = Cell::new(false);
    assert_eq!(
        finish_with(
            &mut subject.child,
            |pid| {
                assert_eq!(pid, expected);
                listed.set(true);
                Inventory::default()
            },
            |_| panic!("known empty inventory")
        )
        .expect("known empty inventory and actual reap"),
        0
    );
    assert!(listed.get());
    assert!(subject.child.is_reaped());
    assert!(
        finish_with(
            &mut subject.child,
            |_| panic!("released PID is not inventory custody"),
            |_| panic!("released PID supplies no helpers")
        )
        .is_err()
    );
}

#[test]
fn a_listed_pid_is_not_custody_without_its_original_parent_in_the_same_read() {
    let subject = Subject::new();
    let line = aterm_uds::process::read_lineage(subject.identity.pid).expect("owned child lineage");
    assert_eq!(line.parent, std::process::id());
    let known = original_children(std::process::id() as i32, vec![line.identity.pid], |_| {
        Some(line)
    });
    assert!(known.failures.is_empty());
    assert_eq!(known.helpers, vec![subject.identity]);
    let mut reused = line;
    reused.parent = 1;
    reused.identity.birth.0 = reused.identity.birth.0.wrapping_add(1);
    assert!(
        original_children(
            std::process::id() as i32,
            vec![line.identity.pid],
            |_| Some(reused)
        )
        .helpers
        .is_empty(),
        "a reused foreign PID must not become an owned helper"
    );
    assert!(
        !original_children(std::process::id() as i32, vec![line.identity.pid], |_| None)
            .failures
            .is_empty(),
        "unread inventory is not an empty success"
    );
}

#[test]
fn an_incomplete_inventory_still_retires_every_independently_proven_helper() {
    let mut original = Subject::new();
    let left = Subject::new();
    let right = Subject::new();
    let mut lines =
        [left.identity, right.identity].map(|id| aterm_uds::process::read_lineage(id.pid).unwrap());
    for line in &mut lines {
        line.parent = original.child.id();
    }
    let mut visited = Vec::new();
    let outcome = finish_with(
        &mut original.child,
        |parent| {
            original_children(
                parent as i32,
                vec![left.identity.pid, i32::MAX, right.identity.pid],
                |pid| lines.iter().find(|line| line.identity.pid == pid).copied(),
            )
        },
        |identity| {
            visited.push(identity);
            // Simulate one independent retirement refusal: the next proven helper must still
            // be attempted, while neither this refusal nor the unread middle PID can pass.
            if identity == left.identity {
                Err(CouldNotRun::Refused("retirement refused".into()))
            } else {
                Ok(())
            }
        },
    );
    assert!(original.child.is_reaped());
    assert_eq!(visited, vec![left.identity, right.identity]);
    let error = outcome.unwrap_err().to_string();
    assert!(
        error.contains("no proven identity") && error.contains("retirement refused"),
        "{error}"
    );
}

fn retirement_identity() -> Identity {
    Identity {
        pid: 42,
        birth: aterm_uds::process::Birth(17),
        idversion: Some(3),
    }
}

#[test]
fn helper_disappearance_requires_an_absence_read_after_the_identity_read() {
    let original = retirement_identity();
    let identity_read = Cell::new(false);
    let reads = RefCell::new(Vec::new());
    let state = retirement_with(
        original,
        |pid| {
            assert_eq!(pid, original.pid);
            reads.borrow_mut().push("identity");
            identity_read.set(true);
            None
        },
        |pid| {
            assert_eq!(pid, original.pid);
            reads.borrow_mut().push("presence");
            // The helper vanishes during its identity read. The old presence-first order
            // would observe Present followed by None and incorrectly refuse this cleanup.
            if identity_read.get() {
                ProcessPresence::Absent
            } else {
                ProcessPresence::Present
            }
        },
    )
    .unwrap();
    assert_eq!(state, Retirement::Gone);
    assert_eq!(*reads.borrow(), ["identity", "presence"]);
}

#[test]
fn exited_helpers_are_pending_and_unread_present_helpers_are_not_gone() {
    let original = retirement_identity();
    assert_eq!(
        retirement_with(original, |_| None, |_| ProcessPresence::Exited).unwrap(),
        Retirement::Exited
    );
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        assert!(
            retirement_with(original, |_| None, |_| presence).is_err(),
            "{presence:?}"
        );
    }
    assert_eq!(
        begin_retirement(
            original,
            || Ok(Retirement::Exited),
            || panic!("an unread exited identity must not be signalled")
        )
        .unwrap(),
        Retirement::Exited
    );

    let mut reused = original;
    reused.birth.0 += 1;
    assert_eq!(
        retirement_with(
            original,
            |_| Some(reused),
            |_| panic!("a different original birth already proves retirement")
        )
        .unwrap(),
        Retirement::Gone
    );
    let mut exec = original;
    exec.idversion = Some(4);
    assert_eq!(
        retirement_with(original, |_| Some(exec), |_| ProcessPresence::Present).unwrap(),
        Retirement::Present
    );
    assert!(
        begin_retirement(
            original,
            || Ok(Retirement::Present),
            || Err(aterm_uds::process::EndRefusal::Stale)
        )
        .is_err(),
        "an exec is not retirement or fresh signalling authority"
    );
}

#[test]
fn a_readable_original_identity_still_observes_a_fresh_native_exit() {
    let original = retirement_identity();
    let mut exec = original;
    exec.idversion = Some(4);
    for readable in [original, exec] {
        for (presence, expected) in [
            (ProcessPresence::Absent, Retirement::Gone),
            (ProcessPresence::Exited, Retirement::Exited),
        ] {
            let state = begin_retirement(
                original,
                || retirement_with(original, |_| Some(readable), |_| presence),
                || panic!("fresh native exit/absence must not attempt another end"),
            )
            .unwrap();
            assert_eq!(state, expected, "{readable:?}: {presence:?}");
        }
        for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
            assert_eq!(
                retirement_with(original, |_| Some(readable), |_| presence).unwrap(),
                Retirement::Present,
                "readable same-birth identity still requires exact end_proven authority"
            );
        }
    }
}

#[test]
fn a_helper_exiting_during_end_proven_is_waited_for_without_another_signal() {
    let original = retirement_identity();
    for after in [Retirement::Gone, Retirement::Exited, Retirement::Present] {
        let reads = Cell::new(0);
        let ends = Cell::new(0);
        let result = begin_retirement(
            original,
            || {
                let n = reads.get();
                reads.set(n + 1);
                Ok(if n == 0 { Retirement::Present } else { after })
            },
            || {
                ends.set(ends.get() + 1);
                Err(aterm_uds::process::EndRefusal::Stale)
            },
        );
        assert_eq!(reads.get(), 2);
        assert_eq!(ends.get(), 1, "no re-signal after authority was lost");
        match after {
            Retirement::Gone | Retirement::Exited => assert_eq!(result.unwrap(), after),
            Retirement::Present => assert!(result.is_err()),
        }
    }
    assert_eq!(
        begin_retirement(
            original,
            || Ok(Retirement::Gone),
            || panic!("known retirement starts no signal")
        )
        .unwrap(),
        Retirement::Gone
    );
    assert_eq!(
        begin_retirement(original, || Ok(Retirement::Present), || Ok(())).unwrap(),
        Retirement::Present,
        "an end request alone supplies no absence or helper-reap receipt"
    );
}
