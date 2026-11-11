// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host capability checks, tool discovery, and hermetic shell test execution.

pub use aterm_uds::host::{
    COULD_NOT_RUN, Lookup, Pin, Provider, TOOLS, Tool, When, could_not_run, could_not_run_as_root,
    could_not_run_without, find_tool, need_tool, refusals_for, tool, tooling_dir,
};

#[cfg(unix)]
pub use aterm_uds::host::raise_open_files;
#[cfg(all(unix, debug_assertions))]
pub use aterm_uds::host::{held_probe, sh_held, sh_held_any};

/// Returns a description of CPU oversubscription on this host, if any: the worse of its 1- and
/// 5-minute load averages (`aterm_sysprobe::load_average`) past its logical cores
/// (`aterm_sysprobe::cpu_layout`); `None` where either is unread.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn oversubscribed() -> Option<String> {
    let [one, five, _] = aterm_sysprobe::load_average()?;
    let worst = one.max(five);
    let cores = aterm_sysprobe::cpu_layout().logical_cores();
    (cores > 0 && worst > cores as f64).then(|| format!("load {worst:.1} over {cores} CPUs"))
}

/// Returns the environment variable naming `shell` for live tests ([`Tool::var`]).
///
/// # Panics
///
/// When `shell` is no row of [`TOOLS`].
#[cfg(unix)]
#[must_use]
pub fn test_shell_var(shell: &str) -> String {
    tool(shell).var()
}

/// Resolves `shell` (`bash`, `zsh`, `fish`, `pwsh`) for live tests ([`Tool::find_in`]).
///
/// # Panics
///
/// When `$ATERM_TEST_<SHELL>` points to a non-file, or `shell` is no row of [`TOOLS`].
#[cfg(unix)]
#[must_use]
pub fn test_shell(shell: &str) -> Option<std::path::PathBuf> {
    find_tool(shell)
}

/// Returns the hermetic `HOME` and XDG base-directory environment under `scratch`.
#[cfg(unix)]
#[must_use]
pub fn hermetic_env(scratch: &std::path::Path) -> [(&'static str, std::path::PathBuf); 5] {
    [
        ("HOME", scratch.to_path_buf()),
        ("XDG_CONFIG_HOME", scratch.join(".config")),
        ("XDG_DATA_HOME", scratch.join(".local/share")),
        ("XDG_STATE_HOME", scratch.join(".local/state")),
        ("XDG_CACHE_HOME", scratch.join(".cache")),
    ]
}

/// Inherited shell startup variables (`ZDOTDIR`, `BASH_ENV`) cleared by [`shell_command`].
#[cfg(unix)]
pub const SHELL_START_POINTERS: [&str; 2] = ["ZDOTDIR", "BASH_ENV"];

/// Builds a hermetic shell [`aterm_uds::spawnfd::Command`] rooted in `scratch`.
#[cfg(unix)]
#[must_use]
pub fn shell_command(
    shell: impl AsRef<std::ffi::OsStr>,
    scratch: &std::path::Path,
) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command(shell);
    for (key, value) in hermetic_env(scratch) {
        cmd.env(key, value);
    }
    for key in SHELL_START_POINTERS {
        cmd.env_remove(key);
    }
    cmd
}

#[cfg(all(test, unix))]
#[path = "host_tests.rs"]
mod tests;
