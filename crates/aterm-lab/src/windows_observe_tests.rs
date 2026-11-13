// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
const HEADER: &str = "snapshot version=1 start=100 end=200 rows=1 observer=42\n";
const ROW: &str =
    "process name_hex=6769742e657865 pid=42 birth=99 parent=1 kernel=0 user=- read=12 write=-\n";

#[test]
fn completed_failure_streams_are_private_exact_and_never_overwritten() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let stdout = b"invalid utf8:\xff\n";
    let stderr = b"private transport diagnostic\0\n";
    write_failure(
        dir.path(),
        "phase=identity-before ssh_exit=0 unexpected_stderr=true",
        Some((stdout, stderr)),
    )
    .unwrap();
    assert_eq!(
        aterm_uds::private_file::read_private(dir.path(), "stdout.bin", OUTPUT_CAP).unwrap(),
        stdout
    );
    assert_eq!(
        aterm_uds::private_file::read_private(dir.path(), "stderr.bin", OUTPUT_CAP).unwrap(),
        stderr
    );
    for name in ["stdout.bin", "stderr.bin", "metadata.txt"] {
        assert_eq!(
            std::fs::symlink_metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(write_failure(dir.path(), "replacement", Some((b"changed", b"changed"))).is_err());
    assert_eq!(
        std::fs::read(dir.path().join("stdout.bin")).unwrap(),
        stdout
    );
    let linked = aterm_tempfile::tempdir().unwrap();
    let outside = linked.path().join("outside");
    std::fs::write(&outside, b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, linked.path().join("stdout.bin")).unwrap();
    assert!(write_failure(linked.path(), "refused", Some((stdout, stderr))).is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"untouched");
}

#[test]
fn unavailable_output_has_only_metadata_and_oversized_streams_are_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    write_failure(
        dir.path(),
        "phase=snapshot io_kind=TimedOut stdout_bytes=unknown stderr_bytes=unknown",
        None,
    )
    .unwrap();
    assert!(!dir.path().join("stdout.bin").exists());
    assert!(!dir.path().join("stderr.bin").exists());
    let metadata = std::fs::read_to_string(dir.path().join("metadata.txt")).unwrap();
    assert!(metadata.contains("raw_streams=unavailable"));
    let oversized = aterm_tempfile::tempdir().unwrap();
    assert!(
        write_failure(
            oversized.path(),
            "refused",
            Some((&vec![0; OUTPUT_CAP + 1], b""))
        )
        .is_err()
    );
    assert!(!oversized.path().join("stdout.bin").exists());
}

#[test]
fn process_observation_preserves_unknown_counters_and_exact_identity() {
    let snapshot = parse(&format!("{HEADER}{ROW}")).unwrap();
    assert_eq!(
        snapshot.processes,
        vec![Process {
            name: "git.exe".into(),
            pid: 42,
            birth: Some(99),
            parent: Some(1),
            kernel: Some(0),
            user: None,
            read: Some(12),
            write: None
        }]
    );
    assert!(validate_args(&[]).is_ok());
    assert!(validate_args(&["exec".into()]).is_err());
    let missing =
        parse(&format!("{HEADER}{ROW}").replace("birth=99 parent=1", "birth=- parent=-")).unwrap();
    assert_eq!(missing.processes[0].birth, None);
    assert_eq!(missing.processes[0].parent, None);
}

#[test]
fn observation_refuses_commands_before_touching_the_held_guest() {
    let held = HeldVm {
        pid: 2,
        birth: aterm_uds::process::ProcessBirth {
            seconds: 1,
            microseconds: 0,
        },
        plan: "0123456789abcdef".into(),
    };
    assert!(run(&held, &["arbitrary-query".into()]).is_err());
    let ssh = Ssh {
        key: "existing-key".into(),
        known_hosts: "existing-hosts".into(),
        port: 2222,
        user: guest::RUNNER,
        connect_within: super::super::CONNECT_WITHIN,
    };
    let options = ssh.options_for(true);
    assert!(
        options
            .iter()
            .any(|option| option == "StrictHostKeyChecking=yes")
    );
    assert!(options.iter().any(|option| option == "UpdateHostKeys=no"));
    assert!(
        !options
            .iter()
            .any(|option| option == "StrictHostKeyChecking=accept-new")
    );
}

#[test]
fn process_observation_rejects_ambiguous_unbounded_or_incomplete_records() {
    for bad in [
        format!("{HEADER}{}", ROW.trim_end()),
        format!("{HEADER}{ROW}{ROW}"),
        format!("{}{ROW}{ROW}", HEADER.replace("rows=1", "rows=2")),
        format!("{HEADER}{ROW}").replace("pid=42", "pid=42 pid=43"),
        format!("{HEADER}{ROW}").replace("kernel=0", "secret=0"),
        format!("{HEADER}{ROW}").replace("birth=99", "birth=201"),
        format!("{HEADER}{ROW}").replace("read=12", "read=-1"),
        format!("{HEADER}{ROW}").replace("read=12", "read=18446744073709551616"),
        format!("{HEADER}{ROW}").replace("pid=42", "pid=042"),
        format!("{HEADER}{ROW}").replace("observer=42", "observer=43"),
        format!("{HEADER}{ROW}").replace("parent=1", "parent=4294967296"),
        format!("{HEADER}{ROW}").replace("name_hex=6769742e657865", "name_hex=2f"),
        format!("{HEADER}{ROW}").replace("name_hex=6769742e657865", "name_hex=ff"),
        format!("{HEADER}{ROW}").replace("rows=1", "rows=257"),
        format!("{HEADER}{ROW}").replace("end=200", "end=99"),
        format!("{HEADER}{ROW}").replace("version=1", "version=2"),
        format!("{HEADER}{ROW}").replace(
            "name_hex=6769742e657865",
            &format!("name_hex={}", "61".repeat(NAME_CAP + 1)),
        ),
    ] {
        assert!(
            parse(&bad).is_err(),
            "accepted malformed bounded process facts"
        );
    }
    let mut oversized =
        format!("snapshot version=1 start=100 end=200 rows={ROW_CAP} observer=42\n");
    for pid in 0..ROW_CAP {
        oversized.push_str(&format!(
            "process name_hex={} pid={pid} birth=- parent=- kernel=- user=- read=- write=-\n",
            "61".repeat(NAME_CAP)
        ));
    }
    assert!(oversized.len() > OUTPUT_CAP);
    assert!(parse(&oversized).is_err());
}
