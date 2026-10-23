// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-thread binary entry point for `aterm_gui::run_redraw_conformance`.

fn main() {
    #[cfg(debug_assertions)]
    std::process::exit(aterm_gui::run_redraw_conformance());
    #[cfg(not(debug_assertions))]
    {
        eprintln!(
            "aterm-redraw-conformance: NOT RUN — the gate is compiled into debug builds \
             only; build it without --release"
        );
        std::process::exit(2);
    }
}
