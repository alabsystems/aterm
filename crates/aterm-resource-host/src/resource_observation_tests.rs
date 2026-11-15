// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::resource_admission::{AccountingScope, CpuTimes};

fn binding() -> Binding {
    Binding {
        session: "tacde112200".into(),
        attempt: [7; 16],
    }
}

#[test]
fn channel_handoff_and_owner_work_keep_observation_demand_until_settled() {
    let binding = binding();
    let mut ledger = Ledger::default();
    ledger.queued(&binding);
    ledger.owner_state(None, Vec::new());
    assert_eq!(
        ledger.sessions_outstanding().as_slice(),
        std::slice::from_ref(&binding.session),
        "a queued actor message has not been processed"
    );
    ledger.owner_state(Some(&binding), [binding.session.clone()]);
    assert_eq!(
        ledger.sessions_outstanding().as_slice(),
        std::slice::from_ref(&binding.session),
        "ownership transfers without an idle gap"
    );
    ledger.owner_state(None, Vec::new());
    assert!(ledger.sessions_outstanding().is_empty());
    ledger.queued(&binding);
    ledger.queued(&binding);
    ledger.owner_state(Some(&binding), Vec::new());
    assert_eq!(
        ledger.sessions_outstanding().as_slice(),
        std::slice::from_ref(&binding.session)
    );
    ledger.unqueue(&binding);
    assert!(ledger.sessions_outstanding().is_empty());
}
fn accounting(cpu: Option<u64>) -> AccountingObservation {
    AccountingObservation {
        scope: AccountingScope::ChildAndWaitedDescendantsV1,
        cpu_times: cpu.map(|user_ns| CpuTimes {
            user_ns,
            system_ns: 0,
        }),
        peak_rss_bytes: None,
    }
}

#[test]
fn an_unread_birth_remains_an_owned_unknown_until_the_original_wait() {
    let mut ledger = Ledger::default();
    ledger.born(&binding(), 200, None);
    assert_eq!(ledger.unproven.get(&200), Some(&binding().session));
    assert!(ledger.originals.is_empty());
    ledger.reaped(200, accounting(Some(90)), None);
    assert!(ledger.unproven.is_empty());
    assert_eq!(
        ledger.completed.get(&binding().session),
        Some(&(90, 1)),
        "the original wait's exact aggregate and missing generation belong to its authenticated session"
    );
    assert_eq!(
        ledger.lost_origins, 1,
        "missing process generation is explicit coverage loss"
    );
    assert!(
        ledger.terminals.is_empty(),
        "an unknown generation never becomes a fabricated terminal"
    );
    ledger.reaped(200, accounting(Some(90)), None);
    assert_eq!(
        ledger.completed.get(&binding().session),
        Some(&(90, 1)),
        "a repeated owning-wait result adds nothing"
    );
}

#[test]
fn a_final_unreaped_child_can_recover_its_missing_birth_without_inventing_cpu() {
    let mut ledger = Ledger::default();
    ledger.born(&binding(), 200, None);
    ledger.reaped(
        200,
        accounting(Some(90)),
        Some(aterm_sysprobe::ProcessReading {
            pid: 200,
            start: Some(20),
            cpu_ns: Some(30),
            child_cpu_ns: Some(60),
            ..Default::default()
        }),
    );
    assert!(ledger.unproven.is_empty());
    assert_eq!(ledger.lost_origins, 0);
    let terminal = ledger.terminals.front().unwrap();
    assert_eq!(
        terminal.process,
        ProcessKey {
            pid: 200,
            start: 20
        }
    );
    assert_eq!(
        (terminal.cpu_ns, terminal.child_cpu_ns, terminal.wait_cpu_ns),
        (Some(30), Some(60), Some(90))
    );
    assert_eq!(terminal.session, binding().session);
}

#[test]
fn repeated_binding_cannot_erase_a_proved_original_when_a_later_read_is_unavailable() {
    let mut ledger = Ledger::default();
    ledger.born(&binding(), 200, Some(20));
    ledger.born(&binding(), 200, None);
    assert_eq!(
        ledger.originals[&200].0,
        ProcessKey {
            pid: 200,
            start: 20
        }
    );
    assert!(ledger.unproven.is_empty());
    ledger.reaped(200, accounting(None), None);
    assert_eq!(ledger.completed[&binding().session], (0, 1));
    assert_eq!(ledger.terminals.front().unwrap().cpu_ns, None);
}

#[test]
fn a_guarded_snapshot_recovers_only_the_originals_it_still_owns() {
    let mut ledger = Ledger::default();
    ledger.born(&binding(), 200, None);
    ledger.recover(&[
        aterm_sysprobe::ProcessReading {
            pid: 200,
            start: Some(20),
            ..Default::default()
        },
        aterm_sysprobe::ProcessReading {
            pid: 300,
            start: Some(30),
            ..Default::default()
        },
    ]);
    assert!(ledger.unproven.is_empty());
    assert_eq!(ledger.originals.len(), 1);
    assert_eq!(
        ledger.originals[&200].0,
        ProcessKey {
            pid: 200,
            start: 20
        }
    );
    assert_eq!(ledger.lost_origins, 0);
}

#[test]
fn a_native_snapshot_reads_only_the_custodian_and_its_owned_originals() {
    let mut ledger = Ledger::default();
    ledger.born(&binding(), 200, Some(20));
    ledger.born(&binding(), 300, None);
    let mut read = Vec::new();
    let usage = owned_usage(&ledger, 100, |pid| {
        read.push(pid);
        Some(aterm_sysprobe::ProcessReading {
            pid,
            ..Default::default()
        })
    });
    assert_eq!(read, [100, 200, 300]);
    assert_eq!(usage.len(), 3);
}
