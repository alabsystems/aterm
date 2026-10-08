// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-redraw-conformance`: the redraw conformance gate as a binary.
//!
//! `EventLoop` construction panics off the process main thread and libtest runs
//! tests on spawned threads, so no `#[test]` can hand `GuiHost` a real
//! `EventLoopProxy`. This binary owns `fn main`, so its body is the main thread.
//! The body lives in the library (`aterm_gui::run_redraw_conformance`, which
//! documents the exit codes: 0 pass, 1 fail, 2 not run); this file only
//! supplies the main thread and the exit code.

fn main() {
    std::process::exit(aterm_gui::run_redraw_conformance());
}
