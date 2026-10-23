// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! atpkg's integration files as modules of ONE test binary: the pure pins (the tar
//! oracle, the shell-integration hook golden) and the process-edge checks that run the
//! real `atpkg` binary over a scratch store of their own. Each file keeps its path — every
//! citation of `crates/atpkg/tests/<file>.rs` still resolves — and its own `#[test]`s.
//!
//! No file below sets or removes an environment variable (a child gets its environment on
//! its own command), installs a hook or an allocator, re-executes itself, changes
//! directory or opens a pty, and each file's scratch directories carry a prefix of its own
//! (`atpkg-agent-warm-`, `atpkg-lane-verb-`, …). A store lock one of them holds is a
//! `flock` on its own scratch prefix, so the files that race real children on that lock
//! share this binary too. The two that do more — open a pty, re-execute themselves — are
//! their own `[[test]]`s in `Cargo.toml`, each with its reason.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` line here nor a
//! `[[test]]` of its own never compiles.
//!
//! Run one file's tests with
//! `targo --unverified test -p atpkg --test atpkg_suite -- <file>::`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod agent_warm;
mod build_cache;
mod hook_reroute_first;
mod landing_passthrough;
mod lane_verb;
mod lease_verb;
mod machine_edge;
mod machine_verb;
mod one_operand_verbs;
mod reroute_names_what_runs;
mod run_is_the_managed_name;
mod seed_disabled;
mod seed_login_rc;
mod selfupdate_intercept;
mod settings_action_completion;
mod shell_integration_hook_pin;
mod store_lock_wait;
mod tar_corpus;
mod vendor_pending_conformance;
