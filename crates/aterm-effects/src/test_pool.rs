// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tests' replay pool, shared by the unit tests and the integration suite
//! (`tests/common`): independent runs side by side, on no more threads than
//! libtest gives the binary.

/// `f` over every item on a pool as wide as `RUST_TEST_THREADS` (as libtest reads it, else the
/// machine's parallelism), results in `items`' order. Each item is one independent replay (its
/// own `Terminal`, engine and clock), so where it runs changes nothing it reads. A panicking
/// item does not stop the others; once all have run, the first panic in `items`' order is
/// re-raised with its own payload, as a serial loop would raise it.
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
    // Each worker hands back the items it ran, so no slot is shared while they run.
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
            // Every item's panic is caught above, so a worker's own join holds none.
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
