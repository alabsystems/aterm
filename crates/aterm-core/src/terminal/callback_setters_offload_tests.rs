// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::Terminal;
use aterm_scrollback::Scrollback;

/// Build a terminal whose bulk history lives in the tiered store.
fn term_with_history(rows: u16, cols: u16, lines: usize) -> Terminal {
    let sb = Scrollback::new(64, 512, 8_000_000);
    let mut t = Terminal::with_scrollback(rows, cols, 8, sb);
    let fill = "x".repeat((cols as usize).saturating_sub(8));
    let mut buf = Vec::new();
    for i in 0..lines {
        buf.extend_from_slice(format!("L{i}-{fill}\r\n").as_bytes());
    }
    t.process(&buf);
    t
}

fn visible_rows(t: &Terminal) -> Vec<String> {
    (0..t.rows())
        .map(|r| {
            t.get_line_text(i32::from(r), None)
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn retained_rows(t: &Terminal) -> Vec<String> {
    let grid = t.grid();
    let mut rows: Vec<String> = (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    rows.extend(visible_rows(t));
    rows
}

/// A width-change offload on the primary screen equals a synchronous resize across detach, reflow, and re-attach.
#[test]
fn offloaded_resize_primary_preserves_scrollback() {
    let mut t = term_with_history(24, 80, 500);
    let mut sync = term_with_history(24, 80, 500);
    let before = t.grid.scrollback_lines();
    assert!(before > 100, "precondition: deep history ({before} lines)");

    sync.resize(24, 40);

    let pending = t
        .resize_offloading_scrollback(24, 40)
        .expect("a width change with tiered history yields an offload job");
    assert!(
        pending.line_count() > 100,
        "the detached job carries the deep history (lazy + tiered)"
    );
    assert_eq!(t.grid.cols(), 40, "visible grid resized synchronously");

    let reflowed = pending.reflow();
    assert!(
        t.finish_resize_offload(reflowed).is_none(),
        "widths agree — no convergence pass expected"
    );

    assert_eq!(
        t.grid.scrollback_lines(),
        sync.grid.scrollback_lines(),
        "offloaded primary history line count matches synchronous resize"
    );
    assert_eq!(
        retained_rows(&t),
        retained_rows(&sync),
        "offloaded primary history and visible rows match synchronous resize"
    );
    assert_eq!(
        (t.grid.cursor_row(), t.grid.cursor_col()),
        (sync.grid.cursor_row(), sync.grid.cursor_col()),
        "offloaded primary cursor matches synchronous resize"
    );
}

/// An alt-screen resize offloads and matches a synchronous resize on both screens.
#[test]
fn offloaded_resize_alt_screen_preserves_saved_primary() {
    let mut t = term_with_history(24, 80, 500);
    let mut sync = term_with_history(24, 80, 500);
    assert!(t.grid.scrollback_lines() > 100);
    t.process(b"\x1b[?1049halt-row"); // enter alt screen (primary saved off)
    sync.process(b"\x1b[?1049halt-row");

    sync.resize(24, 40);

    let pending = t
        .resize_offloading_scrollback(24, 40)
        .expect("alt-screen width change offloads the saved primary's history");
    let reflowed = pending.reflow();
    assert!(
        t.finish_resize_offload(reflowed).is_none(),
        "widths agree — no convergence pass expected"
    );
    assert_eq!(visible_rows(&t), visible_rows(&sync));
    assert_eq!(
        (t.grid.cursor_row(), t.grid.cursor_col()),
        (sync.grid.cursor_row(), sync.grid.cursor_col())
    );

    t.process(b"\x1b[?1049l"); // exit alt → saved primary is active again
    sync.process(b"\x1b[?1049l");
    assert_eq!(
        t.grid.scrollback_lines(),
        sync.grid.scrollback_lines(),
        "saved-primary history line count matches synchronous resize"
    );
    assert_eq!(
        retained_rows(&t),
        retained_rows(&sync),
        "saved-primary history and visible rows match synchronous resize"
    );
    assert_eq!(
        (t.grid.cursor_row(), t.grid.cursor_col()),
        (sync.grid.cursor_row(), sync.grid.cursor_col()),
        "restored primary cursor matches synchronous resize"
    );
}

/// `trickle_lazy_bounded` skips during a flood on the saved primary's backlog and promotes once idle.
#[test]
fn trickle_skips_a_flood_that_outruns_it_on_the_backlog_the_worker_drains() {
    let sb = Scrollback::new(64, 512, 1_200_000);
    let mut t = Terminal::with_scrollback(24, 80, 8, sb);
    t.set_compress_offload_active(true);
    let mut buf = Vec::new();
    for i in 0..22_000 {
        buf.extend_from_slice(format!("L{i}\r\n").as_bytes());
    }
    t.process(&buf);
    let backlog = t.lazy_backlog_len();
    assert!(
        t.scrollback_truncated_lines() > 0,
        "precondition: a flood past the cap is cutting"
    );

    t.process(b"\x1b[?1049h");
    assert_eq!(
        t.trickle_lazy_bounded(256),
        backlog,
        "22k lines arrived since the last opportunity: skipped, on the saved \
         primary's backlog"
    );
    assert_eq!(
        t.trickle_lazy_bounded(256),
        backlog - 256,
        "nothing arrived since: a batch is promoted"
    );
}
