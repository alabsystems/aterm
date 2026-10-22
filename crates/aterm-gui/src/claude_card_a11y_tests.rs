// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A screen reader hears Claude's words under the harness's glass (review
//! G3, owner decision 2026-10-20): the accessibility tree's grid text is read
//! from the cells Claude wrote (`claude_footer::ProgramCells`), as copy and
//! `text`/`screen` keep them, never the glass's `YOLO on`, `✦ alab.systems`,
//! facts or the card's line.

use super::*;

use aterm_a11y::model::Role;

use crate::accessibility::AccessibleSnapshot;

/// Claude Code's bypass pill: the row the glass relabels `YOLO on`.
const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";

/// Claude Code's lockup as the composer's top rule wears it under the
/// harness's glass (`claude_footer::compose_top`): two `─`, the icon's
/// blank cells under its sprite, the name and the next `─` — aterm's
/// drawing, not the card's own mascot (`▐▛███▛█`), which the reader hears.
const LOGO: &str = "\u{2500}\u{2500}    claude \u{2500}";

/// The session's foreground program is Claude Code's: what the pill
/// relabel's gate asks of the pane, with the composer border.
fn claude_program(app: &App, session: u64) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some("claude".into()));
}

/// Rows of text as a snapshot reads them: each glyph (a NUL or a control
/// as a blank), trailing blanks trimmed, each row ended by a newline.
fn as_read(rows: &[String]) -> String {
    rows.iter()
        .map(|row| {
            let row: String = row
                .chars()
                .map(|c| if c == '\0' || c.is_control() { ' ' } else { c })
                .collect();
            format!("{}\n", row.trim_end_matches(' '))
        })
        .collect()
}

/// THE READER HEARS CLAUDE'S ROWS. On the MEASURED update-notice capture at
/// 120 columns ([`WIDE`]) with Claude's bypass pill on its mode row, under a
/// supervising host: the glass shows `✦ alab.systems` over the notice, `YOLO
/// on` over the pill, the card's line, the rule's facts and the top rule's
/// logo — and the grid's snapshot ([`App::grid_snapshot`]) and the tree's
/// terminal document read exactly the terminal's own rows: Claude's notice
/// and its changelog link, `⏵⏵ bypass permissions on`, the rules' own
/// glyphs — allocating no more than the same words read off the engine's
/// cells; the logo, decoration, has no node. NEGATIVE CONTROL: the
/// presented scratch, read as the snapshot read it before, holds the glass's
/// words and the logo.
#[test]
fn a_reader_hears_claudes_words_under_the_glass() {
    let c = WIDE;
    let mut screen = c.screen();
    let mode = screen.len() - 1;
    screen[mode] = BYPASS.to_owned();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, u16_of(c.rows), u16_of(c.cols)));
    publish(&app, session, CAPTURED);
    feed_capture(&app, session, &c, &screen);
    claude_program(&app, session);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(capture_banner(&app, wid, &c), "the banner is on the glass");
    let strip = strip_rows(&app, wid);
    let presented = AccessibleSnapshot::from_cells(
        &app.windows[&wid].input_scratch.cells[strip..],
        c.cols,
        None,
    );
    let presented = presented.value();
    assert!(
        presented.contains(NOTICE.text)
            && presented.contains("  YOLO on")
            && presented.contains(LOGO),
        "NEGATIVE CONTROL: the presented frame holds the glass's words and the rule's lockup: {presented}"
    );
    let claude = as_read(&engine_rows(&app, session));
    let heard = app.grid_snapshot(wid).expect("the grid's snapshot");
    assert_eq!(heard.value(), claude, "the reader hears Claude's rows");
    assert!(
        heard.value().contains(BYPASS)
            && heard
                .value()
                .contains(aterm_phase::anchor_text("notice.updated_details"))
            && !heard.value().contains(NOTICE.text)
            && !heard.value().contains("YOLO")
            && !heard.value().contains(LOGO),
        "{}",
        heard.value()
    );
    // Nothing allocated beyond the snapshot's own text: as many allocations
    // as the same words read off the engine's own cells.
    let engine = {
        let mut fresh = RenderInput::empty();
        let term = app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&term).cell_frame_into(&mut fresh, c.rows, c.cols);
        fresh.cells
    };
    let (plain, bare) = crate::work_counts::allocations_in(|| {
        AccessibleSnapshot::from_cells(&engine, c.cols, None)
    });
    assert_eq!(plain.value(), claude, "CONTROL: the same words");
    let (_, under) = crate::work_counts::allocations_in(|| app.grid_snapshot(wid));
    assert_eq!(under, bare, "the glass's undo record is read, not copied");
    let tree = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    let document = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Terminal)
        .and_then(|(_, n)| n.value())
        .expect("the terminal document");
    assert_eq!(document, claude, "the tree's document");
    // The lockup is decoration: no node of the tree names or holds it.
    let logo_nodes: Vec<_> = tree
        .nodes
        .iter()
        .filter(|(_, n)| {
            n.label().is_some_and(|l| l.contains(LOGO))
                || n.value().is_some_and(|v| v.contains(LOGO))
        })
        .map(|(_, n)| {
            (
                n.role(),
                n.label().map(str::to_owned),
                n.value().map(str::to_owned),
            )
        })
        .collect();
    assert!(
        logo_nodes.is_empty(),
        "no node for the lockup: {logo_nodes:?}"
    );
    host.shutdown_and_join();
}

/// THE SAME IN A SPLIT: Claude Code's pane on the right of a 160-column
/// window ([`split_card`]), its pill relabelled `YOLO on` and its card's
/// line on the glass, is published as its own terminal document that reads
/// the pane's own rows — `⏵⏵ bypass permissions on`, no `YOLO`, no line —
/// and the shell's pane beside it reads its own. NEGATIVE CONTROL: the
/// presented scratch through the pane's rectangle reads `YOLO on`.
#[test]
fn a_split_panes_reader_hears_its_own_claudes_words() {
    let (mut app, wid, shell, session, cols) = split_card("", "");
    claude_program(&app, session);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    let plan = app.active_visible_leaf_plan(wid).expect("the plan");
    let leaf = &plan.leaves[1];
    let (row_off, col_off) = (
        leaf.rect.origin.y.round() as usize,
        leaf.rect.origin.x.round() as usize,
    );
    let rows = leaf.rect.size.height.round() as usize;
    assert_eq!(leaf.rect.size.width.round() as usize, cols, "Claude's pane");
    let scratch = &app.windows[&wid].input_scratch;
    let presented = AccessibleSnapshot::from_cells_in(
        &scratch.cells[..],
        strip + row_off..strip + row_off + rows,
        col_off..col_off + cols,
        None,
    );
    assert!(
        presented.value().contains("  YOLO on"),
        "NEGATIVE CONTROL: the glass relabelled the pill: {}",
        presented.value()
    );
    let tree = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    let documents: Vec<&str> = tree
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::Terminal)
        .filter_map(|(_, n)| n.value())
        .collect();
    let claude = as_read(&engine_rows(&app, session));
    let shell = as_read(&engine_rows(&app, shell));
    assert!(claude.contains(BYPASS), "{claude}");
    assert_eq!(
        documents,
        [shell.as_str(), claude.as_str()],
        "each pane's document reads its own program's rows"
    );
    host.shutdown_and_join();
}
