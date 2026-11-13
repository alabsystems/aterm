// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CORES-NO-CYCLE ([`super::ranked_grant`]): a waiter for memory reserves no cores against a
//! process that holds memory and asks for its subprocesses' cores.

use super::*;

fn record(pid: u32, start_ms: u64, need: Need, held: Option<u32>) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid,
        start_ms,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: held.map(|cores| crate::Held {
            admitted_ms: start_ms,
            cores,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

fn cores14_at(load: f64) -> Reading {
    Reading {
        load1: Some(load),
        load5: Some(load),
        ..Reading::of_gib(48, 40, 14)
    }
}

/// CORES-NO-CYCLE: a process that holds memory and then asks for its subprocesses' cores is
/// never held back by an earlier waiter that still waits for memory, since that waiter may wait
/// for the very memory the process holds (2026-11-10: an hour's stall of every verify on this
/// Mac). An earlier waiter of cores alone still reserves them, the negative control.
#[test]
fn a_waiter_for_memory_reserves_no_cores_against_a_holder() {
    let other = |r| Other {
        record: r,
        used: TreeUse::default(),
        credited: TreeUse::default(),
    };
    let idle = cores14_at(0.0);
    let holds = record(20, 0, Need::new("verify test").mem(3 * GIB), Some(0));
    let compile = record(21, 1, Need::new("test compile").mem(106 * GIB), None);
    let test = record(22, 2, Need::new("targo test").mem(13 * GIB).cores(14), None);
    let subprocesses = Need::new("verify test subprocesses").cores(14);
    let granted = decide(
        &subprocesses,
        (3, 20, 1),
        &idle,
        &[other(&holds), other(&compile), other(&test)],
        4,
    )
    .expect("a memory waiter's cores are no reservation the holder waits on")
    .cores;
    assert!(granted >= 1, "granted {granted}");
    let cores_only = record(23, 2, Need::new("cores waiter").cores(14), None);
    assert!(
        decide(
            &subprocesses,
            (3, 20, 1),
            &idle,
            &[other(&holds), other(&cores_only)],
            4
        )
        .is_err_and(|s| s.shorts.contains(&Short::Cores)),
        "an earlier waiter of cores alone keeps its reservation"
    );
}
