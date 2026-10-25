// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-global held-out counter (Aos design §13.3), alone in its own binary: the unit tests
//! judge held-out items on parallel threads, so only a process of its own can see one gate's count.

#![cfg(any(debug_assertions, feature = "aos-learn"))]

use aterm_effects::aos_momentum::m4::{Evaluator, held_out_judged};
use aterm_effects::aos_momentum::policy::HELD_OUT_COUNT;
use aterm_effects::aos_momentum::search::GridProposer;
use aterm_time::Instant;

/// The counter reads 0 through the evaluator's build and baseline check and the grid's selection,
/// and rises by exactly the held-out split's 9 items across one held-out gate of the grid winner,
/// which passes every one.
#[test]
fn held_out_judged_counts_one_gate() {
    let ev = Evaluator::v1("m6-held-out-counter", Instant::now()).expect("the pinned policy v1");
    assert_eq!(
        held_out_judged(),
        0,
        "building the evaluator judges no held-out item"
    );
    let selection = ev
        .select(&mut GridProposer::new())
        .expect("the grid has a training winner");
    assert_eq!(held_out_judged(), 0, "selection judges no held-out item");
    let admitted = ev
        .held_out(selection)
        .expect("the grid winner passes held-out");
    assert_eq!(HELD_OUT_COUNT, 9);
    assert_eq!(
        held_out_judged(),
        9,
        "one gate judges each held-out item once"
    );
    assert!(admitted.useful());
}
