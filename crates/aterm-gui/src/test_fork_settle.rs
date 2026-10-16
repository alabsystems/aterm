// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE TEST PROCESS FORKS ONLY ONCE libnotify IS UP.
//!
//! A thread inside libnotify's once-gate when another thread forks loses the
//! child inside `fork()` — `BUG IN CLIENT OF LIBPLATFORM: os_once_t is
//! corrupt`, "crashed on child side of fork pre-exec", SIGKILL — and the
//! parent never learns: the child's copy of std's CLOEXEC error pipe closes
//! with it, the EOF reads as an exec that succeeded, and `Command::spawn`
//! answers `Ok` for a child that is already dead.
//! `aterm_uds::spawnfd::pass_libnotify_gate` has the mechanism; every fork
//! that strips its child passes the gate first (`InheritStrip::prepare`), but
//! this binary's test helpers fork on their own.
//!
//! Explicit native observer tests still reach NSWorkspace while process tests
//! fork. Test Apps use inert preference probes; the native runtime tests and
//! process helpers nevertheless share libnotify's process-global fork state.
//!
//! So the gate is passed once, at image load, before the harness has started a
//! thread; from then on `_notify_fork_child` in any child takes the built
//! globals.

aterm_uds::pass_libnotify_gate_at_load!();
