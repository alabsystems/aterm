// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spec-check`: the obligations only the Trust checkers discharge
//! ([`aterm_spec::check`]).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(aterm_spec::check::main(&args));
}
