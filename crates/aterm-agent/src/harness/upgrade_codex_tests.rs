// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::upgrade::QUIET_S;

fn v(s: &str) -> Version {
    Version::parse(s).expect("a version")
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn rows(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_string()).collect()
}

const T1: &str = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
const T2: &str = "01a0dc3a-5c71-7143-bcee-58def0a15dfd";

/// The atpkg store's codex 0.157.1 manifest, verbatim (2026-09-25).
const PACKAGE_0_157_1: &str = r#"{
  "layoutVersion": 1,
  "version": "0.157.1",
  "target": "aarch64-apple-darwin",
  "variant": "codex",
  "entrypoint": "bin/codex",
  "resourcesDir": "codex-resources",
  "pathDir": "codex-path"
}"#;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aterm-codex-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

#[test]
fn the_measured_manifest_parses_and_a_changed_layout_refuses() {
    let (version, entry) = parse_package(PACKAGE_0_157_1).expect("the store's manifest");
    assert_eq!(version, v("0.157.1"));
    assert_eq!(entry, PathBuf::from("bin/codex"));
    for bad in [
        PACKAGE_0_157_1.replace("\"layoutVersion\": 1", "\"layoutVersion\": 2"),
        PACKAGE_0_157_1.replace("0.157.1", "0.157.1-alpha"),
        PACKAGE_0_157_1.replace("bin/codex", "/abs/codex"),
        PACKAGE_0_157_1.replace("bin/codex", "../codex"),
        "not json".to_string(),
    ] {
        assert_eq!(parse_package(&bad), None, "{bad}");
    }
}

#[test]
fn a_version_is_read_from_the_package_that_names_the_binary_never_by_running_it() {
    let d = scratch("pkg");
    let root = d.join("store/codex/1000000000157000001");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    // A binary that would HANG if it were run: the reader must never exec it.
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "#!/bin/sh\nsleep 600\n").expect("exe");
    std::fs::write(root.join(PACKAGE_JSON), PACKAGE_0_157_1).expect("manifest");
    let started = std::time::Instant::now();
    assert_eq!(package_version(&exe), Some(v("0.157.1")));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    // NEGATIVE CONTROL: a manifest whose entrypoint names ANOTHER binary does
    // not answer for this one (a stray `codex-package.json` in a parent).
    let other = d.join("elsewhere/bin");
    std::fs::create_dir_all(&other).expect("other");
    std::fs::write(other.join("codex"), "x").expect("exe");
    std::fs::write(
        d.join("elsewhere").join(PACKAGE_JSON),
        PACKAGE_0_157_1.replace("bin/codex", "bin/not-codex"),
    )
    .expect("manifest");
    assert_eq!(package_version(&other.join("codex")), None);
    // `codex --version`'s own words are no version (its first word).
    assert_eq!(Version::parse("codex-cli"), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_rewrite_resumes_the_thread_with_every_kept_flag() {
    let a = argv(&[
        "/store/codex/bin/codex",
        "--no-daemon",
        "-m",
        "gpt-6-astra",
        "-c",
        "model_reasoning_effort=\"high\"",
        "--config=sandbox_mode=\"read-only\"",
        "-mgpt-x",
        "--add-dir",
        "-",
        "fix the bug",
    ]);
    assert_eq!(
        rewrite_argv(&a, Some(T1)).expect("carried"),
        argv(&[
            "resume",
            "--no-daemon",
            "-m",
            "gpt-6-astra",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--config=sandbox_mode=\"read-only\"",
            "-mgpt-x",
            "--add-dir",
            "-",
            T1,
        ])
    );
    // A session that was itself a resume, or a fork: the old id, `--last`,
    // `--all` and the first prompt go; the thread the exit names comes back.
    for a in [
        argv(&["codex", "resume", T2, "--no-daemon"]),
        argv(&["codex", "resume", "--last", "--no-daemon"]),
        argv(&["codex", "--no-daemon", "fork", T2, "carry on"]),
        argv(&[
            "codex",
            "--no-daemon",
            "-i",
            "a.png",
            "b.png",
            "look at these",
        ]),
    ] {
        assert_eq!(
            rewrite_argv(&a, Some(T1)).expect("carried"),
            argv(&["resume", "--no-daemon", T1]),
            "{a:?}"
        );
    }
    // No thread (a session with no conversation yet): the flags alone.
    assert_eq!(
        rewrite_argv(&argv(&["codex", "-s", "read-only", "hi"]), None).expect("carried"),
        argv(&["-s", "read-only"])
    );
    // `--` ends the options: what follows is the prompt, dropped.
    assert_eq!(
        rewrite_argv(&argv(&["codex", "--search", "--", "--no-daemon"]), Some(T1))
            .expect("carried"),
        argv(&["resume", "--search", T1])
    );
}

#[test]
fn an_unknown_or_unresumable_launch_is_refused_not_guessed() {
    for (a, why) in [
        (
            argv(&["codex", "--full-auto"]),
            ArgvRefusal::UnknownFlag("--full-auto".into()),
        ),
        (
            argv(&["codex", "-x"]),
            ArgvRefusal::UnknownFlag("-x".into()),
        ),
        (
            argv(&["codex", "--no-daemon=1"]),
            ArgvRefusal::UnknownFlag("--no-daemon=1".into()),
        ),
        (
            argv(&["codex", "exec", "hi"]),
            ArgvRefusal::NotResumable("exec".into()),
        ),
        (
            argv(&["codex", "--remote", "ws://h:1"]),
            ArgvRefusal::NotResumable("--remote".into()),
        ),
        (
            argv(&["codex", "--worktree"]),
            ArgvRefusal::NotResumable("--worktree".into()),
        ),
        (
            argv(&["codex", "app-server", "--listen", "unix://"]),
            ArgvRefusal::NotResumable("app-server".into()),
        ),
    ] {
        assert_eq!(rewrite_argv(&a, Some(T1)), Err(why), "{a:?}");
    }
}

#[test]
fn only_an_interactive_tui_is_a_tui_never_the_daemon_or_its_updater() {
    for tui in [
        argv(&["/store/bin/codex"]),
        argv(&["codex", "--no-daemon", "-m", "x"]),
        argv(&["codex", "resume", T1]),
        argv(&["codex", "-c", "exec=1", "fork"]),
        argv(&["codex", "fix the bug"]),
    ] {
        assert!(is_tui_argv(&tui), "{tui:?}");
    }
    // Measured: every process of the daemon is named `codex` too.
    for not in [
        argv(&[
            "/c/packages/app-server-daemon/releases/0.157.0-aarch64-apple-darwin/bin/codex",
            "app-server",
            "--listen",
            "unix://",
            "--managed-daemon",
        ]),
        argv(&["codex", "app-server", "daemon", "pid-update-loop"]),
        argv(&["codex", "exec", "hi"]),
        argv(&["codex", "-m", "x", "agents"]),
    ] {
        assert!(!is_tui_argv(&not), "{not:?}");
    }
}

#[test]
fn a_thread_id_is_a_hyphenated_uuid_and_nothing_else() {
    assert!(is_thread_id(T1));
    for bad in [
        "",
        "01a0dc36",
        "01a0dc36-1dc1-7ee2-be91-11bc323e377",
        "01a0dc36-1dc1-7ee2-be91-11bc323e377cX",
        "01a0dc36-1dc1-7ee2-be91-11bc323e377g",
        "01a0dc36 1dc1-7ee2-be91-11bc323e377c",
        "; rm -rf /",
    ] {
        assert!(!is_thread_id(bad), "{bad:?}");
    }
    assert_eq!(lock_thread(&format!("{T1}.lock")), Some(T1));
    assert_eq!(lock_thread(".coordination.lock"), None);
    assert!(is_rollout_of(
        &format!("rollout-2026-09-25T22-35-29-{T1}.jsonl"),
        T1
    ));
    assert!(!is_rollout_of(
        &format!("rollout-2026-09-25T22-35-29-{T1}.jsonl.zst"),
        T1
    ));
    assert!(!is_rollout_of(
        &format!("rollout-2026-09-25T22-35-29-{T2}.jsonl"),
        T1
    ));
}

/// The tab after a daemon-mode `/exit`, measured 2026-09-25 (0.157.0): the
/// launch's command line, the hint, then the prompt with the cursor.
fn daemon_exit_screen() -> Vec<String> {
    rows(&[
        "% \"$OLDCODEX\"",
        "Installing daemon from CLI version 0.157.0 into /tmp/scratch-01/cxl/home/.codex/packages/app-server-daemon...",
        "Disconnected from this task. Any running work continues.",
        &format!("Reconnect: codex resume {T1}"),
        "Stop the current turn: run codex agents, select this task, and press x.",
        "% ",
    ])
}

/// The same tab after a SECOND exit, of an embedded session (measured): the
/// first exit's hint is still on the screen above it.
fn embedded_exit_screen() -> Vec<String> {
    rows(&[
        &format!("Reconnect: codex resume {T1}"),
        "Stop the current turn: run codex agents, select this task, and press x.",
        "% \"$OLDCODEX\" --no-daemon -m gpt-6-astra",
        "To continue this session, run:",
        &format!("  codex resume {T2}"),
        "% ",
    ])
}

#[test]
fn the_exit_hint_is_the_tuis_own_last_words() {
    assert_eq!(
        parse_exit_hint(&daemon_exit_screen()),
        Some(ExitHint {
            thread: T1.into(),
            daemon: true
        })
    );
    // The LAST hint, not the first: the older exit's is above it.
    assert_eq!(
        parse_exit_hint(&embedded_exit_screen()),
        Some(ExitHint {
            thread: T2.into(),
            daemon: false
        })
    );
    // Above the prompt (no shell integration): the same answers.
    let d = daemon_exit_screen();
    assert_eq!(hint_above_prompt(&d, 5).map(|h| h.thread), Some(T1.into()));
    let e = embedded_exit_screen();
    assert_eq!(hint_above_prompt(&e, 5).map(|h| h.thread), Some(T2.into()));
    // A hint wrapped in a narrow pane is read joined.
    let (head, tail) = T1.split_at(20);
    let wrapped = rows(&[
        &format!("Reconnect: codex resume {head}"),
        tail,
        "Stop the current turn: run codex agents,",
    ]);
    assert_eq!(parse_exit_hint(&wrapped).map(|h| h.thread), Some(T1.into()));
    // An embedded id without its heading is not a hint: `codex resume <id>`
    // is also what a PERSON types at a prompt.
    assert_eq!(
        parse_exit_hint(&rows(&[&format!("% codex resume {T2}")])),
        None
    );
}

#[test]
fn a_tui_that_printed_nothing_is_never_relaunched_into_an_older_exits_thread() {
    // NEGATIVE CONTROL for the whole reason the hint is read from the TUI's
    // own rows: the second TUI died without a word, and the first exit's hint
    // (another conversation) is still two rows above the new prompt.
    let screen = rows(&[
        &format!("Reconnect: codex resume {T1}"),
        "Stop the current turn: run codex agents, select this task, and press x.",
        "% codex --no-daemon",
        "% ",
    ]);
    assert!(
        parse_exit_hint(&screen).is_some(),
        "the whole screen misleads"
    );
    assert_eq!(hint_above_prompt(&screen, 3), None);
    // A two-row prompt with no integration: nothing is claimed either.
    let two_row = rows(&[
        "To continue this session, run:",
        &format!("  codex resume {T2}"),
        "~/work (main)",
        "% ",
    ]);
    assert_eq!(hint_above_prompt(&two_row, 3), None);
}

#[test]
fn the_hint_is_read_above_the_prompts_first_row_past_the_tuis_own_tally() {
    // Measured 2026-09-26 (0.157.0, an inline TUI, a two-row prompt `%~` /
    // `%#`): the daemon-mode hint, its stop line and the token tally the TUI
    // prints once a turn ran, then the prompt's two rows, the cursor on the
    // second.
    let screen = rows(&[
        "  line 45 of a long answer",
        "Disconnected from this task. Any running work continues.",
        &format!("Reconnect: codex resume {T1}"),
        "Stop the current turn: run codex agents, select this task, and press x.",
        "Token usage so far: total=2 input=1 output=1",
        "/tmp/fxl.VyGP/work",
        "%",
    ]);
    // Bounded by the CURSOR row, the prompt's first row stands between the
    // hint and it: the review's failure (exited, never relaunched).
    assert_eq!(hint_above_prompt(&screen, 6), None);
    // Bounded by the prompt's FIRST row (the entering block's `cmd -
    // prompt` = 1 row above the cursor): the TUI's own words.
    assert_eq!(
        hint_above_prompt(&screen, 5),
        Some(ExitHint {
            thread: T1.into(),
            daemon: true
        })
    );
    // The embedded form prints its tally ABOVE the hint (measured).
    let embedded = rows(&[
        "% codex --no-daemon",
        "Token usage: total=4 input=2 output=2",
        "To continue this session, run:",
        &format!("  codex resume {T2}"),
        "%",
    ]);
    assert_eq!(
        hint_above_prompt(&embedded, 4).map(|h| h.thread),
        Some(T2.into())
    );
    // NEGATIVE CONTROL: the tally is the TUI's own line; the command line
    // that ran it is not, and an older exit's hint above it stays unread.
    let nothing = rows(&[
        &format!("Reconnect: codex resume {T1}"),
        "Token usage so far: total=2 input=1 output=1",
        "% codex",
        "%",
    ]);
    assert_eq!(hint_above_prompt(&nothing, 3), None);
}

#[test]
fn a_background_terminal_is_a_session_leader_under_codex_and_a_helper_is_not() {
    // Measured 2026-09-26 (0.157.0): the daemon 6854 leads its own session;
    // its MCP servers 6872/7092 are in that session; the unified-exec
    // `sleep 7771` (7095) leads one of its own. Under an embedded TUI (7180,
    // in the tab shell's session 7173) the same: `sleep 7772` leads its own,
    // the MCP server 7201 does not.
    let table = vec![
        (6854, 1, "codex".to_string()),
        (6872, 6854, "Python".to_string()),
        (7092, 6854, "Python".to_string()),
        (7095, 6854, "sleep".to_string()),
        (7180, 7173, "codex".to_string()),
        (7201, 7180, "Python".to_string()),
        (7203, 7180, "sleep".to_string()),
        (7300, 7203, "node".to_string()),
    ];
    let sid = |p: u32| {
        Some(match p {
            6854 | 6872 | 7092 => 6854,
            7095 => 7095,
            7180 | 7201 => 7173,
            7203 | 7300 => 7203,
            _ => return None,
        })
    };
    assert_eq!(
        terminals_under(6854, &table, sid),
        vec![(7095, "sleep".to_string())]
    );
    // The terminal's own children are in ITS session: one terminal, once.
    assert_eq!(
        terminals_under(7180, &table, sid),
        vec![(7203, "sleep".to_string())]
    );
    // An idle daemon with its helpers only: nothing runs.
    let idle: Vec<_> = table
        .iter()
        .filter(|(p, _, _)| *p != 7095)
        .cloned()
        .collect();
    assert!(terminals_under(6854, &idle, sid).is_empty());
    // A session that cannot be read is counted: an unreadable kernel waits.
    assert_eq!(terminals_under(6854, &idle, |_| None).len(), 2);
}

/// `lsof -a -U -c codex -F pcdn`, measured 2026-09-26 (0.157.0): the TUI 6847
/// (two socketpairs of its own, and fd 35 naming the daemon's listener), the
/// daemon 6854 (its socketpair 7/8, its listener 10 and one accepted
/// connection 29, both named by the socket's path) and the vendor's updater
/// 6864, attached to its own socket.
const LSOF_CODEX_UNIX: &[&str] = &[
    "p6847",
    "ccodex",
    "f7",
    "d0x60e0d75ad1a06e88",
    "n->0x8ebb89a8aa3f3b06",
    "f8",
    "d0x8ebb89a8aa3f3b06",
    "n->0x60e0d75ad1a06e88",
    "f35",
    "d0x6cedd66151437fed",
    "n->0x3dfc2dbbc8e7b7f",
    "p6854",
    "ccodex",
    "f7",
    "d0xcb98fa9659ddadaf",
    "n->0x770005c2a3826c0",
    "f8",
    "d0x770005c2a3826c0",
    "n->0xcb98fa9659ddadaf",
    "f10",
    "d0x3dfc2dbbc8e7b7f",
    "n/private/tmp/codex-daemon-501/6ebd423ea54dcb2de40c8600a66104af",
    "f29",
    "d0xbcb18f3e343b08df",
    "n/private/tmp/codex-daemon-501/6ebd423ea54dcb2de40c8600a66104af",
    "p6864",
    "ccodex",
    "f5",
    "d0x1111",
    "n->0x2222",
];

#[test]
fn the_daemons_clients_are_its_sockets_peers() {
    let lsof = LSOF_CODEX_UNIX.join("\n");
    assert_eq!(daemon_clients_in(&lsof, 6854), Some(vec![6847]));
    // The vendor's updater is no client; a daemon with none attached reads
    // an empty list; one not in the listing reads NOTHING — never "no
    // clients".
    let alone = LSOF_CODEX_UNIX[11..].join("\n");
    assert_eq!(daemon_clients_in(&alone, 6854), Some(vec![]));
    assert_eq!(daemon_clients_in(&lsof, 4242), None);
    assert_eq!(daemon_clients_in("", 6854), None);
}

#[test]
fn codexs_status_line_says_when_a_background_terminal_runs() {
    let screen = rows(&[
        "› start bg 7771 please",
        "• The server runs in the background.",
        "  1:41 AM",
        "  1 background terminal running · /ps to view · /stop to close",
        "› Ask Codex to do anything",
        "  fake-model default · /private/tmp/fxl.VyGP/work",
    ]);
    assert!(terminals_on_screen(&screen));
    assert!(terminals_on_screen(&rows(&[
        "  2 background terminals running · /ps to view"
    ])));
    // A person's message saying the words is no status line.
    assert!(!terminals_on_screen(&rows(&[
        "› 1 background terminal running is fine"
    ])));
    assert!(!terminals_on_screen(&rows(&[
        "› say hello",
        "• ok",
        "› Ask Codex to do anything",
        "  fake-model default · /w",
    ])));
}

/// Measured rollout lines (0.157.0, 2026-09-25): a turn started, the user's
/// message, the turn aborted by Esc.
const STARTED: &str = r#"{"timestamp":"2026-09-26T05:37:10.315Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","root_turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","started_at":1790401030,"model_context_window":258400,"collaboration_mode_kind":"default"}}"#;
const USER: &str = r#"{"timestamp":"2026-09-26T05:37:10.338Z","ordinal":8,"type":"response_item","payload":{"type":"message","id":"msg_01a0dc37-a8c2-78e0-a4a8-fc661bfaae1b","role":"user","content":[{"type":"input_text","text":"say hello"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","create_time":1790401030.338192,"content_item_kinds":["user.text"]}},"metadata":{"client_authored":false,"user_input_order":0,"mcp_attribution":{"status":"none"}}}"#;
const ABORTED: &str = r#"{"timestamp":"2026-09-26T05:37:50.848Z","ordinal":11,"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","reason":"interrupted","started_at":1790401030,"completed_at":1790401070,"duration_ms":40536}}"#;
/// 0.145's end of a turn (the research's shape: `last_agent_message`, `turn_id`).
const COMPLETE_0_145: &str = r#"{"timestamp":"2026-07-28T10:00:00.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"1","last_agent_message":"done"}}"#;

fn assistant(text: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-09-26T05:38:00.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":{}}}]}}}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

fn user(text: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-09-26T05:38:00.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":{}}}]}}}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

#[test]
fn a_turn_is_idle_only_when_its_last_event_ended_it() {
    assert_eq!(rollout_turn(""), TurnState::Unknown);
    assert_eq!(rollout_turn(STARTED), TurnState::Busy);
    assert_eq!(rollout_turn(&[STARTED, USER].join("\n")), TurnState::Busy);
    assert_eq!(
        rollout_turn(&[STARTED, USER, ABORTED].join("\n")),
        TurnState::Idle
    );
    assert_eq!(
        rollout_turn(&[STARTED, COMPLETE_0_145].join("\n")),
        TurnState::Idle
    );
    // A new turn after the last one ended is busy again, and a cut first line
    // (the tail's) or a half-written last one is skipped.
    assert_eq!(
        rollout_turn(&[&ABORTED[40..], ABORTED, STARTED, "{\"type\":\"ev"].join("\n")),
        TurnState::Busy
    );
    // A tail with no turn event in it is no idle.
    assert_eq!(rollout_turn(USER), TurnState::Unknown);
}

#[test]
fn ready_is_the_agents_last_word_after_the_latest_notice() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let notice = prepare_prompt(&v("0.157.0"), &v("0.157.1"), marker);
    // The notice itself carries the marker, and is not the answer.
    assert!(!rollout_has_ready(&user(&notice), marker));
    let answered = [user(&notice), assistant(&format!("Done.\n`{marker}`"))].join("\n");
    assert!(rollout_has_ready(&answered, marker));
    // A marker merely QUOTED in a sentence is no answer.
    let quoted = [
        user(&notice),
        assistant(&format!("I will reply {marker} once the build ends")),
    ]
    .join("\n");
    assert!(!rollout_has_ready(&quoted, marker));
    // Moved past: the agent answered again and went back to work.
    let moved = [answered.clone(), user("keep going"), assistant("On it.")].join("\n");
    assert!(!rollout_has_ready(&moved, marker));
    // A NEW notice ends the old answer until the agent answers it.
    let asked_again = [answered.clone(), user(&notice)].join("\n");
    assert!(!rollout_has_ready(&asked_again, marker));
    // The other two places Codex writes the agent's words.
    let agent_message = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{marker}"}}}}"#
    );
    assert!(rollout_has_ready(
        &[user(&notice), agent_message].join("\n"),
        marker
    ));
    let complete = format!(
        r#"{{"type":"event_msg","payload":{{"type":"task_complete","last_agent_message":"{marker}"}}}}"#
    );
    assert!(rollout_has_ready(
        &[user(&notice), complete].join("\n"),
        marker
    ));
    // Codex's own environment row is no user message and resets nothing.
    let env = user("<environment_context>\n  <cwd>/w</cwd>\n</environment_context>");
    assert!(rollout_has_ready(&[answered, env].join("\n"), marker));
}

/// The idle 0.157.0 screen, measured (the rows under the card, 40 rows cut).
fn idle_rows() -> Vec<String> {
    rows(&[
        "╭─────────────────────────────────────────────╮",
        "│ >_ OpenAI Codex (v0.157.0)                  │",
        "╰─────────────────────────────────────────────╯",
        "",
        "                                               Tip: Type / to open the command popup; Tab autocompletes slash commands.",
        "",
        "› Ask Codex to do anything",
        "",
        "  GPT-6-Astra default · /private/tmp/scratch-01/cxl/work",
        "  ← for agents · ? for shortcuts",
    ])
}

/// A turn running (measured: the status row above the composer, which keeps
/// its placeholder).
fn busy_rows() -> Vec<String> {
    rows(&[
        "› say hello",
        "",
        "• Reconnecting... waiting for network (22s • esc to interrupt)",
        "  └ Connection failed: error sending request",
        "",
        "",
        "› Ask Codex to do anything",
        "",
        "  GPT-6-Astra default · /private/tmp/scratch-01/cxl/work · ⠙",
        "  ← for agents · ? for shortcuts                                                               ⚠ 1 warning · f2 to view",
    ])
}

#[test]
fn the_composer_is_read_by_codexs_caret_and_its_dim_placeholder() {
    let idle = idle_rows();
    assert_eq!(composer_row(&idle), Some(6));
    assert_eq!(
        composer_draft(&idle),
        Some((6, vec!["Ask Codex to do anything".to_string()]))
    );
    // The placeholder: text at the caret, the cursor at column 2, drawn dim.
    assert!(composer_is_empty(&idle, Some((6, 2)), true));
    // NEGATIVE CONTROL: the same row NOT dim is a draft whose caret was moved
    // home (measured: `› my draft here`, Home, cell `m … none`).
    assert!(!composer_is_empty(&idle, Some((6, 2)), false));
    let draft = rows(&[
        "› my draft here",
        "",
        "  GPT-6-Astra default · /private/tmp/scratch-01/cxl/work",
    ]);
    assert!(!composer_is_empty(&draft, Some((0, 15)), false));
    assert!(!composer_is_empty(&draft, Some((0, 2)), false));
    // A second draft row is typed text whatever the cursor says.
    let two = rows(&[
        "› Ask Codex to do anything",
        "  and a second line",
        "",
        "  GPT-6-Astra default · /w",
    ]);
    assert!(!composer_is_empty(&two, Some((0, 2)), true));
    // Claude's caret is no Codex composer, and a shell prompt is none.
    assert_eq!(composer_row(&rows(&["❯ ", "% "])), None);
    // A Codex transcript left in the scrollback above a shell prompt (an
    // inline session that exited) is no composer either.
    assert_eq!(composer_row(&rows(&["› say hello", "• hi", "% "])), None);
    // The busy screen keeps its placeholder, and says it is busy.
    let busy = busy_rows();
    assert_eq!(composer_row(&busy), Some(6));
    assert!(busy_on_screen(&busy));
    assert!(!busy_on_screen(&idle));
}

#[test]
fn a_box_replaces_the_composer_and_is_read_as_one() {
    // The folder-trust gate (0.157.0, the research's measurement).
    let gate = rows(&[
        "  Folder access",
        "",
        "› 1. Trust and continue",
        "  2. Back to Agent Command Center",
        "",
        "  enter continue · esc back",
    ]);
    assert!(box_on_screen(&gate));
    assert_eq!(composer_row(&gate), None);
    assert!(!composer_is_empty(&gate, Some((2, 2)), true));
    assert!(!box_on_screen(&idle_rows()));
}

fn facts(status: &str) -> Facts {
    Facts {
        status: status.to_string(),
        status_age_s: QUIET_S,
        composer_empty: true,
        quiet_s: QUIET_S,
        ..Facts::default()
    }
}

#[test]
fn a_daemon_client_exits_at_an_idle_point_once_its_daemon_went_first() {
    let idle = facts("idle");
    assert_eq!(
        next_step(&Mode::Daemon, &Phase::Pending, &idle, false, false, 0),
        Step::Terminate
    );
    assert_eq!(
        next_step(&Mode::Daemon, &Phase::Pending, &idle, false, true, 0),
        Step::Wait("daemon-first")
    );
    // Every Claude gate stands: a draft, a box, busy, a hold, a person.
    for (f, why) in [
        (
            Facts {
                composer_empty: false,
                ..idle.clone()
            },
            "draft",
        ),
        (
            Facts {
                approval_box: true,
                ..idle.clone()
            },
            "box",
        ),
        (facts("busy"), "not-idle"),
        (
            Facts {
                held: true,
                ..idle.clone()
            },
            "held",
        ),
        (
            Facts {
                attended: true,
                ..idle.clone()
            },
            "attended",
        ),
        (
            Facts {
                quiet_s: 3,
                ..idle.clone()
            },
            "settling",
        ),
    ] {
        assert_eq!(
            next_step(&Mode::Daemon, &Phase::Pending, &f, false, false, 0),
            Step::Wait(why)
        );
    }
    // The owner's --now waives the person and the settling, nothing else.
    let attended = Facts {
        attended: true,
        quiet_s: 1,
        ..idle.clone()
    };
    assert_eq!(
        requested_step(
            &Request::Now,
            &Mode::Daemon,
            &Phase::Pending,
            &attended,
            false,
            false,
            0,
            "0.157.1"
        ),
        Step::Terminate
    );
    assert_eq!(
        requested_step(
            &Request::Skip("0.157.1".into()),
            &Mode::Daemon,
            &Phase::Pending,
            &idle,
            false,
            false,
            0,
            "0.157.1"
        ),
        Step::Wait("skipped")
    );
}

#[test]
fn an_embedded_conversation_is_asked_first_and_exits_only_on_ready() {
    let mode = Mode::Embedded {
        thread: T2.into(),
        conversation: true,
    };
    let idle = facts("idle");
    assert_eq!(
        next_step(&mode, &Phase::Pending, &idle, false, false, 100),
        Step::Announce
    );
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    assert_eq!(
        next_step(&mode, &asked, &idle, false, false, 110),
        Step::Wait("awaiting-ready")
    );
    assert_eq!(
        next_step(&mode, &asked, &idle, true, false, 110),
        Step::Terminate
    );
    // Work still running under it is waited for, READY or not.
    let working = Facts {
        background: vec!["zsh".into()],
        ..idle.clone()
    };
    assert_eq!(
        next_step(&mode, &asked, &working, true, false, 110),
        Step::Wait("background")
    );
    // An embedded session with no conversation yet has nothing to wind down.
    let fresh = Mode::Embedded {
        thread: T2.into(),
        conversation: false,
    };
    assert_eq!(
        next_step(&fresh, &Phase::Pending, &idle, false, false, 100),
        Step::Terminate
    );
    // Its daemon is no concern of an embedded session's.
    assert_eq!(
        next_step(&fresh, &Phase::Pending, &idle, false, true, 100),
        Step::Terminate
    );
}

fn daemon(running: &str, pinned: bool) -> DaemonFacts {
    DaemonFacts {
        running: Version::parse(running),
        managed: v("0.157.1"),
        pinned,
        busy_threads: 0,
        terminals: 0,
        settling_threads: 0,
        attended: false,
        held: false,
        owner_held: false,
        unseen_clients: 0,
    }
}

#[test]
fn the_daemon_moves_onto_the_managed_build_only_while_every_thread_is_idle() {
    assert_eq!(daemon_step(&daemon("0.157.0", true)), DaemonStep::Update);
    // The same build UNPINNED: the vendor's updater is armed; pin it.
    assert_eq!(daemon_step(&daemon("0.157.1", false)), DaemonStep::Update);
    assert_eq!(daemon_step(&daemon("0.157.1", true)), DaemonStep::Current);
    // Never a downgrade: a vendor-updated daemon waits for atpkg.
    assert_eq!(
        daemon_step(&daemon("0.158.0", false)),
        DaemonStep::Wait("vendor-ahead")
    );
    assert_eq!(
        daemon_step(&daemon("?", false)),
        DaemonStep::Wait("daemon-version")
    );
    for (f, why) in [
        (
            DaemonFacts {
                busy_threads: 1,
                ..daemon("0.157.0", true)
            },
            "busy-thread",
        ),
        (
            DaemonFacts {
                terminals: 1,
                ..daemon("0.157.0", true)
            },
            "background-terminal",
        ),
        (
            DaemonFacts {
                owner_held: true,
                ..daemon("0.157.0", true)
            },
            "owner-held",
        ),
        (
            DaemonFacts {
                unseen_clients: 1,
                ..daemon("0.157.0", true)
            },
            "unseen-client",
        ),
        (
            DaemonFacts {
                held: true,
                ..daemon("0.157.0", true)
            },
            "held",
        ),
        (
            DaemonFacts {
                attended: true,
                ..daemon("0.157.0", true)
            },
            "attended",
        ),
        (
            DaemonFacts {
                settling_threads: 2,
                ..daemon("0.157.0", true)
            },
            "settling",
        ),
    ] {
        assert_eq!(daemon_step(&f), DaemonStep::Wait(why));
    }
    // A current, pinned daemon is current whatever runs on it.
    assert_eq!(
        daemon_step(&DaemonFacts {
            busy_threads: 3,
            attended: true,
            ..daemon("0.157.1", true)
        }),
        DaemonStep::Current
    );
}

#[test]
fn the_daemons_files_and_answers_are_read_as_measured() {
    let pid = r#"{"pid":92181,"processStartTime":"Fri Sep 25 22:35:28 2026","processIdentity":{"bootId":"135E8EEF-7971-4687-8910-743EA39B1CC9","uniqueId":36806033,"startSeconds":1790400928,"startMicroseconds":853163},"executableIdentity":{"digest":[204,87,33]}}"#;
    assert_eq!(parse_daemon_pid(pid), Some(92181));
    assert_eq!(parse_daemon_pid(r#"{"pid":1}"#), None);
    assert_eq!(parse_daemon_pid("garbage"), None);
    let update = "Replace installed daemon version 0.157.0 with CLI version 0.157.1 from /store/codex/1000000000157000001.\n\
        The daemon package will be installed in /c/packages/app-server-daemon.\n\
        The selected package will be pinned. Run `codex app-server daemon update` to return to production updates.\n\
        The running daemon will restart; active or queued work may be interrupted.\n\
        {\"status\":\"updated\",\"managedCodexPath\":\"/c/packages/app-server-daemon/current/bin/codex\",\"installedVersion\":\"0.157.1\",\"runningVersion\":\"0.157.1\",\"message\":\"The CLI package is selected and pinned.\"}";
    assert_eq!(parse_update_answer(update), Ok(v("0.157.1")));
    assert_eq!(
        parse_update_answer("{\"status\":\"declined\"}"),
        Err("declined".to_string())
    );
    assert_eq!(
        parse_update_answer("Error: boom"),
        Err("no-answer".to_string())
    );
}

#[test]
fn the_notice_opens_with_its_head_and_never_asks_to_cancel() {
    let text = prepare_prompt(&v("0.157.0"), &v("0.157.1"), "ATERM-UPGRADE-READY-0badf00d");
    assert!(text.starts_with(ANNOUNCE_HEAD));
    assert!(text.contains("do not cancel them"));
    assert!(text.contains("ATERM-UPGRADE-READY-0badf00d on a line by itself"));
    assert!(!text.contains('\n'), "one line, typed as one turn");
    let cont = continue_prompt(&v("0.157.0"), &v("0.157.1"));
    assert!(cont.starts_with("[aterm harness] Upgraded: "));
    assert!(cont.contains("Codex 0.157.1 (from 0.157.0)"));
    assert!(
        !cont.starts_with(ANNOUNCE_HEAD),
        "a continuation is no notice"
    );
}

/// The real facts a model state stands for: `version` 0/1/2 as older than,
/// equal to and ahead of the managed build; every busy thread, attached or
/// detached, a busy thread the daemon holds (the daemon's own locks are what
/// reveal a detached one); a background terminal one session leader under
/// the daemon; the owner's word and an unseen client as themselves.
fn project(state: &std::collections::BTreeMap<&'static str, i64>) -> DaemonFacts {
    let running = match state["version"] {
        0 => "0.157.0",
        1 => "0.157.1",
        _ => "0.158.0",
    };
    DaemonFacts {
        running: Version::parse(running),
        managed: v("0.157.1"),
        pinned: state["pinned"] == 1,
        busy_threads: usize::try_from(state["attached_busy"] + state["detached_busy"])
            .expect("a count"),
        terminals: usize::try_from(state["terminal"]).expect("a count"),
        settling_threads: 0,
        attended: false,
        held: false,
        owner_held: state["owner_held"] == 1,
        unseen_clients: usize::try_from(state["unseen"]).expect("a count"),
    }
}

/// TIER-1: the derived `HarnessCodexDaemonUpdate` machine against the REAL
/// `daemon_step`, over EVERY state the machine reaches — the model's `Update`
/// enabled exactly where the shipping rule says `Update`. NEGATIVE CONTROL:
/// the machine's `Buggy=1` member (the rule that asks only the tabs it can
/// see) enables `Update` in a reachable state where the real rule refuses, so
/// a pass here is never vacuous.
#[test]
fn the_real_daemon_rule_is_the_derived_machine() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let reach = |m: &aterm_spec::derive::Model| {
        let mut seen: BTreeSet<BTreeMap<&'static str, i64>> = BTreeSet::new();
        let mut queue = VecDeque::from([m.init_state()]);
        while let Some(s) = queue.pop_front() {
            if !seen.insert(s.clone()) {
                continue;
            }
            for a in &m.actions {
                queue.extend(m.successors(a.name, &s));
            }
        }
        seen
    };
    let model = aterm_spec::derive::harness_codex_daemon_update_model();
    let states = reach(&model);
    assert!(
        states.len() > 8,
        "the machine has a space: {}",
        states.len()
    );
    let mut updates = 0;
    for s in &states {
        let real = daemon_step(&project(s)) == DaemonStep::Update;
        // The machine's `updated` records the move it already made; the real
        // rule is asked of the daemon as it then stands, which is current.
        assert_eq!(
            model.action_enabled("Update", s),
            real && s["updated"] == 0,
            "{s:?}"
        );
        if s["updated"] == 1 {
            assert_eq!(daemon_step(&project(s)), DaemonStep::Current, "{s:?}");
        }
        updates += usize::from(real);
    }
    assert!(updates > 0, "some reachable state moves the daemon");
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let caught = reach(&buggy).into_iter().any(|s| {
        buggy.action_enabled("Update", &s) && daemon_step(&project(&s)) != DaemonStep::Update
    });
    assert!(caught, "the real rule refuses what the buggy one allows");
}

/// AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK (a background terminal a
/// finished turn left, `Facts::background_point`) an EMBEDDED conversation
/// gets its first notice — the owner's answer of 2026-09-26 — and nothing
/// else; a client with no notice to take (daemon mode, a fresh TUI) ends at
/// its `/exit`, which is an idle point's alone. NEGATIVE CONTROL: the same
/// daemon-mode client at an idle point exits as before.
#[test]
fn a_break_of_background_work_notices_an_embedded_session_and_exits_nothing() {
    let at_break = Facts {
        background_point: true,
        quiet_s: 0,
        ..facts("idle")
    };
    let embedded = Mode::Embedded {
        thread: "t".to_string(),
        conversation: true,
    };
    assert_eq!(
        next_step(&embedded, &Phase::Pending, &at_break, false, false, 0),
        Step::Announce
    );
    let announced = Phase::Announced { at_s: 0, asks: 1 };
    assert_eq!(
        next_step(&embedded, &announced, &at_break, true, false, 10),
        Step::Wait("background")
    );
    for mode in [
        Mode::Daemon,
        Mode::Embedded {
            thread: "t".to_string(),
            conversation: false,
        },
    ] {
        assert_eq!(
            next_step(&mode, &Phase::Pending, &at_break, false, false, 0),
            Step::Wait("background"),
            "{mode:?}"
        );
    }
    assert_eq!(
        next_step(
            &Mode::Daemon,
            &Phase::Pending,
            &facts("idle"),
            false,
            false,
            0
        ),
        Step::Terminate
    );
}
