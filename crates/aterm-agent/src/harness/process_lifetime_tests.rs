// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn captured(os: &str, namespace: Option<&str>) -> ProcessLifetime {
    ProcessLifetime {
        scope: BootStamp {
            host: format!("{os}:{}", "a".repeat(32)),
            boot: "b".repeat(32),
        },
        identity: Identity {
            pid: 4242,
            birth: Birth(1_234_001),
            idversion: None,
        },
        namespace: namespace.map(str::to_owned),
    }
}

#[test]
fn an_exact_birth_refutes_a_reused_pid_even_within_the_same_second() {
    let lifetime = captured("darwin", None);
    let replacement = Birth(1_234_999);
    // The saved lstart hint cannot distinguish these processes: this is the negative control
    // for the former whole-second check, while the durable birth must distinguish them.
    assert_eq!(
        lifetime.identity.birth.0 / 1_000_000,
        replacement.0 / 1_000_000
    );
    assert_eq!(
        lifetime.observed(
            Some(&lifetime.scope),
            None,
            ProcessPresence::Present,
            Some(replacement),
        ),
        ProcessLiveness::Ended
    );
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        assert_eq!(
            lifetime.observed(Some(&lifetime.scope), None, presence, None),
            ProcessLiveness::Unknown
        );
    }
    assert_eq!(
        lifetime.observed(
            Some(&lifetime.scope),
            None,
            ProcessPresence::Unknown,
            Some(lifetime.identity.birth),
        ),
        ProcessLiveness::Unknown
    );
    assert_eq!(
        lifetime.observed(
            Some(&lifetime.scope),
            None,
            ProcessPresence::Present,
            Some(lifetime.identity.birth),
        ),
        ProcessLiveness::Alive
    );
    for presence in [ProcessPresence::Absent, ProcessPresence::Exited] {
        assert_eq!(
            lifetime.observed(Some(&lifetime.scope), None, presence, None),
            ProcessLiveness::Ended
        );
    }
}

#[test]
fn only_the_same_machine_and_namespace_can_observe_a_pid() {
    let lifetime = captured("linux", Some("4026531836"));
    let observe = |scope, namespace| {
        lifetime.observed(
            scope,
            namespace,
            ProcessPresence::Present,
            Some(lifetime.identity.birth),
        )
    };
    assert_eq!(observe(None, Some("4026531836")), ProcessLiveness::Unknown);
    assert_eq!(
        observe(Some(&lifetime.scope), None),
        ProcessLiveness::Unknown
    );
    assert_eq!(
        observe(Some(&lifetime.scope), Some("4026531837")),
        ProcessLiveness::Unknown
    );
    assert_eq!(
        observe(Some(&lifetime.scope), Some("4026531836")),
        ProcessLiveness::Alive
    );
    let foreign = BootStamp {
        host: format!("linux:{}", "c".repeat(32)),
        ..lifetime.scope.clone()
    };
    assert_eq!(
        observe(Some(&foreign), Some("4026531836")),
        ProcessLiveness::Unknown
    );
    let rebooted = BootStamp {
        boot: "c".repeat(32),
        ..lifetime.scope.clone()
    };
    assert_eq!(
        observe(Some(&rebooted), Some("4026531836")),
        ProcessLiveness::Ended
    );
    // Absence in somebody else's scope supplies no evidence about this captured process.
    assert_eq!(
        lifetime.observed(Some(&foreign), None, ProcessPresence::Absent, None),
        ProcessLiveness::Unknown
    );
}

#[test]
fn durable_evidence_round_trips_and_rejects_incomplete_or_ambiguous_scope() {
    for lifetime in [
        captured("darwin", None),
        captured("linux", Some("4026531836")),
    ] {
        let wire = aterm_json::to_value(&lifetime).unwrap();
        assert_eq!(
            aterm_json::from_value::<ProcessLifetime>(wire.clone()).unwrap(),
            lifetime
        );
        let invalid = [
            ("host", aterm_json::json!("darwin:ABCDEF")),
            ("boot", aterm_json::json!("b".repeat(31))),
            ("pid", aterm_json::json!(1)),
            ("pid", aterm_json::json!(u32::MAX)),
            ("birth", aterm_json::json!(0)),
            ("idversion", aterm_json::json!(4)),
            ("namespace", aterm_json::json!("04026531836")),
        ];
        for (field, value) in invalid {
            let mut bad = wire.clone();
            bad.as_object_mut().unwrap().insert(field.to_owned(), value);
            assert!(
                aterm_json::from_value::<ProcessLifetime>(bad).is_err(),
                "{field}"
            );
        }
        let mut incomplete = wire;
        incomplete.as_object_mut().unwrap().remove("birth");
        assert!(aterm_json::from_value::<ProcessLifetime>(incomplete).is_err());
    }
    let mut missing_namespace =
        aterm_json::to_value(captured("linux", Some("4026531836"))).unwrap();
    missing_namespace
        .as_object_mut()
        .unwrap()
        .remove("namespace");
    assert!(aterm_json::from_value::<ProcessLifetime>(missing_namespace).is_err());
    let mut foreign_namespace = aterm_json::to_value(captured("darwin", None)).unwrap();
    foreign_namespace
        .as_object_mut()
        .unwrap()
        .insert("namespace".to_owned(), aterm_json::json!("4026531836"));
    assert!(aterm_json::from_value::<ProcessLifetime>(foreign_namespace).is_err());
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_real_lifetime_survives_exec_and_ends_only_when_its_process_exits() {
    use std::io::{BufRead as _, Write as _};

    let shell = crate::test_program::kept("/bin/sh");
    let cat = crate::test_program::kept("/bin/cat");
    let mut child = aterm_uds::spawnfd::command(shell)
        .args([
            "-c",
            "printf 'before-exec\\n'; read gate; exec \"$1\"",
            "lifetime",
        ])
        .arg(cat)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (lines, read) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let next = || {
        read.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap()
            .unwrap()
    };
    assert_eq!(next(), "before-exec");
    let lifetime = ProcessLifetime::capture(child.id()).expect("the live original shell");
    let before = process::read_identity(child.id() as i32).unwrap();
    assert_eq!(
        lifetime.identity.birth, before.birth,
        "retain the kernel's full birth"
    );
    assert_eq!(lifetime.observe(), ProcessLiveness::Alive);
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"go\nafter-exec\n").unwrap();
    assert_eq!(next(), "after-exec", "the same process has executed cat");
    let after = process::read_identity(child.id() as i32).unwrap();
    assert_eq!(before.birth, after.birth);
    if process::KEEPS_PID_VERSIONS {
        assert_ne!(
            before.idversion, after.idversion,
            "the kernel witnessed the exec"
        );
    }
    assert_eq!(ProcessLifetime::capture(child.id()), Some(lifetime.clone()));
    assert_eq!(lifetime.observe(), ProcessLiveness::Alive);
    aterm_uds::deadline::kill_within(&mut child, aterm_spec::wait::MUST_HAPPEN);
    assert!(child.try_wait().unwrap().is_some(), "owning reap completed");
    assert_eq!(lifetime.observe(), ProcessLiveness::Ended);
    assert_eq!(lifetime.clone().retained_if_current(), None);
    drop(input);
    reader.join().unwrap();
}
