// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Cohort, Lifetime, Look, RecordedCohort, Scope, look, same_leader, settled};
use aterm_uds::spawnfd;
use std::time::{Duration, Instant};

struct Running(spawnfd::Child);

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn token() -> Cohort {
    Cohort {
        scope: Scope {
            host: "linux:11111111111111111111111111111111".into(),
            boot: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            namespace: Some("1234".into()),
        },
        group: 42,
        leader_serial: 17,
    }
}

/// An unknown scope or foreign namespace cannot release a reservation even when a local
/// lookup of the same number would find nothing. A proven reboot can.
#[test]
fn a_group_check_is_only_authoritative_in_its_captured_scope() {
    let cohort = token();
    assert_eq!(
        cohort.lifetime_over(None, |_| panic!("no scope")),
        Lifetime::Unknown
    );
    let mut foreign = cohort.scope.clone();
    foreign.host = "host-b".into();
    foreign.boot = "boot-b".into();
    assert_eq!(
        cohort.lifetime_over(Some(foreign), |_| panic!("foreign host")),
        Lifetime::Unknown
    );
    let mut other = cohort.scope.clone();
    other.namespace = Some("ns-b".into());
    assert_eq!(
        cohort.lifetime_over(Some(other), |_| panic!("foreign namespace")),
        Lifetime::Unknown
    );
    let mut reboot = cohort.scope.clone();
    reboot.boot = "boot-b".into();
    assert_eq!(
        cohort.lifetime_over(Some(reboot), |_| panic!("new boot")),
        Lifetime::Drained
    );
    for found in [Lifetime::Present, Lifetime::Unknown, Lifetime::Drained] {
        assert_eq!(
            cohort.lifetime_over(Some(cohort.scope.clone()), |group| {
                assert_eq!(group, 42);
                found
            }),
            found
        );
        assert_eq!(found.retains(), found != Lifetime::Drained);
    }
}

#[test]
fn durable_tokens_keep_scope_and_reject_incomplete_or_special_group_selectors() {
    let token = token();
    assert_eq!(Cohort::decode(&token.encode()), Some(token.clone()));
    for invalid in [0u64, 1, u64::MAX] {
        let mut value = token.encode();
        value
            .as_object_mut()
            .unwrap()
            .insert("group".into(), aterm_json::Value::from(invalid));
        assert!(Cohort::decode(&value).is_none());
    }
    for field in ["host", "boot", "namespace"] {
        let mut value = token.encode();
        value
            .as_object_mut()
            .unwrap()
            .get_mut("scope")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(Cohort::decode(&value).is_none());
    }
    for (field, invalid) in [
        ("host", "host-a"),
        ("boot", "torn"),
        ("namespace", "unknown"),
    ] {
        let mut value = token.encode();
        value
            .as_object_mut()
            .unwrap()
            .get_mut("scope")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.into(), aterm_json::Value::from(invalid));
        assert!(Cohort::decode(&value).is_none());
    }
    let record = token.record();
    assert_eq!(
        RecordedCohort::decode(&record.encode()),
        Some(record.clone())
    );
    let mut unsupported = record.encode();
    unsupported
        .as_object_mut()
        .unwrap()
        .insert("v".into(), aterm_json::Value::from(2u64));
    assert!(RecordedCohort::decode(&unsupported).is_none());
}

/// The actual self-led child has to run, and only its completed kernel group drains the
/// token. A child in the caller's group is not eligible, nor one already reaped.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn only_an_actual_self_led_child_can_be_captured() {
    let mut child = Running(
        spawnfd::command("/bin/cat")
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("self-led cat"),
    );
    let input = aterm_uds::deadline::cat_echoes(&mut child.0);
    let cohort = Cohort::of_child(&mut child.0).expect("actual child cohort");
    let recorded =
        RecordedCohort::decode(&cohort.record().encode()).expect("complete actual record");
    assert_eq!(cohort.group(), i32::try_from(child.0.id()).unwrap());
    assert_eq!(cohort.lifetime(), Lifetime::Present);
    assert_eq!(recorded.lifetime(), Lifetime::Present);
    drop(input);
    child.0.wait().expect("cat ended");
    assert_eq!(cohort.lifetime(), Lifetime::Drained);
    assert_eq!(recorded.lifetime(), Lifetime::Drained);
    assert!(
        Cohort::of_child(&mut child.0).is_err(),
        "an ended child is not a new cohort"
    );

    let mut inherited = Running(
        spawnfd::command("/bin/cat")
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::piped())
            .spawn()
            .expect("cat in caller's group"),
    );
    let input = aterm_uds::deadline::cat_echoes(&mut inherited.0);
    assert!(
        Cohort::of_child(&mut inherited.0).is_err(),
        "a shared group cannot be admitted as this child"
    );
    drop(input);
    inherited.0.wait().expect("inherited cat ended");
}

/// A child that can no longer be read is looked at again until it can be reaped.
#[test]
fn a_child_on_its_way_out_is_looked_at_until_it_ends() {
    let mut looks = vec![Look::Ended, Look::Leaving, Look::Leaving];
    let started = Instant::now() - Duration::from_secs(2);
    let found = settled(started, || Ok(looks.pop().expect("a look too many"))).unwrap();
    assert!(matches!(found, Look::Ended));
    assert!(looks.is_empty());
}

/// A refused child has settled: its refusal does not wait on the kernel.
#[test]
fn a_refused_child_is_looked_at_once() {
    let mut looks = 0;
    let found = settled(Instant::now(), || {
        looks += 1;
        Ok(Look::Refused)
    });
    assert!(matches!(found.unwrap(), Look::Refused));
    assert_eq!(looks, 1);
}

/// A CHILD THAT ENDED BEFORE ITS CAPTURE IS REAPED WITH ITS ACCOUNTING: the capture's reap is
/// the child's last, so its owner, a supervised build's front, reads the root's own resident
/// peak from it ([`spawnfd::Child::usage`]). NEGATIVE CONTROL (by hand: the capture reaping
/// with a plain `try_wait`): no accounting kept.
#[cfg(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
#[test]
fn a_child_that_ended_before_its_capture_keeps_its_accounting() {
    let mut child = spawnfd::command("/bin/sh")
        .args(["-c", "exit 3"])
        .process_group(0)
        .spawn()
        .expect("self-led sh");
    child.wait_exited().expect("its exit, unreaped");
    let found = look(&mut child, &token().scope).unwrap();
    assert!(matches!(found, Look::Ended));
    let usage = child
        .usage()
        .expect("the capture's reap kept its accounting");
    assert_eq!(usage.status.code(), Some(3));
    assert_eq!(child.wait().expect("its status").code(), Some(3));
}

/// A live child refused is `Refused`, not one to wait for: it is captured in another scope.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_live_child_that_will_not_do_is_refused_not_waited_for() {
    let mut child = Running(
        spawnfd::command("/bin/cat")
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("self-led cat"),
    );
    let input = aterm_uds::deadline::cat_echoes(&mut child.0);
    let found = look(&mut child.0, &token().scope).unwrap();
    assert!(matches!(found, Look::Refused));
    drop(input);
    child.0.wait().expect("cat ended");
}

/// The wait has a bound: past it, a child still leaving is the answer.
#[test]
fn a_child_still_leaving_at_the_deadline_ends_the_wait() {
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut looks = 0;
        let found = settled(Instant::now() - Duration::from_secs(61), || {
            looks += 1;
            Ok(Look::Leaving)
        });
        let _ = done.send((found.map(|look| matches!(look, Look::Leaving)), looks));
    });
    let (leaving, looks) = ended
        .recv_timeout(Duration::from_secs(60))
        .expect("the deadline ends the wait");
    assert!(leaving.unwrap());
    assert_eq!(looks, 1);
}

/// A child that `exec`s between two reads is still the leader they read, though the kernel
/// moved its pid version: `cat` echoing proves the `exec` came between them.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn two_reads_either_side_of_an_exec_are_the_same_leader() {
    use std::io::Write as _;
    let mut child = Running(
        spawnfd::command("/bin/sh")
            .args(["-c", "read go; exec /bin/cat"])
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("self-led sh"),
    );
    let pid = i32::try_from(child.0.id()).unwrap();
    let before = aterm_uds::process::read_lineage(pid).expect("sh reads");
    let mut input = child.0.stdin.take().expect("sh's input");
    input.write_all(b"go\nran\n").unwrap();
    aterm_uds::deadline::prints(&mut child.0, "ran", aterm_uds::deadline::ECHO_HANG);
    let after = aterm_uds::process::read_lineage(pid).expect("cat reads");
    if aterm_uds::process::KEEPS_PID_VERSIONS {
        assert_ne!(
            before.identity, after.identity,
            "the exec moved its version"
        );
    }
    assert!(same_leader(&before, &after, child.0.id()));
    let mut stranger = after;
    stranger.uniqueid += 1;
    assert!(!same_leader(&before, &stranger, child.0.id()));
    drop(input);
    child.0.wait().expect("cat ended");
}
