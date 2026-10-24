// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The test process forks only once libnotify is up.
//!
//! A fork while another thread is inside libnotify's once-gate kills the child
//! inside `fork()` (`os_once_t is corrupt`), and `Command::spawn` still answers
//! `Ok`. `InheritStrip::prepare` passes the gate for every fork that strips its
//! child, but this binary's test helpers fork on their own, so the gate is
//! passed once, at image load, before the harness starts a thread.

aterm_uds::pass_libnotify_gate_at_load!();
