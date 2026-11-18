// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lead may not wait for the end of work that depends, through ancestry or credit, on
//! the waiter it would hold back. Parent dependencies close transitively over held claims.

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

fn processes(rows: &[(u32, u32, u64)]) -> Procs {
    Procs::of(
        rows.iter()
            .map(|&(pid, parent, used)| Proc {
                pid,
                ppid: parent,
                start: 1000 + u64::from(pid),
                charged: used,
                footprint: used,
                written: 0,
                cpu_ns: 0,
                runnable_ns: 0,
            })
            .collect(),
    )
}

/// Preserve the actual whole-ledger observation through admission and the public listing.
fn look(
    asker: &Record,
    records: &[Record],
    procs: &Procs,
) -> (Result<Grant, Shortfall>, Vec<Order>, Option<Spared>) {
    let reading = Reading::of_gib(64, 14, 18);
    let refs: Vec<&Record> = records.iter().collect();
    let observed = look_read(&refs, asker, Some(procs), &reading);
    let own = observed.wide.reckoned[&asker.order()];
    let counted = observed
        .counted
        .iter()
        .map(|record| record.order())
        .collect();
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
        .expect("the asking claim is part of this ledger");
    assert_eq!(listed[at], own.standing, "listing and own look agree");
    (decision, counted, own.standing)
}

/// KI-999: Q (51) credits W (70), while Q's holder is under S (50). S may join Q, and Q
/// may join W, so S's used 20 GiB cannot make the 29 GiB lead sure for W. Without that
/// transitive parent, the lead is incorrectly sure on 14 + 20 - 2 GiB and spares only 3 GiB.
#[test]
fn a_lead_cannot_wait_for_the_ancestor_of_a_claim_that_credits_its_waiter() {
    let ancestor = claim(50, 0, Need::new("ancestor work").mem_gib(30), true);
    let mut parent = claim(51, 1, Need::new("credited work").mem_gib(2), true);
    parent.credit = vec![70];
    let lead = claim(
        60,
        2,
        Need::new("release lead")
            .rank(crate::Rank::Release)
            .mem_gib(29)
            .leads(),
        false,
    );
    let waiter = claim(70, 3, Need::new("credited waiter").mem_gib(4), false);
    let procs = processes(&[
        (1, 0, 0),
        (2, 1, 0),
        (50, 2, 20 * GIB),
        (51, 50, 0),
        (60, 2, 0),
        (70, 1, 0),
    ]);
    let records = [
        ancestor.clone(),
        parent.clone(),
        lead.clone(),
        waiter.clone(),
    ];
    let (decision, counted, _) = look(&waiter, &records, &procs);
    assert!(
        !counted.contains(&parent.order()),
        "credit makes W inside Q"
    );
    assert!(
        counted.contains(&ancestor.order()),
        "S still reserves its unused 10 GiB"
    );
    assert!(
        !counted.contains(&lead.order()),
        "the lead waits for W's transitive parent"
    );
    assert!(
        decision.is_ok(),
        "the 4 GiB beside S belongs to W: {decision:?}"
    );

    let (_, _, standing) = look(&lead, &records, &procs);
    assert_eq!(standing, Some(Spared::Within(3 * GIB)));
    let without_lead = [ancestor, parent, waiter.clone()];
    assert!(look(&waiter, &without_lead, &procs).0.is_ok());
}

/// More than one promotion is needed: Q credits W; S is Q's ancestor; S credits process
/// 41, which is under R (40). R's 20 GiB is not independent funding either. Cutting either
/// dependency restores a genuinely sure lead, whose passing bound then holds W back.
#[test]
fn parent_dependencies_close_over_multiple_ancestry_and_credit_hops_in_any_record_order() {
    let mut tree_holder = claim(40, 0, Need::new("credited tree's holder").mem_gib(20), true);
    let mut ancestor = claim(50, 1, Need::new("ancestor work").mem_gib(30), true);
    let mut parent = claim(51, 2, Need::new("credited work").mem_gib(2), true);
    parent.credit = vec![70];
    let waiter = claim(70, 4, Need::new("credited waiter").mem_gib(4), false);
    for (under_ancestor, credits_tree, cycle, lead_parent) in [
        (true, true, false, 2),
        (false, true, false, 2),
        (true, false, false, 2),
        (true, true, true, 2),
        (true, true, false, 50),
    ] {
        ancestor.credit = if credits_tree { vec![41] } else { Vec::new() };
        tree_holder.credit = if cycle { vec![51] } else { Vec::new() };
        // Under S the lead's own look leaves S out. Its real 32 GiB envelope then fits
        // exactly only if R's 20 GiB end is independent; S must still transmit R's dependency
        // before that ancestry filter. Outside S, a 21 GiB envelope needs both promotions.
        let lead = claim(
            60,
            3,
            Need::new("release lead")
                .rank(crate::Rank::Release)
                .mem_gib(if lead_parent == 50 { 31 } else { 20 })
                .margin(GIB)
                .leads(),
            false,
        );
        let procs = processes(&[
            (1, 0, 0),
            (2, 1, 0),
            (40, 2, 20 * GIB),
            (41, 40, 0),
            (50, 2, 20 * GIB),
            (51, if under_ancestor { 50 } else { 2 }, 0),
            (60, lead_parent, 0),
            (70, 1, 0),
        ]);
        let records = [
            tree_holder.clone(),
            ancestor.clone(),
            parent.clone(),
            lead.clone(),
            waiter.clone(),
        ];
        let depends = under_ancestor && credits_tree;
        for order in [[0, 1, 2, 3, 4], [2, 1, 0, 3, 4], [4, 3, 2, 1, 0]] {
            let reordered: Vec<Record> = order.iter().map(|&i| records[i].clone()).collect();
            let (decision, counted, _) = look(&waiter, &reordered, &procs);
            assert_eq!(
                counted.contains(&lead.order()),
                !depends,
                "ancestry {under_ancestor}, credit {credits_tree}, cycle {cycle}, lead parent {lead_parent}, order {order:?}"
            );
            assert_eq!(
                decision.is_ok(),
                depends,
                "ancestry {under_ancestor}, credit {credits_tree}, cycle {cycle}, lead parent {lead_parent}, order {order:?}: {decision:?}"
            );
            if !depends {
                let short = decision.expect_err("the independent end funds the lead");
                assert!(
                    matches!(short.shorts.as_slice(), [Short::Memory { passes: Some(p), .. }] if *p < 4 * GIB),
                    "W waits specifically for the lead's passing bound: {short:?}"
                );
            }
        }
        let without_lead = [
            tree_holder.clone(),
            ancestor.clone(),
            parent.clone(),
            waiter.clone(),
        ];
        assert!(look(&waiter, &without_lead, &procs).0.is_ok());
    }
}

/// An independently accounted reservation is not its creator's process tree. Its numeric
/// creator PID matching W must not turn its 18 cores into parent cores and hide a sure lead.
#[test]
fn independent_reservations_do_not_become_process_parents_through_their_creator_pid() {
    let mut parent = claim(51, 0, Need::new("credited work").mem_gib(2), true);
    parent.credit = vec![70];
    let lead = claim(
        60,
        1,
        Need::new("release lead")
            .rank(crate::Rank::Release)
            .mem_gib(12)
            .cores(1)
            .leads(),
        false,
    );
    let waiter = claim(70, 2, Need::new("credited waiter").mem_gib(4), false);
    let procs = processes(&[(1, 0, 0), (51, 1, 0), (60, 1, 0), (70, 1, 0)]);
    for accounting in [
        crate::Accounting::Whole,
        crate::Accounting::Detached,
        crate::Accounting::CohortOwned,
    ] {
        let mut independent = claim(70, 0, Need::new("independent CPU work").cores(18), true);
        independent.accounting = accounting;
        independent.held.as_mut().expect("held reservation").cores = 18;
        let records = [
            parent.clone(),
            independent.clone(),
            lead.clone(),
            waiter.clone(),
        ];
        let (decision, counted, _) = look(&waiter, &records, &procs);
        assert!(!counted.contains(&parent.order()));
        assert!(counted.contains(&independent.order()));
        assert!(counted.contains(&lead.order()), "{accounting:?}");
        let short = decision.expect_err("W waits for the independent reservation and lead");
        assert!(
            matches!(short.shorts.as_slice(), [Short::Memory { .. }]),
            "{accounting:?}: {short:?}"
        );
        let without_lead = [parent.clone(), independent, waiter.clone()];
        assert!(
            look(&waiter, &without_lead, &procs).0.is_ok(),
            "{accounting:?}"
        );
    }
}
