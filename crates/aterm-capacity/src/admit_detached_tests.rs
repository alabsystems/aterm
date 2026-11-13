// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native lock custody is tested by retained-claim tests. These controls exercise the
//! production admission arithmetic against a fixed process sweep with a reused owner PID.

use super::*;
use crate::{Accounting, Fixed, Proc};

const OWNER: u32 = 10;
const CREDIT: u32 = 20;
const CONTENDER: u32 = 30;
const MARGIN: u64 = 128 * 1024 * 1024;

fn held(accounting: Accounting) -> Record {
    Record {
        accounting,
        pid: OWNER,
        start_ms: 1,
        seq: 0,
        checkout: "/original".into(),
        need: Need::new("original GUI memory").mem_gib(2),
        timing: crate::TimingEvidence::default(),
        held: Some(Held {
            admitted_ms: 1,
            cores: 0,
            alone: false,
        }),
        credit: vec![CREDIT],
        wrote: None,
        next_ms: None,
    }
}

fn decide_with(machine: &Fixed, records: &[Record]) -> Result<Grant, Shortfall> {
    let procs = machine.procs();
    let records = counted(records.iter().collect(), CONTENDER, procs.as_ref(), None);
    let roots: Vec<_> = records
        .iter()
        .flat_map(|record| std::iter::once(record.pid).chain(record.credit.iter().copied()))
        .collect();
    let trees = procs
        .as_ref()
        .map(|procs| procs.trees(&roots))
        .unwrap_or_default();
    let others = others(
        &records,
        records.iter().map(|record| record.pid).collect(),
        &trees,
    );
    decide(
        &Need::new("independent contender").mem_gib(2).margin(MARGIN),
        (2, CONTENDER, 0),
        &machine.read(None),
        &others,
        2,
    )
}

fn fixed(procs: Procs) -> Fixed {
    let machine = Fixed::new(Reading::of_gib(8, 3, 4));
    machine.set_procs(Some(procs));
    machine
}

fn assert_full_budget_refused(machine: &Fixed, record: Record) {
    let short = decide_with(machine, &[record]).expect_err("the original 2 GiB is still reserved");
    assert_eq!(
        short.shorts,
        vec![Short::Memory {
            need: 2 * GIB,
            margin: MARGIN,
            available: 3 * GIB,
            reserved: 2 * GIB,
            passes: None,
            stuck_ahead: 0,
            stuck: false,
        }]
    );
    assert!(
        decide_with(machine, &[]).is_ok(),
        "releasing the original reservation makes room"
    );
}

#[test]
fn reused_holder_and_credit_pids_cannot_discount_detached_memory() {
    let procs = Procs::of(vec![
        Proc {
            pid: OWNER,
            ppid: 1,
            charged: 2 * GIB,
            footprint: 2 * GIB,
            ..Proc::default()
        },
        Proc {
            pid: CREDIT,
            ppid: 1,
            charged: 2 * GIB,
            footprint: 2 * GIB,
            ..Proc::default()
        },
        Proc {
            pid: CONTENDER,
            ppid: 1,
            ..Proc::default()
        },
    ]);
    let machine = fixed(procs.clone());
    // Negative control: these numeric process samples consume an ordinary holder's
    // reservation, demonstrating why that accounting is unsafe after guard transfer.
    assert!(decide_with(&machine, &[held(Accounting::Holder)]).is_ok());
    let detached = held(Accounting::Detached);
    let trees = procs.trees(&[OWNER, CREDIT]);
    assert_eq!(
        reserved(std::slice::from_ref(&detached), Some(&trees), &[])[0].mem,
        2 * GIB
    );
    assert_full_budget_refused(&machine, detached);
}

#[test]
fn reused_owner_ancestor_cannot_exclude_or_cover_detached_memory() {
    let procs = Procs::of(vec![
        Proc {
            pid: OWNER,
            ppid: 1,
            ..Proc::default()
        },
        Proc {
            pid: CONTENDER,
            ppid: OWNER,
            ..Proc::default()
        },
    ]);
    let machine = fixed(procs.clone());
    let ordinary = held(Accounting::Holder);
    assert!(counted(vec![&ordinary], CONTENDER, Some(&procs), None).is_empty());
    assert!(decide_with(&machine, &[ordinary]).is_ok());
    let mut detached = held(Accounting::Detached);
    assert_eq!(
        counted(vec![&detached], CONTENDER, Some(&procs), None).len(),
        1
    );
    assert_full_budget_refused(&machine, detached.clone());
    // Even an inconsistent old coverage flag in an observational Record supplies no
    // ancestor or credit authority when its accounting is detached. The transfer API
    // itself accepts only a noncovering ordinary memory claim.
    detached.need.covers = true;
    assert!(covering_of(CONTENDER, &[&detached], Some(&procs)).is_none());
}

#[test]
fn whole_cohort_reservation_keeps_its_existing_conservative_accounting() {
    let machine = fixed(Procs::of(vec![
        Proc {
            pid: OWNER,
            ppid: 1,
            charged: 2 * GIB,
            footprint: 2 * GIB,
            ..Proc::default()
        },
        Proc {
            pid: CREDIT,
            ppid: 1,
            charged: 2 * GIB,
            footprint: 2 * GIB,
            ..Proc::default()
        },
        Proc {
            pid: CONTENDER,
            ppid: OWNER,
            ..Proc::default()
        },
    ]));
    assert_full_budget_refused(&machine, held(Accounting::Whole));
}
