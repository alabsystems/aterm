// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn native_capture_needs_a_proven_empty_roster_not_a_missing_reading() {
    for (code, stdout, stderr, allowed) in [
        (0, "", "aterm-ctl: OK 0 (sessions: no results)\n", true),
        (1, "", "aterm-ctl: OK 0 (sessions: no results)\n", false),
        (0, "", "", false),
        (0, "", "aterm-ctl: OK 1\n", false),
        (0, "unread body", "aterm-ctl: OK 0\n", false),
    ] {
        let mut calls = Vec::new();
        let result = request_settled_image(|args| {
            calls.push(args.join(" "));
            match args {
                ["sessions"] => Reply {
                    code,
                    stdout: stdout.into(),
                    stderr: stderr.into(),
                },
                ["image", "frame.png"] => Reply {
                    stdout: "OK 794 462 /tmp/frame.png".into(),
                    ..Reply::default()
                },
                _ => panic!("unexpected request: {args:?}"),
            }
        });
        assert_eq!(result.is_ok(), allowed);
        assert_eq!(
            calls,
            if allowed {
                vec!["sessions", "image frame.png"]
            } else {
                vec!["sessions"]
            }
        );
    }
}

#[test]
fn captures_with_a_terminal_require_a_successful_idle_wait() {
    for code in [0, 1, 124, -1] {
        let mut captured = false;
        let result = request_settled_image(|args| match args {
            ["sessions"] => Reply {
                stdout: "OK 1\n0 s-aa - alive shell active=1 wfocus=1\n".into(),
                ..Reply::default()
            },
            ["@s-aa", "await", "idle", "500", "timeout=60000"] => Reply {
                code,
                stderr: "idle wait result".into(),
                ..Reply::default()
            },
            ["image", "frame.png"] => {
                captured = true;
                Reply::default()
            }
            _ => panic!("unexpected request: {args:?}"),
        });
        assert_eq!(result.is_ok(), code == 0, "idle client exit {code}");
        assert_eq!(captured, code == 0);
    }
}

#[test]
fn failed_control_verbs_stop_the_requested_sequence() {
    let verbs = ["first", "", "second", "third"].map(str::to_owned);
    for code in [0, 1, 124, -1] {
        let mut calls = Vec::new();
        let result = run_verbs(&verbs, |args| {
            calls.push(args.join(" "));
            Reply {
                code: if args == ["second"] { code } else { 0 },
                stdout: if code == 0 {
                    "OK".into()
                } else {
                    String::new()
                },
                stderr: if code == 0 {
                    String::new()
                } else {
                    "control failed".into()
                },
            }
        });
        if code == 0 {
            assert!(result.is_ok());
            assert_eq!(calls, ["first", "second", "third"]);
        } else {
            assert!(
                result.is_err(),
                "client exit {code} must fail the instance run"
            );
            assert_eq!(calls, ["first", "second"]);
        }
    }
}
