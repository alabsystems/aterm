// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inert development-only native tether entry for ordinary supervision controls.

fn main() -> std::process::ExitCode {
    aterm_uds::tether::entry().unwrap_or(std::process::ExitCode::from(2))
}
