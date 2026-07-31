// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bundled-seed discovery — the OFFLINE batteries-included lane (§9.1, §11).
//!
//! A release cut may seal a signed seed registry (the flat `tools/atpkg-*.sh`
//! output: `index.toml`(`.sig`), `pkg-*.toml`(`.sig`), artifact tarballs) into
//! the app bundle at `Contents/Resources/toolchain-seed`. This module resolves
//! that directory **co-located with the running executable only** — never via
//! `PATH`, never via config — mirroring the `aterm pkg` → `atpkg` co-location
//! rule (§10): only a payload sealed under the app's own code signature can
//! serve as the bundled source. The bytes it holds still buy **zero trust**:
//! they are served through [`crate::DirFetcher`] into the identical
//! verify-before-parse + freshness + floor + sha256 + `tree_root` gates as any
//! network registry, under the same pinned root key. No key, no installs.
//!
//! `ATPKG_BUNDLED_SEED=<dir>` overrides for dev/tests (`ATPKG_BUNDLED_SEED=0`
//! disables the probe); the override is validated exactly like the co-located
//! probe — a directory that does not hold a signed index pair resolves to
//! nothing rather than a half-registry.

use std::path::{Path, PathBuf};

/// The seed directory name under the bundle's `Resources/` (macOS) or beside
/// the executable (flat layouts). One name everywhere: the release cutter, the
/// probe below, and the docs must agree on this string.
pub const SEED_DIR_NAME: &str = "toolchain-seed";

/// A directory only counts as a seed registry when the signed index PAIR is
/// present — `index.toml` alone (or an empty dir) is not a registry, and
/// reporting one would let the status surface out-run the proof.
fn holds_signed_index(dir: &Path) -> bool {
    dir.join("index.toml").is_file() && dir.join("index.toml.sig").is_file()
}

/// Resolve the bundled seed registry, if this executable ships one.
///
/// Probe order:
/// 1. `ATPKG_BUNDLED_SEED` (dev/test override; `0`/`off`/empty disables the
///    whole probe — including co-located discovery, so a test run can assert
///    the no-seed path on a machine whose app bundle carries one);
/// 2. `<exe_dir>/../Resources/toolchain-seed` — the macOS `.app` layout, with
///    the executable at `Contents/MacOS/<bin>`;
/// 3. `<exe_dir>/toolchain-seed` — flat (dev tree, Linux/Windows dist).
///
/// The executable path is canonicalized first so the `atpkg`/`aterm-*` argv0
/// symlink aliases resolve to the real binary's home before the layout walk.
#[must_use]
pub fn bundled_seed_dir() -> Option<PathBuf> {
    match std::env::var("ATPKG_BUNDLED_SEED") {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() || v == "0" || v.eq_ignore_ascii_case("off") {
                return None;
            }
            let dir = PathBuf::from(v);
            return holds_signed_index(&dir).then_some(dir);
        }
        Err(std::env::VarError::NotUnicode(_)) => return None,
        Err(std::env::VarError::NotPresent) => {}
    }
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let exe_dir = exe.parent()?;
    co_located_candidates(exe_dir)
        .into_iter()
        .find(|d| holds_signed_index(d))
}

/// The co-located probe candidates for an executable living in `exe_dir`, in
/// order. Split from [`bundled_seed_dir`] so the layout walk is unit-testable
/// without a real `.app` or process-global env.
#[must_use]
pub fn co_located_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    if let Some(contents) = exe_dir.parent() {
        out.push(contents.join("Resources").join(SEED_DIR_NAME));
    }
    out.push(exe_dir.join(SEED_DIR_NAME));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("atpkg-bundled-tests")
            .join(format!("{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn candidates_prefer_the_app_bundle_resources_layout() {
        let exe_dir = Path::new("/Applications/aterm.app/Contents/MacOS");
        let c = co_located_candidates(exe_dir);
        assert_eq!(
            c[0],
            Path::new("/Applications/aterm.app/Contents/Resources/toolchain-seed")
        );
        assert_eq!(
            c[1],
            Path::new("/Applications/aterm.app/Contents/MacOS/toolchain-seed")
        );
    }

    #[test]
    fn a_dir_without_the_signed_pair_is_not_a_registry() {
        let d = scratch("no-pair");
        assert!(!holds_signed_index(&d));
        std::fs::write(d.join("index.toml"), b"schema = 1").unwrap();
        // index.toml alone is NOT a registry — the sig must be present too.
        assert!(!holds_signed_index(&d));
        std::fs::write(d.join("index.toml.sig"), [0u8; 64]).unwrap();
        assert!(holds_signed_index(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
