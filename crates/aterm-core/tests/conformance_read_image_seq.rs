// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance (Tier-1): the real `Terminal::cell_frame` + `damage_epoch` / `take_damage`
//! path obeys the derived snapshot-seq protocol (`read_image_seq_model()`, proved and
//! refuted by `ty` in `aterm-spec/tests/derived_ring_ty.rs`): a snapshot is stamped with
//! `damage_epoch()` under the one lock, later `process()` grows the live epoch without
//! touching the held snapshot (no torn read), and a consumer can always tell its snapshot
//! is behind (`damage_epoch() > snap.snapshot_seq`).

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_spec::derive::read_image_seq_model;

/// Drive the real read_image snapshot-seq path and bind it to the derived model.
#[test]
fn real_cell_frame_seq_is_monotone_isolated_and_stale_detectable() {
    let m = read_image_seq_model();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);

    // Net-new damage, observed (the latch bumps once per damage session) and consumed so
    // the next write opens a fresh session, as the GUI does under the lock.
    term.process(b"hello");
    let _ = term.damage_epoch();
    term.take_damage();
    term.process(b" world");

    // read_image: `snapshot_seq` is the engine's `damage_epoch`, filled under the same
    // lock as the cells.
    let snap = term.cell_frame(rows, cols);
    let snap_seq = snap.snapshot_seq;
    let snap_cells = snap.cells.clone();
    // The live epoch equals the captured seq right after the snapshot: it is current.
    assert_eq!(
        term.damage_epoch(),
        snap_seq,
        "snapshot_seq must equal the live damage_epoch at snapshot time"
    );

    // A later write, net-new after consuming the prior damage, advances the epoch.
    term.take_damage();
    term.process(b"\r\nmore output on a new line");
    let live_after = term.damage_epoch();

    // 1. Monotone seq, staleness detectable: the live epoch passed the held snapshot.
    assert!(
        live_after > snap_seq,
        "later damage must advance the live epoch past the captured snapshot_seq \
         (monotone seq; staleness is observable as live > snap)"
    );

    // 2. No torn read: the emitted snapshot's seq stamp and cells are frozen.
    assert_eq!(
        snap.snapshot_seq, snap_seq,
        "held snapshot's seq must not change after later writes"
    );
    assert_eq!(
        snap.cells, snap_cells,
        "held snapshot's cells must not change after later writes"
    );

    // Project onto the model vars and check its invariants.
    let observed: BTreeMap<&'static str, i64> = [
        ("epoch", i64::try_from(live_after).expect("epoch fits i64")),
        ("snapped", 1),
        (
            "snap_seq",
            i64::try_from(snap_seq).expect("snap_seq fits i64"),
        ),
        ("torn", 0),
    ]
    .into_iter()
    .collect();
    assert!(
        m.check_invariant("NoTornRead", &observed),
        "the real no-torn-read outcome must satisfy the model's NoTornRead invariant"
    );
    assert!(
        m.check_invariant("SeqIsStaleOrCurrent", &observed),
        "the real monotone/staleness outcome must satisfy SeqIsStaleOrCurrent"
    );

    // Negative control: a torn read (the captured seq retro-mutated to the later epoch)
    // violates NoTornRead.
    let torn_state: BTreeMap<&'static str, i64> = [
        ("epoch", i64::try_from(live_after).expect("epoch fits i64")),
        ("snapped", 1),
        (
            "snap_seq",
            i64::try_from(live_after).expect("epoch fits i64"),
        ),
        ("torn", 1),
    ]
    .into_iter()
    .collect();
    assert!(
        !m.check_invariant("NoTornRead", &torn_state),
        "negative control: a torn read MUST fail NoTornRead (so the pass is non-vacuous)"
    );
}

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

/// A no-op `process()` (input that leaves the grid undamaged) does not advance the epoch,
/// so a held snapshot stays current: no false staleness.
#[test]
fn noop_process_does_not_advance_snapshot_seq() {
    let m = read_image_seq_model();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"abc");

    let snap = term.cell_frame(rows, cols);
    let snap_seq = snap.snapshot_seq;
    term.take_damage();

    // A bare ESC completes nothing visible: no cell change, no epoch advance.
    term.process(b"\x1b");
    let live_after = term.damage_epoch();
    assert_eq!(
        live_after, snap_seq,
        "a no-op process must not advance the damage epoch"
    );

    let observed: BTreeMap<&'static str, i64> = [
        ("epoch", i64::try_from(live_after).expect("epoch fits i64")),
        ("snapped", 1),
        (
            "snap_seq",
            i64::try_from(snap_seq).expect("snap_seq fits i64"),
        ),
        ("torn", 0),
    ]
    .into_iter()
    .collect();
    assert!(m.check_invariant("SeqIsStaleOrCurrent", &observed));
    assert!(m.check_invariant("NoTornRead", &observed));
}
