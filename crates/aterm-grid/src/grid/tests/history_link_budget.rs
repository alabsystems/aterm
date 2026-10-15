// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The OSC 8 payload uncompressed history holds is counted and held to its
//! byte budget (`grid::ring_extras`), in the ring and in the lazy buffer's
//! staged rows: past it the oldest rows lose their links and keep their text,
//! and a normal link workload keeps every link.
//!
//! "Held" is measured by walking history, not by the grid's own sums: the
//! distinct URL `Arc`s history rows still reach, each counted once. Once a row
//! has scrolled off the live map, those reads are the only owners left.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use aterm_scrollback::Scrollback;

use crate::Grid;
use crate::grid::ScrolledRowExtras;

/// Columns per row: every cell of a linked row carries its own link.
const COLS: u16 = 80;
/// Screen height: a row reaches history `ROWS` line feeds after it is written.
const ROWS: u16 = 24;
/// The engine's URL ceiling (`MAX_HYPERLINK_URL_BYTES`).
const URL_BYTES: usize = 8 * 1024;
/// The flood: rows of `COLS` distinct `URL_BYTES` links (~500 MiB of links).
const FLOOD_ROWS: usize = 800;

/// A `len`-byte URL unique to (`row`, `col`).
fn url(row: usize, col: u16, len: usize) -> Arc<str> {
    let mut s = String::with_capacity(len);
    let _ = write!(s, "https://e.example/{row}/{col}/");
    s.extend(std::iter::repeat_n('x', len.saturating_sub(s.len())));
    Arc::from(s)
}

/// The text of history row `row` (every row is a full `COLS` wide).
fn row_text(row: usize) -> String {
    let mut s = format!("R{row:04} ");
    s.extend(std::iter::repeat_n('.', usize::from(COLS) - s.len()));
    s
}

/// Write `row_text(row)` on the cursor row, link cells `cols` to the URLs
/// `link(col)` hands out, then end the line.
fn write_row(
    grid: &mut Grid,
    row: usize,
    cols: std::ops::Range<u16>,
    mut link: impl FnMut(u16) -> Arc<str>,
) {
    grid.carriage_return();
    for c in row_text(row).chars() {
        grid.write_char(c);
    }
    let at = grid.cursor().row;
    for col in cols {
        grid.cell_extra_mut(at, col).set_hyperlink(Some(link(col)));
    }
    grid.carriage_return();
    grid.line_feed();
}

/// `FLOOD_ROWS` rows of `COLS` distinct links each, then a screen of plain
/// rows so every linked row is in history and the live map holds no link.
fn flood(grid: &mut Grid, linked: bool) {
    for row in 0..FLOOD_ROWS {
        if linked {
            write_row(grid, row, 0..COLS, |col| url(row, col, URL_BYTES));
        } else {
            write_row(grid, row, 0..0, |_| unreachable!());
        }
    }
    for row in FLOOD_ROWS..FLOOD_ROWS + usize::from(ROWS) {
        write_row(grid, row, 0..0, |_| unreachable!());
    }
}

/// What history still holds of its links: the distinct URL `Arc`s its rows
/// reach (bytes, each counted once) and each row's span count, oldest first.
fn held_links(grid: &Grid) -> (usize, Vec<usize>) {
    let mut seen = HashSet::new();
    let mut held = 0;
    let mut spans_per_row = Vec::new();
    for i in 0..grid.scrollback_lines() {
        let line = grid.get_history_line(i).expect("every history row reads");
        let spans = line.hyperlinks().unwrap_or(&[]);
        spans_per_row.push(spans.len());
        for span in spans {
            if seen.insert(Arc::as_ptr(&span.url).cast::<u8>() as usize) {
                held += span.url.len();
            }
        }
    }
    (held, spans_per_row)
}

/// The flood's linked rows in history: an unlinked oldest prefix, then rows
/// that kept every one of their `COLS` links, and every row kept its text.
/// Returns how many rows kept their links.
fn assert_stripped_oldest_first(grid: &Grid, spans_per_row: &[usize]) -> usize {
    let flood = &spans_per_row[..FLOOD_ROWS];
    let stripped = flood.iter().take_while(|&&n| n == 0).count();
    assert!(stripped > 0, "a ~500 MiB link flood must cost some links");
    assert!(
        flood[stripped..].iter().all(|&n| n == usize::from(COLS)),
        "links go oldest row first, whole rows: {flood:?}"
    );
    for (i, row) in (0..FLOOD_ROWS).enumerate() {
        let line = grid.get_history_line(i).expect("history row");
        assert_eq!(
            line.to_string(),
            row_text(row),
            "row {row} keeps its text when its links go"
        );
    }
    FLOOD_ROWS - stripped
}

/// The out-of-line bytes one kept flood row costs: its link payload plus the
/// rest of its boxed extras (the struct; the flood rows carry nothing else).
fn kept_row_floor() -> usize {
    usize::from(COLS) * URL_BYTES + std::mem::size_of::<ScrolledRowExtras>()
}

/// `memory_used` over the plain twin's, without the live extras map's share:
/// the map keeps scrolled-off entries until it compacts, under its own entry
/// cap (`MAX_HYPERLINK_ENTRIES`), and would otherwise hide what history adds.
fn history_bytes_over(linked: &Grid, plain: &Grid) -> usize {
    let history = |grid: &Grid| grid.memory_used() - grid.storage.extras.memory_used();
    history(linked) - history(plain)
}

/// A program printing a distinct 8 KiB link in every cell (the ring holds all
/// of it): `history_link_bytes` and the links history really holds stay within
/// the budget, the oldest rows lose their links whole and keep their text, and
/// `memory_used` counts what the kept rows hold. Then a width reflow (the
/// ring's bulk take + re-insert) keeps the bound and the sums.
#[test]
fn ring_link_flood_is_held_to_the_budget_oldest_first() {
    // Ring-only, deep enough that no flood row leaves the ring: the budget is
    // the default store's quarter.
    let ring = FLOOD_ROWS + usize::from(ROWS) + 16;
    let mut linked = Grid::with_scrollback(ROWS, COLS, ring);
    let mut plain = Grid::with_scrollback(ROWS, COLS, ring);
    flood(&mut linked, true);
    flood(&mut plain, false);
    linked.assert_invariants();
    let budget = linked.history_link_budget();
    assert_eq!(budget, aterm_scrollback::DEFAULT_MEMORY_BUDGET / 4);
    let accounted = history_bytes_over(&linked, &plain);

    let (held, spans_per_row) = held_links(&linked);
    assert!(held <= budget, "history holds {held} link bytes > {budget}");
    assert!(
        linked.history_link_bytes() <= budget,
        "counted link bytes {} over the {budget}-byte budget",
        linked.history_link_bytes()
    );
    let kept = assert_stripped_oldest_first(&linked, &spans_per_row);
    assert!(
        held > budget / 2,
        "strips to the low-water mark, not to nothing: {held} of {budget}"
    );
    // Counted: the kept rows' links are in `memory_used` (a ring row used to
    // charge its struct only). Bounded: nothing else of the flood is.
    assert!(
        accounted >= kept * kept_row_floor(),
        "memory_used must count the {kept} kept rows' links ({accounted} bytes over plain)"
    );
    assert!(
        accounted <= budget + kept * std::mem::size_of::<ScrolledRowExtras>(),
        "history over plain is the budget and the kept rows' structs at most: {accounted}"
    );

    linked.resize(ROWS, COLS + 20);
    linked.assert_invariants();
    assert!(linked.history_link_bytes() <= linked.history_link_budget());
    let (held_after, _) = held_links(&linked);
    assert!(held_after <= budget, "a reflow re-homes, never re-grows");
}

/// The same flood with the compression worker owning the drain (the GUI's
/// mode): rows pass the ring into the lazy buffer's staged rows, which hold
/// them uncompressed. Staged links count against the same budget, oldest
/// first, in `memory_used` and in the staged-byte sum the flood path weighs.
#[test]
fn staged_link_flood_under_offload_is_held_to_the_budget() {
    // A ring shallower than the rows the budget keeps linked, so the kept
    // rows reach into the staged ones.
    let ring = 8;
    let grid = |offload: bool| {
        let mut grid = Grid::with_tiered_scrollback(ROWS, COLS, ring, Scrollback::with_defaults());
        grid.set_compress_offload_active(offload);
        grid
    };
    let mut linked = grid(true);
    let mut plain = grid(true);
    flood(&mut linked, true);
    flood(&mut plain, false);
    assert!(
        linked.lazy_backlog_len() >= FLOOD_ROWS - ring,
        "the worker owns the drain: the flood is staged ({} rows)",
        linked.lazy_backlog_len()
    );
    linked.assert_invariants();
    let budget = linked.history_link_budget();
    assert_eq!(budget, aterm_scrollback::DEFAULT_MEMORY_BUDGET / 4);
    // Read before the walk below materializes (and caches) staged lines.
    let accounted = history_bytes_over(&linked, &plain);
    let staged_links = linked.storage.lazy_buffer.link_bytes();
    let staged_over_plain =
        linked.storage.lazy_buffer.staged_bytes() - plain.storage.lazy_buffer.staged_bytes();

    let (held, spans_per_row) = held_links(&linked);
    assert!(held <= budget, "history holds {held} link bytes > {budget}");
    assert!(linked.history_link_bytes() <= budget);
    let kept = assert_stripped_oldest_first(&linked, &spans_per_row);
    assert!(held > budget / 2, "{held} of {budget}");
    assert!(
        kept > ring,
        "the kept rows reach into the staged rows ({kept} kept, ring {ring})"
    );
    assert!(
        staged_links > 0 && staged_over_plain >= staged_links,
        "the flood path's staged-byte sum weighs the staged links \
         ({staged_over_plain} over plain, {staged_links} of links)"
    );
    assert!(
        accounted >= kept * kept_row_floor(),
        "memory_used must count the {kept} kept rows' links ({accounted} over plain)"
    );
    assert!(
        accounted <= budget + kept * std::mem::size_of::<ScrolledRowExtras>(),
        "history over plain is the budget and the kept rows' structs at most: {accounted}"
    );
}

/// The control: a GUI-shaped tab (default store, the 80-column ring) keeps
/// every link of an ordinary workload, a few hundred links to one URL, each
/// its own OSC 8 (its own `Arc`), then one 8 KiB link spanning a few hundred
/// rows (one `Arc` the per-span charge counts once per row).
#[test]
fn ordinary_link_workloads_keep_every_link() {
    const EACH: usize = 300;
    let mut grid = Grid::with_tiered_scrollback(ROWS, COLS, 6_123, Scrollback::with_defaults());
    for row in 0..EACH {
        let own: Arc<str> = Arc::from("https://docs.example/guide#configuring-history");
        write_row(&mut grid, row, 0..30, |_| Arc::clone(&own));
    }
    let run = url(0, 0, URL_BYTES);
    for row in EACH..2 * EACH {
        write_row(&mut grid, row, 0..COLS, |_| Arc::clone(&run));
    }
    for row in 2 * EACH..2 * EACH + usize::from(ROWS) {
        write_row(&mut grid, row, 0..0, |_| unreachable!());
    }
    grid.assert_invariants();
    assert!(grid.history_link_bytes() > EACH * URL_BYTES);
    for row in 0..2 * EACH {
        let line = grid.get_history_line(row).expect("history row");
        let spans = line.hyperlinks().unwrap_or(&[]);
        let expected = if row < EACH {
            "https://docs.example/"
        } else {
            "https://e.example/"
        };
        assert!(
            spans.len() == 1 && spans[0].url.starts_with(expected),
            "row {row} lost its link: {spans:?}"
        );
    }
}

/// How many rows the viewport shows with a link on them.
fn linked_on_screen(grid: &Grid) -> usize {
    (0..ROWS)
        .filter(|&r| {
            let view = grid.visible_row_view(r);
            (0..COLS).any(|col| {
                view.cell(col).is_some_and(|cell| {
                    view.cell_data(col, cell)
                        .cell_extra()
                        .is_some_and(|extra| extra.hyperlink().is_some())
                })
            })
        })
        .count()
}

/// A shed (a lowered budget) while the viewport is scrolled back strips the
/// links of rows on screen: the content generation moves, so the viewport
/// memo drops the rows it built with links, and the screen is redrawn.
#[test]
fn a_shed_under_a_scrolled_back_viewport_redraws_the_rows_it_unlinks() {
    const LINKED: usize = 40;
    let mut grid = Grid::with_tiered_scrollback(ROWS, COLS, 200, Scrollback::with_defaults());
    for row in 0..LINKED {
        write_row(&mut grid, row, 0..COLS, |col| url(row, col, 1024));
    }
    for row in LINKED..LINKED + usize::from(ROWS) {
        write_row(&mut grid, row, 0..0, |_| unreachable!());
    }
    grid.scroll_to_top();
    assert_eq!(
        linked_on_screen(&grid),
        usize::from(ROWS),
        "the oldest history rows, every one linked, fill the viewport"
    );

    let generation = grid.content_gen();
    grid.clear_damage();
    let budget = grid.scrollback_memory_budget().expect("tiered");
    grid.set_scrollback_memory_budget(64 * 1024)
        .expect("lower the budget");
    assert!(grid.history_link_bytes() <= 16 * 1024);
    assert!(
        grid.content_gen() > generation,
        "history rows lost their links: the content changed"
    );
    assert!(
        grid.damage().is_full(),
        "the rows on screen lost their links"
    );
    assert_eq!(
        linked_on_screen(&grid),
        0,
        "the viewport reads them unlinked"
    );
    grid.set_scrollback_memory_budget(budget)
        .expect("restore the budget");
    assert_eq!(linked_on_screen(&grid), 0, "a raise brings nothing back");
    grid.assert_invariants();
}

/// The window's pressure shed runs with the viewport at the bottom. Rows the
/// viewport built while scrolled back, linked, must not be served again once
/// the shed unlinked them and the reader scrolls back: the strip moves the
/// content generation even when nothing on screen changed. (Without it, a
/// debug build panics: "viewport row memo served a stale row".)
#[test]
fn a_shed_at_the_bottom_unlinks_the_rows_a_later_scroll_back_reads() {
    let mut grid = Grid::with_tiered_scrollback(ROWS, COLS, 200, Scrollback::with_defaults());
    for row in 0..60 {
        write_row(&mut grid, row, 0..COLS, |col| url(row, col, 1024));
    }
    for row in 60..60 + usize::from(ROWS) {
        write_row(&mut grid, row, 0..0, |_| unreachable!());
    }
    grid.scroll_to_top();
    assert_eq!(
        linked_on_screen(&grid),
        usize::from(ROWS),
        "fixture: linked"
    );
    grid.scroll_to_bottom();
    let budget = grid.scrollback_memory_budget().expect("tiered");
    grid.set_scrollback_memory_budget(64 * 1024)
        .expect("lower the budget");
    grid.set_scrollback_memory_budget(budget)
        .expect("restore the budget");
    grid.scroll_to_top();
    assert_eq!(linked_on_screen(&grid), 0, "the rows read unlinked");
    grid.assert_invariants();
}

/// The window's width change hands the store and ring history to a reflow
/// worker; rows written meanwhile refill the ring and are stripped again,
/// and the re-attach puts the job's history back IN FRONT of them (the strip
/// cursor already past 0). The bound and the running sums hold through it.
#[test]
fn an_offloaded_width_change_keeps_the_link_bound_through_its_reattach() {
    let mut grid = Grid::with_tiered_scrollback(ROWS, COLS, 300, Scrollback::with_defaults());
    let mut row = 0;
    for _ in 0..400 {
        write_row(&mut grid, row, 0..COLS, |col| url(row, col, 1024));
        row += 1;
    }
    let pending = grid
        .resize_offloading_scrollback(ROWS, COLS - 10)
        .expect("a tiered width change detaches");
    for _ in 0..250 {
        write_row(&mut grid, row, 0..COLS - 10, |col| url(row, col, 2048));
        row += 1;
    }
    grid.assert_invariants();
    grid.reattach_reflowed_scrollback(pending.reflow());
    grid.assert_invariants();
    assert!(grid.history_link_bytes() <= grid.history_link_budget());
    for _ in 0..400 {
        write_row(&mut grid, row, 0..COLS - 10, |col| url(row, col, 1024));
        row += 1;
    }
    grid.assert_invariants();
    assert!(grid.history_link_bytes() <= grid.history_link_budget());
}
