// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! RUSTUP, FOUND ONE WAY: its home, its binary, and the two listings the stock
//! lanes read. The toolchain walk ([`crate::discovery`]) reads rustup's home for the
//! `trust` entry; `aterm-verify`'s STOCK EXCEPTION lanes (the foreign cells, the wasm
//! lanes) and `aterm-release`'s provisioning (through `atpkg::toolchain`) find the
//! `rustup` binary here, so a machine where one finds it is a machine where all do.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::discovery::is_executable_file;

/// Rustup's home: a non-empty `$RUSTUP_HOME` (passed in as `env_rustup_home`), else
/// `<home>/.rustup` — rustup's own rule, which `atpkg::seam::rustup_home_with` (and
/// through it `aterm-release`) calls. A SET-BUT-EMPTY `$RUSTUP_HOME` is unset:
/// `RUSTUP_HOME= cmd` means "the default", never "the current directory".
#[must_use]
pub fn rustup_home(env_rustup_home: Option<&OsStr>, home: &Path) -> PathBuf {
    match env_rustup_home {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".rustup"),
    }
}

/// [`rustup_home`] when `$HOME` may be unknown (`None`): a non-empty `$RUSTUP_HOME`
/// still answers; nothing else does.
#[must_use]
pub fn rustup_home_with(env_rustup_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match home {
        Some(h) => Some(rustup_home(env_rustup_home, h)),
        None => env_rustup_home.filter(|v| !v.is_empty()).map(PathBuf::from),
    }
}

/// Where Homebrew's rustup keg puts the binary, which Homebrew never links onto PATH.
pub const HOMEBREW_RUSTUP: &str = "/opt/homebrew/opt/rustup/bin/rustup";

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

/// The first executable `rustup` on PATH, then where rustup-init installs it
/// (`$CARGO_HOME/bin`, `~/.cargo/bin`), then Homebrew's keg ([`HOMEBREW_RUSTUP`]): a
/// machine provisioned through atpkg need not have `~/.cargo/bin` on PATH. Relative
/// PATH entries and relative homes are passed over, so the answer never depends on the
/// cwd. [`find_rustup_among`] with the keg.
#[must_use]
pub fn find_rustup_in(
    path: Option<&OsStr>,
    cargo_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    find_rustup_among(path, cargo_home, home, Some(Path::new(HOMEBREW_RUSTUP)))
}

/// [`find_rustup_in`] with Homebrew's keg an input: `keg` is looked at last, and only when
/// given, so a caller whose homes are a test's scratch dirs finds nothing of this machine's.
#[must_use]
pub fn find_rustup_among(
    path: Option<&OsStr>,
    cargo_home: Option<&OsStr>,
    home: Option<&OsStr>,
    keg: Option<&Path>,
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
    candidates.extend(keg.map(Path::to_path_buf));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_but_empty_rustup_home_is_the_default() {
        let home = Path::new("/h");
        assert_eq!(rustup_home(None, home), PathBuf::from("/h/.rustup"));
        assert_eq!(
            rustup_home(Some(OsStr::new("")), home),
            PathBuf::from("/h/.rustup")
        );
        assert_eq!(
            rustup_home(Some(OsStr::new("/r")), home),
            PathBuf::from("/r")
        );
    }

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

    /// PATH first, then `$CARGO_HOME/bin`, then `~/.cargo/bin`, then the keg when one
    /// is given; a relative entry and a non-executable `rustup` are passed over.
    #[cfg(unix)]
    #[test]
    fn rustup_is_found_on_path_then_where_rustup_installs_itself() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("atc-rustup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = |p: &Path, mode: u32| {
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, "#!/bin/sh\n").expect("write");
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        let on_path = dir.join("path/rustup");
        let cargo_home = dir.join("cargo-home");
        let home = dir.join("home");
        exe(&on_path, 0o644);
        exe(&cargo_home.join("bin/rustup"), 0o755);
        exe(&home.join(".cargo/bin/rustup"), 0o755);
        let path = std::env::join_paths([PathBuf::from("rel"), dir.join("path")]).expect("join");
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
        // The keg is last, and looked at only when given.
        let keg = dir.join("keg/bin/rustup");
        exe(&keg, 0o755);
        assert_eq!(find_rustup_among(None, None, None, Some(&keg)), Some(keg));
        assert_eq!(find_rustup_among(None, None, None, None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
