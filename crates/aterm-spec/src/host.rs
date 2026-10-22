// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test the HOST cannot run ends typed, never as a pass: a GPU, a platform
//! font or a privilege the machine lacks decides nothing about the code.
//!
//! Defined at the bottom of the graph, in aterm-uds (which this crate depends
//! on), so aterm-uds's own tests reach it without a dependency cycle.
//!
//! The shells the live tests run are found here too (`test_shell`, unix), once for
//! every crate, so a host that names a shell for one crate's tests names it for
//! all of them, and every real shell a test runs is started hermetically
//! ([`shell_command`]): a scratch HOME and XDG base dirs, never the person's.
//!
//! [`TOOLS`] (aterm-uds's table: every outside program aterm runs, why, and who
//! provides it) is where each tool is found and what a COULD NOT RUN for it names
//! (docs/DESIGN-init-system-2026-10-20.md §8).
//!
//! So is the one shell read of what a child holds (`sh_held`, `sh_held_any`, `held_probe`,
//! unix): a child's report of its descriptors comes from its own table, never from a path
//! lookup of `/dev/fd/N`, which on Darwin reads an open descriptor as absent while anything
//! else looks the same number up. A Rust test reads a descriptor of its own with
//! `aterm_uds::spawnfd::descriptor_metadata`.

pub use aterm_uds::host::{
    COULD_NOT_RUN, Lookup, Pin, Provider, TOOLS, Tool, When, could_not_run, could_not_run_as_root,
    could_not_run_without, find_tool, need_tool, refusals_for, tool, tooling_dir,
};
// Gated as aterm-uds builds them for a dependent: `test` is this crate's, not theirs.
#[cfg(unix)]
pub use aterm_uds::host::raise_open_files;
#[cfg(all(unix, debug_assertions))]
pub use aterm_uds::host::{held_probe, sh_held, sh_held_any};

/// HOW OVERSUBSCRIBED THIS HOST'S CPUS ARE, when they are: the worse of the
/// 1- and 5-minute load averages above one runnable thread per online CPU
/// (aterm's own reading of contention, `aterm-gui`'s `machine_contention`,
/// whose stall thresholds stretch by it), said as `load 150.3 over 18
/// CPUs`; `None` on a host with CPU to spare, or where a read fails.
///
/// A live test whose product time budget a trivial script missed asks this
/// before it fails: on an oversubscribed host the miss is the load's, decides
/// nothing about the code, and ends the test COULD NOT RUN with these numbers;
/// on a host with CPU to spare the same miss still FAILS.
// Skip: one libc call through a raw pointer to an array this frame owns.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn oversubscribed() -> Option<String> {
    unsafe extern "C" {
        /// `<stdlib.h>`; declared here, as `aterm-gui` does: aterm-libc does
        /// not export it.
        fn getloadavg(loadavg: *mut f64, nelem: libc::c_int) -> libc::c_int;
    }
    let mut load = [0f64; 2];
    // SAFETY: `load` is a live, writable array of two doubles, and
    // `getloadavg` writes at most the count it is given.
    let n = unsafe { getloadavg(load.as_mut_ptr(), 2) };
    let worst = match n {
        2.. => load[0].max(load[1]),
        1 => load[0],
        _ => return None,
    };
    let cpus = std::thread::available_parallelism().ok()?.get();
    (worst.is_finite() && worst > cpus as f64).then(|| format!("load {worst:.1} over {cpus} CPUs"))
}

/// The variable that names `shell` for the live tests: `ATERM_TEST_FISH` for
/// `fish` ([`Tool::var`]).
///
/// # Panics
///
/// When `shell` is no row of [`TOOLS`].
#[cfg(unix)]
#[must_use]
pub fn test_shell_var(shell: &str) -> String {
    tool(shell).var()
}

/// The `shell` (`bash`, `zsh`, `fish`, `pwsh`) a live test runs, as every tool is
/// found ([`Tool::find_in`]): `$ATERM_TEST_<SHELL>`, then the tree's pinned copy in
/// the tooling cache ([`tooling_dir`]), then its usual install paths, then the
/// first `shell` on `$PATH`; `None` where the host has none. A test that lacks one
/// ends COULD NOT RUN naming its provider ([`could_not_run_without`]).
///
/// # Panics
///
/// When `$ATERM_TEST_<SHELL>` is set to something that is not a file: an operator
/// error, never an absent shell. And when `shell` is no row of [`TOOLS`].
#[cfg(unix)]
#[must_use]
pub fn test_shell(shell: &str) -> Option<std::path::PathBuf> {
    find_tool(shell)
}

/// The hermetic environment of every real shell a test runs, under `scratch`:
/// `HOME` is `scratch`, and the four XDG base directories sit where the XDG
/// spec defaults them inside it (`.config`, `.local/share`, `.local/state`,
/// `.cache`), so a program that ignores the variables still writes in the
/// scratch home. A shell given the person's HOME writes the person's files:
/// `fish -c` with the real HOME created `~/.config/fish/config.fish` on
/// 2026-10-20 (docs/DESIGN-init-system-2026-10-20.md §1.16).
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

/// The two inherited pointers into a home that a shell reads at start besides
/// `HOME`: zsh's `$ZDOTDIR` and non-interactive bash's `$BASH_ENV` (which it
/// sources even under `--noprofile --norc`). [`shell_command`] clears both.
#[cfg(unix)]
pub const SHELL_START_POINTERS: [&str; 2] = ["ZDOTDIR", "BASH_ENV"];

/// `shell` as a test runs it: built by `aterm_uds::spawnfd::command`, in the
/// [`hermetic_env`] of `scratch`, with [`SHELL_START_POINTERS`] cleared. Every
/// real-shell spawn goes through here (§8.6 of the init design). A test that
/// needs one of these elsewhere sets it afterwards, which overrides.
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

    /// §10 S2: the hermetic helper's child prints the scratch HOME and XDG dirs.
    /// NEGATIVE CONTROL: the same child without the helper prints this process's
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
