// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The candidate-gate CALL: the argv the group asks the gate with, and what each exit
//! means. What the gate admits is the gate's own (aterm-release `ship trust candidate`);
//! the group's handling of each answer is in `group.rs`.

use std::path::Path;

use atpkg_publish::channel::candidate::{GateAnswer, PROGRAM, answer, command};

#[test]
fn the_gate_is_asked_about_the_seal_against_the_published_pin() {
    let argv: Vec<String> = command(Path::new("/seals/stage-trust-wp"), Some(9192))
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(PROGRAM, "targo");
    assert_eq!(
        argv,
        [
            "--unverified",
            "ship",
            "trust",
            "candidate",
            "--admits",
            "/seals/stage-trust-wp",
            "--base-build",
            "9192"
        ]
    );
    let unread: Vec<String> = command(Path::new("/s"), None)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        !unread.contains(&"--base-build".to_string()),
        "no pin read: the gate reads the channel itself"
    );
}

#[test]
fn exit_0_admits_1_refuses_line_by_line_and_anything_else_could_not_check() {
    assert_eq!(
        answer(Some(0), "PASS: RECEIPT under candidate-gate/x\n", ""),
        GateAnswer::Admitted("PASS: RECEIPT under candidate-gate/x".into())
    );
    assert_eq!(
        answer(
            Some(1),
            "no candidate-gate receipt names this seal\n\nstale receipt\n",
            ""
        ),
        GateAnswer::Refused(vec![
            "no candidate-gate receipt names this seal".into(),
            "stale receipt".into()
        ])
    );
    assert_eq!(
        answer(Some(1), "", "refused on stderr\n"),
        GateAnswer::Refused(vec!["refused on stderr".into()])
    );
    for code in [Some(2), Some(101), None] {
        assert!(
            matches!(
                answer(code, "", "error: no such command `ship`"),
                GateAnswer::CannotCheck(_)
            ),
            "{code:?}"
        );
    }
}

/// The gate's own reason is on STDOUT; stderr is targo's — the lane warnings, the build of
/// the cutter. A could-not-check names the gate's words, never a warning.
#[test]
fn could_not_check_reads_the_gates_reason_never_targos_chatter() {
    let stderr = "warning: nested explicit-unverified Targo propagation is unavailable on this \
                  platform; the outer explicit command remains authorized\n\
                  warning: UNVERIFIED: `targo run` was explicitly authorized\n   \
                  Compiling aterm-release v0.104.0\n    \
                  Finished `release` profile [optimized] target(s) in 61.2s\n     \
                  Running `target/release/aterm-release ship trust candidate --admits /s`\n";
    let stdout = "trust candidate: COULD NOT CHECK — cannot read the published trust build from \
                  the channel\n";
    match answer(Some(2), stdout, stderr) {
        GateAnswer::CannotCheck(why) => {
            assert!(
                why.contains(
                    "(exit 2): trust candidate: COULD NOT CHECK — cannot read the published \
                     trust build"
                ),
                "{why}"
            );
            assert!(!why.contains("warning"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    // Nothing on stdout: the first stderr line that is not chatter.
    match answer(
        Some(101),
        "",
        "warning: x\n   Compiling y\nerror: no such command: `ship`\n",
    ) {
        GateAnswer::CannotCheck(why) => {
            assert!(why.ends_with("error: no such command: `ship`"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    match answer(None, "", "warning: x\n   Compiling y\n") {
        GateAnswer::CannotCheck(why) => {
            assert!(why.contains("by signal"), "{why}");
            assert!(why.contains("said nothing but build output"), "{why}");
        }
        other => panic!("{other:?}"),
    }
}
