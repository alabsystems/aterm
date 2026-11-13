// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

fn lane(slots: Vec<Slot>, sizes: &[u64], budget: u64) -> Ahead {
    assert_eq!(slots.len(), sizes.len());
    let jobs = sizes
        .iter()
        .enumerate()
        .map(|(i, &size)| Job {
            unit: i,
            program: format!("p{i}"),
            pinned: 1,
            repo: "model/repository".to_string(),
            artifact: crate::manifest::Artifact {
                target: "model-target".to_string(),
                kind: "binary".to_string(),
                protocol: "github-release".to_string(),
                asset: format!("p{i}.tar.zst"),
                sha256: String::new(),
                sha512: String::new(),
                tree_root: String::new(),
                size,
                reloc: "self-contained".to_string(),
                cost: crate::manifest::Cost::default(),
                url: String::new(),
                payload: String::new(),
                entry: String::new(),
                strip_components: 0,
                links: Default::default(),
                vendor: String::new(),
                provenance: None,
            },
            dl: PathBuf::from(format!("/model/p{i}/p{i}.tar.zst")),
        })
        .collect();
    let n = sizes.len();
    Ahead {
        jobs,
        budget,
        state: Mutex::new(State {
            slots,
            claimed: vec![false; n],
            current: None,
            passed: BTreeSet::new(),
            fetched: vec![false; n],
            halted: false,
        }),
        changed: Condvar::new(),
    }
}

fn next_without_wait(lane: &Ahead) -> (Next, bool) {
    let waited = Cell::new(false);
    let next = lane.next_with_wait(&[], |state| {
        waited.set(true);
        drop(state);
        Next::Again
    });
    (next, waited.get())
}

#[test]
fn failed_partial_bytes_hold_the_budget_until_the_pass_takes_them() {
    const MIB: u64 = 1 << 20;
    let complete = lane(vec![Slot::Done, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
    let (next, waited) = next_without_wait(&complete);
    assert!(matches!(next, Next::Again) && waited);
    assert!(matches!(
        complete.state.lock().unwrap().slots[1],
        Slot::Queued
    ));

    let taken = lane(vec![Slot::Failed, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
    assert!(taken.claim("p0", &taken.jobs[0].dl).is_none());
    assert!(taken.state.lock().unwrap().claimed[0]);
    let (next, waited) = next_without_wait(&taken);
    assert!(matches!(next, Next::Fetch(1)) && !waited);

    let room = lane(
        vec![Slot::Failed, Slot::Queued],
        &[100 * MIB, 400 * MIB],
        512 * MIB,
    );
    let (next, waited) = next_without_wait(&room);
    assert!(matches!(next, Next::Fetch(1)) && !waited);

    let stopped = lane(vec![Slot::Failed, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
    stopped.halt();
    let (next, waited) = next_without_wait(&stopped);
    assert!(matches!(next, Next::Exit) && !waited);

    let partial = lane(vec![Slot::Failed, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
    let (next, waited) = next_without_wait(&partial);
    let still_queued = matches!(partial.state.lock().unwrap().slots[1], Slot::Queued);
    let retained = partial.untaken() == vec![("p0".to_string(), "p0.tar.zst".to_string())];
    assert!(
        matches!(next, Next::Again) && waited && still_queued && retained,
        "FAILED_PARTIAL_BYTES_MUST_HOLD_THE_AHEAD_RESERVATION"
    );
}
