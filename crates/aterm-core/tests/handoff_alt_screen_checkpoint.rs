// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGRESSION: an in-session update refused to apply — forever — on any desk
//! where a session was sitting on the alternate screen.
//!
//! `Terminal::alt_grid` is the INACTIVE grid, not "the alternate screen". While
//! the alternate screen is up it holds the SAVED PRIMARY, scrollback and all
//! (`terminal/buffer_api.rs`: "`self.alt_grid`, which holds the SAVED PRIMARY").
//!
//! The handoff wire gives the alt blob exactly `rows` records: `CheckpointMeta`
//! carries a single `history_lines`, and that field describes the MAIN blob, so
//! the alt blob has no way to declare a history count of its own. The consumer
//! therefore validates it with `history = 0`. Projecting the inactive grid with
//! `max_history` instead made the blob `min(primary_scrollback, max) + rows`
//! records; `deserialize_lines_strict` refuses a declared count above the
//! expected one outright, so `screen_digest` returned `None` and every apply
//! failed with "visible checkpoint set could not be committed canonically".
//!
//! These tests pin the wire shape at the producer, which is the only place that
//! can honour it.

use aterm_core::scrollback::{deserialize_lines_strict, serialize_lines};
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

const ROWS: u16 = 49;
const COLS: u16 = 131;
/// The producer's real carry target (`seamless::max_handoff_history_lines`).
const CARRY: usize = 256;

/// Exactly the consumer's canonicality check for a grid blob that must hold
/// `expected` records — `seamless::checkpoint_grid_is_canonical`, reproduced so
/// this test fails for the same reason the updater did.
fn is_canonical(bytes: &[u8], expected: usize, cols: u16) -> bool {
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    deserialize_lines_strict(bytes, expected, usize::from(cols), content_cap, record_cap)
        .is_some_and(|lines| lines.len() == expected && serialize_lines(&lines) == bytes)
}

/// A terminal whose PRIMARY screen has real scrollback behind it.
fn primary_with_history() -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    for i in 0..(usize::from(ROWS) + 400) {
        t.process(format!("primary line {i}\r\n").as_bytes());
    }
    t
}

fn carry(t: &Terminal) -> TerminalCheckpoint {
    t.checkpoint_carry(CARRY).expect("parser is Ground")
}

/// The precondition the regression depends on: the primary really does carry
/// history, so the alt-screen case below is not vacuous.
#[test]
fn the_primary_screen_carries_history() {
    let cp = carry(&primary_with_history());
    assert!(
        cp.history_lines > 0,
        "the primary must have scrollback for this suite to mean anything"
    );
}

/// THE REGRESSION. While the alternate screen is up, the saved primary — with
/// its scrollback — is what `alt_grid` holds, and the wire still allows it
/// exactly `rows` records.
#[test]
fn the_alt_blob_is_exactly_rows_records_while_the_alternate_screen_is_up() {
    let mut t = primary_with_history();
    // DECSET 1049 — the primary, scrollback and all, is parked in `alt_grid`.
    t.process(b"\x1b[?1049h");
    t.process(b"a full-screen app draws here");

    let cp = carry(&t);
    let alt = cp
        .alt_grid
        .as_ref()
        .expect("the saved primary is parked in alt_grid");

    assert!(
        is_canonical(alt, usize::from(cp.rows), cp.cols),
        "the alt blob must hold exactly `rows` records: the wire has one \
         `history_lines` and it describes the MAIN blob, so a carried alt \
         history is unrepresentable and the consumer refuses the whole capture"
    );
}

/// The other entry mode. 1047 reuses the PERSISTENT alternate buffer rather
/// than allocating a cleared one, so it reaches the same parked saved primary
/// by a different path.
#[test]
fn the_alt_blob_is_exactly_rows_records_under_mode_1047_too() {
    let mut t = primary_with_history();
    t.process(b"\x1b[?1047h");
    t.process(b"a full-screen app draws here");

    let cp = carry(&t);
    let alt = cp
        .alt_grid
        .as_ref()
        .expect("the saved primary is parked in alt_grid");

    assert!(
        is_canonical(alt, usize::from(cp.rows), cp.cols),
        "the alt blob must hold exactly `rows` records under mode 1047 as well"
    );
}

/// CONTROL: back on the primary screen, `alt_grid` holds the PERSISTENT
/// alternate buffer, whose ring cap is 0 by xterm spec. This passed before the
/// fix and must keep passing — the fix must not disturb the case that worked.
///
/// Exit via 1047, not 1049: 1049 DISCARDS the alternate buffer (`alt_grid`
/// becomes `None`), so it cannot exercise a parked alt blob at all.
#[test]
fn the_alt_blob_is_exactly_rows_records_after_leaving_the_alternate_screen() {
    let mut t = primary_with_history();
    t.process(b"\x1b[?1047h");
    t.process(b"a full-screen app draws here");
    t.process(b"\x1b[?1047l");

    let cp = carry(&t);
    let alt = cp
        .alt_grid
        .as_ref()
        .expect("the alternate buffer is persistent and survives the exit");

    assert!(
        is_canonical(alt, usize::from(cp.rows), cp.cols),
        "the parked alternate buffer must still be exactly `rows` records"
    );
}

/// The MAIN blob's own contract, which is the one `history_lines` describes:
/// `rows + history_lines` records. Pinned alongside so a fix to the alt side
/// cannot quietly break the side that is allowed to carry history.
#[test]
fn the_main_blob_is_rows_plus_its_declared_history() {
    let cp = carry(&primary_with_history());
    let expected = usize::from(cp.rows) + cp.history_lines as usize;

    assert!(
        is_canonical(&cp.grid, expected, cp.cols),
        "the main blob must hold exactly `rows + history_lines` records"
    );
    assert!(
        cp.history_lines as usize <= CARRY,
        "the carry bound is an upper bound on the declared history"
    );
}
