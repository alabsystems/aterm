// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rustup's home and its listings, for [`crate::discovery`] and the STOCK EXCEPTION lanes.
//! The rustup binary is the `rustup` row of `aterm_uds::host::TOOLS`, found by that row's
//! resolver alone.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Rustup's home: a non-empty `$RUSTUP_HOME`, else `<home>/.rustup`.
#[must_use]
pub fn rustup_home(env_rustup_home: Option<&OsStr>, home: &Path) -> PathBuf {
    match env_rustup_home {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".rustup"),
    }
}

/// [`rustup_home`] when `$HOME` may be unknown (`None`): a non-empty `$RUSTUP_HOME` still answers.
#[must_use]
pub fn rustup_home_with(env_rustup_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match home {
        Some(h) => Some(rustup_home(env_rustup_home, h)),
        None => env_rustup_home.filter(|v| !v.is_empty()).map(PathBuf::from),
    }
}

/// Does `listing` (`rustup target list --installed`) name exactly `target`? Line-exact.
#[must_use]
pub fn lists_target(listing: &str, target: &str) -> bool {
    listing.lines().any(|l| l.trim() == target)
}

/// The toolchain names in `rustup toolchain list`, without rustup's ` (active, default)` marker.
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
}
