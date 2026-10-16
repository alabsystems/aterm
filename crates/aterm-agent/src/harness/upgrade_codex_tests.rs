// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::upgrade::{self, QUIET_S};

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

/// The atpkg store's codex 0.157.1 manifest, verbatim.
const PACKAGE_0_157_1: &str = r#"{
  "layoutVersion": 1,
  "version": "0.157.1",
  "target": "aarch64-apple-darwin",
  "variant": "codex",
  "entrypoint": "bin/codex",
  "resourcesDir": "codex-resources",
  "pathDir": "codex-path"
}"#;

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

/// AN ALAB BUILD IS KNOWN BY ITS PACKAGE, NOT ITS VERSION: codex-alab's
/// package carries the publication scheme's pre-release version
/// (`X.Y.0-dev.<n>+g<sha>`), which [`Version::parse`] refuses on purpose. The
/// proven self-claim's image fact ([`alab_bytes`]) must still see an ALab build
/// there (measured 2026-10-16: no claim was ever proven from a real package),
/// while [`package_version`], the only reader that orders builds, still answers
/// nothing for it.
#[test]
fn an_alab_package_with_a_pre_release_version_is_alab_bytes() {
    let d = crate::supervise::ScratchDir::new("codex", "alab-pre-release");
    let root = d.join("codex/0.164.0-dev.12365+g124c29509");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").expect("exe");
    let manifest = PACKAGE_0_157_1.replace("0.157.1", "0.164.0-dev.12365+g124c29509");
    std::fs::write(root.join(PACKAGE_JSON), &manifest).expect("manifest");
    assert_eq!(
        package_entrypoint(&manifest),
        Some(PathBuf::from("bin/codex"))
    );
    assert_eq!(parse_package(&manifest), None, "no order is guessed");
    // Negative control: without ALab's marker it is no ALab build.
    assert!(!alab_bytes(&exe));
    assert!(vendor_bytes(&exe));
    std::fs::write(root.join(ALAB_BUILD_JSON), "{}").expect("marker");
    assert!(
        alab_bytes(&exe),
        "the package and its marker say ALab built it"
    );
    assert!(!vendor_bytes(&exe));
    assert_eq!(package_version(&exe), None);
    // A changed layout or an entrypoint outside the root still refuses.
    for bad in [
        manifest.replace("\"layoutVersion\": 1", "\"layoutVersion\": 2"),
        manifest.replace("bin/codex", "/abs/codex"),
        manifest.replace("bin/codex", "../codex"),
    ] {
        assert_eq!(package_entrypoint(&bad), None, "{bad}");
    }
}

#[test]
fn a_version_is_read_from_the_package_that_names_the_binary_never_by_running_it() {
    let d = crate::supervise::ScratchDir::new("codex", "pkg");
    let root = d.join("store/codex/1000000000157000001");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    // An executable that would hang if run: the reader must never exec it, and
    // a one-minute hang detector tells the two apart.
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "#!/bin/sh\nsleep 600\n").expect("exe");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    std::fs::write(root.join(PACKAGE_JSON), PACKAGE_0_157_1).expect("manifest");
    let started = std::time::Instant::now();
    assert_eq!(package_version(&exe), Some(v("0.157.1")));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    // Negative control: a manifest whose entrypoint names another binary does
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
}

/// The Codex lane carries the model and only carries it: whatever model the launch named, in any
/// spelling (`-m x`, `-mx`, `--model=x`), rides the relaunch verbatim with every other kept flag;
/// a launch that named none gets none.
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
        "--model=gpt-5.4-mini",
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
            "--model=gpt-5.4-mini",
            T1,
        ])
    );
    // A session that was itself a resume, or a fork: the old id, `--last`,
    // `--all` and the first prompt go; the thread the exit names comes back
    // — and a launch that named no model gets none.
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
    // Every process of the daemon is named `codex` too.
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

/// The tab after a daemon-mode `/exit` (0.157.0): the launch's command line,
/// the hint, then the prompt with the cursor.
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

/// A daemon-mode `/exit` from 0.159 on: the embedded shape, under `To reconnect,
/// run:` (the heading and the lines around it are both store builds' strings,
/// 0.159.3 and 0.160.0; neither prints 0.157's `Reconnect:` line).
fn daemon_exit_screen_two_line() -> Vec<String> {
    rows(&[
        "% codex",
        "Disconnected from this task. Any running work continues.",
        "To reconnect, run:",
        &format!("  codex resume {T1}"),
        "Stop the current turn: run codex agents, select this task, and press x.",
        "% ",
    ])
}

/// The same tab after a second exit, of an embedded session: the first exit's
/// hint is still on the screen above it.
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
    // The last hint, not the first: the older exit's is above it.
    assert_eq!(
        parse_exit_hint(&embedded_exit_screen()),
        Some(ExitHint {
            thread: T2.into(),
            daemon: false
        })
    );
    // 0.159+: the daemon's id under its own heading is still a daemon hint
    // (missed, every daemon-mode upgrade would end `failed:no-resume-hint`).
    let two_line = daemon_exit_screen_two_line();
    let daemon = Some(ExitHint {
        thread: T1.into(),
        daemon: true,
    });
    assert_eq!(parse_exit_hint(&two_line), daemon);
    assert_eq!(hint_above_prompt(&two_line, 5), daemon);
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
    // is also what a person types at a prompt.
    assert_eq!(
        parse_exit_hint(&rows(&[&format!("% codex resume {T2}")])),
        None
    );
}

#[test]
fn a_tui_that_printed_nothing_is_never_relaunched_into_an_older_exits_thread() {
    // Negative control for reading the hint from the TUI's own rows: the second
    // TUI died silently, and the first exit's hint (another conversation) is
    // still two rows above the new prompt.
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
    // 0.157.0, an inline TUI, a two-row prompt (`%~` / `%#`): the daemon-mode
    // hint, its stop line and the token tally printed once a turn ran, then
    // the prompt's two rows with the cursor on the second.
    let screen = rows(&[
        "  line 45 of a long answer",
        "Disconnected from this task. Any running work continues.",
        &format!("Reconnect: codex resume {T1}"),
        "Stop the current turn: run codex agents, select this task, and press x.",
        "Token usage so far: total=2 input=1 output=1",
        "/tmp/fxl.VyGP/work",
        "%",
    ]);
    // Bounded by the cursor row, the prompt's first row stands between the
    // hint and it (read as exited, it would never be relaunched).
    assert_eq!(hint_above_prompt(&screen, 6), None);
    // Bounded by the prompt's first row (the entering block's `cmd -
    // prompt` = 1 row above the cursor): the TUI's own words.
    assert_eq!(
        hint_above_prompt(&screen, 5),
        Some(ExitHint {
            thread: T1.into(),
            daemon: true
        })
    );
    // The embedded form prints its tally above the hint.
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
    // Negative control: the tally is the TUI's own line; the command line that
    // ran it is not, and an older exit's hint above it stays unread.
    let nothing = rows(&[
        &format!("Reconnect: codex resume {T1}"),
        "Token usage so far: total=2 input=1 output=1",
        "% codex",
        "%",
    ]);
    assert_eq!(hint_above_prompt(&nothing, 3), None);
}

/// The exit hint is read by words the drift canary looks for: each is a
/// literal `codex.exit.*` row of `aterm_phase::anchors::CODEX_ANCHORS`, which
/// the canary greps the store's Codex for, so a build that rewords one fails
/// a test run before an upgrade types `/exit` into it. 0.157's `Reconnect:`
/// line is the one word read that no anchor carries: no build since prints
/// it. NEGATIVE CONTROL: the embedded row under a heading no anchor names is
/// no hint.
#[test]
fn the_exit_hints_words_are_the_canarys() {
    use aterm_phase::anchors::{AnchorKind, CODEX_ANCHORS};
    let exit: Vec<&str> = CODEX_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("codex.exit.") && a.kind == AnchorKind::Literal)
        .map(|a| a.text)
        .collect();
    for word in [
        RECONNECT_HEADING,
        CONTINUE_HEADING,
        CONTINUE,
        STOP_HINT,
        TOKEN_USAGE,
    ] {
        assert!(exit.contains(&word), "{word:?} is in no codex.exit anchor");
    }
    assert_eq!(exit.len(), 5, "{exit:?}");
    assert!(
        CODEX_ANCHORS
            .iter()
            .all(|a| !a.text.contains(RECONNECT.trim_end())),
        "0.157's line is history, in no anchor"
    );
    let reworded = rows(&["To resume, run:", &format!("  codex resume {T2}")]);
    assert_eq!(parse_exit_hint(&reworded), None);
}

#[test]
fn a_background_terminal_is_a_session_leader_under_codex_and_a_helper_is_not() {
    // A real process table (0.157.0): the daemon 6854 leads its own session;
    // its MCP servers 6872/7092 are in that session; the unified-exec
    // `sleep 7771` (7095) leads its own. Under an embedded TUI (7180, in the
    // tab shell's session 7173) the same: `sleep 7772` leads its own, the MCP
    // server 7201 does not.
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
    // The terminal's own children are in its session: one terminal, once.
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

/// `lsof -a -U -c codex -F pcdn` output (0.157.0): the TUI 6847 (two
/// socketpairs, and fd 35 naming the daemon's listener), the daemon 6854 (its
/// socketpair 7/8, its listener 10 and one accepted connection 29, both named
/// by the socket's path) and the vendor's updater 6864 on its own socket.
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
    // The vendor's updater is no client; a daemon with none attached reads an
    // empty list; one not in the listing reads nothing, never "no clients".
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

/// Real rollout lines (0.157.0): a turn started, the user's message, the turn
/// aborted by Esc.
const STARTED: &str = r#"{"timestamp":"2026-09-26T05:37:10.315Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","root_turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","started_at":1790401030,"model_context_window":258400,"collaboration_mode_kind":"default"}}"#;
const USER: &str = r#"{"timestamp":"2026-09-26T05:37:10.338Z","ordinal":8,"type":"response_item","payload":{"type":"message","id":"msg_01a0dc37-a8c2-78e0-a4a8-fc661bfaae1b","role":"user","content":[{"type":"input_text","text":"say hello"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","create_time":1790401030.338192,"content_item_kinds":["user.text"]}},"metadata":{"client_authored":false,"user_input_order":0,"mcp_attribution":{"status":"none"}}}"#;
const ABORTED: &str = r#"{"timestamp":"2026-09-26T05:37:50.848Z","ordinal":11,"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"01a0dc37-a8a4-7932-8499-ba298833de9f","reason":"interrupted","started_at":1790401030,"completed_at":1790401070,"duration_ms":40536}}"#;
/// 0.145's end of a turn (`last_agent_message`, `turn_id`).
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

/// A settings row Codex 0.158.0 writes between turns (`thread_settings_applied`,
/// its measured keys, trimmed).
const SETTINGS: &str = r#"{"timestamp":"2026-09-28T01:00:00.000Z","ordinal":9,"type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"01a0dc36-1dc1-7ee2-be91-11bc323e377c","thread_settings":{"model":"gpt-5.5","reasoning_effort":"xhigh"}}}"#;

/// An `item_completed` row of at least `size` bytes: a command's output, which
/// Codex can write as one line of hundreds of KiB after its turn ended, its
/// output opening with `quoted`, escaped as JSON text.
fn item_line(size: usize, quoted: &str) -> String {
    let output = format!("{quoted}{}", "x".repeat(size));
    format!(
        r#"{{"timestamp":"2026-09-27T20:56:02.000Z","ordinal":10,"type":"event_msg","payload":{{"type":"item_completed","turn_id":"1","item":{{"type":"commandExecution","aggregated_output":{}}}}}}}"#,
        aterm_json::to_string(&Value::from(output.as_str())).expect("json")
    )
}

/// A rollout of `lines`, each ended by a newline, as bytes.
fn rollout_bytes(lines: &[&str]) -> Vec<u8> {
    lines
        .iter()
        .flat_map(|l| format!("{l}\n").into_bytes())
        .collect()
}

/// [`rollout_turn_back`] over `bytes`, `chunk` bytes at a time, at most
/// `bound` back from their end.
fn back(bytes: &[u8], chunk: u64, bound: u64) -> TurnState {
    let len = u64::try_from(bytes.len()).expect("len");
    rollout_turn_back(&mut std::io::Cursor::new(bytes), len, chunk, bound)
}

/// A turn ended, then one line longer than the 256 KiB tail (a command's output
/// written after the turn), then settings rows: read back past it, the
/// conversation is idle, not Unknown. Controls: the same line after a turn
/// that started reads busy, and so does one whose output quotes a turn's end
/// (text in a string, never an event).
#[test]
fn an_idle_turn_behind_a_line_longer_than_the_tail_reads_idle() {
    const TAIL: u64 = 262_144;
    const BOUND: u64 = 8 << 20;
    let big = item_line(300_000, "");
    let idle = rollout_bytes(&[STARTED, USER, COMPLETE_0_145, &big, SETTINGS, SETTINGS]);
    assert_eq!(
        back(&idle, TAIL, TAIL),
        TurnState::Unknown,
        "the tail alone holds no turn event"
    );
    assert_eq!(back(&idle, TAIL, BOUND), TurnState::Idle);
    let aborted = rollout_bytes(&[STARTED, ABORTED, &big]);
    assert_eq!(back(&aborted, TAIL, BOUND), TurnState::Idle);

    let busy = rollout_bytes(&[STARTED, COMPLETE_0_145, STARTED, &big, SETTINGS]);
    assert_eq!(back(&busy, TAIL, BOUND), TurnState::Busy);
    let quoted = item_line(300_000, &[COMPLETE_0_145, ABORTED].join("\n"));
    assert!(quoted.contains("task_complete") && quoted.contains("turn_aborted"));
    let decoy = rollout_bytes(&[STARTED, &quoted, SETTINGS]);
    assert_eq!(back(&decoy, TAIL, BOUND), TurnState::Busy);
    // And nothing to read reads nothing.
    assert_eq!(back(&[], TAIL, BOUND), TurnState::Unknown);
    let none = rollout_bytes(&[USER, &big, SETTINGS]);
    assert_eq!(back(&none, TAIL, BOUND), TurnState::Unknown);
}

/// Wherever the chunks cut it, the turn read back is the whole file's: every
/// chunk size from one byte to past the file, over a line many chunks long, a
/// turn event at the file's first byte, a last line with no newline or
/// half-written, and a line quoting turn events.
#[test]
fn a_turn_event_is_read_back_wherever_the_chunks_cut_it() {
    let long = item_line(600, "");
    let quoted = item_line(200, &[COMPLETE_0_145, ABORTED].join("\n"));
    let half = &STARTED[..STARTED.len() / 2];
    let cases: [(&str, Vec<u8>, TurnState); 7] = [
        (
            "idle behind a long line",
            rollout_bytes(&[STARTED, USER, COMPLETE_0_145, &long, SETTINGS]),
            TurnState::Idle,
        ),
        (
            "busy behind a long line",
            rollout_bytes(&[COMPLETE_0_145, STARTED, &long, SETTINGS]),
            TurnState::Busy,
        ),
        (
            "a quoted end is text",
            rollout_bytes(&[STARTED, &quoted, SETTINGS]),
            TurnState::Busy,
        ),
        (
            "the first line",
            rollout_bytes(&[ABORTED, &long]),
            TurnState::Idle,
        ),
        (
            "no last newline",
            [STARTED, ABORTED].join("\n").into_bytes(),
            TurnState::Idle,
        ),
        (
            "half-written last line",
            [STARTED, ABORTED, half].join("\n").into_bytes(),
            TurnState::Idle,
        ),
        (
            "no turn event",
            rollout_bytes(&[USER, &long, SETTINGS]),
            TurnState::Unknown,
        ),
    ];
    for (case, bytes, want) in &cases {
        let len = u64::try_from(bytes.len()).expect("len");
        for chunk in 1..=len + 1 {
            assert_eq!(back(bytes, chunk, u64::MAX), *want, "{case}: chunk {chunk}");
        }
    }
}

/// The worst case is linear: a line longer than a chunk is carried as its pieces, each chunk moved
/// into the carry (never copied) and joined once, when the chunk holding the line's start arrives.
#[test]
fn a_line_longer_than_many_chunks_is_carried_once() {
    const TAIL: u64 = 262_144;
    const BOUND: u64 = 8 << 20;
    const CHUNK: usize = 4096;
    let mut back_of = TurnBack::default();
    let mut scan = |text: &str| text.lines().rev().find_map(turn_event);
    let mut bytes = rollout_bytes(&[STARTED]);
    let middle = vec![b'x'; 8 * CHUNK];
    bytes.extend_from_slice(&middle[..middle.len() - 1]);
    bytes.push(b'\n');
    // Fed from the end a chunk at a time: the last chunk (the long line's end
    // and its newline), then every chunk of its middle, none holding a newline.
    let mut end = bytes.len();
    let mut fed = 0;
    while end > STARTED.len() + 1 {
        let start = end.saturating_sub(CHUNK).max(STARTED.len() + 1);
        let chunk = bytes[start..end].to_vec();
        let (at, n) = (chunk.as_ptr(), chunk.len());
        assert_eq!(
            back_of.feed(chunk, false, &mut scan),
            None,
            "no line whole yet"
        );
        fed += n;
        assert_eq!(back_of.carried(), fed, "the carry is the bytes read");
        let last = back_of.pieces.last().expect("a piece");
        assert!(
            std::ptr::eq(last.as_ptr(), at),
            "a chunk is moved into the carry, not copied"
        );
        end = start;
    }
    assert_eq!(back_of.pieces.len(), 8, "one piece per chunk read");
    // The chunk holding the line's start: `task_started` and its newline.
    let head = bytes[..end].to_vec();
    assert_eq!(back_of.feed(head, true, &mut scan), Some(TurnState::Busy));
    assert_eq!(back_of.carried(), 0, "from the file's start nothing is cut");

    // Long-line shapes, at the drive's chunk and bound.
    let past = item_line(9 << 20, "");
    let quoting = item_line(9 << 20, COMPLETE_0_145);
    for (case, text, want) in [
        (
            "a line past the bound",
            rollout_bytes(&[COMPLETE_0_145, &past]),
            TurnState::Unknown,
        ),
        (
            "the same, quoting a turn's end",
            rollout_bytes(&[COMPLETE_0_145, &quoting]),
            TurnState::Unknown,
        ),
        (
            "task_started behind nearly the whole bound",
            rollout_bytes(&[
                COMPLETE_0_145,
                STARTED,
                &item_line((8 << 20) - 200_000, COMPLETE_0_145),
            ]),
            TurnState::Busy,
        ),
        (
            "the bound filled with quoting rows",
            rollout_bytes(
                &std::iter::repeat_n(item_line(64 << 10, COMPLETE_0_145), 144)
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ),
            TurnState::Unknown,
        ),
    ] {
        assert_eq!(back(&text, TAIL, BOUND), want, "{case}");
    }
}

/// Fail closed past the bound: a turn event is read only when its whole line,
/// the newline before it included, lies within `bound` bytes of the end, and
/// nothing further back is read instead; the state stays Unknown, counted
/// running. A rollout shorter than the length it was measured at is Unknown
/// too.
#[test]
fn past_the_bound_the_turn_stays_unknown() {
    let big = item_line(3_000, "");
    let text = rollout_bytes(&[STARTED, COMPLETE_0_145, &big]);
    let from_end = |line: &str| {
        let at = text
            .windows(line.len())
            .position(|w| w == line.as_bytes())
            .expect("the line");
        u64::try_from(text.len() - at).expect("bytes")
    };
    let complete = from_end(COMPLETE_0_145);
    for chunk in [1, 64, 4096, 1 << 20] {
        assert_eq!(back(&text, chunk, complete + 1), TurnState::Idle, "{chunk}");
        assert_eq!(back(&text, chunk, complete), TurnState::Unknown, "{chunk}");
        assert_eq!(
            back(&text, chunk, complete - 10),
            TurnState::Unknown,
            "{chunk}"
        );
    }
    // The older `task_started` is never read in the cut line's place.
    let busy_first = rollout_bytes(&[COMPLETE_0_145, STARTED, &big]);
    let started = {
        let at = busy_first
            .windows(STARTED.len())
            .position(|w| w == STARTED.as_bytes())
            .expect("the line");
        u64::try_from(busy_first.len() - at).expect("bytes")
    };
    assert_eq!(back(&busy_first, 64, started + 1), TurnState::Busy);
    assert_eq!(back(&busy_first, 64, started - 1), TurnState::Unknown);
    let len = u64::try_from(text.len()).expect("len");
    assert_eq!(
        rollout_turn_back(&mut std::io::Cursor::new(&text), len + 1, 64, u64::MAX),
        TurnState::Unknown
    );
}

#[test]
fn ready_is_the_agents_last_word_after_the_latest_notice() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let notice = prepare_prompt(&v("0.157.0"), &v("0.157.1"), marker);
    // The notice itself carries the marker, and is not the answer.
    assert!(!rollout_has_ready(&user(&notice), marker));
    let answered = [user(&notice), assistant(&format!("Done.\n`{marker}`"))].join("\n");
    assert!(rollout_has_ready(&answered, marker));
    // A marker merely quoted in a sentence is no answer.
    let quoted = [
        user(&notice),
        assistant(&format!("I will reply {marker} once the build ends")),
    ]
    .join("\n");
    assert!(!rollout_has_ready(&quoted, marker));
    // Moved past: the agent answered again and went back to work.
    let moved = [answered.clone(), user("keep going"), assistant("On it.")].join("\n");
    assert!(!rollout_has_ready(&moved, marker));
    // A new notice ends the old answer until the agent answers it.
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

/// The idle 0.157.0 screen (the rows under the card, 40 rows cut).
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

/// A turn running: the status row above the composer, which keeps its
/// placeholder.
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

/// The composer is read by `aterm_phase::codex`, which knows both input marks
/// (`›` and 0.158.0's `»`): the dim placeholder on either is empty, a typed
/// draft on either is the person's.
#[test]
fn the_composer_is_read_by_codexs_caret_and_its_dim_placeholder() {
    use aterm_phase::codex::{composer as composer_row, composer_draft};
    let idle = idle_rows();
    assert_eq!(composer_row(&idle), Some(6));
    assert_eq!(
        composer_draft(&idle),
        Some((6, vec!["Ask Codex to do anything".to_string()]))
    );
    // 0.158.0's input line (the goal-mode fixture): its placeholder dim at
    // column 2 is an empty composer; the same row not dim is a draft.
    let goal = aterm_phase::prompt::fixtures::screen(aterm_phase::codex::fixtures::GOAL_BUSY_0_158);
    assert_eq!(composer_row(&goal), Some(60));
    assert!(composer_is_empty(&goal, Some((60, 2)), true));
    assert!(!composer_is_empty(&goal, Some((60, 2)), false));
    let mut typed = goal.clone();
    typed[60] = "» fix the footer".to_string();
    assert!(!composer_is_empty(&typed, Some((60, 16)), false));
    // The placeholder: text at the caret, the cursor at column 2, drawn dim.
    assert!(composer_is_empty(&idle, Some((6, 2)), true));
    // Negative control: the same row not dim is a draft whose caret was moved
    // home (`› my draft here`, Home, cell `m … none`).
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
    use aterm_phase::codex::composer as composer_row;
    // The folder-trust gate (0.157.0).
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

/// The quiet upgrade (the Codex lane's default): an embedded conversation is not asked first; at
/// an idle point its `/exit` is the step, with no notice and no READY, while work, a turn, a box
/// or a draft waits. A thread with nothing to resume is ended and started plain only once the
/// owner was told (`notice-shown`); a daemon-mode client, whose `/exit` ends none of its work, is
/// not held for it. With `quiet` false:
/// [`an_embedded_conversation_is_asked_first_and_exits_only_on_ready`].
#[test]
fn a_quiet_embedded_conversation_exits_at_an_idle_point_with_nothing_asked() {
    let mode = Mode::Embedded {
        thread: T2.into(),
        conversation: true,
    };
    let quiet = Facts {
        quiet: true,
        ..facts("idle")
    };
    assert_eq!(
        next_step(&mode, &Phase::Pending, &quiet, false, false, 100),
        Step::Terminate
    );
    for (f, why) in [
        (
            Facts {
                background: vec!["zsh".into()],
                ..quiet.clone()
            },
            "background",
        ),
        (
            Facts {
                status: "busy".into(),
                ..quiet.clone()
            },
            "not-idle",
        ),
        (
            Facts {
                approval_box: true,
                ..quiet.clone()
            },
            "box",
        ),
        (
            Facts {
                composer_empty: false,
                ..quiet.clone()
            },
            "draft",
        ),
    ] {
        assert_eq!(
            next_step(&mode, &Phase::Pending, &f, false, false, 100),
            Step::Wait(why)
        );
    }
    let untold = Facts {
        unnoticed: true,
        ..quiet.clone()
    };
    let plain = Mode::Embedded {
        thread: T2.into(),
        conversation: false,
    };
    assert_eq!(
        next_step(&plain, &Phase::Pending, &untold, false, false, 100),
        Step::Wait(upgrade::NOTICE_SHOWN)
    );
    assert_eq!(
        next_step(&plain, &Phase::Pending, &quiet, false, false, 100),
        Step::Terminate
    );
    assert_eq!(
        next_step(&Mode::Daemon, &Phase::Pending, &untold, false, false, 100),
        Step::Terminate
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
    // The same build unpinned: the vendor's updater is armed; pin it.
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
    assert!(text.ends_with(upgrade::NOTICE_CLOSE), "{text}");
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

/// Tier-1: the derived `HarnessCodexDaemonUpdate` machine against the real
/// `daemon_step` over every reachable state: the model's `Update` is enabled
/// exactly where the shipping rule says `Update`. Negative control: the
/// machine's `Buggy=1` member (asking only the visible tabs) enables `Update`
/// where the real rule refuses.
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

/// At a break of the agent's own background work (a background terminal a
/// finished turn left, `Facts::background_point`) an embedded conversation
/// gets its first notice and nothing else; a client with no notice to take
/// (daemon mode, a fresh TUI) ends at its `/exit`, only at an idle point.
/// Negative control: the same daemon-mode client at an idle point exits.
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

/// The relaunch-on-exit readers: the thread a TUI's argv resumes, a thread id's
/// creation instant (UUIDv7), and a rollout's directory. Negative controls: a
/// plain launch, `resume --last`, a picker and a `fork` name no thread; a
/// non-v7 id has no instant; any other first line has no directory.
#[test]
fn the_relaunch_on_exit_reads_a_thread_its_instant_and_its_directory() {
    let argv = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let t = "01a0e4bd-2b37-75a0-939c-43fb6f1a5dfb";
    assert_eq!(
        resumed_thread(&argv(&["codex", "--no-alt-screen", "resume", t])),
        Some(t.to_string())
    );
    assert_eq!(
        resumed_thread(&argv(&["codex", "-m", "o4", "resume", "--no-daemon", t])),
        Some(t.to_string())
    );
    for a in [
        &["codex"][..],
        &["codex", "resume", "--last"][..],
        &["codex", "resume"][..],
        &["codex", "fork", t][..],
        &["codex", "say hello"][..],
    ] {
        assert_eq!(resumed_thread(&argv(a)), None, "{a:?}");
    }
    // The thread a TUI began at 21:19:57.751Z.
    assert_eq!(thread_created_ms(t), Some(1_790_543_997_751));
    assert_eq!(
        thread_created_ms("01a0e4bd-2b37-45a0-939c-43fb6f1a5dfb"),
        None,
        "v4"
    );
    assert_eq!(thread_created_ms("not-a-thread"), None);
    assert_eq!(
        rollout_cwd(r#"{"type":"session_meta","payload":{"id":"x","cwd":"/private/tmp/w"}}"#),
        Some("/private/tmp/w".to_string())
    );
    assert_eq!(
        rollout_cwd(r#"{"type":"event_msg","payload":{"cwd":"/w"}}"#),
        None
    );
    assert_eq!(rollout_cwd("not json"), None);
}

/// No stop is final in the Codex lane either: a stopped round of a client with
/// no notice to take (daemon mode, or an embedded thread with no conversation)
/// rests `RETRY_S` (`failed`) and then starts a new round, at a break too; an
/// embedded conversation uses the Claude lane's reducer. The owner's skip of
/// the target holds it; a skip of another build does not.
#[test]
fn a_stopped_codex_round_rests_then_rearms() {
    let stopped = Phase::Failed("resumed-elsewhere".to_string());
    let idle = Facts {
        status: "idle".to_string(),
        status_age_s: 60,
        composer_empty: true,
        quiet_s: 60,
        ..Facts::default()
    };
    let aged = |secs: u64, at_break: bool| Facts {
        failed_s: secs,
        background_point: at_break,
        ..idle.clone()
    };
    let embedded = Mode::Embedded {
        thread: "t".to_string(),
        conversation: true,
    };
    for mode in [
        Mode::Daemon,
        Mode::Embedded {
            thread: "t".to_string(),
            conversation: false,
        },
        embedded,
    ] {
        assert_eq!(
            next_step(
                &mode,
                &stopped,
                &aged(upgrade::RETRY_S - 1, false),
                false,
                false,
                1
            ),
            Step::Wait("failed"),
            "{mode:?}"
        );
        for at_break in [false, true] {
            assert_eq!(
                next_step(
                    &mode,
                    &stopped,
                    &aged(upgrade::RETRY_S, at_break),
                    false,
                    false,
                    1
                ),
                Step::Rearm,
                "{mode:?} at_break={at_break}"
            );
        }
        let step = |request: Request| {
            requested_step(
                &request,
                &mode,
                &stopped,
                &aged(upgrade::RETRY_S, false),
                false,
                false,
                1,
                "0.157.1",
            )
        };
        assert_eq!(step(Request::Skip("0.157.1".into())), Step::Wait("skipped"));
        assert_eq!(step(Request::Skip("0.157.0".into())), Step::Rearm);
    }
}

/// A goal on this screen places nothing: where the kernel names no thread, a running thread placed
/// elsewhere still holds the client while its footer shows a goal pursued (the goal's next turn
/// runs under a screen that reads idle). `goal_on_screen` reads that footer under the input line
/// only. Controls: without the goal the placed thread holds nothing; a thread the kernel names is
/// the client's own (`Goal` or `Own`); the footer's words quoted above the input line are no goal.
#[test]
fn a_goal_on_the_screen_keeps_every_running_thread_unplaced() {
    use aterm_phase::codex::fixtures::GOAL_NEXT_TURN_0_158;
    let goal = aterm_phase::prompt::fixtures::screen(GOAL_NEXT_TURN_0_158);
    assert!(goal_on_screen(&goal));
    let pursuing = aterm_phase::anchors::anchor_text("codex.goal.pursuing");
    let caret = aterm_phase::codex::composer(&goal).expect("the input line");
    let mut quoted = goal.clone();
    for row in quoted.iter_mut().skip(caret + 1) {
        *row = row.replace(pursuing, "");
    }
    quoted[caret - 2] = format!("• The footer said {pursuing}10d 3h 14m).");
    assert!(
        !goal_on_screen(&quoted),
        "a transcript row is never the footer"
    );

    let (tui, other_tui) = (4_242, 4_343);
    let two = [root(T1, TurnState::Busy), root(T2, TurnState::Idle)];
    let placed = [T1.to_string()];
    let clients = [tui, other_tui];
    assert_eq!(
        daemon_turn(tui, &two, Some(&clients), &placed, true),
        upgrade::DaemonTurn::Unplaced,
        "under a goal: never placed"
    );
    assert_eq!(
        daemon_turn(tui, &two, Some(&clients), &placed, false),
        upgrade::DaemonTurn::None,
        "no goal: placed elsewhere"
    );
    assert_eq!(
        daemon_turn(tui, &two, Some(&clients), &[], false),
        upgrade::DaemonTurn::Unplaced,
        "not placed"
    );
    let one = [root(T1, TurnState::Busy)];
    for (goal, want) in [
        (true, upgrade::DaemonTurn::Goal),
        (false, upgrade::DaemonTurn::Own),
    ] {
        assert_eq!(daemon_turn(tui, &one, Some(&[tui]), &placed, goal), want);
    }
}

// ---------------------------------------------------------------- the spawn trees

/// Three subagent threads of one conversation (stand-ins).
const S1: &str = "01a0e8a1-0000-7000-8000-000000000001";
const S2: &str = "01a0e8a1-0000-7000-8000-000000000002";
const S3: &str = "01a0e8a1-0000-7000-8000-000000000003";

fn root(thread: &str, turn: TurnState) -> Loaded {
    Loaded {
        thread: thread.to_string(),
        turn,
        lineage: Lineage::Root,
    }
}

fn spawned(thread: &str, parent: &str, turn: TurnState) -> Loaded {
    Loaded {
        thread: thread.to_string(),
        turn,
        lineage: Lineage::Spawned(parent.to_string()),
    }
}

/// A real daemon shape: one conversation (its root, the thread the one TUI
/// resumed) and three subagents it spawned; `root_busy` is the root's turn and
/// `busy` each subagent's.
fn owners_daemon(root_busy: bool, busy: [bool; 3]) -> Vec<Loaded> {
    let turn = |b: bool| if b { TurnState::Busy } else { TurnState::Idle };
    let mut threads = vec![root(T1, turn(root_busy))];
    threads.extend(
        [S1, S2, S3]
            .iter()
            .zip(busy)
            .map(|(s, b)| spawned(s, T1, turn(b))),
    );
    threads
}

/// A rollout's first line names its spawn tree (`fixtures`): a `cli` root is a
/// root, a spawned subagent names its parent (at depth 2 too); a subagent of
/// another shape (a review's) or a spawn naming no thread id is a subagent with
/// no parent; a line that is not a `session_meta`, not JSON, or has an unknown
/// `source` shape is `Unread`.
#[test]
fn a_rollouts_first_line_names_its_spawn_tree() {
    use super::fixtures::{root_meta, spawned_meta};
    assert_eq!(session_lineage(&root_meta(T1)), Lineage::Root);
    assert_eq!(
        session_lineage(&spawned_meta(S1, T1, T1, 1)),
        Lineage::Spawned(T1.to_string())
    );
    assert_eq!(
        session_lineage(&spawned_meta(S2, S1, T1, 2)),
        Lineage::Spawned(S1.to_string()),
        "the DIRECT parent, not the tree's root"
    );
    let review = root_meta(S1).replace(r#""source":"cli""#, r#""source":{"subagent":"review"}"#);
    assert_eq!(session_lineage(&review), Lineage::Subagent);
    let no_id = spawned_meta(S1, "not-a-thread", T1, 1);
    assert_eq!(session_lineage(&no_id), Lineage::Subagent);
    let unknown = root_meta(T1).replace(r#""source":"cli""#, r#""source":{"custom":"x"}"#);
    assert_eq!(session_lineage(&unknown), Lineage::Unread);
    let no_source = root_meta(T1).replace(r#""source":"cli","#, "");
    assert_eq!(session_lineage(&no_source), Lineage::Unread);
    assert_eq!(
        session_lineage(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#),
        Lineage::Unread
    );
    assert_eq!(
        session_lineage(&root_meta(T1)[..400]),
        Lineage::Unread,
        "cut short"
    );
    assert_eq!(session_lineage(""), Lineage::Unread);
}

/// The kernel names a conversation, not a lock: four writer locks (a root and three subagents)
/// under one TUI, the daemon's only client, name the root (the thread `codex resume` takes back),
/// a depth-2 subagent reaching it through its parent. Controls: a second root or client, clients
/// unread, a subagent whose parent the daemon does not hold or whose lineage is unread, a lone
/// subagent, a loop.
#[test]
fn the_kernel_names_the_root_of_the_one_conversation_on_the_daemon() {
    let (tui, other_tui) = (4_242, 4_343);
    let owners = owners_daemon(false, [false; 3]);
    assert_eq!(own_thread(tui, &owners, Some(&[tui])), Some(T1));
    for l in &owners {
        assert_eq!(tree_root(&l.thread, &owners), Some(T1), "{l:?}");
    }
    let mut deep = owners.clone();
    deep.push(spawned(T2, S1, TurnState::Idle));
    assert_eq!(tree_root(T2, &deep), Some(T1), "depth 2");
    assert_eq!(own_thread(tui, &deep, Some(&[tui])), Some(T1));
    // One thread alone is named whatever its lineage reads — but a subagent.
    for lineage in [Lineage::Root, Lineage::Unread] {
        let one = [Loaded {
            lineage,
            ..root(T1, TurnState::Idle)
        }];
        assert_eq!(own_thread(tui, &one, Some(&[tui])), Some(T1));
    }
    for lineage in [Lineage::Spawned(T2.to_string()), Lineage::Subagent] {
        let one = [Loaded {
            lineage,
            ..root(S1, TurnState::Idle)
        }];
        assert_eq!(own_thread(tui, &one, Some(&[tui])), None, "a lone subagent");
    }
    // Negative controls.
    let mut two_roots = owners.clone();
    two_roots.push(root(T2, TurnState::Idle));
    assert_eq!(own_thread(tui, &two_roots, Some(&[tui])), None, "two roots");
    assert_eq!(
        own_thread(tui, &owners, Some(&[tui, other_tui])),
        None,
        "two clients"
    );
    assert_eq!(own_thread(tui, &owners, None), None, "clients unread");
    assert_eq!(own_thread(tui, &owners, Some(&[other_tui])), None);
    let mut orphan = owners.clone();
    orphan[1].lineage = Lineage::Spawned(T2.to_string());
    assert_eq!(tree_root(S1, &orphan), None, "its parent is not held");
    assert_eq!(own_thread(tui, &orphan, Some(&[tui])), None);
    for lineage in [Lineage::Subagent, Lineage::Unread] {
        let mut unread = owners.clone();
        unread[2].lineage = lineage;
        assert_eq!(own_thread(tui, &unread, Some(&[tui])), None);
    }
    let looped = [
        spawned(S1, S2, TurnState::Idle),
        spawned(S2, S1, TurnState::Idle),
    ];
    assert_eq!(tree_root(S1, &looped), None, "a loop hangs from nothing");
    assert_eq!(own_thread(tui, &looped, Some(&[tui])), None);
}

/// A turn of the tab's own conversation holds it, subagents included: with one root, three
/// subagents and one TUI, any running thread is this client's own (`Goal` under a goal, else
/// `Own`), placed or not; all idle, nothing holds it. Controls with two roots and two TUIs (the
/// kernel names nothing): the other root, placed, holds nothing; a subagent is never placed; a
/// root is never placed while this TUI is the only client, clients are unread, or under a goal.
#[test]
fn a_turn_of_the_tabs_own_conversation_holds_it_subagents_included() {
    use upgrade::DaemonTurn;
    let (tui, other_tui) = (4_242, 4_343);
    for (root_busy, busy) in [
        (false, [true, false, false]),
        (true, [false; 3]),
        (true, [true; 3]),
    ] {
        let d = owners_daemon(root_busy, busy);
        let running: Vec<String> = d
            .iter()
            .filter(|l| l.running())
            .map(|l| l.thread.clone())
            .collect();
        for placed in [Vec::new(), running] {
            for (goal, want) in [(true, DaemonTurn::Goal), (false, DaemonTurn::Own)] {
                assert_eq!(
                    daemon_turn(tui, &d, Some(&[tui]), &placed, goal),
                    want,
                    "{root_busy} {busy:?} {placed:?}"
                );
            }
        }
    }
    let idle = owners_daemon(false, [false; 3]);
    assert_eq!(
        daemon_turn(tui, &idle, Some(&[tui]), &[], false),
        DaemonTurn::None
    );

    // Negative controls: another conversation on the daemon, and its TUI.
    const R2: &str = T2;
    const R2_SUB: &str = "01a0dc3b-0000-7000-8000-000000000001";
    let both = [tui, other_tui];
    let shared = |mine: [bool; 3], theirs: bool, theirs_sub: bool| {
        let mut d = owners_daemon(false, mine);
        let turn = |b: bool| if b { TurnState::Busy } else { TurnState::Idle };
        d.push(root(R2, turn(theirs)));
        d.push(spawned(R2_SUB, R2, turn(theirs_sub)));
        d
    };
    let all: Vec<String> = [T1, S1, S2, S3, R2, R2_SUB]
        .iter()
        .map(|t| (*t).to_string())
        .collect();
    let d = shared([false; 3], true, false);
    assert_eq!(
        daemon_turn(tui, &d, Some(&both), &all, false),
        DaemonTurn::None,
        "the other session's root, placed: holds nothing"
    );
    assert_eq!(
        daemon_turn(tui, &d, Some(&both), &[], false),
        DaemonTurn::Unplaced,
        "not placed yet"
    );
    assert_eq!(
        daemon_turn(tui, &d, Some(&both), &all, true),
        DaemonTurn::Unplaced,
        "under a goal on this screen: never placed"
    );
    for (label, d) in [
        (
            "the other session's subagent",
            shared([false; 3], false, true),
        ),
        (
            "this session's subagent",
            shared([true, false, false], false, false),
        ),
    ] {
        assert_eq!(
            daemon_turn(tui, &d, Some(&both), &all, false),
            DaemonTurn::Unplaced,
            "{label}: a subagent is never placed"
        );
    }
    let alone = shared([false; 3], true, false);
    assert_eq!(
        daemon_turn(tui, &alone, Some(&[tui]), &all, false),
        DaemonTurn::Unplaced,
        "a second root with this TUI the only client: never placed"
    );
    assert_eq!(
        daemon_turn(tui, &alone, None, &all, false),
        DaemonTurn::Unplaced,
        "clients unread: never placed"
    );
    let mut unread = shared([false; 3], true, false);
    unread[4].lineage = Lineage::Unread;
    assert_eq!(
        daemon_turn(tui, &unread, Some(&both), &all, false),
        DaemonTurn::Unplaced,
        "a thread whose lineage was not read: never placed"
    );
}

// --- the goal pause ----------------------------------------------------------

/// A goal turn's opening rows as a 0.158.0 rollout writes them (ids and text
/// are stand-ins): its context, and the goal's own continuation message.
const TURN_CONTEXT: &str = r#"{"timestamp":"2026-09-29T00:35:15.615Z","ordinal":584306,"type":"turn_context","payload":{"cwd":"/stand-in/project","model":"gpt-6-astra"}}"#;
const GOAL_CONTINUATION: &str = r#"{"timestamp":"2026-09-29T00:35:15.617Z","ordinal":584307,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal"}]}}"#;
const REASONING: &str = r#"{"timestamp":"2026-09-29T00:35:19.730Z","ordinal":584309,"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#;
const TOOL_CALL: &str = r#"{"timestamp":"2026-09-29T00:35:23.432Z","ordinal":584311,"type":"response_item","payload":{"type":"function_call","name":"followup_task","arguments":"{}"}}"#;
/// A turn's `world_state` as 0.158.0 writes it between the turn's opening
/// messages and its `turn_context` (its state cut down here).
const WORLD_STATE: &str = r#"{"timestamp":"2026-09-28T15:28:29.649Z","ordinal":6,"type":"world_state","payload":{"full":true,"state":{"agents_md":{"directory":"/stand-in"}}}}"#;

fn head_back(bytes: &[u8], chunk: u64) -> TurnState {
    let len = u64::try_from(bytes.len()).expect("len");
    rollout_head_back(&mut std::io::Cursor::new(bytes), len, chunk, u64::MAX)
}

/// A goal turn's head (`TurnState::Head`, the one point an Esc may stop a turn at):
/// `task_started`, then only what the turn opened with (its context, the goal's message,
/// `world_state`, a token count), read whole wherever the chunks cut it. Its first reasoning item,
/// a tool call or a half-written line is past it; an ended turn is idle; the plain turn reader
/// reads a head `Busy`.
#[test]
fn a_goal_turns_head_is_told_from_its_work() {
    let token = r#"{"timestamp":"2026-09-29T00:35:15.620Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#;
    let half = &REASONING[..REASONING.len() / 2];
    let cases: [(&str, Vec<u8>, TurnState); 7] = [
        (
            "the head",
            rollout_bytes(&[
                COMPLETE_0_145,
                STARTED,
                TURN_CONTEXT,
                GOAL_CONTINUATION,
                token,
            ]),
            TurnState::Head,
        ),
        (
            "the head as 0.158.0 opens it",
            rollout_bytes(&[
                COMPLETE_0_145,
                STARTED,
                GOAL_CONTINUATION,
                WORLD_STATE,
                TURN_CONTEXT,
            ]),
            TurnState::Head,
        ),
        (
            "task_started alone",
            rollout_bytes(&[COMPLETE_0_145, STARTED]),
            TurnState::Head,
        ),
        (
            "its first reasoning",
            rollout_bytes(&[STARTED, TURN_CONTEXT, GOAL_CONTINUATION, REASONING]),
            TurnState::Busy,
        ),
        (
            "a tool call",
            rollout_bytes(&[STARTED, TURN_CONTEXT, GOAL_CONTINUATION, TOOL_CALL]),
            TurnState::Busy,
        ),
        (
            "a line half-written",
            [STARTED, TURN_CONTEXT, half].join("\n").into_bytes(),
            TurnState::Busy,
        ),
        (
            "an ended turn",
            rollout_bytes(&[STARTED, TURN_CONTEXT, REASONING, COMPLETE_0_145]),
            TurnState::Idle,
        ),
    ];
    for (case, bytes, want) in &cases {
        let len = u64::try_from(bytes.len()).expect("len");
        for chunk in 1..=len + 1 {
            assert_eq!(head_back(bytes, chunk), *want, "{case}: chunk {chunk}");
        }
        // The turn reader never tells a head apart.
        assert_eq!(
            back(bytes, u64::MAX, u64::MAX),
            if *want == TurnState::Head {
                TurnState::Busy
            } else {
                *want
            },
            "{case}"
        );
    }
    assert!(head_part(TURN_CONTEXT) && head_part(GOAL_CONTINUATION) && head_part(token));
    assert!(head_part(WORLD_STATE));
    assert!(!head_part(REASONING) && !head_part(TOOL_CALL) && !head_part(&assistant("hi")));
    assert!(!head_part(half), "a cut line is work");
}

/// A pausing hold of the upgrade's, made at `at` by `how`.
fn pausing(how: crate::harness::goal_hold::How, at: u64) -> crate::harness::goal_hold::Hold {
    crate::harness::goal_hold::Hold::pausing(
        crate::harness::goal_hold::Owner::Upgrade,
        how,
        7,
        "0.158.0",
        at,
    )
}

/// A look at a goal-mode tab at `now`, `hold` its record: the goal pursued,
/// its own turn all that holds the move, at the Land rung, nothing else in
/// the way.
fn goal_look(hold: Option<&crate::harness::goal_hold::Hold>, now: u64) -> GoalLook<'_> {
    GoalLook {
        hold,
        behind: true,
        goal: Some(CodexGoal::Pursuing),
        footer: true,
        land: true,
        goal_holds: true,
        waived: Gate::Go,
        switch: false,
        moved: false,
        abandoned: None,
        head: Some(true),
        free: true,
        esc_free: true,
        typing: false,
        person_since: false,
        sandboxed: false,
        now,
    }
}

/// The goal pause's edges the derived machine (`HarnessUpgradeGoalPause`) leaves to unit tests:
/// the take window and the Esc at a head, the give-up past `PAUSE_GIVE_UP_S`, the resume at the
/// bound and its retries up to `MAX_RESUMES`, the footer's verdicts, a person's hand, the
/// sandbox, the rest after a hold, and what keeps the pause off.
#[test]
fn the_goal_pause_waits_retries_gives_up_and_resumes_at_its_bound() {
    use crate::harness::goal_hold::{Hold, How, Stage};
    let now = 1_790_607_857;
    // The pause and what keeps it off.
    assert_eq!(goal_step(&goal_look(None, now)), GoalStep::Pause);
    for off in [
        GoalLook {
            land: false,
            ..goal_look(None, now)
        },
        GoalLook {
            waived: Gate::Wait("draft"),
            ..goal_look(None, now)
        },
        GoalLook {
            free: false,
            ..goal_look(None, now)
        },
        GoalLook {
            typing: true,
            ..goal_look(None, now)
        },
        GoalLook {
            switch: true,
            ..goal_look(None, now)
        },
        GoalLook {
            goal_holds: false,
            ..goal_look(None, now)
        },
        GoalLook {
            behind: false,
            ..goal_look(None, now)
        },
        GoalLook {
            goal: Some(CodexGoal::Paused),
            ..goal_look(None, now)
        },
        GoalLook {
            abandoned: Some("abandoned:skipped"),
            ..goal_look(None, now)
        },
    ] {
        assert_eq!(goal_step(&off), GoalStep::Pass, "{off:?}");
    }
    // Pausing: its take window, then the Esc at a head; past its head, a
    // wait; past the give-up, given up.
    let typed = pausing(How::Typed, now);
    assert_eq!(
        goal_step(&goal_look(Some(&typed), now + 30)),
        GoalStep::Wait("goal-pausing")
    );
    let late = now + PAUSE_TAKE_S;
    assert_eq!(goal_step(&goal_look(Some(&typed), late)), GoalStep::Esc);
    assert_eq!(
        goal_step(&GoalLook {
            head: Some(false),
            ..goal_look(Some(&typed), late)
        }),
        GoalStep::Wait("goal-pausing")
    );
    assert_eq!(
        goal_step(&GoalLook {
            switch: true,
            ..goal_look(Some(&typed), late)
        }),
        GoalStep::Wait("switch"),
        "nothing typed under a switch"
    );
    let escaped = Hold {
        how: How::Esc,
        tried_at: late,
        ..typed.clone()
    };
    assert_eq!(
        goal_step(&goal_look(Some(&escaped), late + PAUSE_TAKE_S)),
        GoalStep::Wait("goal-pausing"),
        "one Esc"
    );
    assert_eq!(
        goal_step(&goal_look(Some(&escaped), now + PAUSE_GIVE_UP_S)),
        GoalStep::GiveUp
    );
    assert_eq!(
        goal_step(&GoalLook {
            person_since: true,
            ..goal_look(Some(&typed), late)
        }),
        GoalStep::Release("by-hand")
    );
    // The Esc at a head is weighed with the turn's own status row up (a head
    // always shows it), never over a box or a draft.
    assert_eq!(
        goal_step(&GoalLook {
            free: false,
            ..goal_look(Some(&typed), late)
        }),
        GoalStep::Esc,
        "the status row stands at a head"
    );
    assert_eq!(
        goal_step(&GoalLook {
            free: false,
            esc_free: false,
            ..goal_look(Some(&typed), late)
        }),
        GoalStep::Wait("goal-pausing"),
        "a box or a draft"
    );
    // A pause shown with a person's hand since it was typed may be theirs
    // (their Esc, their own `/goal pause`): never claimed. With none, taken.
    let shown = |person: bool| GoalLook {
        goal: Some(CodexGoal::Paused),
        person_since: person,
        ..goal_look(Some(&typed), now + 30)
    };
    assert_eq!(goal_step(&shown(false)), GoalStep::Took);
    assert_eq!(goal_step(&shown(true)), GoalStep::Release("by-hand"));
    // Paused: held, then at its bound resumed without the move.
    let paused = Hold {
        stage: Stage::Paused,
        took_at: now + 5,
        ..typed.clone()
    };
    let paused_look = |t: u64| GoalLook {
        goal: Some(CodexGoal::Paused),
        goal_holds: false,
        ..goal_look(Some(&paused), t)
    };
    assert_eq!(goal_step(&paused_look(now + 600)), GoalStep::Hold);
    assert_eq!(
        goal_step(&paused_look(now + 5 + GOAL_HOLD_BOUND_S)),
        GoalStep::Resume("bound")
    );
    assert_eq!(
        goal_step(&GoalLook {
            typing: true,
            ..paused_look(now + 5 + GOAL_HOLD_BOUND_S)
        }),
        GoalStep::Wait("goal-resume")
    );
    assert_eq!(
        goal_step(&GoalLook {
            footer: false,
            ..paused_look(now + 600)
        }),
        GoalStep::Hold,
        "no footer read decides nothing"
    );
    // A footer naming no goal decides nothing with nobody's hand since (a
    // narrow pane's misread must not drop the owed resume): held, then its
    // resume waited on. With a person's hand since (their `/goal clear`, a
    // thread of their own) it releases. A footer naming another state
    // releases it.
    assert_eq!(
        goal_step(&GoalLook {
            goal: None,
            ..paused_look(now + 600)
        }),
        GoalStep::Hold
    );
    assert_eq!(
        goal_step(&GoalLook {
            goal: None,
            moved: true,
            behind: false,
            ..paused_look(now + 600)
        }),
        GoalStep::Wait("goal-resume"),
        "moved, the resume waits for a footer that names the goal"
    );
    assert_eq!(
        goal_step(&GoalLook {
            goal: None,
            person_since: true,
            ..paused_look(now + 600)
        }),
        GoalStep::Release("changed")
    );
    for other in [
        CodexGoal::UsageLimited,
        CodexGoal::Stalled,
        CodexGoal::Achieved,
        CodexGoal::Unmet,
    ] {
        assert_eq!(
            goal_step(&GoalLook {
                goal: Some(other),
                ..paused_look(now + 600)
            }),
            GoalStep::Release("changed"),
            "{other:?}"
        );
    }
    // A goal that ended in the pause's last turn (achieved) releases a hold
    // still pausing, as a paused one: nothing to resume, and the move goes on.
    assert_eq!(
        goal_step(&GoalLook {
            goal: Some(CodexGoal::Achieved),
            ..goal_look(Some(&typed), now + 30)
        }),
        GoalStep::Release("changed")
    );
    // A resume without the move into a thread fallen into a sandbox is
    // withheld — the goal stays paused, said; the move's own resume is not.
    assert_eq!(
        goal_step(&GoalLook {
            sandboxed: true,
            ..paused_look(now + 5 + GOAL_HOLD_BOUND_S)
        }),
        GoalStep::Wait(SANDBOXED)
    );
    assert_eq!(
        goal_step(&GoalLook {
            sandboxed: true,
            moved: true,
            behind: false,
            ..paused_look(now + 600)
        }),
        GoalStep::Resume("moved")
    );
    // Resuming: waited on, made again, and past MAX_RESUMES left paused.
    let resuming = Hold {
        stage: Stage::Resuming,
        resume_at: now + 700,
        resumes: 1,
        why: "moved".into(),
        ..paused.clone()
    };
    let resuming_look = |h: &Hold, t: u64| {
        goal_step(&GoalLook {
            hold: Some(h),
            behind: false,
            moved: true,
            goal: Some(CodexGoal::Paused),
            goal_holds: false,
            ..goal_look(None, t)
        })
    };
    assert_eq!(
        resuming_look(&resuming, now + 710),
        GoalStep::Wait("goal-resuming")
    );
    assert_eq!(
        resuming_look(&resuming, now + 700 + RESUME_TAKE_S),
        GoalStep::Resume("again")
    );
    // Made again without the move (its first resume was `bound`): withheld
    // in a thread fallen into a sandbox; the move's own, never.
    let unmoved = Hold {
        why: "bound".into(),
        ..resuming.clone()
    };
    let sandboxed_again = |h: &Hold| {
        goal_step(&GoalLook {
            hold: Some(h),
            behind: false,
            moved: true,
            goal: Some(CodexGoal::Paused),
            goal_holds: false,
            sandboxed: true,
            ..goal_look(None, now + 700 + RESUME_TAKE_S)
        })
    };
    assert_eq!(sandboxed_again(&unmoved), GoalStep::Wait(SANDBOXED));
    assert_eq!(sandboxed_again(&resuming), GoalStep::Resume("again"));
    let spent = Hold {
        resumes: MAX_RESUMES,
        ..resuming.clone()
    };
    assert_eq!(
        resuming_look(&spent, now + 700 + RESUME_TAKE_S),
        GoalStep::Wait("goal-left-paused")
    );
    assert_eq!(
        goal_left_since(&spent, now + 700 + RESUME_TAKE_S),
        Some(now + 700 + RESUME_TAKE_S)
    );
    assert_eq!(goal_left_since(&resuming, now + 700 + RESUME_TAKE_S), None);
    // The rest after a hold that ended without its move; none after one that
    // ended with it.
    let resumed = |why: &str| Hold {
        stage: Stage::Resumed,
        why: why.to_string(),
        ..resuming.clone()
    };
    let abandoned = resumed("abandoned:failed");
    assert_eq!(
        goal_step(&goal_look(Some(&abandoned), now + 800)),
        GoalStep::Pass,
        "resting"
    );
    assert_eq!(
        goal_step(&goal_look(Some(&abandoned), now + 700 + GOAL_REST_S)),
        GoalStep::Pause,
        "rested"
    );
    assert_eq!(
        goal_step(&goal_look(Some(&resumed("moved")), now + 800)),
        GoalStep::Pause,
        "a new move's goal is paused afresh"
    );
    assert!(!goal_owed(Some(&abandoned)) && goal_owed(Some(&resuming)));
    // The relaunched Codex's box left to a person: their hand on the tab since
    // makes its answer theirs (`Leave paused` too) and nothing is resumed over
    // it; with no hand since, the resume.
    let mut boxed = paused.clone();
    boxed.tried_at = now + 900;
    assert!(boxed.say(BOX_THEIRS));
    let moved_look = |person: bool| GoalLook {
        hold: Some(&boxed),
        behind: false,
        moved: true,
        goal: Some(CodexGoal::Paused),
        goal_holds: false,
        person_since: person,
        ..goal_look(None, now + 960)
    };
    assert_eq!(goal_step(&moved_look(true)), GoalStep::Release("by-hand"));
    assert_eq!(goal_step(&moved_look(false)), GoalStep::Resume("moved"));
    // A Codex the person restarted themselves in place of the one paused:
    // their hand since makes the goal theirs; with none, it is resumed.
    let restarted = |person: bool| GoalLook {
        hold: Some(&paused),
        abandoned: Some(BY_HAND),
        goal: Some(CodexGoal::Paused),
        goal_holds: false,
        person_since: person,
        ..goal_look(None, now + 960)
    };
    assert_eq!(goal_step(&restarted(true)), GoalStep::Release("by-hand"));
    assert_eq!(goal_step(&restarted(false)), GoalStep::Resume(BY_HAND));
}

/// A narrow pane never drops the owed resume. Codex cuts a narrow status row's left side and its
/// ` · ` goes, so a reader keyed on ` · ` alone would take the key-hint row for the status row and
/// release the hold. Footers are Codex 0.158.0's: at 45 columns the pause is made and taken; at
/// 40 the moved goal is resumed, as at 140 (the control).
#[test]
fn a_narrow_pane_never_drops_the_resume_aterm_owes() {
    use crate::harness::goal_hold::{Hold, How, Owner, Stage};
    let pane = |status: &str, hints: &str| {
        rows(&[
            "    dummy**robe. You can find your API key a\u{2026}",
            "",
            "\u{203a} Ask Codex to do anything",
            "",
            status,
            hints,
            "",
        ])
    };
    let hints45 = "  \u{2190} for agents \u{b7} ? for shortcuts    \u{26a0} 1 \u{b7} f2";
    let pursuing45 = pane(
        "  gpt-5 default \u{b7} /pri\u{2026} Pursuing goal (21s)",
        hints45,
    );
    let paused45 = pane(
        "  gpt-5 default\u{2026} Goal paused (/goal resume)",
        hints45,
    );
    let paused40 = pane(
        "  gpt-5 de\u{2026} Goal paused (/goal resume)",
        "  \u{2190} for agents \u{b7} ? for shortc  \u{26a0} 1 \u{b7} f2",
    );
    let paused140 = pane(
        "  gpt-5 default \u{b7} /private/tmp/claude-502/scratchpad/\u{2026} Goal paused (/goal \
         resume)",
        "  \u{2190} for agents \u{b7} ? for shortcuts        \u{26a0} 1 warning \u{b7} f2 to view",
    );
    let now = 1_790_607_857;
    let read = |r: &[String], hold: Option<&Hold>, moved: bool, now: u64| {
        goal_step(&GoalLook {
            hold,
            behind: !moved,
            goal: aterm_phase::codex::goal_state(r),
            footer: aterm_phase::codex::footer_status(r).is_some(),
            goal_holds: !moved,
            moved,
            ..goal_look(None, now)
        })
    };
    // 45 columns: the pause is made on the pursued footer, and the paused
    // one, 7 cells longer, is read as paused — taken, never released.
    assert_eq!(read(&pursuing45, None, false, now), GoalStep::Pause);
    let pausing = Hold::pausing(Owner::Upgrade, How::Typed, 4242, "0.158.0", now);
    assert_eq!(
        read(&paused45, Some(&pausing), false, now + 30),
        GoalStep::Took
    );
    // 40 columns, the move made: resumed, as at 140.
    let paused = Hold {
        stage: Stage::Paused,
        took_at: now - 600,
        ..Hold::pausing(Owner::Upgrade, How::Typed, 4242, "0.158.0", now - 610)
    };
    assert_eq!(
        read(&paused140, Some(&paused), true, now),
        GoalStep::Resume("moved"),
        "the control"
    );
    assert_eq!(
        read(&paused40, Some(&paused), true, now),
        GoalStep::Resume("moved")
    );
}

/// A CODEX LAUNCH'S PERMISSION MODE ([`permission_argv`]) is its approval
/// and sandbox flags, in every spelling, as the rewrite carries them; every
/// other flag goes. NEGATIVE CONTROL: an argv the rewrite refuses keeps its
/// program alone, and a launch that set no mode has none.
#[test]
fn a_codex_launchs_permission_mode_is_its_approval_and_sandbox_flags() {
    let a = argv(&[
        "codex",
        "-m",
        "gpt-5-codex",
        "-sread-only",
        "--ask-for-approval=never",
        "--approve-for-me",
        "-c",
        "sandbox_mode=x",
        "--dangerously-bypass-approvals-and-sandbox",
        "-a",
        "on-request",
        "--sandbox",
        "workspace-write",
        "hi",
    ]);
    assert_eq!(
        permission_argv(&a),
        argv(&[
            "codex",
            "-sread-only",
            "--ask-for-approval=never",
            "--approve-for-me",
            "--dangerously-bypass-approvals-and-sandbox",
            "-a",
            "on-request",
            "--sandbox",
            "workspace-write",
        ])
    );
    assert_eq!(
        permission_argv(&argv(&["codex", "-s", "read-only", "--worktree"])),
        argv(&["codex"])
    );
    assert_eq!(
        permission_argv(&argv(&["codex", "-m", "o3", "--search"])),
        argv(&["codex"])
    );
}

/// A `turn_context` line as Codex 0.163.0 writes it (the lab's m-perms
/// rollout, 2026-10-16, its long `collaboration_mode` cut away): the
/// approval, reviewer and sandbox given.
fn turn_context_0_163(approval: &str, reviewer: &str, sandbox: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-10-16T18:20:07.010Z\",\"ordinal\":22,\"type\":\"turn_context\",\
         \"payload\":{{\"turn_id\":\"01a145f1-57bf-7df1-b824-9ae917a5f4dd\",\"root_turn_id\":\
         \"01a145f1-57bf-7df1-b824-9ae917a5f4dd\",\"disabled_plugin_ids\":[],\"cwd\":\
         \"/private/tmp/at.dRIY/proj\",\"approval_policy\":{approval},\"approvals_reviewer\":\
         \"{reviewer}\",\"sandbox_policy\":{{\"type\":\"{sandbox}\"}},\"permission_profile\":\
         {{\"type\":\"disabled\"}},\"model\":\"gpt-6.1-sol\",\"comp_hash\":\"3000\"}}}}"
    )
}

/// The mode a measured-shape `turn_context` records ([`ThreadMode`]), read by
/// the supervisor's own reader.
fn mode_of(approval: &str, reviewer: &str, sandbox: &str) -> ThreadMode {
    crate::supervise::codex_usage::rollout_turn_context(&turn_context_0_163(
        approval, reviewer, sandbox,
    ))
    .expect("a turn_context")
    .mode()
}

/// A RELAUNCH CARRIES THE THREAD'S OWN PERMISSION MODE ([`carried_mode`],
/// [`rewrite_argv_carrying`]). MEASURED (Codex 0.163.0, the lab, 2026-10-16):
/// a launch `-a on-request -s read-only` switched to Full Access by
/// `/permissions` records `never`/`danger-full-access` in its next
/// `turn_context`, and a resume carrying the launch's flags runs
/// `on-request`/`read-only` again; a resume with none restores Full Access.
/// So the line carries `-a never -s danger-full-access` in place of the
/// launch's two, every other flag kept. NEGATIVE CONTROLS: a thread still in
/// the launch's mode keeps the launch's flags verbatim; a launch that named no
/// permission flag carries none (Codex restores the thread's); a bypassed
/// launch whose thread now runs SANDBOXED keeps its bypass (that may be a
/// fall, whose remedy is the launch's flags), while a narrower approval there
/// is carried; Codex's auto review is carried as `--approve-for-me`; and
/// `rewrite_argv` itself is the launch's line, unchanged.
#[test]
fn a_relaunch_carries_the_threads_own_permission_mode() {
    let full = mode_of("\"never\"", "user", "danger-full-access");
    assert_eq!(
        full,
        ThreadMode {
            approval: Some("never".into()),
            reviewer: Some("user".into()),
            sandbox: Some("danger-full-access".into()),
        }
    );
    let launch = argv(&[
        "codex",
        "-m",
        "gpt-6.1-sol",
        "-a",
        "on-request",
        "--search",
        "-s",
        "read-only",
    ]);
    // The measured case: Full Access chosen in the session is carried, the
    // model flag kept as launched.
    assert_eq!(
        rewrite_argv_carrying(&launch, Some(T1), Some(&full)).expect("carried"),
        argv(&[
            "resume",
            "-m",
            "gpt-6.1-sol",
            "--search",
            "-a",
            "never",
            "-s",
            "danger-full-access",
            T1,
        ])
    );
    // The control: `rewrite_argv` carries the launch's flags, which undo it.
    assert_eq!(
        rewrite_argv(&launch, Some(T1)).expect("carried"),
        argv(&[
            "resume",
            "-m",
            "gpt-6.1-sol",
            "-a",
            "on-request",
            "--search",
            "-s",
            "read-only",
            T1,
        ])
    );
    // Still in the launch's mode: the launch's flags, verbatim.
    let same = mode_of("\"on-request\"", "user", "read-only");
    assert_eq!(carried_mode(&launch, &same), None);
    assert_eq!(
        rewrite_argv_carrying(&launch, Some(T1), Some(&same)),
        rewrite_argv(&launch, Some(T1))
    );
    // No mode read: the launch's line.
    assert_eq!(
        rewrite_argv_carrying(&launch, Some(T1), None),
        rewrite_argv(&launch, Some(T1))
    );
    // A launch that named no permission flag: a bare resume restores the
    // thread's own mode.
    let plain = argv(&["codex", "-m", "gpt-6.1-sol"]);
    assert_eq!(carried_mode(&plain, &full), None);
    // A bypassed launch whose thread now runs sandboxed (the incident's fall:
    // `never` over `workspace-write`): the bypass stands, spelled as launched.
    let bypass = argv(&["codex", "--dangerously-bypass-approvals-and-sandbox"]);
    let fell = mode_of("\"never\"", "user", "workspace-write");
    assert_eq!(carried_mode(&bypass, &fell), None);
    assert_eq!(
        carried_mode(&argv(&["codex", "-s", "danger-full-access"]), &fell),
        None
    );
    // A person's `Ask for approval` under that launch: the approval is
    // carried, the sandbox never narrowed.
    let ask = mode_of("\"on-request\"", "user", "workspace-write");
    assert_eq!(
        carried_mode(&bypass, &ask),
        Some(argv(&["-a", "on-request", "-s", "danger-full-access"]))
    );
    // Wider and equal are carried: `workspace-write` over a `read-only`
    // launch.
    assert_eq!(
        carried_mode(&launch, &ask),
        Some(argv(&["-a", "on-request", "-s", "workspace-write"]))
    );
    // `Approve for me` is its own flag, and leaving it is spelled out.
    let auto = mode_of("\"on-request\"", "auto_review", "workspace-write");
    assert_eq!(
        carried_mode(&launch, &auto),
        Some(argv(&["--approve-for-me"]))
    );
    let approve = argv(&["codex", "--approve-for-me"]);
    assert_eq!(carried_mode(&approve, &auto), None);
    assert_eq!(
        carried_mode(&approve, &ask),
        Some(argv(&["-a", "on-request", "-s", "workspace-write"]))
    );
    // A `granular` policy is no `-a` word: the launch's approval stands
    // beside the thread's wider sandbox.
    let granular = mode_of("{\"granular\":{}}", "user", "danger-full-access");
    assert_eq!(granular.approval, None);
    assert_eq!(
        carried_mode(&launch, &granular),
        Some(argv(&["-a", "on-request", "-s", "danger-full-access"]))
    );
    // Every spelling of the flags is read: attached and `=` forms.
    let spelled = argv(&["codex", "-aon-request", "--sandbox=read-only"]);
    assert_eq!(carried_mode(&spelled, &same), None);
    assert_eq!(
        rewrite_argv_carrying(&spelled, None, Some(&full)).expect("carried"),
        argv(&["-a", "never", "-s", "danger-full-access"])
    );
}
