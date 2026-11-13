// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n";

/// A verb table with enough input verbs to be one.
fn verb_table() -> String {
    let mut t = String::from(HEADER);
    for v in [
        "send", "key", "ctrl", "feed", "paste", "turn", "mouse", "focus", "resize", "tab", "pane",
        "open", "invoke", "confirm", "rain", "kitty", "fx", "signal", "mark", "post", "hwkey",
    ] {
        t.push_str(&format!(
            "v(\"{v}\", Write, Status, Session, \"x\", \"\"),\n"
        ));
    }
    t
}

const DISPATCH_OK: &str = "fn dispatch_mark(ctx: &SessionCtx, scope: Scope, rest: &str) -> \
     String {\n\
     let reads = scope.is_owner_class() || cross_session_authorized(scope, \"text\", ctx);\n\
     let (reply, owed) = crate::session_log::cmd_mark(ctx, control_session::log_actor(scope, \
     ctx), reads, rest);\n\
     if owed { ctx.watchers.ring_watchers(); }\n\
     reply\n}\n";

/// `needle` in a shipped call, as one line.
fn plant(needle: &str) -> String {
    let planted = if needle.starts_with('.') {
        format!("x{needle}1)")
    } else if needle.ends_with("::") {
        format!("{needle}f(1)")
    } else {
        format!("{needle}(1)")
    };
    format!("fn bad() {{ let _ = {planted}; }}\n")
}

/// Where the PTY path lives in the test tree: one shipped line per
/// needle, `but` left out.
const HOME: &str = "crates/aterm-gui/src/input.rs";

fn home_without(but: &[&str]) -> String {
    let mut t = String::from(HEADER);
    for (needle, _, _) in PTY_PATH.iter().filter(|(n, _, _)| !but.contains(n)) {
        t.push_str(&plant(needle));
    }
    t
}

fn tree_with(extra: &[(&str, String)]) -> Vec<Finding> {
    let table = verb_table();
    let control = format!("{HEADER}{DISPATCH_OK}");
    let mut files: Vec<(&str, String)> = vec![
        (VERB_TABLE, table),
        ("crates/aterm-gui/src/control.rs", control),
        (HOME, home_without(&[])),
        (
            "crates/aterm-slog/src/lib.rs",
            format!("{HEADER}pub fn f() {{}}\n"),
        ),
        (
            "crates/aterm-gui/src/session_log.rs",
            format!("{HEADER}fn g() {{}}\n"),
        ),
        (
            "crates/aterm-gui/src/session_log_mark.rs",
            format!("{HEADER}fn h() {{}}\n"),
        ),
    ];
    for (path, body) in extra {
        match files.iter_mut().find(|(p, _)| p == path) {
            Some(slot) => slot.1 = body.clone(),
            None => files.push((path, body.clone())),
        }
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (*p, b.as_str())).collect();
    run(&SLOG_INERT, &refs)
}

#[test]
fn the_clean_tree_has_no_finding() {
    assert!(tree_with(&[]).is_empty(), "{:?}", tree_with(&[]));
}

/// Each PTY-path symbol, planted in a scoped file, is one finding on its
/// line; the same name in a comment, a literal, a test item or an
/// unscoped file is none.
#[test]
fn each_pty_path_symbol_is_found() {
    let mut planted: Vec<String> = PTY_PATH
        .iter()
        .map(|(needle, _, _)| plant(needle))
        .collect();
    planted.push("fn bad() { let _ = ctx.sink.write_frame(b\"y\"); }\n".to_string());
    for body in planted {
        let file = format!("{HEADER}{body}");
        let found = tree_with(&[("crates/aterm-gui/src/session_log_mark.rs", file.clone())]);
        assert_eq!(
            spots(&found),
            ["crates/aterm-gui/src/session_log_mark.rs:2"],
            "{body}: {found:?}"
        );
        let found = tree_with(&[("crates/aterm-slog/src/log.rs", file)]);
        assert_eq!(spots(&found), ["crates/aterm-slog/src/log.rs:2"], "{body}");
    }
    for clean in [
        "// SinkWriter, cmd_key, aterm_pty:: in a comment\n",
        "const S: &str = \"SinkWriter cmd_key\";\n",
        "#[cfg(test)]\nmod t { fn x() { let _ = cmd_key(1); } }\n",
        "fn f() { let _ = self.sink_name(); let _ = std::fs::write(\"p\", b\"x\"); }\n",
    ] {
        let found = tree_with(&[(
            "crates/aterm-gui/src/session_log_journal_fs.rs",
            format!("{HEADER}{clean}"),
        )]);
        assert!(found.is_empty(), "{clean}: {found:?}");
    }
    let unscoped = tree_with(&[(
        "crates/aterm-gui/src/control_input.rs",
        format!("{HEADER}fn send() {{ let _ = cmd_key(1); }}\n"),
    )]);
    assert!(unscoped.is_empty(), "{unscoped:?}");
}

/// A WRITER'S TWIN IS THE SAME PATH: the `_observed`, `_fenced` and
/// `_metered` twins of the input writers, and the worker launch line
/// with its wake, each planted in a mark file, are each one finding on
/// its line (the land's review planted the first two past the rule).
#[test]
fn a_writers_twins_and_the_launch_line_are_found() {
    for call in [
        "crate::control::input_if_fenced_observed(t, c, e, f, None)",
        "crate::task_launch::type_launch_line(store, sid, nonce, prompt, \"x\")",
        "proxy.send_event(crate::Wake::WorkerLaunch { session: 1 })",
        "crate::control_input::cmd_key_fenced_observed(a, b)",
        "crate::control::post_input_reply_to_observed(a, b)",
        "crate::control::background_input_observed(a, b)",
        "crate::app_input::tracked_egress_metered(a, b)",
        "crate::control::front_routed_resize(a, b)",
    ] {
        let file = format!("{HEADER}fn bad() {{ let _ = {call}; }}\n");
        let found = tree_with(&[("crates/aterm-gui/src/session_log_mark.rs", file)]);
        assert_eq!(
            spots(&found),
            ["crates/aterm-gui/src/session_log_mark.rs:2"],
            "{call}: {found:?}"
        );
    }
}

/// A NEEDLE THAT NAMES NOTHING IS A FINDING: each PTY-path needle the
/// PTY's home no longer names on a shipped line (renamed away, or left
/// only in a comment, a test item or a `*_tests.rs` file) is one finding
/// naming it, so a rename cannot leave the rule blind.
#[test]
fn a_needle_that_names_nothing_is_a_finding() {
    const TESTS: &str = "crates/aterm-gui/src/control_tests.rs";
    for (needle, _, _) in PTY_PATH {
        let (line, rest) = (plant(needle), home_without(&[*needle]));
        for files in [
            vec![(HOME, rest.clone())],
            vec![(HOME, format!("{rest}// {line}"))],
            vec![(
                HOME,
                format!("{rest}#[cfg(test)]\nmod t {{ {} }}\n", line.trim_end()),
            )],
            vec![(HOME, rest.clone()), (TESTS, format!("{HEADER}{line}"))],
        ] {
            let found = tree_with(&files);
            assert_eq!(
                spots(&found),
                [format!("{THIS_RULE}:0")],
                "{needle}: {found:?}"
            );
            assert!(
                found[0].detail.contains(&format!("PTY path's {needle} (")),
                "{found:?}"
            );
        }
    }
}

/// What keeps a needle alive is shipped code (`TestCode::holds`): a module the GUI's root
/// declares test-only (`rig.rs`, whose name says nothing) keeps none alive, and a shipped
/// module whose name opens with `test` does.
#[test]
fn a_needle_lives_only_in_shipped_code() {
    let needle = "cmd_key";
    let (line, rest) = (plant(needle), home_without(&[needle]));
    let lib = "#[cfg(test)]\nmod rig;\npub mod test_binary;\n".to_string();
    let with = |rel: &str| {
        tree_with(&[
            (HOME, rest.clone()),
            ("crates/aterm-gui/src/lib.rs", lib.clone()),
            (rel, format!("{HEADER}{line}")),
        ])
    };
    let dead = with("crates/aterm-gui/src/rig.rs");
    assert_eq!(spots(&dead), [format!("{THIS_RULE}:0")], "{dead:?}");
    let alive = with("crates/aterm-gui/src/test_binary.rs");
    assert!(alive.is_empty(), "{alive:?}");
}

/// PHASE 3B'S FILES: a PTY-path symbol planted in the
/// transcript glue, the observed-marks glue or the step fold is one
/// finding on its line. NEGATIVE CONTROL: the same symbol in the step
/// fold's test file, or in a harness file beside it, is none.
#[test]
fn phase_3b_files_are_in_scope() {
    let body = format!("{HEADER}fn bad() {{ let _ = cmd_key(1); }}\n");
    for path in [
        "crates/aterm-gui/src/session_log_transcript.rs",
        "crates/aterm-gui/src/session_log_observed.rs",
        AGENT_STEPS,
    ] {
        let found = tree_with(&[(path, body.clone())]);
        assert_eq!(spots(&found), [format!("{path}:2")], "{found:?}");
    }
    for path in [
        "crates/aterm-agent/src/harness/steps_tests.rs",
        "crates/aterm-agent/src/harness/footer.rs",
    ] {
        let found = tree_with(&[(path, body.clone())]);
        assert!(found.is_empty(), "{path}: {found:?}");
    }
}

/// A request string naming an input verb (`"@. key enter"`) is a finding;
/// a read verb's (`"@. text"`) is not.
#[test]
fn an_input_request_literal_is_found() {
    let found = tree_with(&[(
        "crates/aterm-gui/src/session_log.rs",
        format!("{HEADER}fn bad() {{ let _ = \"@. key enter\"; }}\n"),
    )]);
    assert_eq!(
        spots(&found),
        ["crates/aterm-gui/src/session_log.rs:2"],
        "{found:?}"
    );
    let read = tree_with(&[(
        "crates/aterm-gui/src/session_log.rs",
        format!("{HEADER}fn ok() {{ let _ = \"@. text\"; }}\n"),
    )]);
    assert!(read.is_empty(), "{read:?}");
}

/// `dispatch_mark` calling past its allow-list is a finding; a missing
/// `dispatch_mark` is one (the rule would read nothing).
#[test]
fn the_mark_arm_calls_only_its_allow_list() {
    let bad = DISPATCH_OK.replace(
        "if owed {",
        "let _ = crate::control_input::cmd_key(ctx, rest);\nif owed {",
    );
    let found = tree_with(&[("crates/aterm-gui/src/control.rs", format!("{HEADER}{bad}"))]);
    assert_eq!(
        found.len(),
        2,
        "a call past the list, naming the PTY path: {found:?}"
    );
    assert!(found.iter().all(|f| f.line == 5), "{found:?}");
    let gone = tree_with(&[(
        "crates/aterm-gui/src/control.rs",
        format!("{HEADER}fn other() {{}}\n"),
    )]);
    assert_eq!(gone.len(), 1, "{gone:?}");
    assert!(
        gone[0].detail.starts_with("no fn dispatch_mark"),
        "{gone:?}"
    );
}
