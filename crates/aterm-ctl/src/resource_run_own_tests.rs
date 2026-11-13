// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn non_utf8_command_arguments_remain_exact_os_strings() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![0xff, 0x80]);
    let mut argv = ["--session", "@self", "--job", "build", "--", "tool"]
        .map(OsString::from)
        .to_vec();
    argv.push(raw.clone());
    assert_eq!(parse(argv).unwrap().unwrap().command[1], raw);
}

#[test]
fn arguments_after_separator_are_preserved_including_empty_words() {
    let argv = [
        "--session",
        "@self",
        "--job",
        "work:tool",
        "--",
        "tool",
        "",
        "--ai",
        "private words",
    ]
    .map(OsString::from)
    .to_vec();
    let parsed = parse(argv).unwrap().unwrap();
    assert_eq!(
        parsed.command,
        ["tool", "", "--ai", "private words"].map(OsString::from)
    );
    assert!(parsed.ai.is_none());
}

#[cfg(unix)]
#[test]
fn original_store_expectation_requires_a_bounded_accounted_workload_and_distinct_fd() {
    use std::os::fd::AsRawFd as _;
    let directory = crate::test_scratch_dir("root-store-argument");
    let mut file = std::fs::File::open(directory.path()).unwrap();
    let mut lower = Vec::new();
    while file.as_raw_fd() < 6 {
        lower.push(file);
        file = std::fs::File::open(directory.path()).unwrap();
    }
    let fd = file.as_raw_fd().to_string();
    let prefix = [
        "--accounted",
        "--cpu-units",
        "1",
        "--memory-bytes",
        "1024",
        "--timeout-ms",
        "1000",
        "--expect-original-store-fd",
        &fd,
        "--job",
        "workload",
    ];
    let argv = |prefix: &[&str], command: &[&str]| {
        prefix
            .iter()
            .copied()
            .chain(["--"])
            .chain(command.iter().copied())
            .map(OsString::from)
            .collect()
    };
    let arguments: Vec<OsString> = argv(&prefix, &["targo", "--resource-workload-v1"]);
    let mut startup = vec![OsString::from("resource-run")];
    startup.extend(arguments.iter().cloned());
    // SAFETY: this test owns the source File for the complete capture.
    let original = unsafe { capture_original_root_store(&startup) }.unwrap();
    let valid = parse_with_original_store(arguments, original)
        .unwrap()
        .unwrap();
    assert!(valid.expected_store.is_some());
    for command in [
        vec!["targo", "--resource-metadata-v1"],
        vec!["targo", "--resource-resolver-v1"],
        vec!["targo", "--resource-workload-v1", "extra"],
    ] {
        assert!(parse(argv(&prefix, &command)).is_err());
    }
    let without_accounted = &prefix[1..];
    assert!(
        parse(argv(
            without_accounted,
            &["targo", "--resource-workload-v1"]
        ))
        .is_err()
    );
    let without_timeout = prefix[..5]
        .iter()
        .chain(prefix[7..].iter())
        .copied()
        .collect::<Vec<_>>();
    assert!(parse(argv(&without_timeout, &["targo", "--resource-workload-v1"])).is_err());
    let mut alias = prefix.to_vec();
    alias.extend(["--status-fd", &fd]);
    assert!(parse(argv(&alias, &["targo", "--resource-workload-v1"])).is_err());
    let opaque = parse(argv(
        &["--session", "@self", "--job", "plain"],
        &["tool", "--expect-original-store-fd", &fd],
    ))
    .unwrap()
    .unwrap();
    assert!(opaque.expected_store.is_none());
    assert_eq!(opaque.command[1], "--expect-original-store-fd");
}

#[test]
fn retained_admission_requires_both_positive_resources_and_defaults_self() {
    let args = [
        "--cpu-units",
        "4",
        "--memory-bytes",
        "1024",
        "--timeout-ms",
        "90",
        "--status-fd",
        "7",
        "--job",
        "solve",
        "--",
        "ay",
        "input",
    ];
    let parsed = parse(args.map(OsString::from).to_vec()).unwrap().unwrap();
    assert_eq!(parsed.session, "@self");
    let resources = parsed.resources.unwrap();
    assert_eq!(
        (
            resources.cpu_units,
            resources.memory_bytes,
            resources.timeout_ms,
            resources.status_fd
        ),
        (4, 1024, Some(90), Some(7))
    );
    for prefix in [
        vec!["--cpu-units", "4"],
        vec!["--memory-bytes", "1024"],
        vec!["--timeout-ms", "90"],
        vec!["--cpu-units", "0", "--memory-bytes", "1024"],
        vec!["--cpu-units", "4", "--memory-bytes", "-1"],
        vec![
            "--cpu-units",
            "4",
            "--memory-bytes",
            "1024",
            "--ai",
            "ignored",
        ],
        vec![
            "--cpu-units",
            "4",
            "--memory-bytes",
            "1024",
            "--status-fd",
            "1",
        ],
    ] {
        let mut args = prefix;
        args.extend(["--job", "solve", "--", "ay"]);
        assert!(parse(args.into_iter().map(OsString::from).collect()).is_err());
    }
}

#[test]
fn capability_query_accepts_no_execution_or_resource_arguments() {
    assert!(
        parse(vec!["--capabilities".into()])
            .unwrap()
            .unwrap()
            .capabilities
    );
    for args in [
        vec!["--capabilities", "--capabilities"],
        vec!["--capabilities", "--job", "job"],
        vec!["--capabilities", "--", "ay"],
        vec!["--capabilities", "--cpu-units", "2"],
    ] {
        assert!(parse(args.into_iter().map(OsString::from).collect()).is_err());
    }
}

#[test]
fn confirmation_requires_one_matching_identity_and_session() {
    assert!(registered(
        "OK resources version=1 root_pid=42 start=77 session=s-a",
        42,
        77,
        "s-a"
    ));
    for status in [
        "OK resources version=1 root_pid=43 start=77 session=s-a",
        "OK resources version=1 root_pid=42 start=78 session=s-a",
        "OK resources version=1 root_pid=42 start=77 session=s-b",
        "OK resources version=1 root_pid=42 root_pid=42 start=77 session=s-a",
    ] {
        assert!(!registered(status, 42, 77, "s-a"));
    }
}
