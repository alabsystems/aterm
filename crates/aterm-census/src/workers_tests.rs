// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn one_worker_keeps_nested_jobs_inline_and_preserves_joins() {
    let answer = on_one_worker(|| {
        let owner = std::thread::current().id();
        threads(|scope| {
            let jobs: Vec<_> = (0..3)
                .map(|i| {
                    scope.spawn(move || {
                        assert_eq!(std::thread::current().id(), owner);
                        threads(|nested| {
                            nested
                                .spawn(move || {
                                    assert_eq!(std::thread::current().id(), owner);
                                    i + 1
                                })
                                .join()
                                .unwrap()
                        })
                    })
                })
                .collect();
            assert!(scope.spawn(|| panic!("joined control")).join().is_err());
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .collect::<Vec<_>>()
        })
    })
    .unwrap();
    assert_eq!(answer, [1, 2, 3]);
    let caller = std::thread::current().id();
    let parallel = threads(|scope| scope.spawn(|| std::thread::current().id()).join().unwrap());
    assert_ne!(parallel, caller, "the ordinary path still starts workers");
    without_parallel_workers(|| {
        let inline = threads(|scope| scope.spawn(|| std::thread::current().id()).join().unwrap());
        assert_eq!(inline, caller);
    });
    assert!(!SERIAL.get(), "the caller's policy was restored");
}

#[test]
fn an_unjoined_panic_is_preserved_in_both_execution_modes() {
    for serial in [false, true] {
        let work = || threads(|scope| drop(scope.spawn(|| panic!("unjoined control"))));
        let result = std::panic::catch_unwind(|| {
            if serial {
                without_parallel_workers(work)
            } else {
                work()
            }
        });
        assert!(result.is_err());
    }
}
