// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Reply, terminal_request, terminal_selector};

#[test]
fn terminal_waits_name_the_registry_session_when_a_native_tab_is_front() {
    for wait in [
        &["ready"][..],
        &["await", "idle", "500", "timeout=60000"][..],
    ] {
        let mut calls = Vec::new();
        let reply = terminal_request(
            |args| {
                calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
                match args {
                    ["sessions"] => Reply {
                        stdout: "OK 2\n0 s-aa - exited previous active=0 wfocus=1\n7 s-bb - alive shell active=0 wfocus=1\n".into(),
                        ..Reply::default()
                    },
                    ["@s-bb", rest @ ..] if rest == wait => Reply::default(),
                    _ => Reply {
                        code: 1,
                        stderr: "ERR no active terminal".into(),
                        ..Reply::default()
                    },
                }
            },
            wait,
        );
        assert_eq!(reply.code, 0);
        assert_eq!(calls.len(), 2, "discovery and one selected request");
        assert_eq!(calls[1][0], "@s-bb");
        assert_eq!(&calls[1][1..], wait);
    }
}

#[test]
fn terminal_selection_tracks_the_focused_session_and_keeps_exited_commands_readable() {
    let roster = "OK 3\n0 s-aa - alive active=1 active=0 wfocus=1\n1 s-bb - spawning shell active=1 wfocus=1\n2 s-cc - alive shell active=1 wfocus=0\n";
    assert_eq!(terminal_selector(roster).as_deref(), Some("@s-bb"));
    assert_eq!(
        terminal_selector("0 s-aa - exited finished active=1 wfocus=1\n").as_deref(),
        Some("@s-aa")
    );
    assert_eq!(terminal_selector("OK 0\n"), None);
    assert_eq!(terminal_selector("0 native - native SystemState\n"), None);
}

#[test]
fn terminal_wait_errors_propagate_without_retrying_or_following_the_front() {
    let mut calls = 0;
    let reply = terminal_request(
        |args| {
            calls += 1;
            if args == ["sessions"] {
                Reply {
                    stdout: "0 s-aa - exited finished active=1 wfocus=1\n".into(),
                    ..Reply::default()
                }
            } else {
                assert_eq!(args, ["@s-aa", "ready"]);
                Reply {
                    code: 3,
                    stderr: "ERR selected session unavailable".into(),
                    ..Reply::default()
                }
            }
        },
        &["ready"],
    );
    assert_eq!(reply.code, 3);
    assert_eq!(calls, 2);
}
