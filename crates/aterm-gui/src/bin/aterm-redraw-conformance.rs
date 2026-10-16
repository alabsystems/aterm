// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-redraw-conformance`: the redraw conformance gate as a binary. `EventLoop`
//! construction panics off the process main thread and libtest runs tests on spawned threads,
//! so no `#[test]` can hand `GuiHost` a real `EventLoopProxy`. The body is
//! `aterm_gui::run_redraw_conformance` (exit 0 pass, 1 fail, 2 not run); this file supplies
//! the main thread.

fn main() {
    std::process::exit(aterm_gui::run_redraw_conformance());
}
