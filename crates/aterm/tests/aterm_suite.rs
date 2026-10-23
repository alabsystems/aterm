// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm integration files as modules of one test binary; the `support/` fixtures are
//! declared once here. Each file's headless boots key their scratch worlds on a prefix of their
//! own plus this process's pid, so they share it safely. A file that needs a process of its own
//! is a separate `[[test]]` in `Cargo.toml`, which says why. With `autotests = false`, a
//! `tests/*.rs` file listed in neither place never compiles.
//!
//! Run one file's tests with `targo --unverified test -p aterm --test aterm_suite -- <file>::`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

#[cfg(unix)]
#[path = "support/child_output.rs"]
mod child_output;
#[cfg(unix)]
#[path = "support/ctl_accept.rs"]
mod ctl_accept;
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

#[cfg(target_os = "macos")]
mod accounts_upkeep_live_headless;
#[cfg(unix)]
mod agent_prime_live_headless;
#[cfg(unix)]
mod agent_verdict_live_headless;
#[cfg(unix)]
mod amacs_pty;
#[cfg(unix)]
mod app_front_hwkey_live_headless;
#[cfg(unix)]
mod conn_live_headless;
#[cfg(unix)]
mod control_socket_smoke_live_headless;
#[cfg(unix)]
mod crash_marker_live_headless;
#[cfg(unix)]
mod fences_live_headless;
#[cfg(unix)]
mod fleet_live_headless;
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
#[cfg(target_os = "macos")]
mod keeper_restore_live_headless;
#[cfg(unix)]
mod lifeline_live_headless;
#[cfg(unix)]
mod messages_live_headless;
#[cfg(unix)]
mod person_seam_live_headless;
mod protected_spawn;
#[cfg(unix)]
mod quit_signal_live_headless;
#[cfg(target_os = "macos")]
mod render_desync_live_headless;
#[cfg(unix)]
mod reply_grammar_live_headless;
#[cfg(unix)]
mod restore_take_live_headless;
#[cfg(unix)]
mod retired_hook_shape;
#[cfg(unix)]
mod selector_live_headless;
#[cfg(unix)]
mod session0_before_loop_live_headless;
#[cfg(unix)]
mod session_bin_path;
#[cfg(unix)]
mod session_log_journal_live_headless;
#[cfg(unix)]
mod session_log_prev_end_live_headless;
#[cfg(unix)]
mod session_log_transcript_live_headless;
#[cfg(unix)]
mod shell_exit_at_start_live_headless;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod spare_live_headless;
#[cfg(unix)]
mod standin_path_live_headless;
#[cfg(unix)]
mod startup_prepare_live_headless;
#[cfg(unix)]
mod supervise_turn_end_live_headless;
#[cfg(unix)]
mod survey_live_headless;
#[cfg(unix)]
mod tab_spawn_live_headless;
#[cfg(unix)]
mod tab_zero_master_live_headless;
#[cfg(unix)]
mod task_record_live_headless;
#[cfg(unix)]
mod turn_submit_live_headless;
#[cfg(unix)]
mod typed_by_live_headless;
mod win_resources;
#[cfg(target_os = "macos")]
mod window_server_census_live_headless;
#[cfg(windows)]
mod windows_console_image;
#[cfg(unix)]
mod worker_held_live_headless;
#[cfg(unix)]
mod worker_spawn_live_headless;

/// TWICE AS MANY TESTS AT ONCE AS CORES, unless the run names its own count
/// (`RUST_TEST_THREADS`, or `--test-threads`, which libtest reads over it). The live tests here
/// spend their time waiting on the instances they boot, not computing (a supervisor answers a
/// question every 1.8 s, its settles), so at libtest's one test per core the binary was
/// idle-bound: 24.9-25.9 s at 18 threads against 17.5-17.6 s at 36, at loads of 22 to 54, every
/// run green (2026-10-22). Set at load time because a test binary has no other say in its
/// thread count: libtest reads the variable after `main` starts.
#[cfg(unix)]
extern "C" fn widen_test_threads() {
    if std::env::var_os("RUST_TEST_THREADS").is_none() {
        let n = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_mul(2));
        // SAFETY: load time, before `main`: no other thread exists to read the environment.
        unsafe { std::env::set_var("RUST_TEST_THREADS", n.to_string()) };
    }
}

#[cfg(unix)]
#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
#[cfg_attr(
    any(target_os = "linux", target_os = "android"),
    unsafe(link_section = ".init_array")
)]
static WIDEN_TEST_THREADS: extern "C" fn() = widen_test_threads;
