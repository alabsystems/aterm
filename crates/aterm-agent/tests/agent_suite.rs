// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-agent integration files as modules of ONE test binary: each file
//! keeps its path and its own `#[test]`s. `conformance_upgrade_models` keeps its
//! `#[path]`-mounted submodules (resolved from `tests/`, as before).
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable, installs a hook or an allocator, or re-executes
//! itself; each file's scratch directories carry a prefix of its own
//! (`aterm-footer-model-`, `aterm-operator-conformance-`,
//! `codex-nudge-restart-`), so no two files share one; the one listener binds
//! port 0; and the checker tier's `ty` spawns (`validate_transition_tiered`)
//! take a scratch directory per call (`aterm_spec::verify::ty_output`), and
//! share nothing else: that driver holds no lock since 2026-10-01, so they run
//! concurrently with every other `ty` on the machine.
//! The two files that count process-wide allocations
//! (`footer_window_allocations`, `transcript_allocations`) are their own
//! `[[test]]`s.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-agent --test agent_suite -- <file>::`.

mod conformance_footer_model;
mod conformance_login_wall;
mod conformance_operator;
mod conformance_upgrade_models;
mod remote_turn_loopback_tls;
mod supervise_conformance_account_rotation;
mod supervise_conformance_codex_rate_nudge;
mod supervise_conformance_done_check;
mod supervise_conformance_model_switch;
mod supervise_conformance_network_wall;
mod supervise_conformance_turn_end;
mod supervise_conformance_usage_wall;
mod vendor_corpus;
