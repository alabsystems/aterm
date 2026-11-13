// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::relieve_flood_pressure;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;

fn flooded(n: usize) -> Terminal {
    let mut t = Terminal::with_scrollback(24, 253, 8, Scrollback::new(64, 512, 8_000_000));
    t.set_compress_offload_active(true);
    let mut buf = Vec::with_capacity(n * 256);
    for i in 0..n {
        let row = format!("L{i} ");
        buf.extend_from_slice(row.as_bytes());
        buf.resize(buf.len() + 253 - row.len(), b'p');
        buf.extend_from_slice(b"\r\n");
    }
    t.process(&buf);
    t
}

#[test]
fn relief_promotes_the_backlog_back_under_the_mark_and_loses_nothing() {
    let term = Mutex::new(flooded(15_000));
    let before = {
        let t = term.lock().unwrap();
        assert!(t.main_grid().flood_promotion_due() > 0, "precondition");
        t.lazy_backlog_len()
    };
    relieve_flood_pressure(&term, &AtomicU32::new(0));
    let t = term.lock().unwrap();
    assert_eq!(t.main_grid().flood_promotion_due(), 0, "under the mark");
    assert!(t.lazy_backlog_len() < before, "the backlog was promoted");
    assert_eq!(t.scrollback_truncated_lines(), 0, "nothing lost");
}

#[test]
fn a_partial_pressure_batch_keeps_the_half_floor() {
    let term = Mutex::new(flooded(10_100));
    {
        let t = term.lock().unwrap();
        let due = t.main_grid().flood_promotion_due();
        assert!(due > 0 && due < super::COMPRESS_BUDGET, "a partial batch");
        assert_eq!(t.lazy_backlog_len() - due, 10_000, "the half floor");
    }
    relieve_flood_pressure(&term, &AtomicU32::new(0));
    let t = term.lock().unwrap();
    assert_eq!(t.lazy_backlog_len(), 10_000, "no batch overshoot");
    assert_eq!(t.scrollback_truncated_lines(), 0);
}

#[test]
fn a_history_clear_and_refill_rechecks_the_pressure_floor() {
    let mut t = flooded(15_000);
    let owed = t.main_grid().flood_promotion_due();
    assert!(owed > super::COMPRESS_BUDGET, "several batches are owed");
    let before = t.lazy_backlog_len();
    assert_eq!(
        t.drain_lazy_pressure_bounded(super::COMPRESS_BUDGET),
        before - super::COMPRESS_BUDGET
    );
    let owed = owed - super::COMPRESS_BUDGET;

    t.process(b"\x1b[3J");
    assert_eq!(t.lazy_backlog_len(), 0, "ED3 erased the staged history");
    t.process(&b"after clear\r\n".repeat(128));
    let refilled = t.lazy_backlog_len();
    assert!(refilled > 0 && refilled < 10_000, "a new small backlog");
    assert_eq!(t.main_grid().flood_promotion_due(), 0);
    assert_eq!(
        t.drain_lazy_pressure_bounded(owed.min(super::COMPRESS_BUDGET)),
        refilled,
        "the old owed count cannot drain the new backlog below the floor"
    );
    assert_eq!(t.scrollback_truncated_lines(), 0);
}

#[test]
fn relief_works_the_saved_primary_under_an_alt_screen() {
    let mut t = flooded(15_000);
    t.process(b"\x1b[?1049h");
    let before = t.lazy_backlog_len();
    assert!(t.main_grid().flood_promotion_due() > 0, "precondition");
    let term = Mutex::new(t);
    relieve_flood_pressure(&term, &AtomicU32::new(0));
    let t = term.lock().unwrap();
    assert_eq!(t.main_grid().flood_promotion_due(), 0);
    assert!(t.lazy_backlog_len() < before);
}

#[test]
fn under_the_mark_relief_takes_nothing() {
    let term = Mutex::new(flooded(5_000));
    let before = term.lock().unwrap().lazy_backlog_len();
    assert!(before > 0, "precondition: a staged backlog");
    relieve_flood_pressure(&term, &AtomicU32::new(0));
    assert_eq!(term.lock().unwrap().lazy_backlog_len(), before);
}

#[test]
fn a_log_flood_the_line_limit_holds_is_left_to_the_flood_gate() {
    const BUDGET: usize = 100_000_000;
    let mut t = Terminal::with_scrollback(24, 80, 8, Scrollback::new(64, 512, BUDGET));
    t.set_compress_offload_active(true);
    let mut buf = Vec::with_capacity(100_000 * 82);
    for i in 0..100_000 {
        let row = format!("L{i} ");
        buf.extend_from_slice(row.as_bytes());
        buf.resize(buf.len() + 80 - row.len(), b'p');
        buf.extend_from_slice(b"\r\n");
    }
    t.process(&buf);
    let before = t.lazy_backlog_len();
    assert!(
        before * 80 * 8 > BUDGET / 2,
        "precondition: {before} staged rows are past half the headroom"
    );
    let term = Mutex::new(t);
    relieve_flood_pressure(&term, &AtomicU32::new(0));
    let t = term.lock().unwrap();
    assert_eq!(t.lazy_backlog_len(), before, "relief took nothing");
    assert_eq!(t.scrollback_truncated_lines(), 0, "and nothing was cut");
}
