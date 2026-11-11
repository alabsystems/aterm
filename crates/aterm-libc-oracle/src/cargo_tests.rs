// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ERROR: &str = r#"{"reason":"compiler-message","package_id":"x","message":{"rendered":"error[E0624]: method `si_status` is private\n","message":"method `si_status` is private","code":{"code":"E0624","explanation":"…"},"level":"error","spans":[{"file_name":"conformance/src/cell_mac.rs","label":"private method","text":[{"text":"const _: unsafe fn(&libc_shim::siginfo_t) -> libc_shim::c_int = libc_shim::siginfo_t::si_status;","highlight_start":1,"highlight_end":2}]}],"children":[]}}"#;

#[test]
fn errors_and_test_verdicts_are_read_off_the_json_lines() {
    let stdout = [
        ERROR,
        r#"{"reason":"compiler-message","message":{"message":"unused","level":"warning","spans":[]}}"#,
        r#"{"reason":"build-finished","success":false}"#,
        r#"{ "type": "test", "name": "cell_mac::runtime::behaviour_fd_zero", "event": "ok" }"#,
        r#"{ "type": "test", "name": "cell_mac::runtime::behaviour_fd_set", "event": "failed", "stdout": "FD_SET individual bit pattern differs at fd 0" }"#,
        "   Compiling libc v0.2.186",
        r#"{ "type": "suite", "event": "failed", "passed": 1, "failed": 1 }"#,
    ]
    .join("\n");
    let outcome = Outcome::parse(false, Some(101), &stdout, "");
    assert_eq!(outcome.errors.len(), 1, "warnings are not errors");
    assert!(outcome.has_error("E0624", "si_status"));
    assert!(!outcome.has_error("E0308", "si_status"));
    assert!(!outcome.has_error("E0624", "si_pid"));
    assert_eq!(outcome.passed, 1);
    assert!(outcome.test_failed("behaviour_fd_set"));
    assert!(!outcome.test_failed("behaviour_fd_zero"));
    assert_eq!(outcome.failed[0].leaf(), "behaviour_fd_set");
}

#[test]
fn a_name_matches_whole_identifiers_in_the_source_it_points_at() {
    let outcome = Outcome::parse(false, Some(101), ERROR, "");
    let error = &outcome.errors[0];
    assert!(error.names("si_status"), "in the message");
    assert!(error.names("siginfo_t"), "in the snippet");
    assert!(error.names("private"), "in the label");
    assert!(!error.names("si_stat"), "a prefix is not a name");
    assert!(!error.names(""), "empty string is not an identifier");
}

#[test]
fn a_full_disk_is_told_apart_from_a_failed_build() {
    let full = |stderr| Outcome::parse(false, Some(101), "", stderr).out_of_space();
    assert!(full(
        "error: failed to write `x.rlib`: No space left on device (os error 28)"
    ));
    assert!(full("ld: write failed: No space left on device"));
    assert!(!Outcome::parse(false, Some(101), ERROR, "").out_of_space());
}

/// cargo's own stderr for each case, captured from a copy of the oracle.
#[test]
fn a_locked_offline_refusal_is_classified_by_its_cause() {
    let failed = |stderr: &str| Outcome::parse(false, Some(101), "", stderr).locked_failure();
    assert_eq!(
        failed(
            "error: cannot update the lock file /x/libc-oracle/Cargo.lock because --locked \
             was passed to prevent this\nhelp: to generate the lock file without accessing \
             the network, remove the --locked flag and use --offline instead."
        ),
        Some(LockedFailure::StaleLock)
    );
    assert_eq!(
        failed(
            "error: no matching package named `libc` found\nlocation searched: crates.io \
             index\nnote: offline mode (via `--offline`) can sometimes cause surprising \
             resolution failures"
        ),
        Some(LockedFailure::NotDownloaded)
    );
    assert_eq!(
        failed(
            "error: failed to download `libc v0.2.186`\n\nCaused by:\n  attempting to make \
             an HTTP request, but --offline was specified"
        ),
        Some(LockedFailure::NotDownloaded)
    );
    assert_eq!(
        failed(
            "error: unclosed array, expected `]`\n  --> conformance/Cargo.toml:18:12\nerror: \
             failed to load manifest for workspace member `/x/libc-oracle/conformance`"
        ),
        Some(LockedFailure::Refused)
    );
    assert_eq!(Outcome::parse(true, Some(0), "", "").locked_failure(), None);
}
