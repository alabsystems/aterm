// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Finding THE toolchain — the Trust stage2 tree, which is what
//! `rust-toolchain.toml`'s `trust` pin actually resolves to. rustup is not
//! required and is not the pin's ground truth.
//!
//! Three rules carried over from the script, all load-bearing:
//!
//! 1. RESOLVE THE PHYSICAL PATH. `build/host` is commonly a target-triple
//!    symlink and Trust's drivers reject a symlinked toolchain path, so the
//!    stage2 directory is canonicalised before anything selects a tool out of it
//!    or puts it on PATH.
//! 2. PATH FIRST. Whatever cargo wins the caller's PATH otherwise (Homebrew's,
//!    typically) drives a stable rustc that rejects the workspace's `-Z` flags,
//!    and every stage then fails for a reason that has nothing to do with the
//!    code. Prepending also makes the driver's own children — trustc, build
//!    scripts that re-invoke it — resolve the trust-named tools.
//! 3. DRIVE `targo`, NOT `cargo`. They are the same binary switching on argv0:
//!    as `cargo` it accepts a bare verb and picks a lane silently; as `targo` it
//!    REFUSES one, because an artifact is either `targo trust <verb>` (verified,
//!    fail-closed) or `--unverified` (no proof claim) — never implicitly either.
//!    Riding the compat name would make this gate quietly unverified, which is
//!    the exact thing the two-lane design prevents. Every invocation names its
//!    lane; the workspace rides `--unverified` until the Trust-Std campaign
//!    greens, the same statement `.cargo/config.toml`'s off-switch already makes.
//!
//! Fail-closed: no targo means the gate FAILS honestly, never a stock-cargo pass.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::is_executable_file;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// The resolved (physical) stage2 `bin` directory.
    pub stage2_dir: PathBuf,
    pub targo: PathBuf,
    pub trustdoc: PathBuf,
    /// `targo-tippy`, or the older `targo-clippy`, if either is installed.
    pub tippy: Option<PathBuf>,
}

impl Toolchain {
    /// `$TRUST_STAGE2_BIN`, defaulting to `$HOME/trust/build/host/stage2/bin`,
    /// canonicalised when it exists.
    #[must_use]
    pub fn discover(stage2_bin: Option<&Path>, home: &Path) -> Self {
        let declared = stage2_bin.map_or_else(
            || home.join("trust/build/host/stage2/bin"),
            Path::to_path_buf,
        );
        let stage2_dir = if declared.is_dir() {
            std::fs::canonicalize(&declared).unwrap_or(declared)
        } else {
            declared
        };
        let tippy = ["targo-tippy", "targo-clippy"]
            .into_iter()
            .map(|n| stage2_dir.join(n))
            .find(|p| is_executable_file(p));
        Self {
            targo: stage2_dir.join("targo"),
            trustdoc: stage2_dir.join("trustdoc"),
            tippy,
            stage2_dir,
        }
    }

    /// Is the verified driver actually there? Every cargo-shaped stage asks this
    /// first, and none of them falls back to a stock cargo.
    #[must_use]
    pub fn have_targo(&self) -> bool {
        is_executable_file(&self.targo)
    }

    /// A built Trust stage2 names its documentation driver `trustdoc`. Bound
    /// through `RUSTDOC` for the test and doctest stages only; without it Cargo
    /// keeps its normal rustdoc discovery and any inability to run doctests
    /// remains a real gate failure rather than a skip.
    #[must_use]
    pub fn have_trustdoc(&self) -> bool {
        is_executable_file(&self.trustdoc)
    }

    /// PATH for every child: the stage2 directory first, but only when a `targo`
    /// really lives there — the script guarded the export the same way, so a
    /// stale `TRUST_STAGE2_BIN` cannot shadow the caller's tools with nothing.
    #[must_use]
    pub fn path_with_stage2_first(&self, inherited: &OsStr) -> OsString {
        if !self.have_targo() {
            return inherited.to_os_string();
        }
        let mut p = OsString::from(self.stage2_dir.as_os_str());
        if !inherited.is_empty() {
            p.push(":");
            p.push(inherited);
        }
        p
    }

    /// The diagnostic for a stage2 that is absent — or mid-rebuild, which empties
    /// the directory and refills it at the end.
    #[must_use]
    pub fn missing_targo_label(&self) -> String {
        format!(
            "targo not found at {} (build the Trust stage2: python3 x.py build --stage 2 in $HOME/trust, or set TRUST_STAGE2_BIN)",
            self.targo.display()
        )
    }

    #[must_use]
    pub fn missing_tippy_label(&self) -> String {
        format!(
            "tippy lint (Trust stage2 toolchain not built — looked for targo-tippy and targo-clippy in {})",
            self.stage2_dir.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn exec_stub(path: &Path) {
        fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn the_default_location_is_the_stage2_tree_under_home() {
        let t = Toolchain::discover(None, Path::new("/nonexistent-home"));
        assert_eq!(
            t.targo,
            Path::new("/nonexistent-home/trust/build/host/stage2/bin/targo")
        );
        assert_eq!(
            t.trustdoc,
            Path::new("/nonexistent-home/trust/build/host/stage2/bin/trustdoc")
        );
        assert!(!t.have_targo());
        assert!(t.tippy.is_none());
    }

    #[test]
    fn a_symlinked_stage2_resolves_to_its_physical_path() {
        // Trust's drivers reject a symlinked toolchain path, so `build/host` —
        // usually a target-triple symlink — must be resolved before use.
        let tmp = crate::mktemp_dir("atv-tc").expect("mktemp");
        let real = tmp.join("aarch64-apple-darwin/stage2/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        std::os::unix::fs::symlink(tmp.join("aarch64-apple-darwin"), tmp.join("host")).expect("ln");

        let via_link = tmp.join("host/stage2/bin");
        let t = Toolchain::discover(Some(&via_link), Path::new("/unused"));
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));
        assert!(
            !t.stage2_dir.to_string_lossy().contains("/host/"),
            "the symlinked spelling must not survive into the tool paths"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn tippy_prefers_the_current_name_and_accepts_the_old_one() {
        let tmp = crate::mktemp_dir("atv-tippy").expect("mktemp");
        // The lookup happens in the RESOLVED directory (see the symlink test):
        // on macOS /tmp is itself a symlink to /private/tmp.
        let real = fs::canonicalize(&tmp).expect("canonicalize");
        exec_stub(&tmp.join("targo-clippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"));
        assert_eq!(
            t.tippy.as_deref(),
            Some(real.join("targo-clippy").as_path())
        );

        exec_stub(&tmp.join("targo-tippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"));
        assert_eq!(
            t.tippy.as_deref(),
            Some(real.join("targo-tippy").as_path()),
            "the Trust fork's own name wins when both exist"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn path_is_only_rewritten_when_a_targo_is_really_there() {
        let tmp = crate::mktemp_dir("atv-path").expect("mktemp");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"));
        assert_eq!(
            t.path_with_stage2_first(OsStr::new("/usr/bin")),
            OsString::from("/usr/bin")
        );

        exec_stub(&tmp.join("targo"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"));
        let want = format!("{}:/usr/bin", t.stage2_dir.display());
        assert_eq!(
            t.path_with_stage2_first(OsStr::new("/usr/bin")),
            OsString::from(want)
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_directory_named_targo_is_not_a_driver() {
        let tmp = crate::mktemp_dir("atv-dir").expect("mktemp");
        fs::create_dir_all(tmp.join("targo")).expect("mkdir");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"));
        assert!(
            !t.have_targo(),
            "fail-closed: a directory is not the verified driver"
        );
        fs::remove_dir_all(&tmp).ok();
    }
}
