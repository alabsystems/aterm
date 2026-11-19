// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::{BTreeSet, VecDeque};

fn model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        NativeRetainedOrdering {
            const Buggy = 0;
            var stage = 0;
            var recorded = 0;
            var grant = 0;
            action Held when(stage == 0) {stage=1;}
            action Recorded when(stage == 1) {stage=2;recorded=1;}
            action Granted when(stage == 2 || (Buggy == 1 && stage == 1)) {stage=3;grant=1;}
            action Command when(stage == 3) {stage=4;}
            action Entered when(stage == 4) {stage=5;}
            action Attached when(stage == 5) {stage=6;}
            action Runtime when(stage == 6) {stage=7;}
            action Ui when(stage == 7) {stage=8;}
            action Observed when(stage == 8) {stage=9;}
            action Promoted when(stage == 9) {stage=10;}
            action Committed when(stage == 10) {stage=11;}
            invariant Bounds: stage <= 11 && recorded <= 1 && grant <= 1;
            invariant GrantRequiresDurableRecord: grant <= recorded;
        }
    }
}
#[test]
fn retained_ordering_model_catches_grant_before_record_and_conforms_to_real_transitions() {
    let model = model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([model.init_state()]);
    while let Some(before) = queue.pop_front() {
        if !seen.insert(before.clone()) {
            continue;
        }
        let stage = usize::try_from(before["stage"]).unwrap();
        for (index, step) in Step::ALL.into_iter().enumerate() {
            let name = match step {
                Step::Held => "Held",
                Step::Recorded => "Recorded",
                Step::Granted => "Granted",
                Step::Command => "Command",
                Step::Entered => "Entered",
                Step::Attached => "Attached",
                Step::Runtime => "Runtime",
                Step::Ui => "Ui",
                Step::Observed => "Observed",
                Step::Promoted => "Promoted",
                Step::Committed => "Committed",
            };
            let mut actual = Progress(if stage == 0 { 0 } else { (1 << stage) - 1 });
            let previous = actual;
            if model.action_enabled(name, &before) {
                assert_eq!(index, stage);
                actual.advance(step).unwrap();
                assert!(actual.contains(step));
                let next = model.successors(name, &before);
                assert_eq!(next.len(), 1);
                let after = next.into_iter().next().unwrap();
                assert_eq!(actual.0, (1 << after["stage"]) - 1);
                queue.push_back(after);
            } else if index > stage {
                assert!(actual.advance(step).is_err());
                assert_eq!(actual, previous, "refusal cannot publish a later milestone");
            } else {
                actual.advance(step).unwrap();
                assert_eq!(
                    actual, previous,
                    "historical exact replay adds no transition"
                );
            }
        }
    }
    assert_eq!(seen.len(), 12);
}
