// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn rows(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn kinds(v: &[&str]) -> Vec<(BlockKind, usize)> {
    let r = rows(v);
    blocks(&r).iter().map(|b| (b.kind, b.start)).collect()
}

/// A message, a call, a notice, a tool shown by its description, a group under a message
/// row, a done row and a status row.
#[test]
fn each_head_opens_its_kind_of_block() {
    use BlockKind::*;
    assert_eq!(
        kinds(&[
            "❯ Go on",
            "  with the rest",
            "⏺ The design note checks out.",
            "  Ran 1 shell command",
            "",
            "⏺ Workflow(Read-only inventory of every clock-driven decision",
            "  ⎿  Running in background · /workflows to monitor",
            "⏺ Task Output b59udxsuy",
            "  ⎿  build start 23:24:13 at 0b69c5b76, free 35 GiB",
            "     … +5 lines",
            "⏺ Step 1: screening 400 official CNFs on Mac; checking inbox & machine",
            "  ⎿  $ tail -5 log.txt",
            "⏺ Background command \"Build it\" failed with exit code 144",
            "⏺ Found a negative control: with the flag, a loaded run differs.",
            "  Searched for 1 pattern, read 1 file, ran 4 shell commands",
            "  Running 3 shell commands · 9m 13s…",
            "✻ Waiting for 1 dynamic workflow to finish",
            "✻ Cooked for 23m 0s · done 11:24 PM",
        ]),
        [
            (User, 0),
            (Message, 2),
            (Summary, 3),
            (Tool, 5),
            (Tool, 7),
            (Tool, 10),
            (Tool, 12),
            (Message, 13),
            (Summary, 14),
            (Summary, 15),
            (Status, 16),
            (Done, 17),
        ]
    );
}

/// A message's own rows are never heads: wrapped rows, a paragraph after a blank row, prose
/// that starts like a group, a call-shaped word in a sentence, a bullet, a table.
#[test]
fn a_messages_rows_stay_in_its_block() {
    for row in [
        "  Read 3 files and found the bug.",
        "  Ran the suite twice.",
        "  - Sets B and C: key interning runs in the plain arm",
        "  │ site      │ status │",
        "  Step 1: screen (no timing).",
        "",
        "      let replay = entry.clone();",
    ] {
        let r = rows(&["⏺ Two things:", row]);
        assert_eq!(blocks(&r).len(), 1, "{row:?}");
    }
    assert_eq!(
        kinds(&["⏺ Seed 5 (240 vars, 1,128 clauses) is the sensitive case."]),
        [(BlockKind::Message, 0)]
    );
    assert_eq!(
        kinds(&["⏺ Reading the ghost-key site first.", "  ⎿  Read 40 lines"]),
        [(BlockKind::Message, 0)]
    );
}

/// The two views on one turn ([`view_rows`]).
#[test]
fn the_views_keep_what_was_said() {
    let r = rows(&[
        "❯ /model",
        "  ⎿  Set model to Opus",
        "",
        "⏺ First words.",
        "",
        "  Ran 2 shell commands",
        "",
        "⏺ Bash(cargo test)",
        "  ⎿  ok",
        "",
        "⏺ Last words:",
        "",
        "  - a bullet",
        "",
        "✻ Waiting for 1 dynamic workflow to finish",
        "",
        "✻ Cooked for 4s · done 2:41 PM",
        "",
        "⏺ Background command \"x\" completed (exit code 0)",
        "",
        "✻ Churned for 0s · done 2:50 PM",
    ]);
    assert_eq!(
        view_rows(&r, View::Final),
        rows(&[
            "⏺ Last words:",
            "",
            "  - a bullet",
            "",
            "✻ Cooked for 4s · done 2:41 PM"
        ])
    );
    assert_eq!(
        view_rows(&r, View::Messages),
        rows(&[
            "❯ /model",
            "",
            "⏺ First words.",
            "",
            "⏺ Last words:",
            "",
            "  - a bullet",
            "",
            "✻ Cooked for 4s · done 2:41 PM",
            "",
            "✻ Churned for 0s · done 2:50 PM",
        ])
    );
    assert_eq!(view_rows(&r, View::All), r);
    let tools = rows(&["⏺ Bash(ls)", "  ⎿  a", "  Ran 1 shell command"]);
    assert!(view_rows(&tools, View::Final).is_empty());
    assert!(view_rows(&tools, View::Messages).is_empty());
}

/// ONE READER OF "A CALL": every row
/// aterm-phase's [`super::super::phase::is_tool_call`] takes for a call
/// opens a tool block here — the rows Claude Code 2.1.294 draws for an
/// MCP call (verbose output) and for a shell-mode command among them,
/// measured in the real-Claude lab — so `--messages` never prints a
/// command's output as anybody's words.
#[test]
fn the_calls_the_phase_reader_reads_open_tool_blocks() {
    for row in [
        "⏺ Bash(cargo test)",
        "⏺ mcp__github__get_issue(issue_number: 7)",
        "⏺ github - get_issue (MCP)(owner: \"acme\", issue_number: 7)",
        "⏺ github - ping (MCP)",
        "⏺ ticket desk - get_issue (MCP)(owner: \"acme\")",
        "⏺ Web Search(\"aterm\")",
        "⏺ Claude in Chrome[get_page_text](tabId: 1)",
        "! cat notes.txt",
        "❯ !cat notes.txt",
    ] {
        assert!(super::super::phase::is_tool_call(row), "{row}");
        assert_eq!(
            kinds(&["⏺ Done.", row, "  ⎿  out"]),
            [(BlockKind::Message, 0), (BlockKind::Tool, 1)],
            "{row}"
        );
    }
    let r = rows(&[
        "❯ check the notes",
        "",
        "! cat notes.txt",
        "  ⎿  Not logged in · Please run /login",
        "",
        "⏺ Read them.",
    ]);
    assert_eq!(
        view_rows(&r, View::Messages),
        rows(&["❯ check the notes", "", "⏺ Read them."])
    );
}

/// Rows above the first head: a message's tail (its head scrolled off)
/// is the worker's words; a tool's output is not.
#[test]
fn the_rows_above_the_first_head_are_judged_by_their_indent() {
    let r = rows(&[
        "  so they cannot serve the counter gate.",
        "  Waiting for your go.",
        "✻ Sautéed for 50m 35s · done 10:57 AM",
    ]);
    assert_eq!(
        view_rows(&r, View::Final),
        r[..2]
            .iter()
            .cloned()
            .chain(["".to_string(), r[2].clone()])
            .collect::<Vec<_>>()
    );
    let r = rows(&["     … +5 lines", "", "⏺ Next."]);
    assert_eq!(view_rows(&r, View::Messages), rows(&["⏺ Next."]));
}
