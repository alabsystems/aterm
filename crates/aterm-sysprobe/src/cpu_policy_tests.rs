// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn id() -> Identity {
    Identity {
        pid: 7,
        start_s: 100,
        start_us: 5,
    }
}

fn capture() -> Capture {
    Capture {
        collector: id(),
        parent_pid: 6,
        effective_uid: 0,
        began_unix_ms: 1000,
        ended_unix_ms: 1001,
        elapsed_ms: 1,
        elapsed_ns: 1_000_000,
        excluded_pid_zero: 1,
        initial_processes: 1,
        truncated: false,
        cancelled: false,
        timed_out: false,
        error: None,
        rows: vec![Row {
            identity: id(),
            before: IdentityCheck::Same,
            after: IdentityCheck::Same,
            flags: [Flag::Value(false); 3],
        }],
    }
}

#[test]
fn cpu_policy_codec_binds_actual_rows_counts_and_error_values() {
    let good = capture();
    let encoded = good.json();
    assert_eq!(decode_capture(&encoded), Some(good.clone()));
    for field in [
        "identity_before",
        "identity_after",
        "game_mode",
        "carplay",
        "runaway_mitigation",
        "start_s",
    ] {
        let mut bad = encoded.clone();
        let rows = bad.as_object_mut().unwrap().get_mut("rows").unwrap();
        let _ = rows.as_array_mut().unwrap()[0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(decode_capture(&bad).is_none(), "missing {field}");
    }
    let mut bad = encoded.clone();
    bad.as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()[0] = json!({});
    assert!(decode_capture(&bad).is_none());
    let mut bad = encoded.clone();
    bad.as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(encoded["rows"][0].clone());
    for key in [
        "checked_processes",
        "attempted_processes",
        "initial_processes",
    ] {
        bad.as_object_mut().unwrap().insert(key.into(), json!(2));
    }
    assert!(decode_capture(&bad).is_none(), "duplicate PID");
    let mut partial = good.clone();
    partial.rows[0].flags[0] = Flag::Error {
        value: -1,
        errno: 13,
    };
    partial.rows[0].after = IdentityCheck::Unread { errno: Some(5) };
    assert_eq!(decode_capture(&partial.json()), Some(partial.clone()));
    assert!(!partial.complete());
    let mut forged = partial.json();
    forged
        .as_object_mut()
        .unwrap()
        .insert("complete_initial_census".into(), json!(true));
    assert!(decode_capture(&forged).is_none());
    let mut forged = partial.json();
    forged
        .as_object_mut()
        .unwrap()
        .insert("getter_errors".into(), json!(0));
    assert!(decode_capture(&forged).is_none());
    for noncanonical in ["+100", "0100", "0"] {
        let mut bad = encoded.clone();
        bad.as_object_mut()
            .unwrap()
            .insert("collector_start_s".into(), json!(noncanonical));
        assert!(
            decode_capture(&bad).is_none(),
            "noncanonical {noncanonical}"
        );
    }
    let mut unprivileged = good;
    unprivileged.effective_uid = 501;
    assert!(!decode_capture(&unprivileged.json()).unwrap().complete());
}

#[test]
fn unexpected_and_denied_getters_never_become_off() {
    assert_eq!(
        flag(-1, 1),
        Flag::Error {
            value: -1,
            errno: 1
        }
    );
    assert_eq!(flag(-1, 0), Flag::Unexpected(-1));
    assert_eq!(flag(2, 0), Flag::Unexpected(2));
    assert_eq!(flag(0, 0), Flag::Value(false));
    assert_eq!(flag(1, 0), Flag::Value(true));
}

#[test]
fn pid_reuse_exit_and_unread_identity_are_distinct_and_keep_raw_getters() {
    let mut n = 0;
    let row = bundle(
        id(),
        |_| {
            n += 1;
            Ok((n == 1).then_some(id()))
        },
        |_, _| (-1, 13),
    );
    assert_eq!(row.after, IdentityCheck::Exited);
    assert!(row.flags.iter().all(|f| *f
        == Flag::Error {
            value: -1,
            errno: 13
        }));
    let mut n = 0;
    let row = bundle(
        id(),
        |_| {
            n += 1;
            Ok(Some(Identity {
                start_s: if n == 1 { 100 } else { 101 },
                ..id()
            }))
        },
        |_, _| (0, 0),
    );
    assert_eq!(row.after, IdentityCheck::Changed);
    let row = bundle(
        id(),
        |_| Err(std::io::Error::from_raw_os_error(13)),
        |_, _| panic!("unread identity cannot run getters"),
    );
    assert_eq!(row.before, IdentityCheck::Unread { errno: Some(13) });
    assert_eq!(row.flags, [Flag::NotRead; 3]);
}

#[test]
fn cpu_policy_census_never_passes_pid_zero_to_a_getter() {
    let make = |pid| crate::kproc::Record {
        pid,
        ppid: 0,
        pgid: 0,
        tpgid: 0,
        tty: None,
        uid: 0,
        comm: String::new(),
        start_s: 100,
        start_us: 0,
        zombie: false,
    };
    let mut records = vec![make(0), make(1), make(42)];
    assert_eq!(remove_pid_zero(&mut records), 1);
    let mut got = Vec::new();
    for r in records {
        let expected = identity(&r);
        bundle(
            expected,
            |_| Ok(Some(expected)),
            |_, pid| {
                got.push(pid);
                (0, 0)
            },
        );
    }
    assert_eq!(got, [1, 1, 1, 42, 42, 42]);
}

#[test]
fn nonroot_and_partial_censuses_cannot_claim_complete() {
    let mut capture = Capture {
        collector: id(),
        parent_pid: 1,
        effective_uid: 0,
        began_unix_ms: 1,
        ended_unix_ms: 2,
        elapsed_ms: 1,
        elapsed_ns: 1_000_000,
        excluded_pid_zero: 0,
        initial_processes: 1,
        truncated: false,
        cancelled: false,
        timed_out: false,
        error: None,
        rows: vec![Row {
            identity: id(),
            flags: [Flag::Value(false), Flag::Value(false), Flag::Value(false)],
            before: IdentityCheck::Same,
            after: IdentityCheck::Same,
        }],
    };
    assert!(capture.complete());
    capture.effective_uid = 501;
    assert!(!capture.complete());
    capture.effective_uid = 0;
    capture.truncated = true;
    assert!(!capture.complete());
    capture.truncated = false;
    capture.rows[0].flags[1] = Flag::Error {
        value: -1,
        errno: 1,
    };
    assert!(!capture.complete());
    capture.rows[0].flags[1] = Flag::Value(false);
    capture.cancelled = true;
    assert!(!capture.complete());
    capture.cancelled = false;
    capture.timed_out = true;
    assert!(!capture.complete());
    capture.timed_out = false;
    capture.elapsed_ns = COLLECT_FOR.as_nanos() as u64 + 1;
    assert!(!capture.complete());
}
