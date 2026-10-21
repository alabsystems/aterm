// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm edit` and the `amacs` argv0 alias (docs/DESIGN-amacs-2026-10-19.md
//! §8.1): the amacs program's one entry in the `aterm` binary, reached from
//! the front door's verb match, its `aterm amacs` arm and the alias route.
//!
//! The program is `amacs_term`'s. This entry is where the amacs process's one
//! root authority is minted (design §4.8), since only a launcher may mint one
//! (the census's `launcher-mint` guard lets `crates/aterm-cli/` do so): the
//! terminal face's document store edits through the buffer-write capability
//! granted from it. The native client uses none; the window it hands the file
//! to does the writing.

use std::ffi::OsString;
use std::process::ExitCode;

/// Run amacs with `args` (argv past `amacs`, or past `aterm edit`).
#[must_use]
pub fn run(args: Vec<OsString>) -> ExitCode {
    // The single root-authority mint of an amacs run, granting the
    // buffer-write capability its terminal face's store edits with.
    // SAFETY: the trusted entry of the amacs process, reached exactly once,
    // before any input (an argument, a key, a file) is read.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    amacs_term::run_client(args, authority.grant(aterm_cap::Tier::Trusted))
}
