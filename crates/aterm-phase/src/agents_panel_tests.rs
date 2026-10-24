// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agents panel's row reader ([`super`]).

use super::*;

fn parse(row: &str) -> PanelRow {
    parse_row(row, one_cell).unwrap_or_else(|| panic!("a panel row: {row:?}"))
}

fn span(start: u16, end: u16) -> Span {
    Span { start, end }
}

fn clock(ms: u64, res_ms: u32) -> Elapsed {
    Elapsed::Clock { ms, res_ms }
}

#[test]
fn reads_each_row_kind_of_the_2_1_304_grammar() {
    let main = parse("  ⏺ main");
    assert_eq!(main.kind, RowKind::Main);
    assert_eq!(main.bullet, Some((span(2, 3), Bullet::Filled)));
    assert_eq!(main.name, span(4, 8));
    assert_eq!((main.pointer, main.count), (None, None));

    let main = parse(&format!("  ◯ main{}↑ 2 more  ", " ".repeat(60)));
    assert_eq!(main.kind, RowKind::Main);
    assert_eq!(main.bullet.map(|b| b.1), Some(Bullet::Hollow));
    assert_eq!(main.count, Some((span(70, 71), 2)));

    let selected = parse("❯ ● main");
    assert_eq!(selected.kind, RowKind::Main);
    assert_eq!(selected.pointer, Some(span(0, 1)));
    assert_eq!(selected.bullet, Some((span(2, 3), Bullet::Filled)));

    let row = "  ◯ general-purpose  Searching the report's consumers in tests            10h 9m 42s · ↓ 613.6k tokens  ";
    let agent = parse(row);
    assert_eq!(agent.kind, RowKind::Agent);
    assert_eq!(agent.name, span(4, 19));
    assert_eq!(agent.name_hash, text_hash("general-purpose"));
    assert_eq!(agent.activity, Some(span(21, 62)));
    assert_eq!(
        agent.activity_hash,
        text_hash("Searching the report's consumers in tests")
    );
    assert_eq!(agent.elapsed, Some((span(74, 84), clock(36_582_000, 1000))));
    assert_eq!(
        agent.tokens,
        Some((
            span(87, 102),
            Tokens {
                count: 613_600,
                quantum: 100,
                used_tools: true
            }
        ))
    );
    assert_eq!(agent.status(), Some(span(74, 102)));
    assert!(agent.is_task());

    let nested = parse("  └ ◯ Explore  Reading the footer readers   12s · ↑ 900 tokens");
    assert_eq!((nested.kind, nested.depth), (RowKind::Agent, 1));
    assert_eq!(nested.connector, Some(span(2, 3)));
    assert_eq!(nested.bullet, Some((span(4, 5), Bullet::Hollow)));
    assert_eq!(nested.elapsed.map(|e| e.1), Some(clock(12_000, 1000)));
    assert_eq!(
        nested.tokens.map(|t| t.1),
        Some(Tokens {
            count: 900,
            quantum: 1,
            used_tools: false
        })
    );
    let deeper = parse("❯   ├ ◯ Explore  Reading   1m 0s · ↓ 1.0k tokens");
    assert_eq!((deeper.depth, deeper.pointer), (2, Some(span(0, 1))));
    assert_eq!(deeper.connector, Some(span(4, 5)));
    assert_eq!(deeper.elapsed.map(|e| e.1), Some(clock(60_000, 1000)));

    let queued = parse("  ◯ reviewer  Waiting on the build   2m 5s · ↓ 12.4k tokens · 2 queued");
    assert_eq!(queued.queued.map(|q| q.1), Some(2));
    assert_eq!(queued.tokens.map(|t| t.1.count), Some(12_400));
    assert_eq!(queued.status().map(|s| s.end), Some(70));

    for (word, elapsed) in [
        ("awaiting approval", Elapsed::AwaitingApproval),
        ("idle", Elapsed::Idle),
        ("waiting", Elapsed::Waiting),
    ] {
        let row = format!("  ◯ lead-teammate  Planning the split   {word}");
        let r = parse(&row);
        assert_eq!(r.kind, RowKind::Agent, "{row}");
        assert_eq!(r.elapsed.map(|e| e.1), Some(elapsed), "{row}");
        assert_eq!(r.activity_hash, text_hash("Planning the split"), "{row}");
    }

    let lead = parse("  ◯ general-purpose (+2)  Fan-out lead   1d 3h 12m · ↓ 1.0m tokens");
    assert_eq!(lead.descendants, 2);
    assert_eq!(lead.name_hash, text_hash("general-purpose"));
    assert_eq!(lead.name, span(4, 19));
    assert_eq!(
        lead.elapsed.map(|e| e.1),
        Some(clock(((24 + 3) * 60 + 12) * 60_000, 60_000))
    );
    assert_eq!(
        lead.tokens.map(|t| (t.1.count, t.1.quantum)),
        Some((1_000_000, 100_000))
    );

    for (row, n) in [("  ◯ 3 idle agents", 3), ("❯ ◯ 1 idle agent", 1)] {
        let idle = parse(row);
        assert_eq!(idle.kind, RowKind::IdleSummary, "{row}");
        assert_eq!(idle.count.map(|c| c.1), Some(n), "{row}");
    }

    let more = parse("  ↓ 4 more");
    assert_eq!(more.kind, RowKind::More);
    assert_eq!(more.count, Some((span(4, 5), 4)));
    assert_eq!(more.bullet, None);

    let decorated = parse("  ◯ ctx 12% · 1.5m tok");
    assert_eq!(decorated.kind, RowKind::Agent);
    assert_eq!(decorated.status(), None);
}

#[test]
fn reads_the_2_1_304_workflow_row_at_each_bar_width() {
    let row = "  ◯ clean-char-literals-train  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱  3/4 · 4h10m · ↓ 1.0m tokens";
    let w = parse(row);
    assert_eq!(w.kind, RowKind::Workflow);
    assert_eq!(w.name, span(4, 29));
    assert_eq!(
        w.bar,
        Some(Bar {
            span: span(31, 51),
            filled: 15,
            cells: 20,
            blocks: false
        })
    );
    assert_eq!(
        w.progress,
        Some((span(53, 56), Progress { done: 3, total: 4 }))
    );
    assert_eq!(
        w.elapsed,
        Some((span(59, 64), clock((4 * 60 + 10) * 60_000, 60_000)))
    );
    assert_eq!(
        w.tokens.map(|t| (t.0, t.1.count)),
        Some((span(67, 80), 1_000_000))
    );
    assert_eq!(w.activity, None);

    let w = parse("  ◯ sweep  ▰▰▰▰▰▰▰▰▰▱▱▱  3/4 · 12m05s · ↓ 88.1k tokens");
    assert_eq!(w.bar.map(|b| (b.filled, b.cells)), Some((9, 12)));
    assert_eq!(w.elapsed.map(|e| e.1), Some(clock(725_000, 1000)));

    let w = parse("  ◯ sweep  ██████░░  3/4 · 59s");
    assert_eq!(
        w.bar.map(|b| (b.filled, b.cells, b.blocks)),
        Some((6, 8, true))
    );
    assert_eq!(w.elapsed.map(|e| e.1), Some(clock(59_000, 1000)));
    assert_eq!(w.tokens, None);

    let w = parse("  ◯ sweep  ▰▰▰▰▰▰▱▱  ↓ 1.0m · ⚠");
    assert_eq!(w.tokens.map(|t| t.1.count), Some(1_000_000));
    assert!(w.warning.is_some());
    let w = parse("  ◯ sweep  ▰▰▰▰▰▰▱▱");
    assert_eq!((w.kind, w.status()), (RowKind::Workflow, None));

    let row = "  ◯ big-sweep  ▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  4/30 · 2m01s · ↓ 2.4m tokens · ⚠ Large workflow · /workflows to stop";
    let w = parse(row);
    assert_eq!(
        w.progress.map(|p| p.1),
        Some(Progress { done: 4, total: 30 })
    );
    assert_eq!(w.tokens.map(|t| t.1.count), Some(2_400_000));
    let warn = w.warning.expect("the warning");
    assert_eq!(usize::from(warn.end), row.chars().count());
    assert_eq!(
        row.chars()
            .skip(usize::from(warn.start))
            .take(16)
            .collect::<String>(),
        "⚠ Large workflow"
    );

    let paused = parse("  ⏸ sweep  Rate limited · resets 3pm");
    assert_eq!(paused.kind, RowKind::Workflow);
    assert_eq!(paused.bullet.map(|b| b.1), Some(Bullet::Paused));
}

#[test]
fn reads_the_measured_rows_of_older_builds() {
    let line = |text: &str, row: usize| text.lines().nth(row).expect("the row").to_string();
    let first = line(include_str!("fixtures/wait_bg.out"), 28);
    let second = line(include_str!("fixtures/wait_bg2.out"), 28);
    for (row, elapsed, count) in [
        (&first, (20 * 60 + 59) * 1000, 428_200),
        (&second, (21 * 60 + 55) * 1000, 434_700),
    ] {
        let w = parse(row);
        assert_eq!(w.kind, RowKind::Workflow, "{row}");
        assert_eq!(w.name_hash, text_hash("cache-lever-pick"));
        assert_eq!(
            w.progress.map(|p| p.1),
            Some(Progress { done: 1, total: 4 })
        );
        assert_eq!(w.elapsed.map(|e| e.1), Some(clock(elapsed, 1000)));
        assert_eq!(
            w.tokens.map(|t| (t.1.count, t.1.quantum)),
            Some((count, 100))
        );
        assert!(w.activity.is_some(), "the description is its activity");
        assert_eq!(w.activity_hash, parse(&first).activity_hash);
    }
    let goal = line(
        include_str!("fixtures/claude-goal-active-suggestion.txt"),
        13,
    );
    let w = parse(&goal);
    assert_eq!(w.kind, RowKind::Workflow);
    assert_eq!(
        w.progress.map(|p| p.1),
        Some(Progress { done: 0, total: 1 })
    );
    assert_eq!(w.elapsed.map(|e| e.1), Some(clock(240_000, 1000)));
    let agent = parse(
        "  ◯ general-purpose  Searching the report's consumers in tests                                                  4m 28s · ↓ 115.4k tokens",
    );
    assert_eq!(agent.kind, RowKind::Agent);
    assert_eq!(agent.elapsed.map(|e| e.1), Some(clock(268_000, 1000)));
    assert_eq!(parse("  ⏺ main").kind, RowKind::Main);
}

#[test]
fn reads_the_measured_2_1_304_panel() {
    let rows = |text: &'static str| -> Vec<&'static str> { text.lines().skip(1).collect() };
    for (text, elapsed_s, selected) in [
        (
            include_str!("fixtures/claude-2.1.304-agents-panel.txt"),
            7,
            None,
        ),
        (
            include_str!("fixtures/claude-2.1.304-agents-panel-later.txt"),
            52,
            None,
        ),
        (
            include_str!("fixtures/claude-2.1.304-agents-panel-main-selected.txt"),
            24,
            Some(RowKind::Main),
        ),
        (
            include_str!("fixtures/claude-2.1.304-agents-panel-agent-selected.txt"),
            27,
            Some(RowKind::Agent),
        ),
    ] {
        let rows = rows(text);
        let n = rows.len();
        let (main, agent) = (parse(rows[n - 2]), parse(rows[n - 1]));
        assert_eq!(main.kind, RowKind::Main);
        assert_eq!(main.bullet, Some((span(2, 3), Bullet::Filled)));
        assert_eq!(agent.kind, RowKind::Agent);
        assert_eq!(agent.bullet, Some((span(2, 3), Bullet::Hollow)));
        assert_eq!(agent.name, span(4, 19));
        assert_eq!(agent.name_hash, text_hash("general-purpose"));
        assert_eq!(agent.activity_hash, text_hash("bling probe"));
        assert_eq!(
            agent.elapsed.map(|e| e.1),
            Some(clock(elapsed_s * 1000, 1000))
        );
        assert_eq!(
            agent.tokens.map(|t| t.1),
            Some(Tokens {
                count: 6_200,
                quantum: 100,
                used_tools: true
            })
        );
        assert_eq!(rows[n - 3], "", "the container's marginTop");
        assert_eq!(parse_row(rows[n - 4], one_cell), None, "{:?}", rows[n - 4]);
        let pointed = [main, agent]
            .iter()
            .find(|r| r.pointer.is_some())
            .map(|r| r.kind);
        assert_eq!(pointed, selected);
    }
}

#[test]
fn rows_around_the_panel_are_no_panel_rows() {
    for row in [
        "  ⧉  aterm Wrapper Brief · canvas",
        "  ⏵⏵ bypass permissions on · 14 shells · ← for agents · 1 feedback draft · ↓ to manage",
        "  ? for shortcuts",
        "  ← for agents",
        "  ↓ to manage",
        "⏺ Build and reviewer are running.",
        "◯ cache-lever-pick  at column zero",
        "    ◯ four spaces in and no connector  4s",
        "  ◆ main",
        "  ❯ /compact   Free up context by summarizing",
        "❯ keep going",
        "❯ ",
        "  ⎿  Backgrounded agent (↓ to manage · ctrl+o to expand)",
        "  ◯",
        "  ◯ ",
        "  ├◯ glued",
        "  ↓ 4 more agents",
        "",
    ] {
        assert_eq!(parse_row(row, one_cell), None, "{row:?}");
    }
    assert_eq!(
        parse_row("  ◯ main  ↑ two more", one_cell).map(|r| r.kind),
        Some(RowKind::Agent)
    );
}

#[test]
fn spans_count_the_callers_columns() {
    fn cjk(c: char) -> usize {
        if c == '界' { 2 } else { 1 }
    }
    let row = "  ◯ general-purpose  世界   4m 28s · ↓ 1.2k tokens";
    let narrow = parse_row(row, one_cell).expect("a row");
    let wide = parse_row(row, cjk).expect("a row");
    let at = |r: &PanelRow| r.elapsed.map(|e| e.0.start);
    assert_eq!(at(&wide), at(&narrow).map(|c| c + 1));
    assert_eq!(wide.name, narrow.name);
    assert_eq!(wide.activity.map(|a| a.len()), Some(3));
}

#[test]
fn each_segment_grammar_takes_its_own_shapes_only() {
    for (text, ms, res) in [
        ("0s", 0, 1000),
        ("59s", 59_000, 1000),
        ("1m 0s", 60_000, 1000),
        ("10h 9m 42s", 36_582_000, 1000),
        ("1d 0h 0m", 86_400_000, 60_000),
    ] {
        assert_eq!(duration(text), Some(clock(ms, res)), "{text}");
    }
    for text in [
        "60s",
        "1m 60s",
        "0m 5s",
        "24h 0m 0s",
        "1d 3h 12m 5s",
        "5m",
        "4m28s",
        "1 m",
        "s",
        "",
        "1m  5s",
    ] {
        assert_eq!(duration(text), None, "{text}");
    }
    for (text, ms, res) in [
        ("59s", 59_000, 1000),
        ("4m28s", 268_000, 1000),
        ("4m08s", 248_000, 1000),
        ("4h10m", 15_000_000, 60_000),
        ("2d03h", 183_600_000, 3_600_000),
    ] {
        assert_eq!(bar_elapsed(text), Some(clock(ms, res)), "{text}");
    }
    for text in [
        "4m 28s", "4m8s", "60m00s", "0m05s", "24h00m", "2d24h", "4h10s",
    ] {
        assert_eq!(bar_elapsed(text), None, "{text}");
    }
    for (text, count, quantum, worded) in [
        ("↓ 999 tokens", 999, 1, true),
        ("↓ 1.0k tokens", 1_000, 100, true),
        ("↑ 613.6k tokens", 613_600, 100, true),
        ("↓ 1.0m", 1_000_000, 100_000, false),
        ("↓ 2.5b tokens", 2_500_000_000, 100_000_000, true),
    ] {
        let (t, w) = tokens(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!((t.count, t.quantum, w), (count, quantum, worded), "{text}");
    }
    for text in [
        "↓ 1k tokens",
        "↓ 1.00k tokens",
        "↓ 1.0x tokens",
        "1.0k tokens",
        "↓1.0k",
        "↓ tokens",
    ] {
        assert_eq!(tokens(text), None, "{text}");
    }
    assert_eq!(queued("2 queued"), Some(2));
    assert_eq!(queued("2 queue"), None);
    assert_eq!(progress("3/4"), Some(Progress { done: 3, total: 4 }));
    assert_eq!(
        progress("16/21 agents done"),
        Some(Progress {
            done: 16,
            total: 21
        })
    );
    assert_eq!(
        progress("1/1 agent done"),
        Some(Progress { done: 1, total: 1 })
    );
    for text in ["5/4", "3/", "/4", "3/4 done", "3 / 4"] {
        assert_eq!(progress(text), None, "{text}");
    }
    assert!(warning("⚠") && warning("⚠ Large workflow"));
    assert!(!warning("⚠ Large") && !warning("Large workflow"));
}

#[test]
fn the_panel_holds_its_rows_in_order_up_to_its_capacity() {
    let mut panel = AgentsPanel::EMPTY;
    assert!(panel.is_empty() && panel.top().is_none());
    let row = parse("  ◯ general-purpose  Searching   4s · ↓ 1.2k tokens");
    for i in 0..PANEL_ROWS_MAX {
        assert!(panel.push(10 + i as u16, row));
    }
    assert!(!panel.push(99, row), "full");
    assert_eq!(panel.rows().len(), PANEL_ROWS_MAX);
    assert_eq!((panel.top(), panel.bottom()), (Some(10), Some(25)));
    assert!(panel.rows().iter().all(|r| r.name == row.name));
}

#[test]
fn reads_agy_subagent_panel_rows_with_unwrapped_name_and_activity_clock() {
    let row = parse("  ● Agent(self: Cursor Trail Rendering Engineer)  Reading file · 4m36s");
    assert_eq!(row.kind, RowKind::Agent);
    assert_eq!(row.bullet, Some((span(2, 3), Bullet::Filled)));
    assert_eq!(
        row.name,
        span(4, 4 + "Cursor Trail Rendering Engineer".len() as u16)
    );
    assert_eq!(row.name_hash, text_hash("Cursor Trail Rendering Engineer"));
    assert_eq!(row.activity, Some(span(50, 62)));
    assert_eq!(row.activity_hash, text_hash("Reading file"));
    assert_eq!(row.elapsed, Some((span(65, 70), clock(276_000, 1000))));

    let research = parse("  ● Agent(research: Docs Reader)  Searching web · 1m5s");
    assert_eq!(research.kind, RowKind::Agent);
    assert_eq!(
        research.name,
        span(4, 4 + "research: Docs Reader".len() as u16)
    );
    assert_eq!(research.name_hash, text_hash("research: Docs Reader"));
    assert_eq!(research.activity_hash, text_hash("Searching web"));
    assert_eq!(research.elapsed.map(|e| e.1), Some(clock(65_000, 1000)));

    let bare = parse("  ● Agent(self: Probe)  Working");
    assert_eq!(bare.kind, RowKind::Agent);
    assert_eq!(bare.name, span(4, 9));
    assert_eq!(bare.name_hash, text_hash("Probe"));
    assert_eq!(bare.activity_hash, text_hash("Working"));
    assert_eq!(bare.elapsed, None);
}
