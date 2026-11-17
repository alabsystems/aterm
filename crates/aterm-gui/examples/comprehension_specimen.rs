// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comprehension marks' offscreen specimen (`aterm_gui::comprehension_specimen`), which
//! `targo --unverified lab comprehension-gallery` photographs.

fn main() {
    #[cfg(debug_assertions)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::exit(aterm_gui::comprehension_specimen::run(&args));
    }
    #[cfg(not(debug_assertions))]
    {
        eprintln!("comprehension_specimen: NOT RUN: built into debug builds only");
        std::process::exit(2);
    }
}
