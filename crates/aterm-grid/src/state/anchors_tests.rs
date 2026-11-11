// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn set_with(rows: &[(u64, AnchorRole)]) -> (AnchorSet, Vec<AnchorId>) {
    let mut set = AnchorSet::default();
    let ids = rows
        .iter()
        .map(|&(row, role)| set.place(row, role).unwrap())
        .collect();
    (set, ids)
}

#[test]
fn an_empty_set_allocates_nothing_and_every_hook_is_a_no_op() {
    let mut set = AnchorSet::default();
    set.band_move(0, 0, 10, 3);
    set.splice(5, 2);
    set.lose();
    set.note_rekeyed();
    assert!(set.is_empty());
    assert!(!set.lost(), "a set with no anchor has nothing to lose");
    assert_eq!(set.seal_epoch(), 0);
    assert_eq!(set.slots().len(), 0);
}

#[test]
fn a_band_move_carries_anchors_inside_the_band_and_clamps_at_its_edges() {
    let (mut set, ids) = set_with(&[
        (100, AnchorRole::Start), // above the band
        (103, AnchorRole::Start), // in the band, survives
        (101, AnchorRole::End),   // in the band, pushed off its top
        (108, AnchorRole::End),   // in the band, pushed off its bottom by IL
        (111, AnchorRole::Start), // below the band
    ]);
    set.band_move(100, 1, 8, -2);
    assert_eq!(set.row(ids[0]), Some(100));
    assert_eq!(set.row(ids[1]), Some(101));
    assert_eq!(set.row(ids[2]), Some(101), "clamped to the band's top");
    assert_eq!(set.row(ids[3]), Some(106));
    assert_eq!(set.row(ids[4]), Some(111));
    set.band_move(100, 1, 8, 3);
    assert_eq!(set.row(ids[3]), Some(109), "clamped to bottom + 1");
    assert_eq!(
        set.row(ids[1]),
        Some(101),
        "a start on the insertion's top holds"
    );
}

/// Muse Code's launch: three rows opened at the top of a region that
/// starts on a span's start are that span's, every anchor on the top
/// holding its row; a start below the top follows its row, as does one
/// the band starts above.
#[test]
fn an_insertion_at_a_start_holds_it_and_moves_the_starts_below() {
    let (mut set, ids) = set_with(&[
        (102, AnchorRole::Start), // the open span's
        (102, AnchorRole::Start), // a closed span's
        (102, AnchorRole::End),
        (105, AnchorRole::Start), // in the band, below its top
    ]);
    set.band_move(100, 2, 8, 3);
    assert_eq!(set.row(ids[0]), Some(102), "a start holds");
    assert_eq!(set.row(ids[1]), Some(102), "every start on the top holds");
    assert_eq!(set.row(ids[2]), Some(102), "an end holds");
    assert_eq!(set.row(ids[3]), Some(108));
    set.band_move(100, 1, 8, 2);
    assert_eq!(
        set.row(ids[0]),
        Some(104),
        "rows opened above a start are not its"
    );
    set.band_move(100, 4, 8, -1);
    assert_eq!(
        set.row(ids[0]),
        Some(104),
        "a deletion clamps it to the band's top like any start"
    );
}

#[test]
fn the_start_knob_moves_a_start_on_the_top() {
    let (mut set, ids) = set_with(&[(102, AnchorRole::Start), (102, AnchorRole::End)]);
    with_knob(FenceKnob::StartFollows, || set.band_move(100, 2, 8, 3));
    assert_eq!(set.row(ids[0]), Some(105));
    assert_eq!(set.row(ids[1]), Some(102), "an end holds under the knob");
    assert!(!knob(FenceKnob::StartFollows));
}

#[test]
fn a_splice_whose_end_equals_at_stays_put() {
    let (mut set, ids) = set_with(&[
        (10, AnchorRole::Start),
        (20, AnchorRole::End),
        (20, AnchorRole::Start),
        (25, AnchorRole::End),
        (19, AnchorRole::End),
    ]);
    set.splice(20, 3);
    assert_eq!(set.row(ids[0]), Some(10));
    assert_eq!(
        set.row(ids[1]),
        Some(20),
        "an End at the insertion point keeps its end"
    );
    assert_eq!(set.row(ids[2]), Some(23), "a Start at it shifts");
    assert_eq!(set.row(ids[3]), Some(28));
    assert_eq!(set.row(ids[4]), Some(19));
}

#[test]
fn the_splice_end_knob_moves_an_end_at_the_insertion_point() {
    let (mut set, ids) = set_with(&[(20, AnchorRole::End)]);
    with_knob(FenceKnob::SpliceEndAsStart, || set.splice(20, 3));
    assert_eq!(set.row(ids[0]), Some(23));
    assert!(!knob(FenceKnob::SpliceEndAsStart));
}

#[test]
fn ed3_clamps_anchors_below_the_oldest_row() {
    let (mut set, ids) = set_with(&[(3, AnchorRole::Start), (8, AnchorRole::End)]);
    set.clamp_below(10);
    assert_eq!(set.row(ids[0]), Some(10));
    assert_eq!(set.row(ids[1]), Some(10), "a span wholly erased is empty");
}

#[test]
fn slots_are_reused_and_bounded() {
    let mut set = AnchorSet::default();
    let ids: Vec<_> = (0..MAX_ANCHORS as u64)
        .map(|r| set.place(r, AnchorRole::Start).unwrap())
        .collect();
    assert!(
        set.place(99, AnchorRole::End).is_none(),
        "the cap fails closed"
    );
    set.free(ids[7]);
    assert_eq!(set.place(99, AnchorRole::End), Some(ids[7]));
    assert_eq!(set.live_count(), MAX_ANCHORS);
}

#[test]
fn a_loss_marks_lost_and_moves_the_seal_epoch() {
    let (mut set, _) = set_with(&[(1, AnchorRole::Start)]);
    set.lose();
    assert!(set.lost());
    assert_eq!(set.seal_epoch(), 1);
    set.clear_lost();
    with_knob(FenceKnob::UnlentRenumberKeeps, || set.lose_renumbered());
    assert!(!set.lost(), "the knob keeps a renumbered set");
    set.lose_renumbered();
    assert!(set.lost());
}
