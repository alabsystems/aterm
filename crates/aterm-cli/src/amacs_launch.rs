// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm edit` and the `amacs` argv0 alias (docs/DESIGN-amacs-2026-10-19.md
//! §8.1): the amacs program's one entry in the `aterm` binary, reached from
//! the front door's verb match, its `aterm amacs` arm and the alias route.
//!
//! The program is `amacs_term`'s; this entry is where the amacs process's
//! one root authority will be minted when its terminal face needs a write
//! capability (T3), since only a launcher may mint one (the census's
//! `launcher-mint` guard lets `crates/aterm-cli/` do so). The native client
//! needs none: the window it hands the file to does the writing.

use std::ffi::OsString;
use std::process::ExitCode;

/// Run amacs with `args` (argv past `amacs`, or past `aterm edit`).
#[must_use]
pub fn run(args: Vec<OsString>) -> ExitCode {
    amacs_term::run_client(args)
}
