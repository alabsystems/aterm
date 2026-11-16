// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::Held;
use crate::reading::Proc;

const BOUND: Duration = Duration::from_secs(900);
const PID: u32 = 41_714;

fn claim(seq: u64, admitted_ms: u64) -> Record {
    Record {
        accounting: crate::Accounting::Holder,
        pid: PID,
        start_ms: admitted_ms,
        laid_at: None,
        seq,
        checkout: "/work".to_owned(),
        need: crate::Need::new("held work").mem_gib(16),
        timing: crate::TimingEvidence::default(),
        held: Some(Held {
            admitted_ms,
            cores: 1,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

fn holder(record: &Record) -> Holder {
    Holder {
        pid: record.pid,
        what: record.need.what.clone(),
        checkout: record.checkout.clone(),
        held: record.held.is_some(),
        age: Duration::ZERO,
        mem: record.need.mem_now(),
        claimed: record.need.mem_now(),
        alone: false,
        disk: 0,
        quiet: false,
        order: record.order(),
        stalled: None,
        credit: record.credit.clone(),
        overrun: None,
    }
}

fn idle() -> Procs {
    Procs::of(
        [PID, 98, 99]
            .into_iter()
            .map(|pid| Proc {
                pid,
                ppid: 1,
                ..Proc::default()
            })
            .collect(),
    )
}

fn look(cache: &mut Cache, records: &[Record], blocked_by: &Record, at: Instant) -> Holder {
    let dir = Path::new("/ledger");
    cache.refresh(dir, records, at);
    let mut holders = [holder(blocked_by)];
    cache.read(dir, &mut holders, Some(&idle()), &[], at, BOUND);
    holders.into_iter().next().unwrap()
}

#[test]
fn a_new_claim_cannot_inherit_the_previous_claims_stall() {
    let mut cache = Cache::new();
    let now = Instant::now();
    let old = claim(1, 1_000);
    assert_eq!(look(&mut cache, &[old.clone()], &old, now).stalled, None);
    assert_eq!(
        look(&mut cache, &[old.clone()], &old, now + BOUND).stalled,
        Some(BOUND)
    );

    // No scan of an empty ledger and no counter/member change between the two claims.
    let fresh = claim(2, 901_000);
    let began = now + BOUND + Duration::from_secs(1);
    assert_eq!(
        look(&mut cache, &[fresh.clone()], &fresh, began).stalled,
        None,
        "a fresh claim starts its own observation"
    );
    assert_eq!(
        look(
            &mut cache,
            &[fresh.clone()],
            &fresh,
            began + BOUND - Duration::from_nanos(1)
        )
        .stalled,
        None
    );
    for at in [began + BOUND, began + BOUND + Duration::from_secs(1)] {
        let observed = look(&mut cache, &[fresh.clone()], &fresh, at);
        let stuck = super::super::stuck(&[observed]).expect("genuine qualifying stall look");
        assert_eq!(stuck.holders, [PID]);
        assert!(stuck.span >= BOUND);
    }
}

#[test]
fn simultaneous_claims_of_one_process_keep_independent_observations() {
    let mut cache = Cache::new();
    let now = Instant::now();
    // Both can be admitted in one millisecond; seq distinguishes their immutable orders.
    let a = claim(1, 1_000);
    let b = claim(2, 1_000);
    let records = [a.clone(), b.clone()];
    assert_eq!(look(&mut cache, &records, &a, now).stalled, None);
    assert_eq!(
        look(&mut cache, &records, &a, now + BOUND).stalled,
        Some(BOUND)
    );
    assert_eq!(
        look(&mut cache, &records, &b, now + BOUND).stalled,
        None,
        "a different resource's claim has not been observed idle"
    );
    assert_eq!(
        look(&mut cache, &records, &a, now + BOUND).stalled,
        Some(BOUND),
        "reading B cannot replace A's history in a single PID slot"
    );
    assert_eq!(
        look(&mut cache, &records, &b, now + BOUND * 2).stalled,
        Some(BOUND)
    );
}

#[test]
fn observed_credit_changes_reset_without_resurrecting_a_previous_work_set() {
    let mut cache = Cache::new();
    let now = Instant::now();
    let mut record = claim(1, 1_000);
    record.credit = vec![98];
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now).stalled,
        None
    );
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND).stalled,
        Some(BOUND)
    );
    record.credit = vec![99];
    // A complete scan sees the changed work even while this claim blocks another resource.
    cache.refresh(Path::new("/ledger"), &[record.clone()], now + BOUND);
    record.credit = vec![98];
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND).stalled,
        None,
        "a nonblocking observed epoch change also retires the old history"
    );
    record.credit = vec![99];
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND).stalled,
        None,
        "the same member count and counters cannot identify the credited work"
    );
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND * 2).stalled,
        Some(BOUND)
    );
    record.credit = vec![98];
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND * 2).stalled,
        None,
        "returning to an observed old set must not resurrect its clock"
    );
}

#[test]
fn unchanged_claim_writes_keep_history_but_a_changed_admission_stamp_does_not() {
    let mut cache = Cache::new();
    let now = Instant::now();
    let mut record = claim(1, 1_000);
    record.credit = vec![98, 99];
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now).stalled,
        None
    );
    record.credit = vec![99, 98, 99];
    record.wrote = Some(1);
    record.need.stepped = Some(1 << 30);
    record.held.as_mut().unwrap().alone = false;
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND).stalled,
        Some(BOUND),
        "canonical unchanged credit, writes and steps preserve same-claim history"
    );
    record.held.as_mut().unwrap().admitted_ms += 1;
    assert_eq!(
        look(&mut cache, &[record.clone()], &record, now + BOUND).stalled,
        None
    );
}

#[test]
fn complete_snapshots_prune_ended_claims_and_keep_live_nonblockers() {
    let mut cache = Cache::new();
    let now = Instant::now();
    let a = claim(1, 1_000);
    let b = claim(2, 1_000);
    let dir = Path::new("/ledger");
    let records = [a.clone(), b.clone()];
    assert_eq!(look(&mut cache, &records, &a, now).stalled, None);
    assert_eq!(look(&mut cache, &records, &b, now).stalled, None);
    // Only A blocks this resource, but the successful full snapshot still contains B.
    assert_eq!(
        look(&mut cache, &records, &a, now + BOUND).stalled,
        Some(BOUND)
    );
    assert_eq!(
        look(&mut cache, &records, &b, now + BOUND).stalled,
        Some(BOUND)
    );
    assert_eq!(
        look(&mut cache, &records, &a, now + KEPT).stalled,
        Some(KEPT)
    );
    assert_eq!(
        look(&mut cache, &records, &b, now + KEPT).stalled,
        Some(KEPT),
        "a live nonblocking claim is not expired inside an observed ledger"
    );
    cache.refresh(dir, &[b.clone()], now + KEPT);
    assert_eq!(
        cache.ledgers[dir].claims.len(),
        1,
        "ended A is not retained"
    );
    assert!(cache.ledgers[dir].claims.contains_key(&b.order()));

    // Repeated new claims cannot accumulate in a continuously observed ledger.
    for seq in 3..103 {
        let current = claim(seq, 1_000 + seq);
        cache.refresh(dir, &[current], now + KEPT);
        assert_eq!(cache.ledgers[dir].claims.len(), 1);
    }
    cache.refresh(dir, &[], now + KEPT);
    assert!(!cache.ledgers.contains_key(dir));
    cache.refresh(dir, &[a], now + KEPT);
    cache.refresh(Path::new("/other"), &[b], now + KEPT * 2);
    assert!(
        !cache.ledgers.contains_key(dir),
        "unobserved ledger expires"
    );
}

#[test]
fn unknown_work_and_nested_waits_keep_their_existing_meaning() {
    use aterm_toolchain::waits::{Stuck, Waiting};

    let mut cache = Cache::new();
    let now = Instant::now();
    let record = claim(1, 1_000);
    let dir = Path::new("/ledger");
    cache.refresh(dir, &[record.clone()], now);
    let mut holders = [holder(&record)];
    cache.read(dir, &mut holders, Some(&idle()), &[], now, BOUND);
    cache.read(dir, &mut holders, None, &[], now + BOUND, BOUND);
    assert_eq!(holders[0].stalled, None, "an unread tree proves no stall");
    cache.read(dir, &mut holders, Some(&idle()), &[], now + BOUND, BOUND);
    assert_eq!(holders[0].stalled, None, "the unknown read reset history");

    let behind = [Waiting::said(PID, 0, "waiting", None)];
    cache.read(
        dir,
        &mut holders,
        Some(&idle()),
        &behind,
        now + BOUND * 2,
        BOUND,
    );
    assert_eq!(holders[0].stalled, None, "a working dependency is progress");

    let span = BOUND * 3;
    let nested = [Waiting::said(
        PID,
        0,
        "waiting",
        Some(Stuck {
            holders: vec![98],
            span,
        }),
    )];
    let fresh = claim(2, 2_000);
    cache.refresh(dir, &[fresh.clone()], now + BOUND * 2);
    holders = [holder(&fresh)];
    cache.read(
        dir,
        &mut holders,
        Some(&idle()),
        &nested,
        now + BOUND * 2,
        BOUND,
    );
    assert_eq!(
        holders[0].stalled,
        Some(span),
        "nested stuck authority remains"
    );
}
