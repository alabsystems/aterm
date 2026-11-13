// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-winship` — the Windows owner loop. All parsing, dispatch and the exit
//! code live in the library so the whole surface is testable; this stays thin.

fn main() {
    std::process::exit(aterm_winship::run(
        &std::env::args().skip(1).collect::<Vec<_>>(),
    ));
}
