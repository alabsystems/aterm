// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::link_target::{LinkHover, caption_test_support::instructions};
use crate::tab_model::VisibleLeafPlan;

// Frozen main 8682f2a3f47: collect every other pane's cursor, then test both ends.
fn previous_row(
    app: &App,
    wid: WindowId,
    hover: LinkHover,
    plan: &VisibleLeafPlan,
) -> Option<(usize, bool)> {
    let mut carets = Vec::with_capacity(plan.leaves.len());
    for leaf in &plan.leaves {
        if leaf.focused {
            continue;
        }
        let Some(session) = app
            .view_store
            .get(leaf.view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
        else {
            continue;
        };
        let Some((term, waiting)) = app
            .pool
            .shown(session)
            .map(|(term, entry)| (term, entry.ctx.ui_waiting.clone()))
        else {
            continue;
        };
        let pane_rows = leaf.rect.size.height.round().max(1.0) as usize;
        let offset = leaf.rect.origin.y.round().max(0.0) as usize;
        if let Some(caret) =
            crate::term_lock_ui(&term, &waiting, crate::metrics::TermWaitSite::Redraw)
                .cursor_row_on_screen(pane_rows)
        {
            carets.push(offset + caret);
        }
    }
    let ws = app.windows.get(&wid)?;
    let rows = usize::from(ws.rows);
    let nrows = ws.input_scratch.cells.len();
    let bottom = rows.checked_sub(1)?;
    let strip = nrows.checked_sub(rows)?;
    if ws.cols == 0 || nrows == 0 {
        return None;
    }
    let caret = ws
        .input_scratch
        .cursor_visible
        .then_some(ws.input_scratch.cursor_row);
    let row = [bottom, 0].into_iter().find(|row| {
        usize::from(hover.window_row) != *row
            && caret != Some(strip + row)
            && !carets.contains(row)
            && !super::super::chrome_owns_any_on_terminal_row_of(ws, *row)
    })?;
    Some((strip + row, row == bottom))
}

fn fixture(splits: usize, hovered: bool) -> (App, VisibleLeafPlan) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for _ in 0..splits {
        app.split_active_stub_tab(wid);
    }
    feed(&app, wid, b"\x1b[5;1H");
    feed(&app, wid, PHISH);
    extract_only(&mut app, wid);
    app.splice_tab_strip(wid);
    if hovered {
        let (row, col, _, _) = app.focused_pane_rect(wid);
        hover(&mut app, wid, row + 4, col + LINK_COL);
        assert!(app.windows[&wid].link_hover.is_some());
    }
    let (plan, _) = app.active_visible_frame_layout(wid).expect("layout");
    assert_eq!(plan.leaves.len(), splits + 1);
    (app, plan)
}

#[test]
fn placement_matches_all_cursor_choices_and_frame_end_obstructions() {
    let wid = WindowId(0);
    for splits in 0..=2 {
        let (mut app, plan) = fixture(splits, true);
        let original = app.windows[&wid].link_hover.unwrap();
        let other: Vec<_> = plan.leaves.iter().filter(|leaf| !leaf.focused).collect();
        for pattern in 0..4_usize.pow(splits as u32) {
            for (index, leaf) in other.iter().enumerate() {
                let session = app
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .unwrap();
                let (term, _) = app.pool.shown(session).unwrap();
                let rows = leaf.rect.size.height.round().max(1.0) as usize;
                let position = (pattern / 4_usize.pow(index as u32)) % 4;
                let control = match position {
                    0 => "\x1b[?25l".to_owned(),
                    1 => "\x1b[?25h\x1b[1;1H".to_owned(),
                    2 => "\x1b[?25h\x1b[6;1H".to_owned(),
                    _ => format!("\x1b[?25h\x1b[{rows};1H"),
                };
                term_lock(&term).process(control.as_bytes());
            }
            for hover_row in [0, original.window_row, app.windows[&wid].rows - 1] {
                for obstruction in 0..4 {
                    app.windows.get_mut(&wid).unwrap().chrome_rows.clear();
                    app.windows.get_mut(&wid).unwrap().chrome_spans.clear();
                    for (mask, row) in [(1, 0), (2, usize::from(app.windows[&wid].rows - 1))] {
                        if obstruction & mask != 0 {
                            super::super::claim_chrome_rows(
                                app.windows.get_mut(&wid).unwrap(),
                                row..row + 1,
                            );
                        }
                    }
                    let hover = LinkHover {
                        window_row: hover_row,
                        ..original
                    };
                    assert_eq!(
                        app.link_caption_row(wid, hover, &plan),
                        previous_row(&app, wid, hover, &plan),
                        "splits={splits} pattern={pattern} hover={hover_row} obstruction={obstruction}"
                    );
                }
            }
        }
    }
}

fn full_caption(app: &mut App, plan: &VisibleLeafPlan, hovered: bool) {
    let wid = WindowId(0);
    extract_only(app, wid);
    app.splice_tab_strip(wid);
    let shown = app.compose_link_caption(wid, Some(plan));
    assert_eq!(
        shown.is_some(),
        hovered,
        "the fixture must compose its caption every time"
    );
    std::hint::black_box(shown);
}

#[test]
#[ignore = "manual whole capture/caption and placement allocation/instruction measurement"]
fn caption_composition_cost() {
    use std::hint::black_box;
    let wid = WindowId(0);
    for (name, splits, hovered) in [
        ("one", 0, true),
        ("two", 1, true),
        ("three", 2, true),
        ("no-hover", 0, false),
    ] {
        let (mut app, plan) = fixture(splits, hovered);
        for sample in 0..5 {
            if hovered {
                extract_only(&mut app, wid);
                app.splice_tab_strip(wid);
                let hover = app.windows[&wid].link_hover.unwrap();
                assert_eq!(
                    app.link_caption_row(wid, hover, &plan),
                    previous_row(&app, wid, hover, &plan)
                );
                for _ in 0..100 {
                    black_box(app.link_caption_row(wid, hover, &plan));
                }
                let (_, allocations) =
                    aterm_heap_count::allocations_in(|| app.link_caption_row(wid, hover, &plan));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(app.link_caption_row(wid, black_box(hover), black_box(&plan)));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "caption-compose case={name} scope=placement sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
            for _ in 0..100 {
                full_caption(&mut app, &plan, hovered);
            }
            let (_, allocations) =
                aterm_heap_count::allocations_in(|| full_caption(&mut app, &plan, hovered));
            let before = instructions();
            for _ in 0..100 {
                full_caption(black_box(&mut app), black_box(&plan), hovered);
            }
            let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
            println!(
                "caption-compose case={name} scope=capture-and-caption sample={sample} iterations=100 allocations={allocations} instructions={work:?}"
            );
        }
    }
}
