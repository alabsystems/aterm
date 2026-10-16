// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm integration files as modules of one test binary; the `support/` fixtures are
//! declared once here. Each file's headless boots key their scratch worlds on a prefix of their
//! own plus this process's pid, so they share it safely. A file that needs a process of its own
//! is a separate `[[test]]` in `Cargo.toml`, which says why. With `autotests = false`, a
//! `tests/*.rs` file listed in neither place never compiles.
//!
//! Run one file's tests with `targo --unverified test -p aterm --test aterm_suite -- <file>::`.

#[cfg(unix)]
#[path = "support/child_output.rs"]
mod child_output;
#[cfg(unix)]
#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;
#[cfg(unix)]
#[path = "support/live_ctl.rs"]
mod live_ctl;
#[cfg(unix)]
#[path = "support/standin.rs"]
mod standin;

#[cfg(unix)]
mod agent_verdict_live_headless;
#[cfg(unix)]
mod crash_marker_live_headless;
#[cfg(unix)]
mod fences_live_headless;
#[cfg(target_os = "macos")]
mod foreground_handback_live_headless;
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
#[cfg(target_os = "macos")]
mod render_desync_live_headless;
#[cfg(unix)]
mod reply_grammar_live_headless;
#[cfg(unix)]
mod retired_hook_shape;
#[cfg(unix)]
mod shell_exit_at_start_live_headless;
#[cfg(unix)]
mod startup_prepare_live_headless;
#[cfg(unix)]
mod supervise_turn_end_live_headless;
#[cfg(unix)]
mod tab_spawn_live_headless;
#[cfg(unix)]
mod tab_zero_master_live_headless;
mod win_resources;
#[cfg(target_os = "macos")]
mod window_server_census_live_headless;
#[cfg(windows)]
mod windows_console_image;
