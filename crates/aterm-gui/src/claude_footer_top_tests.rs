// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Claude composer top rule: its lockup, the harness switch and its label laid
//! from the rule's right end, Claude Code's label zone moved left of them, and the tint.

use super::probe::*;
use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;

const LIT: SwitchPaint =
    SwitchPaint::bare(HarnessSwitch::On, Some(aterm_phase::CliHarness::ClaudeCode));

/// An off face: its agent's own switch holds the harness off (the master's reads the same).
const OFF: SwitchPaint = SwitchPaint::bare(
    HarnessSwitch::Off(SwitchOff::Agent(aterm_phase::Program::Claude)),
    Some(aterm_phase::CliHarness::ClaudeCode),
);

const MASTER_OFF: SwitchPaint = SwitchPaint::bare(
    HarnessSwitch::Off(SwitchOff::Master),
    Some(aterm_phase::CliHarness::ClaudeCode),
);

const TURNING: SwitchPaint = SwitchPaint::bare(
    HarnessSwitch::Turning,
    Some(aterm_phase::CliHarness::ClaudeCode),
);

const TURNING_OFF: SwitchPaint = SwitchPaint::bare(
    HarnessSwitch::TurningOff,
    Some(aterm_phase::CliHarness::ClaudeCode),
);

const CELL: (u16, u16) = (7, 14);

const MARKED: [aterm_phase::Program; 4] = [
    aterm_phase::Program::Claude,
    aterm_phase::Program::Agy,
    aterm_phase::Program::Muse,
    aterm_phase::Program::Codex,
];

const LOCKUPS: [(aterm_phase::Program, Build); 5] = [
    (aterm_phase::Program::Claude, Build::Vendor),
    (aterm_phase::Program::Agy, Build::Vendor),
    (aterm_phase::Program::Muse, Build::Vendor),
    (aterm_phase::Program::Codex, Build::Vendor),
    (aterm_phase::Program::Codex, Build::Alab),
];

fn screen_with(cols: usize, top: &str) -> String {
    let rule = "\u{2500}".repeat(cols);
    let fit = |s: &str| s.chars().take(cols).collect::<String>();
    let mut out = String::from("\x1b[2J");
    for i in 0..18 {
        out.push_str(&format!(
            "\x1b[{};1H{}",
            i + 1,
            fit(&format!("\u{23FA} Message {i} of the transcript"))
        ));
    }
    out.push_str(&format!("\x1b[21;1H{top}\x1b[0m"));
    out.push_str("\x1b[22;1H\u{276F} ");
    out.push_str(&format!("\x1b[23;1H\x1b[38;2;90;90;90m{rule}\x1b[0m"));
    out.push_str(&format!("\x1b[24;1H{}", fit(BYPASS)));
    out.push_str("\x1b[22;3H");
    out
}

fn top_rules(cols: usize) -> Vec<(&'static str, String)> {
    let gray = "\x1b[38;2;90;90;90m";
    let rule = |n: usize| "\u{2500}".repeat(n);
    let half = cols / 2;
    let mut rules = vec![
        ("plain", format!("{gray}{}", rule(cols))),
        (
            "tagged",
            format!(
                "{gray}{} \x1b[38;2;175;135;255mworkspace{gray} \u{2500}",
                rule(cols.saturating_sub(12))
            ),
        ),
        (
            "two styles",
            format!(
                "{gray}{}\x1b[38;2;136;136;136m{}",
                rule(half),
                rule(cols - half)
            ),
        ),
        (
            "highlighted",
            format!(
                "{gray}{}\x1b[48;2;60;60;140m\u{2500}\x1b[49m{}",
                rule(half),
                rule(cols - half - 1)
            ),
        ),
        (
            "combining",
            format!(
                "{gray}{}\u{2500}\u{0301}{}",
                rule(half),
                rule(cols - half - 1)
            ),
        ),
    ];
    if cols >= 32 {
        rules.push((
            "voice label",
            format!(
                "\x1b[38;2;75;79;85m\u{2500}\u{2500}\x1b[38;2;138;144;152m Voice input (\u{2325} + v to start) \x1b[38;2;75;79;85m{}",
                rule(cols - 32)
            ),
        ));
    } else {
        let cut: String = " Voice input (\u{2325} + v to start)"
            .chars()
            .take(cols - 4)
            .collect();
        rules.push((
            "voice label cut",
            format!("\x1b[38;2;75;79;85m\u{2500}\u{2500}\x1b[38;2;138;144;152m{cut}\u{2026}"),
        ));
    }
    rules
}

/// Every switch and theme a top line meets: the harness on is lit under every Autopilot (Off's
/// white included), and an off or turning-on face is bare.
const CASES: [(Option<SwitchPaint>, Option<CardPalette>); 9] = [
    (None, None),
    (Some(LIT), Some(CardPalette::Cruise)),
    (Some(LIT), Some(CardPalette::Land)),
    (Some(LIT), Some(CardPalette::Off)),
    (
        Some(SwitchPaint {
            hover: HoverAt::Switch,
            ..OFF
        }),
        None,
    ),
    (Some(MASTER_OFF), None),
    (Some(LIT), None),
    (Some(TURNING), None),
    (Some(TURNING_OFF), Some(CardPalette::Off)),
];

fn key_for(
    program: aterm_phase::Program,
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    blank: RenderCell,
    face: KeyFace,
) -> TopKey {
    TopKey {
        frame_row: 30,
        term_row: 20,
        line: TopLine::Rule,
        run,
        program,
        build: Build::Vendor,
        switch,
        palette,
        blank,
        mark_fg: [0xd7, 0x77, 0x57],
        face,
        cell: CELL,
        clip: aterm_core::render::PxRect::EVERYWHERE,
        mark_hover: false,
        title_gen: 0,
        chip: TopChip::NONE,
    }
}

/// Claude Code's top-effort key's label zone in its own styles: the key's word bold in its violet.
fn claude_tail() -> Vec<RenderCell> {
    format!(" {} ", aterm_primer::CLAUDE_TOP_EFFORT_KEY)
        .chars()
        .map(|ch| RenderCell {
            ch,
            fg: if ch == ' ' {
                [90, 90, 90]
            } else {
                [175, 135, 255]
            },
            bold: ch != ' ',
            ..RenderCell::default()
        })
        .collect()
}

/// A plain run of `len` cells with Claude's label zone `tail` and the `─` closing it after it.
fn tailed_run(len: usize, tail: &[RenderCell]) -> TopRun {
    let run = plain_run(len);
    TopRun {
        tail: Some(Tail {
            col: run.col + len,
            len: tail.len(),
            term: tail_term(tail),
            plain: true,
        }),
        end: run.col + len + tail.len() + 1,
        ..run
    }
}

/// The key's tag in a [`tailed_run`] of `len` cells with [`claude_tail`]: after the zone's blank.
fn tag_in(len: usize) -> (usize, usize) {
    let start = plain_run(len).col + len + 1;
    (start, start + aterm_primer::CLAUDE_TOP_EFFORT_KEY.len())
}

/// A WIDE NAME MOVES WITH THE TAG: a session name of wide glyphs in Claude's label zone (a CJK
/// `/rename`, a one-character emoji) is plain, each glyph's two halves moved together, so the
/// harness's label ends the rule as beside a name of one-cell letters, Claude's cells laid whole
/// left of the switch. CONTROL: a name with a combining mark stays where Claude drew it, the
/// label left of it.
#[test]
fn a_wide_name_moves_with_the_tag() {
    let cols = 120;
    let gray = "\x1b[38;2;90;90;90m";
    let paint = SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..LIT
    };
    for (name, plain) in [
        ("workspace", true),
        ("\u{4fee}\u{590d}", true),
        ("\u{1f41b} fix", true),
        ("cafe\u{0301}", false),
    ] {
        let width: usize = name.chars().map(aterm_grapheme::char_width).sum();
        let top = format!(
            "{gray}{} \x1b[38;2;175;135;255m{name}{gray} \u{2500}",
            "\u{2500}".repeat(cols - 3 - width)
        );
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        let _ = app.apply_term_resize(wid, 24, cols as u16);
        bytes(&app, session, screen_with(cols, &top).as_bytes());
        refill(&mut app, wid);
        let ws = &app.windows[&wid];
        let scratch = &ws.input_scratch;
        let region = PaneTextRegion {
            strip: scratch.cells.len() - usize::from(ws.rows),
            row_off: 0,
            col_off: 0,
            rows: 24,
            cols,
        };
        let blank = scratch.implicit_blank;
        let run = read_top_run(scratch, region, 20, TopLine::Rule, blank, false).expect("the rule");
        let zone = run.tail.expect("Claude's label zone");
        assert_eq!(zone.plain, plain, "{name:?}: plain");
        let row = scratch.cells[region.strip + 20].clone();
        let tail: &[RenderCell] = if zone.plain {
            &row[zone.col..zone.col + zone.len]
        } else {
            &[]
        };
        let key = TopKey {
            frame_row: region.strip + 20,
            ..key_for(
                aterm_phase::Program::Claude,
                run,
                Some(paint),
                Some(CardPalette::Cruise),
                blank,
                KeyFace::default(),
            )
        };
        let mut top = ComposedTop::default();
        compose_top(&key, &mut top, None, None, tail);
        let s = top.switch.as_ref().expect("the switch");
        let (label, _) = s.label.clone().expect("a label at 120 columns");
        if plain {
            assert_eq!(
                label.end + CLOSE,
                run.end,
                "{name:?}: the label ends the rule"
            );
            assert!(
                top.cells.windows(tail.len()).any(|cells| cells == tail),
                "{name:?}: Claude's cells laid whole"
            );
        } else {
            assert!(
                label.end < zone.col,
                "CONTROL: {name:?}: the zone stays, the label left of it"
            );
        }
    }
}

/// THE HARNESS LAYS FROM THE RULE'S RIGHT END: on every top rule a program draws (plain, tagged,
/// two styles, highlighted, combining, Muse Code's voice label), under every switch and theme,
/// the harness is laid where the ladder says, its switch and label ending at the close before
/// the rule's end, Claude's label zone moved whole to their left in Claude's own cells, and
/// nothing is written past the rule. CONTROL: a run a cell shorter composes otherwise.
#[test]
fn the_harness_lays_from_the_rules_right_end() {
    use aterm_phase::Program;
    const LABEL: usize = 32;
    let (mut drawn, mut moved, mut lockups, mut voiced) = (0, 0, 0, 0);
    let mut narrow = std::collections::BTreeSet::new();
    const COLS: [usize; 12] = [26, 30, 32, 33, 34, 36, 40, 52, 60, 80, 120, 200];
    for cols in COLS {
        for (what, top) in top_rules(cols) {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let session = app.focused_session_id(wid).expect("the front session");
            let _ = app.apply_term_resize(wid, 24, cols as u16);
            bytes(&app, session, screen_with(cols, &top).as_bytes());
            refill(&mut app, wid);
            let ws = &app.windows[&wid];
            assert_eq!(
                (usize::from(ws.rows), usize::from(ws.cols)),
                (24, cols),
                "the pane's size"
            );
            let scratch = &ws.input_scratch;
            let region = PaneTextRegion {
                strip: scratch.cells.len() - usize::from(ws.rows),
                row_off: 0,
                col_off: 0,
                rows: 24,
                cols,
            };
            let blank = scratch.implicit_blank;
            let before = scratch.cells[region.strip + 20].clone();
            for program in MARKED {
                let muse = program == Program::Muse;
                let Some(run) = read_top_run(scratch, region, 20, TopLine::Rule, blank, muse)
                else {
                    continue;
                };
                assert!(
                    run.tail.is_none() || (what == "tagged" && !muse),
                    "{cols} cols, {what}, {program:?}: a label zone only where Claude drew one"
                );
                let tail: &[RenderCell] = run
                    .tail
                    .filter(|t| t.plain)
                    .map_or(&[], |t| &before[t.col..t.col + t.len]);
                for (switch, palette) in CASES {
                    let at = format!("{cols} cols, {what}, {program:?}, {switch:?}, {palette:?}");
                    let mut top = ComposedTop::default();
                    let key = TopKey {
                        frame_row: region.strip + 20,
                        ..key_for(program, run, switch, palette, blank, KeyFace::default())
                    };
                    compose_top(&key, &mut top, None, None, tail);
                    let (laid, _) = laid_run(&key);
                    if what.starts_with("voice label") {
                        let mut after = before.clone();
                        for (k, cell) in top.cells.iter().enumerate() {
                            after[top.col + k] = *cell;
                        }
                        let label = LABEL.min(before.len());
                        let left: String = after[..label].iter().map(|c| c.ch).collect();
                        let was: String = before[..label].iter().map(|c| c.ch).collect();
                        if muse && !top.lockup.is_empty() {
                            assert_eq!(what, "voice label", "{at}: never the cut label");
                            if cols < 40 {
                                narrow.insert(cols);
                            }
                            assert_eq!(
                                laid,
                                TopRun {
                                    col: 0,
                                    len: cols,
                                    end: cols,
                                    ..run
                                },
                                "{at}"
                            );
                            assert!(top.lockup.start <= 2, "{at}: at the rule's left");
                            assert!(
                                !left.contains("Voice") && !left.contains("start"),
                                "{at}: the label's cells written over: {left:?}"
                            );
                            voiced += 1;
                        } else {
                            assert_eq!(laid, run, "{at}: the plain run after the label");
                            assert_eq!(left, was, "{at}: the label as Muse drew it");
                        }
                    } else {
                        assert_eq!(laid, run, "{at}: no label to take");
                    }
                    assert!(
                        top.cells.is_empty()
                            || (top.col >= laid.col && top.col + top.cells.len() <= laid.end),
                        "{at}: every cell written lies in the rule"
                    );
                    let span = laid.span(KeyFace::default());
                    let lockup = cli_mark(program).map(|m| m.lockup_cells(Build::Vendor));
                    let want = lay_top(span, switch, lockup);
                    assert_eq!(
                        top.switch.is_some(),
                        want.as_ref().is_some_and(|l| l.face.is_some()),
                        "{at}: a switch where the ladder lays one"
                    );
                    if let (Some(s), Some(want)) = (&top.switch, &want) {
                        let face = want.face.as_ref().expect("the laid switch");
                        let rung = want.rung;
                        let field = laid.col + field_len(span, &rung);
                        let end = field - rung.close - want.label.as_ref().map_or(0, |l| 1 + l.len);
                        assert_eq!(s.cols, end - face.len..end, "{at}: from the right end");
                        // What the row shows once the write lands: its cells, the vendor's
                        // where it writes none.
                        let written = top.col..top.col + top.cells.len();
                        let text = |r: std::ops::Range<usize>| -> String {
                            r.map(|c| {
                                if written.contains(&c) {
                                    top.cells[c - top.col].ch
                                } else {
                                    before[c].ch
                                }
                            })
                            .collect()
                        };
                        assert_eq!(text(s.cols.clone()), face.text(), "{at}: the switch laid");
                        if let Some(label) = &want.label {
                            let cols = end + 1..end + 1 + label.len;
                            assert_eq!(text(cols.clone()), label.text(), "{at}: the label laid");
                            assert_eq!(s.label, Some((cols, rung.label.expect("a tier"))));
                        }
                        assert_eq!(
                            text(field - 2..field),
                            "\u{2500}\u{2500}",
                            "{at}: the close's `\u{2500}\u{2500}`"
                        );
                        if rung.field == Field::End && !tail.is_empty() {
                            let slot = s.cols.end - SWITCH_SLOT - SLOT_RULE;
                            assert_eq!(
                                &top.cells[slot - tail.len() - top.col..slot - top.col],
                                tail,
                                "{at}: Claude's zone, cell for cell"
                            );
                            assert_eq!(
                                text(slot..slot + SLOT_RULE),
                                "\u{2500}\u{2500}",
                                "{at}: `\u{2500}\u{2500}` into the switch"
                            );
                            moved += 1;
                        }
                        drawn += 1;
                    }
                    if !top.lockup.is_empty() {
                        let word = cli_mark(program).expect("a mark").word();
                        let text: String = top.cells
                            [top.lockup.start - top.col..top.lockup.end - top.col]
                            .iter()
                            .map(|c| c.ch)
                            .collect();
                        assert!(
                            text == "    " || text == format!("    {word} "),
                            "{at}: the lockup {text:?}"
                        );
                        assert!(
                            top.lockup.start == laid.col + 2 || top.lockup.start == laid.col + 1,
                            "{at}: two cells in, one tight"
                        );
                        assert_eq!(top.icon, Some(top.lockup.start + 1), "{at}: the icon");
                        lockups += 1;
                    }
                }
            }
            if what == "plain"
                && let Some(run) = read_top_run(scratch, region, 20, TopLine::Rule, blank, false)
            {
                let key = |len| TopKey {
                    frame_row: region.strip + 20,
                    ..key_for(
                        Program::Claude,
                        TopRun {
                            len,
                            end: run.col + len,
                            ..run
                        },
                        Some(LIT),
                        Some(CardPalette::Cruise),
                        blank,
                        KeyFace::default(),
                    )
                };
                let (mut whole, mut short) = (ComposedTop::default(), ComposedTop::default());
                compose_top(&key(run.len), &mut whole, None, None, &[]);
                compose_top(&key(run.len - 1), &mut short, None, None, &[]);
                assert_ne!(
                    whole.cells, short.cells,
                    "CONTROL {cols}: a shorter run composes otherwise"
                );
            }
        }
    }
    assert!(drawn > 300, "most compositions lay a switch: {drawn}");
    assert!(
        moved > 20,
        "Claude's zone moved left of the switch: {moved}"
    );
    assert!(lockups > 600, "most compositions lay the lockup: {lockups}");
    assert!(voiced > 0, "Muse Code's lockup took its label: {voiced}");
    assert_eq!(
        narrow.into_iter().collect::<Vec<_>>(),
        [32, 33, 34, 36],
        "Muse Code's lockup takes its label in a narrow pane"
    );
}

fn glass_chars(app: &App, wid: WindowId, row: usize) -> String {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip + row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn top_cost(app: &mut App, wid: WindowId) -> (usize, usize, usize) {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_footer_top_reads(),
        crate::work_counts::take_footer_tops_composed(),
    );
    let (_, allocations) = crate::work_counts::allocations_in(|| pass_on(app, wid, &plan, route));
    (
        allocations,
        crate::work_counts::take_footer_top_reads(),
        crate::work_counts::take_footer_tops_composed(),
    )
}

fn held(app: &mut App, session: u64) -> Hosted {
    let host = hosted(app);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    host
}

fn held_glass() -> (App, WindowId, u64, Hosted) {
    let (mut app, wid, session) = probe_glass();
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_agent_reader_for_test(true);
    }
    let host = held(&mut app, session);
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    (app, wid, session, host)
}

#[test]
fn the_top_rule_at_rest_costs_nothing_with_the_harness_on() {
    let (mut app, wid, session, _host) = held_glass();
    let top = AT_REST.bottom() - 2;
    let at_rest = glass_chars(&app, wid, top);
    assert!(
        at_rest.starts_with("\u{2500}\u{2500}    claude \u{2500}")
            && at_rest.ends_with(&format!(
                "\u{2500} \u{25cf} on  alab claude harness v{} (v2.1.283) \u{2500}\u{2500}",
                aterm_phase::CliHarness::ClaudeCode.version_str()
            )),
        "the lockup and the lit switch are on the top rule: {at_rest:?}"
    );
    for i in 0..30 {
        assert_eq!(top_cost(&mut app, wid), (0, 0, 0), "frame {i}");
        assert_eq!(
            glass_chars(&app, wid, top),
            at_rest,
            "frame {i}: the same glass"
        );
    }
    for i in 0..4 {
        keystroke(&app, session, i);
        refill(&mut app, wid);
        let (_, reads, composed) = top_cost(&mut app, wid);
        assert_eq!(
            (reads, composed),
            (0, 0),
            "keystroke {i}: the top rule kept"
        );
        assert_eq!(glass_chars(&app, wid, top), at_rest, "keystroke {i}");
    }
    let cols = usize::from(COLS) - 30;
    assert!(app.apply_term_resize(wid, ROWS, cols as u16));
    feed(
        &app,
        session,
        &screen(&AT_REST)
            .into_iter()
            .map(|row| row.chars().take(cols).collect())
            .collect::<Vec<String>>(),
    );
    refill(&mut app, wid);
    let (_, reads, composed) = top_cost(&mut app, wid);
    assert_eq!(
        (reads, composed),
        (1, 1),
        "CONTROL: narrowed, read and composed once"
    );
    let narrowed = glass_chars(&app, wid, top);
    assert!(
        narrowed.contains("alab claude harness") && narrowed.chars().count() == cols,
        "CONTROL: the switch laid anew at {cols}: {narrowed:?}"
    );
    assert_eq!(top_cost(&mut app, wid), (0, 0, 0), "CONTROL: at rest again");
    let (mut fresh, fresh_wid, fresh_session) = probe_glass();
    {
        let entry = fresh.pool.get(fresh_session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_agent_reader_for_test(true);
    }
    let _fresh_host = held(&mut fresh, fresh_session);
    refill(&mut fresh, fresh_wid);
    let (_, reads, composed) = top_cost(&mut fresh, fresh_wid);
    assert_eq!(
        (reads, composed),
        (1, 1),
        "CONTROL: the first frame with Claude"
    );
}

fn marks_on(app: &App, wid: WindowId, session: u64) -> Vec<aterm_core::render::FxLayer> {
    app.windows[&wid]
        .input_scratch
        .fx_layers
        .iter()
        .filter(|l| l.id == top_mark_id(session))
        .cloned()
        .collect()
}

#[test]
fn a_settled_mark_builds_nothing() {
    let (mut app, wid, session, _host) = held_glass();
    let first = marks_on(&app, wid, session);
    assert_eq!(first.len(), 1, "one mark layer on the glass");
    let _ = crate::work_counts::take_footer_tops_composed();
    for i in 0..6 {
        if i % 2 == 0 {
            refill(&mut app, wid);
        }
        let before = aterm_core::render::fx_counts_here();
        let _ = pass(&mut app, wid);
        assert_eq!(
            aterm_core::render::fx_counts_here(),
            before,
            "frame {i}: no content sealed or copied"
        );
        assert_eq!(
            crate::work_counts::take_footer_tops_composed(),
            0,
            "frame {i}: nothing composed"
        );
        let now = marks_on(&app, wid, session);
        assert_eq!(now.len(), 1, "frame {i}: one mark layer");
        assert!(
            Arc::ptr_eq(&now[0].content, &first[0].content) && now[0] == first[0],
            "frame {i}: the same content at the same place"
        );
    }
    let px = app.windows[&wid].metrics.font_px;
    app.windows.get_mut(&wid).expect("window").metrics.font_px = px * 2.0;
    assert_ne!(app.win_cell_size(wid), (7, 14), "CONTROL: a new cell");
    let before = aterm_core::render::fx_counts_here();
    let _ = pass(&mut app, wid);
    let after = aterm_core::render::fx_counts_here();
    assert_eq!(
        (
            after.0 - before.0,
            after.1 - before.1,
            crate::work_counts::take_footer_tops_composed()
        ),
        (1, 0, 1),
        "CONTROL: one composition, one content sealed: the mark's (the switch has none)"
    );
    let moved = marks_on(&app, wid, session);
    assert!(
        moved.len() == 1 && !Arc::ptr_eq(&moved[0].content, &first[0].content),
        "CONTROL: the mark laid for the new cell"
    );
    let before = aterm_core::render::fx_counts_here();
    let _ = pass(&mut app, wid);
    assert_eq!(
        (
            aterm_core::render::fx_counts_here(),
            crate::work_counts::take_footer_tops_composed()
        ),
        (before, 0),
        "CONTROL: at rest again"
    );
}

fn plain_run(len: usize) -> TopRun {
    TopRun {
        col: 2,
        len,
        rule: RenderCell {
            ch: '\u{2500}',
            fg: [90, 90, 90],
            ..RenderCell::default()
        },
        voice: None,
        tail: None,
        end: 2 + len,
    }
}

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [220, 220, 220],
        ..RenderCell::default()
    }
}

fn composed_with(
    (program, build): (aterm_phase::Program, Build),
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
    tail: &[RenderCell],
) -> ComposedTop {
    let mut top = ComposedTop::default();
    compose_top(
        &TopKey {
            build,
            ..key_for(program, run, switch, palette, blank(), face)
        },
        &mut top,
        None,
        None,
        tail,
    );
    top
}

fn composed_as(
    program: (aterm_phase::Program, Build),
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
) -> ComposedTop {
    composed_with(program, run, switch, palette, face, &[])
}

fn composed(
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
) -> ComposedTop {
    composed_as(
        (aterm_phase::Program::Claude, Build::Vendor),
        run,
        switch,
        palette,
        face,
    )
}

/// The line `top` draws over `run`, the rule's own `─` where it writes nothing.
fn line_of(top: &ComposedTop, run: TopRun) -> Vec<char> {
    let mut line = vec!['\u{2500}'; run.end - run.col];
    for (k, c) in top.cells.iter().enumerate() {
        if let Some(slot) = line.get_mut(top.col + k - run.col) {
            *slot = c.ch;
        }
    }
    line
}

const CLAUDE: (aterm_phase::Program, Build) = (aterm_phase::Program::Claude, Build::Vendor);

const GHOST: KeyFace = KeyFace {
    ghost: Some(false),
    tag: None,
    mark: None,
};

/// THE GHOST SITS LEFT OF THE CAPSULE: where Claude Code draws no tag, aterm's top-effort `off`
/// ghost takes the tail slot, `──` before the switch, in Claude's gray; where only the last
/// resort fits, the ghost stays at the run's end with the switch two `─` before it; with no
/// host, or no rung, the ghost alone stands at the run's end.
#[test]
fn the_ghost_sits_left_of_the_switch() {
    let ghost_text = format!(" {} off ", aterm_primer::CLAUDE_TOP_EFFORT_KEY);
    let lockup = CliMark::Claude.lockup_cells(Build::Vendor);
    let mut seen = [false; 3];
    for len in (19..=140).rev() {
        let run = plain_run(len);
        let at_end = run.col + len - GHOST_CELLS - 1;
        for (switch, palette) in [
            (LIT, Some(CardPalette::Cruise)),
            (MASTER_OFF, None),
            (TURNING, None),
        ] {
            let top = composed(run, Some(switch), palette, GHOST);
            let at = format!("{len} {:?}", switch.face);
            let text = |r: std::ops::Range<usize>| -> String {
                top.cells[r.start - top.col..r.end - top.col]
                    .iter()
                    .map(|c| c.ch)
                    .collect()
            };
            let (a, b) = top.ghost.expect("the ghost is drawn");
            let ghost = a - 1..b + 1;
            assert_eq!(text(ghost.clone()), ghost_text, "{at}: the ghost whole");
            // Claude's grey, floored to read as words do ([`greyed_on`]).
            let greyed = FactInks::of(run.rule.fg, blank().fg, run.rule.bg).stub;
            for c in ghost.clone() {
                let cell = top.cells[c - top.col];
                assert_eq!(
                    (cell.fg, cell.bold, cell.script),
                    (greyed, false, aterm_core::terminal::Script::Normal),
                    "{at}: the ghost in Claude's gray"
                );
            }
            let laid = lay_top(run.span(GHOST), Some(switch), Some(lockup));
            match laid.map(|l| l.rung.field) {
                Some(Field::End) => {
                    let s = top.switch.clone().expect("the switch");
                    let slot = s.cols.end - SWITCH_SLOT;
                    assert_eq!(
                        text(ghost.end..slot),
                        "\u{2500}\u{2500}",
                        "{at}: `\u{2500}\u{2500}` between the ghost and the switch"
                    );
                    assert_eq!(
                        top.col + top.cells.len(),
                        run.end,
                        "{at}: to the rule's end"
                    );
                    seen[0] = true;
                }
                Some(Field::Run) => {
                    assert_eq!(ghost.start, at_end, "{at}: the ghost at the run's end");
                    let s = top.switch.clone().expect("the switch");
                    assert_eq!(
                        s.cols.end + 2,
                        ghost.start,
                        "{at}: two `\u{2500}` before it"
                    );
                    assert_eq!(
                        top.col + top.cells.len(),
                        run.end - 1,
                        "{at}: Claude's last `\u{2500}` left as is"
                    );
                    seen[1] = true;
                }
                None => {
                    assert_eq!(top.switch, None, "{at}: no switch");
                    assert_eq!(ghost.start, at_end, "{at}: the ghost at the run's end");
                    seen[2] = true;
                }
            }
        }
        let unhosted = composed(run, None, None, GHOST);
        assert_eq!(unhosted.switch, None);
        let text: String = unhosted.cells.iter().map(|c| c.ch).collect();
        assert!(text.ends_with(&ghost_text), "{len}: no host, the ghost");
        assert_eq!(
            unhosted.col,
            if unhosted.lockup.is_empty() {
                at_end
            } else {
                unhosted.lockup.start
            },
            "{len}: no host, from the lockup, or the ghost alone"
        );
    }
    assert_eq!(seen, [true; 3], "moved, the last resort and no rung");
    assert_eq!(
        composed(plain_run(18), None, None, GHOST).ghost,
        None,
        "18: no ghost"
    );
    assert!(
        composed(plain_run(19), None, None, GHOST).ghost.is_some(),
        "19: the ghost"
    );
}

#[test]
fn the_lockup_stands_two_rule_cells_in() {
    let ground = crate::claude_card::pack(blank().bg);
    let mut laid = [0usize; 2];
    let mut controls = 0;
    for (program, build) in LOCKUPS {
        let mark = cli_mark(program).expect("a mark");
        let word: Vec<char> = mark.name(build).chars().collect();
        for (switch, palette) in [
            (Some(LIT), Some(CardPalette::Cruise)),
            (Some(LIT), Some(CardPalette::Off)),
            (Some(MASTER_OFF), None),
            (Some(TURNING), None),
            (None, None),
        ] {
            let lockup_cells = Some(mark.lockup_cells(build));
            for len in 0..=200 {
                let run = plain_run(len);
                let top = composed_as((program, build), run, switch, palette, KeyFace::default());
                let at = format!("{program:?} {build:?} {switch:?} {palette:?} {len}");
                let Some(rung) = lay_top(TopSpan::plain(len), switch, lockup_cells).map(|l| l.rung)
                else {
                    assert!(top.cells.is_empty(), "{at}: nothing laid");
                    continue;
                };
                let Some(lockup) = rung.lockup else {
                    assert!(top.lockup.is_empty() && top.icon.is_none(), "{at}");
                    continue;
                };
                let start = run.col + rung.lead;
                let cells = match lockup {
                    Lockup::Full => mark.lockup_cells(build),
                    Lockup::Icon => aterm_effects::cli_marks::ICON_RUN,
                };
                assert_eq!(top.lockup, start..start + cells, "{at}: the lockup's cells");
                assert_eq!(top.icon, Some(start + 1), "{at}: the icon's first cell");
                let line = line_of(&top, run);
                assert!(
                    line[..rung.lead].iter().all(|&c| c == '\u{2500}'),
                    "{at}: the lead is the rule's"
                );
                let (limit, gap) = match &top.switch {
                    Some(s) => (s.cols.end - SWITCH_SLOT, RULE_MIN),
                    None => (run.end, rung.lead),
                };
                assert!(
                    limit >= top.lockup.end + gap,
                    "{at}: {} cells after the lockup",
                    limit.saturating_sub(top.lockup.end)
                );
                let cell = |c: usize| top.cells[c - top.col];
                for c in start..start + 4 {
                    assert_eq!(cell(c), RenderCell { ch: ' ', ..blank() }, "{at}: a blank");
                }
                if lockup == Lockup::Full {
                    for (k, &ch) in word.iter().enumerate() {
                        let fg = rgb_of(aterm_effects::cli_marks::name_fg(mark, k, ground));
                        assert_eq!(
                            (
                                cell(start + 4 + k).ch,
                                cell(start + 4 + k).fg,
                                cell(start + 4 + k).bold
                            ),
                            (ch, fg, true),
                            "{at}: letter {k}"
                        );
                    }
                    assert_eq!(cell(start + cells - 1).ch, ' ', "{at}: the trailing pad");
                    laid[0] += 1;
                    let (rungs, n) = ladder(TopSpan::plain(len), switch, lockup_cells);
                    let last_full = rungs[..n]
                        .iter()
                        .rev()
                        .find(|r| r.lockup == Some(Lockup::Full))
                        .copied();
                    if len == rung.need && last_full == Some(rung) {
                        controls += 1;
                        let short = composed_as(
                            (program, build),
                            plain_run(rung.need - 1),
                            switch,
                            palette,
                            KeyFace::default(),
                        );
                        let text: String = short.cells.iter().map(|c| c.ch).collect();
                        assert!(
                            !text.contains(mark.name(build)),
                            "CONTROL {at}: a cell short, no name: {text:?}"
                        );
                    }
                } else {
                    laid[1] += 1;
                }
            }
        }
    }
    assert!(
        laid[0] > 1000 && laid[1] > 20,
        "full and icon rungs laid: {laid:?}"
    );
    assert_eq!(controls, LOCKUPS.len() * 5, "a control a lockup and state");
    let alab = CliMark::Codex.lockup_cells(Build::Alab);
    let broken: Vec<usize> = (0..=200)
        .filter(|&len| {
            let top = composed_as(
                (aterm_phase::Program::Codex, Build::Vendor),
                plain_run(len),
                Some(LIT),
                None,
                KeyFace::default(),
            );
            let Some(s) = top.switch else {
                return false;
            };
            top.lockup.end - top.lockup.start == CliMark::Codex.lockup_cells(Build::Vendor)
                && s.cols.end - SWITCH_SLOT < top.lockup.start + alab + RULE_MIN
        })
        .collect();
    assert!(
        broken.len() > 5,
        "CONTROL: the vendor's ladder crowds codex-alab's name: {broken:?}"
    );
}

/// A rung as spec §1's table names it.
fn rung_name(rung: Option<Rung>) -> String {
    let Some(r) = rung else {
        return "nothing".into();
    };
    match (r.switch, r.label, r.lockup, r.field) {
        (false, _, Some(Lockup::Full), _) => "full".into(),
        (false, _, Some(Lockup::Icon), _) if r.lead == 1 => "icon tight".into(),
        (false, _, Some(Lockup::Icon), _) => "icon".into(),
        (false, _, None, _) => "nothing".into(),
        (true, _, _, Field::Run) => "last resort".into(),
        (true, Some(label), Some(Lockup::Icon), _) => format!("T{label} icon"),
        (true, Some(label), _, _) => format!("T{label}"),
        (true, None, Some(Lockup::Full), _) => "T4".into(),
        (true, None, Some(Lockup::Icon), _) => "T4 icon".into(),
        (true, None, None, _) => "T4 bare".into(),
    }
}

/// The first of `rungs` that fits `span`, as [`lay_top`] picks.
fn first_fit(span: TopSpan, rungs: &[Rung]) -> Option<Rung> {
    rungs.iter().copied().find(|r| r.need <= field_len(span, r))
}

/// Walks `rungs` from a wide line to nothing: each is laid in its turn, and what a narrower line
/// gives up never comes back. The words go first (the label's CLI word, `harness`, the CLI's
/// version, the label), then the lockup's name, then its icon; with the ⬆️ laid (`arrow`) the
/// lockup's name goes before the CLI's version.
fn walk_order(
    span_of: &dyn Fn(usize) -> TopSpan,
    widths: std::ops::RangeInclusive<usize>,
    rungs: &[Rung],
    arrow: bool,
) -> Result<(), String> {
    let lockup = rungs.iter().any(|r| r.lockup.is_some());
    let rank = |r: &Rung| {
        [
            r.label.unwrap_or(LABEL_TIERS),
            match r.lockup {
                Some(Lockup::Full) => 0,
                Some(Lockup::Icon) if r.lead == 1 => 2,
                Some(Lockup::Icon) => 1,
                None => 3,
            },
            usize::from(r.field == Field::Run),
        ]
    };
    let mut walked: Vec<Rung> = Vec::new();
    let mut gone = false;
    for w in widths.rev() {
        let Some(now) = first_fit(span_of(w), rungs) else {
            gone = true;
            continue;
        };
        let name = rung_name(Some(now));
        if gone {
            return Err(format!("{w}: {name} came back after nothing"));
        }
        if let Some(&was) = walked.last() {
            if was == now {
                continue;
            }
            let (a, b) = (rank(&was), rank(&now));
            if (0..a.len()).any(|k| b[k] < a[k]) {
                return Err(format!(
                    "{w}: {name} gives back what {} gave up",
                    rung_name(Some(was))
                ));
            }
        }
        let named = if arrow { LABEL_TIERS - 2 } else { LABEL_TIERS };
        if lockup
            && now.switch
            && now.lockup != Some(Lockup::Full)
            && now.label.is_some_and(|tier| tier < named)
        {
            return Err(format!("{w}: {name}: the name went before the label"));
        }
        walked.push(now);
    }
    if walked != rungs {
        let names = |r: &[Rung]| r.iter().map(|r| rung_name(Some(*r))).collect::<Vec<_>>();
        return Err(format!(
            "laid {:?}, the ladder {:?}",
            names(&walked),
            names(rungs)
        ));
    }
    Ok(())
}

#[test]
fn the_ladder_gives_up_words_then_the_name_then_the_icon() {
    let tail = claude_tail();
    let known = |p: SwitchPaint| SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..p
    };
    type SpanOf = fn(usize) -> TopSpan;
    // Below `GHOST_RUN` the ghost is not drawn and the last resort has the run back.
    let lines: [(&str, SpanOf, usize); 4] = [
        ("plain", |w| plain_run(w).span(KeyFace::default()), 0),
        ("ghost", |w| plain_run(w).span(GHOST), GHOST_RUN),
        (
            "tailed",
            |w| tailed_run(w.saturating_sub(12), &claude_tail()).span(KeyFace::default()),
            0,
        ),
        (
            "stuck",
            |w| {
                let run = tailed_run(w.saturating_sub(12), &claude_tail());
                TopRun {
                    tail: run.tail.map(|t| Tail { plain: false, ..t }),
                    ..run
                }
                .span(KeyFace::default())
            },
            0,
        ),
    ];
    let mut walked = 0;
    for mark in CliMark::ALL {
        for build in Build::ALL {
            for switch in [
                Some(LIT),
                Some(known(LIT)),
                Some(OFF),
                Some(known(MASTER_OFF)),
                Some(TURNING),
                Some(SwitchPaint {
                    harness: None,
                    ..LIT
                }),
                Some(arrow_paint(UpState::Ready)),
                Some(arrow_paint(UpState::Held)),
                None,
            ] {
                let arrow = switch.is_some_and(|p| p.up.shows());
                for (line, span_of, least) in lines {
                    let (rungs, n) = ladder(span_of(200), switch, Some(mark.lockup_cells(build)));
                    walk_order(&span_of, least..=260, &rungs[..n], arrow)
                        .unwrap_or_else(|e| panic!("{mark:?} {build:?} {line} {switch:?}: {e}"));
                    walked += 1;
                }
            }
        }
    }
    assert_eq!(walked, CliMark::ALL.len() * Build::ALL.len() * 9 * 4);
    let span_of = |w: usize| tailed_run(w.saturating_sub(12), &tail).span(KeyFace::default());
    let (rungs, n) = ladder(
        span_of(200),
        Some(known(LIT)),
        Some(CliMark::Claude.lockup_cells(Build::Vendor)),
    );
    let mut skipped: Vec<Rung> = rungs[..n].to_vec();
    let icon = skipped
        .iter()
        .position(|r| rung_name(Some(*r)) == "T4 icon")
        .expect("the icon's rung");
    let rung = skipped.remove(icon);
    skipped.insert(icon - 2, rung);
    assert!(
        walk_order(&span_of, 0..=260, &skipped, false).is_err_and(|e| e.contains("laid")),
        "CONTROL: a rung the walk never lays is caught"
    );
    let mut early: Vec<Rung> = rungs[..n].to_vec();
    early[3].lockup = Some(Lockup::Icon);
    assert!(
        walk_order(&span_of, 0..=260, &early, false).is_err_and(|e| e.contains("before the label")),
        "CONTROL: the name dropped before the label is caught"
    );
    let (rungs, n) = ladder(
        span_of(200),
        Some(arrow_paint(UpState::Ready)),
        Some(CliMark::Claude.lockup_cells(Build::Vendor)),
    );
    let mut early: Vec<Rung> = rungs[..n].to_vec();
    early[1].lockup = Some(Lockup::Icon);
    assert!(
        walk_order(&span_of, 0..=260, &early, true).is_err_and(|e| e.contains("before the label")),
        "CONTROL: with the arrow, the name dropped before T2 is caught"
    );
}

/// SPEC §1'S TABLE: Claude Code with the top-effort key's tag and v2.1.305 lays each rung from the width the
/// table names, the next one a column short; a ghost adds 4 to every need, no tail takes 13 off;
/// below the last resort, nothing.
#[test]
fn the_ladder_lays_the_specs_tiers_at_the_specs_widths() {
    let tail = claude_tail();
    let paint = SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..LIT
    };
    let lockup = Some(CliMark::Claude.lockup_cells(Build::Vendor));
    let rung_at = |span: TopSpan| rung_name(lay_top(span, Some(paint), lockup).map(|l| l.rung));
    let tailed = |w: usize| tailed_run(w - 12, &tail).span(KeyFace::default());
    let ghost = |w: usize| plain_run(w).span(GHOST);
    let plain = |w: usize| plain_run(w).span(KeyFace::default());
    let table = [
        ("T0", 75),
        ("T1", 68),
        ("T2", 60),
        ("T3", 49),
        ("T4", 39),
        ("T4 icon", 32),
        ("T4 bare", 26),
    ];
    for (name, need) in table {
        assert_eq!(rung_at(tailed(need)), name, "{name} at {need}");
        assert_ne!(rung_at(tailed(need - 1)), name, "{name}: a column short");
        assert_eq!(rung_at(ghost(need + 4)), name, "{name}: the ghost's 4 more");
        assert_ne!(
            rung_at(ghost(need + 3)),
            name,
            "{name}: the ghost, a column short"
        );
        assert_eq!(rung_at(plain(need - 13)), name, "{name}: no tail, 13 fewer");
        assert_ne!(
            rung_at(plain(need - 14)),
            name,
            "{name}: no tail, a column short"
        );
    }
    assert_eq!(
        rung_at(tailed(80)),
        "T0",
        "the 80-column pane shows the whole label"
    );
    // With the ⬆️'s 3 cells the lockup's name goes before the CLI's build: a 60-column pane keeps
    // the running build beside the offer (`alab v0.4 (v2.1.305)`, the lockup's icon alone).
    // CONTROL: the order without the name's rung lays T3, the build gone.
    let up = SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..arrow_paint(UpState::Ready)
    };
    let up_at = |w: usize| rung_name(lay_top(tailed(w), Some(up), lockup).map(|l| l.rung));
    assert_eq!(up_at(63), "T2", "the arrow's 3 more");
    assert_eq!(up_at(62), "T2 icon", "a column short: the name goes first");
    assert_eq!(up_at(60), "T2 icon", "60 columns keep the build");
    let (rungs, n) = ladder(tailed(60), Some(up), lockup);
    let full = CliMark::Claude.lockup_cells(Build::Vendor);
    let named: Vec<Rung> = rungs[..n]
        .iter()
        .map(|r| match (r.label, r.lockup) {
            (Some(_), Some(Lockup::Icon)) => Rung {
                lockup: Some(Lockup::Full),
                need: r.need + full - aterm_effects::cli_marks::ICON_RUN,
                ..*r
            },
            _ => *r,
        })
        .collect();
    assert_eq!(
        rung_name(first_fit(tailed(60), &named)),
        "T3",
        "CONTROL: the name kept, the build goes at 60"
    );
    for w in 22..=25 {
        assert_eq!(rung_at(tailed(w)), "last resort", "{w}: in the run");
    }
    assert_eq!(rung_at(tailed(21)), "nothing", "below the last resort");
    let off = SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..MASTER_OFF
    };
    let laid = lay_top(tailed(23), Some(off), lockup).expect("the last resort");
    assert_eq!(
        (rung_name(Some(laid.rung)), laid.rung.need),
        ("last resort".to_owned(), 11),
        "the off switch in the run: 11"
    );
    assert!(lay_top(tailed(22), Some(off), lockup).is_none());
}

/// The narrowest switch hb-int laid for `paint`, as it read.
fn hb_int_narrowest(paint: SwitchPaint) -> &'static str {
    match paint.face {
        HarnessSwitch::On => " \u{25cf} on ",
        HarnessSwitch::Off(_) => " \u{2590} on \u{258c} ",
        HarnessSwitch::Turning => " \u{25d0} on ",
        HarnessSwitch::TurningOff => " \u{25d0} off ",
    }
}

#[test]
fn a_switch_stands_wherever_it_stood() {
    for (program, build) in [
        (aterm_phase::Program::Claude, Build::Vendor),
        (aterm_phase::Program::Agy, Build::Vendor),
        (aterm_phase::Program::Muse, Build::Vendor),
        (aterm_phase::Program::Codex, Build::Alab),
    ] {
        let mark = cli_mark(program).expect("a mark");
        for paint in [LIT, OFF, MASTER_OFF, TURNING, TURNING_OFF] {
            let narrowest = hb_int_narrowest(paint).chars().count();
            let compact = switch_face(paint).len;
            assert!(compact <= narrowest, "{paint:?}: never wider than hb-int's");
            for span in 0..=200 {
                let stood = narrowest + 4 <= span;
                let laid = lay_top(
                    TopSpan::plain(span),
                    Some(paint),
                    Some(mark.lockup_cells(build)),
                );
                let at = format!("{program:?} {build:?} {paint:?} {span}");
                let stands = laid.as_ref().is_some_and(|l| l.face.is_some());
                assert!(stands || !stood, "{at}: a switch where one stood");
                assert_eq!(stands, compact + 4 <= span, "{at}: the last resort's cells");
                if let Some(laid) = laid {
                    let face = laid.face.expect("the switch");
                    let top = composed_as(
                        (program, build),
                        plain_run(span),
                        Some(paint),
                        None,
                        KeyFace::default(),
                    );
                    let end = plain_run(span).col + span
                        - laid.rung.close
                        - laid.label.as_ref().map_or(0, |l| 1 + l.len);
                    assert_eq!(
                        top.switch.map(|s| s.cols),
                        Some(end - face.len..end),
                        "{at}: at its close"
                    );
                    if laid.rung.field == Field::Run {
                        assert_eq!(end, plain_run(span).col + span - 2, "{at}: span - 2 - len");
                    }
                }
            }
        }
    }
    for (paint, lost) in [(LIT, 10..=18), (OFF, 11..=18)] {
        let span = TopSpan::plain(200);
        let (rungs, n) = ladder(
            span,
            Some(paint),
            Some(CliMark::Claude.lockup_cells(Build::Vendor)),
        );
        let without: Vec<Rung> = rungs[..n]
            .iter()
            .copied()
            .filter(|r| r.lockup.is_some())
            .collect();
        let missing: Vec<usize> = (0..=200)
            .filter(|&w| {
                first_fit(TopSpan::plain(w), &rungs[..n]).is_some()
                    && first_fit(TopSpan::plain(w), &without).is_none()
            })
            .collect();
        assert_eq!(
            missing,
            lost.collect::<Vec<_>>(),
            "CONTROL {paint:?}: the spans the bare rungs hold"
        );
    }
}

/// THE KEY'S MARK STANDS TWO CELLS BEFORE ITS TAG ON EVERY RUNG: where Claude's label zone moved
/// left of the switch, before the moved tag; where it stayed, before Claude's own. CONTROL: once
/// the zone moved, the grid's column is no longer the mark's.
#[test]
fn the_mark_cell_survives_every_rung() {
    let tail = claude_tail();
    let key = aterm_primer::CLAUDE_TOP_EFFORT_KEY;
    let mut checked = [0usize; 2];
    for (program, build) in LOCKUPS {
        for (switch, palette) in CASES {
            for len in 6..=200 {
                let run = tailed_run(len, &tail);
                let tag = tag_in(len);
                let face = KeyFace {
                    ghost: None,
                    tag: Some(tag),
                    mark: Some('\u{25d0}'),
                };
                let top = composed_with((program, build), run, switch, palette, face, &tail);
                if top.cells.is_empty() {
                    continue;
                }
                let at = format!("{program:?} {build:?} {switch:?} {len}");
                let cell = |c: usize| top.cells.get(c.checked_sub(top.col)?).copied();
                let now = top.tag.unwrap_or(tag);
                assert_eq!(
                    cell(now.0 - 2).map(|c| (c.ch, c.fg)),
                    Some(('\u{25d0}', [0xd7, 0x77, 0x57])),
                    "{at}: the mark, in the accent"
                );
                if top.tag.is_some() {
                    let word: String = (now.0..now.1).filter_map(cell).map(|c| c.ch).collect();
                    assert_eq!(word, key, "{at}: the moved tag whole");
                    assert_eq!(cell(now.0 - 1).map(|c| c.ch), Some(' '), "{at}: its blank");
                    checked[0] += 1;
                } else {
                    checked[1] += 1;
                }
            }
        }
    }
    assert!(checked[0] > 3000 && checked[1] > 500, "{checked:?}");
    let len = 120;
    let top = composed_with(
        CLAUDE,
        tailed_run(len, &tail),
        Some(LIT),
        Some(CardPalette::Cruise),
        KeyFace {
            ghost: None,
            tag: Some(tag_in(len)),
            mark: Some('\u{25d0}'),
        },
        &tail,
    );
    assert!(top.tag.is_some(), "CONTROL: the zone moved");
    assert_ne!(
        top.cells[tag_in(len).0 - 2 - top.col].ch,
        '\u{25d0}',
        "CONTROL: the grid's column holds the harness now"
    );
}

fn tint_at(palette: CardPalette, t: f32) -> [u8; 3] {
    let dark = crate::claude_card::dark_ground(blank());
    let lifted = mix(rgb_of(palette.art(t, dark)), blank().fg, 0.25);
    let fg = mix([90, 90, 90], lifted, 0.68);
    if fg == [90, 90, 90] {
        [fg[0] ^ 1, fg[1], fg[2]]
    } else {
        fg
    }
}

/// Whether `top`'s rule flows into its switch under `palette`: the rule after the lockup at the
/// arc's end, the last of `gap` at its start, the `─` from there to the switch at its start, and
/// the close at its end again.
fn flows(
    top: &ComposedTop,
    palette: CardPalette,
    gap: std::ops::Range<usize>,
) -> Result<(), String> {
    let at = |c: usize| top.cells[c - top.col];
    let s = top.switch.as_ref().ok_or("no switch")?;
    let end = top.col + top.cells.len();
    if at(gap.start).fg != tint_at(palette, 1.0) {
        return Err("the rule after the lockup is not t = 1".into());
    }
    if at(gap.end - 1).fg != tint_at(palette, 0.0) {
        return Err(format!(
            "the gap's last cell is not t = 0: {:?}",
            at(gap.end - 1).fg
        ));
    }
    for c in gap.end..s.cols.start {
        if at(c).ch == '\u{2500}' && at(c).fg != tint_at(palette, 0.0) {
            return Err(format!(
                "the `\u{2500}` at {c} into the switch is not t = 0"
            ));
        }
    }
    for c in end - 2..end {
        if at(c).fg != tint_at(palette, 1.0) {
            return Err("the close is not t = 1".into());
        }
    }
    Ok(())
}

#[test]
fn the_rule_flows_into_the_switch() {
    let tail = claude_tail();
    for (what, run, cells) in [
        ("plain", plain_run(68), &[][..]),
        ("tailed", tailed_run(68, &tail), &tail[..]),
    ] {
        let top_of = |paint: SwitchPaint, palette| {
            composed_with(
                CLAUDE,
                run,
                Some(paint),
                Some(palette),
                KeyFace::default(),
                cells,
            )
        };
        let cruise = top_of(LIT, CardPalette::Cruise);
        let land = top_of(LIT, CardPalette::Land);
        let off = top_of(LIT, CardPalette::Off);
        let turning_off = top_of(TURNING_OFF, CardPalette::Off);
        // The arc runs to the face itself, or to Claude's zone where that moved before it.
        let gap_of = |top: &ComposedTop| {
            let s = top.switch.as_ref().expect("the switch");
            let next = if cells.is_empty() {
                s.cols.start
            } else {
                s.cols.end - SWITCH_SLOT - SLOT_RULE - cells.len()
            };
            top.lockup.end..next
        };
        for (top, palette, paint) in [
            (&cruise, CardPalette::Cruise, LIT),
            (&land, CardPalette::Land, LIT),
            (&off, CardPalette::Off, LIT),
            (&turning_off, CardPalette::Off, TURNING_OFF),
        ] {
            let at = format!("{what} {palette:?} {:?}", paint.face);
            let gap = gap_of(top);
            flows(top, palette, gap.clone()).unwrap_or_else(|e| panic!("{at}: {e}"));
            assert_eq!(top.col, run.col, "{at}: the whole span");
            assert_eq!(
                top.col + top.cells.len(),
                run.end,
                "{at}: to the rule's end"
            );
            for c in 0..2 {
                assert_eq!(top.cells[c].fg, tint_at(palette, 1.0), "{at}: the lead");
            }
            let g = gap.len() as f32 - 1.0;
            for (j, c) in gap.clone().enumerate() {
                assert_eq!(
                    top.cells[c - top.col].fg,
                    tint_at(palette, 1.0 - j as f32 / g),
                    "{at}: gap cell {j}"
                );
            }
            let s = top.switch.clone().expect("the switch");
            let face = switch_face(paint);
            for (k, &ch) in face.cells().iter().enumerate() {
                let cell = top.cells[s.cols.start + k - top.col];
                assert_eq!(
                    (cell.ch, cell.bg, cell.bold),
                    (ch, blank().bg, false),
                    "{at}: switch cell {k}: the ground, never bold"
                );
            }
            let (label, _) = s.label.clone().expect("the label");
            let brand = FaceInks::of(&paint, Some(palette), blank(), [0xd7, 0x77, 0x57]).brand;
            for c in label.start..label.start + 4 {
                let cell = top.cells[c - top.col];
                assert_eq!((cell.fg, cell.bold), (brand, true), "{at}: `alab`");
            }
        }
        let layout = |t: &ComposedTop| t.cells.iter().map(|c| c.ch).collect::<String>();
        assert_eq!(layout(&cruise), layout(&land), "{what}: one layout");
        assert_eq!(layout(&cruise), layout(&off), "{what}: Autopilot Off's too");
        let s = cruise.switch.clone().expect("Cruise's switch");
        for (k, (a, b)) in cruise.cells.iter().zip(&land.cells).enumerate() {
            let col = cruise.col + k;
            if a.ch == '\u{2500}' && !cruise.lockup.contains(&col) {
                assert_ne!(a.fg, b.fg, "{what} cell {k}: Cruise and Land's rules apart");
            }
            if s.cols.contains(&col) {
                assert_eq!(a, b, "{what} cell {k}: the switch keeps the pane's inks");
            }
        }
        let mut old = cruise.cells.clone();
        let gap = gap_of(&cruise);
        let g = gap.len() as f32 - 1.0;
        for (j, c) in gap.clone().enumerate() {
            old[c - cruise.col].fg = tint_at(CardPalette::Cruise, j as f32 / g);
        }
        let reversed = ComposedTop {
            cells: old,
            switch: cruise.switch.clone(),
            lockup: cruise.lockup.clone(),
            col: cruise.col,
            ..ComposedTop::default()
        };
        assert!(
            flows(&reversed, CardPalette::Cruise, gap).is_err(),
            "CONTROL {what}: the left-to-right walk leaves a seam"
        );
    }
}

/// A SHORT GAP RAMPS TOO: where the pane leaves only a few `─` between the lockup and the
/// harness (2 to 7 cells), they still run the arc from its end at the lockup to its start at the
/// switch, one step a cell, as a wide gap does. CONTROL: the gap in one ink, the arc's start,
/// does not flow.
#[test]
fn a_short_gap_ramps_too() {
    let mut short = std::collections::BTreeSet::new();
    for len in 20..68 {
        let run = plain_run(len);
        let top = composed(
            run,
            Some(LIT),
            Some(CardPalette::Cruise),
            KeyFace::default(),
        );
        let Some(s) = top.switch.as_ref() else {
            continue;
        };
        if top.lockup.is_empty() {
            continue;
        }
        let gap = top.lockup.end..s.cols.start;
        let g = gap.len();
        if !(2..8).contains(&g) {
            continue;
        }
        short.insert(g);
        let at = format!("run {len}, gap {g}");
        flows(&top, CardPalette::Cruise, gap.clone()).unwrap_or_else(|e| panic!("{at}: {e}"));
        for (j, c) in gap.clone().enumerate() {
            assert_eq!(
                top.cells[c - top.col].fg,
                tint_at(CardPalette::Cruise, 1.0 - j as f32 / (g - 1) as f32),
                "{at}: gap cell {j}"
            );
        }
        let mut flat = top.cells.clone();
        for c in gap.clone() {
            flat[c - top.col].fg = tint_at(CardPalette::Cruise, 0.0);
        }
        let flat = ComposedTop {
            cells: flat,
            switch: top.switch.clone(),
            lockup: top.lockup.clone(),
            col: top.col,
            ..ComposedTop::default()
        };
        assert!(
            flows(&flat, CardPalette::Cruise, gap).is_err(),
            "CONTROL {at}: one ink does not flow"
        );
    }
    assert!(
        short.len() >= 3,
        "the walk meets short gaps of several widths: {short:?}"
    );
}

/// CLAUDE'S TAG READS ON A LIGHT PAGE: the tag moved left of the switch keeps its hue and reads
/// as words do on the ground it is laid on: Claude's dark-theme purple (`#af87ff`, 2.5:1 on
/// Default Light's page) floored to 4.5:1 there, the zone's blanks as Claude drew them.
/// CONTROL: on a dark ground the moved cells are Claude's, byte for byte.
#[test]
fn claudes_tag_reads_on_a_light_page() {
    use aterm_effects::cli_marks::NAME_CONTRAST;
    use aterm_effects::color_math::contrast_ratio;
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let len = 60;
    for (ground, light) in [([0xf6, 0xf7, 0xf9], true), ([0x11, 0x13, 0x18], false)] {
        let tail: Vec<RenderCell> = claude_tail()
            .into_iter()
            .map(|c| RenderCell { bg: ground, ..c })
            .collect();
        let face = KeyFace {
            tag: Some(tag_in(len)),
            ..KeyFace::default()
        };
        let run = tailed_run(len, &tail);
        let top = composed_with(CLAUDE, run, Some(LIT), Some(CardPalette::Land), face, &tail);
        let moved = top.tag.expect("the tag moved");
        let mut floored = 0;
        for (k, claude) in tail.iter().enumerate() {
            let cell = top.cells[moved.0 - 1 + k - top.col];
            if !light || claude.ch == ' ' {
                assert_eq!(cell, *claude, "light {light}: Claude's own cell {k}");
                continue;
            }
            assert!(
                reads(claude.fg, ground) < NAME_CONTRAST,
                "CONTROL: Claude's ink at {:.2}:1",
                reads(claude.fg, ground)
            );
            let k = reads(cell.fg, ground);
            assert!(k >= NAME_CONTRAST, "{:?} at {k:.2}:1", cell.ch);
            let [r, g, b] = cell.fg;
            assert!(
                b > r && r > g,
                "{:?}: the purple's hue kept: {:?}",
                cell.ch,
                cell.fg
            );
            assert_eq!(
                RenderCell {
                    fg: claude.fg,
                    ..cell
                },
                *claude,
                "only the ink moves"
            );
            floored += 1;
        }
        assert_eq!(
            floored,
            if light {
                aterm_primer::CLAUDE_TOP_EFFORT_KEY.len()
            } else {
                0
            }
        );
    }
}

/// THE TAG IS REDRAWN WHOLE, LEFT OF THE SWITCH: a laid switch moves Claude Code's label zone cell
/// for cell, in Claude's own styles, to the tail slot `──` before the switch; the key's tag (its
/// hit's columns), its mark and the rule's last cells follow. A zone whose cells are not at hand,
/// or not plain, stays where Claude drew it, the switch in the run before it; with no host,
/// nothing moves.
#[test]
fn the_tag_is_redrawn_whole_left_of_the_switch() {
    let run = plain_run(60);
    let at = run.len - GHOST_CELLS - 1;
    let pending = composed(
        plain_run(19),
        None,
        None,
        KeyFace {
            mark: Some('\u{25d0}'),
            ..GHOST
        },
    );
    assert_eq!(
        pending.col,
        run.col + 19 - GHOST_CELLS - 2,
        "the mark's cell first"
    );
    assert_eq!(pending.cells[0].ch, '\u{25d0}');
    assert_eq!(pending.cells[0].fg, [0xd7, 0x77, 0x57], "the accent");
    assert_eq!(pending.cells.len(), 1 + GHOST_CELLS);
    let lit = composed(
        run,
        None,
        None,
        KeyFace {
            ghost: Some(true),
            ..GHOST
        },
    );
    for cell in &lit.cells[at - (lit.col - run.col)..] {
        let underlined = cell.underline == aterm_core::terminal::UnderlineStyle::Single;
        assert_eq!(underlined, cell.ch != ' ', "{:?}", cell.ch);
        assert_eq!(cell.fg, value_fg(run.rule.fg, [220, 220, 220]), "lifted");
    }
    let tail = claude_tail();
    let len = 60;
    let run = tailed_run(len, &tail);
    let tag = tag_in(len);
    let selected = KeyFace {
        ghost: None,
        tag: Some(tag),
        mark: Some('\u{25b8}'),
    };
    for (switch, palette) in [
        (LIT, Some(CardPalette::Land)),
        (MASTER_OFF, None),
        (TURNING, None),
    ] {
        let what = format!("{:?}", switch.face);
        let top = composed_with(CLAUDE, run, Some(switch), palette, selected, &tail);
        let s = top.switch.clone().expect("the switch");
        let moved = top.tag.expect("the tag moved");
        assert_eq!(moved.1 - moved.0, tag.1 - tag.0, "{what}: the tag whole");
        assert_eq!(
            moved.1 + 1 + SLOT_RULE,
            s.cols.end - SWITCH_SLOT,
            "{what}: the zone's blank, then `\u{2500}\u{2500}`, then the switch"
        );
        for (k, cell) in tail.iter().enumerate() {
            assert_eq!(
                top.cells[moved.0 - 1 + k - top.col],
                *cell,
                "{what}: Claude's own cell {k}"
            );
        }
        assert_eq!(
            top.cells[moved.0 - 2 - top.col].ch,
            '\u{25b8}',
            "{what}: the mark"
        );
        assert_eq!(
            top.col + top.cells.len(),
            run.end,
            "{what}: to the rule's end"
        );
        let grid: String = top.cells[tag.0 - top.col..tag.1 - top.col]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_ne!(
            grid,
            aterm_primer::CLAUDE_TOP_EFFORT_KEY,
            "{what}: the grid's tag columns are the harness's"
        );
    }
    let stuck = TopRun {
        tail: run.tail.map(|t| Tail { plain: false, ..t }),
        ..run
    };
    for (what, top) in [
        (
            "not plain",
            composed_with(
                CLAUDE,
                stuck,
                Some(LIT),
                Some(CardPalette::Land),
                selected,
                &tail,
            ),
        ),
        (
            "CONTROL: its cells not at hand",
            composed_with(
                CLAUDE,
                run,
                Some(LIT),
                Some(CardPalette::Land),
                selected,
                &[],
            ),
        ),
    ] {
        assert_eq!(top.tag, None, "{what}: the zone stays");
        assert!(top.switch.is_some(), "{what}: the switch in the run");
        assert_eq!(
            top.col + top.cells.len(),
            run.col + len,
            "{what}: the run alone"
        );
        assert_eq!(
            top.cells[tag.0 - 2 - top.col].ch,
            '\u{25b8}',
            "{what}: the mark"
        );
    }
    let unhosted = composed_with(CLAUDE, run, None, None, selected, &tail);
    assert_eq!(unhosted.tag, None, "no host: nothing moves");
    assert_eq!(
        unhosted.col, unhosted.lockup.start,
        "no host: from the lockup"
    );
    assert_eq!(
        unhosted.cells.last().map(|c| c.ch),
        Some('\u{25b8}'),
        "the `\u{2500}` before the tag's blank"
    );
    assert_eq!(
        unhosted.col + unhosted.cells.len(),
        tag.0 - 1,
        "to the mark"
    );
    let rest = composed(
        TopRun {
            len: 5,
            end: plain_run(5).end,
            ..plain_run(5)
        },
        None,
        None,
        KeyFace {
            mark: None,
            ..selected
        },
    );
    assert!(rest.cells.is_empty());
}

#[test]
fn the_mark_stands_only_where_its_write_took() {
    let (mut app, wid, session, _host) = held_glass();
    let ws = &app.windows[&wid];
    let row = ws.input_scratch.cells.len() - usize::from(ws.rows) + AT_REST.bottom() - 2;
    let (cw, ch) = app.win_cell_size(wid);
    let px = |v: usize| i32::try_from(v).expect("small");
    let laid = marks_on(&app, wid, session);
    assert_eq!(laid.len(), 1, "CONTROL: the write took, one layer");
    assert_eq!(
        (laid[0].offset, laid[0].slot, laid[0].space),
        (
            (px(3 * cw), px(row * ch)),
            aterm_core::render::FxSlot::Over,
            aterm_core::render::FxSpace::Grid
        ),
        "CONTROL: over the icon's first cell, two `\u{2500}` and a pad in"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .contains(&laid[0]),
        "CONTROL: in the card's record"
    );
    let ws = app.windows.get_mut(&wid).expect("window");
    assert!(
        crate::claude_footer::undo(&mut ws.claude_footer_undo, &mut ws.input_scratch),
        "the footer's write comes off"
    );
    ws.input_scratch.combining[row].push((40, Box::new(['\u{301}'])));
    let _ = crate::work_counts::take_footer_tops_composed();
    let _ = pass(&mut app, wid);
    assert_eq!(
        crate::work_counts::take_footer_tops_composed(),
        0,
        "the composition stands"
    );
    assert!(
        !app.windows[&wid]
            .claude_footer_undo
            .painted_spans()
            .any(|(r, ..)| r == row),
        "the top line's write was refused"
    );
    assert!(
        marks_on(&app, wid, session).is_empty(),
        "a refused write puts no layer"
    );
    assert!(
        !app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.id == top_mark_id(session)),
        "nor records one"
    );
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    assert_eq!(marks_on(&app, wid, session).len(), 1, "laid again");
    let top = AT_REST.bottom() - 2;
    let term = app.pool.get(session).expect("session").term.clone();
    let select = |grid_row: Option<usize>| {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let mut t = crate::term_lock(&term);
        let selection = t.text_selection_mut();
        selection.clear();
        if let Some(r) = grid_row {
            let r = i32::try_from(r).expect("small");
            selection.start_selection(r, 2, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(r, 40, SelectionSide::Right);
            selection.complete_selection();
        }
    };
    for (what, grid_row, drawn) in [
        ("CONTROL: a transcript row", Some(0), true),
        ("the line", Some(top), false),
        ("CONTROL: let go", None, true),
    ] {
        select(grid_row);
        refill(&mut app, wid);
        assert_eq!(
            app.windows[&wid].input_scratch.any_selection(),
            grid_row.is_some(),
            "{what}: the fill carries the selection"
        );
        let before = app.windows[&wid].input_scratch.cells[row].clone();
        let _ = pass(&mut app, wid);
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.input_scratch.cells[row] != before,
            drawn,
            "{what}: the line written"
        );
        assert_eq!(
            marks_on(&app, wid, session).len(),
            usize::from(drawn),
            "{what}: its layer"
        );
        assert_eq!(
            ws.claude_lights
                .border_toggles
                .iter()
                .any(|b| b.session == session),
            drawn,
            "{what}: its switch"
        );
        assert_eq!(
            ws.claude_lights
                .mark_hits
                .iter()
                .any(|m| m.session == session),
            drawn,
            "{what}: its lockup"
        );
    }
}

#[test]
fn the_mark_leaves_with_every_strip() {
    let glass = |tab_strip: u16| {
        let (mut app, wid, session) = probe_glass();
        app.tab_strip_rows = tab_strip;
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.set_agent_reader_for_test(true);
        }
        let host = held(&mut app, session);
        for _ in 0..2 {
            refill(&mut app, wid);
            let _ = pass(&mut app, wid);
        }
        (app, wid, session, host)
    };
    for tab_strip in [0, 1] {
        let (mut app, wid, session, _host) = glass(tab_strip);
        let count = |app: &App| marks_on(app, wid, session).len();
        assert_eq!(count(&app), 1, "strip {tab_strip}: on the frame");
        for i in 0..3 {
            let _ = pass(&mut app, wid);
            assert_eq!(count(&app), 1, "strip {tab_strip}, splice {i}: once");
        }
        for i in 0..3 {
            app.capture_words(wid, std::time::Instant::now());
            assert_eq!(
                count(&app),
                0,
                "strip {tab_strip}, capture {i}: the refill took it"
            );
            app.splice_tab_strip(wid);
            assert_eq!(
                count(&app),
                0,
                "strip {tab_strip}, capture {i}: nothing moved"
            );
            let _ = pass(&mut app, wid);
            assert_eq!(count(&app), 1, "strip {tab_strip}, capture {i}: once");
        }
        app.windows.get_mut(&wid).expect("window").present = Some(crate::PresentTarget::Cpu {
            surface: crate::present::CpuSurface::Scripted(Default::default()),
        });
        for i in 0..3 {
            keystroke(&app, session, i);
            let commits = crate::scripted_redraw_fixture::surface(&mut app).commits();
            app.redraw_window(wid);
            assert!(
                crate::scripted_redraw_fixture::surface(&mut app).commits() > commits,
                "strip {tab_strip}, frame {i}: the frame ran"
            );
            assert_eq!(count(&app), 1, "strip {tab_strip}, frame {i}: once");
        }
    }
    let (mut app, wid, session, _host) = glass(1);
    let id = top_mark_id(session);
    let forget = |app: &mut App| {
        app.windows
            .get_mut(&wid)
            .expect("window")
            .claude_card
            .forget_layers(id);
        marks_on(app, wid, session)
    };
    let mut record = forget(&mut app);
    assert_eq!(record.len(), 1, "CONTROL: one to hold");
    for i in 1..=3 {
        app.capture_words(wid, std::time::Instant::now());
        app.splice_tab_strip(wid);
        app.windows
            .get_mut(&wid)
            .expect("window")
            .input_scratch
            .fx_layers
            .retain(|l| !record.contains(l));
        let _ = pass(&mut app, wid);
        let now = forget(&mut app);
        assert_eq!(now.len(), 1 + i, "CONTROL: capture {i} strands one more");
        record = now.last().cloned().into_iter().collect();
    }
}

fn measured_top(fixture: &str) -> String {
    let lines: Vec<&str> = fixture.lines().filter(|l| !l.starts_with("# ")).collect();
    let caret = lines
        .iter()
        .rposition(|l| (l.starts_with('\u{276F}') || l.starts_with('>')) && !l.contains("/effort"))
        .expect("the composer's caret row");
    lines[caret - 1].trim_end().to_owned()
}

fn writes_only_the_run(
    before: &[RenderCell],
    run: TopRun,
    top: &ComposedTop,
) -> Result<(), String> {
    let mut after = before.to_vec();
    for (k, cell) in top.cells.iter().enumerate() {
        let Some(slot) = after.get_mut(top.col + k) else {
            return Err(format!("column {} past the row", top.col + k));
        };
        *slot = *cell;
    }
    let ghost = top.ghost.map(|(a, b)| a - 1..b + 1);
    for (c, (was, now)) in before.iter().zip(&after).enumerate() {
        let inside = (run.col..run.end).contains(&c);
        let ghosts = ghost.as_ref().is_some_and(|g| g.contains(&c));
        if !inside && !ghosts && was != now {
            return Err(format!("column {c} ({:?}) changed outside the run", was.ch));
        }
    }
    Ok(())
}

#[test]
fn marks_write_only_the_run() {
    use aterm_phase::Program;
    let label = "\u{2500}\u{2500} Voice input (\u{2325} + v to start) ";
    type Line = fn(usize) -> Option<(String, String)>;
    let claude: Line = |w| {
        let r = "\u{2500}".repeat(w.checked_sub(12)?);
        let text = format!("{r} workspace \u{2500}");
        let fed = format!(
            "\x1b[38;2;136;136;136m{r} \x1b[38;2;175;135;255mworkspace\x1b[38;2;136;136;136m \u{2500}"
        );
        Some((text, fed))
    };
    let plain: Line = |w| {
        let r = "\u{2500}".repeat(w);
        Some((r.clone(), format!("\x1b[38;2;136;136;136m{r}")))
    };
    let muse: Line = |w| {
        let label = "\u{2500}\u{2500} Voice input (\u{2325} + v to start) ";
        let Some(fill) = w.checked_sub(label.chars().count()) else {
            let cut: String = label.chars().take(w - 2).chain(['\u{2026}']).collect();
            return Some((cut.clone(), format!("\x1b[38;2;136;136;136m{cut}")));
        };
        let r = "\u{2500}".repeat(fill);
        Some((
            format!("{label}{r}"),
            format!("\x1b[38;2;136;136;136m{label}{r}"),
        ))
    };
    for (fixture, line, cols) in [
        (
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-on-26col.txt"),
            claude,
            26,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-on-34col.txt"),
            claude,
            34,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-on-61col.txt"),
            claude,
            61,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-on-80col.txt"),
            claude,
            80,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-on-120col.txt"),
            claude,
            120,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-on-144col.txt"),
            claude,
            144,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/agy-1.4.0-idle.txt"),
            plain,
            120,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/muse-1.4.10-R6427.1-idle.txt"),
            muse,
            144,
        ),
    ] {
        let (text, _) = line(cols).expect("the fixture's width");
        assert_eq!(measured_top(fixture), text, "the measured line at {cols}");
    }
    let kinds: [(&str, Line, Program, KeyFace); 4] = [
        (
            "claude, KEY tag",
            claude,
            Program::Claude,
            KeyFace::default(),
        ),
        ("claude, ghost", plain, Program::Claude, GHOST),
        ("agy", plain, Program::Agy, KeyFace::default()),
        ("muse", muse, Program::Muse, KeyFace::default()),
    ];
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    let (mut checked, mut taken, mut muse_seen) = (0, 0, None);
    for cols in 20..=160usize {
        let _ = app.apply_term_resize(wid, 24, cols as u16);
        for (what, line, program, face) in kinds {
            let Some((text, fed)) = line(cols) else {
                continue;
            };
            bytes(&app, session, screen_with(cols, &fed).as_bytes());
            refill(&mut app, wid);
            let ws = &app.windows[&wid];
            let scratch = &ws.input_scratch;
            let strip = scratch.cells.len() - usize::from(ws.rows);
            let region = PaneTextRegion {
                strip,
                row_off: 0,
                col_off: 0,
                rows: 24,
                cols,
            };
            let before = scratch.cells[strip + 20].clone();
            let drawn: String = before.iter().map(|c| c.ch).collect();
            assert_eq!(
                drawn.trim_end(),
                text.trim_end(),
                "{what} at {cols}: as fed"
            );
            let blank = scratch.implicit_blank;
            let muse = program == Program::Muse;
            let Some(run) = read_top_run(scratch, region, 20, TopLine::Rule, blank, muse) else {
                continue;
            };
            let tail: &[RenderCell] = run
                .tail
                .filter(|t| t.plain)
                .map_or(&[], |t| &before[t.col..t.col + t.len]);
            let face = match what {
                "claude, KEY tag" => KeyFace {
                    tag: Some((cols - 11, cols - 2)),
                    mark: Some('\u{25b8}'),
                    ..face
                },
                _ => face,
            };
            for (switch, palette) in CASES {
                let mut top = ComposedTop::default();
                let key = TopKey {
                    frame_row: strip + 20,
                    ..key_for(program, run, switch, palette, blank, face)
                };
                compose_top(&key, &mut top, None, None, tail);
                let (laid, label) = laid_run(&key);
                writes_only_the_run(&before, laid, &top).unwrap_or_else(|e| {
                    panic!("{what} at {cols}, {switch:?} {palette:?}: {e}: {drawn:?}")
                });
                if label > 0 {
                    assert!(muse, "{what} at {cols}: only Muse's label is taken");
                    assert!(
                        laid.col == 0 && top.lockup.start <= 2,
                        "{what} at {cols}: the lockup at the rule's left"
                    );
                    assert!(
                        top.col <= 2 && top.col + top.cells.len() >= label,
                        "{what} at {cols}, {switch:?}: the label written over"
                    );
                    taken += 1;
                } else if muse {
                    writes_only_the_run(&before, run, &top).unwrap_or_else(|e| {
                        panic!("{what} at {cols}, {switch:?}: the label kept: {e}")
                    });
                }
                if let Some((a, b)) = top.ghost {
                    let ghost: String = top.cells[a - 1 - top.col..=b - top.col]
                        .iter()
                        .map(|c| c.ch)
                        .collect();
                    assert_eq!(
                        ghost,
                        ghost_chars().collect::<String>(),
                        "{what} at {cols}: the ghost whole"
                    );
                    assert!(
                        top.switch.is_some() || top.col + top.cells.len() < run.col + run.len,
                        "{what} at {cols}: with no switch, the `\u{2500}` after the ghost is Claude's"
                    );
                }
                checked += 1;
                if muse && muse_seen.is_none() {
                    let plain = TopRun { voice: None, ..run };
                    let mut kept = ComposedTop::default();
                    compose_top(&TopKey { run: plain, ..key }, &mut kept, None, None, &[]);
                    if !kept.cells.is_empty() {
                        muse_seen = Some((before.clone(), plain, kept));
                    }
                }
            }
        }
    }
    assert!(checked > 3000, "{checked}");
    assert!(taken > 0, "Muse's lockup took its label: {taken}");
    let (before, run, mut top) = muse_seen.expect("a Muse line composed");
    assert_eq!(
        run.col,
        label.chars().count(),
        "Muse's run starts after its label"
    );
    top.col = run.col - 1;
    let caught = writes_only_the_run(&before, run, &top);
    assert!(
        caught
            .as_ref()
            .is_err_and(|e| e.contains(&format!("column {}", run.col - 1))),
        "CONTROL: the label's last space is caught: {caught:?}"
    );
}

/// THE BARE SWITCH READS ON EVERY GROUND: with no harness theme the switch's and the label's every
/// glyph, at every tier, in every state, at rest and under the pointer, keeps the pane's ground
/// and reads at the name floor on it, for every built-in scheme's ground under every built-in's
/// accent. CONTROL: the label's quiet words unfloored, and an accent alone, fail on paper.
#[test]
fn the_bare_switch_reads_on_every_ground() {
    use aterm_effects::color_math::contrast_ratio;
    use aterm_types::scheme::{builtin, builtin_names};
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let accent_of = |s: &aterm_types::ColorScheme| {
        crate::chrome_band::band_colors(aterm_render::Theme::from(s)).accent
    };
    let floor = aterm_effects::cli_marks::NAME_CONTRAST;
    let schemes: Vec<_> = builtin_names()
        .into_iter()
        .filter_map(|n| Some((n, builtin(n)?)))
        .collect();
    assert!(schemes.len() >= 17, "{} built-ins", schemes.len());
    let mut checked = 0usize;
    for (ground_name, ground) in &schemes {
        let blank = RenderCell {
            ch: ' ',
            fg: rgb(ground.foreground),
            bg: rgb(ground.background),
            ..RenderCell::default()
        };
        for (accent_name, instance) in &schemes {
            let accent = accent_of(instance);
            for face in [
                HarnessSwitch::On,
                HarnessSwitch::Off(SwitchOff::Master),
                HarnessSwitch::Off(SwitchOff::Agent(aterm_phase::Program::Claude)),
                HarnessSwitch::Turning,
                HarnessSwitch::TurningOff,
            ] {
                for hover in [
                    HoverAt::None,
                    HoverAt::Switch,
                    HoverAt::Label,
                    HoverAt::Arrow,
                ] {
                    let paint = SwitchPaint {
                        hover,
                        cli_ver: Inline::new("2.1.305"),
                        up: arrow_paint(UpState::Ready).up,
                        ..SwitchPaint::bare(face, Some(aterm_phase::CliHarness::ClaudeCode))
                    };
                    let inks = FaceInks::of(&paint, None, blank, accent);
                    let mut cells: Vec<(String, RenderCell)> = switch_face(paint)
                        .cells()
                        .iter()
                        .map(|&ch| {
                            let c = switch_cell(ch, &paint, &inks, blank);
                            (format!("switch {ch:?}"), c)
                        })
                        .collect();
                    for tier in 0..LABEL_TIERS {
                        let Some(f) = label_face(&paint, tier) else {
                            continue;
                        };
                        // The label, and every form of the words its slot may show in its place.
                        let forms = slot_words(&paint, tier).map(|w| w.0).unwrap_or_default();
                        let shown = std::iter::once(f)
                            .chain(forms.iter().map(|form| LabelFace::of_words(form)));
                        for face in shown {
                            for &(ch, ink) in face.cells() {
                                let c = label_cell(ch, ink, &inks, blank);
                                cells.push((format!("label {tier} {ch:?}"), c));
                            }
                        }
                    }
                    for (what, c) in cells {
                        let what = format!(
                            "{accent_name}'s accent on {ground_name}, {face:?} {hover:?}, {what}"
                        );
                        assert_eq!(c.bg, blank.bg, "{what}: on the ground");
                        if c.ch == ' ' {
                            continue;
                        }
                        let k = reads(c.fg, blank.bg);
                        assert!(k >= floor, "{what}: at {k:.2}:1");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 17 * 17 * 5 * 3 * 40, "{checked}");
    let solarized = builtin("Solarized Light").expect("a built-in");
    let paper = rgb(solarized.background);
    let quiet = mix(rgb(solarized.foreground), paper, 0.35);
    assert!(
        reads(quiet, paper) < floor,
        "CONTROL: the quiet words unfloored at {:.2}:1 on Solarized Light",
        reads(quiet, paper)
    );
    let dracula = accent_of(&builtin("Dracula").expect("a built-in"));
    let light = rgb(builtin("Default Light").expect("a built-in").background);
    assert!(
        reads(dracula, light) < 1.5,
        "CONTROL: Dracula's accent at {:.2}:1 on Default Light",
        reads(dracula, light)
    );
}

/// THE THEMED SWITCH READS ON EVERY GROUND: under every harness theme the switch's and the
/// label's inks (the version's, the quiet words and the switch at rest, the switch under the
/// pointer, `alab` in the arc's end) read at the name floor on every built-in scheme's ground and
/// on a grey ramp, Silver Aerogel's and Unikitty's included, and so does every glyph of the switch
/// at rest and under the pointer. CONTROL: the arc's letter lifted but unfloored reads under 1.5:1
/// on Silver Aerogel.
#[test]
fn the_themed_switch_reads_on_every_ground() {
    use aterm_effects::color_math::contrast_ratio;
    use aterm_types::scheme::{builtin, builtin_names};
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let mut grounds: Vec<(String, [u8; 3], [u8; 3])> = builtin_names()
        .into_iter()
        .filter_map(|n| Some((n.to_owned(), builtin(n)?)))
        .map(|(n, s)| (n, rgb(s.background), rgb(s.foreground)))
        .collect();
    assert!(grounds.len() >= 17, "{} built-ins", grounds.len());
    for v in (0..=255u8).step_by(5) {
        let (black, white) = ([0; 3], [0xff; 3]);
        let ink = if reads(black, [v; 3]) > reads(white, [v; 3]) {
            black
        } else {
            white
        };
        grounds.push((format!("grey {v}"), [v; 3], ink));
    }
    grounds.push(("Silver Aerogel".into(), [0x92; 3], [0; 3]));
    grounds.push(("Unikitty".into(), [0xff, 0x8c, 0xd9], [0x0b; 3]));
    let floor = aterm_effects::cli_marks::NAME_CONTRAST;
    let accent = [0x3b, 0x8e, 0xea];
    let (mut checked, mut glyphs, mut lows) = (0usize, 0usize, Vec::new());
    for (name, bg, fg) in &grounds {
        let blank = RenderCell {
            ch: ' ',
            fg: *fg,
            bg: *bg,
            ..RenderCell::default()
        };
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for paint in [LIT, OFF, MASTER_OFF, TURNING, TURNING_OFF] {
                let inks = FaceInks::of(&paint, Some(palette), blank, accent);
                for hover in [HoverAt::None, HoverAt::Switch] {
                    let paint = SwitchPaint { hover, ..paint };
                    for &ch in switch_face(paint).cells().iter().filter(|&&ch| ch != ' ') {
                        let c = switch_cell(ch, &paint, &inks, blank);
                        let k = reads(c.fg, c.bg);
                        if c.bg != blank.bg || k < floor {
                            lows.push(format!(
                                "{name} {bg:?}, {palette:?} {:?} {hover:?}: {ch:?} {:?} on {:?} at \
                                 {k:.2}:1",
                                paint.face, c.fg, c.bg
                            ));
                        }
                        glyphs += 1;
                    }
                }
                for (what, ink) in [
                    ("the version", inks.lit),
                    ("the quiet words", inks.muted),
                    ("the switch under the pointer", inks.lifted),
                    ("alab", inks.brand),
                ] {
                    let k = reads(ink, blank.bg);
                    if k < floor {
                        lows.push(format!(
                            "{name} {bg:?}, {palette:?} {:?}: {what} {ink:?} at {k:.2}:1",
                            paint.face
                        ));
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 70 * 3 * 5 * 4, "{checked}");
    assert!(glyphs >= 70 * 3 * 5 * 2 * 3, "{glyphs}");
    assert!(
        lows.is_empty(),
        "{} inks under {floor}:1: {lows:#?}",
        lows.len()
    );
    let aerogel = [0x92; 3];
    let lifted = (0..=255u8)
        .map(|k| {
            let letter = CardPalette::Land.letter(f32::from(k) / 255.0, true);
            let letter = [(letter >> 16) as u8, (letter >> 8) as u8, letter as u8];
            reads(mix(letter, [0; 3], 0.25), aerogel)
        })
        .fold(f32::MAX, f32::min);
    assert!(
        lifted < 1.5,
        "CONTROL: the lifted letter alone on Silver Aerogel, {lifted:.2}:1"
    );
}

/// `n` held Claude Code panes in one window (1, 2, or 9 in a three by three
/// grid), each drawn at rest and settled, with each one's session and the
/// rows of its screen's two composer rules, the top's and the bottom's.
fn held_panes(n: usize) -> (App, WindowId, Hosted, Vec<(u64, usize, usize)>) {
    use crate::pane::SplitDir;
    const PGID: i32 = 4242;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.apply_term_resize(wid, 99, 330));
    let a = app.focused_session_id(wid).expect("the front session");
    let mut sessions = vec![a];
    if n >= 2 {
        sessions.push(app.split_active_stub_tab_dir(wid, SplitDir::Vertical));
    }
    if n == 9 {
        let b = sessions[1];
        assert!(app.focus_session_window(b));
        let c = app.split_active_stub_tab_dir(wid, SplitDir::Vertical);
        sessions.push(c);
        for column in [a, b, c] {
            assert!(app.focus_session_window(column));
            let mid = app.split_active_stub_tab_dir(wid, SplitDir::Horizontal);
            assert!(app.focus_session_window(mid));
            let low = app.split_active_stub_tab_dir(wid, SplitDir::Horizontal);
            sessions.extend([mid, low]);
        }
    }
    assert_eq!(sessions.len(), n);
    let host = hosted(&mut app);
    let mut tops = Vec::with_capacity(n);
    for &session in &sessions {
        let (_, _, rows, cols) = app.visible_session_rect(wid, session).expect("shown");
        let (rows, cols) = (usize::from(rows), usize::from(cols));
        let entry = app.pool.get(session).expect("session");
        {
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(PGID);
            timeline.set_program(PGID, Some("claude".into()));
            timeline.set_agent_reader_for_test(true);
            assert!(timeline.set_claude_footer(PGID, None, Some(facts())));
        }
        crate::session_timeline::claim_supervisor(
            &entry.ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
        let rule = "\u{2500}".repeat(cols);
        let mut screen: Vec<String> = (0..rows - 6)
            .map(|i| format!("\u{23FA} Message {i}: what Claude said."))
            .collect();
        screen.extend([
            DONE.to_owned(),
            String::new(),
            rule.clone(),
            "\u{276F} ".to_owned(),
            rule,
            BYPASS.to_owned(),
        ]);
        feed(&app, session, &screen);
        tops.push((session, rows - 4, rows - 2));
    }
    for _ in 0..3 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    (app, wid, host, tops)
}

/// The top lines read and the composer rules masked by one footer pass.
fn rule_reads(app: &mut App, wid: WindowId) -> (usize, usize) {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_footer_top_reads(),
        crate::work_counts::take_footer_rule_masks(),
    );
    let _ = pass_on(app, wid, &plan, route);
    (
        crate::work_counts::take_footer_top_reads(),
        crate::work_counts::take_footer_rule_masks(),
    )
}

/// A SPLIT WINDOW'S UNCHANGED RULES ARE KEPT, AS ONE PANE'S ARE (review
/// 13): a split window's composed frame carries no row revisions, and a
/// refill of its unchanged screens read every pane's top line again, nine
/// reads a refill for nine panes where one pane's reads none. The cells a
/// rule was read from vouch for its row now: a refill of two or nine held
/// panes at rest reads no top line and masks no rule. CONTROL: one pane's
/// refill reads none. NEGATIVE CONTROL: one pane's top rule repainted in
/// another colour is read again, and its bottom rule's mask, each alone.
#[test]
fn a_split_windows_unchanged_rules_are_kept_as_one_panes_are() {
    for n in [1, 2, 9] {
        let (mut app, wid, _host, rules) = held_panes(n);
        for i in 0..4 {
            refill(&mut app, wid);
            assert_eq!(rule_reads(&mut app, wid), (0, 0), "{n} panes, refill {i}");
        }
        if n != 9 {
            continue;
        }
        let (session, top, bottom) = rules[4];
        let (_, _, _, cols) = app.visible_session_rect(wid, session).expect("shown");
        let repaint = |app: &App, row: usize| {
            bytes(
                app,
                session,
                format!(
                    "\x1b7\x1b[{};1H\x1b[38;2;200;90;90m{}\x1b[0m\x1b8",
                    row + 1,
                    "\u{2500}".repeat(usize::from(cols))
                )
                .as_bytes(),
            );
        };
        for (row, read, what) in [(top, (1, 0), "top"), (bottom, (0, 1), "bottom")] {
            repaint(&app, row);
            refill(&mut app, wid);
            assert_eq!(
                rule_reads(&mut app, wid),
                read,
                "NEGATIVE CONTROL: the {what} rule repainted is read again, alone"
            );
            refill(&mut app, wid);
            assert_eq!(rule_reads(&mut app, wid), (0, 0), "{what}: kept again");
        }
    }
}

/// An upgrade row of Claude Code moving `from` → `to` in `phase`, waiting on `wait`.
fn up_row(
    from: &str,
    to: &str,
    phase: aterm_agent::harness::upgrade::Phase,
    wait: &str,
) -> aterm_agent::harness::upgrade_drive::Row {
    aterm_agent::harness::upgrade_drive::Row {
        tab: "s-up".to_string(),
        from: from.to_string(),
        to: to.to_string(),
        phase,
        wait: wait.to_string(),
        ..aterm_agent::harness::upgrade_drive::Row::default()
    }
}

/// THE ARROW OFFERS A LATER BUILD UNTIL ITS MOVE LANDS: a numerically later build pending,
/// announced or given up unanswered is Ready, the owner's Now or a move under way is Asked, and
/// the arrow says when it would land. CONTROLS: the same build, an older one, a skipped one, a
/// done move and a failed one show none; a skip of another build leaves it Ready.
#[test]
fn the_arrow_offers_a_later_build_until_its_move_lands() {
    use aterm_agent::harness::upgrade::{GAVE_UP, Phase, Request};
    let up = |row| TopUp::of(&row);
    {
        // A REVISION UNDER ONE CORE IS OFFERED, NAMED WHOLE: the lane moves a Muse tab onto a
        // later revision of the build it runs (codex-alab's lane mints no such move).
        for (from, to, target) in [
            ("1.4.10-R6400.1", "1.4.10-R6500.1", "v1.4.10 (R6500.1)"),
            ("1.4.10-R6427.9", "1.4.10-R6427.10", "v1.4.10 (R6427.10)"),
        ] {
            let revised = up(up_row(from, to, Phase::Pending, ""));
            assert_eq!(revised.state, UpState::Ready, "{to}: offered");
            assert_eq!(
                up_said(&revised, "", None).as_deref(),
                Some(format!("update to {target} \u{b7} now").as_str()),
                "{to}: the words name the build past the numbers"
            );
        }
        let later = up(up_row(
            "1.4.10-R6500.1",
            "1.4.11-R6510.1",
            Phase::Pending,
            "",
        ));
        assert_eq!(
            up_said(&later, "", None).as_deref(),
            Some("update to v1.4.11 \u{b7} now"),
            "a later core is named by its numbers"
        );
    }
    {
        // A MOVE ITS TAB SETS ASIDE IS HELD: nothing steps the tab, so the arrow says when it
        // would land and why a press cannot ask for it, the person's Now included; but a Claude
        // Code tab's press passes its Autopilot or its harness switched off, once, so there the
        // arrow is Ready, the person's Now (which does not pass them) included.
        use aterm_agent::harness::upgrade::Agent;
        use aterm_agent::harness::upgrade_drive::{
            AGENT_OFF, AUTOPILOT_OFF, CLAIMED, NOT_FOREGROUND,
        };
        let pending = up_row("2.1.305", "2.1.310", Phase::Pending, "background");
        for agent in [Agent::Claude, Agent::Codex] {
            for (aside, wait, why, pressed_past) in [
                (
                    AUTOPILOT_OFF,
                    "once autopilot is on",
                    "autopilot is off",
                    true,
                ),
                (
                    AGENT_OFF,
                    "once the harness is on",
                    "the harness is off",
                    true,
                ),
                (
                    CLAIMED,
                    "once its supervisor lets go",
                    "another supervisor holds it",
                    false,
                ),
            ] {
                for request in [Request::None, Request::Now] {
                    let row = aterm_agent::harness::upgrade_drive::Row {
                        aside: aside.to_owned(),
                        wait: aside.to_owned(),
                        request: request.clone(),
                        agent,
                        ..pending.clone()
                    };
                    let shown = up(row);
                    if pressed_past && agent == Agent::Claude {
                        assert_eq!(
                            (shown.state, shown.why, shown.press),
                            (UpState::Ready, "", Some("")),
                            "{agent:?} {aside} {request:?}: a press passes it"
                        );
                    } else {
                        assert_eq!(
                            (shown.state, shown.wait, shown.why),
                            (UpState::Held, wait, why),
                            "{agent:?} {aside} {request:?}"
                        );
                    }
                }
            }
        }
        // A ROW WHOSE AGENT IS NOT THE FOREGROUND JOB IS ANOTHER PROGRAM'S: the rule is drawn by
        // the one in front (a Claude Code suspended under another), so it shows no arrow, set
        // aside or held as not in front, kept or live.
        for (aside, wait) in [(NOT_FOREGROUND, NOT_FOREGROUND), ("", NOT_FOREGROUND)] {
            let behind = aterm_agent::harness::upgrade_drive::Row {
                aside: aside.to_owned(),
                wait: wait.to_owned(),
                ..pending.clone()
            };
            assert_eq!(up(behind.clone()), TopUp::default(), "{aside:?} {wait:?}");
            assert_eq!(TopUp::harness_off(&behind), TopUp::default(), "kept");
        }
        let codex = aterm_agent::harness::upgrade_drive::Row {
            agent: Agent::Codex,
            ..pending.clone()
        };
        let gone = TopUp::harness_off(&codex);
        assert_eq!(
            (gone.state, gone.wait, gone.why),
            (
                UpState::Held,
                "once the harness is on",
                "the harness is off"
            ),
            "the row kept from before the host stood down"
        );
        assert_eq!(
            TopUp::harness_off(&pending),
            up(pending.clone()),
            "a Claude Code row kept as it stands: a press moves it with the harness off"
        );
        assert_eq!(
            TopUp::harness_off(&up_row("2.1.305", "2.1.310", Phase::Done, "")),
            TopUp::default(),
            "CONTROL: a kept row with no move waiting shows none"
        );
    }
    let pending = up_row("2.1.305", "2.1.310", Phase::Pending, "background");
    let ready = up(pending.clone());
    assert_eq!(
        (ready.state, ready.to.as_str(), ready.wait),
        (UpState::Ready, "2.1.310", "after background jobs")
    );
    for phase in [
        Phase::Announced { at_s: 1, asks: 1 },
        Phase::Failed(GAVE_UP.to_string()),
    ] {
        let row = aterm_agent::harness::upgrade_drive::Row {
            phase: phase.clone(),
            ..pending.clone()
        };
        assert_eq!(up(row).state, UpState::Ready, "{phase:?}");
    }
    let asked = aterm_agent::harness::upgrade_drive::Row {
        request: Request::Now,
        ..pending.clone()
    };
    assert_eq!(up(asked).state, UpState::Asked, "the owner's Now");
    for phase in [Phase::Exiting { at_s: 1 }, Phase::Relaunched { at_s: 1 }] {
        let row = aterm_agent::harness::upgrade_drive::Row {
            phase: phase.clone(),
            ..pending.clone()
        };
        assert_eq!(up(row).state, UpState::Asked, "{phase:?}: under way");
    }
    let other_skip = aterm_agent::harness::upgrade_drive::Row {
        request: Request::Skip("2.1.309".to_string()),
        ..pending.clone()
    };
    assert_eq!(
        up(other_skip).state,
        UpState::Ready,
        "a skip of another build"
    );
    let other_revision = aterm_agent::harness::upgrade_drive::Row {
        request: Request::Skip("1.4.10-R6499.1".to_string()),
        agent: aterm_agent::harness::upgrade::Agent::Muse,
        ..up_row("1.4.10-R6427.1", "1.4.10-R6500.1", Phase::Pending, "")
    };
    assert_eq!(
        up(other_revision).state,
        UpState::Ready,
        "a skip of another revision"
    );
    for (what, row) in [
        (
            "the same build",
            up_row("2.1.305", "2.1.305", Phase::Pending, ""),
        ),
        (
            "an older build",
            up_row("2.1.310", "2.1.305", Phase::Pending, ""),
        ),
        ("a done move", up_row("2.1.305", "2.1.310", Phase::Done, "")),
        (
            "a failed move",
            up_row(
                "2.1.305",
                "2.1.310",
                Phase::Failed("stale-exit".to_string()),
                "",
            ),
        ),
        (
            "a skipped build",
            aterm_agent::harness::upgrade_drive::Row {
                request: Request::Skip("2.1.310".to_string()),
                ..pending.clone()
            },
        ),
        (
            "a skipped revision",
            aterm_agent::harness::upgrade_drive::Row {
                request: Request::Skip("1.4.10-R6500.1".to_string()),
                agent: aterm_agent::harness::upgrade::Agent::Muse,
                ..up_row("1.4.10-R6427.1", "1.4.10-R6500.1", Phase::Pending, "")
            },
        ),
    ] {
        assert_eq!(up(row), TopUp::default(), "CONTROL: {what}");
    }
    for (wait, words) in [
        ("", "now"),
        ("next-point", "now"),
        ("background", "after background jobs"),
        ("not-idle:shell", "after background jobs"),
        ("busy", "at turn end"),
        ("busy-thread", "at turn end"),
        ("not-idle:busy", "at turn end"),
        ("release:busy", "at turn end"),
        ("not-idle:waiting", "once its dialog is answered"),
        ("not-idle", "once it is idle"),
        ("status-stale:busy", "once it is idle"),
        ("notice-shown", "once it is idle"),
        ("settling", "once its tab settles"),
        ("awaiting-ready", "once it agrees to move"),
        ("not-ready", "once it agrees to move"),
        ("login", "after login"),
        ("limited:5h", "after the usage limit"),
        ("attended", "once you stop typing"),
        ("draft", "once your draft is sent or cleared"),
        ("screen-unreadable", ""),
    ] {
        assert_eq!(up_wait(wait), words, "{wait:?}");
    }
}

/// A lit Claude Code paint at 2.1.305 whose arrow is `state` toward 2.1.310.
fn arrow_paint(state: UpState) -> SwitchPaint {
    SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        up: TopUp {
            state,
            to: Inline::new("2.1.310"),
            revision: false,
            wait: "after background jobs",
            why: if state == UpState::Held {
                "autopilot is off"
            } else {
                ""
            },
            press: None,
            agent: aterm_agent::harness::upgrade::Agent::Claude,
        },
        ..LIT
    }
}

/// THE ARROW ENDS THE RULE A BLANK FROM ITS CLOSE: on a wide rule the ⬆️ takes the two cells
/// before the close's blank, emoji-presented with its continuation, a blank before it, the label
/// right-aligned to that blank; its hit takes it and both blanks. An asked move's one-cell `◐`
/// takes the first of those cells with the second blank, and the close runs on from there as
/// rule (`◐ ───`): one blank either side of it, as of the ⬆️. The ladder keeps it on every rung,
/// three cells more: an 80-column pane lays T0 with it too, a 77-column one T1 with it. CONTROL:
/// with no arrow the label ends at the close's blank.
#[test]
fn the_arrow_ends_the_rule_a_blank_from_its_close() {
    let tail = claude_tail();
    let lockup = Some(CliMark::Claude.lockup_cells(Build::Vendor));
    let run = tailed_run(158 - 12, &tail);
    let field = run.col + run.span(KeyFace::default()).whole;
    for state in [UpState::Ready, UpState::Asked] {
        let paint = arrow_paint(state);
        let top = composed_with(
            CLAUDE,
            run,
            Some(paint),
            Some(CardPalette::Cruise),
            KeyFace::default(),
            &tail,
        );
        let at = |c: usize| top.cells[c - top.col];
        let s = top.switch.as_ref().expect("laid");
        assert_eq!(
            s.arrow,
            Some(field - 6..field - 2),
            "{state:?}: the arrow's hit"
        );
        assert_eq!(at(field - 6).ch, ' ', "{state:?}: the blank before it");
        assert_eq!(
            [at(field - 2).ch, at(field - 1).ch],
            ['\u{2500}'; 2],
            "{state:?}: the close"
        );
        match state {
            UpState::Ready => {
                assert_eq!(at(field - 5).ch, UP_ARROW);
                assert!(at(field - 5).emoji_presentation, "emoji-presented");
                assert!(at(field - 4).wide, "its continuation");
                assert_eq!(at(field - 3).ch, ' ', "the close's blank");
            }
            _ => {
                assert_eq!(
                    (at(field - 5).ch, at(field - 5).fg, at(field - 4).ch),
                    ('\u{25d0}', [0xd7, 0x77, 0x57], ' '),
                    "asked: the accent's half-moon, a blank"
                );
                assert_eq!(
                    (at(field - 3).ch, at(field - 3).fg),
                    ('\u{2500}', at(field - 2).fg),
                    "asked: the close runs on to the half-moon's blank, in its own ink"
                );
            }
        }
        let (label, _) = s.label.clone().expect("a label at 158");
        assert_eq!(
            label.end,
            field - 6,
            "{state:?}: the label ends at the blank"
        );
    }
    let plain = composed_with(
        CLAUDE,
        run,
        Some(SwitchPaint {
            cli_ver: Inline::new("2.1.305"),
            ..LIT
        }),
        Some(CardPalette::Cruise),
        KeyFace::default(),
        &tail,
    );
    let s = plain.switch.as_ref().expect("laid");
    assert_eq!(s.arrow, None, "CONTROL: no arrow");
    assert_eq!(
        s.label.clone().expect("a label").0.end,
        field - 3,
        "CONTROL"
    );
    let rung_at = |paint: SwitchPaint, w: usize| {
        let span = tailed_run(w - 12, &tail).span(KeyFace::default());
        rung_name(lay_top(span, Some(paint), lockup).map(|l| l.rung))
    };
    let ready = arrow_paint(UpState::Ready);
    // The arrow's 3 cells more on every rung, and the lockup's name gone before the CLI's build.
    for (name, need) in [
        ("T0", 78),
        ("T1", 71),
        ("T2", 63),
        ("T2 icon", 56),
        ("T3 icon", 45),
        ("T4 icon", 35),
        ("T4 bare", 29),
    ] {
        assert_eq!(
            rung_at(ready, need),
            name,
            "{name} at {need} with the arrow"
        );
        assert_ne!(rung_at(ready, need - 1), name, "{name}: a column short");
    }
    let lit = SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..LIT
    };
    assert_eq!(
        (rung_at(lit, 80), rung_at(ready, 80), rung_at(ready, 77)),
        ("T0".to_owned(), "T0".to_owned(), "T1".to_owned()),
        "80 columns: T0 with the arrow too; 77, T1 with it"
    );
    let off = SwitchPaint {
        up: ready.up,
        ..SwitchPaint {
            cli_ver: Inline::new("2.1.305"),
            ..MASTER_OFF
        }
    };
    let span = tailed_run(26 - 12, &tail).span(KeyFace::default());
    let laid = lay_top(span, Some(off), lockup).expect("the last resort");
    assert_eq!(
        (rung_name(Some(laid.rung)), laid.rung.need),
        ("last resort".to_owned(), 14),
        "the off switch and the arrow in the run: 14"
    );
}

/// AN ASKED MOVE KEEPS THE LABEL'S WIDTH: a press turns the ⬆️ to `◐` and nothing else moves,
/// the rung and the label as they were at every width; under the pointer the asked arrow says
/// the move is on its way.
#[test]
fn an_asked_move_keeps_the_labels_width() {
    let tail = claude_tail();
    let lockup = Some(CliMark::Claude.lockup_cells(Build::Vendor));
    let laid_at = |paint: SwitchPaint, w: usize| {
        let span = tailed_run(w - 12, &tail).span(KeyFace::default());
        lay_top(span, Some(paint), lockup).expect("a rung")
    };
    for w in [158, 78, 60, 46] {
        let ready = laid_at(arrow_paint(UpState::Ready), w);
        let asked = laid_at(arrow_paint(UpState::Asked), w);
        assert_eq!(asked.rung, ready.rung, "{w}: the same rung");
        assert_eq!(
            asked.label.as_ref().map(LabelFace::text),
            ready.label.as_ref().map(LabelFace::text),
            "{w}: the same label"
        );
    }
    assert_eq!(
        up_said(&arrow_paint(UpState::Asked).up, "1 shell", None).as_deref(),
        Some("updating to v2.1.310 \u{b7} after 1 shell")
    );
}

/// A top rule `w` columns wide composed for `paint`, Claude's tag in its zone: the key and what
/// it laid.
fn slot_top(paint: SwitchPaint, w: usize) -> (TopKey, ComposedTop) {
    let tail = claude_tail();
    let key = key_for(
        aterm_phase::Program::Claude,
        tailed_run(w - 12, &tail),
        Some(paint),
        Some(CardPalette::Cruise),
        blank(),
        KeyFace {
            tag: Some(tag_in(w - 12)),
            ..KeyFace::default()
        },
    );
    let mut top = ComposedTop::default();
    compose_top(&key, &mut top, None, None, &tail);
    (key, top)
}

/// The words in a composed top rule's label slot, its leading blanks dropped; `None` with no
/// label.
fn slot_of(top: &ComposedTop) -> Option<String> {
    let (cols, _) = top.switch.as_ref()?.label.clone()?;
    Some(
        cols.map(|c| top.cells[c - top.col].ch)
            .collect::<String>()
            .trim_start()
            .to_owned(),
    )
}

/// Where a composed top rule laid the switch and Claude's tag, glass columns.
fn laid_at(top: &ComposedTop) -> (std::ops::Range<usize>, Option<(usize, usize)>) {
    (top.switch.as_ref().expect("laid").cols.clone(), top.tag)
}

/// THE ARROW'S WORDS TAKE THE LABEL'S OWN CELLS: under the pointer the slot says the verb, the
/// target build bold in its own ink and when the move would land, in Claude's own job words
/// where its mode row gives them, in the first form that fits the label's own cells, so neither
/// the switch nor Claude's tag moves; a form that drops the verb keeps the target and when
/// (`v2.1.310 · after 1 shell`), then the verb and the target, then the target alone; a
/// refused press's words win for a few seconds; a sprite rings the arrow, and none rings an
/// asked move's `◐`, which takes no press. CONTROL: at rest the label shows, and at a tier with
/// no label the hover writes nothing new.
#[test]
fn the_arrows_words_take_the_labels_own_cells() {
    let rest = arrow_paint(UpState::Ready);
    let (rest_key, at_rest) = slot_top(rest, 158);
    let label = label_face(&rest, 0).expect("T0").text();
    assert_eq!(
        slot_of(&at_rest).as_deref(),
        Some(label.as_str()),
        "CONTROL: at rest, the label"
    );
    let hovered = SwitchPaint {
        hover: HoverAt::Arrow,
        ..rest
    };
    let jobs = hovered.with_jobs("1 shell");
    let asked = SwitchPaint {
        up: arrow_paint(UpState::Asked).up,
        ..jobs
    };
    let idle = SwitchPaint {
        up: TopUp {
            wait: "now",
            ..hovered.up
        },
        ..hovered
    };
    assert!(
        label.chars().count() < "updating to v2.1.310 \u{b7} after 1 shell".chars().count(),
        "CONTROL: the asked verb's form is wider than the label: {label:?}"
    );
    // Each form is shorter than the one before, so every one can be the first that fits: a verb's
    // form longer than the target and a short when (`v2.1.310 · now`, 14 cells, then `update to
    // v2.1.310`, 18) never would be. CONTROL: a long when keeps the verb's form.
    let widths = |paint: &SwitchPaint| -> Vec<usize> {
        up_forms(&paint.up, paint.jobs.as_str(), paint.press)
            .expect("forms")
            .0
            .iter()
            .map(|form| form.iter().map(|(t, _)| t.chars().count()).sum())
            .collect()
    };
    for (state, wait) in [
        (UpState::Ready, "now"),
        (UpState::Ready, "after background jobs"),
        (UpState::Asked, "now"),
        (UpState::Asked, "at turn end"),
        (UpState::Held, ""),
    ] {
        let paint = SwitchPaint {
            up: TopUp {
                wait,
                ..arrow_paint(state).up
            },
            ..hovered
        };
        let w = widths(&paint);
        assert!(
            w.windows(2).all(|p| p[1] < p[0]),
            "{state:?}, {wait:?}: each form shorter than the one before: {w:?}"
        );
    }
    assert_eq!(
        widths(&hovered).len(),
        4,
        "CONTROL: a long when keeps the verb's form"
    );
    for (w, paint, want) in [
        (158, jobs, "update to v2.1.310 \u{b7} after 1 shell"),
        (158, hovered, "v2.1.310 \u{b7} after background jobs"),
        (100, jobs, "update to v2.1.310 \u{b7} after 1 shell"),
        (76, jobs, "v2.1.310 \u{b7} after 1 shell"),
        (76, hovered, "update to v2.1.310"),
        (50, jobs, "v2.1.310"),
        // An asked move's words say it is under way wherever they keep when it lands: its verb's
        // form (36 cells) is one past the owner's 35-cell label, so `→` takes the verb's place,
        // never the offer's bare words; a label of 9 cells (`alab v0.5`) holds the target alone.
        (158, asked, "\u{2192} v2.1.310 \u{b7} after 1 shell"),
        (76, asked, "\u{2192} v2.1.310 \u{b7} after 1 shell"),
        (50, asked, "v2.1.310"),
        // An idle tab's move lands now: the short when says more than the verb would.
        (158, idle, "update to v2.1.310 \u{b7} now"),
        (76, idle, "update to v2.1.310 \u{b7} now"),
        (50, idle, "v2.1.310"),
    ] {
        let (_, still) = slot_top(
            SwitchPaint {
                hover: HoverAt::None,
                ..paint
            },
            w,
        );
        let (_, over) = slot_top(paint, w);
        assert_eq!(slot_of(&over).as_deref(), Some(want), "{w}: {want}");
        assert_eq!(
            laid_at(&over),
            laid_at(&still),
            "{w}: the switch and the tag stand where they stood"
        );
    }
    let (key, over) = slot_top(jobs, 158);
    let inks = FaceInks::of(&jobs, Some(CardPalette::Cruise), blank(), key.mark_fg);
    let (cols, _) = over
        .switch
        .as_ref()
        .and_then(|s| s.label.clone())
        .expect("a label");
    let target: Vec<RenderCell> = cols
        .map(|c| over.cells[c - over.col])
        .skip_while(|c| c.ch == ' ')
        .skip("update to ".len())
        .take("v2.1.310".len())
        .collect();
    assert!(
        target.iter().all(|c| c.bold && c.fg == inks.target) && inks.target != inks.muted,
        "the target build bold in its own ink: {target:?}"
    );
    let ring = ring_layer(&key, &over, 7, &mut RingContents::default()).expect("a ring");
    let s = over.switch.as_ref().expect("laid");
    let arrow = s.arrow.clone().expect("an arrow");
    let cw = i32::from(key.cell.0);
    assert_eq!(
        ring.offset.0,
        i32::try_from(arrow.start + 1).expect("a column") * cw - cw / 2,
        "the sprite rings the arrow, from half a cell left of its first cell"
    );
    assert!(ring_layer(&rest_key, &at_rest, 7, &mut RingContents::default()).is_none());
    let (asked_key, asked_over) = slot_top(asked, 158);
    assert!(
        asked_key.switch.is_some_and(|p| p.hover == HoverAt::Arrow),
        "CONTROL: the pointer is on the asked arrow"
    );
    assert!(
        ring_layer(&asked_key, &asked_over, 7, &mut RingContents::default()).is_none(),
        "an asked move's \u{25d0} takes no press: no ring"
    );
    let held = SwitchPaint {
        up: arrow_paint(UpState::Held).up,
        ..hovered
    };
    let (held_key, held_over) = slot_top(held, 158);
    assert!(
        held_key.switch.is_some_and(|p| p.hover == HoverAt::Arrow),
        "CONTROL: the pointer is on the held arrow"
    );
    assert!(
        ring_layer(&held_key, &held_over, 7, &mut RingContents::default()).is_none(),
        "a held move's \u{2191} asks nothing: no ring"
    );
    let (_, said) = slot_top(rest.refused_with(Some("press it again")), 158);
    assert_eq!(
        slot_of(&said).as_deref(),
        Some("couldn't update \u{b7} press it again"),
        "a refusal's words"
    );
    let (_, t4) = slot_top(rest, 40);
    let (_, t4_over) = slot_top(hovered, 40);
    assert!(
        t4.switch.as_ref().is_some_and(|s| s.label.is_none()),
        "T4: no label"
    );
    assert_eq!(
        t4_over.cells, t4.cells,
        "CONTROL: no label, the hover writes nothing new"
    );
}

/// A HELD ARROW'S WORDS AND A REFUSAL KEEP THEIR REASON: at every width from 158 columns down to
/// the narrowest that lays a label, the slot under the pointer on a held ⬆️ and after a refused
/// press says why the move cannot be asked for (in its fewest words where the label is narrow:
/// `v2.1.310 · autopilot off`, `the harness is off`), whole, never cut; it moves nothing where
/// a form fits the label's own cells, and grows only as far as a whole form needs (`alab v0.5`
/// gives way to `autopilot off`). CONTROL: a rule too narrow for any form keeps its label.
#[test]
fn a_held_arrows_words_and_a_refusal_keep_their_reason() {
    use aterm_agent::harness::upgrade::{Agent, Phase, Request};
    use aterm_agent::harness::upgrade_drive::{AGENT_OFF, AUTOPILOT_OFF};
    // A CLI whose press is the tab menu's Upgrade Now: a Claude Code press passes these asides.
    let held = |aside: &str| {
        let row = aterm_agent::harness::upgrade_drive::Row {
            aside: aside.to_owned(),
            wait: aside.to_owned(),
            request: Request::None,
            agent: Agent::Codex,
            ..up_row("2.1.305", "2.1.310", Phase::Pending, "background")
        };
        SwitchPaint {
            up: TopUp::of(&row),
            ..arrow_paint(UpState::Ready)
        }
    };
    let autopilot = held(AUTOPILOT_OFF);
    let harness = held(AGENT_OFF);
    assert_eq!(
        (autopilot.up.state, harness.up.state),
        (UpState::Held, UpState::Held)
    );
    let cases: [(&str, SwitchPaint, &[&str]); 5] = [
        (
            "the held hover",
            SwitchPaint {
                hover: HoverAt::Arrow,
                ..autopilot
            },
            &["once autopilot is on", "autopilot off"],
        ),
        (
            "the harness-off hover",
            SwitchPaint {
                hover: HoverAt::Arrow,
                ..harness
            },
            &["once the harness is on", "harness off"],
        ),
        (
            "a held press",
            autopilot.refused_with(Some("autopilot is off")),
            &["autopilot is off", "autopilot off"],
        ),
        (
            "a press with the harness off",
            harness.refused_with(Some("the harness is off")),
            &["the harness is off", "harness off"],
        ),
        (
            "a lane's refusal",
            arrow_paint(UpState::Ready).refused_with(Some("its restart is already under way")),
            &["its restart is already under way", "already under way"],
        ),
    ];
    for (what, paint, reasons) in cases {
        let mut laid = 0;
        for w in (40..=158).rev() {
            let at_rest = SwitchPaint {
                hover: HoverAt::None,
                refused: Inline::EMPTY,
                ..paint
            };
            let (_, still) = slot_top(at_rest, w);
            let Some(label) = slot_of(&still) else {
                continue;
            };
            let (_, shown) = slot_top(paint, w);
            let words = slot_of(&shown).expect("the slot");
            if words == label {
                // No form fits: the label stands, never cut.
                assert!(
                    reasons.iter().all(|r| label.len() < r.len()),
                    "{what} at {w}: the label stood though {reasons:?} fit {label:?}"
                );
                continue;
            }
            laid += 1;
            assert!(
                reasons.iter().any(|r| words.ends_with(r)) && !words.contains('\u{2026}'),
                "{what} at {w}: {words:?} says why, whole"
            );
            let moved = laid_at(&shown) != laid_at(&still);
            assert_eq!(
                moved,
                words.chars().count() > label.chars().count(),
                "{what} at {w}: {words:?} in place of {label:?} moves the switch only to fit"
            );
        }
        assert!(laid > 90, "{what}: said at {laid} widths");
    }
    let at = |paint: SwitchPaint, w| slot_of(&slot_top(paint, w).1);
    let hover = SwitchPaint {
        hover: HoverAt::Arrow,
        ..autopilot
    };
    assert_eq!(
        at(hover, 76).as_deref(),
        Some("v2.1.310 \u{b7} autopilot off"),
        "76: the target and why, in the label's own cells"
    );
    assert_eq!(
        at(autopilot.refused_with(Some("autopilot is off")), 76).as_deref(),
        Some("autopilot is off"),
        "76: a held press says why"
    );
    assert_eq!(
        at(harness.refused_with(Some("the harness is off")), 76).as_deref(),
        Some("the harness is off"),
        "76: with the harness off"
    );
    let (_, rest50) = slot_top(autopilot, 50);
    let (_, press50) = slot_top(autopilot.refused_with(Some("autopilot is off")), 50);
    let short = format!(
        "alab v{}",
        aterm_phase::CliHarness::ClaudeCode.version_str()
    );
    assert_eq!(slot_of(&rest50).as_deref(), Some(short.as_str()), "50: T3");
    assert_eq!(
        slot_of(&press50).as_deref(),
        Some("autopilot off"),
        "50: grown to the fewest words"
    );
    assert_eq!(
        laid_at(&rest50).0.start - laid_at(&press50).0.start,
        "autopilot off".len() - short.len(),
        "50: the switch moved only as far as the words need"
    );
}

/// THE LABEL'S HOVER SAYS ONLY WHAT IT LEAVES OUT: under the pointer a Muse label shows its
/// whole build in the label's own inks (`alab` in the brand's), in the first form that fits the
/// label's own cells (`alab v0.5 (v1.4.13-R6996.1)` where `alab muse harness v0.5 (v1.4.13)`
/// stood), so nothing moves; where no form fits, and for Claude Code (whose build is its
/// numbers), the label stands.
#[test]
fn the_labels_hover_says_only_what_it_leaves_out() {
    let tail = claude_tail();
    let compose = |paint: SwitchPaint, w: usize| {
        let key = key_for(
            aterm_phase::Program::Claude,
            tailed_run(w - 12, &tail),
            Some(paint),
            Some(CardPalette::Cruise),
            blank(),
            KeyFace::default(),
        );
        let mut top = ComposedTop::default();
        compose_top(&key, &mut top, None, None, &tail);
        top
    };
    let slot = |top: &ComposedTop| -> (String, Vec<RenderCell>) {
        let (cols, _) = top
            .switch
            .as_ref()
            .and_then(|s| s.label.clone())
            .expect("a label");
        let cells: Vec<RenderCell> = cols.map(|c| top.cells[c - top.col]).collect();
        (cells.iter().map(|c| c.ch).collect(), cells)
    };
    let muse = SwitchPaint {
        cli_ver: Inline::new("1.4.13-R6996.1"),
        ..SwitchPaint::bare(HarnessSwitch::On, Some(aterm_phase::CliHarness::Muse))
    };
    let over = SwitchPaint {
        hover: HoverAt::Label,
        ..muse
    };
    let (rest, _) = slot(&compose(muse, 158));
    let (shown, cells) = slot(&compose(over, 158));
    let whole = format!(
        "alab muse harness v{} (v1.4.13-R6996.1)",
        aterm_phase::CliHarness::Muse.version_str()
    );
    assert_eq!(
        rest.trim_start(),
        whole.replace("-R6996.1", ""),
        "at rest, the numbers"
    );
    assert_eq!(
        shown.trim_start(),
        format!(
            "alab v{} (v1.4.13-R6996.1)",
            aterm_phase::CliHarness::Muse.version_str()
        ),
        "the hover, the whole build in the label's own cells"
    );
    assert_eq!(
        compose(over, 158).switch.map(|s| s.cols),
        compose(muse, 158).switch.map(|s| s.cols),
        "the switch stands where it stood"
    );
    assert!(
        whole.len() > rest.len(),
        "CONTROL: the widest form would move it"
    );
    let inks = FaceInks::of(
        &over,
        Some(CardPalette::Cruise),
        blank(),
        [0xd7, 0x77, 0x57],
    );
    let lead = cells.iter().position(|c| c.ch != ' ').expect("words");
    assert!(
        cells[lead..lead + 4]
            .iter()
            .all(|c| c.bold && c.fg == inks.brand),
        "`alab` keeps the brand's ink"
    );
    let snug = (60..158)
        .find(|&w| {
            compose(muse, w)
                .switch
                .as_ref()
                .is_some_and(|s| s.label.is_some())
        })
        .expect("a width that lays the label");
    let (snug_rest, _) = slot(&compose(muse, snug));
    let (snug_over, _) = slot(&compose(over, snug));
    assert_eq!(snug_over, snug_rest, "no room: the label stands, never cut");
    let full_version = format!("1.2.3-{}", "r".repeat(CLI_VERSION_MAX - "1.2.3-".len()));
    assert_eq!(full_version.len(), CLI_VERSION_MAX);
    let full = SwitchPaint {
        hover: HoverAt::Label,
        cli_ver: Inline::new(&full_version),
        ..SwitchPaint::bare(HarnessSwitch::On, Some(aterm_phase::CliHarness::CodexAlab))
    };
    let (shown, _) = slot(&compose(full, 250));
    assert_eq!(
        shown.trim_start(),
        format!(
            "alab codex harness v{} (v{full_version})",
            aterm_phase::CliHarness::CodexAlab.version_str()
        )
    );
    let overflow = SlotWords(vec![vec![("x".repeat(LABEL_MAX + 1), LabelInk::Muted)]], 0);
    assert!(overflow.cells(LABEL_MAX + 1, LABEL_MAX + 1).is_none());
    assert_eq!(LabelFace::of_words(&overflow.0[0]).len, LABEL_MAX);

    let claude = SwitchPaint {
        hover: HoverAt::Label,
        cli_ver: Inline::new("2.1.305"),
        ..LIT
    };
    let at_rest = compose(
        SwitchPaint {
            hover: HoverAt::None,
            ..claude
        },
        158,
    );
    assert_eq!(
        compose(claude, 158).cells,
        at_rest.cells,
        "CONTROL: Claude Code's label says all of its build"
    );
    // A pane that lays the last tier (`alab v0.4`) leaves the build out: the hover says it, in
    // the label's own cells.
    let narrow = 55;
    let rest = SwitchPaint {
        hover: HoverAt::None,
        ..claude
    };
    let (rest_words, _) = slot(&compose(rest, narrow));
    assert_eq!(
        rest_words.trim_start(),
        format!(
            "alab v{}",
            aterm_phase::CliHarness::ClaudeCode.version_str()
        ),
        "the last tier at {narrow}"
    );
    let (over_words, _) = slot(&compose(claude, narrow));
    assert_eq!(
        over_words.trim_start(),
        "v2.1.305",
        "the hover names the build the tier leaves out"
    );
    assert_eq!(
        compose(claude, narrow).switch.map(|s| s.cols),
        compose(rest, narrow).switch.map(|s| s.cols),
        "the switch stands where it stood"
    );
}

/// A KEPT ARROW STAYS WITH THE PROGRAM IT WAS MINTED FOR: the row a host last sent before it
/// stood down shows on its tab's rule, with the harness off, only while the foreground group in
/// front when it was kept still is, only on a rule of the CLI its lane moves, and only while its
/// target is still past the running build; a build under the same numbers that the running one
/// is already past is never offered as an update. CONTROL: the same row kept and nothing moved
/// shows the arrow a press moves once (Claude Code's), or the held arrow (Muse Code's).
#[test]
fn a_kept_arrow_stays_with_the_program_it_was_minted_for() {
    use crate::autopilot::{HarnessSwitch, SwitchOff};
    use aterm_agent::harness::upgrade::{Agent, Phase};
    let off = HarnessSwitch::Off(SwitchOff::Master);
    let kept = |timeline: &mut crate::session_timeline::SessionTimeline, row| {
        timeline.set_upgrade(Some(row));
        timeline.set_upgrade(None);
    };
    let paint = |timeline: &crate::session_timeline::SessionTimeline, program| {
        SwitchPaint::of(
            off,
            HoverAt::None,
            Some(program),
            TopCli::from_timeline(timeline),
        )
        .up
    };
    let mut timeline = crate::session_timeline::SessionTimeline::default();
    timeline.note_foreground_group(4242);
    kept(
        &mut timeline,
        up_row("2.1.305", "2.1.310", Phase::Pending, "background"),
    );
    let shown = paint(&timeline, aterm_phase::Program::Claude);
    assert_eq!(
        (shown.state, shown.to.as_str(), shown.why),
        (UpState::Ready, "2.1.310", ""),
        "CONTROL: kept, and nothing moved: a press moves it once with the harness off"
    );
    assert_eq!(
        paint(&timeline, aterm_phase::Program::Muse),
        TopUp::default(),
        "Claude Code's row on a Muse Code rule: none"
    );
    let cli = TopCli::from_timeline(&timeline);
    let foreign = TopCli {
        kept: TopUp {
            agent: Agent::Muse,
            ..cli.kept
        },
        ..cli
    };
    assert_ne!(
        cli.term(),
        foreign.term(),
        "a changed lane repaints its arrow"
    );
    timeline.note_foreground_group(4343);
    assert_eq!(
        paint(&timeline, aterm_phase::Program::Claude),
        TopUp::default(),
        "another program in front: none"
    );
    timeline.note_foreground_group(4242);
    assert_eq!(
        paint(&timeline, aterm_phase::Program::Claude).state,
        UpState::Ready,
        "the program it was kept for back in front"
    );
    let mut muse = crate::session_timeline::SessionTimeline::default();
    muse.note_foreground_group(77);
    kept(
        &mut muse,
        aterm_agent::harness::upgrade_drive::Row {
            agent: Agent::Muse,
            ..up_row("1.4.10-R6427.1", "1.4.10-R6500.1", Phase::Pending, "")
        },
    );
    assert_eq!(
        paint(&muse, aterm_phase::Program::Muse).state,
        UpState::Held,
        "CONTROL: a later revision kept for Muse Code"
    );
    // The build that runs now, past the kept target under the same numbers.
    muse.set_claude_footer(
        77,
        None,
        Some(aterm_agent::harness::footer::FooterFacts {
            version: Some("1.4.10-R6501.1".to_owned()),
            ..aterm_agent::harness::footer::FooterFacts::default()
        }),
    );
    assert_eq!(
        TopCli::from_timeline(&muse).kept,
        TopUp::default(),
        "the running R6501.1 is past the kept R6500.1: no downgrade offered"
    );
}

/// A LIVE ARROW STAYS WITH ITS OWN PROGRAM: the tab's live row shows only on a rule of the CLI
/// its lane moves, and a row whose agent is not the foreground job (a Claude Code suspended under
/// another) shows on no rule and names no build in the label. CONTROL: Claude Code's row, its
/// agent in front, on Claude Code's rule.
#[test]
fn a_live_arrow_stays_with_its_own_program() {
    use crate::autopilot::HarnessSwitch;
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::NOT_FOREGROUND;
    let paint = |timeline: &crate::session_timeline::SessionTimeline, program| {
        SwitchPaint::of(
            HarnessSwitch::On,
            HoverAt::None,
            Some(program),
            TopCli::from_timeline(timeline),
        )
    };
    let mut timeline = crate::session_timeline::SessionTimeline::default();
    timeline.note_foreground_group(4242);
    let row = up_row("2.1.305", "2.1.310", Phase::Pending, "background");
    timeline.set_upgrade(Some(row.clone()));
    let shown = paint(&timeline, aterm_phase::Program::Claude);
    assert_eq!(
        (shown.up.state, shown.up.to.as_str(), shown.cli_ver.as_str()),
        (UpState::Ready, "2.1.310", "2.1.305"),
        "CONTROL: Claude Code's own row"
    );
    assert_eq!(
        paint(&timeline, aterm_phase::Program::Muse).up,
        TopUp::default(),
        "Claude Code's row on a Muse Code rule: none"
    );
    timeline.set_upgrade(Some(aterm_agent::harness::upgrade_drive::Row {
        aside: NOT_FOREGROUND.to_owned(),
        wait: NOT_FOREGROUND.to_owned(),
        ..row
    }));
    let behind = paint(&timeline, aterm_phase::Program::Claude);
    assert_eq!(behind.up, TopUp::default(), "a program behind: no arrow");
    assert_eq!(behind.cli_ver.as_str(), "", "nor its build in the label");
}

/// A SLOT NEVER SHOWS A CUT FORM: a form longer than a slot's cells can hold, or with a
/// character not one cell wide, is passed over for the next, however much room the rule has.
/// CONTROL: the short form after them shows, right-aligned in the label's cells.
#[test]
fn a_slot_never_shows_a_cut_form() {
    let words = |text: String| vec![(text, LabelInk::Muted)];
    let slot = SlotWords(
        vec![
            words("x".repeat(LABEL_MAX + 4)),
            words("wide \u{4e2d}".to_owned()),
            words("short".to_owned()),
        ],
        0,
    );
    assert_eq!(
        slot.cells(10, 4 * LABEL_MAX).map(|c| c.text()).as_deref(),
        Some("     short"),
        "CONTROL: the short form"
    );
    assert!(
        SlotWords(vec![words("x".repeat(LABEL_MAX + 4))], 0)
            .cells(10, 4 * LABEL_MAX)
            .is_none(),
        "past the cells: none"
    );
}

/// Why the switch `top` laid from `key` reads as a box, if it does: a cell of it off the pane's
/// ground, bold, or in an ink other than the label's quiet one (the lifted one under the pointer),
/// or a sprite of `layers` that writes a pixel of its cells.
fn boxed(
    key: &TopKey,
    top: &ComposedTop,
    layers: &[&aterm_core::render::FxLayer],
) -> Result<(), String> {
    let paint = key.switch.ok_or("no switch painted")?;
    let s = top.switch.as_ref().ok_or("no switch laid")?;
    let inks = FaceInks::of(&paint, key.palette, key.blank, key.mark_fg);
    let ink = if paint.forced {
        inks.lit
    } else if paint.hovered() {
        inks.lifted
    } else {
        inks.muted
    };
    for c in s.cols.clone() {
        let cell = top.cells[c - top.col];
        if cell.bg != key.blank.bg {
            return Err(format!(
                "column {c} {:?}: a ground of its own, {:?}",
                cell.ch, cell.bg
            ));
        }
        if cell.bold || cell.fg != ink {
            return Err(format!(
                "column {c} {:?}: bold {} in {:?}, not its {ink:?}",
                cell.ch, cell.bold, cell.fg
            ));
        }
    }
    let (cw, ch) = (i32::from(key.cell.0), i32::from(key.cell.1));
    let px = |c: usize| i32::try_from(c).expect("a column") * cw;
    let y = i32::try_from(key.frame_row).expect("a row") * ch;
    let cells = aterm_core::render::PxRect::new(px(s.cols.start), y, px(s.cols.end), y + ch);
    for l in layers {
        let wrote = l.content.extent().translate(l.offset.0, l.offset.1);
        if wrote.overlaps(cells) {
            return Err(format!(
                "layer {:#x} writes {wrote:?} over the switch's {cells:?}",
                l.id
            ));
        }
    }
    Ok(())
}

/// THE SWITCH DRAWS NO BOX: lit, off, turning on and turning off, at rest, under the pointer and
/// with the pointer on the ⬆️, plain and under High Contrast, bare and under every harness palette,
/// on a dark ground and on paper, at every width from a wide rule to the last resort, the switch
/// is its mark and word alone: each of its cells on the pane's ground, never bold, in the label's
/// quiet ink (the lifted one under the pointer, the version's lit one under High Contrast), and no
/// sprite of the top rule writes a pixel of it. NEGATIVE CONTROL: the same reading catches a cell
/// of it on a track's ground, its word bold, and the ⬆️'s ring laid over it.
#[test]
fn the_switch_draws_no_box() {
    let tail = claude_tail();
    let paper = RenderCell {
        ch: ' ',
        fg: [0x20; 3],
        bg: [0xff; 3],
        ..RenderCell::default()
    };
    let faces = [
        HarnessSwitch::On,
        HarnessSwitch::Off(SwitchOff::Master),
        HarnessSwitch::Off(SwitchOff::Agent(aterm_phase::Program::Claude)),
        HarnessSwitch::Turning,
        HarnessSwitch::TurningOff,
    ];
    let palettes = [
        None,
        Some(CardPalette::Cruise),
        Some(CardPalette::Land),
        Some(CardPalette::Off),
    ];
    let (mut checked, mut ringed, mut sample) = (0usize, 0usize, None);
    for ground in [blank(), paper] {
        for palette in palettes {
            for face in faces {
                for hover in [HoverAt::None, HoverAt::Switch, HoverAt::Arrow] {
                    for forced in [false, true] {
                        for width in [158usize, 80, 40, 26] {
                            let paint = SwitchPaint {
                                face,
                                hover,
                                forced,
                                ..arrow_paint(UpState::Ready)
                            };
                            let key = key_for(
                                aterm_phase::Program::Claude,
                                tailed_run(width - 12, &tail),
                                Some(paint),
                                palette,
                                ground,
                                KeyFace::default(),
                            );
                            let mut footer = PaneFooter::default();
                            let (mut marks, mut rings) =
                                (MarkContents::default(), RingContents::default());
                            let top = footer.compose_top(key, &tail, &mut marks, &mut rings);
                            if top.switch.is_none() {
                                continue;
                            }
                            let layers: Vec<_> = top.mark.iter().chain(top.ring.iter()).collect();
                            let at = format!(
                                "{ground:?} {palette:?} {face:?} {hover:?} forced {forced} at \
                                 {width}"
                            );
                            boxed(&key, top, &layers).unwrap_or_else(|e| panic!("{at}: {e}"));
                            checked += 1;
                            if top.ring.is_some() {
                                ringed += 1;
                            }
                            if width == 158 && hover == HoverAt::Arrow && sample.is_none() {
                                sample = top.ring.clone().map(|ring| {
                                    let copy = ComposedTop {
                                        cells: top.cells.clone(),
                                        col: top.col,
                                        switch: top.switch.clone(),
                                        ..ComposedTop::default()
                                    };
                                    (key, copy, ring)
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(checked >= 2 * 4 * 5 * 3 * 2 * 3, "{checked}");
    assert!(ringed >= 2 * 4 * 5 * 2 * 3, "the ring is met: {ringed}");
    let (key, top, ring) = sample.expect("a ringed wide rule");
    assert_eq!(boxed(&key, &top, &[&ring]), Ok(()), "CONTROL: as laid");
    let s = top.switch.clone().expect("laid");
    let first = s.cols.start - top.col;
    let altered = |edit: &dyn Fn(&mut RenderCell, usize)| {
        let mut cells = top.cells.clone();
        for (k, cell) in cells.iter_mut().enumerate() {
            edit(cell, k);
        }
        ComposedTop {
            cells,
            col: top.col,
            switch: top.switch.clone(),
            ..ComposedTop::default()
        }
    };
    let track = mix(key.blank.bg, key.blank.fg, 0.12);
    let tracked = altered(&|cell, k| {
        if k == first + 1 {
            cell.bg = track;
        }
    });
    assert!(
        boxed(&key, &tracked, &[]).is_err(),
        "NEGATIVE CONTROL: a cell on a track's ground"
    );
    let bold = altered(&|cell, k| {
        if k == first + 3 {
            cell.bold = true;
        }
    });
    assert!(
        boxed(&key, &bold, &[]).is_err(),
        "NEGATIVE CONTROL: the word bold"
    );
    let cw = i32::from(key.cell.0);
    let over = aterm_core::render::FxLayer {
        offset: (
            i32::try_from(s.cols.start).expect("a column") * cw,
            ring.offset.1,
        ),
        ..ring
    };
    assert!(
        boxed(&key, &top, &[&over]).is_err(),
        "NEGATIVE CONTROL: the ring laid over the switch"
    );
}

/// THE PRESS NAMES WHAT IT ENDS: a Claude Code press's words say when its restart lands, read off
/// the screen as the press's own lane reads it (at once, at a turn's end, once a draft is sent),
/// and name the background shells the restart ends, which it does
/// (`upgrade::RESTART_ENDS_BACKGROUND`, measured). NEGATIVE CONTROLS: where a restart left them
/// running the words name none; with no shells they name none; another CLI's arrow, whose press
/// is the tab menu's Upgrade Now, keeps the lane's words.
#[test]
fn the_press_names_what_it_ends() {
    assert_eq!(ends_words("1 shell", true), " \u{b7} ends 1 shell");
    assert_eq!(
        ends_words("1 shell", false),
        "",
        "CONTROL: shells that live on"
    );
    assert_eq!(ends_words("", true), "", "CONTROL: no shells");
    let pressed = |press: Option<&'static str>| TopUp {
        press,
        ..arrow_paint(UpState::Ready).up
    };
    let ends = if aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND {
        " \u{b7} ends 1 shell"
    } else {
        ""
    };
    for (seen, live, when) in [
        (Some(""), None, "now"),
        (Some("busy"), None, "at turn end"),
        (
            Some(""),
            Some("draft"),
            "once your draft is sent or cleared",
        ),
        (Some("busy"), Some(""), "now"),
    ] {
        assert_eq!(
            up_said(&pressed(seen), "1 shell", live).as_deref(),
            Some(format!("update to v2.1.310 \u{b7} {when}{ends}").as_str()),
            "{seen:?} {live:?}: the live screen's reading wins"
        );
    }
    assert_eq!(
        up_said(&pressed(None), "1 shell", Some("")).as_deref(),
        Some("update to v2.1.310 \u{b7} after 1 shell"),
        "CONTROL: another CLI's arrow keeps the lane's words"
    );
    assert_eq!(
        up_said(&arrow_paint(UpState::Asked).up, "1 shell", Some("")).as_deref(),
        Some("updating to v2.1.310 \u{b7} after 1 shell"),
        "CONTROL: a press under way says the lane's words"
    );
    let rule = "\u{2500}".repeat(20);
    let idle = vec![rule.clone(), "\u{276f} ".to_string(), rule.clone()];
    let turn = vec![
        "\u{273b} Pondering\u{2026} (3s \u{b7} esc to interrupt)".to_string(),
        rule.clone(),
        "\u{276f} ".to_string(),
        rule,
    ];
    assert_eq!(press_wait(&idle, false), "", "an idle screen: at once");
    assert_eq!(press_wait(&turn, false), "busy", "a turn: at its end");
    assert_eq!(press_wait(&turn, true), "draft", "a draft before a turn");
    assert_eq!(
        press_wait_of("background"),
        "",
        "a press passes background work"
    );
    assert_eq!(press_wait_of("not-idle:shell"), "");
    assert_eq!(
        press_wait_of(aterm_agent::harness::upgrade_drive::AGENT_OFF),
        ""
    );
    assert_eq!(press_wait_of("not-idle:busy"), "busy", "CONTROL: a turn");
    assert_eq!(press_wait_of("release:draft"), "draft", "CONTROL: a draft");
}

/// AN ARROW SAYS A TURN'S END ONLY FOR A TURN: the words of an arrow no press reads the screen
/// for (a move asked for or under way, another CLI's arrow) name what the lane's wait is: the
/// settle before it acts, the agent's agreement, a dialog, background work or an agent not yet
/// idle, never a turn that is not running. CONTROL: a running turn's wait says its end.
#[test]
fn an_arrow_says_a_turns_end_only_for_a_turn() {
    use aterm_agent::harness::upgrade::{Agent, Phase, Request};
    for (agent, request) in [
        (Agent::Claude, Request::Now),
        (Agent::Codex, Request::None),
        (Agent::Codex, Request::Now),
    ] {
        for (wait, when) in [
            ("settling", "once its tab settles"),
            ("awaiting-ready", "once it agrees to move"),
            ("notice-shown", "once it is idle"),
            ("status-stale:busy", "once it is idle"),
            ("not-idle:waiting", "once its dialog is answered"),
            ("not-idle:shell", "after background jobs"),
            ("not-idle:busy", "at turn end"),
            ("busy", "at turn end"),
        ] {
            let row = aterm_agent::harness::upgrade_drive::Row {
                agent,
                request: request.clone(),
                ..up_row("2.1.305", "2.1.310", Phase::Pending, wait)
            };
            let up = TopUp::of(&row);
            let verb = if request == Request::Now {
                (UpState::Asked, "updating to")
            } else {
                (UpState::Ready, "update to")
            };
            assert_eq!(up.state, verb.0, "{agent:?} {request:?} {wait:?}");
            assert_eq!(
                up_said(&up, "", None).as_deref(),
                Some(format!("{} v2.1.310 \u{b7} {when}", verb.1).as_str()),
                "{agent:?} {request:?} {wait:?}"
            );
        }
    }
}

/// A PRESS'S HOVER KEEPS ITS WAIT AND WHAT IT ENDS: with a draft in the composer, the slot under
/// the pointer on a Claude Code ⬆️ says the press waits for it and what the restart ends, before
/// the press and once it is under way. Where no form that says both fits the label's own cells
/// (35 here, with a shell to end), the slot grows left into the rule's room for the most said that
/// does: the whole sentence at 144 and 230 columns alike, the wait in its fewest words (`v2.1.310 ·
/// after your draft · ends 1 shell`) at 95; at 80, where none fits, it keeps the wait and cuts
/// what the restart ends (`v2.1.310 · after your draft`), never to the verb and the target. A
/// slot takes a form that says all it must wherever one fits, before one that cuts it. NEGATIVE
/// CONTROLS: another CLI's arrow, whose words carry no restart, keeps to the label's own cells
/// (its wait in the fewest words, the switch where it stood); a slot with no form that must be
/// said shows the first that fits the label's cells.
#[test]
fn a_press_keeps_its_wait_and_what_it_ends() {
    let words = |text: &str| vec![(text.to_owned(), LabelInk::Muted)];
    let slot = |whole| SlotWords(vec![words("says all it must"), words("cut")], whole);
    assert_eq!(
        slot(1).cells(8, 20).map(|c| c.text()).as_deref(),
        Some("says all it must"),
        "the room holds the form that says all"
    );
    assert_eq!(
        slot(1).cells(8, 10).map(|c| c.text()).as_deref(),
        Some("     cut"),
        "CONTROL: no room for it, the form that cuts"
    );
    assert_eq!(
        slot(0).cells(8, 20).map(|c| c.text()).as_deref(),
        Some("     cut"),
        "NEGATIVE CONTROL: nothing that must be said, the label's own cells"
    );
    let ends = if aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND {
        " \u{b7} ends 1 shell"
    } else {
        ""
    };
    let draft = "once your draft is sent or cleared";
    let ready = SwitchPaint {
        hover: HoverAt::Arrow,
        press: Some("draft"),
        up: TopUp {
            press: Some(""),
            ..arrow_paint(UpState::Ready).up
        },
        ..arrow_paint(UpState::Ready)
    }
    .with_jobs("1 shell");
    let asked = SwitchPaint {
        up: TopUp {
            state: UpState::Asked,
            wait: draft,
            ..ready.up
        },
        ..ready
    };
    let said = format!("update to v2.1.310 \u{b7} {draft}{ends}");
    let terse = format!("v2.1.310 \u{b7} after your draft{ends}");
    assert_eq!(
        up_said(&ready.up, "1 shell", ready.press),
        Some(said.clone())
    );
    // The verb's form grows wherever the room holds it. With no shell to end the forms are
    // shorter: a narrower rule shows the first of them its room holds, the terse one in the label.
    let asked_said = format!("updating to v2.1.310 \u{b7} {draft}{ends}");
    let first_held = |forms: [String; 3], w: usize| {
        let room = room_at(w);
        forms
            .into_iter()
            .find(|form| form.chars().count() <= room)
            .unwrap_or_else(|| terse.clone())
    };
    let (ready_95, narrow, asked_95) = if ends.is_empty() {
        (
            first_held(
                [
                    said.clone(),
                    format!("v2.1.310 \u{b7} {draft}"),
                    terse.clone(),
                ],
                95,
            ),
            terse.clone(),
            first_held(
                [
                    asked_said.clone(),
                    format!("\u{2192} v2.1.310 \u{b7} {draft}"),
                    format!("\u{2192} {terse}"),
                ],
                95,
            ),
        )
    } else {
        (
            terse.clone(),
            "v2.1.310 \u{b7} after your draft".to_owned(),
            format!("\u{2192} {terse}"),
        )
    };
    for (w, paint, want) in [
        (230, ready, said.clone()),
        (144, ready, said.clone()),
        (95, ready, ready_95),
        (80, ready, narrow),
        (144, asked, asked_said),
        (95, asked, asked_95),
    ] {
        let (_, over) = slot_top(paint, w);
        assert_eq!(slot_of(&over).as_deref(), Some(want.as_str()), "{w}");
    }
    let (_, still) = slot_top(
        SwitchPaint {
            hover: HoverAt::None,
            ..ready
        },
        144,
    );
    let (_, over) = slot_top(ready, 144);
    assert_ne!(
        laid_at(&over),
        laid_at(&still),
        "the switch and the tag move left for the grown slot"
    );
    let other = SwitchPaint {
        press: None,
        up: TopUp {
            press: None,
            wait: draft,
            ..ready.up
        },
        ..ready
    };
    let (_, other_over) = slot_top(other, 144);
    assert_eq!(
        slot_of(&other_over).as_deref(),
        Some("v2.1.310 \u{b7} after your draft"),
        "NEGATIVE CONTROL: another CLI's arrow, in the label's own cells"
    );
    assert_eq!(
        laid_at(&other_over),
        laid_at(&still),
        "NEGATIVE CONTROL: its switch and tag stand where they stood"
    );
}

/// The cells the slot under the pointer may grow into on a [`slot_top`] rule of `w` columns:
/// the longest refusal it shows whole, which a slot shows only in its room once the refusal with
/// its lead (`couldn't update · …`) does not fit there. Rooms past 60 cells are not read.
fn room_at(w: usize) -> usize {
    let room = (1..=60)
        .rev()
        .find(|&n| {
            let why = "x".repeat(n);
            let (_, top) = slot_top(
                arrow_paint(UpState::Ready).refused_with(Some(why.as_str())),
                w,
            );
            slot_of(&top).as_deref() == Some(why.as_str())
        })
        .expect("a room");
    assert!(room < 60, "{w}: a room past what is read");
    room
}

/// A PRESS'S HOVER KEEPS ITS VERB WHERE THE RULE HAS ROOM: with no draft, the slot under the
/// pointer on a Claude Code ⬆️ whose press lands now says the whole sentence (`update to v2.1.310
/// · now · ends 1 shell`), grown left into the rule's room, wherever the room holds it (230 and
/// 144 columns, and 95 or 80 where theirs does), before the form without the verb that fits the
/// label's own cells (`v2.1.310 · now · ends 1 shell`), which shows only where growing cannot
/// fit. Of a slot's forms that say all they must, each in turn is shown wherever it fits, the
/// label's cells or the room. NEGATIVE CONTROLS: where the room cannot hold the verb's form, the
/// shorter one takes the label's cells and nothing moves; a slot with no form that must be said
/// shows the first that fits the label's cells though the room would hold a longer one; and
/// another CLI's arrow, whose words carry no restart, keeps to the label's own cells without its
/// verb, its switch and tag where they stood.
#[test]
fn a_press_keeps_its_verb_where_the_rule_has_room() {
    let words = |text: &str| vec![(text.to_owned(), LabelInk::Muted)];
    let slot = |whole| {
        SlotWords(
            vec![
                words("the most said form"),
                words("said form"),
                words("cut"),
            ],
            whole,
        )
    };
    assert_eq!(
        slot(2).cells(10, 20).map(|c| c.text()).as_deref(),
        Some("the most said form"),
        "the room holds the most said form: it grows before the shorter one fits the label"
    );
    assert_eq!(
        slot(2).cells(10, 15).map(|c| c.text()).as_deref(),
        Some(" said form"),
        "NEGATIVE CONTROL: no room for it, the shorter one in the label's cells"
    );
    assert_eq!(
        slot(0).cells(10, 20).map(|c| c.text()).as_deref(),
        Some(" said form"),
        "NEGATIVE CONTROL: nothing that must be said, the label's own cells first"
    );
    let ends = if aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND {
        " \u{b7} ends 1 shell"
    } else {
        ""
    };
    let ready = SwitchPaint {
        hover: HoverAt::Arrow,
        press: Some(""),
        up: TopUp {
            press: Some(""),
            ..arrow_paint(UpState::Ready).up
        },
        ..arrow_paint(UpState::Ready)
    }
    .with_jobs("1 shell");
    let said = format!("update to v2.1.310 \u{b7} now{ends}");
    let verbless = format!("v2.1.310 \u{b7} now{ends}");
    assert_eq!(
        up_said(&ready.up, "1 shell", ready.press),
        Some(said.clone())
    );
    let still = |w| {
        slot_top(
            SwitchPaint {
                hover: HoverAt::None,
                ..ready
            },
            w,
        )
        .1
    };
    let mut grew = Vec::new();
    for w in [230, 144, 95, 80] {
        let label = slot_of(&still(w)).expect("the label").chars().count();
        let room = if w > 100 { usize::MAX } else { room_at(w) };
        assert!(
            verbless.chars().count() <= label,
            "{w}: the form without the verb fits the label's {label} cells"
        );
        let grows = said.chars().count() <= room;
        let (_, over) = slot_top(ready, w);
        let shown = slot_of(&over).expect("the slot");
        assert_eq!(
            shown,
            if grows {
                said.as_str()
            } else {
                verbless.as_str()
            },
            "{w}: a label of {label} cells, a room of {room}"
        );
        assert_eq!(
            laid_at(&over) != laid_at(&still(w)),
            shown.chars().count() > label,
            "{w}: the switch and the tag move left only for a grown slot"
        );
        grew.push(grows);
    }
    assert_eq!(&grew[..2], [true, true], "230 and 144 hold the verb's form");
    let other = SwitchPaint {
        press: None,
        up: TopUp {
            press: None,
            wait: "after background jobs",
            ..ready.up
        },
        ..ready
    }
    .with_jobs("");
    let (_, other_over) = slot_top(other, 144);
    assert_eq!(
        slot_of(&other_over).as_deref(),
        Some("v2.1.310 \u{b7} after background jobs"),
        "NEGATIVE CONTROL: another CLI's arrow, in the label's own cells"
    );
    assert_eq!(
        laid_at(&other_over),
        laid_at(&still(144)),
        "NEGATIVE CONTROL: its switch and tag stand where they stood"
    );
}

/// A PRESS'S WAIT SURVIVES A NARROW RULE: where no form that names the shells a Claude Code
/// restart ends fits (80 columns, Claude's effort tag on the rule), the slot under the pointer
/// still says what the press waits for, in its fewest words, before the verb and the target:
/// `v2.1.310 · after your draft`, `· after its dialog`, `· after the hold`, and an asked move's
/// `→ v2.1.310 · after your draft`, each in the label's own cells, so nothing moves. CONTROLS:
/// at 95 the form that names the shells still wins; the verb and the target remain the form after
/// it. NEGATIVE CONTROLS: with no shell to end there is no second form in the fewest words (the
/// one there is says the wait alone already); another CLI's arrow, whose words carry no restart,
/// gets none either.
#[test]
fn a_press_keeps_its_wait_on_a_narrow_rule() {
    let ends = if aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND {
        " \u{b7} ends 1 shell"
    } else {
        ""
    };
    let pressed = |wait: &'static str| {
        SwitchPaint {
            hover: HoverAt::Arrow,
            press: Some(wait),
            up: TopUp {
                press: Some(""),
                ..arrow_paint(UpState::Ready).up
            },
            ..arrow_paint(UpState::Ready)
        }
        .with_jobs("1 shell")
    };
    let texts = |paint: &SwitchPaint| -> Vec<String> {
        up_forms(&paint.up, paint.jobs.as_str(), paint.press)
            .expect("forms")
            .0
            .iter()
            .map(|form| form.iter().map(|(t, _)| t.as_str()).collect())
            .collect()
    };
    let still = SwitchPaint {
        hover: HoverAt::None,
        ..pressed("draft")
    };
    let (_, rest) = slot_top(still, 80);
    let label = slot_of(&rest).expect("the label").chars().count();
    for (wait, terse) in [
        ("draft", "after your draft"),
        ("box", "after its dialog"),
        ("held", "after the hold"),
    ] {
        let paint = pressed(wait);
        let bare = format!("v2.1.310 \u{b7} {terse}");
        let forms = texts(&paint);
        let at = forms
            .iter()
            .position(|f| *f == bare)
            .expect("the wait alone");
        assert_eq!(
            forms.get(at + 1).map(String::as_str),
            Some("update to v2.1.310"),
            "{wait}: the verb and the target after it"
        );
        let (_, over) = slot_top(paint, 80);
        let shown = slot_of(&over).expect("the slot");
        assert!(
            shown.starts_with(&bare),
            "{wait} at 80: the wait survives, {shown:?}"
        );
        if wait == "draft" {
            assert_eq!(shown, bare, "{wait} at 80: the wait alone");
            assert!(
                bare.chars().count() <= label,
                "{wait}: in the label's own cells"
            );
            assert_eq!(
                laid_at(&over),
                laid_at(&rest),
                "{wait}: its switch and tag stand where they stood"
            );
        }
        let (_, wider) = slot_top(paint, 95);
        let wider = slot_of(&wider).expect("the slot");
        assert!(
            ends.is_empty() || wider.ends_with(ends),
            "CONTROL: {wait} at 95, the shells named: {wider:?}"
        );
    }
    let asked = SwitchPaint {
        up: TopUp {
            state: UpState::Asked,
            wait: "once your draft is sent or cleared",
            ..pressed("draft").up
        },
        ..pressed("draft")
    };
    let (_, asked_over) = slot_top(asked, 80);
    assert_eq!(
        slot_of(&asked_over).as_deref(),
        Some("\u{2192} v2.1.310 \u{b7} after your draft"),
        "an asked move at 80"
    );
    let quiet = pressed("draft").with_jobs("");
    assert_eq!(
        texts(&quiet)
            .iter()
            .filter(|f| f.contains("after your draft"))
            .count(),
        1,
        "NEGATIVE CONTROL: no shell to end, one form in the fewest words"
    );
    let other = SwitchPaint {
        press: None,
        up: TopUp {
            press: None,
            wait: "once your draft is sent or cleared",
            ..pressed("draft").up
        },
        ..pressed("draft")
    };
    assert_eq!(
        texts(&other)
            .iter()
            .filter(|f| f.contains("after your draft"))
            .count(),
        1,
        "NEGATIVE CONTROL: another CLI's arrow, one form in the fewest words"
    );
}
