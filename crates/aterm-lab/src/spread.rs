// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's threaded [`Spread`], a neutral module (design §13.3) so the M6 learner reaches it
//! without naming `aos_workers`.

use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_effects::aos_momentum::m4::Spread;

/// A [`Spread`] over `n` scoped threads: each takes the next index until none is left, so a spread
/// changes only speed.
#[derive(Clone, Copy, Debug)]
pub struct Threads(pub usize);

impl Spread for Threads {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        let next = AtomicUsize::new(0);
        let width = self.0.clamp(1, n.max(1));
        std::thread::scope(|s| {
            for _ in 0..width {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        job(i);
                    }
                });
            }
        });
    }
}
