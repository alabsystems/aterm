// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NAME: &str = "lab: the split held";

/// A record id: `n` in every byte.
fn id(n: u8) -> RecordId {
    [n; 32]
}

/// A record `signer` made on `island`.
fn made(signer: &str, island: Island, kind: &str, n: u8) -> (RecordId, Made) {
    (
        id(n),
        Made {
            what: format!("{signer}'s {kind} {n:02x}{n:02x}{n:02x}{n:02x}"),
            from: Some(island),
        },
    )
}

/// A digest: `digit` 64 times.
fn digest(digit: char) -> String {
    std::iter::repeat_n(digit, 64).collect()
}

fn head(seq: u64, digit: char) -> Option<HeadMark> {
    Some(HeadMark {
        seq,
        digest: digest(digit),
        parents: Vec::new(),
    })
}

fn holder(
    name: &str,
    island: Island,
    records: &[(RecordId, Made)],
    head: Option<HeadMark>,
) -> Holder {
    Holder {
        name: name.to_owned(),
        island,
        server: name.contains("server"),
        records: records.iter().cloned().collect(),
        head,
    }
}

/// What every holder held going into the split: the genesis and a1's admission of b1, and one
/// head.
fn before() -> Vec<(RecordId, Made)> {
    vec![
        made("a1", Island::A, "Genesis", 1),
        made("a1", Island::A, "Admit", 2),
    ]
}

/// F21's split as it should go: island A's a1 removes b3 (records 10 and 11, then a head it
/// published on its island, seq 8 `a…`), island B's b1 writes (record 20, head seq 8 `b…`), and
/// each island's Macs and server hold only their own island's. Nothing was in flight when the
/// cut fell, so its start read is what the cut's read found. The other planes' tests use it.
pub(crate) fn held() -> Ledger {
    let start = Snapshot {
        holders: [
            ("a1", Island::A),
            ("b1", Island::B),
            ("island A's server", Island::A),
            ("island B's server", Island::B),
        ]
        .iter()
        .map(|(name, island)| holder(name, *island, &before(), head(7, '0')))
        .collect(),
        unread: Vec::new(),
    };
    let mut a = before();
    a.push(made("a1", Island::A, "Remove", 10));
    a.push(made("a1", Island::A, "Epoch", 11));
    let mut b = before();
    b.push(made("b1", Island::B, "Put", 20));
    Ledger {
        cut: start.clone(),
        start,
        end: Snapshot {
            holders: vec![
                holder("a1", Island::A, &a, head(8, 'a')),
                holder("b1", Island::B, &b, head(8, 'b')),
                holder("island A's server", Island::A, &a, head(8, 'a')),
                holder("island B's server", Island::B, &b, head(8, 'b')),
            ],
            unread: Vec::new(),
        },
        landed_ms: 3_000,
        ms: 3_900,
        failed: None,
        syncs: vec![synced("a1", Island::A, 2), synced("b3", Island::B, 1)],
    }
}

/// A records sync on `mac` under the split that put the `made` records it made on its island's
/// server.
fn synced(mac: &str, island: Island, made: usize) -> Synced {
    Synced {
        mac: mac.to_owned(),
        island,
        made,
        lacking: Vec::new(),
        unread: None,
        said: None,
    }
}

/// [`held`] where a1's records sync under the split left island A's server without a1's
/// removal of b3: the other planes' tests use it.
pub(crate) fn unexercised() -> Ledger {
    let mut ledger = held();
    ledger.syncs[0]
        .lacking
        .push(made("a1", Island::A, "Remove", 10).1.what);
    ledger
}

fn end_mut<'a>(ledger: &'a mut Ledger, name: &str) -> &'a mut Holder {
    ledger
        .end
        .holders
        .iter_mut()
        .find(|h| h.name == name)
        .unwrap()
}

/// [`held`] where a1's removal of b3 reached island B's server and b1 before the heal, as the
/// 2026-11-02 heal-retry run found: the other planes' tests use it.
pub(crate) fn crossed() -> Ledger {
    let mut ledger = held();
    let removal = made("a1", Island::A, "Remove", 10);
    for name in ["island B's server", "b1"] {
        end_mut(&mut ledger, name)
            .records
            .insert(removal.0, removal.1.clone());
    }
    ledger
}

/// A records sync under the split that left its own island's server short, that had nothing
/// to post, or that could not be read: the split is not shown to have held that pass back, and
/// says so, though nothing crossed.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `shortfalls`, drop the `Unexercised` push: the split passes, and the test fails at its first
/// `assert!(!check.pass)` (proven 2026-11-03).
#[test]
fn a_records_sync_that_left_its_own_server_short_is_not_exercised() {
    let ledger = unexercised();
    let check = judge(NAME, "F21's split", Some(&ledger));
    assert!(!check.pass, "{check:?}");
    assert_eq!(shortfalls(Some(&ledger)), vec![Shortfall::Unexercised]);
    assert!(
        check
            .detail
            .contains("a1's records sync left island A's server lacking 1 of the 2"),
        "{check:?}"
    );
    assert!(!Shortfall::Unexercised.voids());

    let mut idle = held();
    idle.syncs[1].made = 0;
    assert!(!judge(NAME, "F21's split", Some(&idle)).pass);

    let mut unread = held();
    unread.syncs[1].unread = Some("island B's server's records: timed out".to_owned());
    assert!(!judge(NAME, "F21's split", Some(&unread)).pass);

    let check = judge(NAME, "F21's split", Some(&held()));
    assert!(
        check.pass
            && check
                .detail
                .contains("a1's records sync put the 2 records it made on island A's server"),
        "{check:?}"
    );
}

/// Each island kept what it made: the split holds and says how much each made.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): make
/// `is_crossing` answer `true` for every id, so the records both islands held going in count as
/// crossed: fails at `assert!(check.pass)`, the genesis and a1's admission read as crossed
/// (proven 2026-11-03).
#[test]
fn a_split_where_each_island_kept_what_it_made_holds() {
    let check = judge(NAME, "F21's split", Some(&held()));
    assert!(check.pass, "{check:?}");
    assert_eq!(crossings(&held()), Vec::new());
    assert_eq!(
        (made_in(&held(), Island::A), made_in(&held(), Island::B)),
        (2, 1)
    );
}

/// a1's removal of b3 on island B's server (a1's subscriber posted its held records to a
/// server that lacked its frontier, today's leak) and so on b1, which follows that server: it
/// crossed by the records sync.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): drop
/// `judge`'s loop that pushes each crossing: the split passes, and the test fails at
/// `assert!(!check.pass)` (proven 2026-11-02).
#[test]
fn a_removal_the_other_islands_server_took_crossed_by_the_records_sync() {
    let ledger = crossed();
    let removal = made("a1", Island::A, "Remove", 10);
    assert_eq!(
        crossings(&ledger),
        vec![Crossing {
            what: removal.1.what.clone(),
            plane: Plane::RecordsSync,
            on: vec![
                "a1".to_owned(),
                "b1".to_owned(),
                "island A's server".to_owned(),
                "island B's server".to_owned()
            ],
            held_at_cut: false,
        }]
    );
    let check = judge(NAME, "F21's split", Some(&ledger));
    assert!(!check.pass, "{check:?}");
}

/// b1 holding a1's removal while island B's server does not: b1 took it itself, so the plane
/// is a Mac's own pull, not a server's.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `crossings`, count any server holding it (`h.server` alone) as the records sync: fails with
/// left `RecordsSync`, right `Direct`, since island A's server holds it (proven 2026-11-02).
#[test]
fn a_record_one_mac_took_straight_crossed_by_its_own_pull() {
    let mut ledger = held();
    let removal = made("a1", Island::A, "Remove", 10);
    end_mut(&mut ledger, "b1")
        .records
        .insert(removal.0, removal.1);
    let crossed = crossings(&ledger);
    assert_eq!(crossed.len(), 1, "{crossed:?}");
    assert_eq!(crossed[0].plane, Plane::Direct);
    assert!(!judge(NAME, "F21's split", Some(&ledger)).pass);
}

/// A head island A published during the split, held on island B: it crossed by the envelope,
/// though no record did.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): drop the
/// loop over `heads_a` in `crossings`: fails with left `[]` (proven 2026-11-02).
#[test]
fn a_head_one_island_made_and_the_other_holds_crossed_by_the_envelope() {
    let mut ledger = held();
    end_mut(&mut ledger, "island B's server").head = head(8, 'a');
    let crossed = crossings(&ledger);
    assert_eq!(
        crossed,
        vec![Crossing {
            what: "head seq 8 aaaaaaaa".to_owned(),
            plane: Plane::Envelope,
            on: vec![
                "a1".to_owned(),
                "island A's server".to_owned(),
                "island B's server".to_owned()
            ],
            held_at_cut: false,
        }]
    );
    assert!(!judge(NAME, "F21's split", Some(&ledger)).pass);
}

/// An epoch island A's tender minted just before the cut (record 5), held on island A alone
/// when it fell and at the start read, that b1 and island B's server hold at the end: it crossed
/// by the records sync, though a holder held it going in, and the crossing says so.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `is_crossing`, judge only what no holder held when the cut fell (`!at_cut`), the shared
/// filter the ledger had: the epoch reads as old, and the test fails with left `[]` (proven
/// 2026-11-03).
#[test]
fn a_record_one_island_held_alone_at_the_start_that_both_hold_at_the_end_crossed() {
    let mut ledger = held();
    let epoch = made("a2", Island::A, "Epoch", 5);
    for snapshot in [&mut ledger.cut, &mut ledger.start] {
        for h in snapshot
            .holders
            .iter_mut()
            .filter(|h| h.island == Island::A)
        {
            h.records.insert(epoch.0, epoch.1.clone());
        }
    }
    for h in &mut ledger.end.holders {
        h.records.insert(epoch.0, epoch.1.clone());
    }
    assert_eq!(
        crossings(&ledger),
        vec![Crossing {
            what: epoch.1.what.clone(),
            plane: Plane::RecordsSync,
            on: vec![
                "a1".to_owned(),
                "b1".to_owned(),
                "island A's server".to_owned(),
                "island B's server".to_owned()
            ],
            held_at_cut: true,
        }]
    );
    let check = judge(NAME, "F21's split", Some(&ledger));
    assert!(!check.pass, "{check:?}");
    assert!(
        check
            .detail
            .contains("held on one island when the cut fell"),
        "{check:?}"
    );
    // What island A alone held at the start and still holds alone is no crossing.
    let mut kept = held();
    for snapshot in [&mut kept.cut, &mut kept.start, &mut kept.end] {
        for h in snapshot
            .holders
            .iter_mut()
            .filter(|h| h.island == Island::A)
        {
            h.records.insert(epoch.0, epoch.1.clone());
        }
    }
    assert_eq!(crossings(&kept), Vec::new());
}

/// a1's removal, made after the cut fell and on both islands by the start read (it crossed
/// while the ledger waited out what was in flight): it crossed, though both islands held it at
/// the start read.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `is_crossing`, drop `at_cut` (judge the start read alone): the removal reads as old, and the
/// test fails at `crossed.len()` with left `0`, right `1` (proven 2026-11-03).
#[test]
fn a_record_made_after_the_cut_that_crossed_before_the_start_read_crossed() {
    let mut ledger = crossed();
    let removal = made("a1", Island::A, "Remove", 10);
    for h in &mut ledger.start.holders {
        h.records.insert(removal.0, removal.1.clone());
    }
    let crossed = crossings(&ledger);
    assert_eq!(crossed.len(), 1, "{crossed:?}");
    assert_eq!(
        (crossed[0].plane, crossed[0].held_at_cut),
        (Plane::RecordsSync, false)
    );
    assert!(!judge(NAME, "F21's split", Some(&ledger)).pass);
}

/// Island A's head (seq 8 `a…`) reached island B's server, which then published seq 9 `c…` on
/// it: the end read finds `a…` only among `c…`'s parents, and it crossed by the envelope.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `heads_of`, show a holder's own head alone (drop its parents): the replaced head is unseen,
/// and the test fails with left `[]` (proven 2026-11-03).
#[test]
fn a_head_that_crossed_and_was_replaced_crossed_by_the_envelope() {
    let mut ledger = held();
    end_mut(&mut ledger, "island B's server").head = Some(HeadMark {
        seq: 9,
        digest: digest('c'),
        parents: vec![digest('a'), digest('b')],
    });
    assert_eq!(
        crossings(&ledger),
        vec![Crossing {
            what: "head seq 8 aaaaaaaa".to_owned(),
            plane: Plane::Envelope,
            on: vec![
                "a1".to_owned(),
                "island A's server".to_owned(),
                "island B's server".to_owned()
            ],
            held_at_cut: false,
        }]
    );
    assert!(!judge(NAME, "F21's split", Some(&ledger)).pass);
}

/// A head both islands held going in is no crossing, though both still hold it.
#[test]
fn a_head_both_islands_held_before_the_split_is_no_crossing() {
    let mut ledger = held();
    for h in &mut ledger.end.holders {
        h.head = head(7, '0');
    }
    assert!(judge(NAME, "F21's split", Some(&ledger)).pass);
}

/// A holder the end could not read, a split the lab could not hold, and a split never read are
/// each not shown to hold, though nothing is seen to cross.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): drop the
/// `unread` loop in `judge`: the unread holder's split passes, and the test fails at its first
/// `assert!(!check.pass)` (proven 2026-11-02).
#[test]
fn a_split_not_read_whole_or_not_held_is_not_shown_to_hold() {
    let mut unread = held();
    unread
        .end
        .unread
        .push("island B's server's records: connection refused".to_owned());
    let check = judge(NAME, "F21's split", Some(&unread));
    assert!(!check.pass, "{check:?}");

    let mut not_held = held();
    not_held.failed = Some("the cut file: read-only".to_owned());
    assert!(!judge(NAME, "F21's split", Some(&not_held)).pass);

    let mut empty = held();
    empty.start.holders.clear();
    assert!(!judge(NAME, "F21's split", Some(&empty)).pass);

    let mut cut_unread = held();
    cut_unread.cut.unread.push("a1's records: torn".to_owned());
    assert!(!judge(NAME, "F21's split", Some(&cut_unread)).pass);

    assert!(!judge(NAME, "F21's split", None).pass);
}
