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
//! In this test binary the gate is armed by the first headless `App`: its
//! appearance read (`native_appearance_preferences` → NSWorkspace →
//! CFPreferences → `os_log_create`) is the process's first libnotify call, and
//! it runs on one test thread while others fork. Measured 2026-10-01, load
//! 100-150 on 18 cores, `title_summary:: qos:: --test-threads 16` looped 360
//! times: 8 runs lost a forked child that way — a `sleep 30` "disappeared
//! before identity capture" (its `waitid` status: killed, SIGKILL), a stand-in
//! daemon or a `/usr/bin/true` that exited on signal 9, a shell that died
//! before printing its child's pid — each with the crash report above, named
//! after the test binary. With the gate passed at load, 0 in 360, run beside
//! them.
//!
//! So the gate is passed once, at image load, before the harness has started a
//! thread; from then on `_notify_fork_child` in any child takes the built
//! globals.

aterm_uds::pass_libnotify_gate_at_load!();
