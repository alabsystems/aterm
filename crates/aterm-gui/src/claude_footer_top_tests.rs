// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The composer top rule's lockup, harness switch and tint, read once per
//! rebuild of the pane's rows and composed once per change of what it is
//! made of ([`read_top_run`], [`compose_top`], [`PaneFooter::compose_top`]):
//! the narrow ladder's order ([`ladder`]), the switch where it stood before
//! the lockup, the key's mark on a `─` on every rung, the rule flowing into
//! the switch on one stop, the cells inside the run at every width, state,
//! palette and program — and a frame at rest reads, composes and allocates
//! nothing for it.

use super::probe::*;
use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;

/// The lit switch.
const LIT: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::On,
    hover: false,
};

/// The session's own Off: `○ off ▐ on ▌`.
const OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Off(SwitchOff::Session),
    hover: false,
};

/// The master switch off: the same face, over a bare rule.
const MASTER_OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Off(SwitchOff::Master),
    hover: false,
};

/// A switch turning on.
const TURNING: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Turning,
    hover: false,
};

/// The headless window's cell, 1x: 7 × 14 device pixels.
const CELL: (u16, u16) = (7, 14);

/// The programs whose top line wears a mark, every one the ladder serves.
const MARKED: [aterm_phase::Program; 5] = [
    aterm_phase::Program::Claude,
    aterm_phase::Program::Agy,
    aterm_phase::Program::Muse,
    aterm_phase::Program::Codex,
    aterm_phase::Program::Gemini,
];

/// Every lockup a top line wears: each marked program's vendor build, and
/// codex-alab's, whose name (`codex-alab`, 15 cells laid) is the longest.
const LOCKUPS: [(aterm_phase::Program, Build); 6] = [
    (aterm_phase::Program::Claude, Build::Vendor),
    (aterm_phase::Program::Agy, Build::Vendor),
    (aterm_phase::Program::Muse, Build::Vendor),
    (aterm_phase::Program::Codex, Build::Vendor),
    (aterm_phase::Program::Codex, Build::Alab),
    (aterm_phase::Program::Gemini, Build::Vendor),
];

/// THE SCAN THE CACHE REPLACED, its run alone (main's
/// `collect_composer_border_brand_edits` of 2026-10-20): the rightmost plain
/// run of `─` in the rule's own style — the style most of its `─` wear, its
/// last one's on a tie — over a row wholly on the ground; none where a cell
/// is off it; for Muse Code (`muse`), also the whole rule from its first
/// `─` to its last where its idle voice label stands at its left, no cell of
/// it a cluster, a combining mark or an image (main's of 2026-10-21), and
/// in a pane under 34 columns, where the label's own `──` is the longest
/// run and at 32 columns the label ends the row, the whole rule to the
/// label's closing blank (the lab of 2026-10-22). What [`read_top_run`] must
/// read, and the span the switch's placement oracle lays hb-int's switch
/// in.
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

/// The rows of a Claude Code screen `cols` wide, 24 rows, its composer's top
/// rule on row 20 written as `top` (escapes and all), the caret row, the
/// bottom rule and the mode row under it — every row cut to the width, so
/// none wraps and scrolls the rule off its row.
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

/// The top rules a test reads at `cols`: plain; with Claude's tag (` KEY `
/// in its violet at the right, one `─` after it); in two styles, the left
/// half's and the right's; with a cell off the ground (a highlight); with a
/// combining mark mid-run; with Muse Code's idle voice label at the left
/// (measured `4b4f55` rule, `8a9098` label), which Muse Code's lockup takes
/// — down to 32 columns, where the label alone fills the row — and under
/// 32 columns cut with `…` as Muse Code 1.4.11 cuts it (measured at 26, 28
/// and 30 columns: the row's first `cols − 2` cells of the label, then
/// `…`), which no lockup takes.
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
    // The label and its closing blank are 32 cells.
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

/// The switch and the theme a top rule is composed under, as the splice
/// pairs them: no host; Cruise and Land held by this window's host; Off in
/// white, plain and hovered; the master switch off and a claim this host
/// does not hold, bare; a switch turning on.
const CASES: [(Option<SwitchPaint>, Option<CardPalette>); 8] = [
    (None, None),
    (Some(LIT), Some(CardPalette::Cruise)),
    (Some(LIT), Some(CardPalette::Land)),
    (Some(OFF), Some(CardPalette::Off)),
    (
        Some(SwitchPaint { hover: true, ..OFF }),
        Some(CardPalette::Off),
    ),
    (Some(MASTER_OFF), None),
    (Some(LIT), None),
    (Some(TURNING), None),
];

/// A top key for `program`'s vendor build on `run` at frame row 30.
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

/// THE SWITCH SITS WHERE THE SCAN PUT IT: on a real engine's frame (the
/// capture route's extraction and strip, before the footer's splice), at
/// every width from where Claude draws its tag (26 columns) to a wide pane,
/// for each top rule [`top_rules`] draws, each program that wears a mark and
/// each switch and theme the splice pairs ([`CASES`]), [`read_top_run`]
/// reads the scan's run ([`scan_run`]), every cell [`compose_top`] writes
/// lies in the run it is laid on ([`laid_run`]), a switch stands exactly
/// where hb-int laid one ([`fitted_switch_face`] of that run),
/// right-aligned two cells short of its end in the form the ladder chose,
/// its text that form's, and the lockup stands two cells in: the pads, the
/// icon's two blanks, the CLI's word. On Muse Code's idle voice label the
/// lockup takes the label (owner, 2026-10-21): it stands at the rule's left
/// and every cell of the label is written over, while any other program's
/// line, and Muse's where no lockup is laid, leaves the label as drawn —
/// down to the 32 columns the label alone fills, where the label's own
/// `──` is the rule's longest run (the lab of 2026-10-22: under 34 columns
/// nothing was laid on Muse's line), and never the cut label under that.
///
/// NEGATIVE CONTROL: the comparison sees a difference — the plain rule's
/// composition at a run one cell shorter is not the run's own.
#[test]
fn the_switch_sits_where_the_scan_put_it_with_no_ghost() {
    use aterm_phase::Program;
    // The idle voice label's cells, `── Voice input (⌥ + v to start) `.
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
            // `false` where the window is that size already.
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
                    compose_top(&key, &mut top, None);
                    let (laid, _) = laid_run(&key);
                    if what.starts_with("voice label") {
                        let mut after = before.clone();
                        for (k, cell) in top.cells.iter().enumerate() {
                            after[top.col + k] = *cell;
                        }
                        // Under 32 columns the cut label's row ends at its `…`.
                        let label = LABEL.min(before.len());
                        let left: String = after[..label].iter().map(|c| c.ch).collect();
                        let was: String = before[..label].iter().map(|c| c.ch).collect();
                        if muse && !top.lockup.is_empty() {
                            assert_eq!(what, "voice label", "{at}: never the cut label");
                            if cols < 40 {
                                narrow.insert(cols);
                            }
                            // The lockup took the label: at the rule's left,
                            // none of the label's words left on the glass.
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
            // NEGATIVE CONTROL: one cell shorter is not the run's own.
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
                compose_top(&key(run.len), &mut whole, None);
                compose_top(&key(run.len - 1), &mut short, None);
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

/// The pane's frame row `row` as the glass shows it, char by char.
fn glass_chars(app: &App, wid: WindowId, row: usize) -> String {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip + row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// What one pass cost the top rule: allocations in the whole pass, its
/// reads and its compositions.
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

/// This window's host holding `session`: a harness host, and its claim on
/// the session, so its top rule wears the harness's theme and its lit
/// switch. The guard shuts the host down.
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

/// The probe's Claude Code pane read by its agent reader and held by this
/// window's host, two frames drawn.
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

/// THE TOP RULE AT REST COSTS NOTHING, the harness's theme on it: a
/// Claude Code pane this window's host holds whose screen and facts did not
/// move — the lockup at its left, the spectrum tinting its top rule, the lit
/// switch at its right — reads the rule off no frame, composes nothing and
/// allocates nothing in the footer's pass, frame after frame, the glass the
/// same; a keystroke's echo on the caret row keeps the run read and the
/// composition.
///
/// NEGATIVE CONTROLS: the pane narrowed reads the rule once and composes
/// it once, its switch laid anew — then rests at nothing; and a fresh
/// window's first frame reads and composes (the counters are wired to the
/// work).
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
    // CONTROL: the pane narrowed.
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
    // CONTROL: a first frame reads and composes.
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

/// The mark layers of `session`'s top rule on the window's frame.
fn marks_on(app: &App, wid: WindowId, session: u64) -> Vec<aterm_core::render::FxLayer> {
    app.windows[&wid]
        .input_scratch
        .fx_layers
        .iter()
        .filter(|l| l.id == top_mark_id(session))
        .cloned()
        .collect()
}

/// A SETTLED MARK BUILDS NOTHING (design §6.5): over frames whose key holds,
/// the footer's pass composes no top rule, seals no fx content and copies
/// none (`fx_layer_builds`, `fx_payload_copies`, this thread's), and every
/// frame's mark layer is the first frame's very content (`Arc::ptr_eq`) at
/// the same place: the renderer's one pointer compare.
///
/// NEGATIVE CONTROL: a cell-size change (the font doubled) recomposes the
/// top rule once and seals exactly one content, then rests again.
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
    // CONTROL: the font doubled, so the cell.
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

/// A plain top rule's run of `len` cells at frame column 2, Claude's gray.
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

/// The pane's blank in the pure tests: light ink on the default ground.
fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [220, 220, 220],
        ..RenderCell::default()
    }
}

/// [`compose_top`] of `run` for `program`'s vendor build, under the switch
/// `switch` and the theme `palette`, the key's `face`.
fn composed_for(
    program: aterm_phase::Program,
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
) -> ComposedTop {
    composed_as((program, Build::Vendor), run, switch, palette, face)
}

/// [`composed_for`] for a lockup's program and build ([`LOCKUPS`]).
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
    );
    top
}

/// [`composed_for`] Claude.
fn composed(
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    face: KeyFace,
) -> ComposedTop {
    composed_for(aterm_phase::Program::Claude, run, switch, palette, face)
}

/// The ghost's face: drawn, not lit.
const GHOST: KeyFace = KeyFace {
    ghost: Some(false),
    tag: None,
    mark: None,
};

/// The chars of a composition laid over a run of `len` `─` from the run's
/// first cell: what the line reads with the cells written.
fn line_of(top: &ComposedTop, run: TopRun) -> Vec<char> {
    let mut line = vec!['\u{2500}'; run.len];
    for (k, c) in top.cells.iter().enumerate() {
        if let Some(slot) = line.get_mut(top.col + k - run.col) {
            *slot = c.ch;
        }
    }
    line
}

/// THE SWITCH YIELDS ITS CELLS TO THE GHOST (design §10.6.14): the ghost
/// takes Claude's slot at the run's right end — ` KEY off ` then the one `─`
/// left as Claude's — and the ladder lays the lockup and the switch in the
/// span short of it, the switch's right edge two `─` left of the ghost, in
/// the form the ladder chose for that span; no lockup or switch cell is
/// under the ghost, and the ghost's ink is Claude's rule gray, not bold, at
/// full size, never the palette's. Bare (the harness's glass not worn), the
/// lockup, the switch and the ghost are written over the vendor's rule; with
/// no host, the lockup and the ghost. A run of 19 cells holds the ghost and
/// its lead; 18 does not.
///
/// NEGATIVE CONTROL: with no ghost the same runs lay the switch at the run's
/// own end, as before the key.
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
            // No room for the switch: the ghost alone.
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
        // CONTROL: no ghost, the switch at the run's own end.
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
        // Bare: the lockup and the off switch over Claude's rule, then the
        // ghost.
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
        // No host: the lockup's cells and the ghost's.
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
    // The run's floor.
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

/// THE LOCKUP (owner, 2026-10-21: "each CLI has an emoji-sized icon and
/// color on the left side of the top line of CLI to standardize"): on every
/// rung that lays it, for every lockup — each program that wears a mark,
/// and codex-alab, whose name is the longest — and in every harness state —
/// lit, Off's white, bare, turning on and with no host — the lockup stands
/// its lead in from the run's left (two `─`, one on the compact rung): a
/// pad, the icon's two cells and a pad, all blanks on the pane's ground with
/// no underline (the sprite draws over the icon's), and on a full rung the
/// build's name (`codex-alab` for ALab's Codex, the CLI's word for every
/// other) bold in its brand ink floored to 4.5:1 (`cli_marks::name_fg`) and
/// a trailing pad, with at least its lead of `─` after it before the switch
/// (or the span's end). Its cells and its icon's first are recorded
/// (`ComposedTop::lockup`, `icon`).
///
/// NEGATIVE CONTROLS: a span one cell short of the full rung's need lays no
/// name; and a ladder sized for vendor `codex`'s name, laid with
/// codex-alab's, would leave the lit switch under two `─` after the lockup
/// — at the 26-column pane among others — which the gap check catches.
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
            (Some(OFF), Some(CardPalette::Off)),
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
                    // CONTROL: a cell short of the last full rung's need, no
                    // name — not even the CLI's word codex-alab's begins with.
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
    // CONTROL: codex-alab's name laid on the rungs of a ladder sized for
    // vendor `codex`'s.
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

/// Whether a lockup ending at `end` leaves at least `lead` cells of `─`
/// before `limit` (the switch's first cell, or the span's end): the gap the
/// ladder's rungs keep (two, one on the compact rung).
fn gap_holds(end: usize, limit: usize, lead: usize) -> bool {
    limit >= end + lead
}

/// The rung's name as the spec's tables print it (§5.2, §5.5).
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

/// THE ORDER (§5.3), walked down spans 200 to 0 over `rungs` for a switch
/// whose last form is `last`: the tier never widens; the name goes only once
/// the switch is in its last form, the icon only once the name is gone, the
/// switch only once the icon is; and nothing given up comes back. `Err`
/// names the first span that breaks it.
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

/// One row of §5.5's table: the CLI and its build, its switch's state
/// (`none`: no switch), and the rung it shows at each span.
type Shown = (
    (CliMark, Build),
    &'static str,
    &'static [(usize, &'static str)],
);

/// What §5.5 says each CLI shows, by rung and span (the parenthesised span
/// of each cell of its table).
const SHOWS: &[Shown] = &[
    // Claude, its KEY tag drawn: the span is W − 12.
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
    // Claude, the ghost: the span is W − 16.
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
    // Claude with no right label, and Gemini and Codex: the span is W.
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
        (CliMark::Gemini, Build::Vendor),
        "turning",
        &[
            (120, "full s0"),
            (80, "full s0"),
            (50, "full s0"),
            (33, "full s2"),
            (26, "full s3"),
        ],
    ),
    // codex-alab (a Codex with a switch is codex-alab: no host admits a
    // vendor `codex`): its name lays 15 cells, five more than §5.5's `codex`,
    // so at 26 and 33 columns its name gives way a rung sooner.
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
    // Vendor `codex`: no switch, its own word.
    (
        (CliMark::Codex, Build::Vendor),
        "none",
        &[(120, "full"), (26, "full"), (14, "full"), (13, "icon")],
    ),
    // agy: the span is W.
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
    // Muse: the span is W − 32, after a voice label with news (at rest the
    // lockup takes the idle label, and the span is the whole rule).
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
    // No switch (no host, a vendor Codex, Grok).
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
];

/// The switch a §5.5 state word names.
fn paint_of(state: &str) -> Option<SwitchPaint> {
    match state {
        "on" => Some(LIT),
        "off" => Some(OFF),
        "turning" => Some(TURNING),
        _ => None,
    }
}

/// THE LADDER GIVES UP WORDS, THEN THE NAME, THEN THE ICON (§5.3): for every
/// mark and build — codex-alab's longer name among them — every switch face
/// and none, the rung chosen walking down every span from 200 to 0 keeps the
/// order ([`walk_order`]), and the rungs §5.5's table names are the ones laid
/// at its spans (Claude with its KEY tag, the ghost or neither; agy; Muse
/// after its label; Gemini; codex-alab, its name giving way a rung sooner
/// than §5.5's `codex`; vendor `codex` and no switch).
///
/// NEGATIVE CONTROLS: a ladder with the icon's rung before the name's last
/// tier breaks the order, and at agy off, span 26, shows `icon s3`, not
/// `full s3`; and codex-alab's line laid on the ladder of vendor `codex`'s
/// name shows `full s2` at 26 columns, its name where it no longer fits.
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
    // CONTROL: codex-alab on the vendor's ladder.
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
    // CONTROL: the icon's rung moved before the last full one.
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

/// hb-int's switch faces, before the lockup, for a program named `word`:
/// the forms its `fitted_switch_face` chose from, widest first.
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
    }
}

/// A SWITCH STANDS WHEREVER IT STOOD (§5.6, invariants 2 and 3): for every
/// span from 0 to 200, every lockup with a switch — Claude's, agy's, Muse's,
/// Gemini's and codex-alab's, the longest name — and every switch face, a
/// switch is laid exactly where hb-int's `fitted_switch_face` laid one (its
/// widest form of the program's word that left two rule cells either side),
/// and it stands at `span − 2 − len`, as [`compose_top`] lays it.
///
/// NEGATIVE CONTROL: a ladder with no `alone` rung loses the switch at spans
/// 10 to 13 on the on face and 12 to 15 on the off face.
#[test]
fn a_switch_stands_wherever_it_stood() {
    for (program, build) in [
        (aterm_phase::Program::Claude, Build::Vendor),
        (aterm_phase::Program::Agy, Build::Vendor),
        (aterm_phase::Program::Muse, Build::Vendor),
        (aterm_phase::Program::Gemini, Build::Vendor),
        (aterm_phase::Program::Codex, Build::Alab),
    ] {
        let mark = cli_mark(program).expect("a mark");
        for paint in [LIT, OFF, MASTER_OFF, TURNING] {
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
    // CONTROL: no `alone` rung.
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

/// Whether the span's last cell of `line` (a composition laid over its run,
/// [`line_of`]) is the rule's `─` — the key's mark takes it — and, where a
/// switch is laid, the one before it too, which keeps the mark off the
/// switch's trailing blank.
fn mark_cell_free(line: &[char], span: usize, switch: bool) -> bool {
    span >= 2 && line[span - 1] == '\u{2500}' && (!switch || line[span - 2] == '\u{2500}')
}

/// THE MARK CELL SURVIVES EVERY RUNG (§5.6, invariant 5): on every rung of
/// every lockup's ladder (codex-alab's included), in every switch state and
/// none, at every span
/// from 6 to 200, the span's last cell is the rule's `─`, and where a
/// switch is laid the one before it too — so the key's mark (`◐`, `▸`: the
/// span's last cell, `mark_at`) lands on a `─`, never on the lockup nor
/// against the switch — and with the mark drawn it is there, in the accent.
///
/// NEGATIVE CONTROL: the compact rung laid with a tail of one (its switch a
/// cell right) puts the mark against the switch's trailing blank, and the
/// check catches it.
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
                // The mark drawn left of Claude's tag, just past the run.
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
    // CONTROL: tail one on the compact rung.
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

/// The tint's ink at `t` on the gray rule over [`blank`]: hb-int's formula.
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

/// The seam check: the gap's last `─` and the switch's first glyph share
/// `t = 0`, the switch's last glyph and the tail share `t = 1` — in the
/// tint's and the letters' inks of `palette` for `face`.
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
            HarnessSwitch::Turning => turning_ink(l, blank().bg),
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

/// THE RULE FLOWS INTO THE SWITCH (§4.3, §1.1): at Claude's 80 columns with
/// its KEY tag (a span of 68), the `─` before the lockup stand at `t = 1`,
/// the gap walks from `t = 1` at the lockup down to `t = 0`, so its last `─`
/// and the switch's first glyph share `t = 0`, and the switch's last glyph
/// and its tail share `t = 1` ([`flows`]); Cruise's and Land's inks differ in
/// every tinted and every glyph cell at the same layout; Off's are the
/// moonbow (`CardPalette::Off`), regular weight, Cruise's bold.
///
/// NEGATIVE CONTROL: hb-int's left-to-right walk over the same gap puts
/// `t = 1` against the switch's `t = 0`, and the seam check fails.
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
    let off = composed(run, Some(OFF), Some(CardPalette::Off), KeyFace::default());
    for (top, palette, face) in [
        (&cruise, CardPalette::Cruise, HarnessSwitch::On),
        (&land, CardPalette::Land, HarnessSwitch::On),
        (
            &off,
            CardPalette::Off,
            HarnessSwitch::Off(SwitchOff::Session),
        ),
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
        // The gap walks down, one cell a step.
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
    // Cruise and Land: one layout, two palettes, every inked cell apart.
    let layout = |t: &ComposedTop| t.cells.iter().map(|c| c.ch).collect::<String>();
    assert_eq!(layout(&cruise), layout(&land), "one layout");
    let lockup = cruise.lockup.clone();
    for (k, (a, b)) in cruise.cells.iter().zip(&land.cells).enumerate() {
        if lockup.contains(&(cruise.col + k)) || a.ch == ' ' {
            continue;
        }
        assert_ne!(a.fg, b.fg, "cell {k} {:?}: Cruise and Land apart", a.ch);
    }
    // Off: the moonbow's arc.
    let switch = off.switch.clone().expect("the off switch");
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
    // CONTROL: hb-int's walk, left to right over the same gap.
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

/// THE KEY'S MARKS take the `─` left of the ghost's leading blank or of
/// Claude's tag's — `◐` while a press runs, `▸` while selected — in the
/// accent's ink; lit, the ghost's glyphs lift to the value ink and are
/// underlined, its blanks not. Over Claude's tag nothing is written but the
/// mark and the harness's own: with no host, the lockup and the mark; bare,
/// the lockup, the switch and the mark after it; worn, the run's tint with
/// the mark in it, the tag's own cells never among them. NEGATIVE CONTROL:
/// a run too short for any rung, no mark and no host: no cell written over
/// Claude's tag.
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
    // Claude's tag: its key just past the run's end and the tag's leading
    // blank.
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
    // CONTROL: a run no rung fits, no mark and no host: nothing written.
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

/// THE MARK STANDS ONLY WHERE ITS WRITE TOOK (§6.6): the sprite over the
/// icon's cells goes on the glass after the footer's write, only where
/// `FooterUndo::took` confirms the top line's — so a frame whose write is
/// refused (a combining mark landed in the run on the frame the splice
/// paints again, its rows and its run kept: `write_edits` skips the span)
/// puts no layer on the frame and records none, though the composition
/// stands, and the next clean frame lays it again. A selection over the
/// line — the renderer's overlay, which no cell shows — makes no write:
/// the row is Claude's own, no layer stands, and neither the lockup nor the
/// switch is a target.
///
/// NEGATIVE CONTROLS: the write that took puts exactly one layer, in the
/// Over slot in grid space, at the icon's first cell `(col·cw, frame_row·ch)`,
/// recorded in the card's record; a selection on another row keeps the
/// line's write, its layer and its switch.
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
    // The refused write.
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
    // The next clean frame.
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    assert_eq!(marks_on(&app, wid, session).len(), 1, "laid again");
    // A selection over the line, and (CONTROL) over a transcript row.
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

/// THE MARK LEAVES WITH EVERY STRIP (§6.7): after the footer's splice the
/// mark is on the frame (so the law is not vacuous), recorded in the card's
/// own record, so the strips that take the card's light back take it too —
/// the top of the splice (spliced again, it stands once), the capture's
/// one-pane refill (which runs no footer undo: the refill alone takes it,
/// and the tab strip's prepend moves nothing stranded), and the top of a
/// frame on the live path (`redraw_window`, a scripted surface) — bare and
/// under a tab strip, capture after capture.
///
/// NEGATIVE CONTROL: the layer recorded where the footer's undo would hold
/// it (withdrawn only where the footer's undo runs: the top of a splice, by
/// identity) leaves one stranded copy per capture under the tab strip,
/// counted.
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
        // The live path: the top of a frame.
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
    // CONTROL: the record the footer's undo would hold.
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
        // The footer's undo at the top of the splice, by identity.
        app.windows
            .get_mut(&wid)
            .expect("window")
            .input_scratch
            .fx_layers
            .retain(|l| !record.contains(l));
        let _ = pass(&mut app, wid);
        let now = forget(&mut app);
        assert_eq!(now.len(), 1 + i, "CONTROL: capture {i} strands one more");
        // The splice's own, laid last: the one the next undo would hold.
        record = now.last().cloned().into_iter().collect();
    }
}

/// The composer's top rule of a measured screen (`aterm-phase`'s fixtures,
/// `aterm ctl text` of the real CLI): the last `─` line over the caret's
/// row (`❯`, or agy's `>`), comment lines (`# …`) aside.
fn measured_top(fixture: &str) -> String {
    let lines: Vec<&str> = fixture.lines().filter(|l| !l.starts_with("# ")).collect();
    let caret = lines
        .iter()
        .rposition(|l| (l.starts_with('\u{276F}') || l.starts_with('>')) && !l.contains("/effort"))
        .expect("the composer's caret row");
    lines[caret - 1].trim_end().to_owned()
}

/// Whether `top` written over `before` (a top rule's frame cells) touches
/// only `run`'s cells: every other cell byte-identical after. `Err` names the
/// first cell it changed outside the run.
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

/// MARKS WRITE ONLY THE RUN (§5.6, invariant 1): over the measured top lines
/// — Claude Code 2.1.304's with its KEY tag at 26, 34, 61, 80, 120 and 144
/// columns, the plain rule aterm's ghost stands on, agy 1.4.0's full-width
/// rule, Muse Code 1.4.10's after its `── Voice input (⌥ + v to start) `
/// label (and 1.4.11's label alone, and cut with `…`, in a pane under 32
/// columns) and Gemini CLI 0.46.0's full-width live-zone rule (`idle`,
/// `end-of-turn`, `narrow-end-of-turn`; Codex's slab is
/// `codex_gets_a_switch_only_as_codex_alab`'s) — each laid out again at every
/// width from 20 to 160 the CLI draws
/// it at (and checked to be the fixture's line at the fixture's width), for
/// every switch and theme the splice pairs ([`CASES`]): every cell
/// [`compose_top`] writes lies in the run it is laid on ([`laid_run`] of
/// the run [`read_top_run`] reads; the ghost's own cells aside), so
/// Claude's tag, the ghost and the `─` after it are byte-identical before
/// and after. Muse's idle label is the one cell range the lockup may take
/// (owner, 2026-10-21): where it does, the lockup stands at the rule's left
/// and none of the label's words are left; where it does not — the label
/// cut, which may be any voice state's — the label is byte-identical.
///
/// NEGATIVE CONTROL: Muse's composition on the plain run after its label,
/// laid one cell left (a lead of `run.col − 1`), writes the label's last
/// space, and is caught.
#[test]
fn marks_write_only_the_run() {
    use aterm_phase::Program;
    let label = "\u{2500}\u{2500} Voice input (\u{2325} + v to start) ";
    // The lines as each CLI draws them at `w` columns (plain text), and as
    // fed (its inks).
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
            // Under 32 columns, cut as Muse Code 1.4.11 cuts it: the row's
            // first `w − 2` cells of the label, then `…`.
            let cut: String = label.chars().take(w - 2).chain(['\u{2026}']).collect();
            return Some((cut.clone(), format!("\x1b[38;2;136;136;136m{cut}")));
        };
        let r = "\u{2500}".repeat(fill);
        Some((
            format!("{label}{r}"),
            format!("\x1b[38;2;136;136;136m{label}{r}"),
        ))
    };
    // The fixtures, each at its own width, are the lines as laid out.
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
    // Gemini's line is the live-zone rule its reader places, never its band.
    for (fixture, cols) in [
        (
            include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-idle.txt"),
            144,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-end-of-turn.txt"),
            144,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-narrow-end-of-turn.txt"),
            72,
        ),
    ] {
        let rows: Vec<String> = fixture
            .lines()
            .filter(|l| !l.starts_with("# "))
            .map(str::to_owned)
            .collect();
        let rule = aterm_phase::gemini::top_rule(&rows).expect("Gemini's live-zone rule");
        let (text, _) = plain(cols).expect("the fixture's width");
        assert_eq!(rows[rule], text, "Gemini's measured line at {cols}");
    }
    let kinds: [(&str, Line, Program, KeyFace); 5] = [
        (
            "claude, KEY tag",
            claude,
            Program::Claude,
            KeyFace::default(),
        ),
        ("claude, ghost", plain, Program::Claude, GHOST),
        ("agy", plain, Program::Agy, KeyFace::default()),
        ("muse", muse, Program::Muse, KeyFace::default()),
        ("gemini", plain, Program::Gemini, KeyFace::default()),
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
                // Claude's tag: its key's frame columns.
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
                compose_top(&key, &mut top, None);
                let (laid, label) = laid_run(&key);
                writes_only_the_run(&before, laid, &top).unwrap_or_else(|e| {
                    panic!("{what} at {cols}, {switch:?} {palette:?}: {e}: {drawn:?}")
                });
                if label > 0 {
                    // The lockup took Muse's label: at the rule's left, the
                    // label's every cell written over.
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
                // The control's composition: Muse's on the plain run after
                // its label, the label kept.
                if muse && muse_seen.is_none() {
                    let plain = TopRun { voice: None, ..run };
                    let mut kept = ComposedTop::default();
                    compose_top(&TopKey { run: plain, ..key }, &mut kept, None);
                    if !kept.cells.is_empty() {
                        muse_seen = Some((before.clone(), plain, kept));
                    }
                }
            }
        }
    }
    assert!(checked > 3000, "{checked}");
    assert!(taken > 0, "Muse's lockup took its label: {taken}");
    // CONTROL: Muse's composition on the plain run after its label, a cell
    // left.
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

/// THE BARE SWITCH READS ON EVERY GROUND IT IS LAID ON (design §8: every
/// letter aterm draws reads at 4.5:1 or better): the switch no theme wears
/// — the master switch off, a claim this host does not hold, a switch
/// turning on — on every built-in's ground, under the accent of every
/// built-in's band, since a tab's own theme draws its pane in one theme
/// while the accent is the instance's (`session_theme`: the window's chrome
/// follows the instance). In every face, hovered or not, every letter and
/// state glyph — `alab harness`, `● on`, `○ off`, `◐ turning on`, the `on`
/// button's letters on its fill — reads at 4.5:1 or more on what it is laid
/// on, and the button's caps and its fill at 3:1 or more on the ground
/// (WCAG 1.4.11: the pill is the control's shape).
///
/// NEGATIVE CONTROLS: the inks unfloored read under it — the quiet label
/// (`mix(fg, bg, 0.3)`) on Solarized Light under 3:1, and Dracula's accent,
/// the `on` pill's fill under an instance in Dracula, on a tab in Default
/// Light under 1.5:1.
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
                HarnessSwitch::Off(SwitchOff::Session),
                HarnessSwitch::Turning,
            ] {
                for hover in [false, true] {
                    let paint = SwitchPaint { face, hover };
                    // The widest form wears every ink its face has.
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
    // CONTROLS: the inks unfloored.
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
