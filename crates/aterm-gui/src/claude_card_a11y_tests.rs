// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that accessibility grid text reads Claude's program cells rather than glass overlays.

use super::*;

use aterm_a11y::model::Role;

use crate::accessibility::AccessibleSnapshot;

const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";

const LOGO: &str = "\u{2500}\u{2500}    claude \u{2500}";

fn claude_program(app: &App, session: u64) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some("claude".into()));
}

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
                .contains(aterm_phase::anchor!("notice.updated_details"))
            && !heard.value().contains(NOTICE.short)
            && !heard.value().contains("YOLO")
            && !heard.value().contains(LOGO),
        "{}",
        heard.value()
    );
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
    let links: Vec<_> = tree
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::Link)
        .map(|(id, n)| {
            (
                crate::a11y_tree::chip_for(*id),
                n.label().map(str::to_owned),
                n.supports_action(aterm_a11y::model::Action::Click),
            )
        })
        .collect();
    assert_eq!(
        links,
        vec![(
            Some((session, crate::a11y_tree::HarnessChip::Link)),
            Some(NOTICE.text.to_owned()),
            true
        )],
        "the banner's link is a link a reader can press, named by its words"
    );
    let _ = crate::platform::take_opened_urls();
    app.press_claude_chip_for_reader(wid, session, crate::a11y_tree::HarnessChip::Link);
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![NOTICE.url.to_string()],
        "the reader's press opens it"
    );
    app.press_claude_chip_for_reader(wid, session + 1, crate::a11y_tree::HarnessChip::Link);
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "CONTROL: no banner, nothing opened"
    );
    host.shutdown_and_join();
}

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
