// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-winsign` — the `cargo winsign` alias. All parsing, dispatch and the
//! exit code live in the library so the whole surface is testable; this stays
//! thin forever.

fn main() {
    std::process::exit(aterm_winsign::run(
        std::env::args().skip(1).collect(),
        &aterm_winsign::RealHost,
    ));
}
