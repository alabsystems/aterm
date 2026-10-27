// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parallel replay pool bounded by `RUST_TEST_THREADS` or machine parallelism.

/// Evaluates `f` across `items` on a bounded thread pool and returns results in order.
#[inline]
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let configured = match std::env::var("RUST_TEST_THREADS") {
        Ok(v) => match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => None,
        },
        Err(_) => None,
    };
    let threads = match configured {
        Some(n) => n,
        None => match std::thread::available_parallelism() {
            Ok(n) => n.get(),
            Err(_) => 4,
        },
    }
    .min(items.len());
    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<std::thread::Result<R>>> = Vec::with_capacity(items.len());
    slots.resize_with(items.len(), || None);
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(threads);
        let mut wi = 0;
        while wi < threads {
            workers.push(scope.spawn(|| {
                let mut ran = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    ran.push((i, catch_unwind(AssertUnwindSafe(|| f(item)))));
                }
                ran
            }));
            wi += 1;
        }
        while let Some(worker) = workers.pop() {
            let mut ran = match worker.join() {
                Ok(ran) => ran,
                Err(p) => resume_unwind(p),
            };
            while let Some((i, out)) = ran.pop() {
                slots[i] = Some(out);
            }
        }
    });
    let mut out = Vec::with_capacity(slots.len());
    let mut si = 0;
    while si < slots.len() {
        match slots[si].take().expect("every item ran") {
            Ok(r) => out.push(r),
            Err(payload) => resume_unwind(payload),
        }
        si += 1;
    }
    out
}
