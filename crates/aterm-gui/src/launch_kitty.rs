// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide launch-kitty seed generation.

/// Mints the process's launch-kitty seed from the OS CSPRNG, falling back to a clock-and-PID mix.
pub(crate) fn mint_launch_seed() -> u64 {
    let mut bytes = [0u8; 8];
    match aterm_uds::rand::fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes),
        Err(_) => fallback_seed(),
    }
}

fn fallback_seed() -> u64 {
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    nanos ^ pid.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15
}
