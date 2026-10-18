// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance (Tier-1): the real `Terminal::cell_frame` stamp obeys the
//! derived snapshot-seq protocol (`aterm_models_session::read_image_seq_model()`):
//! the engine stamp (`engine_fill_seq`) is never ahead of the live
//! `damage_epoch`, so a held frame's staleness stays detectable, while a
//! host-bumped `snapshot_seq` read in its place is the model's `Buggy = 1`
//! run-ahead capture. (`damage_epoch` itself advancing on writes and not on
//! no-ops is pinned in `terminal/state.rs`.)

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_models_session::read_image_seq_model;

/// Project one held snapshot stamp against the live epoch.
fn seq_state(epoch: u64, snap_seq: u64) -> BTreeMap<&'static str, i64> {
    [
        ("epoch", i64::try_from(epoch).expect("epoch fits i64")),
        ("snapped", 1),
        ("snap_seq", i64::try_from(snap_seq).expect("seq fits i64")),
        ("torn", 0),
    ]
    .into_iter()
    .collect()
}

/// The model's `snap_seq` is the engine stamp (`engine_fill_seq`); its `Buggy = 1`
/// run-ahead capture is a host-bumped `snapshot_seq` (the ghost-paint bump a host mutator
/// performs after writing cells) read in its place. The engine itself only lags.
#[test]
fn host_bumped_seq_is_the_run_ahead_stamp_the_engine_stamp_never_is() {
    let m = read_image_seq_model();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello");
    let mut snap = term.cell_frame(rows, cols);
    let live = term.damage_epoch();
    assert_eq!(snap.snapshot_seq, live, "the engine stamps the live epoch");
    assert_eq!(snap.engine_fill_seq, snap.snapshot_seq);

    // A host mutator writes cells and bumps the stamp, as the discipline requires.
    snap.snapshot_seq = snap.snapshot_seq.wrapping_add(1);
    assert!(
        !m.check_invariant("SeqIsStaleOrCurrent", &seq_state(live, snap.snapshot_seq)),
        "the host-bumped seq, read as the engine's stamp, IS the Buggy=1 capture"
    );
    assert!(m.check_invariant(
        "SeqIsStaleOrCurrent",
        &seq_state(live, snap.engine_fill_seq)
    ));

    // One real engine advance lands the live epoch on the bumped seq.
    term.take_damage();
    term.process(b"\r\nnewer output");
    let live_after = term.damage_epoch();
    assert_eq!(live_after, snap.snapshot_seq, "the two clocks collide");
    assert!(
        live_after <= snap.snapshot_seq,
        "read as the engine's stamp, the bumped seq passes a stale frame as current"
    );
    assert!(
        live_after > snap.engine_fill_seq,
        "the engine stamp still sees the frame is behind"
    );
    assert!(m.check_invariant(
        "SeqIsStaleOrCurrent",
        &seq_state(live_after, snap.engine_fill_seq)
    ));
}
