// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The test process forks only once libnotify is up.
//!
//! A thread inside libnotify's once-gate when another thread forks loses the
//! child inside `fork()` (`BUG IN CLIENT OF LIBPLATFORM: os_once_t is corrupt`,
//! SIGKILL), and the parent never learns: the child's copy of std's CLOEXEC
//! error pipe closes with it, the EOF reads as a successful exec, and
//! `Command::spawn` answers `Ok` for a child already dead.
//! `aterm_uds::spawnfd::pass_libnotify_gate` has the mechanism; every fork that
//! strips its child passes the gate first (`InheritStrip::prepare`), but this
//! binary's test helpers fork on their own.
//!
//! Here the first headless `App`'s appearance read (`native_appearance_preferences`
//! → NSWorkspace → CFPreferences → `os_log_create`) is the process's first
//! libnotify call, made on one test thread while others fork. So the gate is
//! passed once, at image load, before the harness starts a thread.

aterm_uds::pass_libnotify_gate_at_load!();
