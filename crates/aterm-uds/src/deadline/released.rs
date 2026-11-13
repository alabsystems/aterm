// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io;
use std::time::{Duration, Instant};

/// Observe release after dropping a close-only lock or a socket/pipe endpoint. A sibling
/// thread's child can retain the open description between fork and exec even when every local
/// copy has closed. Only `WouldBlock` means that release has not yet been observed; every other
/// error fails immediately. The probe must be nonblocking and safe to repeat before release.
/// This waits for a state transition, never reruns a failed test or changes its verdict.
///
/// # Panics
/// The probe fails, or release is still unobserved at the one-minute hang detector.
#[track_caller]
pub fn once_released<T>(attempt: impl FnMut() -> io::Result<T>) -> T {
    released(attempt, Instant::now, super::poll_yield).unwrap()
}

fn released<T>(
    mut attempt: impl FnMut() -> io::Result<T>,
    now: impl Fn() -> Instant,
    mut pause: impl FnMut(),
) -> io::Result<T> {
    let give_up = now() + Duration::from_secs(60);
    loop {
        match attempt() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && now() < give_up => {
                pause();
            }
            other => return other,
        }
    }
}

#[cfg(test)]
#[path = "released_tests.rs"]
mod tests;
