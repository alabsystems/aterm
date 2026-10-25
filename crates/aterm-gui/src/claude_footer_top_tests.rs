// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Claude composer top rule lockup, harness switch, and tint.

use super::probe::*;
use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;

const LIT: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::On,
    hover: false,
    version: None,
};

/// An off face: its agent's own switch holds the harness off (the master's reads the same).
const OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Off(SwitchOff::Agent(aterm_phase::Program::Claude)),
    hover: false,
    version: None,
};

const MASTER_OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Off(SwitchOff::Master),
    hover: false,
    version: None,
};

const TURNING: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Turning,
    hover: false,
    version: None,
};

const TURNING_OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::TurningOff,
    hover: false,
    version: None,
};

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

fn scan_run(
    scratch: &RenderInput,
    region: PaneTextRegion,
    top_rule: usize,
    blank: RenderCell,
    muse: bool,
) -> Option<TopRun> {
    let frame_row = region.strip + region.row_off + top_rule;
    let frame_cells = scratch.cells.get(frame_row)?;
    let pane_end = frame_cells.len().min(region.col_off + region.cols);
    let pane_slice = frame_cells.get(region.col_off..pane_end)?;
    if pane_slice.is_empty() || pane_slice.iter().any(|c| c.bg != blank.bg) {
        return None;
    }
    let first = pane_slice.iter().find(|c| c.ch == '\u{2500}').copied()?;
    let last = pane_slice
        .iter()
        .rev()
        .find(|c| c.ch == '\u{2500}')
        .copied()
        .unwrap_or(first);
    let count = |style: &RenderCell| pane_slice.iter().filter(|c| *c == style).count();
    let rule = if first == last || count(&last) >= count(&first) {
        last
    } else {
        first
    };
    let mut best = 0..0;
    let mut cur_start = None;
    for (idx, cell) in pane_slice.iter().enumerate() {
        let col = region.col_off + idx;
        let plain = *cell == rule
            && !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1));
        if plain {
            let s = *cur_start.get_or_insert(idx);
            if idx + 1 - s >= best.len() {
                best = s..idx + 1;
            }
        } else {
            cur_start = None;
        }
    }
    let mut voice = None;
    if muse
        && let (Some(first), Some(last), Some(ink)) = (
            pane_slice.iter().position(|c| c.ch == '\u{2500}'),
            pane_slice.iter().rposition(|c| c.ch == '\u{2500}'),
            pane_slice
                .iter()
                .rposition(|c| c.ch != '\u{2500}' && c.ch != ' '),
        )
        && first < ink
    {
        let label = (ink + 2).min(pane_slice.len());
        let end = (last + 1).max(label);
        let text: String = pane_slice.iter().map(|c| c.ch).collect();
        let untouched = (first..end).all(|idx| {
            let col = region.col_off + idx;
            !touches(&scratch.clusters, frame_row, &(col..col + 1))
                && !touches(&scratch.combining, frame_row, &(col..col + 1))
                && !touches(&scratch.images, frame_row, &(col..col + 1))
        });
        if untouched && aterm_phase::muse::is_idle_voice_rule(&text) {
            voice = Some(VoiceRule {
                col: region.col_off + first,
                len: end - first,
                label: label - first,
            });
        }
    }
    Some(TopRun {
        col: region.col_off + best.start,
        len: best.len(),
        rule,
        voice,
    })
}

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
    (Some(SwitchPaint { hover: true, ..OFF }), None),
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
    }
}

#[test]
fn the_switch_sits_where_the_scan_put_it_with_no_ghost() {
    use aterm_phase::Program;
    const LABEL: usize = 32;
    let mut compared = 0;
    let mut drawn = 0;
    let mut lockups = 0;
    let mut voiced = 0;
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
                let run = read_top_run(scratch, region, 20, TopLine::Rule, blank, muse);
                assert_eq!(
                    run,
                    scan_run(scratch, region, 20, blank, muse),
                    "{cols} cols, {what}, {program:?}: the scan's run"
                );
                for (switch, palette) in CASES {
                    let at = format!("{cols} cols, {what}, {program:?}, {switch:?}, {palette:?}");
                    compared += 1;
                    let Some(run) = run else {
                        continue;
                    };
                    let mut top = ComposedTop::default();
                    let key = TopKey {
                        frame_row: region.strip + 20,
                        ..key_for(program, run, switch, palette, blank, KeyFace::default())
                    };
                    compose_top(&key, &mut top, None, None);
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
                            || (top.col >= laid.col
                                && top.col + top.cells.len() <= laid.col + laid.len),
                        "{at}: every cell written lies in the run"
                    );
                    let want = switch.and_then(|p| fitted_switch_face(p, laid.len));
                    assert_eq!(
                        top.switch.is_some(),
                        want.is_some(),
                        "{at}: a switch exactly where hb-int laid one"
                    );
                    if let (Some(laid_switch), Some(paint)) = (&top.switch, switch) {
                        let face = switch_face(paint, laid_switch.tier).expect("the laid form");
                        let end = laid.col + laid.len - 2;
                        assert_eq!(laid_switch.cols, end - face.len..end, "{at}: right-aligned");
                        let text: String = top.cells
                            [laid_switch.cols.start - top.col..end - top.col]
                            .iter()
                            .map(|c| c.ch)
                            .collect();
                        assert_eq!(text, face.text(), "{at}: the form laid");
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
                            "{at}: two cells in, one compact"
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
                        TopRun { len, ..run },
                        Some(LIT),
                        Some(CardPalette::Cruise),
                        blank,
                        KeyFace::default(),
                    )
                };
                let (mut whole, mut short) = (ComposedTop::default(), ComposedTop::default());
                compose_top(&key(run.len), &mut whole, None, None);
                compose_top(&key(run.len - 1), &mut short, None, None);
                assert_ne!(
                    whole.cells, short.cells,
                    "CONTROL {cols}: a shorter run composes otherwise"
                );
            }
        }
    }
    let rules: usize = COLS.iter().map(|&cols| top_rules(cols).len()).sum();
    assert_eq!(compared, rules * MARKED.len() * CASES.len());
    assert!(drawn > 300, "most compositions lay a switch: {drawn}");
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
            && at_rest.contains("\u{2500} alab harness \u{25cf} on \u{2500}"),
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
        narrowed.contains("alab harness") && narrowed.chars().count() == cols,
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
        "CONTROL: one composition, one content sealed"
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
    }
}

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [220, 220, 220],
        ..RenderCell::default()
    }
}

fn composed_for(
    program: aterm_phase::Program,
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
) -> ComposedTop {
    composed_as((program, Build::Vendor), run, switch, palette, face)
}

fn composed_as(
    (program, build): (aterm_phase::Program, Build),
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
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
    );
    top
}

fn composed(
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
) -> ComposedTop {
    composed_for(aterm_phase::Program::Claude, run, switch, palette, face)
}

const GHOST: KeyFace = KeyFace {
    ghost: Some(false),
    tag: None,
    mark: None,
};

fn line_of(top: &ComposedTop, run: TopRun) -> Vec<char> {
    let mut line = vec!['\u{2500}'; run.len];
    for (k, c) in top.cells.iter().enumerate() {
        if let Some(slot) = line.get_mut(top.col + k - run.col) {
            *slot = c.ch;
        }
    }
    line
}

#[test]
fn the_switch_yields_its_cells_to_the_ghost() {
    let key = aterm_primer::CLAUDE_TOP_EFFORT_KEY;
    let ghost_text = format!(" {key} off ");
    let lockup = CliMark::Claude.lockup_cells(Build::Vendor);
    let mut seen = [false; 4];
    for len in (19..=140).rev() {
        let run = plain_run(len);
        let top = composed(run, Some(LIT), Some(CardPalette::Cruise), GHOST);
        let at = len - GHOST_CELLS - 1;
        let text: String = top.cells.iter().map(|c| c.ch).collect();
        let Some((rung, Some(face))) = lay_top(at, Some(LIT), Some(lockup)) else {
            assert_eq!(
                (top.col, text.as_str()),
                (run.col + at, ghost_text.as_str()),
                "{len}"
            );
            assert_eq!(top.switch, None, "{len}: no switch");
            seen[3] = true;
            continue;
        };
        seen[match rung.lockup {
            Some(Lockup::Full) => 0,
            Some(Lockup::Icon) => 1,
            None => 2,
        }] = true;
        let b_len = face.len;
        assert_eq!(top.col, run.col, "{len}: from the run's start");
        assert_eq!(
            top.cells.len(),
            len - 1,
            "{len}: Claude's last `\u{2500}` left as is"
        );
        assert_eq!(&text[text.len() - ghost_text.len()..], ghost_text, "{len}");
        assert_eq!(
            top.ghost,
            Some((run.col + at + 1, run.col + at + GHOST_CELLS - 1)),
            "{len}: the ghost's glyphs are its target"
        );
        for cell in &top.cells[at..] {
            assert_eq!(
                (cell.fg, cell.bold, cell.script),
                (run.rule.fg, false, aterm_core::terminal::Script::Normal),
                "{len}: the ghost in Claude's gray"
            );
        }
        let chars: Vec<char> = top.cells[..at].iter().map(|c| c.ch).collect();
        let laid: String = chars[at - 2 - b_len..at - 2].iter().collect();
        assert_eq!(laid, face.text(), "{len}: the ladder's form");
        assert!(
            chars[at - 2..].iter().all(|&c| c == '\u{2500}'),
            "{len}: two `\u{2500}` between the switch and the ghost"
        );
        let switch = top.switch.clone().expect("the switch");
        assert_eq!(
            (Some(switch.tier), switch.cols.clone()),
            (rung.tier, run.col + at - 2 - b_len..run.col + at - 2),
            "{len}"
        );
        let no_key = composed(
            run,
            Some(LIT),
            Some(CardPalette::Cruise),
            KeyFace::default(),
        );
        assert_eq!(no_key.cells.len(), len, "{len}: the run whole");
        assert_eq!(no_key.ghost, None);
        let end = no_key.switch.expect("the switch").cols.end;
        assert_eq!(end + 2, run.col + len, "CONTROL {len}: at the run's end");
        let bare = composed(run, Some(MASTER_OFF), None, GHOST);
        if let Some(switch) = bare.switch.clone() {
            let first = if bare.lockup.is_empty() {
                switch.cols.start
            } else {
                bare.lockup.start
            };
            assert_eq!(bare.col, first, "{len}: bare, from the first cell drawn");
            assert_eq!(switch.cols.end + 2, run.col + at, "{len}: bare");
            let text: String = bare.cells.iter().map(|c| c.ch).collect();
            assert!(
                text.ends_with(&format!("\u{2500}\u{2500}{ghost_text}")),
                "{len}: bare, the switch then the ghost: {text:?}"
            );
            assert!(text.contains("\u{2590} on \u{258c}"), "{len}: {text:?}");
        }
        let unhosted = composed(run, None, None, GHOST);
        assert_eq!(unhosted.switch, None);
        let text: String = unhosted.cells.iter().map(|c| c.ch).collect();
        assert!(text.ends_with(&ghost_text), "{len}: no host, the ghost");
        assert_eq!(
            unhosted.col, unhosted.lockup.start,
            "{len}: no host, from the lockup"
        );
    }
    assert_eq!(seen, [true; 4], "every lockup, and no switch, was laid");
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
            for len in 0..=200 {
                let run = plain_run(len);
                let top = composed_as((program, build), run, switch, palette, KeyFace::default());
                let at = format!("{program:?} {build:?} {switch:?} {palette:?} {len}");
                let Some((rung, _)) = lay_top(len, switch, Some(mark.lockup_cells(build))) else {
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
                let limit = top.switch.as_ref().map_or(run.col + len, |s| s.cols.start);
                assert!(
                    gap_holds(top.lockup.end, limit, rung.lead),
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
                    let (rungs, n) = ladder(switch, Some(mark.lockup_cells(build)));
                    let last_full = rungs[..n]
                        .iter()
                        .rev()
                        .find(|r| r.lockup == Some(Lockup::Full))
                        .copied();
                    let short = composed_as(
                        (program, build),
                        plain_run(rung.need - 1),
                        switch,
                        palette,
                        KeyFace::default(),
                    );
                    if len == rung.need && last_full == Some(rung) {
                        controls += 1;
                        let text: String = short.cells.iter().map(|c| c.ch).collect();
                        assert!(
                            !text.contains(mark.word()),
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
    let (vendor, alab) = (
        CliMark::Codex.lockup_cells(Build::Vendor),
        CliMark::Codex.lockup_cells(Build::Alab),
    );
    let broken: Vec<usize> = (0..=200)
        .filter(|&span| {
            let Some((rung, Some(face))) = lay_top(span, Some(LIT), Some(vendor)) else {
                return false;
            };
            rung.lockup == Some(Lockup::Full)
                && !gap_holds(rung.lead + alab, span - 2 - face.len, rung.lead)
        })
        .collect();
    assert!(
        broken.contains(&26) && broken.len() > 5,
        "CONTROL: the vendor's ladder crowds codex-alab's switch: {broken:?}"
    );
}

fn gap_holds(end: usize, limit: usize, lead: usize) -> bool {
    limit >= end + lead
}

fn rung_name(rung: Option<Rung>) -> String {
    let Some(r) = rung else {
        return "nothing".into();
    };
    let compact = if r.lead == 1 { " compact" } else { "" };
    match (r.lockup, r.tier) {
        (Some(Lockup::Full), Some(t)) => format!("full s{t}"),
        (Some(Lockup::Icon), Some(t)) => format!("icon s{t}{compact}"),
        (None, Some(t)) => format!("alone s{t}"),
        (Some(Lockup::Full), None) => "full".into(),
        (Some(Lockup::Icon), None) => format!("icon{compact}"),
        (None, None) => "nothing".into(),
    }
}

fn walk_order(rungs: &[Rung], last: Option<usize>) -> Result<(), String> {
    let mut before: Option<Rung> = first_fit(200, rungs);
    for span in (0..200).rev() {
        let now = first_fit(span, rungs);
        let (Some(was), now) = (before, now) else {
            if now.is_some() {
                return Err(format!("{span}: something came back"));
            }
            continue;
        };
        let tier = |r: Option<Rung>| r.and_then(|r| r.tier);
        let lockup = |r: Option<Rung>| r.and_then(|r| r.lockup);
        if tier(now) < tier(Some(was)) && now.is_some_and(|r| r.tier.is_some()) {
            return Err(format!("{span}: the tier widened"));
        }
        let had_name = lockup(Some(was)) == Some(Lockup::Full);
        let has_name = lockup(now) == Some(Lockup::Full);
        if had_name && !has_name && was.tier != last {
            return Err(format!(
                "{span}: the name went before the switch's last form ({} then {})",
                rung_name(Some(was)),
                rung_name(now)
            ));
        }
        if !had_name && has_name {
            return Err(format!("{span}: the name came back"));
        }
        if was.lockup.is_some() && lockup(now).is_none() && now.is_some() && had_name {
            return Err(format!("{span}: the icon went with the name"));
        }
        if was.lockup.is_none() && lockup(now).is_some() {
            return Err(format!("{span}: the icon came back"));
        }
        if was.lockup.is_some() && now.is_none() && was.tier.is_some() {
            return Err(format!("{span}: the switch went with the icon"));
        }
        before = now;
    }
    Ok(())
}

type Shown = (
    (CliMark, Build),
    &'static str,
    &'static [(usize, &'static str)],
);

const SHOWS: &[Shown] = &[
    (
        (CliMark::Claude, Build::Vendor),
        "on",
        &[
            (108, "full s0"),
            (68, "full s0"),
            (38, "full s0"),
            (21, "icon s2"),
            (14, "icon s2 compact"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "off",
        &[
            (108, "full s0"),
            (68, "full s0"),
            (38, "full s1"),
            (21, "icon s3"),
            (14, "alone s3"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "turning",
        &[
            (108, "full s0"),
            (68, "full s0"),
            (38, "full s1"),
            (21, "icon s3"),
            (14, "icon s3 compact"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "on",
        &[
            (104, "full s0"),
            (64, "full s0"),
            (34, "full s1"),
            (17, "icon s2"),
            (10, "alone s2"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "off",
        &[
            (104, "full s0"),
            (64, "full s0"),
            (34, "full s2"),
            (17, "icon s3 compact"),
            (10, "nothing"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "turning",
        &[
            (104, "full s0"),
            (64, "full s0"),
            (34, "full s2"),
            (17, "icon s3"),
            (10, "alone s3"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "on",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s1"),
            (26, "full s2"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "off",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s2"),
            (26, "full s3"),
        ],
    ),
    (
        (CliMark::Codex, Build::Alab),
        "on",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (40, "full s0"),
            (39, "full s1"),
            (33, "full s1"),
            (27, "full s2"),
            (26, "icon s2"),
            (16, "icon s2"),
            (15, "icon s2 compact"),
            (13, "alone s2"),
        ],
    ),
    (
        (CliMark::Codex, Build::Alab),
        "off",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (48, "full s0"),
            (47, "full s1"),
            (39, "full s2"),
            (33, "full s3"),
            (29, "full s3"),
            (28, "icon s3"),
            (26, "icon s3"),
        ],
    ),
    (
        (CliMark::Codex, Build::Alab),
        "turning",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s3"),
            (27, "full s3"),
            (26, "icon s3"),
        ],
    ),
    (
        (CliMark::Codex, Build::Vendor),
        "none",
        &[(120, "full"), (26, "full"), (14, "full"), (13, "icon")],
    ),
    (
        (CliMark::Agy, Build::Vendor),
        "on",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s0"),
            (26, "full s1"),
        ],
    ),
    (
        (CliMark::Agy, Build::Vendor),
        "off",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s1"),
            (26, "full s3"),
        ],
    ),
    (
        (CliMark::Agy, Build::Vendor),
        "turning",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s1"),
            (26, "full s3"),
        ],
    ),
    (
        (CliMark::Muse, Build::Vendor),
        "on",
        &[
            (88, "full s0"),
            (48, "full s0"),
            (18, "icon s2"),
            (1, "nothing"),
        ],
    ),
    (
        (CliMark::Muse, Build::Vendor),
        "off",
        &[
            (88, "full s0"),
            (48, "full s0"),
            (18, "icon s3"),
            (1, "nothing"),
        ],
    ),
    (
        (CliMark::Muse, Build::Vendor),
        "turning",
        &[
            (88, "full s0"),
            (48, "full s0"),
            (18, "icon s3"),
            (1, "nothing"),
        ],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "none",
        &[(108, "full"), (38, "full"), (21, "full"), (14, "icon")],
    ),
    (
        (CliMark::Claude, Build::Vendor),
        "none",
        &[(104, "full"), (34, "full"), (17, "full"), (10, "icon")],
    ),
    (
        (CliMark::Muse, Build::Vendor),
        "none",
        &[(88, "full"), (48, "full"), (18, "full"), (1, "nothing")],
    ),
    (
        (CliMark::Grok, Build::Vendor),
        "none",
        &[(120, "full"), (33, "full"), (26, "full")],
    ),
    (
        (CliMark::Kimi, Build::Vendor),
        "none",
        &[(120, "full"), (33, "full"), (26, "full")],
    ),
];

fn paint_of(state: &str) -> Option<SwitchPaint> {
    match state {
        "on" => Some(LIT),
        "off" => Some(OFF),
        "turning" => Some(TURNING),
        _ => None,
    }
}

#[test]
fn the_ladder_gives_up_words_then_the_name_then_the_icon() {
    let mut walked = 0;
    for mark in CliMark::ALL {
        for build in Build::ALL {
            for switch in [Some(LIT), Some(OFF), Some(MASTER_OFF), Some(TURNING), None] {
                let (rungs, n) = ladder(switch, Some(mark.lockup_cells(build)));
                let last = rungs[..n].iter().filter_map(|r| r.tier).max();
                walk_order(&rungs[..n], last)
                    .unwrap_or_else(|e| panic!("{mark:?} {build:?} {switch:?}: {e}"));
                walked += 1;
            }
        }
    }
    assert_eq!(walked, CliMark::ALL.len() * Build::ALL.len() * 5);
    for ((mark, build), state, cells) in SHOWS {
        for &(span, want) in *cells {
            let rung =
                lay_top(span, paint_of(state), Some(mark.lockup_cells(*build))).map(|(r, _)| r);
            assert_eq!(
                rung_name(rung),
                want,
                "{mark:?} {build:?} {state} at span {span}"
            );
        }
    }
    let vendor = lay_top(
        26,
        Some(LIT),
        Some(CliMark::Codex.lockup_cells(Build::Vendor)),
    );
    assert_eq!(
        rung_name(vendor.map(|(r, _)| r)),
        "full s2",
        "CONTROL: the vendor's ladder keeps a name at 26"
    );
    let (rungs, n) = ladder(Some(OFF), Some(CliMark::Agy.lockup_cells(Build::Vendor)));
    let mut mutant: Vec<Rung> = rungs[..n].to_vec();
    let icon = mutant
        .iter()
        .position(|r| r.lockup == Some(Lockup::Icon))
        .expect("the icon's rung");
    let rung = mutant.remove(icon);
    mutant.insert(icon - 1, rung);
    assert!(
        walk_order(&mutant, Some(3)).is_err(),
        "CONTROL: the mutant breaks the order"
    );
    assert_eq!(
        rung_name(first_fit(26, &mutant)),
        "icon s3",
        "CONTROL: agy off at 26 shows the icon alone"
    );
    assert_eq!(
        rung_name(first_fit(26, &rungs[..n])),
        "full s3",
        "the ladder shows the name there"
    );
}

fn hb_int_faces(paint: SwitchPaint, word: &str) -> Vec<String> {
    match paint.face {
        HarnessSwitch::On => vec![
            format!(" {word} harness \u{25cf} on "),
            format!(" {word} \u{25cf} on "),
            " \u{25cf} on ".into(),
        ],
        HarnessSwitch::Off(_) => vec![
            format!(" {word} harness \u{25cb} off \u{2590} on \u{258c} "),
            format!(" {word} \u{25cb} off \u{2590} on \u{258c} "),
            " \u{25cb} off \u{2590} on \u{258c} ".into(),
            " \u{2590} on \u{258c} ".into(),
        ],
        HarnessSwitch::Turning => vec![
            format!(" {word} harness \u{25d0} turning on "),
            format!(" {word} \u{25d0} turning on "),
            " \u{25d0} turning on ".into(),
            " \u{25d0} on ".into(),
        ],
        HarnessSwitch::TurningOff => vec![
            format!(" {word} harness \u{25d0} turning off "),
            format!(" {word} \u{25d0} turning off "),
            " \u{25d0} turning off ".into(),
            " \u{25d0} off ".into(),
        ],
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
            let old = hb_int_faces(paint, mark.word());
            for span in 0..=200 {
                let stood = old.iter().any(|f| f.chars().count() + 4 <= span);
                let laid = lay_top(span, Some(paint), Some(mark.lockup_cells(build)))
                    .and_then(|(_, face)| face);
                let at = format!("{program:?} {build:?} {paint:?} {span}");
                assert_eq!(laid.is_some(), stood, "{at}: a switch where one stood");
                assert_eq!(
                    fitted_switch_face(paint, span).is_some(),
                    stood,
                    "{at}: the switch alone"
                );
                if let Some(face) = laid {
                    let top = composed_as(
                        (program, build),
                        plain_run(span),
                        Some(paint),
                        None,
                        KeyFace::default(),
                    );
                    let start = plain_run(span).col + span - 2 - face.len;
                    assert_eq!(
                        top.switch.map(|s| s.cols),
                        Some(start..start + face.len),
                        "{at}: at span - 2 - len"
                    );
                }
            }
        }
    }
    for (paint, lost) in [(LIT, 10..=13), (OFF, 12..=15)] {
        let (rungs, n) = ladder(
            Some(paint),
            Some(CliMark::Claude.lockup_cells(Build::Vendor)),
        );
        let without: Vec<Rung> = rungs[..n]
            .iter()
            .copied()
            .filter(|r| r.lockup.is_some())
            .collect();
        let missing: Vec<usize> = (0..=200)
            .filter(|&span| {
                first_fit(span, &rungs[..n]).is_some() && first_fit(span, &without).is_none()
            })
            .collect();
        assert_eq!(
            missing,
            lost.collect::<Vec<_>>(),
            "CONTROL {paint:?}: the spans the alone rung holds"
        );
    }
}

fn mark_cell_free(line: &[char], span: usize, switch: bool) -> bool {
    span >= 2 && line[span - 1] == '\u{2500}' && (!switch || line[span - 2] == '\u{2500}')
}

#[test]
fn the_mark_cell_survives_every_rung() {
    let mut checked = 0;
    for (program, build) in LOCKUPS {
        for (switch, palette) in CASES {
            for len in 6..=200 {
                let run = plain_run(len);
                let top = composed_as((program, build), run, switch, palette, KeyFace::default());
                if top.cells.is_empty() {
                    continue;
                }
                let line = line_of(&top, run);
                assert!(
                    mark_cell_free(&line, len, top.switch.is_some()),
                    "{program:?} {build:?} {switch:?} {len}: {:?}",
                    line.iter().collect::<String>()
                );
                let tag = (run.col + len + 1, run.col + len + 10);
                let marked = composed_as(
                    (program, build),
                    run,
                    switch,
                    palette,
                    KeyFace {
                        ghost: None,
                        tag: Some(tag),
                        mark: Some('\u{25d0}'),
                    },
                );
                let at = run.col + len - 1 - marked.col;
                assert_eq!(
                    (marked.cells[at].ch, marked.cells[at].fg),
                    ('\u{25d0}', [0xd7, 0x77, 0x57]),
                    "{program:?} {build:?} {switch:?} {len}: the mark, in the accent"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 5000, "{checked}");
    let lockup = CliMark::Claude.lockup_cells(Build::Vendor);
    let span = 14;
    let (rung, face) = lay_top(span, Some(LIT), Some(lockup)).expect("a rung");
    assert_eq!(rung_name(Some(rung)), "icon s2 compact");
    let face = face.expect("the switch");
    let mut line = vec!['\u{2500}'; span];
    let start = span - 1 - face.len;
    for (k, &(ch, _)) in face.cells().iter().enumerate() {
        line[start + k] = ch;
    }
    assert!(
        !mark_cell_free(&line, span, true),
        "CONTROL: a tail of one is caught"
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

fn flows(
    cells: &[RenderCell],
    switch: &LaidSwitch,
    col: usize,
    palette: CardPalette,
    face: crate::autopilot::HarnessSwitch,
) -> Result<(), String> {
    let at = |c: usize| cells[c - col];
    let letter = |t: f32| {
        let l = brand_letter_fg(palette, t, blank());
        match face {
            HarnessSwitch::Turning | HarnessSwitch::TurningOff => turning_ink(l, blank().bg),
            _ => l,
        }
    };
    let first = switch.switch.start;
    let last = switch.switch.end - 1;
    let gap_last = switch.cols.start - 1;
    let tail = switch.cols.end;
    if at(gap_last).fg != tint_at(palette, 0.0) {
        return Err(format!(
            "the gap's last cell is not t = 0: {:?}",
            at(gap_last).fg
        ));
    }
    if at(first).fg != letter(0.0) {
        return Err("the switch's first glyph is not t = 0".into());
    }
    if at(last).fg != letter(1.0) {
        return Err("the switch's last glyph is not t = 1".into());
    }
    if at(tail).fg != tint_at(palette, 1.0) {
        return Err("the tail is not t = 1".into());
    }
    Ok(())
}

#[test]
fn the_rule_flows_into_the_switch() {
    let run = plain_run(68);
    let cruise = composed(
        run,
        Some(LIT),
        Some(CardPalette::Cruise),
        KeyFace::default(),
    );
    let land = composed(run, Some(LIT), Some(CardPalette::Land), KeyFace::default());
    let off = composed(run, Some(LIT), Some(CardPalette::Off), KeyFace::default());
    let turning_off = composed(
        run,
        Some(TURNING_OFF),
        Some(CardPalette::Off),
        KeyFace::default(),
    );
    for (top, palette, face) in [
        (&cruise, CardPalette::Cruise, HarnessSwitch::On),
        (&land, CardPalette::Land, HarnessSwitch::On),
        (&off, CardPalette::Off, HarnessSwitch::On),
        (&turning_off, CardPalette::Off, HarnessSwitch::TurningOff),
    ] {
        let switch = top.switch.clone().expect("the switch");
        flows(&top.cells, &switch, top.col, palette, face)
            .unwrap_or_else(|e| panic!("{palette:?}: {e}"));
        assert_eq!(top.col, run.col, "{palette:?}: the whole span");
        for c in 0..2 {
            assert_eq!(
                top.cells[c].fg,
                tint_at(palette, 1.0),
                "{palette:?}: the lead"
            );
        }
        let gap = top.lockup.end..switch.cols.start;
        let g = gap.len() as f32 - 1.0;
        for (j, c) in gap.clone().enumerate() {
            assert_eq!(
                top.cells[c - top.col].fg,
                tint_at(palette, 1.0 - j as f32 / g),
                "{palette:?}: gap cell {j}"
            );
        }
        let bold = matches!(face, HarnessSwitch::On);
        for c in switch.switch.clone() {
            let cell = top.cells[c - top.col];
            if cell.ch != ' ' {
                assert_eq!(cell.bold, bold, "{palette:?}: the switch's weight");
            }
        }
    }
    let layout = |t: &ComposedTop| t.cells.iter().map(|c| c.ch).collect::<String>();
    assert_eq!(layout(&cruise), layout(&land), "one layout");
    assert_eq!(
        layout(&cruise),
        layout(&off),
        "Autopilot Off's switch is the lit one, `alab harness ● on`"
    );
    let lockup = cruise.lockup.clone();
    for (k, (a, b)) in cruise.cells.iter().zip(&land.cells).enumerate() {
        if lockup.contains(&(cruise.col + k)) || a.ch == ' ' {
            continue;
        }
        assert_ne!(a.fg, b.fg, "cell {k} {:?}: Cruise and Land apart", a.ch);
    }
    let switch = off.switch.clone().expect("Off's lit switch");
    let glyphs: Vec<usize> = switch
        .switch
        .clone()
        .filter(|&c| off.cells[c - off.col].ch != ' ')
        .collect();
    let n = glyphs.len() - 1;
    for (k, &c) in glyphs.iter().enumerate() {
        let stop = u8::try_from(k * 255 / n).expect("a stop");
        let want = brand_letter_fg(CardPalette::Off, f32::from(stop) / 255.0, blank());
        assert_eq!(
            off.cells[c - off.col].fg,
            want,
            "Off glyph {k}: the moonbow"
        );
    }
    let mut old = cruise.cells.clone();
    let switch = cruise.switch.clone().expect("the switch");
    let gap = cruise.lockup.end..switch.cols.start;
    let g = gap.len() as f32 - 1.0;
    for (j, c) in gap.enumerate() {
        old[c - cruise.col].fg = tint_at(CardPalette::Cruise, j as f32 / g);
    }
    assert!(
        flows(
            &old,
            &switch,
            cruise.col,
            CardPalette::Cruise,
            HarnessSwitch::On
        )
        .is_err(),
        "CONTROL: the left-to-right walk leaves a seam"
    );
}

#[test]
fn the_keys_marks_take_the_rule_cell_before_it() {
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
    let tag_run = TopRun { len: 60, ..run };
    let tag = (
        tag_run.col + tag_run.len + 1,
        tag_run.col + tag_run.len + 1 + 9,
    );
    let selected = KeyFace {
        ghost: None,
        tag: Some(tag),
        mark: Some('\u{25b8}'),
    };
    let unhosted = composed(tag_run, None, None, selected);
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
    assert_eq!(unhosted.ghost, None);
    let bare = composed(tag_run, Some(MASTER_OFF), None, selected);
    assert!(bare.switch.is_some(), "the off switch");
    assert_eq!(bare.col, bare.lockup.start, "bare: from the lockup");
    assert_eq!(bare.col + bare.cells.len(), tag.0 - 1, "to the mark");
    assert_eq!(bare.cells.last().map(|c| c.ch), Some('\u{25b8}'));
    let worn = composed(tag_run, Some(LIT), Some(CardPalette::Land), selected);
    assert_eq!(worn.cells.len(), tag_run.len, "the run alone");
    assert_eq!(worn.cells[tag_run.len - 1].ch, '\u{25b8}');
    assert!(worn.col + worn.cells.len() < tag.0, "never the tag's cells");
    let rest = composed(
        TopRun { len: 5, ..tag_run },
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
        let inside = (run.col..run.col + run.len).contains(&c);
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
                compose_top(&key, &mut top, None, None);
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
                        top.col + top.cells.len() < run.col + run.len,
                        "{what} at {cols}: the `\u{2500}` after the ghost is Claude's"
                    );
                }
                checked += 1;
                if muse && muse_seen.is_none() {
                    let plain = TopRun { voice: None, ..run };
                    let mut kept = ComposedTop::default();
                    compose_top(&TopKey { run: plain, ..key }, &mut kept, None, None);
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
                for hover in [false, true] {
                    let paint = SwitchPaint {
                        face,
                        hover,
                        version: None,
                    };
                    let f = switch_face(paint, 0).expect("the widest form");
                    let mut seen: Vec<SwitchInk> = Vec::new();
                    for &(ch, ink) in f.cells() {
                        if ch == ' ' || seen.contains(&ink) {
                            continue;
                        }
                        seen.push(ink);
                        let c = switch_cell(ch, ink, paint, None, blank, accent);
                        let what = format!(
                            "{accent_name}'s accent on {ground_name}, {face:?}{}, {:?}'s {ch:?}",
                            if hover { " hovered" } else { "" },
                            f.text()
                        );
                        if ink == SwitchInk::Cap {
                            let k = reads(c.fg, blank.bg);
                            assert!(k >= 3.0, "{what}: the cap at {k:.2}:1");
                        } else {
                            let k = reads(c.fg, c.bg);
                            assert!(k >= floor, "{what}: the letter at {k:.2}:1");
                            if ink == SwitchInk::Button {
                                let k = reads(c.bg, blank.bg);
                                assert!(k >= 3.0, "{what}: the pill at {k:.2}:1");
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 17 * 17 * 8 * 2, "{checked}");
    let solarized = builtin("Solarized Light").expect("a built-in");
    let paper = rgb(solarized.background);
    let quiet = mix(rgb(solarized.foreground), paper, 0.3);
    assert!(
        reads(quiet, paper) < 3.0,
        "CONTROL: the quiet label at {:.2}:1 on Solarized Light",
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
    let (mut checked, mut lows) = (0usize, Vec::new());
    for (name, bg, fg) in &grounds {
        let blank = RenderCell {
            ch: ' ',
            fg: *fg,
            bg: *bg,
            ..RenderCell::default()
        };
        let mut least = (f32::MAX, String::new());
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for paint in [LIT, OFF, MASTER_OFF, TURNING, TURNING_OFF] {
                for stop in (0..=255u8).step_by(3) {
                    let c = switch_cell(
                        'a',
                        SwitchInk::Arc(stop),
                        paint,
                        Some(palette),
                        blank,
                        accent,
                    );
                    let k = reads(c.fg, blank.bg);
                    if k < least.0 {
                        least = (
                            k,
                            format!("{palette:?} {:?} at stop {stop}: {:?}", paint.face, c.fg),
                        );
                    }
                    checked += 1;
                }
            }
        }
        if least.0 < floor {
            lows.push(format!("{name} {bg:?}: {} at {:.2}:1", least.1, least.0));
        }
    }
    assert!(checked >= 70 * 3 * 4 * 86, "{checked}");
    assert!(
        lows.is_empty(),
        "{} grounds under {floor}:1: {lows:#?}",
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
