// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A report's rows as the worker's program draws them: blocks, each opened by
//! a head row, so `report --final` and `report --messages` can print what the
//! worker said without what its tools did.
//!
//! Claude Code's head row (the grammar of every program but Codex), found by
//! what it starts with:
//! * `❯` in column 0, then whitespace: the user's turn ([`BlockKind::User`]);
//! * `⏺` (or `●`; never the session survey) in column 0: the worker's message
//!   ([`BlockKind::Message`]) or a tool row ([`BlockKind::Tool`]): a call as
//!   the phase reader reads one ([`is_tool_call`]: `⏺ Bash(…`, one to three
//!   capitalised words run into `(`, an MCP tool `⏺ <server> - <title>
//!   (MCP)(…)`, an integration's `⏺ Claude in Chrome[<tool>](…)`), one of
//!   Claude Code's notices (`NOTICE_HEADS`), or a head whose `⎿` output is
//!   on the next row and which does not end like a sentence;
//! * a shell-mode command in column 0 (`! cmd`, or `❯ !cmd`): a tool row, its
//!   output under it, never the person's words;
//! * a spinner glyph in column 0: a done row ending a turn
//!   ([`BlockKind::Done`]) or a status row still in flight
//!   ([`BlockKind::Status`]);
//! * two spaces, then a collapsed tool group (`Ran 3 shell commands`, `Read 1
//!   file, ran 2 shell commands`; [`BlockKind::Summary`]).
//!
//! Every other row belongs to the block above it. Rows above the first head
//! are a block of their own: the worker's words when indented two columns
//! like a message's, a tool's output otherwise.
//!
//! **CODEX** draws its transcript in a grammar of its own, and a report of a
//! Codex worker is split by it ([`aterm_phase::codex::transcript_head`], the
//! reader's): `›` and a space a user block; a `•` block the agent's words, or
//! a tool or notice block (its head, or the `└` output right under it); `✔`
//! and `✗` an approval's echo, and a card (a `╭` corner, or the session card
//! drawn with no box: `  >_ OpenAI Codex (v0.163.0)` and the greeting under
//! it), a tool block too; an end row (`1:39 PM`, `Worked for 1s • 7:14 AM`,
//! at column 2) or a `■` block (an interrupt, an error, a wall: its rows wrap
//! to column 0) a done block; a status row, or an empty composer an archived
//! frame left (`› Ask Codex to do anything`, its footer under it), a status
//! block. Its gutter is `└`, and a done block that is a `■` block keeps every
//! row the block owns ([`end_block_rows`]: never a tip Codex drew right under
//! it).

use aterm_phase::Program;
use aterm_phase::codex::{Head, end_block_rows, transcript_head};

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
    /// `--messages`: every user and message block and every done row — no
    /// tool row, and no `⎿` output under any row.
    Messages,
}

impl View {
    /// The header's `view=` word (`All` prints none).
    pub fn name(self) -> &'static str {
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

/// Split `rows`, read by `program`'s grammar (Claude Code's for any program
/// but Codex), into blocks, in order; every row is in exactly one.
pub fn blocks(rows: &[String], program: Program) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut open: Option<Block> = None;
    for i in 0..rows.len() {
        match head_kind(rows, i, program) {
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
    // The rows above the first head: the worker's words when they open like a
    // message's wrapped rows do.
    if let Some(first) = out.first_mut()
        && head_kind(rows, first.start, program).is_none()
        && rows[first.start..first.end]
            .iter()
            .find(|r| !r.trim().is_empty())
            .is_some_and(|r| is_prose_row(r, program))
    {
        first.kind = BlockKind::Message;
    }
    out
}

/// A row indented exactly two columns that is not tool output: a message's
/// wrapped row (Codex's: none of its `└` output either).
fn is_prose_row(row: &str, program: Program) -> bool {
    row.strip_prefix("  ").is_some_and(|t| {
        !t.starts_with(' ')
            && !t.starts_with(gutter(program))
            && match program {
                Program::Codex => true,
                Program::Claude | Program::Agy | Program::Muse | Program::Generic => {
                    !is_group_row(row)
                }
            }
    })
}

/// The glyph `program`'s tool output hangs under: `⎿` (Claude Code), `└`
/// (Codex).
fn gutter(program: Program) -> char {
    match program {
        Program::Codex => '└',
        Program::Claude | Program::Agy | Program::Muse | Program::Generic => '⎿',
    }
}

/// The kind of block row `i` opens, or `None` when it continues the one above,
/// by `program`'s grammar (the module doc).
fn head_kind(rows: &[String], i: usize, program: Program) -> Option<BlockKind> {
    match program {
        Program::Codex => transcript_head(rows, i).map(|h| match h {
            Head::User => BlockKind::User,
            Head::Said => BlockKind::Message,
            Head::Tool => BlockKind::Tool,
            Head::Status => BlockKind::Status,
            Head::End => BlockKind::Done,
        }),
        Program::Claude | Program::Agy | Program::Muse | Program::Generic => {
            claude_head_kind(rows, i)
        }
    }
}

/// [`head_kind`] by Claude Code's grammar.
fn claude_head_kind(rows: &[String], i: usize) -> Option<BlockKind> {
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

/// Whether the `⏺` row at `i` is a tool call or a notice, not the worker's
/// words (see the module doc). A call is the phase reader's
/// ([`is_tool_call`]): one reader of "a call" for both, so a report and the
/// wall placement never disagree about whose words a `⎿` block holds.
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
    // A group still running ticks: `Running 3 shell commands · 9m 13s…`.
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

/// `rows` less every gutter row (`⎿`; Codex's `└`) and the rows indented
/// past it under it (five columns or more; Codex's four: the output it hangs
/// over), down to a blank row or a row back at the text's indent.
fn without_gutter(rows: &[String], program: Program) -> Vec<String> {
    let deeper = match program {
        Program::Codex => 4,
        Program::Claude | Program::Agy | Program::Muse | Program::Generic => 5,
    };
    let mut out = Vec::new();
    let mut under = false;
    for row in rows {
        let t = row.trim_start();
        if t.starts_with(gutter(program)) {
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

/// A done block's own rows: its done row — and, for Codex's `■` block (an
/// interrupt, an error, a wall), every row the block owns: the rows it wraps
/// onto at column 0, down to the first blank one, a tip or the background
/// terminals' line ([`end_block_rows`], the rows Codex's reader reads the
/// wall from: 0.157.1 drew its `Tip: Use /statusline …` right under a usage
/// wall, no blank between).
fn done_rows(rows: &[String], b: &Block, program: Program) -> Vec<String> {
    let head = rows[b.start].clone();
    match program {
        Program::Codex if head.starts_with('■') => {
            rows[b.start..end_block_rows(rows, b.start).min(b.end)].to_vec()
        }
        Program::Codex | Program::Claude | Program::Agy | Program::Muse | Program::Generic => {
            vec![head]
        }
    }
}

/// The rows a view keeps, in order, the rows read by `program`'s grammar
/// ([`blocks`]). `All` is `rows` unchanged. `Final` is the last message block
/// (its gutter output, if any, dropped) and, when a done block follows it, a
/// blank row and that block's own rows ([`done_rows`]) — the first after it,
/// the one that ended the turn. `Messages` is every user and message block
/// (their gutter output dropped) and every done block's own rows, one blank
/// row between. Nothing kept is an empty list.
pub fn view_rows(rows: &[String], view: View, program: Program) -> Vec<String> {
    let all = blocks(rows, program);
    let body = |b: &Block| trimmed(without_gutter(&rows[b.start..b.end], program));
    match view {
        View::All => rows.to_vec(),
        View::Final => {
            let Some(last) = all.iter().rposition(|b| b.kind == BlockKind::Message) else {
                return Vec::new();
            };
            let mut out = body(&all[last]);
            if let Some(done) = all[last + 1..].iter().find(|b| b.kind == BlockKind::Done) {
                out.push(String::new());
                out.extend(done_rows(rows, done, program));
            }
            out
        }
        View::Messages => {
            let mut out: Vec<String> = Vec::new();
            for b in &all {
                let kept = match b.kind {
                    BlockKind::User | BlockKind::Message => body(b),
                    BlockKind::Done => done_rows(rows, b, program),
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
        blocks(&r, Program::Claude)
            .iter()
            .map(|b| (b.kind, b.start))
            .collect()
    }

    /// The heads as a live worker's archive draws them: a message, a call, a
    /// notice, a tool shown by its description, a collapsed group right under
    /// a message row, a done row and a status row.
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

    /// A message's own rows are never heads: its wrapped rows, a paragraph
    /// after a blank row, a sentence that starts like a group but is prose,
    /// a call-shaped word in a sentence, a bullet, a table.
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
            assert_eq!(blocks(&r, Program::Claude).len(), 1, "{row:?}");
        }
        // `⏺ Seed 5 (240 vars…` has a space before its `(`: prose.
        assert_eq!(
            kinds(&["⏺ Seed 5 (240 vars, 1,128 clauses) is the sensitive case."]),
            [(BlockKind::Message, 0)]
        );
        // A message ending like a sentence keeps its kind over a `⎿` row.
        assert_eq!(
            kinds(&["⏺ Reading the ghost-key site first.", "  ⎿  Read 40 lines"]),
            [(BlockKind::Message, 0)]
        );
    }

    /// The two views on a turn: `--final` is the last message and the done
    /// row that ended it; `--messages` every message and user block and done
    /// row, no tool, group or status row and no `⎿` output.
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
            view_rows(&r, View::Final, Program::Claude),
            rows(&[
                "⏺ Last words:",
                "",
                "  - a bullet",
                "",
                "✻ Cooked for 4s · done 2:41 PM"
            ])
        );
        assert_eq!(
            view_rows(&r, View::Messages, Program::Claude),
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
        assert_eq!(view_rows(&r, View::All, Program::Claude), r);
        // Nothing said: nothing kept.
        let tools = rows(&["⏺ Bash(ls)", "  ⎿  a", "  Ran 1 shell command"]);
        assert!(view_rows(&tools, View::Final, Program::Claude).is_empty());
        assert!(view_rows(&tools, View::Messages, Program::Claude).is_empty());
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
            view_rows(&r, View::Messages, Program::Claude),
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
            view_rows(&r, View::Final, Program::Claude),
            r[..2]
                .iter()
                .cloned()
                .chain(["".to_string(), r[2].clone()])
                .collect::<Vec<_>>()
        );
        let r = rows(&["     … +5 lines", "", "⏺ Next."]);
        assert_eq!(
            view_rows(&r, View::Messages, Program::Claude),
            rows(&["⏺ Next."])
        );
    }

    /// A CODEX screen read up to where its live zone begins, as a report holds
    /// it.
    fn codex_transcript(text: &str) -> Vec<String> {
        let rows = aterm_phase::prompt::fixtures::screen(text);
        let end = aterm_phase::codex::transcript_end(&rows);
        rows[..end].to_vec()
    }

    /// Codex's SESSION CARD as 0.158.0 on draws it, with no box (`  >_ OpenAI
    /// Codex (v0.163.0)` over its folder, then its greeting, its command list
    /// and its art; measured on 0.159.2 and 0.163.0), is a card like the boxed
    /// one, never the worker's words: a report that holds it (one opened at the
    /// top, `--since` an early mark, a start not found) keeps none of it in
    /// `--final` or `--messages`. Read as the rows above the first head, the
    /// card stood in both views as the worker's last message, and a wall's
    /// `--final` printed the card over the wall. The title alone and whole
    /// (the alt-screen archive keeps a frame's lone title over the card) is
    /// the card too. NEGATIVE CONTROL: a paragraph of the worker's that quotes
    /// the title with no folder row under it is still its words.
    #[test]
    fn a_codex_session_card_is_no_message() {
        use aterm_phase::codex::fixtures as cx;
        for text in [cx::IDLE_0_163, cx::IDLE_0_159] {
            let r = codex_transcript(text);
            assert!(
                r.iter().any(|row| row.starts_with("  >_ OpenAI Codex")),
                "the card is in the rows read"
            );
            assert!(view_rows(&r, View::Final, Program::Codex).is_empty());
            assert!(view_rows(&r, View::Messages, Program::Codex).is_empty());
        }
        let wall = codex_transcript(cx::WALL_MID_TURN_160X50_0_159);
        let words = "■ You’ve hit your usage limit. Visit https://chatgpt.com/codex/settings/usage \
                     to purchase more credits or try again later.";
        assert!(view_rows(&wall, View::Final, Program::Codex).is_empty());
        assert_eq!(
            view_rows(&wall, View::Messages, Program::Codex),
            rows(&["› TOOLTHEN429 please", "", words])
        );

        // The alt-screen archive's own copy (measured in the lab on 0.163.0,
        // 2026-10-16, `offscreen` from the session's start): a frame's lone
        // title over the card drawn whole.
        let archived = rows(&[
            "  >_ OpenAI Codex (v0.163.0)",
            "  >_ OpenAI Codex (v0.163.0)",
            "     /private/tmp/at.wZPe/proj",
            "",
            "",
            "• Hi! I am the local lab stub, not a model. (reply 1)",
            "",
            "  Worked for <1s • 1:35 PM",
        ]);
        assert_eq!(
            view_rows(&archived, View::Messages, Program::Codex),
            rows(&[
                "• Hi! I am the local lab stub, not a model. (reply 1)",
                "",
                "  Worked for <1s • 1:35 PM",
            ])
        );

        let quoted = rows(&[
            "  >_ OpenAI Codex (v0.163.0) is the build this folder runs.",
            "  Its card names the folder too.",
            "",
            "  Worked for 2s • 9:00 AM",
        ]);
        assert_eq!(
            view_rows(&quoted, View::Final, Program::Codex),
            rows(&[
                "  >_ OpenAI Codex (v0.163.0) is the build this folder runs.",
                "  Its card names the folder too.",
                "",
                "  Worked for 2s • 9:00 AM",
            ])
        );
    }

    /// A Codex `■` block keeps the rows it OWNS in `--final` and `--messages`
    /// — the rows it wraps onto at column 0 — never a tip Codex drew right
    /// under it with no blank between (0.157.1 under a usage wall, the owner's
    /// 2026-09-27 journal: `Tip: Use /statusline …`, or `└ Tip: …`) nor the
    /// background terminals' line: the same rows Codex's reader reads the
    /// wall from. NEGATIVE CONTROL: the measured wall with nothing under it
    /// keeps both its rows.
    #[test]
    fn a_codex_walls_block_keeps_its_own_rows_and_no_tip() {
        use aterm_phase::codex::fixtures as cx;
        let plain = codex_transcript(cx::HIT_LIMIT);
        let head = plain
            .iter()
            .rposition(|r| r.starts_with('■'))
            .expect("the ■ row");
        let wall = plain[head..head + 2].to_vec();
        // HIT_LIMIT is 0.163.0's measured weekly wall, wrapped to column 0
        // after `purchase more`.
        assert!(
            wall[1].starts_with("credits or try again at Oct 17th"),
            "{wall:#?}"
        );
        let tail = |r: &[String]| -> Vec<String> {
            let kept = view_rows(r, View::Messages, Program::Codex);
            kept[kept.len().saturating_sub(3)..].to_vec()
        };
        assert_eq!(tail(&plain)[1..], wall[..], "the control");
        for under in [
            "  Tip: Use /statusline to configure which items appear in the status line.",
            "  └ Tip: Use /statusline to configure which items appear in the status line.",
            "  1 background terminal running · /ps to view · /stop to close",
        ] {
            let mut with = plain.clone();
            with.insert(head + 2, under.to_string());
            let kept = view_rows(&with, View::Messages, Program::Codex);
            assert_eq!(kept[kept.len() - 2..], wall[..], "{under}");
            assert!(!kept.iter().any(|r| r == under), "{under}: {kept:#?}");
        }
    }
}
