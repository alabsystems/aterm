// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development-only process controls.

#[path = "capture_fixture.rs"]
mod capture;
#[cfg(unix)]
#[path = "warm_fixture.rs"]
mod warm;

fn main() -> std::process::ExitCode {
    let ended = aterm_uds::tether::entry().or_else(capture::entry);
    #[cfg(unix)]
    let ended = ended.or_else(warm::entry);
    ended.unwrap_or(std::process::ExitCode::from(2))
}
