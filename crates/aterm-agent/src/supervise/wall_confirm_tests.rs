// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transcript's half of H6-HARNESS-1 on what Claude Code 2.1.294 wrote
//! (`supervise/fixtures/claude-2.1.294-transcript-*.jsonl`, measured in
//! `tools/real-claude-lab` on 2026-10-08).

use aterm_phase::{ApiCause, WallKind, classify_wall};

use super::{Entry, TranscriptEnd, confirms, transcript_ends_on};

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

/// EVERY WALL THE POLICY ACTS ON, AS CLAUDE CODE 2.1.294 WROTE IT: the
/// transcript ends on its own notice row — the `error` word measured with it
/// — and that row confirms the wall the screen drew (read from the screen's
/// words through the one table, `classify_wall`): the 529, a 500, the API
/// unreachable, the full context (the screen's `Context limit reached`, the
/// row's `Prompt is too long`) after a prompt and after a tool's output, a
/// `/compact` that failed (its local command's error row), no login and a
/// refused key, the session limit (and its auto-continue row) and the weekly
/// one, an API key's 429 — and the session limit met on a relaunched Claude
/// Code's first turn, whose prompt row the file holds AFTER the notice (the
/// conversation's first row, `parentUuid: null`: read as opening it, the
/// one exception to the file's order). And A DIFFERENT KIND: each confirms
/// no wall of another class — a 529's row is no lost login, a limit's no
/// full context.
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

/// WHAT A TOOL'S OUTPUT LEAVES confirms no wall, of any kind: a Bash call
/// whose output opens `Not logged in · Please run /login`, a shell-mode `!
/// cat` of the same words, an MCP result of three blocks (a context wall's
/// words, then a login wall's) and one whose first block of 60 rows scrolls
/// its call off the screen, each with the reply after it empty, so the
/// screen ends on the output — the screen's half reads the two MCP ones as
/// walls — and the transcript on the model's (empty) reply, the turn
/// closed. The same Bash and MCP shapes with the follow-up request too long
/// — the real wall, the same screen as the MCP spoof but one block shorter —
/// are confirmed (the controls).
#[test]
fn a_tools_output_confirms_no_wall() {
    for name in [
        "bash-output-wall-phrase",
        "shell-mode-wall-phrase",
        "mcp-wall-phrase",
        "mcp-long-wall-phrase",
    ] {
        let end = transcript_ends_on(fixture(name));
        assert_eq!(
            end,
            TranscriptEnd {
                last: Entry::Reply,
                closed: true
            },
            "{name}"
        );
        for kind in every_kind() {
            assert!(!confirms(kind, &end), "{name}: {kind:?}");
        }
    }
    for name in ["bash-output-then-context", "mcp-output-then-context"] {
        let end = transcript_ends_on(fixture(name));
        assert!(confirms(WallKind::Context, &end), "{name}: {end:?}");
    }
}

/// The transcript read before it caught up with the screen — cut before its
/// notice and its `turn_duration` were written — is LAGGING (read again
/// before the wall is refused): at the prompt, at the model's tool call
/// (most likely the tail the lab's hosted loop refused a real wall after an
/// MCP call on, before this rule), at the tool's result, and nothing at all.
/// Not lagging: a notice (its `turn_duration` not written yet or written),
/// and any end a `turn_duration` closed — a reply, an input. A torn first
/// line (a tail cut mid-row) is passed over.
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
    assert_eq!(
        closed,
        TranscriptEnd {
            last: Entry::Input,
            closed: true
        }
    );
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

/// Rows that are no entry of the conversation's pass over the notice before
/// them: a subagent's own notice (`isSidechain`) is not the session's wall;
/// a local command that is not a failed compaction (`/model`'s output,
/// Claude Code's meta caveat), Claude Code's meta nudge after an empty reply
/// (`isMeta`) and a compaction's summary (`isCompactSummary`) leave the
/// notice the newest entry — while a person's prompt, a tool's result and
/// the model's words end it. The rows are measured ones from several
/// fixtures.
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
    for (name, after) in [
        ("meta nudge", nudge),
        ("compaction summary", summary.as_str()),
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

/// THE FILE'S ORDER, NOT THE CLOCK'S: an entry written after the notice
/// ends it whatever its own `timestamp` says — a clock stepped back an hour
/// between them (the review of pass 3: read by time, the old notice stayed
/// "newest" through the step, confirming a spoof of its class and refusing
/// a real wall of another). THE ONE EXCEPTION, measured: a prompt row with
/// no parent (`parentUuid: null`, the conversation's first) written after
/// the rows that answered it opens the conversation and ends nothing — the
/// relaunched first turn's session limit stands; the same row with a parent
/// ends the notice, and so does the root row whose `parentUuid` is not
/// written null but missing; read first, the root row is the turn's input.
/// And only a prompt is so read: a notice or a reply with no parent is read
/// where the file holds it.
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
        // An hour BEFORE the notice by its own clock.
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
    // Only a PROMPT opens the conversation out of the file's order: any other
    // row with no parent is read where the file holds it — a notice after
    // the prompt confirms its wall, the model's reply after a notice ends it.
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

/// LOCAL COMMANDS ARE NO TURNS (measured on 2.1.294: the review of pass 4
/// found a real session limit refused as ending on "a user turn" after a
/// person opened `/usage`, `/login` and `/model` at it). Every row the
/// measured local commands wrote — `/model`, `/status`, `/usage`, `/mcp` and
/// an unchanged `/config` as `system` rows; a dismissed `/login`, a `/config`
/// that changed a setting and a `/compact` that compacted as `user` rows
/// (`<command-name>…`, `<local-command-stdout>Login interrupted`, `Disabled
/// auto-compact`, `Compacted`, the bare `/compact` and `/compact keep the
/// notes`), with their caveats, a `compact_boundary` and a summary, and the
/// `user` row a prompt command writes first (`/doctor`'s, opening
/// `<command-message>doctor</command-message>`, measured by the review of
/// pass 5) — is passed over, alone and all together, after a notice of every
/// class: the notice stays the newest entry, and confirms its wall — and so
/// the review's own case, measured whole (a real session limit, then
/// `/usage`, `/login` dismissed, `/model`), confirms the limit. NEVER A
/// NOTICE: a `user` row of a local command's output in a full context's words
/// (`<local-command-stderr>Compaction failed · …`) leaves a 529's notice
/// standing and confirms no context wall. CONTROLS: the person's prompt and a
/// shell-mode `!` command's rows (`<bash-input>…`, `<bash-stdout>…`, as
/// measured) end the notice; the `system` row of a failed `/compact` is the
/// context wall's notice.
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
            assert_eq!(end, before, "{name}: {row}");
        }
    }
    // The review of pass 4's case, measured whole: a real session limit,
    // then a person's `/usage`, `/login` dismissed and `/model` — the limit
    // is confirmed.
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
/// `rate_limit` a limit's notice, `authentication_failed` a lost login's,
/// `server_error` an API error's — each confirming its own class and no
/// other; an `error` word of no class (`invalid_request`) confirms nothing.
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
        ("authentication_failed", Some(WallKind::Auth)),
        ("server_error", Some(WallKind::Overloaded)),
        ("invalid_request", None),
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
