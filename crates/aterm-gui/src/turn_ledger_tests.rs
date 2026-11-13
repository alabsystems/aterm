// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CEILING: u64 = 1 << 20;

fn rec(id: u64) -> TurnRecord {
    TurnRecord {
        id,
        started_ms: id,
        dur_ms: 1,
        submitted: true,
        status: "settled",
        text: format!("msg{id}").into(),
        screen_hash: id,
        seq: id,
        arch: ArchMark::default(),
        by: TurnBy::Ctl,
        carried: false,
    }
}

#[test]
fn by_words_round_trip_and_anything_else_is_unknown() {
    let sid = aterm_session::SessionId::generate();
    for by in [
        TurnBy::Harness,
        TurnBy::Ctl,
        TurnBy::Bridge,
        TurnBy::Sid(sid.as_str().to_string()),
        TurnBy::fleet(&"87be5228".repeat(4), None),
        TurnBy::fleet(&"87be5228".repeat(4), Some(sid.as_str())),
        TurnBy::Unknown,
    ] {
        assert_eq!(TurnBy::from_wire(by.as_wire()), by, "{by:?}");
    }
    assert_eq!(
        TurnBy::fleet(&"87be5228".repeat(4), None).as_wire(),
        "fleet:87be5228:-",
        "the node's head, `-` for no session"
    );
    assert_eq!(
        TurnBy::fleet(&"87be5228".repeat(4), Some(sid.as_str())).as_wire(),
        format!("fleet:87be5228:{}", sid.as_str()),
        "the whole minted sid, which reads back as itself"
    );
    assert_eq!(TurnBy::fleet(" ", None), TurnBy::Unknown, "no node");
    for word in [
        "",
        "fleet",
        "fleet:",
        "fleet:m9",
        "fleet::-",
        "fleet:m9:",
        "fleet:m9:- x",
        "human",
        "Harness",
        "s-",
        "s-xyz",
        "s-1 text=x",
    ] {
        assert_eq!(TurnBy::from_wire(word), TurnBy::Unknown, "{word:?}");
    }
    assert_eq!(TurnBy::default(), TurnBy::Unknown);
}

#[test]
fn every_record_moves_the_generation_and_tells_the_watch() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let told = Arc::new(AtomicU64::new(0));
    let mut l = TurnLedger::default();
    assert_eq!(l.generation(), 0);
    assert!(!l.carries_anything());
    l.push(rec(1));
    assert_eq!(l.generation(), 1, "a record before any watch still counts");
    let seen = Arc::clone(&told);
    l.watch(Arc::new(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    }));
    for id in 2..=(LEDGER_CAP as u64 + 3) {
        l.push(rec(id));
    }
    assert_eq!(l.generation(), LEDGER_CAP as u64 + 3);
    assert_eq!(told.load(Ordering::Relaxed), LEDGER_CAP as u64 + 2);
    l.begin(9_000);
    l.abandon(9_000);
    assert_eq!(l.record(rec(9_001), || 9_002), 9_001);
    assert_eq!(
        (l.generation(), told.load(Ordering::Relaxed)),
        (LEDGER_CAP as u64 + 4, LEDGER_CAP as u64 + 3),
        "a turn's record counts once; its begin and abandon do not"
    );
    let carried = TurnLedger::carried(vec![rec(4), rec(5)], 0);
    assert_eq!(carried.generation(), 2);
    assert!(carried.carries_anything());
    assert!(TurnLedger::carried(Vec::new(), 3).carries_anything());
    assert!(!TurnLedger::carried(Vec::new(), 0).carries_anything());
}

#[test]
fn the_room_turn_is_the_newest_the_harness_did_not_type() {
    let harness = |id: u64, carried: bool| TurnRecord {
        status: STATUS_PRESSED,
        by: TurnBy::Harness,
        carried,
        ..rec(id)
    };
    let mut l = TurnLedger::default();
    assert!(l.newest_not_harness().is_none());
    l.push(harness(1, true));
    assert!(l.newest_not_harness().is_none(), "the harness's alone");
    l.push(rec(2));
    l.push(harness(3, false));
    l.push(harness(4, true));
    assert_eq!(l.high_id(), Some(4));
    assert_eq!(l.newest_not_harness().map(|r| r.id), Some(2));
    l.push(rec(5));
    assert_eq!(l.newest_not_harness().map(|r| r.id), Some(5));
}

#[test]
fn the_harness_makes_room_among_its_own_and_the_managers_turn_stays() {
    let harness = |id: u64| TurnRecord {
        status: STATUS_PRESSED,
        by: TurnBy::Harness,
        ..rec(id)
    };
    let cap = LEDGER_CAP as u64;
    let ids = |l: &TurnLedger| l.since(None).map(|r| r.id).collect::<Vec<u64>>();
    let mut l = TurnLedger::default();
    l.push(rec(1));
    let last = 1 + 2 * cap;
    for id in 2..=last {
        l.push(harness(id));
    }
    let held = ids(&l);
    assert_eq!(held.len(), LEDGER_CAP);
    assert_eq!(l.newest_not_harness().map(|r| r.id), Some(1), "kept");
    assert_eq!(held[1..], (last - cap + 2..=last).collect::<Vec<u64>>()[..]);
    assert_eq!(l.low_id(), Some(held[1]), "a resume from 1 is told");
    assert_eq!(l.shed_below(), held[1]);
    let resumed = last + 1;
    for id in resumed..resumed + cap - 1 - HARNESS_SHARE as u64 {
        l.push(rec(id));
    }
    assert_eq!(l.since(None).next().map(|r| r.id), Some(1), "still held");
    l.push(rec(resumed + cap));
    assert_ne!(l.since(None).next().map(|r| r.id), Some(1), "drop-oldest");
    let held = ids(&l);
    assert!(held.windows(2).all(|w| w[0] < w[1]), "in id order");
    for run in [rec as fn(u64) -> TurnRecord, |id| TurnRecord {
        by: TurnBy::Harness,
        ..rec(id)
    }] {
        let mut l = TurnLedger::default();
        for id in 1..=cap + 5 {
            l.push(run(id));
        }
        assert_eq!(ids(&l), (6..=cap + 5).collect::<Vec<u64>>());
        assert_eq!((l.shed_below(), l.low_id()), (0, Some(6)));
    }
}

#[test]
fn since_seek_matches_the_linear_filter_for_every_watermark() {
    let mut l = TurnLedger::default();
    for i in 1..=(LEDGER_CAP as u64 + 5) {
        l.push(rec(i * 7));
    }
    let low = l.low_id().expect("non-empty");
    let high = l.high_id().expect("non-empty");
    assert!(
        low > 1,
        "the fixture must have evicted, or the below-low arm is vacuous"
    );

    let reference = |after: Option<u64>| -> Vec<u64> {
        l.records
            .iter()
            .filter(|r| after.is_none_or(|a| r.id > a))
            .map(|r| r.id)
            .collect()
    };
    let observed = |after: Option<u64>| -> Vec<u64> { l.since(after).map(|r| r.id).collect() };

    let mut probes: Vec<Option<u64>> = vec![None, Some(0), Some(1), Some(low - 1)];
    for id in [
        low,
        low + 1,
        low + 3,
        high - 7,
        high - 1,
        high,
        high + 1,
        high + 100,
    ] {
        probes.push(Some(id));
    }
    for after in probes {
        assert_eq!(
            observed(after),
            reference(after),
            "since({after:?}) diverged"
        );
    }
    assert_eq!(
        observed(Some(low - 1)).len(),
        LEDGER_CAP,
        "below low-water = all retained"
    );
    assert!(
        observed(Some(high)).is_empty(),
        "at the high-water = nothing new"
    );
}

#[test]
fn a_preempted_turn_that_records_late_is_renumbered_and_streamed() {
    let mut l = TurnLedger::default();
    l.push(rec(5));
    l.begin(10);
    l.begin(11);
    assert_eq!(l.record(rec(11), || unreachable!("in order")), 11);
    assert_eq!(l.carry_floor(CEILING), CEILING + 1);
    let watermark = l.high_id();
    let mut minted = 11;
    let late = l.record(rec(10), || {
        minted += 1;
        minted
    });
    assert_eq!(late, 12, "re-numbered past the newer record");
    let ids: Vec<u64> = l.since(None).map(|r| r.id).collect();
    assert_eq!(ids, vec![5, 11, 12], "rising in landing order");
    let streamed: Vec<u64> = l.since(watermark).map(|r| r.id).collect();
    assert_eq!(streamed, vec![12], "the watcher past 11 is sent it");
    assert_eq!(l.carry_floor(CEILING), 0, "nothing is open any more");

    let carried = TurnLedger::carried(vec![rec(5), rec(11), rec(10), rec(11)], 0);
    let ids: Vec<u64> = carried.since(None).map(|r| r.id).collect();
    assert_eq!(ids, vec![5, 10, 11]);
    assert_eq!(carried.since(Some(9)).count(), 2, "turn 10 is found");
}

#[test]
fn an_open_turn_is_carried_as_an_id_the_ledger_does_not_vouch_for() {
    let mut l = TurnLedger::default();
    for id in 1..=3 {
        l.push(rec(id));
    }
    l.begin(4);
    assert_eq!(l.carry_floor(CEILING), 5);
    let adopted = TurnLedger::carried(l.since(None).cloned().collect(), l.carry_floor(CEILING));
    assert_eq!(adopted.low_id(), Some(5), "the floor, over the held 1");
    l.abandon(4);
    assert_eq!(l.carry_floor(CEILING), 0);
}

#[test]
fn a_floor_goes_stale_once_the_records_under_it_would_have_aged_out() {
    let cap = LEDGER_CAP as u64;
    let empty = TurnLedger::carried(Vec::new(), 10);
    assert_eq!(empty.live_floor(), Some(10), "an empty ledger's floor");
    assert_eq!(empty.carry_floor(CEILING), 10);
    let mut dropped = TurnLedger::carried(Vec::new(), 10);
    dropped.push(rec(10));
    assert_eq!(dropped.live_floor(), Some(10), "one turn past a drop");
    assert_eq!(
        dropped.carry_floor(CEILING),
        10,
        "carried on by a second update"
    );
    let at = TurnLedger::carried((10..=12).map(rec).collect(), 10);
    assert_eq!(at.live_floor(), Some(10), "at the oldest held record");
    assert_eq!(at.low_id(), Some(10), "the low-water says it too");
    let mut l = TurnLedger::carried((1..=3).map(rec).collect(), 10);
    assert_eq!(l.live_floor(), Some(10), "above the oldest held record");
    for id in 11..10 + cap - 1 {
        dropped.push(rec(id));
    }
    for id in 10..10 + cap - 1 {
        l.push(rec(id));
    }
    assert_eq!(dropped.len(), LEDGER_CAP - 1);
    assert_eq!(dropped.live_floor(), Some(10), "not yet LEDGER_CAP turns");
    assert_eq!(l.live_floor(), Some(10), "records under 10 still held");
    dropped.push(rec(10 + cap - 1));
    l.push(rec(10 + cap - 1));
    assert_eq!(l.low_id(), Some(10));
    assert_eq!(l.live_floor(), None, "LEDGER_CAP turns past the floor");
    assert_eq!(dropped.live_floor(), None, "the lost turns aged out");
    assert_eq!(l.carry_floor(CEILING), 0, "a stale floor is not carried on");
    l.begin(10 + cap);
    assert_eq!(l.carry_floor(CEILING), 11 + cap);
}

#[test]
fn text_clamps_on_boundary() {
    let long = format!("a{}", "é".repeat(400));
    assert_eq!(&*clamp_text(&long), format!("a{}", "é".repeat(255)));
    let on_boundary = "é".repeat(400);
    assert_eq!(
        &*clamp_text(&on_boundary),
        "é".repeat(256),
        "cut at the cap"
    );
    let short = "é".repeat(256);
    assert_eq!(&*clamp_text(&short), short, "at the cap it is kept whole");
}
