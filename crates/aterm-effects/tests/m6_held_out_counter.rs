// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-global held-out counter (Aos design §13.3), alone in its own binary: the unit tests
//! judge held-out items on parallel threads, so only a process of its own can see one gate's count.

#![cfg(any(debug_assertions, feature = "aos-learn"))]

use aterm_effects::aos_momentum::corpus::GOLDEN_TRACE_TARGET;
use aterm_effects::aos_momentum::m4::{Evaluator, Spread, held_out_judged};
use aterm_effects::aos_momentum::policy::{HELD_OUT_COUNT, PolicyRefusal};
use aterm_effects::aos_momentum::search::GridProposer;
use aterm_effects::aos_momentum::target_name;
use aterm_time::Instant;

/// Every job on a scoped thread of its own.
struct Threads;

impl Spread for Threads {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|s| {
            for i in 0..n {
                s.spawn(move || job(i));
            }
        });
    }
}

/// The counter reads 0 through the evaluator's build and baseline check and the grid's selection
/// (its plans trained on threads, which changes only how fast it runs), and rises by exactly the
/// held-out split's 9 items across one held-out gate of the grid winner, which passes every one.
#[test]
fn held_out_judged_counts_one_gate() {
    let built = Evaluator::v1("m6-held-out-counter", Instant::now());
    assert_eq!(
        held_out_judged(),
        0,
        "building the evaluator judges no held-out item"
    );
    if target_name() != GOLDEN_TRACE_TARGET {
        // Another target's profile is another policy, which v1 refuses
        // (`policy_tests::the_policy_is_pinned`): nothing is judged.
        assert!(
            matches!(built, Err(PolicyRefusal::PolicyMutated { .. })),
            "off {GOLDEN_TRACE_TARGET} v1 is refused as another policy: {:?}",
            built.err()
        );
        return;
    }
    let ev = built.expect("the pinned policy v1");
    let selection = ev
        .select_spread(&mut GridProposer::new(), &Threads)
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
