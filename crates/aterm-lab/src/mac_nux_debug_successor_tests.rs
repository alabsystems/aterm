// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn required_query_completes_native_command_after_the_old_query_limit() {
    const ACTOR: &str = "ATERM_NUX_REQUIRED_QUERY_ACTOR";
    if std::env::var_os(ACTOR).is_some() {
        use std::io::Write as _;
        println!("required-query-executed");
        std::io::stdout().flush().unwrap();
        return;
    }
    let started = Instant::now() - Duration::from_secs(6);
    let out = required_query(started, started + WAIT, |left| {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "mac_nux::debug_successor::tests::required_query_completes_native_command_after_the_old_query_limit",
                "--exact",
                "--nocapture",
            ])
            .env(ACTOR, "1");
        aterm_uds::deadline::output_within(&mut command, left)
            .map_err(|error| Fail::die(format!("{error:?}")))
    })
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    // Not a whole line: one test thread (`RUST_TEST_THREADS=1`, as verify runs tests) has
    // libtest print `test <name> ... ` before the test body runs, so the marker ends that line.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("required-query-executed"), "{stdout}");
}

#[test]
fn expired_required_query_does_not_start_another_command() {
    let started = Instant::now() - Duration::from_secs(61);
    assert!(
        required_query(started, started + WAIT, |_| -> Result<(), Fail> {
            panic!("expired query ran");
        })
        .is_err()
    );
    assert!(
        required_query(Instant::now(), started + WAIT, |_| -> Result<(), Fail> {
            panic!("query restarted the enclosing wait");
        })
        .is_err()
    );
}

#[test]
fn only_a_new_launched_gui_with_the_same_live_shell_qualifies() {
    let before = Identity::parse_shaped("10@100:1", true).unwrap();
    let after = Identity::parse_shaped("11@101:1", true).unwrap();
    let shell = "20:unexported-state\n";
    let stamp = "unexported-state";
    assert!(continuity(before, after, shell, shell, stamp, LAUNCHED_ROUTE).is_ok());
    assert!(continuity(before, before, shell, shell, stamp, LAUNCHED_ROUTE).is_err());
    assert!(
        continuity(
            before,
            after,
            shell,
            "21:new-shell\n",
            stamp,
            LAUNCHED_ROUTE
        )
        .is_err()
    );
    assert!(continuity(before, after, shell, shell, stamp, "forked successor").is_err());
    assert!(continuity(before, after, "20:\n", "20:\n", stamp, LAUNCHED_ROUTE).is_err());
}

#[test]
fn the_successor_must_report_the_same_complete_build_identity() {
    let before = "OK version=1.2.0 build=123 commit=abc profile=debug";
    assert!(same_image(before, before));
    assert!(!same_image(before, "OK version=1.2.0 build=124 commit=abc"));
    assert!(!same_image(before, "OK version=1.2.0 build=123 commit=def"));
    assert!(!same_image("OK version=1.2.0", "OK version=1.2.0"));
    assert!(!same_image(
        "OK version= build= commit=",
        "OK version= build= commit="
    ));
}
