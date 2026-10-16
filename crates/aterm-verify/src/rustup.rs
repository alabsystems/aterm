// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! rustup's toolchains, for the STOCK EXCEPTION lanes: the foreign cells
//! (`verify cells`) and the wasm perf lane. Trust carries only the host std,
//! so a cross triple is compiled by a rustup toolchain that has its std,
//! driven by that toolchain's own `cargo` and `rustc` by path.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::{Command, command};

use crate::is_executable_file;

/// Where [`find_rustup_in`] looks after PATH, for the sentence a skip prints.
pub const RUSTUP_LOOKED: &str = "on PATH, in $CARGO_HOME/bin, in ~/.cargo/bin and in \
                                 Homebrew's rustup keg (/opt/homebrew/opt/rustup/bin)";

/// This process's `rustup` ([`find_rustup_in`] over its own environment).
#[must_use]
pub fn find_rustup() -> Option<PathBuf> {
    find_rustup_in(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("CARGO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The first executable `rustup` on PATH, then where rustup-init and Homebrew
/// install it: a machine provisioned through atpkg need not have
/// `~/.cargo/bin` on PATH. Relative PATH entries are passed over, so the
/// answer never depends on the cwd.
#[must_use]
pub fn find_rustup_in(
    path: Option<&OsStr>,
    cargo_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = path
        .map(|p| {
            std::env::split_paths(p)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("rustup"))
                .collect()
        })
        .unwrap_or_default();
    let absolute = |v: Option<&OsStr>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(cargo_home) = absolute(cargo_home) {
        candidates.push(cargo_home.join("bin/rustup"));
    }
    if let Some(home) = absolute(home) {
        candidates.push(home.join(".cargo/bin/rustup"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/opt/rustup/bin/rustup"));
    candidates.into_iter().find(|p| is_executable_file(p))
}

/// Does `listing` (`rustup target list --installed`) name exactly `target`?
/// Line-exact: `wasm32-wasip1` is not `wasm32-unknown-unknown`.
#[must_use]
pub fn lists_target(listing: &str, target: &str) -> bool {
    listing.lines().any(|l| l.trim() == target)
}

/// The toolchain names in `rustup toolchain list`, without rustup's
/// ` (active, default)` marker.
#[must_use]
pub fn toolchain_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Does `rustup`'s toolchain `name` carry `triple`'s std?
#[must_use]
pub fn carries(rustup: &Path, name: &str, triple: &str) -> bool {
    command(rustup)
        .args(["target", "list", "--installed", "--toolchain", name])
        .output_owned()
        .is_ok_and(|o| {
            o.status.success() && lists_target(&String::from_utf8_lossy(&o.stdout), triple)
        })
}

/// A rustup toolchain resolved to its own drivers by path, never a `cargo`
/// off PATH trusted to honour `RUSTUP_TOOLCHAIN` (only rustup's proxies read
/// it). `RUSTC` pins the compiler for cargo and every build script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    pub name: String,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    /// Whether it has the clippy component (a foreign cell is linted).
    pub clippy: bool,
}

impl Toolchain {
    /// `name`'s drivers, as `rustup which --toolchain` answers them.
    ///
    /// # Errors
    /// rustup could not be asked, or names no `cargo`/`rustc` for `name`.
    pub fn resolve(rustup: &Path, name: &str) -> Result<Self, String> {
        let which = |tool: &str| -> Result<PathBuf, String> {
            let out = command(rustup)
                .args(["which", "--toolchain", name, tool])
                .output_owned()
                .map_err(|e| format!("could not ask rustup for `{name}`'s {tool} ({e})"))?;
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if out.status.success() && path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "`rustup which --toolchain {name} {tool}` names no file: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        Ok(Self {
            name: name.to_string(),
            cargo: which("cargo")?,
            rustc: which("rustc")?,
            clippy: which("cargo-clippy").is_ok(),
        })
    }

    /// This toolchain's `cargo`, driving this toolchain's `rustc`.
    #[must_use]
    pub fn cargo(&self) -> Command {
        let mut cmd = command(&self.cargo);
        cmd.env("RUSTC", &self.rustc)
            .env("RUSTUP_TOOLCHAIN", &self.name);
        cmd
    }
}

/// The first installed toolchain carrying `triple`'s std, release channels
/// before nightly (a nightly accepts what a release refuses): `Ok(None)` when
/// none does.
///
/// # Errors
/// rustup could not list its toolchains, or one that carries the std named
/// no drivers.
pub fn with_std(rustup: &Path, triple: &str) -> Result<Option<Toolchain>, String> {
    let list = command(rustup)
        .args(["toolchain", "list"])
        .output_owned()
        .map_err(|e| format!("could not ask rustup which toolchains are installed ({e})"))?;
    if !list.status.success() {
        return Err(format!(
            "`rustup toolchain list` failed: {}",
            String::from_utf8_lossy(&list.stderr).trim_end()
        ));
    }
    let mut names = toolchain_names(&String::from_utf8_lossy(&list.stdout));
    names.sort_by_key(|tc| u8::from(tc.starts_with("nightly")));
    names
        .into_iter()
        .find(|tc| carries(rustup, tc, triple))
        .map(|tc| Toolchain::resolve(rustup, &tc))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_listed_only_by_its_exact_line() {
        let listing = "aarch64-apple-darwin\nwasm32-wasip1\nwasm32-unknown-unknown-nightly\n";
        assert!(lists_target(listing, "aarch64-apple-darwin"));
        assert!(!lists_target(listing, "wasm32-unknown-unknown"));
        assert!(lists_target(
            "  wasm32-unknown-unknown  \n",
            "wasm32-unknown-unknown"
        ));
    }

    #[test]
    fn toolchain_names_drop_rustups_markers() {
        let listing = "stable-aarch64-apple-darwin (active, default)\nnightly-2026-01-01\ntrust\n";
        assert_eq!(
            toolchain_names(listing),
            ["stable-aarch64-apple-darwin", "nightly-2026-01-01", "trust"]
        );
    }

    /// PATH first, then `$CARGO_HOME/bin`, then `~/.cargo/bin`; a relative
    /// entry and a non-executable `rustup` are passed over.
    #[cfg(unix)]
    #[test]
    fn rustup_is_found_on_path_then_where_rustup_installs_itself() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-rustup-")
            .tempdir()
            .expect("scratch");
        let exe = |p: &Path, mode: u32| {
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, "#!/bin/sh\n").expect("write");
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        let on_path = dir.path().join("path/rustup");
        let cargo_home = dir.path().join("cargo-home");
        let home = dir.path().join("home");
        exe(&on_path, 0o644);
        exe(&cargo_home.join("bin/rustup"), 0o755);
        exe(&home.join(".cargo/bin/rustup"), 0o755);
        let path =
            std::env::join_paths([PathBuf::from("rel"), dir.path().join("path")]).expect("join");
        assert_eq!(
            find_rustup_in(
                Some(&path),
                Some(cargo_home.as_os_str()),
                Some(home.as_os_str())
            ),
            Some(cargo_home.join("bin/rustup")),
            "a non-executable rustup on PATH is passed over"
        );
        assert_eq!(
            find_rustup_in(Some(&path), None, Some(home.as_os_str())),
            Some(home.join(".cargo/bin/rustup"))
        );
        exe(&on_path, 0o755);
        assert_eq!(
            find_rustup_in(
                Some(&path),
                Some(cargo_home.as_os_str()),
                Some(home.as_os_str())
            ),
            Some(on_path)
        );
    }
}
