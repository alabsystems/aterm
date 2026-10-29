// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance test verifying that `Terminal::cell_frame` obeys `read_image_seq_model`.

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_models_session::read_image_seq_model;

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

/// `engine_fill_seq` never runs ahead of `damage_epoch`, whereas a host-bumped `snapshot_seq` triggers the model's `Buggy = 1` capture.
#[test]
fn host_bumped_seq_is_the_run_ahead_stamp_the_engine_stamp_never_is() {
    let m = read_image_seq_model();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello");
    let snap = term.cell_frame(rows, cols);
    let live = term.damage_epoch();
    assert_eq!(snap.snapshot_seq, live, "the engine stamps the live epoch");
    assert_eq!(snap.engine_fill_seq, snap.snapshot_seq);

    let bumped_seq = snap.snapshot_seq.wrapping_add(1);
    assert!(
        !m.check_invariant("SeqIsStaleOrCurrent", &seq_state(live, bumped_seq)),
        "the host-bumped seq, read as the engine's stamp, IS the Buggy=1 capture"
    );
    assert!(m.check_invariant(
        "SeqIsStaleOrCurrent",
        &seq_state(live, snap.engine_fill_seq)
    ));

    term.take_damage();
    term.process(b"\r\nnewer output");
    let live_after = term.damage_epoch();
    assert_eq!(
        live_after, bumped_seq,
        "read as the engine's stamp, the bumped seq collides with the newer epoch and passes a stale frame as current"
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
