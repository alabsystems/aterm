// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Splits worker report rows into semantic blocks for `report --final` and `report --messages`.

use super::phase::{is_done_row, is_glyph_row, is_survey, is_tool_call};

/// What a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// The user's `❯` row and the rows it wrapped onto.
    User,
    /// The worker's words: a `⏺` message and everything under it.
    Message,
    /// A tool call or one of Claude Code's notices, and its output.
    Tool,
    /// A collapsed group of tool calls (`Ran 3 shell commands`).
    Summary,
    /// The done row that ends a turn.
    Done,
    /// A status row still in flight (a spinner, `Waiting for …`).
    Status,
}

/// One block: `rows[start..end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub start: usize,
    pub end: usize,
}

/// Which rows `report` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// Every row (the default: the report unchanged).
    #[default]
    All,
    /// `--final`: the worker's last message block, then the done row after it.
    Final,
    /// `--messages`: every user and message block and every done row, without tool output.
    Messages,
}

impl View {
    /// The header's `view=` word (`All` prints none).
    pub const fn name(self) -> &'static str {
        match self {
            View::All => "all",
            View::Final => "final",
            View::Messages => "messages",
        }
    }
}

/// Claude Code's notices and tool rows that carry no `(` after their name.
const NOTICE_HEADS: &[&str] = &[
    "Background command \"",
    "Dynamic workflow \"",
    "Task Output",
    "Stop Task",
    "Update Todos",
    "Agent \"",
    "Shell \"",
    "Monitor \"",
];

/// The verbs of a collapsed tool group (`Ran 3 shell commands, read 1 file`),
/// finished and still running (`Running 3 shell commands · 9m 13s…`).
const GROUP_VERBS: &[&str] = &[
    "ran",
    "read",
    "listed",
    "searched",
    "edited",
    "wrote",
    "updated",
    "created",
    "fetched",
    "found",
    "called",
    "used",
    "viewed",
    "loaded",
    "queried",
    "recalled",
    "saved",
    "deleted",
    "running",
    "reading",
    "searching",
    "listing",
    "editing",
    "writing",
    "updating",
    "creating",
    "fetching",
];

/// Splits `rows` into ordered semantic [`Block`]s.
pub fn blocks(rows: &[String]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut open: Option<Block> = None;
    for i in 0..rows.len() {
        match head_kind(rows, i) {
            Some(kind) => {
                if let Some(b) = open.take() {
                    out.push(Block { end: i, ..b });
                }
                open = Some(Block {
                    kind,
                    start: i,
                    end: i + 1,
                });
            }
            None if open.is_none() => {
                open = Some(Block {
                    kind: BlockKind::Tool,
                    start: i,
                    end: i + 1,
                });
            }
            None => {}
        }
    }
    if let Some(b) = open {
        out.push(Block {
            end: rows.len(),
            ..b
        });
    }
    if let Some(first) = out.first_mut()
        && head_kind(rows, first.start).is_none()
        && rows[first.start..first.end]
            .iter()
            .find(|r| !r.trim().is_empty())
            .is_some_and(|r| is_prose_row(r))
    {
        first.kind = BlockKind::Message;
    }
    out
}

/// A row indented exactly two columns that is not tool output: a message's
/// wrapped row.
fn is_prose_row(row: &str) -> bool {
    row.strip_prefix("  ")
        .is_some_and(|t| !t.starts_with(' ') && !t.starts_with(GUTTER) && !is_group_row(row))
}

/// The glyph a tool's output hangs under.
const GUTTER: char = '⎿';

/// The kind of block row `i` opens, or `None` when it continues the one above,
/// by Claude Code's grammar (the module doc).
fn head_kind(rows: &[String], i: usize) -> Option<BlockKind> {
    let row = rows[i].as_str();
    if !row.starts_with(['⏺', '●']) && is_tool_call(row) {
        return Some(BlockKind::Tool);
    }
    if row
        .strip_prefix('❯')
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    {
        return Some(BlockKind::User);
    }
    if row.starts_with('⏺') || (row.starts_with('●') && !is_survey(row)) {
        return Some(if is_tool_head(rows, i) {
            BlockKind::Tool
        } else {
            BlockKind::Message
        });
    }
    if is_glyph_row(row) {
        return Some(if is_done_row(row) {
            BlockKind::Done
        } else {
            BlockKind::Status
        });
    }
    is_group_row(row).then_some(BlockKind::Summary)
}

/// Whether the `⏺` row at `i` is a tool call or a notice, not the worker's words. A call is
/// what [`is_tool_call`] reads, so a report and the wall placement agree on whose words a `⎿`
/// block holds.
fn is_tool_head(rows: &[String], i: usize) -> bool {
    let text = rows[i].trim_start_matches(['⏺', '●']).trim_start();
    if NOTICE_HEADS.iter().any(|h| text.starts_with(h)) || is_tool_call(&rows[i]) {
        return true;
    }
    rows.get(i + 1)
        .is_some_and(|next| next.trim_start().starts_with('⎿'))
        && !text.trim_end().ends_with(['.', '!', '?', ':'])
}

/// A collapsed tool group: two spaces, then clauses joined by `, ` — each a
/// verb, an optional `for`, a count and one to three lower-case words
/// (`Ran 3 shell commands`, `searched for 1 pattern`) or a verb and
/// `memories` — the first capitalised, and an optional `(ctrl+o to expand)`
/// or, while the group still runs, the ` · <elapsed>…` Claude Code ticks on
/// the end of it.
fn is_group_row(row: &str) -> bool {
    let Some(text) = row.strip_prefix("  ") else {
        return false;
    };
    if text.starts_with(' ') {
        return false;
    }
    let text = text.trim_end();
    let text = text
        .strip_suffix("(ctrl+o to expand)")
        .map_or(text, str::trim_end);
    let text = match text.strip_suffix('…').and_then(|t| t.rsplit_once(" · ")) {
        Some((head, _)) => head,
        None => text,
    };
    text.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && text.split(", ").all(is_group_clause)
}

fn is_group_clause(clause: &str) -> bool {
    let words: Vec<&str> = clause.split(' ').collect();
    let Some((verb, rest)) = words.split_first() else {
        return false;
    };
    if !GROUP_VERBS.contains(&verb.to_ascii_lowercase().as_str()) {
        return false;
    }
    let rest = match rest.split_first() {
        Some((&"for", tail)) => tail,
        _ => rest,
    };
    match rest {
        [word] => word.starts_with("memor"),
        [count, nouns @ ..] => {
            !nouns.is_empty()
                && nouns.len() <= 3
                && !count.is_empty()
                && count.chars().all(|c| c.is_ascii_digit())
                && nouns
                    .iter()
                    .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()))
        }
        [] => false,
    }
}

/// `rows` less every gutter row (`⎿`) and the rows indented past it under
/// it (five columns or more), down to a blank row or a row back at the
/// text's indent.
fn without_gutter(rows: &[String]) -> Vec<String> {
    let deeper = 5;
    let mut out = Vec::new();
    let mut under = false;
    for row in rows {
        let t = row.trim_start();
        if t.starts_with(GUTTER) {
            under = true;
            continue;
        }
        let lead = row.chars().take_while(|c| c.is_whitespace()).count();
        if under && !t.is_empty() && lead >= deeper {
            continue;
        }
        under = false;
        out.push(row.clone());
    }
    out
}

/// `rows` without blank rows at either end.
fn trimmed(rows: Vec<String>) -> Vec<String> {
    let Some(start) = rows.iter().position(|r| !r.trim().is_empty()) else {
        return Vec::new();
    };
    let end = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .map_or(start, |e| e + 1);
    rows[start..end].to_vec()
}

/// Filters `rows` according to `view`.
pub fn view_rows(rows: &[String], view: View) -> Vec<String> {
    let all = blocks(rows);
    let body = |b: &Block| trimmed(without_gutter(&rows[b.start..b.end]));
    match view {
        View::All => rows.to_vec(),
        View::Final => {
            let Some(last) = all.iter().rposition(|b| b.kind == BlockKind::Message) else {
                return Vec::new();
            };
            let mut out = body(&all[last]);
            if let Some(done) = all[last + 1..].iter().find(|b| b.kind == BlockKind::Done) {
                out.push(String::new());
                out.push(rows[done.start].clone());
            }
            out
        }
        View::Messages => {
            let mut out: Vec<String> = Vec::new();
            for b in &all {
                let kept = match b.kind {
                    BlockKind::User | BlockKind::Message => body(b),
                    BlockKind::Done => vec![rows[b.start].clone()],
                    BlockKind::Tool | BlockKind::Summary | BlockKind::Status => continue,
                };
                if kept.is_empty() {
                    continue;
                }
                if !out.is_empty() {
                    out.push(String::new());
                }
                out.extend(kept);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
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

    /// ONE READER OF "A CALL" (H6-HARNESS-1, 2026-10-08): every row
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
}
