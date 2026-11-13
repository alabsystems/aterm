// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-vacuity and mutant fault-sweep tests for all registered models.

#[test]
fn every_registered_model_holds_and_each_law_is_what_it_declares() {
    let _domains = aterm_models::link_all();
    let models = aterm_spec::xref::model_configurations();
    assert!(!models.is_empty(), "no model is registered in this binary");
    let faults = faults_across(
        &models,
        |configs| configs[0].name,
        |configs| aterm_spec::verify::sweep_faults(configs),
    );
    assert!(
        faults.is_empty(),
        "{} fault(s) across the {} registered models:\n  {}",
        faults.len(),
        models.len(),
        faults.join("\n  ")
    );
}

#[test]
fn every_liveness_obligation_is_proven_minimal_and_caught() {
    let _domains = aterm_models::link_all();
    let obligations = aterm_spec::xref::liveness_registry();
    assert!(
        !obligations.is_empty(),
        "no liveness obligation is registered in this binary"
    );
    let failures = faults_across(
        &obligations,
        |(m, _)| m.name,
        |(m, live)| {
            aterm_spec::verify::audit_liveness(m, live)
                .err()
                .into_iter()
                .collect()
        },
    );
    assert!(
        failures.is_empty(),
        "liveness obligations that do not hold, are not minimal, or are not caught:\n  {}",
        failures.join("\n  ")
    );
}

/// The shared-file model (docs/DESIGN-shared-files-2026-11-11.md §8) is registered, holds with no
/// mutant on while every action fires, and each of its laws falls to exactly the one mutant
/// `SHARED_FILE_LAWS` names for it, which breaks no other law and neither bound.
#[test]
fn shared_file_holds_and_each_law_falls_to_exactly_its_one_named_mutant() {
    use aterm_spec::derive::InvariantKind;
    use aterm_spec::interp;
    let _domains = aterm_models::link_all();
    let registered = aterm_spec::xref::model_configurations();
    let configs = registered
        .iter()
        .find(|configs| configs[0].name == "SharedFile")
        .expect("SharedFile is not registered in this binary, so no sweep reads it");
    let model = &configs[0];
    let faults = aterm_spec::verify::sweep_faults(configs);
    assert!(
        faults.is_empty(),
        "the sweep's faults:\n  {}",
        faults.join("\n  ")
    );
    let states = interp::bmc(model)
        .unwrap_or_else(|(state, law)| panic!("{law} is broken with no mutant on, at {state:?}"));
    let fired = interp::fired_actions(model);
    let dead: Vec<_> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !fired.contains(name))
        .collect();
    assert!(dead.is_empty(), "actions that never fire: {dead:?}");

    let laws = aterm_models_db::SHARED_FILE_LAWS;
    let of_kind = |kind| -> Vec<&str> {
        model
            .invariants
            .iter()
            .filter(|law| law.kind == kind)
            .map(|law| law.name)
            .collect()
    };
    assert_eq!(
        of_kind(InvariantKind::Claim),
        laws.map(|(law, _)| law),
        "the model's claims are the design's sixteen laws, in its order"
    );
    assert_eq!(
        of_kind(InvariantKind::Bound),
        ["OutboxWithinCap", "LogWithinKeep"]
    );
    assert_eq!(
        model.mutant_dials(),
        (1..=16).collect::<Vec<i64>>(),
        "one mutant to a law"
    );
    for (dial, (law, mutant)) in (1..).zip(laws) {
        let broken = interp::violated_invariants(&interp::with_buggy(model, dial));
        assert_eq!(
            broken,
            std::collections::BTreeSet::from([law]),
            "mutant {dial} ({mutant}) breaks {law} and nothing else"
        );
    }
    eprintln!(
        "SharedFile: {states} states with no mutant on; {} laws, each broken by exactly its one \
         named mutant; 2 bounds no mutant breaks",
        laws.len()
    );
}

/// Every fault `check` finds across `items`, sorted.
fn faults_across<T: Sync>(
    items: &[T],
    name: impl Fn(&T) -> &'static str + Sync,
    check: impl Fn(&T) -> Vec<String> + Sync,
) -> Vec<String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = aterm_cores::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(items.len());
    let mut faults: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut faults = Vec::new();
                    while let Some(item) =
                        items.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        let run = std::panic::AssertUnwindSafe(|| check(item));
                        match std::panic::catch_unwind(run) {
                            Ok(found) => faults.extend(found),
                            Err(why) => faults.push(format!(
                                "{}: the check could not run: {}",
                                name(item),
                                why.downcast_ref::<String>()
                                    .map(String::as_str)
                                    .or_else(|| why.downcast_ref::<&str>().copied())
                                    .unwrap_or("a panic with no message")
                            )),
                        }
                    }
                    faults
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a check worker died outside an item"))
            .collect()
    });
    faults.sort();
    faults
}
