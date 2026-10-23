// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm edit` and the `amacs` argv0 alias (docs/DESIGN-amacs-2026-10-19.md §8.1): the amacs
//! program's one entry in the `aterm` binary, reached from the front door's verb match, its `aterm
//! amacs` arm and the alias route.

use std::ffi::OsString;
use std::process::ExitCode;

/// Run amacs with `args` (argv past `amacs`, or past `aterm edit`).
#[must_use]
pub fn run(args: Vec<OsString>) -> ExitCode {
    // SAFETY: the trusted entry of the amacs process, reached exactly once, before any input (an
    // argument, a key, a file) is read.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    amacs_term::run_client(args, authority.grant(aterm_cap::Tier::Trusted))
}
