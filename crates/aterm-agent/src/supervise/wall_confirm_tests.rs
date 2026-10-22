// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transcript's half of the wall check, on what Claude Code 2.1.294 wrote
//! (`supervise/fixtures/claude-2.1.294-transcript-*.jsonl`, measured in the real-Claude lab).

use aterm_phase::{ApiCause, WallKind, classify_wall};

use super::{
    Entry, TranscriptEnd, confirms, ended_with_nothing_pending, found_named, limit_named,
    limit_words, refined, transcript_ends_on, transcript_wall, waits_on_background,
};

/// A measured transcript fixture: its header line, then the rows.
fn fixture(name: &str) -> &'static str {
    match name {
        "529" => include_str!("fixtures/claude-2.1.294-transcript-529.jsonl"),
        "500" => include_str!("fixtures/claude-2.1.294-transcript-500.jsonl"),
        "context" => include_str!("fixtures/claude-2.1.294-transcript-context.jsonl"),
        "compact-failed" => include_str!("fixtures/claude-2.1.294-transcript-compact-failed.jsonl"),
        "not-logged-in" => include_str!("fixtures/claude-2.1.294-transcript-not-logged-in.jsonl"),
        "invalid-key" => include_str!("fixtures/claude-2.1.294-transcript-invalid-key.jsonl"),
        "api-key-429" => include_str!("fixtures/claude-2.1.294-transcript-api-key-429.jsonl"),
        "session-limit" => include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl"),
        "weekly-limit" => include_str!("fixtures/claude-2.1.294-transcript-weekly-limit.jsonl"),
        "session-limit-first-turn" => {
            include_str!("fixtures/claude-2.1.294-transcript-session-limit-first-turn.jsonl")
        }
        "unreachable" => include_str!("fixtures/claude-2.1.294-transcript-unreachable.jsonl"),
        "bash-output-then-context" => {
            include_str!("fixtures/claude-2.1.294-transcript-bash-output-then-context.jsonl")
        }
        "bash-output-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-bash-output-wall-phrase.jsonl")
        }
        "shell-mode-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-shell-mode-wall-phrase.jsonl")
        }
        "mcp-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-mcp-wall-phrase.jsonl")
        }
        "mcp-long-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-mcp-long-wall-phrase.jsonl")
        }
        "mcp-output-then-context" => {
            include_str!("fixtures/claude-2.1.294-transcript-mcp-output-then-context.jsonl")
        }
        "session-limit-then-local-commands" => include_str!(
            "fixtures/claude-2.1.294-transcript-session-limit-then-local-commands.jsonl"
        ),
        "local-commands" => include_str!("fixtures/claude-2.1.294-transcript-local-commands.jsonl"),
        "bucket-cancelled-then-model" => {
            include_str!("fixtures/claude-2.1.302-transcript-bucket-cancelled-then-model.jsonl")
        }
        other => panic!("no fixture {other}"),
    }
}

/// Every kind of wall a screen can read, one of each class and then some.
fn every_kind() -> Vec<WallKind> {
    vec![
        WallKind::UsageSession,
        WallKind::UsageWeekly,
        WallKind::ModelBucket { consent: false },
        WallKind::Spend,
        WallKind::Context,
        WallKind::Auth,
        WallKind::Overloaded,
        WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: ApiCause::Server,
        },
        WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        },
        WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        },
    ]
}

/// Whether two kinds are one class to [`confirms`]: the same wall answered
/// the same way.
fn same_class(a: WallKind, b: WallKind) -> bool {
    let class = |k: WallKind| {
        if k.reads_limited() {
            0
        } else {
            match k {
                WallKind::Context => 1,
                WallKind::Auth => 2,
                WallKind::Memory => 4,
                _ => 3,
            }
        }
    };
    class(a) == class(b)
}

/// Every wall the policy acts on, as Claude Code 2.1.294 wrote it, ends the transcript on its own
/// notice row (its measured `error` word), which confirms the wall the screen drew and no wall of
/// another class. The relaunched first turn's session limit, whose prompt row the file holds
/// after the notice, is among them.
#[test]
fn every_wall_claude_code_writes_confirms_its_own_kind_and_no_other() {
    let cases: [(&str, &[&str], Option<&str>); 12] = [
        (
            "529",
            &[
                "API Error: Repeated 529 Overloaded errors. The API is at capacity — this is usually \
               temporary. Try again in a moment.",
            ],
            Some("server_error"),
        ),
        (
            "500",
            &[
                "API Error: 500 Internal server error. This is a server-side issue, usually \
               temporary — try again in a moment.",
            ],
            Some("server_error"),
        ),
        (
            "unreachable",
            &[
                "API Error: Connection refused — a firewall or proxy may be blocking it \
               (ECONNREFUSED)",
            ],
            Some("server_error"),
        ),
        (
            "context",
            &["Context limit reached · /compact or /clear to continue"],
            Some("invalid_request"),
        ),
        (
            "bash-output-then-context",
            &["Context limit reached · /compact or /clear to continue"],
            Some("invalid_request"),
        ),
        (
            "compact-failed",
            &["Compaction failed · conversation could not be reduced below the context limit"],
            None,
        ),
        (
            "not-logged-in",
            &["Not logged in · Please run /login"],
            Some("authentication_failed"),
        ),
        (
            "invalid-key",
            &["Invalid API key · Fix external API key"],
            Some("authentication_failed"),
        ),
        (
            "session-limit",
            &[
                "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                "Usage limit reached · continuing automatically at 11:33am · esc to cancel",
            ],
            Some("rate_limit"),
        ),
        (
            "session-limit-first-turn",
            &[
                "You've hit your session limit · resets 1:17pm (America/Los_Angeles)",
                "Usage limit reached · continuing automatically at 1:17pm · esc to cancel",
            ],
            Some("rate_limit"),
        ),
        (
            "weekly-limit",
            &["You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)"],
            Some("rate_limit"),
        ),
        (
            "api-key-429",
            &[
                "API Error: Request rejected (429) · This request would exceed your account's rate \
               limit. Please try again later.",
            ],
            Some("rate_limit"),
        ),
    ];
    for (name, screens, error) in cases {
        let end = transcript_ends_on(fixture(name));
        let Entry::Notice { error: written, .. } = &end.last else {
            panic!("{name}: {end:?}");
        };
        assert_eq!(written.as_deref(), error, "{name}");
        assert!(!end.lagging(), "{name}");
        for screen in screens {
            let kind = classify_wall(screen).unwrap_or_else(|| panic!("{name}: {screen}"));
            assert!(confirms(kind, &end), "{name}: {screen} ({kind:?})");
            for other in every_kind() {
                assert_eq!(
                    confirms(other, &end),
                    same_class(other, kind),
                    "{name}: {other:?} against {kind:?}"
                );
            }
        }
    }
}

/// A tool's output in a wall's words (a Bash call, a shell-mode `!`, MCP results of several
/// blocks or a long first block) confirms no wall: the transcript ends on the model's reply.
/// Controls: the same Bash and MCP shapes followed by a real context wall are confirmed.
#[test]
fn a_tools_output_confirms_no_wall() {
    for name in [
        "bash-output-wall-phrase",
        "shell-mode-wall-phrase",
        "mcp-wall-phrase",
        "mcp-long-wall-phrase",
    ] {
        let end = transcript_ends_on(fixture(name));
        assert_eq!((&end.last, end.closed), (&Entry::Reply, true), "{name}");
        for kind in every_kind() {
            assert!(!confirms(kind, &end), "{name}: {kind:?}");
        }
    }
    for name in ["bash-output-then-context", "mcp-output-then-context"] {
        let end = transcript_ends_on(fixture(name));
        assert!(confirms(WallKind::Context, &end), "{name}: {end:?}");
    }
}

/// A transcript cut before its notice and `turn_duration` is lagging (at the prompt, the model's
/// call, the tool's result, or empty); a notice, or any end a `turn_duration` closed, is not. A
/// torn first line is passed over.
#[test]
fn a_transcript_cut_before_its_notice_is_lagging() {
    let rows = |name: &str| -> Vec<&'static str> { fixture(name).lines().skip(1).collect() };
    let upto = |name: &str, n: usize| rows(name)[..n].join("\n");
    // The prompt alone, the notice not written yet.
    let at_prompt = transcript_ends_on(&upto("context", 1));
    assert_eq!(at_prompt.last, Entry::Input);
    assert!(at_prompt.lagging());
    // The tool's result, the request after it not answered yet.
    let bash = rows("bash-output-then-context");
    let result = bash
        .iter()
        .position(|r| r.contains("\"tool_result\""))
        .expect("the tool's result");
    let at_result = transcript_ends_on(&bash[..=result].join("\n"));
    assert_eq!(at_result.last, Entry::ToolResult);
    assert!(at_result.lagging());
    // The model's call, its result not written yet.
    let at_call = transcript_ends_on(&bash[..result].join("\n"));
    assert_eq!(at_call.last, Entry::Reply);
    assert!(at_call.lagging());
    // The notice, its turn_duration not written yet.
    let notice = bash
        .iter()
        .position(|r| r.contains("\"isApiErrorMessage\":true"))
        .expect("the notice");
    assert!(!transcript_ends_on(&bash[..=notice].join("\n")).lagging());
    assert!(transcript_ends_on("").lagging());
    // A torn first line, then the rows: read as the rows alone.
    let torn = format!("ror\":\"rate_limit\"}}\n{}", rows("context").join("\n"));
    assert_eq!(
        transcript_ends_on(&torn),
        transcript_ends_on(fixture("context"))
    );
    // Settled.
    for name in ["context", "bash-output-wall-phrase"] {
        assert!(!transcript_ends_on(fixture(name)).lagging(), "{name}");
    }
    let closed = format!(
        "{}\n{}",
        rows("context")[0],
        rows("context").last().expect("its turn_duration")
    );
    let closed = transcript_ends_on(&closed);
    assert_eq!((&closed.last, closed.closed), (&Entry::Input, true));
    assert!(!closed.lagging());
}

/// `row` (a measured row) as written at `at`: its own `timestamp` replaced.
fn at(row: &str, at: &str) -> String {
    let from = row.find("\"timestamp\":\"").expect("a timestamp") + "\"timestamp\":\"".len();
    let to = from + row[from..].find('"').expect("its end");
    format!("{}{at}{}", &row[..from], &row[to..])
}

/// `row` (a measured row) with its `parentUuid` set to `parent` (a JSON
/// value: `null`, or a quoted id).
fn parent(row: &str, parent: &str) -> String {
    let from = row.find("\"parentUuid\":").expect("a parent") + "\"parentUuid\":".len();
    let to = from + row[from..].find(',').expect("its end");
    format!("{}{parent}{}", &row[..from], &row[to..])
}

/// The first row of fixture `name` that holds `needle`.
fn row_of(name: &str, needle: &str) -> &'static str {
    fixture(name)
        .lines()
        .find(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("{name}: no row with {needle}"))
}

/// Rows that are no entry of the conversation's (a subagent's notice, a local command's rows,
/// the meta nudge, a compaction's summary) leave the notice the newest entry; a prompt, a tool's
/// result and the model's words end it.
#[test]
fn only_the_conversations_own_entries_end_a_notice() {
    let notice = row_of("529", "\"isApiErrorMessage\":true");
    let sidechain = notice.replace("\"isSidechain\":false", "\"isSidechain\":true");
    let prompt = fixture("context").lines().nth(1).expect("the prompt row");
    // A subagent's notice after a prompt: the prompt stands.
    let end = transcript_ends_on(&format!("{prompt}\n{sidechain}"));
    assert_eq!(end.last, Entry::Input);
    assert!(!confirms(WallKind::Overloaded, &end));
    // A local command's own rows after the notice: the notice stands.
    let model = r#"{"parentUuid":"a","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#;
    let caveat = r#"{"parentUuid":"b","isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-caveat>The command below was run directly in Claude Code.</local-command-caveat>"},"isMeta":true}"#;
    let out = r#"{"parentUuid":"c","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>Set model to Opus</local-command-stdout>"}"#;
    let end = transcript_ends_on(&format!("{prompt}\n{notice}\n{caveat}\n{model}\n{out}"));
    assert!(confirms(WallKind::Overloaded, &end), "{end:?}");
    // Claude Code's meta nudge, and a compaction's summary: the notice
    // stands.
    let nudge = row_of("bash-output-wall-phrase", "\"isMeta\":true");
    let summary = prompt.replace(
        "\"type\":\"user\",",
        "\"type\":\"user\",\"isCompactSummary\":true,",
    );
    assert!(summary.contains("isCompactSummary"));
    let transcript_only = prompt.replace(
        "\"type\":\"user\",",
        "\"type\":\"user\",\"queueTranscriptOnly\":true,",
    );
    assert!(transcript_only.contains("queueTranscriptOnly"));
    for (name, after) in [
        ("meta nudge", nudge),
        ("compaction summary", summary.as_str()),
        (
            "transcript-only queue notification",
            transcript_only.as_str(),
        ),
    ] {
        let end = transcript_ends_on(&format!("{prompt}\n{notice}\n{after}"));
        assert!(confirms(WallKind::Overloaded, &end), "{name}: {end:?}");
    }
    // The person's prompt, a tool's result, the model's words: each ends it.
    let result = row_of("bash-output-wall-phrase", "\"tool_result\"");
    let reply = fixture("bash-output-wall-phrase")
        .lines()
        .rfind(|r| r.contains("\"type\":\"assistant\""))
        .expect("a reply");
    for (after, entry) in [
        (prompt, Entry::Input),
        (result, Entry::ToolResult),
        (reply, Entry::Reply),
    ] {
        let end = transcript_ends_on(&format!("{notice}\n{after}"));
        assert_eq!(end.last, entry);
        assert!(!confirms(WallKind::Overloaded, &end), "{entry:?}");
    }
}

/// The file's order decides, not the rows' clocks: an entry written after the notice ends it
/// whatever its `timestamp` says. The one exception: a prompt row with no parent
/// (`parentUuid: null`) written after the rows that answered it ends nothing; with a parent, or
/// with the key missing, it ends the notice.
#[test]
fn the_files_order_decides_but_for_the_conversations_first_row() {
    let notice = at(
        row_of("529", "\"isApiErrorMessage\":true"),
        "2026-10-08T12:00:00.000Z",
    );
    let prompt = row_of("context", "third try");
    let result = row_of("bash-output-wall-phrase", "\"tool_result\"");
    let reply = fixture("bash-output-wall-phrase")
        .lines()
        .rfind(|r| r.contains("\"type\":\"assistant\""))
        .expect("a reply");
    for (after, entry) in [
        (prompt, Entry::Input),
        (result, Entry::ToolResult),
        (reply, Entry::Reply),
    ] {
        // An hour before the notice by its own clock.
        let stepped = at(after, "2026-10-08T11:00:00.000Z");
        let end = transcript_ends_on(&format!("{notice}\n{stepped}"));
        assert_eq!(end.last, entry, "{entry:?}");
        assert!(!confirms(WallKind::Overloaded, &end), "{entry:?}");
    }
    // A notice from before the step, written after the entries: it stands,
    // and confirms its own class only.
    let late = at(&notice, "2026-10-08T10:00:00.000Z");
    let end = transcript_ends_on(&format!("{prompt}\n{late}"));
    assert!(confirms(WallKind::Overloaded, &end), "{end:?}");
    assert!(!confirms(WallKind::Auth, &end), "{end:?}");
    // The measured exception, and its controls.
    let first = fixture("session-limit-first-turn");
    let limited = transcript_ends_on(first);
    assert!(confirms(WallKind::UsageSession, &limited), "{limited:?}");
    assert!(limited.closed);
    let root = row_of("session-limit-first-turn", "\"parentUuid\":null");
    assert!(root.contains("\"type\":\"user\""));
    let rest: Vec<&str> = first
        .lines()
        .skip(1)
        .filter(|r| !r.contains("\"parentUuid\":null"))
        .collect();
    let with_parent = parent(root, "\"9036f8d4-0000-4000-8000-000000000000\"");
    let missing = root.replace("\"parentUuid\":null,", "");
    assert!(!missing.contains("parentUuid"));
    for (name, row) in [
        ("a parent", with_parent.as_str()),
        ("no key", missing.as_str()),
    ] {
        let end = transcript_ends_on(&format!("{}\n{row}", rest.join("\n")));
        assert_eq!(end.last, Entry::Input, "{name}");
        assert!(!confirms(WallKind::UsageSession, &end), "{name}");
    }
    let end = transcript_ends_on(root);
    assert_eq!(end.last, Entry::Input, "the root row read first");
    assert!(end.lagging());
    // Only a prompt opens the conversation out of the file's order: a notice or a reply with no
    // parent is read where the file holds it.
    let root_notice = parent(&notice, "null");
    assert!(root_notice.contains("\"parentUuid\":null"));
    let end = transcript_ends_on(&format!("{prompt}\n{root_notice}"));
    assert!(
        confirms(WallKind::Overloaded, &end),
        "a notice with no parent: {end:?}"
    );
    let root_reply = parent(reply, "null");
    let end = transcript_ends_on(&format!("{notice}\n{root_reply}"));
    assert_eq!(end.last, Entry::Reply, "a reply with no parent");
    assert!(!confirms(WallKind::Overloaded, &end));
}

/// Every row the measured local commands wrote (`system` and `user` rows, caveats, a
/// `compact_boundary`, a summary, a prompt command's `<command-message>` row) is passed over,
/// alone and all together, after a notice of every class. A `user` row of a local command's output
/// in a full context's words is never a notice. Controls: a prompt and a shell-mode command's rows
/// end the notice; a failed `/compact`'s `system` row is the context wall's notice.
#[test]
fn a_local_commands_rows_are_no_turn_and_no_notice() {
    let commands: Vec<&str> = fixture("local-commands").lines().skip(1).collect();
    // Not vacuous: the fixture holds each measured shape, as its kind of row.
    for (kind, shape) in [
        ("system", "\"subtype\":\"local_command\""),
        ("user", "\"content\":\"<command-name>/login</command-name>"),
        (
            "user",
            "\"content\":\"<local-command-stdout>Login interrupted</local-command-stdout>\"",
        ),
        (
            "user",
            "\"content\":\"<local-command-stdout>Disabled auto-compact</local-command-stdout>\"",
        ),
        ("user", "Compacted \\u001b[22m</local-command-stdout>\""),
        ("user", "\"content\":\"/compact\""),
        ("user", "\"content\":\"/compact keep the notes\""),
        (
            "user",
            "\"content\":\"<command-message>doctor</command-message>\\n<command-name>/doctor",
        ),
        ("system", "\"subtype\":\"compact_boundary\""),
        ("user", "\"isCompactSummary\":true"),
    ] {
        let typed = format!("\"type\":\"{kind}\"");
        assert!(
            commands
                .iter()
                .any(|r| r.contains(shape) && r.contains(&typed)),
            "the fixture holds a {kind} row {shape}"
        );
    }
    for (name, kind) in [
        ("529", WallKind::Overloaded),
        ("weekly-limit", WallKind::UsageWeekly),
        ("not-logged-in", WallKind::Auth),
        ("context", WallKind::Context),
    ] {
        let notice = fixture(name).lines().skip(1).collect::<Vec<_>>().join("\n");
        let before = transcript_ends_on(&notice);
        assert!(confirms(kind, &before), "{name}");
        let all = transcript_ends_on(&format!("{notice}\n{}", commands.join("\n")));
        assert_eq!(all.last, before.last, "{name}: all of them");
        assert!(confirms(kind, &all), "{name}: all of them");
        for row in &commands {
            let end = transcript_ends_on(&format!("{notice}\n{row}"));
            // A prompt command's invocation is marked as passed over: its
            // prompt asks the model, whose answer is not written yet.
            let prompted = row.contains("\"type\":\"user\"")
                && row.contains("\"content\":\"<command-message>");
            assert_eq!(
                end,
                TranscriptEnd {
                    prompted,
                    ..before.clone()
                },
                "{name}: {row}"
            );
        }
    }
    // Measured whole: a real session limit, then `/usage`, `/login` dismissed and `/model`.
    let measured = transcript_ends_on(fixture("session-limit-then-local-commands"));
    assert!(confirms(WallKind::UsageSession, &measured), "{measured:?}");
    assert!(!measured.lagging());
    // Never a notice: a user row of a local command's output, a full
    // context's words in it.
    let notice = row_of("529", "\"isApiErrorMessage\":true");
    let login = commands
        .iter()
        .find(|r| r.contains("Login interrupted"))
        .expect("the login's output");
    let failed = login.replace(
        "<local-command-stdout>Login interrupted</local-command-stdout>",
        "<local-command-stderr>Compaction failed · conversation could not be reduced below the \
         context limit</local-command-stderr>",
    );
    assert!(failed.contains("Compaction failed"), "{failed}");
    let end = transcript_ends_on(&format!("{notice}\n{failed}"));
    assert!(confirms(WallKind::Overloaded, &end), "{end:?}");
    assert!(!confirms(WallKind::Context, &end), "{end:?}");
    // The controls.
    let prompt = row_of("context", "third try");
    let bash_input = row_of("shell-mode-wall-phrase", "<bash-input>");
    let bash_stdout = row_of("shell-mode-wall-phrase", "<bash-stdout>");
    for (name, after) in [
        ("a prompt", prompt),
        ("a shell-mode command", bash_input),
        ("its output", bash_stdout),
    ] {
        let end = transcript_ends_on(&format!("{notice}\n{after}"));
        assert_eq!(end.last, Entry::Input, "{name}");
        assert!(!confirms(WallKind::Overloaded, &end), "{name}");
    }
    assert!(confirms(
        WallKind::Context,
        &transcript_ends_on(fixture("compact-failed"))
    ));
}

/// A NOTICE THE TABLE DOES NOT NAME is classed by the `error` word Claude
/// Code wrote with it: words no phrase of the wall table holds, with
/// `rate_limit` and `billing_error` a limit's notice, `authentication_failed`
/// a lost login's, `server_error` an API error's, and (2.1.300's own words
/// for a request it refuses, 2026-10-15) `invalid_request`, `model_not_found`,
/// `account_on_hold` and `max_output_tokens` an API error's too — each
/// confirming its own class and no other; an `error` word of no class
/// confirms nothing.
#[test]
fn a_notice_the_table_does_not_name_is_classed_by_its_error_word() {
    const WORDS: &str = "Something went sideways on our end";
    assert_eq!(classify_wall(WORDS), None, "words no phrase holds");
    let notice = row_of("529", "\"isApiErrorMessage\":true");
    let from = notice.find("API Error: Repeated 529").expect("the text");
    let to = from + notice[from..].find('"').expect("its end");
    let worded = format!("{}{WORDS}{}", &notice[..from], &notice[to..]);
    for (error, kind) in [
        ("rate_limit", Some(WallKind::UsageWeekly)),
        ("billing_error", Some(WallKind::Spend)),
        ("authentication_failed", Some(WallKind::Auth)),
        ("server_error", Some(WallKind::Overloaded)),
        ("invalid_request", Some(WallKind::Overloaded)),
        ("model_not_found", Some(WallKind::Overloaded)),
        ("account_on_hold", Some(WallKind::Overloaded)),
        ("max_output_tokens", Some(WallKind::Overloaded)),
        ("no_such_kind", None),
    ] {
        let row = worded.replace(
            "\"error\":\"server_error\"",
            &format!("\"error\":\"{error}\""),
        );
        let end = transcript_ends_on(&row);
        assert!(matches!(end.last, Entry::Notice { .. }), "{error}");
        for other in every_kind() {
            assert_eq!(
                confirms(other, &end),
                kind.is_some_and(|k| same_class(other, k)),
                "{error}: {other:?}"
            );
        }
    }
}

/// The 529 fixture's last turn — its prompt, Claude Code's notice and the
/// `turn_duration` that closed it — with the notice's words `text`, its
/// `error` word, its `apiErrorStatus` (`None`: left out) and its typed
/// `apiError` (`None`: none), as 2.1.300 writes a notice of each kind.
fn notice_turn(text: &str, error: &str, status: Option<u16>, api_error: Option<&str>) -> String {
    let rows: Vec<&str> = fixture("529").lines().collect();
    let at = rows
        .iter()
        .rposition(|r| r.contains("\"isApiErrorMessage\":true"))
        .expect("the notice");
    let prompt = rows[..at]
        .iter()
        .rposition(|r| r.contains("\"type\":\"user\""))
        .expect("its prompt");
    let closed = rows[at + 1..]
        .iter()
        .find(|r| r.contains("\"turn_duration\""))
        .expect("its close");
    let mut notice = rows[at].to_string();
    let from = notice.find("API Error: Repeated 529").expect("the text");
    let to = from + notice[from..].find('"').expect("its end");
    notice = format!(
        "{}{}{}",
        &notice[..from],
        text.replace('"', "\\\""),
        &notice[to..]
    );
    notice = notice.replace(
        "\"error\":\"server_error\"",
        &format!("\"error\":\"{error}\""),
    );
    notice = match status {
        Some(code) => notice.replace(
            "\"apiErrorStatus\":529",
            &format!("\"apiErrorStatus\":{code}"),
        ),
        None => notice.replace("\"apiErrorStatus\":529,", ""),
    };
    if let Some(kind) = api_error {
        notice = notice.replace(
            "\"isApiErrorMessage\":true",
            &format!("\"isApiErrorMessage\":true,\"apiError\":\"{kind}\""),
        );
    }
    format!("{}\n{notice}\n{closed}", rows[prompt..at].join("\n"))
}

/// A TURN THAT ENDED ON CLAUDE CODE'S OWN ERROR ROW IS A WALL (2026-10-15):
/// a CLOSED notice is the wall its words name (the one table; the vendor's
/// `Run /compact` remedy for images and attachments the full context's), else the
/// wall its typed fields name — an `apiError` of 2.1.300's terminal set, an
/// `error` word of a refused request, a `billing_error`, a status the vendor
/// does not retry, an `authentication_failed` — and anything else the
/// server's, retried. Before, a
/// notice the table could not name (a model not found, an account on hold,
/// an image too large, the vendor's refusals) was no wall, and the ordinary
/// policy typed `keep going` into it hourly for good. NEGATIVE CONTROLS: the
/// vendor's own generic fallbacks (`unknown`, the bare `API Error`, `API
/// Error: terminated`) are the server's, NEVER a refused request; an
/// aborted request is no wall; the same words as the model's reply, a
/// notice whose turn is still open, and a notice a reply came after, name
/// none.
#[test]
fn a_closed_error_notice_is_the_wall_its_words_or_its_typed_fields_name() {
    let refused = |code| {
        Some(WallKind::ApiError {
            code,
            retryable: false,
            cause: ApiCause::Request,
        })
    };
    let server = |code| {
        Some(WallKind::ApiError {
            code,
            retryable: true,
            cause: ApiCause::Server,
        })
    };
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    /// A notice's words, `error` word, status and typed `apiError`, and the
    /// wall it is.
    type Case<'a> = (
        &'a str,
        &'a str,
        Option<u16>,
        Option<&'a str>,
        Option<WallKind>,
    );
    let cases: [Case; 31] = [
        // By the words: the table's own rows.
        (MODEL, "model_not_found", None, None, refused(None)),
        (
            "You've hit your session limit · resets 3pm (America/Los_Angeles)",
            "rate_limit",
            Some(429),
            None,
            Some(WallKind::UsageSession),
        ),
        (
            "API Error: Usage credits required for 1M context · run /model to switch to standard context",
            "rate_limit",
            Some(429),
            Some("long_context_credits_required"),
            Some(WallKind::Spend),
        ),
        (
            "API Error: Claude's response exceeded the 32000 output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.",
            "max_output_tokens",
            None,
            Some("max_output_tokens"),
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Overlong,
            }),
        ),
        // By the typed `apiError`, the words naming nothing.
        (
            "Your ANTHROPIC_API_KEY belongs to a disabled organization · Unset the environment variable to use your subscription instead",
            "invalid_request",
            None,
            Some("org_disabled_credential"),
            refused(None),
        ),
        (
            "API Error: Request blocked by your organization's policy.",
            "invalid_request",
            Some(400),
            Some("dlp_request_denied"),
            refused(Some(400)),
        ),
        (
            "PDF too large (max 100 pages, 32MB). Double press esc to go back and try again, or use pdftotext to convert to text first.",
            "invalid_request",
            None,
            Some("pdf_too_large"),
            refused(None),
        ),
        // By the `error` word.
        (
            "Image was too large. Double press esc to go back and try again with a smaller image.",
            "invalid_request",
            None,
            None,
            refused(None),
        ),
        // The model fallback's form of a model not found (2.1.300's `ZGe`):
        // the table's no-status fallback, named by its `error` word.
        (
            "API Error (claude-opus-9): model: claude-opus-9. Try /model to switch to Opus 5.5.",
            "model_not_found",
            None,
            None,
            refused(None),
        ),
        // The image past the many-image size, where `/compact` is on: its
        // own remedy is the full context's act. Where it is off, the same
        // notice's `Start a new session` is a refused request.
        (
            "An image in the conversation exceeds the dimension limit for many-image requests (2000px). Run /compact to remove old images from context, or start a new session.",
            "invalid_request",
            None,
            None,
            Some(WallKind::Context),
        ),
        (
            "An image in the conversation exceeds the dimension limit for many-image requests (2000px). Start a new session with fewer images.",
            "invalid_request",
            None,
            None,
            refused(None),
        ),
        // A request its images and attachments pushed past the API's size
        // (413, 2.1.300's measured and unmeasured forms): `/compact` is its
        // own remedy too. Where nothing removable fills it, no compaction
        // makes it fit, and it names no `/compact`: a refused request.
        (
            "Request too large (max 32 MB; 30 MB of about 33 MB is images or documents). Accumulated images and attachments in the conversation pushed the request over the limit. Run /compact, or double press esc to go back and remove attachments.",
            "invalid_request",
            Some(413),
            None,
            Some(WallKind::Context),
        ),
        (
            "Request too large (max 32 MB). Accumulated images and attachments in the conversation pushed the request over the limit. Run /compact, or double press esc to go back and remove attachments.",
            "invalid_request",
            Some(413),
            None,
            Some(WallKind::Context),
        ),
        (
            "Request too large for the API's 32 MB request limit: this conversation is about 33 MB, and none of it is images or documents that could be removed, so removing attachments or compacting cannot make it fit. Double press esc to go back past the large content, or /clear to start a new conversation.",
            "invalid_request",
            Some(413),
            None,
            refused(Some(413)),
        ),
        // A login the API refused, in the words no table names (2.1.300's
        // non-interactive form): the lost login's, never the server's.
        (
            "Failed to authenticate. API Error: 401 Invalid bearer token",
            "authentication_failed",
            Some(401),
            None,
            Some(WallKind::Auth),
        ),
        // The same with a 403 (limits: "an
        // `authentication_failed` with status 403 that no table names climbs
        // the refused-request ladder instead of typing `/login`"): the
        // vendor's typed word that the login failed decides over the status
        // range, so the lost login's `/login` answers it, never three slow
        // retries of a request the login refuses. NEGATIVE CONTROL: a 403
        // whose `error` word is no login's is the refused request.
        (
            "Failed to authenticate. API Error: 403 Forbidden",
            "authentication_failed",
            Some(403),
            None,
            Some(WallKind::Auth),
        ),
        (
            "Something new forbids this request",
            "unknown",
            Some(403),
            None,
            refused(Some(403)),
        ),
        // The same 403 drawn as the bare `API Error: <status> …` (the API's
        // own words passed on, a JSON body among them): words that name
        // nothing but the status say only that the API refused, and the typed
        // word that the LOGIN failed names which refusal (the review's
        // KR-LIM-3). NEGATIVE CONTROLS: the same words with no login's
        // `error` word are the refused request, as are a refusal a row of
        // the table names and one a terminal `apiError` names, whatever the
        // `error` word; and the status the words print stands where the
        // typed one is missing.
        (
            "API Error: 403 Forbidden",
            "authentication_failed",
            Some(403),
            None,
            Some(WallKind::Auth),
        ),
        (
            r#"API Error: 403 {"type":"error","error":{"type":"permission_error","message":"OAuth token has been revoked. Please obtain a new token."}}"#,
            "authentication_failed",
            Some(403),
            None,
            Some(WallKind::Auth),
        ),
        (
            "API Error: 403 Forbidden",
            "unknown",
            Some(403),
            None,
            refused(Some(403)),
        ),
        (
            "API Error: 403 Request blocked by your organization's data loss prevention policy.",
            "authentication_failed",
            Some(403),
            None,
            refused(Some(403)),
        ),
        (
            "API Error: 403 Forbidden",
            "authentication_failed",
            Some(403),
            Some("org_disabled_credential"),
            refused(Some(403)),
        ),
        (
            "API Error: 404 Not Found",
            "unknown",
            None,
            None,
            refused(Some(404)),
        ),
        (
            "OAuth authentication is currently not allowed for this organization.",
            "oauth_org_not_allowed",
            None,
            None,
            refused(None),
        ),
        (
            "Something about the bill",
            "billing_error",
            None,
            None,
            Some(WallKind::Spend),
        ),
        // By a status the vendor does not retry.
        (
            "API Error: something new went wrong",
            "server_error",
            Some(404),
            None,
            refused(Some(404)),
        ),
        // The Cloud gateway's sign-in (2.1.301, the residual review's R1):
        // its `run /login` past the table's opening words, its `error`
        // `unknown` and its apiError no terminal one — the lost login by its
        // words, never the server's error retried for good.
        (
            "API Error: Not signed in to the Cloud gateway — run /login.",
            "unknown",
            None,
            Some("gateway_signin_required"),
            Some(WallKind::Auth),
        ),
        (
            "API Error: Cloud gateway session expired — run /login to reconnect.",
            "unknown",
            None,
            Some("gateway_session_expired"),
            Some(WallKind::Auth),
        ),
        // The vendor's own fallbacks: the server's, retried.
        (
            "API Error: Please wait a moment and try again.",
            "unknown",
            None,
            None,
            server(None),
        ),
        ("API Error: terminated", "unknown", None, None, server(None)),
        ("API Error", "unknown", None, None, server(None)),
    ];
    for (text, error, status, api, want) in cases {
        let end = transcript_ends_on(&notice_turn(text, error, status, api));
        assert!(end.closed, "{text}: {end:?}");
        assert!(
            matches!(&end.last, Entry::Notice { text: t, .. } if t == text),
            "{text}: {end:?}"
        );
        assert_eq!(transcript_wall(&end), want, "{text}");
        assert_eq!(end.typed().as_deref(), Some(api.unwrap_or(error)), "{text}");
        if want == Some(WallKind::Auth) {
            assert!(
                confirms(WallKind::Auth, &end),
                "{text}: the screen's login confirmed"
            );
        }
    }
    // NEGATIVE CONTROLS.
    let aborted = notice_turn("API Error: Request was aborted.", "unknown", None, None);
    assert_eq!(
        transcript_wall(&transcript_ends_on(&aborted)),
        None,
        "aborted"
    );
    let notice = notice_turn(MODEL, "model_not_found", None, None);
    let reply = notice
        .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
        .replace("\"<synthetic>\"", "\"claude-opus-5-5\"");
    let end = transcript_ends_on(&reply);
    assert_eq!(end.last, Entry::Reply, "the model's own words");
    assert_eq!(transcript_wall(&end), None, "the model's own words");
    let open: Vec<&str> = notice
        .lines()
        .filter(|r| !r.contains("\"turn_duration\""))
        .collect();
    let end = transcript_ends_on(&open.join("\n"));
    assert!(!end.closed);
    assert_eq!(transcript_wall(&end), None, "a turn still open");
    let answered = format!("{notice}\n{reply}");
    assert_eq!(
        transcript_wall(&transcript_ends_on(&answered)),
        None,
        "a reply after it"
    );
}

/// THE SCREEN'S UNNAMED `API Error` NAMED BY THE NOTICE THAT CONFIRMED IT
/// (the review of 2026-10-15): an `API Error: <the server's words>` with no
/// status the table names nothing of reads on the screen as the server's,
/// retried hourly for good, and is confirmed by its notice's `error` word
/// alone. The notice's typed fields name it — a DLP policy's
/// `dlp_request_denied`, an organization's minimum version
/// (`claude_code_version_too_old`), the model fallback's `model_not_found` —
/// a refused request, on the screen's own row and words; an open notice
/// names it too (its typed fields are its own row's). NEGATIVE CONTROLS: an
/// `unknown` notice names nothing more; a wall the table named (a 500, a
/// refusal by its words) is never renamed; a notice of another class (a
/// lost login) renames nothing; an older notice than the row the screen
/// shows renames nothing; an end that is no notice names nothing.
#[test]
fn a_confirmed_unnamed_api_error_is_the_wall_its_notice_is_typed_as() {
    let server = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: ApiCause::Server,
    };
    let refused = WallKind::ApiError {
        code: None,
        retryable: false,
        cause: ApiCause::Request,
    };
    let on_screen = |text: &str| {
        let kind = classify_wall(text).expect("the screen reads a wall");
        aterm_phase::Wall {
            kind,
            message: text.to_string(),
            reset: None,
            row: 3,
            placement: aterm_phase::Placement::ErrorRow,
            under_call: false,
        }
    };
    const DLP: &str = "API Error: Request blocked by your organization's policy.";
    const OLD: &str = "API Error: Claude Code 2.1.200 is older than the minimum version required by your organization's policy.";
    const FALLBACK: &str =
        "API Error (claude-opus-9): model: claude-opus-9. Try /model to switch to Opus 5.5.";
    for (text, error, api) in [
        (DLP, "invalid_request", Some("dlp_request_denied")),
        (OLD, "invalid_request", Some("claude_code_version_too_old")),
        (FALLBACK, "model_not_found", None),
    ] {
        let w = on_screen(text);
        assert_eq!(w.kind, server, "{text}: the screen's words name nothing");
        let notice = notice_turn(text, error, None, api);
        let end = transcript_ends_on(&notice);
        assert!(confirms(w.kind, &end), "{text}");
        let named = refined(&w, &end).expect("named by its notice");
        assert_eq!(named.kind, refused, "{text}");
        assert_eq!(
            (named.row, named.message.as_str(), named.placement),
            (w.row, w.message.as_str(), w.placement),
            "{text}: the screen's own row"
        );
        let open: Vec<&str> = notice
            .lines()
            .filter(|r| !r.contains("\"turn_duration\""))
            .collect();
        let end = transcript_ends_on(&open.join("\n"));
        assert!(!end.closed);
        assert_eq!(
            refined(&w, &end).map(|n| n.kind),
            Some(refused),
            "{text}: open"
        );
    }
    // NEGATIVE CONTROLS.
    let w = on_screen(DLP);
    let unknown = transcript_ends_on(&notice_turn(DLP, "unknown", None, None));
    assert!(confirms(w.kind, &unknown));
    assert_eq!(refined(&w, &unknown), None, "an unknown names nothing more");
    let five = on_screen("API Error: 500 Internal server error");
    let end = transcript_ends_on(&notice_turn(
        "API Error: 500 Internal server error",
        "server_error",
        Some(500),
        Some("dlp_request_denied"),
    ));
    assert_eq!(refined(&five, &end), None, "a wall the table named");
    let login = transcript_ends_on(&notice_turn(DLP, "authentication_failed", Some(401), None));
    assert_eq!(refined(&w, &login), None, "another class");
    // THE GATEWAY'S SIGN-IN BY ITS TYPED FIELD (the residual review's R1): a
    // notice a later build rewords, which the table names nothing of, is the
    // lost login by its `apiError` — confirmed as the screen's unnamed `API
    // Error` and named Auth, the one class change allowed. NEGATIVE CONTROL:
    // the same words with no such `apiError` stay the server's.
    const REWORDED: &str = "API Error: The Cloud gateway needs you to sign in again.";
    let reworded = on_screen(REWORDED);
    assert_eq!(reworded.kind, server, "the words name nothing");
    let end = transcript_ends_on(&notice_turn(
        REWORDED,
        "unknown",
        None,
        Some("gateway_signin_required"),
    ));
    assert_eq!(transcript_wall(&end), Some(WallKind::Auth));
    assert!(confirms(reworded.kind, &end));
    assert_eq!(
        refined(&reworded, &end).map(|n| n.kind),
        Some(WallKind::Auth)
    );
    let end = transcript_ends_on(&notice_turn(REWORDED, "unknown", None, None));
    assert_eq!(transcript_wall(&end), Some(server));
    assert_eq!(refined(&reworded, &end), None);
    // A 403 THE SCREEN'S WORDS NAME BY ITS STATUS ALONE (the review's
    // KR-LIM-3): confirmed as the screen's refused request, its class by its
    // words, and named the lost login by the typed word that the login
    // failed. NEGATIVE CONTROLS: the same words with no login's word, and a
    // 403 whose words the table names, rename nothing.
    const FORBIDDEN: &str = "API Error: 403 Forbidden";
    let forbidden = on_screen(FORBIDDEN);
    let end = transcript_ends_on(&notice_turn(
        FORBIDDEN,
        "authentication_failed",
        Some(403),
        None,
    ));
    assert!(confirms(forbidden.kind, &end));
    assert_eq!(
        refined(&forbidden, &end).map(|n| n.kind),
        Some(WallKind::Auth)
    );
    let end = transcript_ends_on(&notice_turn(FORBIDDEN, "unknown", Some(403), None));
    assert_eq!(refined(&forbidden, &end), None, "no login's word");
    const BLOCKED: &str =
        "API Error: 403 Request blocked by your organization's data loss prevention policy.";
    let blocked = on_screen(BLOCKED);
    let end = transcript_ends_on(&notice_turn(
        BLOCKED,
        "authentication_failed",
        Some(403),
        None,
    ));
    assert_eq!(refined(&blocked, &end), None, "the table's words");
    // An older notice that is still the newest entry (a prompt command's
    // rows are passed over until its reply or notice is written): it
    // confirms the class of the screen's newer `API Error: terminated`, and
    // names nothing of a row the screen does not show.
    let terminated = on_screen("API Error: terminated");
    let older = transcript_ends_on(&notice_turn(
        DLP,
        "invalid_request",
        None,
        Some("dlp_request_denied"),
    ));
    assert!(confirms(terminated.kind, &older));
    assert_eq!(
        refined(&terminated, &older),
        None,
        "a notice the screen does not show"
    );
    let reply = notice_turn(DLP, "invalid_request", None, Some("dlp_request_denied"))
        .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
        .replace("\"<synthetic>\"", "\"claude-opus-5-5\"");
    assert_eq!(
        refined(&w, &transcript_ends_on(&reply)),
        None,
        "the model's own words"
    );
}

/// Main-chain rows of the shape Claude Code writes around a turn it started
/// itself (the keys of the trust session's rows, 2.1.267-2.1.300: a
/// `<task-notification>` prompt of origin `task-notification`, the model's
/// reply, the `turn_duration` that closes it with its pending counts),
/// written at `at` (`HH:MM:SS` on 2026-10-07).
mod carried {
    pub(super) fn notification(at: &str) -> String {
        format!(
            r#"{{"parentUuid":"p0","isSidechain":false,"type":"user","message":{{"role":"user","content":"<task-notification>\n<status>completed</status>\n</task-notification>"}},"uuid":"u1","timestamp":"2026-10-07T{at}.100Z","origin":{{"kind":"task-notification"}},"promptSource":"system","turnOrigin":"task_notification"}}"#
        )
    }

    pub(super) fn reply(at: &str) -> String {
        format!(
            r#"{{"parentUuid":"u1","isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5-5","content":[{{"type":"text","text":"I restarted the lander watch."}}]}},"uuid":"a1","timestamp":"2026-10-07T{at}.500Z"}}"#
        )
    }

    /// The `turn_duration` row, `workflows` and `agents` pending (`None`:
    /// the key absent), its `timestamp` at `at` (`None`: none).
    pub(super) fn closed(at: Option<&str>, workflows: Option<u64>, agents: Option<u64>) -> String {
        let mut row = String::from(
            r#"{"parentUuid":"a1","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":4094,"messageCount":1087,"uuid":"d1","isMeta":false"#,
        );
        if let Some(n) = workflows {
            row.push_str(&format!(r#","pendingWorkflowCount":{n}"#));
        }
        if let Some(n) = agents {
            row.push_str(&format!(r#","pendingBackgroundAgentCount":{n}"#));
        }
        if let Some(at) = at {
            row.push_str(&format!(r#","timestamp":"2026-10-07T{at}.994Z""#));
        }
        row.push('}');
        row
    }

    /// The process's launch, unix seconds, at `at` on 2026-10-07.
    pub(super) fn launch(at: &str) -> i64 {
        crate::harness::upgrade_models::parse_utc(&format!("2026-10-07T{at}Z"))
            .and_then(|s| i64::try_from(s).ok())
            .expect("a time")
    }

    pub(super) fn tail(rows: &[String]) -> String {
        rows.join("\n")
    }
}

/// BG-1's transcript half: a turn that ended waiting on background work —
/// the newest main-chain `turn_duration` counts a workflow or a background
/// agent still pending, closes the newest entry (the model's reply), was
/// written by the process launched before it, and no person's row follows
/// it — and only that. NEGATIVE CONTROLS: nothing pending (the key absent
/// or 0), a row older than the launch (a relaunched or resumed process: the
/// dead one's count), a row that names no time, a later entry (the file
/// lags the screen), a turn that ended on a notice (the wall's), a local
/// command a person ran after it. A meta row, a subagent's row and a
/// `/model`'s `system` rows after it change nothing.
#[test]
fn a_turn_that_ended_waiting_on_background_work_is_read_only_where_it_stands() {
    use carried::{closed, launch, notification, reply, tail};
    let earlier = launch("20:00:00");
    let ended = |close: String| tail(&[notification("22:53:28"), reply("22:53:30"), close]);
    assert!(waits_on_background(
        &ended(closed(Some("22:53:32"), Some(1), Some(0))),
        earlier
    ));
    assert!(waits_on_background(
        &ended(closed(Some("22:53:32"), None, Some(2))),
        earlier
    ));
    // NEGATIVE CONTROLS.
    for (close, why) in [
        (
            closed(Some("22:53:32"), Some(0), Some(0)),
            "nothing pending",
        ),
        (closed(Some("22:53:32"), None, None), "no count at all"),
        (closed(None, Some(1), None), "no time"),
    ] {
        assert!(!waits_on_background(&ended(close), earlier), "{why}");
    }
    let row = closed(Some("22:53:32"), Some(1), None);
    assert!(
        !waits_on_background(&ended(row.clone()), launch("22:53:40")),
        "written before this process was launched"
    );
    assert!(
        !waits_on_background(&ended(row.clone()), launch("22:53:32")),
        "in the launch's own second"
    );
    let lagging = tail(&[
        ended(row.clone()),
        notification("22:54:00"),
        reply("22:54:01"),
    ]);
    assert!(!waits_on_background(&lagging, earlier), "a later turn");
    let notice = r#"{"parentUuid":"u1","isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've reached your Fable limit. Run /usage-credits to continue or switch models with /model."}]},"isApiErrorMessage":true,"error":"rate_limit","uuid":"a2","timestamp":"2026-10-07T22:53:31.000Z"}"#;
    let walled = tail(&[notification("22:53:28"), notice.to_string(), row.clone()]);
    assert!(!waits_on_background(&walled, earlier), "the wall's");
    let login = r#"{"parentUuid":"d1","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/login</command-name>"},"uuid":"c1","timestamp":"2026-10-07T22:54:00.000Z"}"#;
    assert!(
        !waits_on_background(&tail(&[ended(row.clone()), login.to_string()]), earlier),
        "a person's local command"
    );
    // What changes nothing.
    let meta = r#"{"parentUuid":"d1","isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat</local-command-caveat>"},"isMeta":true,"uuid":"m1","timestamp":"2026-10-07T22:54:00.000Z"}"#;
    let subagent = r#"{"parentUuid":"s0","isSidechain":true,"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Still reviewing."}]},"uuid":"s1","timestamp":"2026-10-07T22:54:01.000Z"}"#;
    let model = r#"{"parentUuid":"d1","isSidechain":false,"type":"system","subtype":"local_command","content":"<command-name>/model</command-name>","uuid":"l1","timestamp":"2026-10-07T22:54:02.000Z"}"#;
    for after in [meta, subagent, model] {
        assert!(
            waits_on_background(&tail(&[ended(row.clone()), after.to_string()]), earlier),
            "{after}"
        );
    }
}

/// The other side of the same row (the verifier's correction 3): the turn
/// CLOSED WITH NOTHING PENDING — the newest main-chain `turn_duration`
/// closes the model's reply, this process wrote it, no person's row follows,
/// and its counts say nothing pending (absent, as Claude Code writes a zero,
/// or 0). NEGATIVE CONTROLS, each the screen's carried wait's to keep: work
/// pending, no `turn_duration` after the reply (the turn Claude Code started
/// itself and drew no row for), a row that names no time, a row older than
/// the launch, a later entry, a turn that ended on a notice, a person's
/// local command after it, an empty tail.
#[test]
fn a_turn_closed_with_nothing_pending_is_read_only_where_it_stands() {
    use carried::{closed, launch, notification, reply, tail};
    let earlier = launch("20:00:00");
    let ended = |close: String| tail(&[notification("22:53:28"), reply("22:53:30"), close]);
    for close in [
        closed(Some("22:53:32"), None, None),
        closed(Some("22:53:32"), Some(0), Some(0)),
    ] {
        let t = ended(close);
        assert!(ended_with_nothing_pending(&t, earlier), "{t}");
        assert!(!waits_on_background(&t, earlier), "{t}");
    }
    let row = closed(Some("22:53:32"), None, None);
    for (t, launched, why) in [
        (
            ended(closed(Some("22:53:32"), Some(1), None)),
            earlier,
            "a workflow pending",
        ),
        (
            ended(closed(Some("22:53:32"), None, Some(2))),
            earlier,
            "agents pending",
        ),
        (
            tail(&[notification("22:53:28"), reply("22:53:30")]),
            earlier,
            "no row closes the reply",
        ),
        (ended(closed(None, None, None)), earlier, "no time"),
        (
            ended(row.clone()),
            launch("22:53:40"),
            "written before this process was launched",
        ),
        (
            tail(&[
                ended(row.clone()),
                notification("22:54:00"),
                reply("22:54:01"),
            ]),
            earlier,
            "a later turn",
        ),
        (String::new(), earlier, "nothing read"),
    ] {
        assert!(!ended_with_nothing_pending(&t, launched), "{why}");
    }
    let notice = r#"{"parentUuid":"u1","isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've reached your Fable limit. Run /usage-credits to continue or switch models with /model."}]},"isApiErrorMessage":true,"error":"rate_limit","uuid":"a2","timestamp":"2026-10-07T22:53:31.000Z"}"#;
    let walled = tail(&[notification("22:53:28"), notice.to_string(), row.clone()]);
    assert!(!ended_with_nothing_pending(&walled, earlier), "the wall's");
    let login = r#"{"parentUuid":"d1","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/login</command-name>"},"uuid":"c1","timestamp":"2026-10-07T22:54:00.000Z"}"#;
    assert!(
        !ended_with_nothing_pending(&tail(&[ended(row), login.to_string()]), earlier),
        "a person's local command"
    );
}

/// WHICH LIMIT A NOTICE ROW NAMES (KI-1015-1 W3): its words through the wall
/// table, else its `quotaLimits.rateLimitType` — which also names the limit
/// of words that name none of their own. Read off the measured weekly row
/// (`rateLimitType` `seven_day`), its words and type changed as a model's
/// bucket's are written. NEGATIVE CONTROLS: words naming their own limit
/// keep it whatever the type says; a row of no limit and no type names none.
#[test]
fn a_limit_row_names_its_limit_by_its_words_else_its_limit_type() {
    let weekly = row_of("weekly-limit", "\"isApiErrorMessage\":true");
    let written = |words: &str, kind: &str| {
        let row = weekly
            .replace(
                "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
                words,
            )
            .replace(
                "\"rateLimitType\":\"seven_day\"",
                &format!("\"rateLimitType\":\"{kind}\""),
            );
        match transcript_ends_on(&row).last {
            Entry::Notice { text, limit, .. } => limit_named(&text, limit.as_deref()),
            other => panic!("no notice: {other:?}"),
        }
    };
    let bucket = WallKind::ModelBucket { consent: false };
    for (words, kind, want) in [
        (
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            "seven_day",
            WallKind::UsageWeekly,
        ),
        (
            "You've hit your Sonnet limit · resets 4pm (America/Los_Angeles)",
            "seven_day_sonnet",
            bucket,
        ),
        (
            "You've hit your limit · resets 4pm",
            "seven_day_opus",
            bucket,
        ),
        (
            "You've hit your limit · resets 4pm",
            "seven_day_overage_included",
            bucket,
        ),
        (
            "You've hit your limit · resets 4pm",
            "seven_day",
            WallKind::UsageWeekly,
        ),
        (
            "You've hit your limit · resets 4pm",
            "five_hour",
            WallKind::UsageSession,
        ),
        (
            "You've hit your limit · resets 4pm",
            "overage",
            WallKind::Spend,
        ),
        ("Rate limited", "seven_day_sonnet", bucket),
        // NEGATIVE CONTROLS: the words name their own limit.
        (
            "You've hit your session limit · resets 4pm (America/Los_Angeles)",
            "seven_day_sonnet",
            WallKind::UsageSession,
        ),
        (
            "You've reached your Fable limit. Run /usage-credits to continue or switch models \
             with /model.",
            "five_hour",
            bucket,
        ),
        (
            "You've hit your limit · resets 4pm",
            "not_a_type",
            WallKind::UsageSession,
        ),
    ] {
        assert_eq!(written(words, kind), Some(want), "{words} / {kind}");
    }
    assert_eq!(limit_named("Rate limited", None), None);
    assert_eq!(limit_named("Rate limited", Some("weird")), None);
    // THE WALL'S WORDS name the bucket's model where only the type does
    // (the review of LIM-4: read without it, the default fallback relaunched
    // the agent onto the very Opus bucket at its limit).
    assert_eq!(
        limit_words("You've hit your limit · resets 4pm", Some("seven_day_opus")),
        "You've hit your limit · resets 4pm \u{2014} seven_day_opus"
    );
    assert_eq!(
        crate::supervise::policy::turn_end::bucket_model(&limit_words(
            "You've hit your limit · resets 4pm",
            Some("seven_day_sonnet")
        ))
        .as_deref(),
        Some("sonnet")
    );
    // NEGATIVE CONTROLS: the words as written — they name the model, the
    // bucket's type names none, or the limit is no bucket.
    for (words, kind) in [
        (
            "You've hit your Sonnet limit · resets 4pm (America/Los_Angeles)",
            Some("seven_day_opus"),
        ),
        (
            "You've hit your limit · resets 4pm",
            Some("seven_day_overage_included"),
        ),
        ("You've hit your limit · resets 4pm", Some("seven_day")),
        (
            "You've hit your session limit · resets 4pm",
            Some("seven_day_opus"),
        ),
        ("You've hit your limit · resets 4pm", None),
    ] {
        assert_eq!(limit_words(words, kind), words, "{words} / {kind:?}");
    }
}

/// A LIMIT ONLY THE TRANSCRIPT NAMES is named as where the screen read it
/// (the integration of API-1's `transcript_found` and LIM-4): words that
/// name no limit of their own take the bucket their `rateLimitType` names,
/// that type riding the message so the fallback is never the model at its
/// limit. NEGATIVE CONTROLS: a session limit's type, words that name their
/// own limit, and a wall of another class keep the notice's kind and words.
#[test]
fn a_limit_only_the_transcript_names_takes_its_rows_type() {
    const WORDS: &str = "You've hit your limit · resets 6pm";
    assert_eq!(
        found_named(WallKind::UsageSession, WORDS, Some("seven_day_opus")),
        (
            WallKind::ModelBucket { consent: false },
            format!("{WORDS} \u{2014} seven_day_opus")
        )
    );
    assert_eq!(
        found_named(WallKind::UsageSession, WORDS, Some("five_hour")),
        (WallKind::UsageSession, WORDS.to_string())
    );
    let weekly = "You've hit your weekly limit · resets Oct 11 at 9:35am";
    assert_eq!(
        found_named(WallKind::UsageWeekly, weekly, Some("seven_day")),
        (WallKind::UsageWeekly, weekly.to_string())
    );
    let server = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: ApiCause::Server,
    };
    assert_eq!(
        found_named(server, "API Error: terminated", Some("seven_day_opus")),
        (server, "API Error: terminated".to_string())
    );
}

/// THE MODEL CHOSEN SINCE A LIMIT'S NOTICE ROW. On Claude Code 2.1.302 (the lab's `r21lebk5`) a
/// Sonnet bucket's notice, Esc on the countdown and a person's `/model opus`, its rows `user`
/// rows: the choice is the `/model`'s output after the notice row —
/// `Set model to `Opus 5.5` and …`, read as the screen draws it. NEGATIVE CONTROLS, nothing
/// chosen: the tail cut before the `/model`; a tail that does not hold the notice row (another
/// conversation's); a `/model` left with Esc (`Kept model as …`, 2.1.294's `system`
/// `local_command` rows); a `Set model to` that no `/model` printed.
#[test]
fn a_tail_names_the_model_chosen_since_a_notice() {
    use super::chosen_since_notice;
    let measured = fixture("bucket-cancelled-then-model");
    let notice = "6f0c0c14-56cf-4622-a181-27337aa0ef2c";
    assert!(
        measured
            .lines()
            .any(|l| l.contains(notice) && l.contains("Sonnet limit"))
    );
    assert_eq!(
        chosen_since_notice(measured, notice).as_deref(),
        Some("Opus 5.5 and saved as your default for new sessions")
    );
    // NEGATIVE CONTROLS.
    let cut: String = measured
        .lines()
        .take_while(|l| !(l.starts_with('{') && l.contains("<command-name>/model</command-name>")))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        chosen_since_notice(&cut, notice),
        None,
        "nothing chosen yet"
    );
    assert_eq!(
        chosen_since_notice(measured, "another-conversations-row"),
        None,
        "not this notice's tail"
    );
    let kept = fixture("session-limit-then-local-commands");
    assert_eq!(
        chosen_since_notice(kept, "88c9221d-fa0c-4816-8165-2d440ff9e619"),
        None,
        "a `/model` left with Esc chose nothing"
    );
    let unprinted = measured.replace(
        "<command-name>/model</command-name>",
        "<command-name>/usage</command-name>",
    );
    assert_eq!(
        chosen_since_notice(&unprinted, notice),
        None,
        "no `/model` printed it"
    );
}
