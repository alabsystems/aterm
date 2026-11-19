// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The memory ceiling's `restart` record (HX-15m, D18, D32), driven through the same rig as the
//! stream's own tests: a real claim on a real connection, the shipping lane, and the window
//! host's worker for the tab (`harness_host::tests::memory::worker_over`), whose claim reads and
//! memory restart are the ones the window wires, over the rig's store. The claimant is the
//! shared stand-in, which leads no tab here, so the restart is told and not watched;
//! `harness_host_memory_tests.rs` ends an unobeyed one on a real pty.

use aterm_agent::supervise::IdleHost;
use aterm_agent::supervise::policy::turn_end::Restart;
use aterm_phase::Program;

use super::*;
use crate::restart_act::RESTART_EVENT;

/// The payloads of `sid`'s `restart` rows, oldest first.
fn restart_rows(rig: &Rig, sid: &str) -> Vec<String> {
    rig.service
        .handle(sid)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .view_copy(None, 0)
        .rows()
        .iter()
        .filter(|row| row.kind == RESTART_EVENT)
        .map(|row| row.payload.to_string())
        .collect()
}

/// HX-15m EXIT TEST: THE MEMORY CEILING RESTARTS A RELAUNCH CLAIMANT THROUGH THE RESTART RECORD
/// (D18, D32). A Muse tab whose harness holds a contract 4 claim of `relaunch` on a real
/// connection, its record stream open: the window host's worker counts the harness as the tab's
/// restart lane (Muse has no built-in one), so the policy's memory restart is decided for it and
/// is the harness's, and asked for it the worker writes `restart cause=memory` through
/// `record_restart`: the open stream carries it as a `restart` record with cause `memory`, the
/// row stays for subscribers, and the worker answers `relaunched:scoped-memory`.
/// NEGATIVE CONTROL: the same harness's claim placed again with `state` alone (no `relaunch`) is
/// no restart lane: the memory restart is not made (no answer), no row is written and the stream
/// hears nothing. Before HX-15m the worker answered no restart for any scoped claim and wrote
/// nothing, as the control does.
#[test]
fn the_memory_ceiling_restarts_a_relaunch_claimant_through_the_restart_record() {
    let rig = Rig::new(2);
    let handle = rig.service.handle(ONE);
    let mut client = rig.claimant(ONE, "relaunch");
    let id = client.id();
    client.open(ONE);
    let worker =
        crate::harness_host::tests::memory::worker_over(&rig.service.store, ONE, Program::Muse);
    assert!(
        IdleHost::can_restart(&worker),
        "the harness holding relaunch is the tab's restart lane"
    );
    assert!(
        IdleHost::scoped_restart(&worker, &Restart::Memory),
        "the memory restart is the harness's, not a built-in one"
    );

    assert_eq!(
        worker.restart_once(&Restart::Memory).as_deref(),
        Some(crate::restart_act::MEMORY_TOLD)
    );
    let restart = client.own();
    assert_eq!(
        (restart.seq, restart.body),
        (
            client.opening_end() + 1,
            Body::Restart {
                cause: "memory".to_string(),
                account: None,
                model: None,
            }
        ),
        "the open stream carries the restart record"
    );
    assert_eq!(
        restart_rows(&rig, ONE),
        ["cause=memory"],
        "the row stays for subscribers"
    );

    // NEGATIVE CONTROL: the claim placed again on the stream's own connection without
    // `relaunch`.
    place(
        &handle,
        4,
        &[Duty::State],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a narrower grant");
    assert_eq!(client.own().body, Body::Policy, "the grant moved");
    assert!(!IdleHost::can_restart(&worker), "no restart lane is left");
    assert!(!IdleHost::scoped_restart(&worker, &Restart::Memory));
    assert_eq!(worker.restart_once(&Restart::Memory), None);
    for seq in 1..=2 {
        assert_eq!(client.record(seq, Body::Beat), (seq, None));
    }
    assert!(
        client.heard.is_empty(),
        "nothing more is sent: {:?}",
        client.heard
    );
    assert_eq!(
        restart_rows(&rig, ONE),
        ["cause=memory"],
        "no second row is written"
    );
}
