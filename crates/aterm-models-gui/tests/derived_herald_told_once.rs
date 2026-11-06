// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_gui::herald_told_once_model;
use aterm_spec::interp;

/// THE TOLD-ONCE MEMORY HOLDS, AND EACH MUTANT BREAKS ITS OWN LAW (design §14.18): no memory
/// names a need twice, the first need's key alone leaves the second untold, an owner turn or a
/// take-down that keeps the memory forgets nothing, news judged only on a key change leaves a
/// need swapped in under the same key untold, and a restart, or the restart's gap, that forgets
/// tells a need told before it again.
#[test]
fn a_need_is_told_once_and_each_mutant_breaks_its_law() {
    let model = herald_told_once_model();
    let malformed = model.malformed_bounds();
    assert!(
        malformed.is_empty(),
        "bounds describe numeric ranges: {malformed:?}"
    );
    assert!(interp::violated_invariants(&model).is_empty());
    for (dial, law) in [
        (1, "ToldOnce"),
        (2, "ShownIsTold"),
        (3, "TurnForgets"),
        (4, "ClearForgets"),
        (5, "ShownIsTold"),
        (6, "ToldOnce"),
        (7, "ToldOnce"),
    ] {
        assert!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)).contains(law),
            "Buggy={dial} must break {law}"
        );
    }
    let faults = aterm_spec::verify::sweep_faults(&[model]);
    assert!(faults.is_empty(), "{faults:?}");
}
