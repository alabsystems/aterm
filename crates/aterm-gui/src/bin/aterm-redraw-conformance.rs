// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-redraw-conformance`: the redraw conformance gate as a binary.
//!
//! `EventLoop` construction panics off the process main thread and libtest runs
//! tests on spawned threads, so no `#[test]` can hand `GuiHost` a real
//! `EventLoopProxy`. This binary owns `fn main`, so its body is the main thread.
//! The body lives in the library (`aterm_gui::run_redraw_conformance`, which
//! documents the exit codes: 0 pass, 1 fail, 2 not run); this file only
//! supplies the main thread and the exit code. The gate is a debug build's
//! (aterm-control's verb matrix is), so a release build of this binary says
//! so and answers 2, never a pass.

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
