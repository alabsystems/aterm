// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified bench --profile opt-test -p aterm-netplace --bench pacing_cache_cost`.
//! Manual warm-cache measurement; no timing assertion or live network facts.

#[cfg(all(unix, debug_assertions))]
fn main() {
    use aterm_netplace::{Facts, facts, routes, sample};
    let place = sample::hotspot(1, 1);
    let script = facts::Script {
        facts: Facts {
            route: place.route,
            tunnel: place.tunnel,
            path: place.path,
        },
        via: Vec::new(),
    };
    let state = aterm_tempfile::tempdir().expect("private pacing state");
    facts::script_here(script, || {
        for rounds in [1, 64, 1024, 16_384] {
            let expected = routes::paced_now(state.path());
            let start = std::time::Instant::now();
            let (last, allocations) = aterm_heap_count::allocations_in(|| {
                let mut last = expected;
                for _ in 0..rounds {
                    last =
                        std::hint::black_box(routes::paced_now(std::hint::black_box(state.path())));
                }
                last
            });
            println!(
                "rounds={rounds} paced={last} allocations={allocations} elapsed_ns={} cache_window_ns={}",
                start.elapsed().as_nanos(),
                aterm_netplace::keeper::CACHE_FOR.as_nanos()
            );
            assert_eq!(
                last, expected,
                "the fixed synthetic facts retain their pacing"
            );
        }
    });
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!("pacing_cache_cost requires Unix and --profile opt-test for scripted facts");
}
