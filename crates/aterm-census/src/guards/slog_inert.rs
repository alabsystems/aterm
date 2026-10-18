// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! slog-inert (design §7.4, phase 3A ruling R5): a mark never becomes input.
//!
//! A tripwire, not the proof: the session log's code — the engine crate
//! `aterm-slog`, and aterm-gui's files that implement marks and the log —
//! names no symbol on the path a byte takes to a session's PTY, nor a request
//! string naming an input verb; and `control.rs`'s `dispatch_mark`, the `mark`
//! arm's whole job, calls only the mark handler, the connection's actor, the
//! read check and the watchers' wake. The PTY path only: a crate, file I/O or
//! a liveness probe (the journal's) is no input path and is not banned. A path
//! through a helper elsewhere evades a lexical rule; the functional negative
//! control (`control_tests::a_mark_act_on_an_open_approval_writes_no_byte`:
//! `mark p<n> act word=approve` on an open approval puts no byte on the PTY)
//! is the proof.

use super::graph::attribute;
use super::{Bound, Finding, Rule, SourceFile, Tree, hits, is_word};

/// The control-verb table the input verbs are read from.
const VERB_TABLE: &str = "crates/aterm-types/src/control_verbs.rs";

/// The session log's engine.
const SLOG_ENGINE: &str = "crates/aterm-slog/src/";

/// aterm-gui's files that implement marks and the log (the journal's by
/// prefix: `session_log_journal.rs`, `_fs.rs`, `_read.rs`).
const GUI_SLOG: &[&str] = &[
    "crates/aterm-gui/src/session_log.rs",
    "crates/aterm-gui/src/session_log_mark.rs",
    "crates/aterm-gui/src/session_log_closed.rs",
    "crates/aterm-gui/src/session_log_carry.rs",
    "crates/aterm-gui/src/session_log_journal",
    // The bus mirror's GUI half (phase 4): what a bridge takes of a log and
    // lands, never a byte toward a PTY. By name, so a renamed file is a
    // scope finding rather than silently unguarded.
    "crates/aterm-gui/src/session_log_mirror.rs",
];

/// The `mark` arm: the file, the function, and what it may call.
const DISPATCH: (&str, &str) = ("crates/aterm-gui/src/control.rs", "dispatch_mark");

/// What `dispatch_mark` may call, by a call's last path segment (a keyword
/// before a parenthesis, `let (a, b)`, is no call).
fn dispatch_allows(call: &str) -> bool {
    matches!(
        call.rsplit("::").next().unwrap_or(call),
        "cmd_mark"
            | "log_actor"
            | "cross_session_authorized"
            | "wake_watchers"
            | "let"
            | "if"
            | "match"
            | "return"
    )
}

/// THE PTY PATH: every symbol that writes a byte to a session's PTY or reaches
/// a path that does, and what it is.
const PTY_PATH: &[(&str, Bound, &str)] = &[
    ("SinkWriter", Bound::Word, "the PTY's writer"),
    ("write_input", Bound::Word, "an input write"),
    ("post_input", Bound::Sub, "the event loop's input hop"),
    ("InputEvent", Bound::Word, "an input event"),
    ("Wake::Input", Bound::Sub, "the input wake"),
    ("cmd_key", Bound::Word, "the key verb"),
    ("cmd_send", Bound::Sub, "the send verb"),
    ("cmd_paste", Bound::Word, "the paste verb"),
    ("cmd_feed", Bound::Sub, "the feed verb"),
    ("cmd_turn", Bound::Sub, "the turn verb"),
    ("TurnIo", Bound::Word, "a turn's input routes"),
    ("HostCtl", Bound::Word, "a control client"),
    ("RelayCtl", Bound::Word, "a control client"),
    (".master(", Bound::Sub, "the PTY master"),
    ("aterm_pty::", Bound::Sub, "the PTY crate"),
    ("background_input", Bound::Word, "a background input write"),
    ("cross_input", Bound::Word, "a cross-session input write"),
    (
        "cross_raw_input",
        Bound::Word,
        "a cross-session input write",
    ),
    (
        "front_routed_input",
        Bound::Word,
        "the front tab's input seam",
    ),
    ("tracked_egress", Bound::Word, "an input write"),
    ("input_if_fenced", Bound::Word, "a fenced input write"),
    ("send_bytes", Bound::Word, "the send verb's bytes"),
    ("feed_bytes", Bound::Word, "the feed verb's bytes"),
];

pub(super) const SLOG_INERT: Rule = Rule {
    id: "slog-inert",
    why: "a mark never becomes input (design §7.4): the session log's code (aterm-slog, \
          aterm-gui's session_log*.rs) names no PTY write path and no input verb in a request, \
          and control.rs's dispatch_mark calls only the mark handler, the connection's actor, \
          the read check and the watchers' wake",
    scope: &[
        SLOG_ENGINE,
        "crates/aterm-gui/src/session_log.rs",
        "crates/aterm-gui/src/session_log_mark.rs",
        "crates/aterm-gui/src/session_log_mirror.rs",
    ],
    check: slog_inert,
};

/// Whether `rel` is a scoped file's shipped source.
fn scoped(rel: &str) -> bool {
    (rel.starts_with(SLOG_ENGINE) || GUI_SLOG.iter().any(|p| rel.starts_with(p)))
        && !super::rules::test_path(rel)
}

/// `.sink` as a field or method (`ctx.sink`, `x.sink(`), never `.sink_…`.
fn names_sink(code: &str) -> bool {
    hits(code, "sink", Bound::Word).any(|at| at > 0 && code.as_bytes()[at - 1] == b'.')
}

fn slog_inert(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let verbs = tree
        .corpus
        .get(VERB_TABLE)
        .map(|f| input_verbs(&f.lexed().text))
        .unwrap_or_default();
    if verbs.len() < 20 {
        out.push(Finding {
            rule: SLOG_INERT.id,
            path: VERB_TABLE.to_string(),
            line: 0,
            detail: format!(
                "read {} input verbs off the verb table, too few to be it",
                verbs.len()
            ),
        });
    }
    let files = tree
        .corpus
        .under(&[SLOG_ENGINE, "crates/aterm-gui/src/"])
        .filter(|f| scoped(&f.rel));
    for file in files {
        out.extend(pty_path_findings(file, &verbs));
    }
    let (path, func) = DISPATCH;
    match tree.corpus.get(path) {
        Some(file) => out.extend(dispatch_findings(file, func)),
        None => out.push(Finding {
            rule: SLOG_INERT.id,
            path: path.to_string(),
            line: 0,
            detail: format!("no fn {func} in this file, so the rule reads nothing"),
        }),
    }
}

/// Every PTY-path symbol (CODE view) and input-verb request (TEXT view) on
/// `file`'s shipped lines.
fn pty_path_findings(file: &SourceFile, verbs: &[String]) -> Vec<Finding> {
    let mut out = Vec::new();
    for line in file.shipped_lines() {
        let named = PTY_PATH
            .iter()
            .find(|(needle, bound, _)| hits(line.code, needle, *bound).next().is_some())
            .map(|(needle, _, what)| format!("names {needle} ({what})"))
            .or_else(|| names_sink(line.code).then(|| ".sink (the PTY's writer)".to_string()));
        if let Some(what) = named {
            out.push(file.finding(SLOG_INERT.id, line.no, format!("a mark's path {what}")));
        } else if let Some(verb) = request_verb(line.text, verbs) {
            out.push(file.finding(
                SLOG_INERT.id,
                line.no,
                format!("a request names the input verb {verb}"),
            ));
        }
    }
    out
}

/// Every call `func` makes past its allow-list, and a finding when `file`
/// defines no `func` (the rule would read nothing).
fn dispatch_findings(file: &SourceFile, func: &str) -> Vec<Finding> {
    let mut seen = false;
    let mut out = Vec::new();
    for l in attribute(file) {
        if l.in_test || l.func != Some(func) {
            continue;
        }
        seen = true;
        for call in path_calls(l.line.code) {
            if call != func && !dispatch_allows(call) {
                out.push(file.finding(
                    SLOG_INERT.id,
                    l.line.no,
                    format!("the mark arm calls past its allow-list: {func} -> {call}"),
                ));
            }
        }
        // A method call is no path call: one that names the PTY path still is.
        if PTY_PATH
            .iter()
            .any(|(needle, bound, _)| hits(l.line.code, needle, *bound).next().is_some())
            || names_sink(l.line.code)
        {
            out.push(file.finding(
                SLOG_INERT.id,
                l.line.no,
                format!("the mark arm names the PTY path: {}", l.line.code.trim()),
            ));
        }
    }
    if !seen {
        out.push(Finding {
            rule: SLOG_INERT.id,
            path: file.rel.clone(),
            line: 0,
            detail: format!("no fn {func} in this file, so the rule reads nothing"),
        });
    }
    out
}

/// Every control verb whose op-class is not `Read`, by its first word: each
/// `v("<name>", <OpClass>, …)` row of the verb table's text view.
fn input_verbs(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut verbs = Vec::new();
    for (at, _) in text.match_indices("v(") {
        if at > 0 && (b[at - 1].is_ascii_alphabetic() || b[at - 1] == b'_') {
            continue;
        }
        let rest = text[at + 2..].trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some((name, rest)) = rest.split_once('"') else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(',') else {
            continue;
        };
        let rest = rest.trim_start();
        let class_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let class = &rest[..class_len];
        if name.is_empty() || class.is_empty() || !rest[class_len..].starts_with(',') {
            continue;
        }
        if class != "Read" {
            verbs.push(name.split(' ').next().unwrap_or(name).to_string());
        }
    }
    verbs.sort();
    verbs.dedup();
    verbs
}

/// The input verb a request string on this line names: `"@… <verb>"` with the
/// verb followed by a space, a `{` or the string's end.
fn request_verb<'v>(text: &str, verbs: &'v [String]) -> Option<&'v str> {
    for (at, _) in text.match_indices("\"@") {
        let body = &text[at + 2..];
        let (body, closed) = match body.find('"') {
            Some(end) => (&body[..end], true),
            None => (body, false),
        };
        for verb in verbs {
            let found = body.match_indices(verb.as_str()).any(|(v, _)| {
                let spaced = v > 0 && body.as_bytes()[v - 1].is_ascii_whitespace();
                let after = body.as_bytes().get(v + verb.len());
                let ends = match after {
                    Some(&c) => c.is_ascii_whitespace() || c == b'{',
                    None => closed,
                };
                spaced && ends
            });
            if found {
                return Some(verb);
            }
        }
    }
    None
}

/// Every free or path call on a line (`name(`, `a::b(`), methods (`.name(`)
/// left out.
fn path_calls(code: &str) -> Vec<&str> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (open, _) in code.match_indices('(') {
        let mut end = open;
        while end > 0 && matches!(b[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (is_word(b[start - 1]) || b[start - 1] == b':') {
            start -= 1;
        }
        let path = code[start..end].trim_start_matches(':');
        let starts_ok = path
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_');
        let method = start > 0 && b[start - 1] == b'.';
        if starts_ok && !method && start == end - path.len() {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n";

    /// A verb table with enough input verbs to be one.
    fn verb_table() -> String {
        let mut t = String::from(HEADER);
        for v in [
            "send", "key", "ctrl", "feed", "paste", "turn", "mouse", "focus", "resize", "tab",
            "pane", "open", "invoke", "confirm", "rain", "kitty", "fx", "signal", "mark", "post",
            "hwkey",
        ] {
            t.push_str(&format!(
                "v(\"{v}\", Write, Status, Session, \"x\", \"\"),\n"
            ));
        }
        t
    }

    const DISPATCH_OK: &str = "fn dispatch_mark(ctx: &SessionCtx, scope: Scope, session: u64, \
         rest: &str, subscribers: &Subscribers) -> String {\n\
         let reads = scope.is_owner_class() || cross_session_authorized(scope, \"text\", ctx);\n\
         let (reply, owed) = crate::session_log::cmd_mark(ctx, control_session::log_actor(scope, \
         ctx), reads, rest);\n\
         if owed { wake_watchers(subscribers, session); }\n\
         reply\n}\n";

    fn tree_with(extra: &[(&str, String)]) -> Vec<Finding> {
        let table = verb_table();
        let control = format!("{HEADER}{DISPATCH_OK}");
        let mut files: Vec<(&str, String)> = vec![
            (VERB_TABLE, table),
            ("crates/aterm-gui/src/control.rs", control),
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
            (
                "crates/aterm-gui/src/session_log_mirror.rs",
                format!("{HEADER}fn m() {{}}\n"),
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
            .map(|(needle, _, _)| {
                let planted = if needle.starts_with('.') {
                    format!("x{needle}1)")
                } else if needle.ends_with("::") {
                    format!("{needle}f(1)")
                } else {
                    format!("{needle}(1)")
                };
                format!("fn bad() {{ let _ = {planted}; }}\n")
            })
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
            let found = tree_with(&[("crates/aterm-slog/src/log.rs", file.clone())]);
            assert_eq!(spots(&found), ["crates/aterm-slog/src/log.rs:2"], "{body}");
            // The bus mirror's GUI half, named in the scope (phase 4).
            let found = tree_with(&[("crates/aterm-gui/src/session_log_mirror.rs", file)]);
            assert_eq!(
                spots(&found),
                ["crates/aterm-gui/src/session_log_mirror.rs:2"],
                "{body}"
            );
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

    /// A renamed scope file reads as a scope finding.
    #[test]
    fn a_renamed_scope_file_is_a_scope_finding() {
        let corpus = super::super::Corpus::of(&[("crates/aterm-slog/src/lib.rs", "")]);
        let found = super::super::scope_findings(&corpus, std::slice::from_ref(&SLOG_INERT));
        assert_eq!(
            found.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            [
                "crates/aterm-gui/src/session_log.rs",
                "crates/aterm-gui/src/session_log_mark.rs",
                "crates/aterm-gui/src/session_log_mirror.rs"
            ]
        );
    }
}
