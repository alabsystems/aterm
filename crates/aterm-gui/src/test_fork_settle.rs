// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The test process forks only once libnotify is up.

aterm_uds::pass_libnotify_gate_at_load!();
