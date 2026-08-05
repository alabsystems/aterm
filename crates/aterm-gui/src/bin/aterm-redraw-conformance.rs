// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-redraw-conformance` — the Phase 1a exit gate, as a BINARY.
//!
//! WHY A BINARY AND NOT A `#[test]`: `EventLoop` construction panics off the
//! process main thread on every target, and libtest runs every test body on a
//! spawned thread — so no test can hand `GuiHost` a real `EventLoopProxy`, and the
//! unit matrix must build its host with `proxy: None`. This target owns `fn main`,
//! so its body IS the main thread. The full argument, and the exit codes CI gates
//! on (0 pass / 1 fail / 2 not-run), are in
//! `aterm_gui::control_redraw_conformance`.
//!
//! Thin by the same rule as `main.rs`: the gate needs `GuiHost`, `Wake` and the
//! session registry, all private to the library, so the body lives there and this
//! file only supplies the main thread and the exit code.

fn main() {
    std::process::exit(aterm_gui::run_redraw_conformance());
}
