// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn failed(ev: &Evidence) -> Vec<&'static str> {
    judge(ev)
        .into_iter()
        .filter(|c| !c.pass)
        .map(|c| c.name)
        .collect()
}

fn end(mac: &str, member: bool, reads_value: bool) -> MacEnd {
    MacEnd {
        mac: mac.to_owned(),
        has_log: true,
        member,
        undone: true,
        remover_out_by_quorum: true,
        rotation_due: false,
        reads_value,
    }
}

fn act(on: &str, names: &str) -> Act {
    Act {
        on: on.to_owned(),
        names: names.to_owned(),
        ok: true,
        said: String::new(),
    }
}

/// What F16 promises after b2 removes a1, a2 and b1: two of them undo it, all three are back
/// in and read the value a2 put after, and b2 is out by the quorum and reads none of it.
fn whole() -> Evidence {
    Evidence {
        skipped: None,
        targets: vec!["a1".to_owned(), "a2".to_owned(), "b1".to_owned()],
        removals: vec![act("b2", "a1"), act("b2", "a2"), act("b2", "b1")],
        undos: vec![act("a1", "b2"), act("a2", "b2")],
        voters: 3,
        signed: 2,
        minted: vec!["a1 minted 4 records in round 1".to_owned()],
        carry_failures: Vec::new(),
        put: Some(Ok(())),
        wrapped_to_remover: Vec::new(),
        remover_row: Some("m4 was removed by a quorum of m1, m2 on Oct 29".to_owned()),
        ends: vec![
            end("a1", true, true),
            end("a2", true, true),
            end("b1", true, true),
            end("b2", false, false),
        ],
        ms: 4_000,
    }
}

/// A run where every Mac b2 removed is back in and reads the new value passes both checks.
///
/// NEGATIVE CONTROL: in `judge`, flag the remover when it does not read the value
/// (`if !e.reads_value`): `whole` fails [`BACK_IN`] (proven 2026-10-29).
#[test]
fn a_run_where_every_removed_mac_is_back_in_passes_both_checks() {
    assert_eq!(failed(&whole()), Vec::<&str>::new());
}

/// The voided remover holding a wrap of the value's epoch (no new epoch after the quorum
/// `Remove`, the lane's negative control) fails [`BACK_IN`] though every Mac reads as it should.
///
/// NEGATIVE CONTROL: drop `judge`'s `wrapped_to_remover` arm: the run passes (proven
/// 2026-10-29).
#[test]
fn the_voided_remover_holding_the_values_epoch_fails_back_in() {
    let mut ev = whole();
    ev.wrapped_to_remover = vec!["a1".to_owned(), "a2".to_owned()];
    assert_eq!(failed(&ev), vec![BACK_IN]);
}

/// An Undo short of its quorum on one Mac's records fails [`UNDONE`].
///
/// NEGATIVE CONTROL: drop `judge`'s loop over each Mac's end for [`UNDONE`]: the run passes
/// (proven 2026-10-29).
#[test]
fn an_undo_short_of_its_quorum_fails_undone() {
    let mut ev = whole();
    ev.ends[2].undone = false;
    ev.ends[2].remover_out_by_quorum = false;
    assert_eq!(failed(&ev), vec![UNDONE]);
}
