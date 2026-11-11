// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::prompt::fixtures::{
    BOX_BASH_TOUCH, END_529, END_OFFER, GOAL_ACTIVE_SUGGESTION, composer, rows, screen,
};

fn framed(body: &[&str], footer: &str) -> Vec<String> {
    let mut r = rows(body);
    r.extend(composer(footer));
    r
}

#[test]
fn a_turn_runs_tools_by_the_call_rows_under_its_prompt() {
    let shells = screen(crate::prompt::fixtures::IDLE_SHELLS_2_1_271);
    assert!(turn_ran_tools(&shells), "its two Bash calls");
    let frame = crate::phase::composer_frame(&shells)
        .expect("the composer")
        .top;
    let mut later = shells[..frame].to_vec();
    later.extend(rows(&["❯ thanks", "", "⏺ You're welcome.", ""]));
    later.extend(shells[frame..].iter().cloned());
    assert!(
        !turn_ran_tools(&later),
        "the control: the calls are a turn before"
    );
    assert!(!turn_ran_tools(&framed(
        &["⏺ Done.", ""],
        "  ? for shortcuts"
    )));
}

#[test]
fn a_turn_opens_above_the_screen_when_no_prompt_is_drawn_over_its_composer() {
    let turn = framed(&["❯ go on", "", "⏺ Done.", ""], "  ? for shortcuts");
    assert!(!turn_opens_above(&turn), "its prompt drawn");
    assert!(turn_opens_above(&turn[1..]), "the rows under its prompt");
    assert!(
        turn_opens_above(&screen(crate::prompt::fixtures::IDLE_SHELLS_2_1_271)),
        "a screen of a turn's last calls and words"
    );
}

#[test]
fn a_turn_runs_tools_by_its_collapsed_group_row() {
    use crate::prompt::fixtures::{
        BOX_BASH_FIND_CONTINUED_2_1_308, BOX_EDIT, BOX_FETCH_2_1_287, BOX_READ_OUTSIDE_2_1_284,
        BOX_READ_OUTSIDE_2_1_292, COLLAPSED_GROUP_CONTEXT_WALL_MEASURED,
        IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED,
        STOP_HOOK_CONTEXT_WALL_MEASURED, STOP_HOOK_STOP_REASON_DETAILED_MEASURED,
        STOP_HOOKS_VERBOSE_PROMPT_ROW_MEASURED, WRAPPED_GROUP_ROW_CONTEXT_WALL,
    };
    // A turn of the measured `row` alone, under the screen above it.
    let ended_on = |text: &str, row: &str| -> Vec<String> {
        let r = screen(text);
        let at = r.iter().rposition(|x| x == row).expect("the row measured");
        let mut out = r[..at].to_vec();
        out.extend(rows(&["❯ go on", "", row, ""]));
        out.extend(composer("  ? for shortcuts"));
        out
    };
    let swapped = |rows: &[String], from: &str, to: &str| -> Vec<String> {
        let at = rows.iter().rposition(|r| r == from).expect("the group row");
        let mut out = rows.to_vec();
        out[at] = to.to_owned();
        out
    };
    for (name, text, group) in [
        ("2.1.280, no hint", BOX_EDIT, "  Read 1 file"),
        (
            "2.1.284",
            BOX_READ_OUTSIDE_2_1_284,
            "⏺ Reading 1 file… (ctrl+o to expand)",
        ),
        (
            "2.1.292",
            BOX_READ_OUTSIDE_2_1_292,
            "⏺ Reading 1 file… (ctrl+o to expand)",
        ),
        (
            "2.1.287",
            BOX_FETCH_2_1_287,
            "  Read 1 file (ctrl+o to expand)",
        ),
        (
            "2.1.294",
            COLLAPSED_GROUP_CONTEXT_WALL_MEASURED,
            "  Read 1 file (ctrl+o to expand)",
        ),
        (
            "2.1.294, an MCP server's",
            SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED,
            "  Called github (ctrl+o to expand)",
        ),
        (
            "2.1.308",
            BOX_BASH_FIND_CONTINUED_2_1_308,
            "  Listed 1 directory (ctrl+o to expand)",
        ),
        (
            "2.1.308",
            BOX_BASH_FIND_CONTINUED_2_1_308,
            "⏺ Searching for 1 pattern… (ctrl+o to expand)",
        ),
        (
            "the 2026-09-12 capture, shell commands",
            IDLE_AFTER_LIMIT_AND_MODEL_SWITCH,
            "  Ran 2 shell commands",
        ),
    ] {
        let r = ended_on(text, group);
        assert!(turn_ran_tools(&r), "{name}: {group:?}");
        for prose in [
            "⏺ Reading the parser.",
            "  Read 1 file before the rest",
            "  Read 2 files, then fixed the parser",
            "  Ran 2 shell commands.",
            "  Called github about it",
        ] {
            assert!(
                !turn_ran_tools(&swapped(&r, group, prose)),
                "the control, {name}: {prose:?} is words"
            );
        }
    }
    for (name, text, hook) in [
        (
            "2.1.294",
            STOP_HOOK_CONTEXT_WALL_MEASURED,
            "⏺ Ran 1 stop hook (ctrl+o to expand)",
        ),
        (
            "2.1.302, no hint",
            STOP_HOOK_STOP_REASON_DETAILED_MEASURED,
            "⏺ Ran 1 stop hook",
        ),
        (
            "2.1.304",
            STOP_HOOKS_VERBOSE_PROMPT_ROW_MEASURED,
            "⏺ Ran 2 stop hooks",
        ),
    ] {
        assert!(
            !turn_ran_tools(&ended_on(text, hook)),
            "the control, {name}: a stop hook is no tool"
        );
    }
    // A reply's next row is its words, a count or not; standing alone, a noun no group counts is words too.
    for said in [
        &[
            "⏺ Nothing changed since my last report:",
            "  Ran 312 tests",
            "  The suite is green and I am waiting on review.",
        ][..],
        &["⏺ Rebuilt and reran the suite:", "  Ran 2 shell commands"],
        &[
            "⏺ Nothing changed since my last report.",
            "",
            "  Ran 312 tests",
        ],
    ] {
        let mut turn = rows(&["❯ go on", ""]);
        turn.extend(rows(said));
        turn.extend(rows(&["", "✻ Cogitated for 3s"]));
        turn.extend(composer("  ? for shortcuts"));
        assert!(!turn_ran_tools(&turn), "the control: {said:?} is words");
    }
    // TaskStop's row, as the 2026-09-12 capture draws it, its output under it.
    let r = screen(IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
    let at = r
        .iter()
        .rposition(|x| x == "⏺ Stop Task")
        .expect("the row measured");
    let mut stopped = rows(&["❯ go on", ""]);
    stopped.extend(r[at..at + 3].iter().cloned());
    stopped.extend(rows(&[
        "",
        "⏺ Stopped the rebuild.",
        "",
        "✻ Cogitated for 3s",
    ]));
    stopped.extend(composer("  ? for shortcuts"));
    assert!(turn_ran_tools(&stopped), "{:?}", &r[at..at + 3]);
    assert!(
        !turn_ran_tools(&swapped(&stopped, "⏺ Stop Task", "⏺ Stop Task now")),
        "the control: words"
    );
    let mut said = stopped.clone();
    said.drain(3..5);
    assert!(!turn_ran_tools(&said), "the control: no output under it");
    // ExitPlanMode's row hands a plan to a person: no tool run by design (2.1.300).
    let plan = screen(crate::prompt::fixtures::PLAN_READY_70_2_1_300);
    let at = plan
        .iter()
        .position(|x| x == "⏺ Exited plan mode")
        .expect("the row measured");
    let mut planned = rows(&["❯ go on", ""]);
    planned.extend(plan[at..at + 2].iter().cloned());
    planned.extend(rows(&["⏺ Tool call done.", "", "✻ Sautéed for 0s"]));
    planned.extend(composer("  ? for shortcuts"));
    assert!(!turn_ran_tools(&planned), "{planned:?}");
    // An edit of the plan file is drawn with no `(…)` too (2.1.308).
    let plan = screen(crate::prompt::fixtures::BOX_RM_TOO_MANY_2_1_308);
    let at = plan
        .iter()
        .position(|x| x == "⏺ Updated plan")
        .expect("the row measured");
    let mut edited = rows(&["❯ go on", ""]);
    edited.extend(plan[at..at + 2].iter().cloned());
    edited.extend(rows(&["", "⏺ Tool call done.", "", "✻ Churned for 0s"]));
    edited.extend(composer("  ? for shortcuts"));
    assert!(turn_ran_tools(&edited), "{:?}", &plan[at..at + 2]);
    assert!(
        !turn_ran_tools(&swapped(&edited, "⏺ Updated plan", "⏺ Updated plan ideas")),
        "the control: words"
    );
    let mut said = edited.clone();
    said.remove(3);
    assert!(!turn_ran_tools(&said), "the control: no output under it");
    // Hand-built; `Searched for` unwrapped is measured (2.1.295) in aterm-agent's
    // `claude_codes_measured_group_rows_are_a_tool_run`.
    let wrapped = screen(WRAPPED_GROUP_ROW_CONTEXT_WALL);
    assert!(
        turn_ran_tools(&wrapped),
        "the group row wrapped at its hint"
    );
    let hooked = swapped(
        &wrapped,
        "  Searched for 2 patterns, read 1 file (ctrl+o",
        "⏺ Ran 1 stop hook (ctrl+o",
    );
    assert!(
        !turn_ran_tools(&hooked),
        "the control: a stop hook is no tool"
    );
}

#[test]
fn a_turn_a_background_tasks_end_opens_ran_a_tool() {
    use crate::prompt::fixtures::{
        BOX_BASH_RM_GLOB_SUBAGENT_2_1_292, IDLE_AFTER_LIMIT_AND_MODEL_SWITCH,
        VERBOSE_BACKGROUND_COMMAND_LOGIN_WORDS_MEASURED,
    };
    // The notice's rows as measured, then the turn it opened: the program's own, no `❯` row.
    let opened = |text: &str, head: &str| -> Vec<String> {
        let r = screen(text);
        let at = r
            .iter()
            .position(|x| x.starts_with(head))
            .expect("the row measured");
        let to = (at + 1..r.len())
            .find(|&k| r[k].is_empty())
            .expect("its end");
        let mut out = rows(&["❯ go on", "", "⏺ Started it.", ""]);
        out.extend(r[at..to].iter().cloned());
        out.extend(rows(&[
            "",
            "⏺ The suite passed. Done.",
            "",
            "✻ Churned for 2s",
        ]));
        out.extend(composer("  ? for shortcuts"));
        out
    };
    for (name, text, head) in [
        (
            "2.1.304",
            VERBOSE_BACKGROUND_COMMAND_LOGIN_WORDS_MEASURED,
            "⏺ Background command \"",
        ),
        (
            "2.1.283, its description wrapped",
            include_str!("fixtures/claude-2.1.283-wall-session-goal-checkin-shell-measured.txt"),
            "⏺ Dynamic workflow \"",
        ),
        ("2.1.292", BOX_BASH_RM_GLOB_SUBAGENT_2_1_292, "⏺ Agent \""),
        (
            "2.1.304",
            include_str!("fixtures/claude-2.1.304-agents-panel-long-bash-4m57s-120col.txt"),
            "⏺ Agent \"bg probe\" is waiting",
        ),
        (
            "no version",
            IDLE_AFTER_LIMIT_AND_MODEL_SWITCH,
            "⏺ Background command \"",
        ),
        (
            "no version",
            IDLE_AFTER_LIMIT_AND_MODEL_SWITCH,
            "⏺ Dynamic workflow \"Second",
        ),
    ] {
        let r = opened(text, head);
        assert!(turn_ran_tools(&r), "{name}: {r:?}");
        let at = r.iter().position(|x| x.starts_with(head)).expect("the row");
        let mut said = r.clone();
        said[at] = said[at].replacen('"', "", 1);
        assert!(!turn_ran_tools(&said), "the control, {name}: words");
    }
    // 2.1.268's notice builders, read in its binary: every end of each head, the time and delivery its row adds after them.
    for row in [
        "⏺ Background command \"Run the suite\" failed with exit code 1",
        "⏺ Background command \"Run the suite\" failed",
        "⏺ Background command \"Run the suite\" completed",
        "⏺ Background command \"Run the suite\" completed (exit code 0) · delivered to Claude as a \
         Bash result (1,204 chars)",
        "⏺ Background command \"Run the suite\" was stopped",
        "⏺ Background command \"Run the suite\" was stopped because the system is running low on memory",
        "⏺ Monitor \"suite watch\" stream ended",
        "⏺ Monitor \"suite watch\" ended without producing output (exit 0)",
        "⏺ Monitor \"suite watch\" script failed (exit 2)",
        "⏺ Monitor \"suite watch\" stopped",
        "⏺ Agent \"probe\" failed: Unknown error · 4s",
        "⏺ Agent \"probe\" was stopped by user",
        "⏺ Agent \"probe\" stopped at its 30-turn limit (partial result; SendMessage to task-id to \
         continue) · 3m 2s",
        "⏺ Dynamic workflow \"wave\" failed: a step threw · 1m 5s",
        "⏺ Dynamic workflow \"wave\" was stopped",
        "⏺ Remote task \"review\" completed successfully",
        "⏺ Remote task \"review\" is blocked: it asks for a permission",
    ] {
        let r = framed(
            &["❯ go on", "", row, "", "⏺ Done.", "", "✻ Churned for 2s"],
            "  ? for shortcuts",
        );
        assert!(turn_ran_tools(&r), "{row:?}");
    }
    // A reply that names a task in words, an end another task's notice takes, or a Monitor's event: no tool run.
    for row in [
        "⏺ Background command \"Run the suite\" is still running.",
        "⏺ Background command \"Run the suite\" completed, so I am done.",
        "⏺ Background command \"Run the suite\" failed again, so I am rerunning it.",
        "⏺ Background command \"Run the suite\" was started earlier.",
        "⏺ Agent \"probe\" completed (exit code 0)",
        "⏺ Monitor \"suite watch\" is still quiet.",
        "⏺ Monitor event: \"24-run load test progress (baseline vs candidate), one line per run\"",
    ] {
        let r = framed(
            &["❯ go on", "", row, "", "⏺ Done.", "", "✻ Churned for 2s"],
            "  ? for shortcuts",
        );
        assert!(!turn_ran_tools(&r), "the control: {row:?}");
    }
}

#[test]
fn a_turn_runs_tools_by_any_clause_of_its_collapsed_group() {
    let turn = |group: &str| {
        framed(
            &["❯ go on", "", group, "", "⏺ Done.", "", "✻ Worked for 3s"],
            "  ? for shortcuts",
        )
    };
    // Measured, no version: a turn whose only Bash calls were git commands.
    let measured = screen(include_str!("fixtures/wait_bg17.out"));
    let git = "  Pushed to main, rebased onto origin/main";
    let at = measured
        .iter()
        .position(|r| r == git)
        .expect("the row measured");
    let mut r = measured[..at].to_vec();
    r.extend(turn(git));
    assert!(turn_ran_tools(&r), "{git:?}");
    // 2.1.268's group renderer, read in its binary: every clause it draws, the first capitalised.
    for group in [
        "  Committed 1a2b3c4, pushed to main",
        "  Committed 1a2b3c4, 5d6e7f8, pushed to main, release",
        "⏺ Committed 1a2b3c4, pushed to main (ctrl+o to expand)",
        "⏺ Edited 2 files +10 -3 (ctrl+o to expand)",
        "  Edited 1 file +3",
        "⏺ Thought for 3s, read 2 files (ctrl+o to expand)",
        "  Thought for 1m 5s, edited 1 file +3 -1, committed 1a2b3c4, pushed to main, \
         created PR #12, ran 3 shell commands",
        "  Merged feature, rebased onto origin/main",
        "  Amended commit 1a2b3c4",
        "  Cherry-picked 1a2b3c4",
        "  Merged PR #12",
        "  Enabled auto-merge on PR #12",
        "  Commented on MR !7",
        "  Ran 2 agents",
        "  Ran 1 agent",
        "⏺ Ran agent · Fix the parser, then rerun the suite (ctrl+o to expand)",
        "  Recalled 2 memories",
        "  Read 2 files, recalled 1 team memory",
        "  Called github",
        "  Called github 2 times",
        "  Called github, slack 3 times",
        "  Called 2 tools",
        "⏺ REPL'd 3 times (ctrl+o to expand)",
        "⏺ Read 1 file, REPL'd 3 times (ctrl+o to expand)",
        "⏺ Made 2 scratchpad edits +4 (ctrl+o to expand)",
        "⏺ Searched memories (ctrl+o to expand)",
        "⏺ Published (ctrl+o to expand)",
        "⏺ Thinking for 4s, running 2 shell commands… (ctrl+o to expand)",
        // A lone agent's description, its first word conjugated by the binary's UWt.
        "⏺ Reviewed the diff (ctrl+o to expand)",
        "⏺ Explored the codebase (ctrl+o to expand)",
        "  Reviewing the diff… (ctrl+o to expand)",
        "⏺ Re-ran the suite (ctrl+o to expand)",
        "⏺ Copied the fixtures, then reran the suite (ctrl+o to expand)",
        "⏺ Thought for 3s, reviewed the diff (ctrl+o to expand)",
        "⏺ Ran 1 agent (ctrl+o to expand)",
    ] {
        assert!(turn_ran_tools(&turn(group)), "{group:?}");
    }
    for words in [
        "⏺ Thought for 3s (ctrl+o to expand)",
        "  Thought for 3s",
        "⏺ Ran 2 PreToolUse hooks (12ms) (ctrl+o to expand)",
        "  Ran 2 PreToolUse hooks (12ms)",
        "  Pushed to main.",
        "  Pushed to the main branch",
        "  Pushed to main, then waited for the checks",
        "  Committed the fix, pushed to main",
        "  Committed 1a2b3c4 and pushed it",
        "  Merged the two branches",
        "  Opened PR #12 on GitHub",
        "  Ran 2 agents in parallel",
        "  Called github about it",
        "  Called the owner twice",
        "  Recalled 2 earlier sessions",
        "  Thought for a while, read 2 files",
        "  Searched memories for the lifetime",
        "  Published the report",
        "  Waiting on PR #12",
        "  Opened PR #12",
        "⏺ Ran 2 stop hooks (ctrl+o to expand)",
        "  Reviewed the diff",
        "⏺ Bg probe (ctrl+o to expand)",
        "⏺ The diff, reviewed (ctrl+o to expand)",
    ] {
        assert!(
            !turn_ran_tools(&turn(words)),
            "the control: {words:?} is words"
        );
    }
    // An error the vendor cut at 1000 characters draws the hint under it: its first word is no agent's.
    let mut cut = vec!["❯ go on".to_owned(), String::new()];
    let words = "Failed to reach the service, the route timed out again ".repeat(20);
    let mut wrapped = words
        .as_bytes()
        .chunks(100)
        .map(|c| String::from_utf8_lossy(c).into_owned());
    cut.push(format!("⏺ {}", wrapped.next().unwrap_or_default()));
    cut.extend(wrapped.map(|r| format!("  {r}")));
    cut.extend(rows(&[
        "  …",
        "  (ctrl+o to expand)",
        "",
        "✻ Worked for 3s",
    ]));
    cut.extend(composer("  ? for shortcuts"));
    assert!(!turn_ran_tools(&cut), "the control: a cut error");
    // A git or pull request clause is a tool run by what a sentence lacks, ending the turn under a reply or between messages alike.
    for (row, git) in [
        ("  Merged PR #12", true),
        ("  Created PR #12", true),
        ("  Committed 1a2b3c4", true),
        ("  Committed 1a2b3c4, pushed to main", true),
        ("  Committed abc1234, pushed to main", true),
        ("  Pushed to main, rebased onto origin/main", true),
        ("  Pushed to origin/main", true),
        ("  Merged feature/parser", true),
        ("⏺ Committed 1a2b3c4", true),
        ("  Pushed to main", false),
        ("  Merged cleanly", false),
        ("  Pushed to production", false),
        ("  Rebased onto main", false),
        ("  Committed 1a2b3c", false),
        ("  Pushed to origin/main.", false),
        ("  Committed 1a2b3c4.", false),
        ("  Created PR #12.", false),
        ("  Pushed to main, release", false),
        ("⏺ Pushed to main", false),
        ("  Waiting on PR #12", false),
        ("  Blocked on PR #12", false),
        ("  See PR #12", false),
        ("⏺ Waiting on PR #12", false),
    ] {
        let mut paragraph = vec![
            "❯ go on",
            "",
            "⏺ I can't land this until it is reviewed.",
            "",
        ];
        if row.starts_with('⏺') {
            paragraph.truncate(2);
        }
        paragraph.extend([row, "", "✻ Worked for 3s"]);
        let ends = framed(&paragraph, "  ? for shortcuts");
        assert_eq!(turn_ran_tools(&ends), git, "ending the turn: {row:?}");
        let at = paragraph.len() - 1;
        paragraph.splice(at..at, ["⏺ Landed.", ""]);
        let between = framed(&paragraph, "  ? for shortcuts");
        assert_eq!(turn_ran_tools(&between), git, "between messages: {row:?}");
    }
    // A row a reply's wrapped line runs onto, from its `⏺` row or a two-space one, is that line's: a conjugated word and the hint there are no lone agent's group.
    let full = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
                incididunt ut labore et dolore magna";
    let agent = "  Reviewed the route and it timed out again (ctrl+o to expand)";
    let tail = |lines: usize, gap: &[&str]| {
        let mut r = rows(&["❯ go on", ""]);
        r.push(format!("⏺ {full}"));
        r.extend((1..lines).map(|_| format!("  {full}")));
        r.extend(rows(gap));
        r.extend(rows(&[agent, "", "✻ Worked for 3s"]));
        r.extend(composer("  ? for shortcuts"));
        r
    };
    for lines in [1, 2] {
        assert!(
            !turn_ran_tools(&tail(lines, &[])),
            "a wrapped line's tail, the line {lines} rows before it"
        );
        assert!(
            turn_ran_tools(&tail(lines, &[""])),
            "the control: under a blank row, a group ({lines})"
        );
    }
    // A reply's next row is its words, whatever clause it reads as.
    let said = framed(
        &[
            "❯ go on",
            "",
            "⏺ Landed it:",
            "  Pushed to main, rebased onto origin/main",
            "",
            "✻ Worked for 3s",
        ],
        "  ? for shortcuts",
    );
    assert!(!turn_ran_tools(&said), "the control: a reply's next row");
}

#[test]
fn a_turns_group_read_visits_each_row_a_bounded_number_of_times() {
    let word = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
                incididunt ut labore et dolore magna";
    let full = format!("  {word}");
    assert!(
        crate::wrap::continues(word, 2, &full, crate::wrap::room(2, 120)),
        "each paragraph row wraps onto the next"
    );
    // One reply over `k` two-space rows: a paragraph each row of which is as full as the next (one wrapped line), then a list's short items.
    type Row = fn(&str, usize) -> String;
    let shapes: [(&str, Row); 2] = [
        ("a wrapped paragraph", |word, _| format!("  {word}")),
        ("a list", |_, n| format!("  - item {n}")),
    ];
    for (shape, row) in shapes {
        for k in [50, 100, 200, 400] {
            let mut turn = rows(&["❯ go on", ""]);
            turn.push(format!("⏺ {word}"));
            turn.extend((0..k).map(|n| row(word, n)));
            turn.extend(rows(&["", "✻ Worked for 3s"]));
            turn.extend(composer("  ? for shortcuts"));
            GROUP_ROWS.with(|c| c.set(0));
            assert!(!turn_ran_tools(&turn), "{shape}, {k} rows: words");
            let read = GROUP_ROWS.with(std::cell::Cell::get);
            assert!(
                read <= 3 * (k + 1),
                "{shape}: {read} rows read for {k}, linear in the rows"
            );
        }
    }
}

/// A two-space row under a vendor `⏺` notice or a dialog's paragraph is the rest of its line,
/// read in [`crate::wrap::room`] with its slack ([`wraps_onto`]). MEASURED: the 2.1.283
/// unreachable API's notice at 80 columns (`(ENOTFOUND)`) and the 2.1.304 hook's notice at 144
/// (`Usage limit …`), each its text wrapped in the pane less ten (`NOTICE_MARGIN`), and the
/// 2.1.302 MCP server dialog at 40 (`access system …`), drawn in the pane less twice its
/// column. NEGATIVE CONTROL: in the transcript's box ([`crate::wrap::transcript_room`]) each
/// row's first word fits on the row above, so a reader in that box would read each as a line
/// of its own.
#[test]
fn a_notice_or_a_dialogs_wrapped_row_is_the_rest_of_its_line() {
    for (name, capture, above) in [
        (
            "2.1.283 unreachable API notice at 80",
            include_str!("fixtures/claude-2.1.283-api-error-enotfound-80col-measured.txt"),
            "⏺ API Error: Can't reach the API server — check your internet or DNS",
        ),
        (
            "2.1.304 hook notice at 144",
            include_str!(
                "fixtures/claude-2.1.304-user-prompt-submit-hook-continue-false-long-reason-wrapped-limit-phrase.txt"
            ),
            "⏺ Operation stopped by hook: the nightly pipeline finished its integration suite \
             and the deploy gate reported back to the build bot that",
        ),
        (
            "2.1.302 MCP server dialog at 40",
            include_str!("fixtures/claude-2.1.302-mcp-server-two-40col.txt"),
            "  MCP servers may execute code or",
        ),
    ] {
        let rows = crate::prompt::fixtures::screen(capture);
        let width = crate::question::screen_width(&rows);
        let at = rows
            .iter()
            .position(|r| crate::text::trim_end(r) == above)
            .unwrap_or_else(|| panic!("{name}: the row {above:?}"));
        let row = rows[at + 1].as_str();
        assert!(wraps_onto(&rows[at], row, width), "{name}: {row:?}");
        let said = crate::text::trim(above.strip_prefix("⏺ ").unwrap_or(above));
        assert!(
            !crate::wrap::continues_exactly(
                said,
                0,
                2,
                row,
                crate::wrap::transcript_room(2, width)
            ),
            "NEGATIVE CONTROL: {name}"
        );
    }
}

#[test]
fn the_dim_suggestion_is_read_and_a_typed_draft_is_not() {
    let r = screen(GOAL_ACTIVE_SUGGESTION);
    assert_eq!(
        continuation_suggestion(&r, 2).as_deref(),
        Some("keep going")
    );
    assert_eq!(continuation_suggestion(&r, 12), None, "typed text");
    let b = screen(BOX_BASH_TOUCH);
    assert_eq!(continuation_suggestion(&b, 2), None);
    let idle = framed(&["⏺ Done.", ""], "  ? for shortcuts");
    assert_eq!(continuation_suggestion(&idle, 2), None);
}

#[test]
fn goal_active_is_read_from_the_live_zone_only() {
    assert!(goal_active(&screen(GOAL_ACTIVE_SUGGESTION)));
    let quoted = framed(
        &[
            "⏺ The indicator reads",
            "  ◎ /goal active (3h)",
            "  when a goal is armed.",
            "",
            "✻ Worked for 3s · done 9:01 AM",
            "",
        ],
        "  ? for shortcuts",
    );
    assert!(!goal_active(&quoted), "quoted in the transcript");
    assert!(!goal_active(&screen(END_OFFER)));
    for body in [
        &[
            "⏺ The manager's footer read:",
            "",
            "  ◎ /goal active (3h)",
            "",
        ][..],
        &["⏺ The manager's footer read:", "  ◎ /goal active (3h)", ""],
    ] {
        assert!(!goal_active(&framed(body, "  ? for shortcuts")), "{body:?}");
    }
    let right = format!("{}◎ /goal active (3h)", " ".repeat(99));
    let armed = framed(&["⏺ Done.", "", &right], "  ? for shortcuts");
    assert!(goal_active(&armed));
}

/// Claude Code right-aligns its goal mark as its row's rightmost item, alone or run into the
/// row's other items, as measured: 2.1.283's update hint
/// (`✔ Update installed · Restart to update◎ /goal active (1d)`) and 2.1.311's footer
/// (`⏵⏵ auto mode on (shift+tab to cycle)` … `◎ /goal active (36s)`). The mark with an item to
/// its right, or on a row short of the right edge, is no indicator.
#[test]
fn a_goal_mark_run_into_its_rows_other_items_is_read_as_their_rightmost() {
    use crate::prompt::fixtures::{
        WALL_GOAL_API_UNAVAILABLE_ECONNREFUSED_2_1_311, WRAPUP_WAITING_2_1_283,
    };
    for text in [
        WRAPUP_WAITING_2_1_283,
        include_str!("fixtures/claude-2.1.283-wall-session-goal-checkin-shell-measured.txt"),
        include_str!("fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"),
        WALL_GOAL_API_UNAVAILABLE_ECONNREFUSED_2_1_311,
    ] {
        let r = screen(text);
        assert!(goal_active(&r), "{:?}", r.first());
    }
    let hint = "✔ Update installed · Restart to update";
    let edge = |item: &str| format!("{}{item}", " ".repeat(120 - item.chars().count()));
    let spread = |left: &str, right: &str| {
        let gap = 120 - left.chars().count() - right.chars().count();
        format!("{left}{}{right}", " ".repeat(gap))
    };
    let hinted = |row: &str| {
        framed(
            &["⏺ Done.", "", "✻ Worked for 3s · done 9:01 AM", row],
            "  ? for shortcuts",
        )
    };
    let footed = |row: &str| framed(&["⏺ Done.", "", "✻ Worked for 3s · done 9:01 AM"], row);
    let run_in = edge(&format!("{hint}◎ /goal active (1d)"));
    assert!(goal_active(&hinted(&run_in)), "run into the update hint");
    assert!(
        goal_active(&hinted(&edge("◎ /goal active"))),
        "its age not drawn"
    );
    let mode = "  ⏵⏵ auto mode on (shift+tab to cycle)";
    assert!(
        goal_active(&footed(&spread(mode, "◎ /goal active (36s)"))),
        "the footer's rightmost item"
    );
    // CONTROLS: the update hint alone, the mark with an item to its right, and the mark on a
    // row short of the right edge.
    assert!(!goal_active(&hinted(&edge(hint))), "the update hint alone");
    let followed = edge(&format!("{hint}◎ /goal active (1d) · 2 shells"));
    assert!(!goal_active(&hinted(&followed)), "an item to its right");
    let short = format!("      {hint}◎ /goal active (1d)");
    assert!(!goal_active(&hinted(&short)), "short of the right edge");
    assert!(
        !goal_active(&footed(&spread(
            &format!("{mode} · ◎ /goal active (36s)"),
            "/rc active"
        ))),
        "the footer's item with another to its right"
    );
    assert!(
        !goal_active(&footed(&format!("{mode} · ◎ /goal active (36s)"))),
        "the footer's item short of the right edge"
    );
    assert!(
        !goal_active(&footed(&spread(mode, "◎ /goal active (36s) (2)"))),
        "two parentheses"
    );
}

#[test]
fn said_tail_is_the_last_message_without_its_gutter() {
    assert_eq!(
        said_tail(&screen(END_OFFER)).as_deref(),
        Some(
            "Done: 12 of 67 divisions now solve. Next: the FP-sorted binder path; want me to take that on?"
        )
    );
    assert_eq!(
        said_tail(&screen(END_529)).as_deref(),
        Some("Suites are running; I'll report the number when it lands.")
    );
    let para = framed(
        &[
            "⏺ Merged the fix.",
            "",
            "  Next I would port the reader. Shall I go on?",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(
        said_tail(&para).as_deref(),
        Some("Merged the fix.\nNext I would port the reader. Shall I go on?")
    );
    let user = framed(&["❯ carry on with the parser", ""], "  ? for shortcuts");
    assert_eq!(said_tail(&user), None);
    let tool = framed(
        &["⏺ Bash(cargo test)", "  ⎿  test result: ok", ""],
        "  ? for shortcuts",
    );
    assert_eq!(said_tail(&tool), None);
}

#[test]
fn a_message_is_whole_only_where_its_head_is_on_the_rows() {
    let whole = framed(
        &[
            "⏺ Merged it.",
            "",
            "  Nothing left: all of it is on main.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert!(said_whole(&whole));
    let cut = framed(
        &[
            "  - finding 29: details",
            "",
            "  Nothing left: all of it is on main.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert!(said_tail(&cut).is_some(), "the visible rows are read");
    assert!(!said_whole(&cut));
    let user = framed(&["❯ carry on", ""], "  ? for shortcuts");
    assert!(!said_whole(&user));
}

#[test]
fn the_turns_other_words_are_its_messages_above_the_last() {
    let turn = framed(
        &[
            "⏺ An older turn's words.",
            "",
            "❯ land the parser",
            "",
            "⏺ Merged the lexer.",
            "  NEEDS YOU: the signing credential",
            "",
            "⏺ Bash(git push)",
            "  ⎿  Everything up-to-date",
            "",
            "⏺ Pushed the docs too.",
            "",
            "⏺ Nothing left: all of it is on main.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(
        said_tail(&turn).as_deref(),
        Some("Nothing left: all of it is on main.")
    );
    assert_eq!(
        turn_said_before(&turn).as_deref(),
        Some("Merged the lexer.\nNEEDS YOU: the signing credential\n\nPushed the docs too.")
    );
    // Message by message, as the readers of a turn's needs tell them apart.
    let messages = [
        "Merged the lexer.\nNEEDS YOU: the signing credential",
        "Pushed the docs too.",
    ]
    .map(str::to_string);
    assert_eq!(turn_messages_before(&turn).as_deref(), Some(&messages[..]));
    let reader = crate::reader::ClaudeReader;
    assert_eq!(
        crate::reader::ScreenReader::turn_messages_before(&reader, &turn).as_deref(),
        Some(&messages[..])
    );
    assert_eq!(
        crate::reader::ScreenReader::turn_message_lines_before(&reader, &turn, |_, _| false)
            .map(|m| m.len()),
        Some(2)
    );
    let alone = framed(
        &[
            "⏺ An older turn's words.",
            "",
            "❯ land the parser",
            "",
            "⏺ Nothing left: all of it is on main.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(turn_said_before(&alone), None);
    let cut = framed(
        &["  - finding 29: details", "  Nothing left: on main.", ""],
        "  ? for shortcuts",
    );
    assert_eq!(turn_said_before(&cut), None);
}

#[test]
fn claudes_own_notice_rows_are_not_the_turns_earlier_words() {
    for notice in [
        &["⏺ Background command \"Build and tag the release\" completed (exit code 0)"][..],
        &[
            "⏺ Background command \"Build and tag the release\"",
            "  completed (exit code 0) · 12s",
        ],
        &["⏺ Agent \"tag and release audit\" finished · 4s"],
        &["⏺ Dynamic workflow \"deploy and tag\" completed · 5s"],
        &["⏺ Monitor event: \"Build and tag the release\""],
        &["⏺ Ran 2 stop hooks"],
        &["⏺ Updated plan", "  ⎿  /tmp/plan.md"],
        &["⏺ Ran agent · Build and tag the release (ctrl+o to expand)"],
    ] {
        let mut rows = vec!["✻ Worked for 2m 10s", ""];
        rows.extend_from_slice(notice);
        rows.extend([
            "",
            "⏺ NEEDS YOU: say \"go\" so I can rebuild the branch.",
            "",
        ]);
        let drawn = framed(&rows, "  ? for shortcuts");
        assert_eq!(turn_said_before(&drawn), None, "{notice:?}");
        assert_eq!(turn_lines_before(&drawn, |_, _| false), None, "{notice:?}");
        assert!(turn_whole(&drawn), "{notice:?}");
    }
    // Control: the agent's own earlier prose naming a background command or plan is still read.
    let prose = framed(
        &[
            "❯ go on",
            "",
            "⏺ Background command \"Build and tag the release\" completed, so I'll tag next.",
            "",
            "⏺ NEEDS YOU: say \"go\" so I can rebuild the branch.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(
        turn_said_before(&prose).as_deref(),
        Some("Background command \"Build and tag the release\" completed, so I'll tag next.")
    );
}

#[test]
fn a_turn_whose_start_is_above_the_rows_is_no_whole_turn() {
    let body = [
        "⏺ Bash(git push)",
        "  ⎿  Everything up-to-date",
        "",
        "⏺ Nothing left: all of it is on main.",
        "",
    ];
    let cut = framed(&body, "  ? for shortcuts");
    assert!(said_whole(&cut) && !turn_whole(&cut));
    for opener in ["❯ land the parser", "✻ Worked for 3m 2s"] {
        let mut rows = vec![opener, ""];
        rows.extend(body);
        let whole = framed(&rows, "  ? for shortcuts");
        assert!(turn_whole(&whole), "{opener}");
    }
    let tall = framed(
        &["  - finding 29: details", "  Nothing left: on main.", ""],
        "  ? for shortcuts",
    );
    assert!(!turn_whole(&tall));
    assert!(turn_history_stop("❯ land the parser") && turn_history_stop("✻ Worked for 3m"));
    for row in [
        "⏺ Merged the lexer.",
        "● Bash(ls)",
        "  NEEDS YOU: the key",
        "",
    ] {
        assert!(!turn_history_stop(row), "{row}");
    }
}

#[test]
fn a_message_whose_head_scrolled_off_still_has_a_tail() {
    let mut body: Vec<String> = (0..30)
        .map(|i| format!("  - finding {i}: details"))
        .collect();
    body.push(String::new());
    body.push("  Should I drop the prod table or keep it?".to_string());
    body.push(String::new());
    body.push("✻ Cooked for 3m 2s · done 4:24 PM".to_string());
    body.push(String::new());
    let refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let long = framed(&refs, "  ? for shortcuts");
    let tail = said_tail(&long).expect("a tail");
    assert!(tail.starts_with("- finding 0: details"), "{tail}");
    assert!(
        tail.ends_with("Should I drop the prod table or keep it?"),
        "{tail}"
    );
    let goal = said_tail(&screen(GOAL_ACTIVE_SUGGESTION)).expect("the goal fixture's tail");
    assert!(goal.starts_with("with zero hooks installed."), "{goal}");
    let user = framed(
        &[
            "❯ summarise",
            "  - finding 1: details",
            "  Should I drop it?",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(said_tail(&user), None);
}

#[test]
fn a_persons_interrupt_is_read_under_the_last_block_only() {
    let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    let tool = framed(
        &[
            "⏺ Running the schema migration against the staging database now.",
            "",
            "⏺ Bash(./migrate.sh --env staging)",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
        ],
        foot,
    );
    assert!(interrupted(&tool));
    let message = framed(
        &[
            "⏺ Running the schema migration against the staging database now.",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
        ],
        foot,
    );
    assert!(interrupted(&message));
    let quoted = framed(
        &[
            "⏺ The vendor prints Interrupted · What should Claude do instead? on Esc.",
            "",
        ],
        foot,
    );
    assert!(!interrupted(&quoted));
    let answered = framed(
        &[
            "⏺ Bash(./migrate.sh --env staging)",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
            "❯ run it against the scratch database instead",
            "",
            "⏺ Running it against the scratch database.",
            "",
        ],
        foot,
    );
    assert!(!interrupted(&answered));
}

#[test]
fn a_declined_question_is_a_persons_stop() {
    use crate::prompt::fixtures::{QUESTION_DECLINED_CANCEL, QUESTION_DECLINED_ESC, screen};
    for text in [QUESTION_DECLINED_CANCEL, QUESTION_DECLINED_ESC] {
        let r = screen(text);
        assert!(interrupted(&r), "{r:?}");
        assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Idle);
    }
    let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    let quoted = framed(
        &[
            "⏺ If you press Esc, Claude Code prints User declined to answer questions.",
            "",
        ],
        foot,
    );
    assert!(!interrupted(&quoted));
    let answered = framed(
        &[
            "⏺ User declined to answer questions",
            "  ⎿  · Which shell should the demo target? (zsh (Recommended) / bash)",
            "",
            "❯ use zsh",
            "",
            "⏺ Using zsh.",
            "",
        ],
        foot,
    );
    assert!(!interrupted(&answered));
}

#[test]
fn claude_codes_idle_compaction_is_its_own_housekeeping() {
    use crate::prompt::fixtures::{IDLE_COMPACTED_HINT_MEASURED, IDLE_COMPACTED_MEASURED};
    use crate::reader::Program;
    for fixture in [IDLE_COMPACTED_MEASURED, IDLE_COMPACTED_HINT_MEASURED] {
        let measured = screen(fixture);
        assert!(idle_compacted(&measured));
        assert!(Program::Claude.reader().housekeeping(&measured));
        assert_eq!(said_tail(&measured).as_deref(), Some("ATERM-TASK-DONE"));
        assert_eq!(
            crate::phase::worker_phase(&measured),
            crate::phase::Phase::Idle
        );
        for p in [Program::Generic, Program::Codex] {
            assert!(!p.reader().housekeeping(&measured), "{p:?}");
        }
    }
    let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    for body in [
        &[
            "⏺ Compacted while idle, before the prompt",
            "  cache expired",
            "",
        ][..],
        &[
            "⏺ Compacted while idle, before the prompt cache expired ·\u{a0}turn off",
            "  with \"idleCompaction\": false in settings.json",
            "",
        ],
    ] {
        assert!(idle_compacted(&framed(body, foot)), "{body:?}");
    }
    for body in [
        &[
            "⏺ Compacted while idle, before the prompt cache expired, so the next",
            "  turn starts from the summary.",
            "",
        ][..],
        &[
            "⏺ Compacted while idle, before the prompt cache expired and moved on.",
            "",
        ],
        &[
            "❯ Compacted while idle, before the prompt cache expired",
            "",
        ],
        &[
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
            "❯ keep going",
            "",
            "⏺ Nothing is left to do.",
            "",
        ],
        &[
            "⏺ Compacted after an hour idle, using a summary written while you were away",
            "",
        ],
    ] {
        assert!(!idle_compacted(&framed(body, foot)), "{body:?}");
    }
}

#[test]
fn the_last_words_are_read_through_an_idle_compaction() {
    let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    let compacted = |said: &[&str]| {
        let mut body = said.to_vec();
        body.extend([
            "",
            "✻ Worked for 57s · done 4:28 PM",
            "",
            "※ recap: You wanted the auto-prompting fixed, and that fix is now on main. Next is",
            "  your call: should I also retire the token? (disable recaps in /config)",
            "  ⎿  Read ../.claude/projects/-Users-user00-aterm/memory/notes.md (33 lines)",
            "  ⎿  Referenced file ../../../private/tmp/claude-1000/-Users-user00-aterm/scratchpad/",
            "     design-final.md",
            "  ⎿  Skills restored (workflow-authoring)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
        ]);
        framed(&body, foot)
    };
    let report = compacted(&[
        "⏺ Merged the fix; the checks pass.",
        "",
        "  ATERM-TASK-DONE",
    ]);
    assert!(idle_compacted(&report));
    assert_eq!(
        said_tail(&report).as_deref(),
        Some("Merged the fix; the checks pass.\nATERM-TASK-DONE")
    );
    let held = compacted(&["⏺ Shall I drop the old table?"]);
    assert_eq!(
        said_tail(&held).as_deref(),
        Some("Shall I drop the old table?")
    );
    let tool = compacted(&["⏺ Bash(targo --unverified test)", "  ⎿  ok"]);
    assert_eq!(said_tail(&tool), None);
    let alone = framed(
        &[
            "  ⎿  Skills restored (workflow-authoring)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
        ],
        foot,
    );
    assert!(idle_compacted(&alone));
    assert_eq!(said_tail(&alone), None);
}

/// A TASK'S ROW UNDER AN IDLE COMPACTION IS CLAUDE CODE'S, NEVER THE WORKER'S LAST WORDS (the
/// tab of 2026-10-27, whose last words read `Task "…" still running in background`). The
/// control: a message of the worker's that has not that row's shape stays its last words, one
/// that opens `Task "…"` and goes on in words the renderer never gives a status included.
#[test]
fn a_task_row_under_an_idle_compaction_is_no_last_word() {
    use crate::prompt::fixtures::IDLE_COMPACTED_OVER_A_SHELL_2_1_312;
    let measured = screen(IDLE_COMPACTED_OVER_A_SHELL_2_1_312);
    assert!(idle_compacted(&measured));
    assert!(
        said_tail(&measured).is_some_and(|said| said.starts_with("Lint and the cross-OS")),
        "{:?}",
        said_tail(&measured)
    );
    let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    let compacted = |task: &[&str]| {
        let mut body = vec![
            "⏺ Merged the fix.",
            "",
            "✻ Worked for 57s · done 4:28 PM · 1 shell still running",
            "  ⎿  Read ../.claude/projects/-Users-user00-aterm/memory/notes.md (33 lines)",
            "",
        ];
        body.extend(task);
        body.extend([
            "  ⎿  Skills restored (workflow-authoring)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
        ]);
        framed(&body, foot)
    };
    for task in [
        &["⏺ Task \"Lint the tree\" still running in background"][..],
        &["● Task \"Lint the tree\" still running in background"],
        &["⏺ Task \"Lint the tree\" completed in background"],
        &["⏺ Task \"Lint the tree\" stopped"],
        &["⏺ Task \"Lint the tree\" failed"],
        &["⏺ Task \"Lint the \"landed\" tree\" still running in background"],
        &[
            "⏺ Task \"Lint the tree and the cross cells on it\" still running in",
            "  background",
        ],
        &[
            "⏺ Task \"Lint the tree and the cross cells on the tree",
            "  that landed\" still running in background",
        ],
        &[
            "⏺ Task \"Lint the tree\" still running in background",
            "",
            "⏺ Task \"Soak the tree\" still running in background",
        ],
    ] {
        let rows = compacted(task);
        assert_eq!(
            said_tail(&rows).as_deref(),
            Some("Merged the fix."),
            "{task:?}"
        );
    }
    for (mine, said) in [
        ("⏺ Task list is done.", "Task list is done."),
        ("⏺ Task \"Lint the tree\"", "Task \"Lint the tree\""),
        ("⏺ Task \"\" stopped", "Task \"\" stopped"),
        (
            "⏺ Task \"lint\" is what I ran; it landed clean.",
            "Task \"lint\" is what I ran; it landed clean.",
        ),
        ("⏺ Task \"lint\" Failed", "Task \"lint\" Failed"),
    ] {
        assert_eq!(said_tail(&compacted(&[mine])).as_deref(), Some(said));
    }
}

#[test]
fn idle_compaction_keeps_an_indented_worker_tail() {
    let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    for tail in ["ATERM-TASK-DONE", "Should I remove the old table?"] {
        for restored in [
            &[][..],
            &["  ⎿  Read notes.md", "     wrapped attachment path"][..],
            &[
                "※ recap: The change is ready.",
                "  It is waiting for you.",
                "  ⎿  Read notes.md",
                "     wrapped attachment path",
            ][..],
        ] {
            let indented = format!("      {tail}");
            let mut body = vec!["⏺ Finished the review.", indented.as_str(), ""];
            body.extend_from_slice(restored);
            body.extend([
                "",
                "⏺ Compacted while idle, before the prompt cache expired",
                "",
            ]);
            let rows = framed(&body, foot);
            assert!(idle_compacted(&rows));
            assert_eq!(
                said_tail(&rows).as_deref(),
                Some(format!("Finished the review.\n{tail}").as_str()),
                "{restored:?}"
            );
        }
    }
    for head in ["⏺ Finished the review.", "      Finished the review."] {
        let rows = framed(
            &[
                head,
                "  ⎿  tool output",
                "",
                "      ATERM-TASK-DONE",
                "",
                "⏺ Compacted while idle, before the prompt cache expired",
                "",
            ],
            foot,
        );
        assert_eq!(
            said_tail(&rows).as_deref(),
            Some("Finished the review.\nATERM-TASK-DONE"),
            "{head}"
        );
    }
}

#[test]
fn a_launch_with_no_turn_is_fresh() {
    use crate::prompt::fixtures::{first_turn_under_a_table, launch_under_a_table};
    let launch = launch_under_a_table("❯ Try \"refactor <filepath>\"", "  ? for shortcuts");
    assert!(fresh(&launch), "{launch:#?}");
    assert!(!fresh(&first_turn_under_a_table(&["❯ then add tests"])));
    let mut answered = launch_under_a_table("❯", "  ? for shortcuts");
    let top = answered.len() - 3;
    for (k, r) in ["❯ fix the parser", "", "⏺ Fixed.", ""].iter().enumerate() {
        answered.insert(top + k, (*r).to_string());
    }
    assert!(!fresh(&answered), "{answered:#?}");
    assert!(!fresh(&framed(&["⏺ Done.", ""], "  ? for shortcuts")));
    assert!(!fresh(&screen(END_OFFER)));
}

#[test]
fn a_conversation_a_clear_just_started_is_fresh() {
    let cleared = measured("claude-2.1.300-cleared-at-armed-limit.txt");
    assert!(fresh(&cleared), "{cleared:#?}");
    let clear = cleared
        .iter()
        .position(|r| r == "❯ /clear")
        .expect("the /clear row");
    let mut asked = cleared.clone();
    asked.splice(
        clear + 1..clear + 1,
        ["", "❯ fix the parser"].map(str::to_string),
    );
    assert!(!fresh(&asked), "{asked:#?}");
    let mut usage = cleared.clone();
    usage[clear] = "❯ /usage".to_string();
    assert!(!fresh(&usage), "{usage:#?}");
}

#[test]
fn the_launch_card_over_one_message_is_the_opening_exchange() {
    let one = measured("claude-2.1.286-survey-race-0hello-measured.txt");
    assert!(opening(&one), "{one:#?}");
    let hi = one.iter().position(|r| r == "❯ hi").expect("the message");
    let mut two = one.clone();
    two.splice(
        hi..hi,
        ["❯ fix the parser", "", "⏺ Fixed.", ""].map(str::to_string),
    );
    assert!(!opening(&two), "a second message: {two:#?}");
    let mut scrolled = one.clone();
    scrolled.drain(..hi);
    assert!(!opening(&scrolled), "the card scrolled off: {scrolled:#?}");
    let compacted = measured("claude-2.1.286-compact-succeeded.txt");
    assert!(
        !opening(&compacted),
        "a reply over the one message: {compacted:#?}"
    );
    let mut kept = compacted.clone();
    let compact = kept
        .iter()
        .position(|r| r.trim_end() == "❯ /compact")
        .expect("/compact");
    kept.drain(compact..compact + 2);
    assert!(!opening(&kept), "a reply and no message: {kept:#?}");
    assert!(!opening(&measured("claude-2.1.283-launch-repl-ready.txt")));
    let mut cleared = measured("claude-2.1.300-cleared-at-armed-limit.txt");
    let clear = cleared
        .iter()
        .position(|r| r == "❯ /clear")
        .expect("/clear");
    cleared.splice(
        clear + 1..clear + 1,
        ["", "❯ fix the parser"].map(str::to_string),
    );
    assert!(opening(&cleared), "a /clear aside: {cleared:#?}");
}

fn measured(name: &str) -> Vec<String> {
    let path = aterm_tempfile::manifest_dir()
        .join("src/fixtures")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_launch_card_names_the_starting_model() {
    assert_eq!(
        launch_card(&measured("claude-2.1.283-footer-bypass-idle.txt")),
        Some(LaunchCard {
            version: "2.1.283".into(),
            model: "Opus 5.5".into(),
            effort: Some("xhigh".into()),
        })
    );
    assert!(!fresh(&measured("claude-2.1.283-footer-bypass-idle.txt")));
    assert_eq!(
        launch_card(&measured("claude-2.1.283-launch-repl-ready.txt")),
        Some(LaunchCard {
            version: "2.1.283".into(),
            model: "Haiku 4.5".into(),
            effort: None,
        })
    );
    let mut relaunch = measured("claude-2.1.283-inline-relaunch-repl-ready.txt");
    let first = relaunch
        .iter()
        .position(|r| r.contains("Claude Code v"))
        .unwrap();
    relaunch[first + 1] = "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5 \u{00B7} Claude Max".into();
    assert_eq!(
        launch_card(&relaunch).map(|c| c.model).as_deref(),
        Some("Haiku 4.5")
    );
    let mut below = measured("claude-2.1.283-launch-repl-ready.txt");
    let card: Vec<String> = below
        .iter()
        .filter(|r| r.contains("Claude Code v") || r.contains("Claude Max"))
        .cloned()
        .collect();
    below.extend(card);
    assert_eq!(launch_card(&below), None);
    assert_eq!(
        launch_card(&measured("claude-2.1.283-footer-busy.txt")),
        None
    );
    assert_eq!(
        launch_card(&measured("claude-2.1.283-effort-switch.txt")),
        None
    );
    let base = measured("claude-2.1.283-footer-bypass-idle.txt");
    let at = base.iter().position(|r| r.contains("Claude Max")).unwrap();
    for cut in [
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 with xh\u{2026}",
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 with xhigh effort",
    ] {
        let mut narrow = base.clone();
        narrow[at] = cut.into();
        assert_eq!(launch_card(&narrow), None, "{cut}");
    }
}

fn card_rows() -> Vec<String> {
    measured("claude-2.1.294-session-limit-then-local-commands.txt")[..3].to_vec()
}

fn header_version(header: &str) -> Option<&str> {
    header
        .split_once("claude-code ")?
        .1
        .split_whitespace()
        .next()
}

#[test]
fn every_recorded_card_is_placed_or_refused_by_its_layout() {
    let banner = crate::anchor!("launch.banner");
    let dir = aterm_tempfile::manifest_dir().join("src/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("a fixture").path())
        .collect();
    paths.sort();
    let (mut placed, mut narrow, mut entrance) = (0, 0, 0);
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let rows = crate::prompt::fixtures::screen(&text);
        if !rows.iter().any(|r| r.contains(banner)) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let place = launch_card_place(&rows);
        if ["-34col.txt", "-36col.txt", "-40col.txt"]
            .iter()
            .any(|narrow| name.ends_with(narrow))
            || name.contains("-narrow-")
        {
            let top = rows.iter().rposition(|r| r.contains(banner)).unwrap();
            let (text_col, mascot_end) = if name.contains("-workspace-") {
                (13, 11)
            } else {
                (11, 9)
            };
            let place = place.unwrap_or_else(|| panic!("{name}: the narrow card"));
            assert_eq!(
                (
                    place.top,
                    place.rows,
                    place.mascot_row,
                    place.text_col,
                    place.mascot_end,
                    place.ends[0]
                ),
                (top, 4, 1, text_col, mascot_end, text_col + 20),
                "{name}: the narrow card"
            );
            narrow += 1;
            continue;
        }
        if name.contains("-entrance-") && (name.contains("arms-up-") || name.contains("-dust-")) {
            assert_eq!(
                place, None,
                "{name}: the mascot's arms up, or a row low, mid-entrance"
            );
            entrance += 1;
            continue;
        }
        let place = place.unwrap_or_else(|| panic!("{name}: no card placed"));
        let newest = rows
            .iter()
            .rposition(|r| r.starts_with(" \u{2590}") && r.contains(banner))
            .unwrap();
        let (text_col, mascot_end) = if name.contains("-entrance-") && name.contains("-stand-") {
            (13, 9)
        } else if name.contains("-workspace-") {
            (13, 11)
        } else {
            (11, 9)
        };
        assert_eq!(
            (
                place.top,
                place.rows,
                place.text_col,
                place.mascot_end,
                place.ends[0]
            ),
            (newest, 3, text_col, mascot_end, text_col + 20),
            "{name}"
        );
        assert!(
            place.ends[1..].iter().all(|&e| e > text_col),
            "{name}: {place:?}"
        );
        placed += 1;
    }
    assert!(
        placed >= 150 && narrow >= 8 && entrance >= 4,
        "the corpus must exercise the reader: {placed} placed, {narrow} narrow, \
         {entrance} mid-entrance"
    );
}

#[test]
fn the_card_is_placed_where_it_was_measured() {
    for (name, top, ends) in [
        ("claude-2.1.280-box-bash-touch.txt", 1, [31, 33, 108]),
        ("claude-2.1.281-ask-multi.txt", 0, [31, 33, 49]),
        ("claude-2.1.282-question-afk.txt", 1, [31, 33, 122]),
        (
            "claude-2.1.283-api-error-enotfound-measured.txt",
            3,
            [31, 52, 112],
        ),
        ("claude-2.1.283-footer-bypass-idle.txt", 1, [31, 50, 24]),
        (
            "claude-2.1.283-ctrlc-relaunch-stale-question.txt",
            40,
            [31, 33, 60],
        ),
        (
            "claude-2.1.283-exit-relaunch-stale-footer.txt",
            25,
            [31, 33, 60],
        ),
        (
            "claude-2.1.283-inline-relaunch-repl-ready.txt",
            16,
            [31, 33, 126],
        ),
        ("claude-2.1.286-compact-failed-first.txt", 0, [31, 39, 36]),
        (
            "claude-2.1.294-session-limit-then-local-commands.txt",
            0,
            [31, 34, 36],
        ),
        ("claude-2.1.295-question-one-44col.txt", 0, [31, 39, 36]),
        ("claude-2.1.302-launch-144col.txt", 0, [31, 39, 36]),
    ] {
        assert_eq!(
            launch_card_place(&measured(name)),
            Some(CardPlace {
                top,
                rows: 3,
                mascot_row: 0,
                text_col: 11,
                mascot_end: 9,
                ends,
            }),
            "{name}"
        );
    }
    for (name, top) in [
        ("claude-2.1.302-launch-workspace-144col.txt", 0),
        ("claude-2.1.302-launch-workspace-120col.txt", 0),
        ("claude-2.1.302-launch-workspace-80col.txt", 0),
        ("claude-2.1.302-launch-workspace-61col.txt", 0),
        ("claude-2.1.302-launch-workspace-fullscreen-144col.txt", 1),
    ] {
        assert_eq!(
            launch_card_place(&measured(name)),
            Some(CardPlace {
                top,
                rows: 3,
                mascot_row: 0,
                text_col: 13,
                mascot_end: 11,
                ends: [33, 59, 38],
            }),
            "{name}"
        );
    }
}

#[test]
fn the_wand_flares_in_claudes_two_inks() {
    let off = |c: [u8; 3], by: i16| c.map(|v| (i16::from(v) + by).clamp(0, 255) as u8);
    for ink in [WAND_FLARE_DARK, WAND_FLARE_LIGHT] {
        assert!(wand_flare('\u{2726}', ink), "{ink:?}");
        let t = i16::from(WAND_FLARE_TOLERANCE);
        assert!(
            wand_flare('\u{2726}', off(ink, -t)),
            "{ink:?} within tolerance"
        );
        let mut far = ink;
        far[1] = far[1].wrapping_sub(WAND_FLARE_TOLERANCE + 1);
        assert!(!wand_flare('\u{2726}', far), "{ink:?}: a channel past it");
        for ch in ['\u{2571}', '\u{2588}', '*'] {
            assert!(!wand_flare(ch, ink), "{ch:?} in {ink:?}");
        }
    }
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    for measured in [
        0xffe18d, 0xffd77b, 0xffcc68, 0xf08048, 0xfdc364, 0xffd476, 0xffdc84, 0xffe492, 0xffeca0,
        0xaf87ff, 0x8700ff, 0xd0a264, 0xa76e27, 0xd77757,
    ] {
        assert!(
            !wand_flare('\u{2726}', rgb(measured)),
            "{measured:06x} is no flare"
        );
    }
    assert!(
        wand_flare('\u{2726}', rgb(0xfffceb)),
        "the measured dark flare"
    );
    let plain = measured("claude-2.1.302-launch-144col.txt");
    let place = launch_card_place(&plain).expect("the plain card");
    assert_eq!(place.wand(), Some((8, 0)), "the mascot's right edge");
    assert_ne!(plain[0].chars().nth(8), Some('\u{2726}'));
}

#[test]
fn the_workspace_card_is_placed_with_its_wand() {
    let card = measured("claude-2.1.302-launch-workspace-144col.txt");
    assert_eq!(
        card[0],
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}\u{2584}\u{2571}\u{2726}  Claude Code v2.1.302",
        "the measured banner row"
    );
    let place = launch_card_place(&card).expect("the workspace card");
    assert_eq!((place.top, place.text_col), (0, 13));
    assert_eq!(place.wand(), Some((10, 0)), "the wand's cell");
    assert_eq!(
        card[0].chars().nth(10),
        Some('\u{2726}'),
        "the wand's `✦` is where the place says"
    );
    assert_eq!(launch_card_version(&card, &place), Some("2.1.302"));
    assert_eq!(
        launch_card(&card),
        Some(LaunchCard {
            version: "2.1.302".into(),
            model: "Opus 5.5".into(),
            effort: Some("xhigh".into()),
        })
    );
    let entrance =
        measured("claude-2.1.302-launch-workspace-fullscreen-entrance-arms-up-144col.txt");
    assert!(
        entrance[1].starts_with("\u{2597}\u{259F}"),
        "{:?}",
        entrance[1]
    );
    assert_eq!(launch_card_scan(&entrance), (None, None));
}

fn jump(pose: &str) -> Vec<String> {
    measured(&format!(
        "claude-2.1.302-launch-workspace-fullscreen-entrance-jump-{pose}-144col.txt"
    ))
}

fn jump_entrance() -> Vec<Vec<String>> {
    let poses = ["1-stand", "2-dust", "3-dust", "4-arms-up"];
    let mut frames: Vec<_> = poses.iter().chain(&poses).map(|p| jump(p)).collect();
    frames.extend([jump("1-stand"), jump("5-rest")]);
    frames
}

fn follow(
    rows: &[String],
    kept: Option<&(CardPlace, Vec<String>)>,
) -> Option<(CardPlace, Vec<String>)> {
    let scan = launch_card_scan(rows);
    if let Some((place, words)) = kept {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        if launch_card_held(rows, scan, place, &words) {
            return kept.cloned();
        }
    }
    let place = scan.1?;
    let words = launch_card_words(rows, &place).expect("a placed card's words");
    Some((place, owned(&words)))
}

fn owned(words: &CardWords<'_>) -> Vec<String> {
    words.as_slice().iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn the_entrance_holds_the_card_it_placed() {
    let frames = jump_entrance();
    let mut kept = None;
    let mut shown = Vec::new();
    for rows in &frames {
        kept = follow(rows, kept.as_ref());
        shown.push(kept.as_ref().map(|(place, _)| *place));
    }
    let stand = launch_card_place(&frames[0]).expect("the mascot standing");
    assert_eq!((stand.top, stand.text_col, stand.mascot_end), (1, 13, 9));
    let rest = launch_card_place(&frames[9]).expect("the mascot at rest");
    assert_eq!((rest.top, rest.text_col, rest.mascot_end), (1, 13, 11));
    let mut want = vec![Some(stand); 9];
    want.push(Some(rest));
    assert_eq!(
        shown, want,
        "the card on every frame, held through the entrance"
    );
    let (_, words) = kept.expect("the card");
    assert_eq!(
        words,
        [
            "Claude Code v2.1.302",
            "Opus 5.5 with xhigh effort \u{b7} API Usage Billing",
            "/private/tmp/at.TOxX/proj",
        ]
        .map(str::to_owned)
    );
    let bare: Vec<bool> = frames
        .iter()
        .map(|f| launch_card_place(f).is_some())
        .collect();
    assert_eq!(
        bare,
        [
            true, false, false, false, true, false, false, false, true, true
        ]
    );
    assert_eq!(launch_card_scan(&frames[1]), (Some(1), None));
    assert_eq!(launch_card_scan(&frames[2]), (Some(1), None));
    assert_eq!(launch_card_scan(&frames[3]), (None, None));
    for alone in [
        jump("2-dust"),
        jump("3-dust"),
        jump("4-arms-up"),
        measured("claude-2.1.302-launch-workspace-fullscreen-entrance-arms-up-144col.txt"),
    ] {
        assert_eq!(follow(&alone, None), None, "{alone:#?}");
    }
}

#[test]
fn a_held_card_goes_with_its_banner() {
    let stand = jump("1-stand");
    let arms = jump("4-arms-up");
    let kept = follow(&stand, None).expect("the standing card");
    let place = kept.0;
    let then = |rows: &[String]| follow(rows, Some(&kept)).map(|(p, _)| p);
    assert_eq!(then(&arms), Some(place), "CONTROL: the raised arms hold it");

    let blank = || vec![String::new(); arms.len()];
    let up = |rows: &[String], by: usize| {
        let mut moved = rows[by..].to_vec();
        moved.resize(rows.len(), String::new());
        moved
    };
    let with = |rows: &[String], at: usize, new: &[&str]| {
        let mut edited = rows.to_vec();
        for (k, row) in new.iter().enumerate() {
            edited[at + k] = (*row).to_string();
        }
        edited
    };
    let model = &arms[2];
    assert_eq!(model.chars().nth(12), Some(' '));
    let over_mascot = format!("say hi, then{}", model.chars().skip(12).collect::<String>());
    let mut narrow = vec![String::new()];
    narrow.extend(measured("claude-2.1.302-launch-workspace-narrow-60col.txt"));
    let newer_narrow = with(&arms, 10, &["             Claude Code v2.1.302", ""]);
    let newer_wide = with(&arms, 10, &[&stand[1], &stand[2], &stand[3]]);
    for (what, rows, want) in [
        ("scrolled a row", up(&arms, 1), None),
        ("scrolled three rows", up(&arms, 3), None),
        ("cleared", blank(), None),
        (
            "a box over it",
            with(
                &arms,
                1,
                &[
                    "\u{256D}\u{2500}\u{2500}\u{2500}\u{2500}\u{256E}",
                    "\u{2502} Do you want to proceed? \u{2502}",
                    "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256F}",
                ],
            ),
            None,
        ),
        (
            "words over the mascot",
            with(&arms, 2, &[&over_mascot]),
            None,
        ),
        (
            "the program gone",
            with(&blank(), 0, &["$ claude", "$ "]),
            None,
        ),
        ("the narrow layout, the scan's card", narrow, Some(1)),
        ("a newer narrow banner below", newer_narrow, None),
        ("a newer card below", newer_wide, Some(10)),
        ("scrolled standing", up(&stand, 1), Some(0)),
    ] {
        assert_eq!(then(&rows).map(|p| p.top), want, "{what}: {rows:#?}");
    }
}

#[test]
fn a_cards_words_open_with_its_banner() {
    let narrow = measured("claude-2.1.302-launch-workspace-narrow-60col.txt");
    let rest = jump("5-rest");
    let wide = launch_card_place(&rest).expect("the card at rest");
    let own = launch_card_place(&narrow).expect("the narrow card");
    assert_eq!((own.rows, own.mascot_row), (4, 1));
    assert_eq!(
        launch_card_words(&narrow, &own).map(|w| owned(&w)),
        Some(
            [
                "Claude Code v2.1.302",
                "Opus 5.5 with xhigh effort",
                "API Usage Billing",
                "/private/tmp/at.tPwT/proj",
            ]
            .map(str::to_owned)
            .to_vec()
        ),
    );
    let under = CardPlace { top: 1, ..wide };
    let words: Vec<_> = (1..4).map(|k| card_words(&narrow[k], 13)).collect();
    assert_eq!(
        words,
        [
            Some("Opus 5.5 with xhigh effort"),
            Some("API Usage Billing"),
            Some("/private/tmp/at.tPwT/proj"),
        ],
        "CONTROL: a card row's words, each: {narrow:#?}"
    );
    assert_eq!(launch_card_words(&narrow, &under), None, "{narrow:#?}");
    assert_eq!(
        launch_card_words(&rest, &wide).map(|w| owned(&w)),
        Some(
            [
                "Claude Code v2.1.302",
                "Opus 5.5 with xhigh effort \u{b7} API Usage Billing",
                "/private/tmp/at.TOxX/proj",
            ]
            .map(str::to_owned)
            .to_vec()
        ),
        "CONTROL: the card's own words"
    );
}

#[test]
fn the_workspace_narrow_card_is_placed_never_passed_over() {
    let under_a_card = |newest: &[String]| {
        let mut both = card_rows();
        both.extend(rows(&["", "\u{276F} exit", ""]));
        let at = both.len();
        both.extend(newest.iter().cloned());
        (at, both)
    };
    for name in [
        "claude-2.1.302-launch-workspace-narrow-60col.txt",
        "claude-2.1.302-launch-workspace-narrow-40col.txt",
        "claude-2.1.302-launch-workspace-narrow-34col.txt",
    ] {
        let narrow = measured(name);
        let place = launch_card_place(&narrow).expect("the narrow card");
        assert_eq!(
            (
                place.top,
                place.rows,
                place.mascot_row,
                place.text_col,
                place.mascot_end
            ),
            (0, 4, 1, 13, 11),
            "{name}"
        );
        assert_eq!(place.wand(), Some((10, 1)), "{name}: the wand's cell");
        let (at, both) = under_a_card(&narrow);
        assert_eq!(
            launch_card_scan(&both),
            (Some(at), Some(CardPlace { top: at, ..place })),
            "{name}: {both:#?}"
        );
        let mascot_alone: String = narrow[1].chars().take(11).collect();
        let mut no_margin = narrow[1..4].to_vec();
        no_margin.push(narrow[3].clone());
        for (what, tail) in [
            ("cut under its banner", vec![]),
            ("over a blank row", vec![String::new()]),
            ("over the mascot without its text", vec![mascot_alone]),
            ("over a fourth row of text", no_margin),
        ] {
            let mut newest = vec![narrow[0].clone()];
            newest.extend(tail);
            let (at, both) = under_a_card(&newest);
            assert_eq!(
                launch_card_scan(&both),
                (Some(at), None),
                "{name} {what}: {both:#?}"
            );
        }
    }
    let classic = measured("claude-2.1.300-prose-opens-with-limit-words-34col.txt");
    let quote: Vec<String> = classic[..4].iter().map(|r| format!("  {r}")).collect();
    assert_eq!(
        quote[0].find('C').map(|at| quote[0][..at].chars().count()),
        Some(NARROW_HELD_COL),
        "the quoted banner's column: {quote:#?}"
    );
    assert_eq!(launch_card_place(&quote), None, "{quote:#?}");
    let mut under = card_rows();
    under.extend(rows(&["", "\u{23FA} The narrow card read:", ""]));
    under.extend(quote);
    assert_eq!(
        launch_card_place(&under).map(|p| p.top),
        Some(0),
        "{under:#?}"
    );
    let wrapped = measured("claude-2.1.302-launch-workspace-narrow-26col.txt");
    assert_eq!(launch_card_scan(&wrapped), (None, None));
}

#[test]
fn the_narrow_card_is_placed_by_its_measured_shape() {
    let capture = |name: &str| measured(&format!("claude-2.1.304-update-notice-{name}.txt"));
    for (name, top, text_col, mascot_end) in [
        ("narrow-workspace-fullscreen-50col", 1, 13, 11),
        ("narrow-workspace-fullscreen-34col", 1, 13, 11),
        ("narrow-workspace-fullscreen-33col", 1, 13, 11),
        ("narrow-workspace-inline-50col", 0, 13, 11),
        ("narrow-workspace-inline-33col", 0, 13, 11),
        ("narrow-xhigh-fullscreen-50col", 1, 11, 9),
        ("narrow-xhigh-fullscreen-31col", 1, 11, 9),
    ] {
        let screen = capture(name);
        let place = launch_card_place(&screen).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(
            (
                place.top,
                place.rows,
                place.mascot_row,
                place.text_col,
                place.mascot_end
            ),
            (top, 4, 1, text_col, mascot_end),
            "{name}"
        );
        assert_eq!(launch_card_version(&screen, &place), Some("2.1.304"));
        assert_eq!(place.wand(), Some((mascot_end - 1, top + 1)), "{name}");
    }
    for name in [
        "narrow-workspace-fullscreen-32col",
        "narrow-xhigh-fullscreen-30col",
    ] {
        assert_eq!(launch_card_scan(&capture(name)), (None, None), "{name}");
    }
    let screen = capture("narrow-workspace-fullscreen-50col");
    assert!(
        crate::text::blank(&screen[5]),
        "CONTROL: the margin under the card: {screen:#?}"
    );
    let edited = |at: usize, row: String| {
        let mut s = screen.clone();
        s[at] = row;
        s
    };
    for (why, rows) in [
        (
            "words after the version",
            edited(1, format!("{} is out", screen[1])),
        ),
        (
            "the mascot's top row a column in",
            edited(2, format!(" {}", screen[2])),
        ),
        (
            "the model a column off",
            edited(2, screen[2].replacen("  Opus", "   Opus", 1)),
        ),
        (
            "a fifth row of the card's text",
            edited(5, screen[4].clone()),
        ),
        (
            "the billing's text gone",
            edited(3, screen[3].chars().take(10).collect()),
        ),
    ] {
        assert_eq!(launch_card_place(&rows), None, "{why}: {rows:#?}");
    }
}

#[test]
fn a_row_naming_the_workspace_build_is_no_card() {
    let card = measured("claude-2.1.302-launch-workspace-144col.txt");
    for named in [
        "\u{23FA} Claude Code v2.1.302 holds a wand on workspace.",
        "  \u{2726} Claude Code v2.1.302",
        "\u{2726}\u{2571} Claude Code v2.1.302",
        " \u{2571}\u{2726}  Claude Code v2.1.302",
        "\u{276F} \u{2571}\u{2726} Claude Code v2.1.302",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}\u{2584}x\u{2726}  Claude Code v2.1.302",
        " \u{2590} see  Claude Code v2.1.302",
    ] {
        let mut screen = vec![named.to_string()];
        screen.extend(card[1..3].iter().cloned());
        assert_eq!(launch_card_scan(&screen), (None, None), "{named:?}");
    }
    let mut wand_alone = card.clone();
    wand_alone[1] = format!(
        "         \u{2571}\u{2726}  {}",
        card[1].chars().skip(13).collect::<String>()
    );
    assert_eq!(launch_card_place(&wand_alone), None, "{wand_alone:#?}");
    for by in [2, 5] {
        let quote: Vec<String> = card[..3]
            .iter()
            .map(|r| format!("{}{r}", " ".repeat(by)))
            .collect();
        assert_eq!(launch_card_place(&quote), None, "quoted at {by}");
        let mut under = card_rows();
        under.extend(rows(&["", "\u{23FA} The card read:", ""]));
        under.extend(quote);
        assert_eq!(
            launch_card_place(&under).map(|p| p.top),
            Some(0),
            "quoted at {by}: {under:#?}"
        );
    }
}

#[test]
fn a_relaunch_places_the_newer_card_and_never_falls_back() {
    let relaunch = measured("claude-2.1.283-inline-relaunch-repl-ready.txt");
    assert_eq!(launch_card_place(&relaunch).map(|p| p.top), Some(16));
    assert_eq!(launch_card_place(&relaunch[..16]).map(|p| p.top), Some(3));
    for cut in [17, 18] {
        assert_eq!(launch_card_place(&relaunch[..cut]), None, "rows[..{cut}]");
    }
}

#[test]
fn the_card_is_placed_under_a_persons_rows() {
    let busy = measured("claude-2.1.283-footer-busy.txt");
    assert_eq!(launch_card(&busy), None);
    assert_eq!(launch_card_place(&busy).map(|p| p.top), Some(1));
    let mut named = card_rows();
    named.extend(rows(&[
        "",
        "❯ what changed in Claude Code v2.1.284?",
        "",
        "⏺ Claude Code v2.1.284 added the drafts card.",
        "",
    ]));
    assert_eq!(launch_card_place(&named).map(|p| p.top), Some(0));
    let boxed = measured("claude-2.1.283-effort-switch.txt");
    assert_eq!(launch_card(&boxed), None);
    assert_eq!(launch_card_place(&boxed).map(|p| p.top), Some(1));
}

#[test]
fn a_quoted_card_is_not_a_card() {
    let card = card_rows();
    let under_gutter = vec![
        "⏺ Bash(head -3 card.txt)".to_string(),
        format!("  ⎿  {}", card[0]),
        format!("     {}", card[1]),
        format!("     {}", card[2]),
    ];
    let indent = |by: usize| -> Vec<String> {
        let mut r = vec!["⏺ The card read:".to_string(), String::new()];
        r.extend(card.iter().map(|c| format!("{}{c}", " ".repeat(by))));
        r
    };
    for (what, quote) in [
        ("under the gutter", under_gutter),
        ("at a message's column", indent(2)),
        ("at a tool output's column", indent(5)),
    ] {
        assert_eq!(launch_card_place(&quote), None, "{what}: {quote:#?}");
        let mut under = card.clone();
        under.push(String::new());
        under.extend(quote);
        assert_eq!(
            launch_card_place(&under).map(|p| p.top),
            Some(0),
            "{what}: {under:#?}"
        );
    }
}

#[test]
fn a_narrow_banner_is_its_card_or_refused() {
    let narrow = measured("claude-2.1.300-prose-opens-with-limit-words-34col.txt");
    let banner = narrow[0].clone();
    assert_eq!(
        banner.trim(),
        "Claude Code v2.1.300",
        "the fixture's banner row"
    );
    let over = |tail: &[&str]| {
        let mut r = vec![banner.clone()];
        r.extend(rows(tail));
        r
    };
    let whole = launch_card_place(&narrow).expect("the whole narrow card");
    assert_eq!((whole.top, whole.rows, whole.text_col), (0, 4, 11));
    let mut both = card_rows();
    both.extend(rows(&["", "❯ exit", ""]));
    let at = both.len();
    both.extend(narrow.iter().cloned());
    assert_eq!(
        launch_card_place(&both),
        Some(CardPlace { top: at, ..whole }),
        "the newest card, under an older one: {both:#?}"
    );
    for (what, newest) in [
        ("cut to its banner row", narrow[..1].to_vec()),
        ("over one blank row", over(&[""])),
        (
            "over the mascot alone",
            over(&[" \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}"]),
        ),
        (
            "over a row without the mascot",
            over(&["           Opus 5.5"]),
        ),
        ("with trailing blanks", vec![format!("{banner}      ")]),
    ] {
        assert_eq!(launch_card_place(&newest), None, "{what}: {newest:#?}");
        let mut both = card_rows();
        both.extend(rows(&["", "❯ exit", ""]));
        both.extend(newest);
        assert_eq!(
            launch_card_place(&both),
            None,
            "under a card, {what}: {both:#?}"
        );
    }
    let mut words = card_rows();
    words.extend(rows(&[
        "",
        "  Claude Code v2.1.284 added the drafts card.",
        "",
    ]));
    assert_eq!(
        launch_card_place(&words).map(|p| p.top),
        Some(0),
        "{words:#?}"
    );
    let mut confirmed = card_rows();
    confirmed.push(String::new());
    confirmed.push(format!("{banner}  (latest)"));
    confirmed.extend(narrow[1..].iter().cloned());
    assert_eq!(launch_card_place(&confirmed), None, "{confirmed:#?}");

    let bare = banner.trim_start();
    for (col, cut_refused, over_quote_refused) in [
        (2, false, false),
        (8, false, false),
        (9, true, true),
        (11, true, true),
        (12, true, true),
        (13, true, false),
        (14, false, false),
        (16, false, false),
    ] {
        for (refused, tail) in [
            (cut_refused, None),
            (
                over_quote_refused,
                Some("   \u{2590}\u{259B}\u{2588}   Opus 5.5"),
            ),
        ] {
            let mut under = card_rows();
            under.extend(rows(&["", "⏺ Done.", ""]));
            under.push(format!("{}{bare}", " ".repeat(col)));
            under.extend(tail.map(str::to_owned));
            assert_eq!(
                launch_card_place(&under).map(|p| p.top),
                (!refused).then_some(0),
                "a bare banner at column {col} over {tail:?}: {under:#?}"
            );
        }
    }
    let mut quoted = card_rows();
    quoted.extend(rows(&[
        "",
        "❯ show me the narrow card",
        "",
        "⏺ Bash(cat narrow-card.txt)",
        "  ⎿  captured at 34 columns:",
    ]));
    quoted.extend(narrow[..4].iter().map(|r| format!("     {r}")));
    assert_eq!(
        quoted[8]
            .find(bare)
            .map(|at| quoted[8][..at].chars().count()),
        Some(16),
        "the quoted banner's column: {quoted:#?}"
    );
    assert_eq!(
        launch_card_place(&quoted).map(|p| p.top),
        Some(0),
        "{quoted:#?}"
    );
}

#[test]
fn the_placed_cards_version_is_its_banners() {
    let dir = aterm_tempfile::manifest_dir().join("src/fixtures");
    let mut read = 0;
    for entry in std::fs::read_dir(&dir).expect("the fixtures") {
        let path = entry.expect("a fixture").path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Some((header, _)) = text.split_once('\n') else {
            continue;
        };
        let rows = crate::prompt::fixtures::screen(&text);
        let Some(place) = launch_card_place(&rows) else {
            continue;
        };
        let version = launch_card_version(&rows, &place)
            .unwrap_or_else(|| panic!("{}: no version", path.display()));
        assert_eq!(
            header_version(header),
            Some(version),
            "{}: {version:?} under {header:?}",
            path.display()
        );
        read += 1;
    }
    assert!(read >= 144, "the corpus must exercise the reader: {read}");
    let header = "# claude-code 2.1.300 \u{00B7} MEASURED 2026-10-14";
    assert_eq!(header_version(header), Some("2.1.300"));
    for wrong in ["2.1.30", "2.1.3000", "2.1"] {
        assert_ne!(header_version(header), Some(wrong), "{wrong}");
    }

    let card = card_rows();
    let place = launch_card_place(&card).expect("the measured card");
    assert_eq!(launch_card_version(&card, &place), Some("2.1.294"));
    assert_eq!(launch_card_version(&card[..0], &place), None);
    let shifted = CardPlace { top: 1, ..place };
    assert_eq!(launch_card_version(&card, &shifted), None);
    let banner = crate::anchor!("launch.banner");
    let at_text_col = "\u{276F} is it at Claude Code v2.1.284";
    assert_eq!(
        at_text_col
            .find(banner)
            .map(|at| at_text_col[..at].chars().count()),
        Some(place.text_col),
        "the person's row holds the banner at the text column"
    );
    for named in [
        "\u{276F} what changed in Claude Code v2.1.284?",
        at_text_col,
        "           Claude Code v2.1.284",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}     Claude Code v2.1.284",
    ] {
        assert!(
            named.contains(&format!("{banner}2.1.284")),
            "the row names a version: {named:?}"
        );
        let mut edited = card.clone();
        edited[0] = named.to_string();
        assert_eq!(launch_card_version(&edited, &place), None, "{named:?}");
    }
    for banner in [
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2..300",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.3.0",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.1234567",
    ] {
        let mut edited = card.clone();
        edited[0] = banner.to_string();
        assert_eq!(launch_card_version(&edited, &place), None, "{banner:?}");
    }
}

#[test]
fn garbage_places_nothing() {
    let card = card_rows();
    let edited = |k: usize, row: &str| {
        let mut r = card.clone();
        r[k] = row.to_string();
        r
    };
    for screen in [
        Vec::new(),
        rows(&[""]),
        rows(&["Claude Code v"]),
        rows(&[" \u{2590}Claude Code v2.1.300", " \u{2590}x", " \u{2590}x"]),
        rows(&["\u{2590}", "\u{2580}", "\u{259F}"]),
        card[..1].to_vec(),
        card[..2].to_vec(),
        edited(1, "           Sonnet 5.5 · Claude API"),
        edited(1, "▝▜██████▀   Sonnet 5.5 · Claude API"),
        edited(1, "▝▜██████▀  ▀Sonnet 5.5"),
        edited(2, " ▝▝   ▝▝  ▝/private/tmp/at.0000/proj"),
        edited(2, " ▝▝   ▝▝"),
    ] {
        assert_eq!(launch_card_place(&screen), None, "{screen:#?}");
    }
}

#[test]
fn a_kept_card_reading_is_the_scans() {
    let anchor = crate::anchor!("launch.banner");
    let dir = aterm_tempfile::manifest_dir().join("src/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("a fixture").path())
        .collect();
    paths.sort();
    let card = card_rows();
    let rewrites: [&[String]; 5] = [
        &rows(&[""]),
        &rows(&["\u{276F} what changed in Claude Code v2.1.284?"]),
        &rows(&["           Claude Code v2.1.299"]),
        &card[..2],
        &card[1..2],
    ];
    let (mut kept, mut moved, mut screens) = (0, 0, 0);
    let (mut newer_below, mut own_rows) = (0, 0);
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let before = crate::prompt::fixtures::screen(&text);
        if !before.iter().any(|r| r.contains(anchor)) {
            continue;
        }
        screens += 1;
        let reading = launch_card_scan(&before);
        for at in 0..before.len() {
            for rewrite in rewrites {
                let mut after = before.clone();
                for (k, row) in rewrite.iter().enumerate() {
                    if let Some(slot) = after.get_mut(at + k) {
                        slot.clone_from(row);
                    }
                }
                let changed: Vec<usize> = (0..after.len())
                    .filter(|&i| after[i] != before[i])
                    .collect();
                let scan = launch_card_scan(&after);
                if launch_card_unmoved(&after, reading.0, &changed) {
                    assert_eq!(scan, reading, "{}: rows {at}.. {rewrite:?}", path.display());
                    kept += 1;
                } else {
                    moved += 1;
                    let top = reading.0.unwrap_or(0);
                    if at > top && scan != reading {
                        newer_below += 1;
                    }
                    if changed.iter().any(|j| (top..top + 3).contains(j)) {
                        own_rows += 1;
                    }
                }
            }
        }
    }
    assert!(
        screens >= 140,
        "the corpus must exercise the check: {screens}"
    );
    assert!(
        kept > moved,
        "most rewrites keep the reading: {kept} kept, {moved} moved"
    );
    assert!(newer_below > 0 && own_rows > 0, "{newer_below} {own_rows}");
    let mut screen = card_rows();
    screen.extend(rows(&["", "", "", ""]));
    let reading = launch_card_scan(&screen);
    assert_eq!(reading.0, Some(0));
    let mut words = screen.clone();
    words[3] = "\u{276F} keep going".into();
    assert!(launch_card_unmoved(&words, reading.0, &[3]));
    words[2] = "\u{276F} keep going".into();
    assert!(!launch_card_unmoved(&words, reading.0, &[2, 3]));
    let narrow = measured("claude-2.1.304-update-notice-narrow-workspace-fullscreen-50col.txt");
    let at = launch_card_scan(&narrow);
    assert_eq!(at.0, Some(1), "CONTROL: the narrow card's banner");
    let mut words = narrow.clone();
    words[6] = "\u{276F} keep going".into();
    assert!(launch_card_unmoved(&words, at.0, &[6]));
    words[5] = "\u{276F} keep going".into();
    assert!(!launch_card_unmoved(&words, at.0, &[5, 6]), "its margin");
    assert_eq!(launch_card_scan(&words), (Some(1), None), "read again");
    let mut newer = screen.clone();
    newer[4..].clone_from_slice(&card_rows()[..3]);
    assert!(!launch_card_unmoved(&newer, reading.0, &[4, 5, 6]));
    assert_eq!(launch_card_scan(&newer).0, Some(4));
    let mut named = screen.clone();
    named[4] = "           Claude Code v2.1.299 is out".into();
    let reading = launch_card_scan(&named);
    assert_eq!(reading.0, Some(0));
    let mut confirmed = named.clone();
    confirmed[5] = " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Opus 5.5".into();
    assert_eq!(launch_card_scan(&confirmed), (Some(4), None));
    assert!(!launch_card_unmoved(&confirmed, reading.0, &[5]));
    let blank = rows(&["", "", ""]);
    assert!(launch_card_unmoved(&blank, None, &[0, 1, 2]));
    assert!(!launch_card_unmoved(&card_rows(), None, &[0, 1, 2]));
    let _ = launch_card_unmoved(&blank, Some(usize::MAX), &[usize::MAX, 0]);
    assert!(launch_card_unmoved(&blank, Some(1), &[0, usize::MAX]));
}

#[test]
fn the_answered_prompt_is_the_row_over_the_last_words() {
    let measured = screen(include_str!(
        "fixtures/claude-2.1.303-fast-answers-same-idle-line-measured.txt"
    ));
    let idle = "Both branches have landed and no lander is running.";
    assert_eq!(said_tail(&measured).as_deref(), Some(idle));
    assert_eq!(answered_prompt(&measured).as_deref(), Some("keep going"));
    let caret = measured
        .iter()
        .position(|r| r == "❯")
        .expect("the caret row");
    let top = caret - 1;
    let keep = measured
        .iter()
        .rposition(|r| r == "❯ keep going")
        .expect("the last act's row");
    let checked: Vec<String> = measured[..keep]
        .iter()
        .chain(&measured[top..])
        .cloned()
        .collect();
    let check = answered_prompt(&checked).expect("the check's row");
    assert!(
        check.starts_with("Before you stop: do every open item"),
        "{check}"
    );
    assert!(
        check.contains("(build, test, measure where numbers"),
        "{check}"
    );
    assert!(
        check.ends_with("a slow check is not a blocker. Otherwise continue."),
        "{check}"
    );
    assert_eq!(said_tail(&checked).as_deref(), Some(idle));
    let mut drafted = checked.clone();
    let at = drafted
        .iter()
        .position(|r| r == "❯")
        .expect("the caret row");
    drafted[at] = "❯ keep going".to_string();
    assert_eq!(answered_prompt(&drafted).as_deref(), Some(check.as_str()));
    let pending: Vec<String> = measured[..keep + 2]
        .iter()
        .chain(&measured[top..])
        .cloned()
        .collect();
    assert_eq!(said_tail(&pending).as_deref(), Some(idle));
    assert_eq!(answered_prompt(&pending).as_deref(), Some(check.as_str()));
    let unanswered = framed(&["⏺ Done.", "", "❯ keep going", ""], "  ? for shortcuts");
    assert_eq!(answered_prompt(&unanswered), None);
    let tooled = framed(
        &[
            "❯ run the suite",
            "",
            "⏺ Bash(targo --unverified test -p aterm-phase)",
            "  ⎿  test result: ok. 412 passed",
            "",
            "⏺ All green.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(answered_prompt(&tooled).as_deref(), Some("run the suite"));
    let woke = framed(
        &[
            "❯ keep going",
            "",
            &format!("⏺ {idle}"),
            "",
            "✻ Baked for 0s · done 11:06 PM",
            "",
            "⏺ The background suite finished: all green.",
            "",
        ],
        "  ? for shortcuts",
    );
    assert_eq!(answered_prompt(&woke), None);
    assert_eq!(answered_prompt(&measured[..4]), None);
    assert_eq!(
        answered_prompt(&rows(&["❯ keep going", "", "⏺ Done."])),
        None
    );
}

#[test]
fn a_prompt_a_hook_stopped_closes_no_turn_and_its_label_is_no_words() {
    use crate::prompt::fixtures::{
        PROMPT_HOOK_BLOCKED_LOGIN_PHRASE_MEASURED, PROMPT_HOOK_STOPPED_LOGIN_PHRASE_MEASURED,
    };
    let stopped = screen(PROMPT_HOOK_STOPPED_LOGIN_PHRASE_MEASURED);
    let label = stopped
        .iter()
        .position(|r| r == "⏺ Operation stopped by hook: Not logged in · Please run /login")
        .expect("the measured label row");
    let with = |words: &[&str]| -> Vec<String> {
        let mut r = stopped[..label].to_vec();
        r.extend(words.iter().map(|w| (*w).to_string()));
        r.extend_from_slice(&stopped[label + 1..]);
        r
    };
    for (name, read) in [
        ("measured", stopped.clone()),
        ("no reason", with(&["⏺ Operation stopped by hook"])),
        (
            "a reason on two rows",
            with(&[
                "⏺ Operation stopped by hook: Not logged in ·",
                "  Please run /login",
            ]),
        ),
    ] {
        assert_eq!(answered_prompt(&read), None, "{name}");
        assert_eq!(said_tail(&read), None, "{name}");
    }
    let blocked = screen(PROMPT_HOOK_BLOCKED_LOGIN_PHRASE_MEASURED);
    assert_eq!(said_tail(&blocked), None);
    assert_eq!(answered_prompt(&blocked), None);
    let expansion: Vec<String> = blocked
        .iter()
        .map(|r| r.replace("⏺ UserPromptSubmit ", "⏺ UserPromptExpansion "))
        .collect();
    assert_ne!(expansion, blocked);
    assert_eq!(said_tail(&expansion), None);
    let answered = with(&["⏺ Nothing to say."]);
    assert_eq!(answered_prompt(&answered).as_deref(), Some("say nothing"));
    assert_eq!(said_tail(&answered).as_deref(), Some("Nothing to say."));
    for own in [
        "The hook printed Operation stopped by hook: Not logged in.",
        "Operation stopped by hooks in CI, so I removed them.",
        "I read UserPromptSubmit operation blocked by hook: in the log.",
        "The operation blocked by hook: none, the suite ran.",
    ] {
        let mine = with(&[&format!("⏺ {own}")]);
        assert_eq!(
            answered_prompt(&mine).as_deref(),
            Some("say nothing"),
            "{own}"
        );
        assert_eq!(said_tail(&mine).as_deref(), Some(own), "{own}");
    }
}

/// THE AGENT'S WORDS BESIDE THE SIDE PANEL ARE READ UP TO ITS RULE: its last message
/// ([`said_tail`], [`said_lines`], [`said_whole`]), the turn's earlier messages
/// ([`turn_said_before`], [`turn_lines_before`], [`turn_whole`]) and the person's message the
/// turn answers ([`answered_prompt`]) are read on the transcript's rows ([`Transcript`]), each
/// cut at the panel's rule and wrapped in the rule's room. Read whole before (KI-1101-3), each
/// row of every one carried its padding, the rule and the panel's text after it, on each
/// MEASURED panel capture (2.1.310's and both of 2.1.316's, as captured): the words the needs
/// fold, the done and go-ahead readers and the question's reader (`asked_by`) read ended in
/// `│` on every row, the first message beside the panel's heading carried `1 file changed …`,
/// and no line Ink wrapped at the rule was read whole, so a `NEEDS YOU:` item wrapped there was
/// read as its first row. CONTROL: each capture with no panel (each row cut at the rule) reads
/// the same words.
#[test]
fn the_agents_words_beside_the_side_panel_are_read_up_to_its_rule() {
    use crate::prompt::fixtures::{
        INFERRING_OVER_BACKGROUND_2_1_316, WAITING_AGENTS_WORKFLOWS_2_1_316,
        WAITING_WORKFLOWS_2_1_310,
    };
    fn apart(_: &str, _: &str) -> bool {
        false
    }
    let no_panel = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.split_once('│')
                    .map_or(row.as_str(), |(left, _)| crate::text::trim_end(left))
                    .to_string()
            })
            .collect()
    };
    for (name, capture) in [
        ("2.1.310 waiting", WAITING_WORKFLOWS_2_1_310),
        ("2.1.316 waiting", WAITING_AGENTS_WORKFLOWS_2_1_316),
        ("2.1.316 inferring", INFERRING_OVER_BACKGROUND_2_1_316),
    ] {
        let panel = screen(capture);
        let alone = no_panel(&panel);
        let tail = said_tail(&panel).expect("the last message");
        let before = turn_said_before(&panel).expect("the turn's earlier messages");
        for words in [
            tail.as_str(),
            before.as_str(),
            &said_lines(&panel, apart).expect("its lines"),
            &turn_lines_before(&panel, apart).expect("their lines"),
        ] {
            assert!(!words.contains('│'), "{name}: {words}");
        }
        assert_eq!(said_tail(&alone).as_deref(), Some(tail.as_str()), "{name}");
        assert_eq!(
            turn_said_before(&alone).as_deref(),
            Some(before.as_str()),
            "{name}"
        );
        assert_eq!(said_whole(&panel), said_whole(&alone), "{name}");
        assert_eq!(turn_whole(&panel), turn_whole(&alone), "{name}");
        assert_eq!(answered_prompt(&panel), answered_prompt(&alone), "{name}");
        assert!(
            answered_prompt(&panel).is_none_or(|p| !p.contains('│')),
            "{name}"
        );
    }
    let waiting = screen(WAITING_AGENTS_WORKFLOWS_2_1_316);
    let lines = said_lines(&waiting, apart).expect("its lines");
    for need in [
        "NEEDS YOU: one sign-in for the Claude account row m21 (aterm harness accounts browser m21).",
        "NEEDS YOU: an xAI account for Grok on m21.",
    ] {
        assert!(lines.lines().any(|l| l == need), "{need}: {lines}");
    }
    assert_eq!(
        answered_prompt(&waiting).as_deref(),
        Some(
            "[aterm harness] Autopilot is Cruise: your turn ended while your own background \
             work runs. Check on that work, then keep going with everything else in hand; \
             never wait idle for it. Put what only a person can give on a line starting \
             NEEDS YOU: and carry on. A go-ahead for your own reversible work is never a \
             NEEDS YOU item."
        )
    );
}
