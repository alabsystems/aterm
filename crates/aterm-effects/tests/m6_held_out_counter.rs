// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-global held-out counter (Aos design §13.3), alone in its own binary: the unit tests
//! judge held-out items on parallel threads, so only a process of its own can see one gate's count.

#![cfg(any(debug_assertions, feature = "aos-learn"))]

use aterm_effects::aos_momentum::corpus::GOLDEN_TRACE_TARGET;
use aterm_effects::aos_momentum::m4::{Evaluator, Spread, held_out_judged};
use aterm_effects::aos_momentum::plan::{SchedulePlan, Split, Wake};
use aterm_effects::aos_momentum::policy::{HELD_OUT_COUNT, PolicyRefusal};
use aterm_effects::aos_momentum::search::FileProposer;
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

/// The grid's winner and three grid plans that lose to it: one admitted with more ticks, one
/// refused on a lane item (`train-01`) and one on the golden trace, so a selection takes each way a
/// candidate leaves it. Each is hex on its own line, as a plans file reads.
fn winner_and_losers() -> FileProposer {
    let plan = |split, wake| SchedulePlan::new(split, wake).expect("a grid plan");
    let text: String = [
        plan(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep),
        plan(Split::Shipped, Wake::Cadence),
        plan(Split::Shipped, Wake::ColdOnly),
        plan(Split::Shipped, Wake::Never),
    ]
    .iter()
    .map(|p| {
        let hex: String = p.encode().iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}\n")
    })
    .collect();
    FileProposer::from_text(&text)
}

/// The counter reads 0 through the evaluator's build and baseline check and a selection of the
/// grid's winner over its losers ([`winner_and_losers`], trained on threads, which changes only how
/// fast it runs), and rises by exactly the held-out split's 9 items across one held-out gate of the
/// winner, which passes every one. The selection is four plans and not the grid's 54, at a quarter of
/// the cost: every plan goes through the same `select_local`, so a held-out read that only the grid's
/// own proposer reaches, or only a plan past these four, goes unseen here, and is
/// `m4_tests::a_selection_of_the_grid_asks_for_no_held_out_item`'s.
///
/// NEGATIVE CONTROL (2026-11-05): judging a held-out item in `select_local` for a refused candidate
/// fires `"selection judges no held-out item"` (left 2, right 0); for an admitted candidate that
/// loses, the same (left 1, right 0); a gate that judges one held-out item twice fires `"one gate
/// judges each held-out item once"` (left 10, right 9).
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
        .select_spread(&mut winner_and_losers(), &Threads)
        .expect("the grid's winner beats the baseline");
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
