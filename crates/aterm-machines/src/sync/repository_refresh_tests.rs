// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scope(byte: u8) -> RefreshScope {
    RefreshScope::new([byte; 32])
}

fn pass(memory: &Arc<Memory>, now: u64) -> (RefreshObservation, Arc<RepositoryPass>) {
    let access = RepositoryPass::memory(memory, now);
    (access.observed, access.pass.expect("the lease is free"))
}

/// A request that names inputs which moved before its runner admitted it is admitted for the
/// inputs the runner validated: it owns a wave of that scope under a later input, and once
/// that wave is complete the same stale request is fulfilled by it rather than run again.
/// NEGATIVE CONTROL: the refusal of a request whose scope differs from the runner's (0.200.0)
/// makes the first admission an error; a wave that kept the request's scope fails the scope
/// assertion.
#[test]
fn a_request_made_before_its_inputs_moved_is_admitted_for_the_inputs_there_are() {
    let memory = Arc::new(Memory::default());
    let (old, new) = (scope(1), scope(2));
    let (observed, first) = pass(&memory, 1_000);
    let Admission::Refresh(ticket) = first
        .admit(
            RefreshRequest::ensure(Some(old), observed.version),
            old,
            1_000,
        )
        .expect("the first wave")
    else {
        panic!("nothing was fulfilled yet");
    };
    first
        .finish(ticket, true, 2_000)
        .expect("the first wave ends");
    let settled = first.record().repositories.expect("recorded").version;

    let stale = RefreshRequest::inputs(old, observed.version);
    let Admission::Refresh(ticket) = first
        .admit(stale, new, 3_000)
        .expect("admitted, not refused")
    else {
        panic!("the moved inputs owe a wave");
    };
    assert!(ticket.scope() == new, "the wave is the runner's own scope");
    let admitted = first.record().repositories.expect("recorded").version;
    assert!(admitted.input > settled.input, "the move is a new input");
    first
        .finish(ticket, true, 4_000)
        .expect("the second wave ends");

    assert!(
        matches!(
            first.admit(stale, new, 5_000).expect("admitted"),
            Admission::Fulfilled
        ),
        "the same stale request is answered by the wave that ran for it"
    );
}
