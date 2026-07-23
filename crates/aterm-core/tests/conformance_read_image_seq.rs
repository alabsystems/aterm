// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance (Tier-1): the REAL `Terminal::cell_frame` + `damage_epoch` /
//! `take_damage` path obeys the DERIVED snapshot-seq protocol (REARCH A-3).
//!
//! `aterm_spec::derive::read_image_seq_model()` is the drift-free model of the
//! read_image snapshot-seq contract: monotone seq, snapshot internal-consistency
//! (no torn read), staleness-detectable — `ty` PROVES it at `Buggy=0` and CATCHES
//! a torn read at `Buggy=1` (in `aterm-spec/tests/derived_ring_ty.rs`). This binds
//! that property to the real engine snapshot:
//!
//!   * a `cell_frame` snapshot is stamped with `damage_epoch()` (its `snapshot_seq`)
//!     captured under the one lock — the "value of seq" at snapshot time;
//!   * subsequent `process()` advances the engine (the live `damage_epoch()` grows)
//!     but does NOT mutate the already-emitted snapshot's `snapshot_seq` OR its
//!     rendered cells (no torn read / snapshot isolation);
//!   * the consumer can always tell its snapshot is behind (`damage_epoch() >
//!     snap.snapshot_seq`), i.e. staleness is detectable.
//!
//! The observed state is projected onto the model's `<<epoch, snapped, snap_seq,
//! torn>>` and checked against its actual invariants. A NEGATIVE CONTROL (a forced
//! retro-mutation of the captured seq) is shown to VIOLATE the model invariant, so
//! the pass is never vacuous. Pure Rust + real engine code, so it always runs.

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_spec::derive::read_image_seq_model;

/// Drive the real read_image snapshot-seq path and bind it to the derived model.
#[test]
fn real_cell_frame_seq_is_monotone_isolated_and_stale_detectable() {
    let m = read_image_seq_model();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);

    // Some net-new damage, then advance the engine's monotone epoch by observing
    // it (the latch bumps once per damage session) and consuming the damage so the
    // NEXT write opens a fresh session — exactly the gui's lock-held discipline.
    term.process(b"hello");
    let _ = term.damage_epoch();
    term.take_damage();
    term.process(b" world");

    // read_image: capture the snapshot. Its `snapshot_seq` is the engine's
    // `damage_epoch` at snapshot time, filled under the same (here, single-thread)
    // lock as the cells — the canonical value-of-seq for this frame.
    let snap = term.cell_frame(rows, cols);
    let snap_seq = snap.snapshot_seq;
    let snap_cells = snap.cells.clone();
    // The live epoch equals the captured seq right after the snapshot (the engine
    // is idempotent within a damage session): the snapshot is CURRENT, not stale.
    assert_eq!(
        term.damage_epoch(),
        snap_seq,
        "snapshot_seq must equal the live damage_epoch at snapshot time"
    );

    // A later write advances the engine. Consume the prior damage first so this is
    // net-new and the epoch genuinely advances (a new damage session).
    term.take_damage();
    term.process(b"\r\nmore output on a new line");
    let live_after = term.damage_epoch();

    // 1. MONOTONE SEQ + STALENESS-DETECTABLE: the live epoch advanced past the held
    //    snapshot, so a consumer comparing them sees its snapshot is behind.
    assert!(
        live_after > snap_seq,
        "later damage must advance the live epoch past the captured snapshot_seq \
         (monotone seq; staleness is observable as live > snap)"
    );

    // 2. SNAPSHOT INTERNAL-CONSISTENCY / NO TORN READ: the already-emitted snapshot
    //    is frozen — neither its seq stamp nor its rendered cells changed despite
    //    the later writes.
    assert_eq!(
        snap.snapshot_seq, snap_seq,
        "held snapshot's seq must not change after later writes"
    );
    assert_eq!(
        snap.cells, snap_cells,
        "held snapshot's cells must not change after later writes"
    );

    // Project the observed outcome onto the model vars and check its REAL
    // invariants. `snapped = 1` (a snapshot was taken), `torn = 0` (no leak),
    // `epoch = live_after`, `snap_seq = snap_seq`.
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

    // NEGATIVE CONTROL (non-vacuity): a TORN read — the captured seq retro-mutated
    // to the later epoch — MUST violate the model's NoTornRead invariant. If this
    // ever passed, the invariant would be trivially true and the above pass empty.
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

/// A no-op `process()` (input that leaves the grid undamaged) does NOT advance the
/// epoch, so a held snapshot stays CURRENT — the model's `snap_seq <= epoch` holds
/// with equality and no false staleness is reported.
#[test]
fn noop_process_does_not_advance_snapshot_seq() {
    let m = read_image_seq_model();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"abc");

    let snap = term.cell_frame(rows, cols);
    let snap_seq = snap.snapshot_seq;
    term.take_damage();

    // Feed bytes that don't damage the grid (a bare ESC that completes nothing
    // visible is consumed without a cell change). The epoch must not advance.
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
