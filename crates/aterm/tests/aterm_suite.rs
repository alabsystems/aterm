// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm integration files as modules of ONE test binary: each file stays
//! at its own path and keeps its own `#[test]`s; the `support/` fixtures are
//! declared once here. Each file's headless boots key their scratch worlds on
//! a prefix of their own plus this process's pid, so they share it safely.
//! Separate `[[test]]`s: `conn_live_headless` (re-execs its binary with an
//! `--exact` filter), `control_lanes_live_headless` (raises this process's
//! descriptor limit), `front_door_verbs` and `session_launch_silence`
//! (inheritable pty descriptors a sibling's child could hold open).
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm --test aterm_suite -- <file>::`.

#[cfg(unix)]
#[path = "support/child_output.rs"]
mod child_output;
#[cfg(unix)]
#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

#[cfg(unix)]
mod agent_verdict_live_headless;
#[cfg(unix)]
mod crash_marker_live_headless;
#[cfg(unix)]
mod fences_live_headless;
#[cfg(target_os = "macos")]
mod headless_activation;
#[cfg(unix)]
mod headless_teardown_live;
#[cfg(unix)]
mod host_live;
#[cfg(unix)]
mod host_live_program;
#[cfg(unix)]
mod lifeline_live_headless;
#[cfg(unix)]
mod messages_live_headless;
#[cfg(unix)]
mod operator_live_headless;
mod protected_spawn;
#[cfg(unix)]
mod reply_grammar_live_headless;
#[cfg(unix)]
mod retired_hook_shape;
#[cfg(unix)]
mod shell_exit_at_start_live_headless;
#[cfg(unix)]
mod supervise_turn_end_live_headless;
#[cfg(unix)]
mod tab_zero_master_live_headless;
mod win_resources;
#[cfg(windows)]
mod windows_console_image;
