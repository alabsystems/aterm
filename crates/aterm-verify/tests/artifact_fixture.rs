// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development-only facts probe for the verifier's private-image execution tests.

#[path = "../../atpkg/tests/facts_fixture.rs"]
mod facts;

fn main() -> std::process::ExitCode {
    facts::entry().unwrap_or(std::process::ExitCode::from(2))
}
