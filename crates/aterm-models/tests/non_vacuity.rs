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

/// Every fault `check` finds across `items`, sorted.
fn faults_across<T: Sync>(
    items: &[T],
    name: impl Fn(&T) -> &'static str + Sync,
    check: impl Fn(&T) -> Vec<String> + Sync,
) -> Vec<String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
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
