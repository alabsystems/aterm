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

/// Returns a description of CPU oversubscription on this host, if any.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn oversubscribed() -> Option<String> {
    unsafe extern "C" {
        /// not export it.
        fn getloadavg(loadavg: *mut f64, nelem: libc::c_int) -> libc::c_int;
    }
    let mut load = [0f64; 2];
    // SAFETY: `load` is a valid writable 2-element array passed to `getloadavg`.
    let n = unsafe { getloadavg(load.as_mut_ptr(), 2) };
    let worst = match n {
        2.. => load[0].max(load[1]),
        1 => load[0],
        _ => return None,
    };
    let cpus = std::thread::available_parallelism().ok()?.get();
    (worst.is_finite() && worst > cpus as f64).then(|| format!("load {worst:.1} over {cpus} CPUs"))
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
mod tests {
    use super::*;

    /// own HOME. `/bin/sh -c printf` reads and writes nothing in either home.
    #[test]
    fn the_hermetic_childs_home_is_the_scratch_and_a_plain_childs_is_not() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let print = r#"printf '%s\n' "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "${ZDOTDIR-unset}" "${BASH_ENV-unset}""#;
        let mut cmd = shell_command("/bin/sh", scratch.path());
        let cleared: Vec<&std::ffi::OsStr> = cmd
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key)
            .collect();
        assert_eq!(cleared, ["BASH_ENV", "ZDOTDIR"].map(std::ffi::OsStr::new));
        let out = cmd.args(["-c", print]).output().expect("/bin/sh");
        assert!(out.status.success(), "{out:?}");
        let home = scratch.path().display().to_string();
        let want: Vec<String> = [
            home.clone(),
            format!("{home}/.config"),
            format!("{home}/.local/share"),
            format!("{home}/.local/state"),
            format!("{home}/.cache"),
            "unset".into(),
            "unset".into(),
        ]
        .into();
        let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(got, want);

        let plain = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", r#"printf '%s' "$HOME""#])
            .output()
            .expect("/bin/sh");
        let real = std::env::var("HOME").unwrap_or_default();
        let got = String::from_utf8_lossy(&plain.stdout);
        assert_eq!(
            got, real,
            "without the helper the child has this process's HOME"
        );
        assert_ne!(got, home, "and that is not the scratch");
    }
}
