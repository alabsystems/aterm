// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parallel replay pool bounded by `RUST_TEST_THREADS` or machine parallelism.

/// Evaluates `f` across `items` on a bounded thread pool and returns results in order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let threads = std::env::var("RUST_TEST_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .min(items.len());
    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<std::thread::Result<R>>> = items.iter().map(|_| None).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut ran = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        ran.push((i, catch_unwind(AssertUnwindSafe(|| f(item)))));
                    }
                    ran
                })
            })
            .collect();
        for worker in workers {
            for (i, out) in worker.join().unwrap_or_else(|p| resume_unwind(p)) {
                slots[i] = Some(out);
            }
        }
    });
    slots
        .into_iter()
        .map(|slot| match slot.expect("every item ran") {
            Ok(r) => r,
            Err(payload) => resume_unwind(payload),
        })
        .collect()
}
