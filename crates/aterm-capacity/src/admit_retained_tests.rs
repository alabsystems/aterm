// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained funding protects a turn only while its counted ends and qualified sponsor
//! processes can deliver it. Unclaimed memory must not make a descendant wait on its holder.

use super::*;
use crate::reading::Proc;

fn claim(pid: u32, asked: u64, need: Need, held: bool) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid,
        start_ms: asked,
        laid_at: None,
        seq: 0,
        checkout: "/release".to_owned(),
        need,
        held: held.then_some(Held {
            admitted_ms: asked,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

fn process(pid: u32, parent: u32, charged: u64) -> Proc {
    Proc {
        pid,
        ppid: parent,
        start: 1000 + u64::from(pid),
        charged,
        footprint: charged,
        written: 0,
        cpu_ns: 0,
        runnable_ns: 0,
    }
}

fn reading(available: u64) -> Reading {
    Reading {
        available: Some(available),
        ..Reading::of_gib(64, 0, 18)
    }
}

/// Build the proof through the same funding rule that records a real terminal sponsor.
fn retained_lead() -> Record {
    let mut lead = claim(
        80,
        2,
        Need::new("retained final link")
            .rank(crate::Rank::Release)
            .mem_gib(24)
            .margin(2 * GIB)
            .critical_path(),
        false,
    );
    let sponsor = claim(
        50,
        1,
        Need::new("terminal sponsor")
            .rank(crate::Rank::Release)
            .mem_gib(30)
            .critical_path(),
        true,
    );
    let procs = Procs::of(vec![
        process(1, 0, 0),
        process(50, 1, 20 * GIB),
        process(80, 1, 0),
    ]);
    let records = [&sponsor, &lead];
    let observed = look_read(&records, &lead, Some(&procs), &reading(10 * GIB));
    let proof = funded_lead_reservation(&lead, &records, &procs, &observed.wide, Some(10 * GIB))
        .expect("the terminal sponsor funds the full shortage");
    assert_eq!((proof.needed, proof.holdback), (26 * GIB, 0));
    assert_eq!(proof.sponsors.len(), 1);
    assert_eq!(proof.sponsors[0].order, sponsor.order());
    assert_eq!(proof.sponsors[0].start, 1050);
    lead.need.lead_reservation = Some(proof);
    lead
}

/// Keep the complete ledger observation through both the decision and the public listing.
/// Plain hand-built `Other`s would lose the sponsor identity and descendant exclusions.
fn judge(
    asker: &Record,
    records: &[Record],
    procs: &Procs,
    available: u64,
) -> (Result<Grant, Shortfall>, Reckoned) {
    let refs: Vec<&Record> = records.iter().collect();
    let reading = reading(available);
    let observed = look_read(&refs, asker, Some(procs), &reading);
    let own = observed.wide.reckoned[&asker.order()];
    let seen = others(
        &observed.counted,
        observed.used.claimants,
        &observed.used.trees,
        &observed.used.lines,
        &observed.wide,
    );
    let decision = decide_with(
        &asker.need,
        asker.order(),
        &reading,
        &seen,
        10,
        DiskRule::Reserved,
        Some(own),
    );
    let listed = spared(records, Some(&observed.used.trees), Some(procs), &reading);
    let at = records
        .iter()
        .position(|record| record.order() == asker.order())
        .expect("the actual asking claim is in the ledger");
    assert_eq!(listed[at], own.standing, "the listing agrees with the look");
    (decision, own)
}

fn assert_stuck(decision: &Result<Grant, Shortfall>, expected: bool) {
    let short = decision
        .as_ref()
        .expect_err("the lead is not funded by free room");
    assert!(
        matches!(short.shorts.as_slice(), [Short::Memory { stuck, .. }] if *stuck == expected),
        "{short:?}"
    );
}

/// KI-998: process 40 has no claim and joins child 41. Its 24 GiB cannot fund a retained
/// lead's turn, so the lead must not keep the child behind it on 10 GiB available.
#[test]
fn a_retained_lead_short_of_unclaimed_memory_does_not_hold_its_holders_child() {
    let lead = retained_lead();
    let child = claim(41, 3, Need::new("joined child").mem_gib(4), false);
    let procs = Procs::of(vec![
        process(1, 0, 0),
        process(40, 1, 24 * GIB),
        process(41, 40, 0),
        process(80, 1, 0),
    ]);
    let records = [lead.clone(), child.clone()];
    let (decision, known) = judge(&lead, &records, &procs, 10 * GIB);
    assert_eq!(known.remains, 0);
    assert_eq!(known.standing, Some(Spared::Stuck));
    assert_stuck(&decision, true);
    assert!(judge(&child, &records, &procs, 10 * GIB).0.is_ok());

    assert!(judge(&child, &[child.clone()], &procs, 10 * GIB).0.is_ok());
    let mut ordinary = lead;
    ordinary.need.lead_reservation = None;
    let without_retention = [ordinary.clone(), child.clone()];
    assert_stuck(
        &judge(&ordinary, &without_retention, &procs, 10 * GIB).0,
        true,
    );
    assert!(
        judge(&child, &without_retention, &procs, 10 * GIB)
            .0
            .is_ok()
    );
}

/// The same wait can be under the original sponsor after its claim ends. Its matching birth
/// and 20 GiB do not establish REMAINS once its descendant is itself waiting to run.
#[test]
fn an_ended_sponsors_waiting_child_can_pass_a_retained_lead() {
    let lead = retained_lead();
    let child = claim(52, 3, Need::new("sponsor child").mem_gib(4), false);
    let procs = Procs::of(vec![
        process(1, 0, 0),
        process(50, 1, 20 * GIB),
        process(52, 50, 0),
        process(80, 1, 0),
    ]);
    let records = [lead.clone(), child.clone()];
    let (decision, known) = judge(&lead, &records, &procs, 10 * GIB);
    assert_eq!(known.remains, 0, "the sponsor may be joining the child");
    assert_eq!(known.standing, Some(Spared::Stuck));
    assert_stuck(&decision, true);
    assert!(judge(&child, &records, &procs, 10 * GIB).0.is_ok());
}

/// Valid retained funding still reserves the turn. The exact 16 GiB shortage is funded by
/// the ended sponsor alone, or by 6 GiB of it plus a held 10 GiB end; one byte less is STUCK.
/// REMAINS never admits the lead against the 10 GiB actually available now.
#[test]
fn a_retained_turn_requires_its_full_shortage_from_qualified_remains_and_counted_ends() {
    let lead = retained_lead();
    let later = claim(70, 3, Need::new("later unit").mem_gib(4), false);
    for held_gib in [0, 10] {
        for missing in [0, 1] {
            let sponsor_bytes = (16 - held_gib) * GIB - missing;
            let procs = Procs::of(vec![
                process(1, 0, 0),
                process(50, 1, sponsor_bytes),
                process(60, 1, held_gib * GIB),
                process(70, 1, 0),
                process(80, 1, 0),
            ]);
            let mut records = vec![lead.clone(), later.clone()];
            if held_gib > 0 {
                records.push(claim(
                    60,
                    0,
                    Need::new("counted holder").mem_gib(held_gib),
                    true,
                ));
            }
            let (decision, known) = judge(&lead, &records, &procs, 10 * GIB);
            let funded = missing == 0;
            assert_eq!(known.remains, sponsor_bytes);
            assert_eq!(
                known.standing,
                Some(if funded {
                    Spared::Within(0)
                } else {
                    Spared::Stuck
                }),
                "held {held_gib} GiB, missing {missing} byte"
            );
            assert_stuck(&decision, !funded);
            assert_eq!(
                judge(&later, &records, &procs, 10 * GIB).0.is_err(),
                funded,
                "only a fully funded turn keeps the later unit behind it"
            );
        }
    }
}

/// Holdback protects backfill beside a lead whose own parent is hidden from its look. It is
/// not another admission demand: this real 34 GiB turn is funded even though its conservative
/// 17 GiB holdback makes the virtual protected shortage larger than every counted end.
#[test]
fn a_funded_retained_lead_keeps_its_sponsors_memory_despite_its_parents_holdback() {
    let parent = claim(
        20,
        0,
        Need::new("slice driver")
            .rank(crate::Rank::Release)
            .mem_gib(13),
        true,
    );
    let tree = claim(
        10,
        1,
        Need::new("tree driver")
            .rank(crate::Rank::Release)
            .mem_gib(4),
        true,
    );
    let sponsor = claim(
        30,
        2,
        Need::new("terminal sponsor")
            .rank(crate::Rank::Release)
            .mem_gib(20)
            .critical_path(),
        true,
    );
    let mut lead = claim(
        21,
        3,
        Need::new("native final link")
            .rank(crate::Rank::Release)
            .mem_gib(30)
            .margin(4 * GIB)
            .critical_path(),
        false,
    );
    let mut processes = vec![
        process(1, 0, 0),
        process(10, 1, 0),
        process(20, 1, 0),
        process(21, 20, 0),
        process(30, 1, 20 * GIB),
    ];
    processes.extend((100..108).map(|pid| process(pid, 10, 0)));
    let procs = Procs::of(processes);
    let proving = [&parent, &tree, &sponsor, &lead];
    let observed = look_read(&proving, &lead, Some(&procs), &reading(30 * GIB));
    let proof = funded_lead_reservation(&lead, &proving, &procs, &observed.wide, Some(30 * GIB))
        .expect("the terminal sponsor funds the lead beside its own parent");
    assert_eq!((proof.needed, proof.holdback), (8 * GIB, 17 * GIB));
    lead.need.lead_reservation = Some(proof);

    for pid in 100..108 {
        let unit = claim(pid, 4, Need::new("temporal tree unit").mem_gib(4), false);
        let records = [parent.clone(), tree.clone(), lead.clone(), unit.clone()];
        let (decision, known) = judge(&lead, &records, &procs, 30 * GIB);
        assert_eq!(known.remains, 20 * GIB);
        assert_eq!(known.standing, Some(Spared::Within(0)));
        assert_stuck(&decision, false);
        assert!(
            judge(&unit, &records, &procs, 30 * GIB).0.is_err(),
            "temporal unit {pid} cannot refill the sponsor's funded memory"
        );

        let without_lead = [parent.clone(), tree.clone(), unit.clone()];
        assert!(judge(&unit, &without_lead, &procs, 30 * GIB).0.is_ok());
    }
}
