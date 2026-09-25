// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared live-conformance artifact preparation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Resolve a caller-supplied artifact or freshen the shared RELEASE build.
///
/// The dedicated target avoids repeatedly rebuilding dependencies with the
/// outer integration test's different feature set. Paint and spin intentionally
/// share it, and since the package-identity sweep below they really do: after
/// the first build every other caller — and
/// the gate's own priming stage — gets a freshness check measured at 0.2 s,
/// where an alternation between two packages used to relink for 208 s.
pub(crate) fn release_bin(root: &Path, overrides: &[&str]) -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        for var in overrides {
            if let Ok(path) = std::env::var(var) {
                let path = PathBuf::from(path);
                assert!(path.is_file(), "{var}={} does not exist", path.display());
                return path;
            }
        }

        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let target = root.join("target/conformance-release");
        // Under the branded driver a nested invocation must name its own lane:
        // the outer authorization does not propagate to child Cargo processes.
        let mut command = Command::new(&cargo);
        // THE NESTED BUILD MUST NOT INHERIT THIS TEST BINARY'S PACKAGE IDENTITY.
        // Cargo sets `CARGO_MANIFEST_DIR` and the `CARGO_PKG_*` family for the
        // test it is running, and a build script that reads one of them tracks
        // it in its fingerprint: `ring` tracks `CARGO_MANIFEST_DIR`,
        // `CARGO_PKG_NAME` and the version triple, `aterm-update-core` tracks
        // `CARGO_PKG_REPOSITORY`. Forwarded, they make this shared target
        // directory package-specific — so `paint`/`spin` (aterm-conformance)
        // and atpkg's since-deleted `untracked_stage` invalidated each other's
        // copy of the ring → rustls → aterm-update → atpkg → aterm-gui subtree on
        // every run: MEASURED 2026-09-22, 208.9 s of relinking per alternation, and
        // the reason the helper's "the other suite is warm" held only within
        // one package. Removed here, every caller — and the gate stage that
        // primes the same artifact, which has none of these set — spells the
        // same fingerprint.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("CARGO_PKG_")
                || name == "CARGO_MANIFEST_DIR"
                || name == "CARGO_MANIFEST_LINKS"
                || name == "CARGO_MANIFEST_PATH"
            {
                command.env_remove(&key);
            }
        }
        if Path::new(&cargo)
            .file_stem()
            .is_some_and(|name| name.to_string_lossy().starts_with("targo"))
        {
            command.arg("--unverified");
        }
        let status = command
            .args(["build", "--locked", "--release", "-p", "aterm"])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(root)
            .status()
            .unwrap_or_else(|error| {
                panic!("could not spawn `{cargo} build --release -p aterm`: {error}")
            });
        assert!(
            status.success(),
            "`{cargo} build --release -p aterm` failed ({status}) — live conformance judges the \
             RELEASE binary and refuses to run without one (set {} to drive a prebuilt artifact)",
            overrides.join(" or "),
        );
        let binary = target.join("release/aterm");
        assert!(
            binary.is_file(),
            "built the release profile but {} is missing",
            binary.display(),
        );
        binary
    })
    .clone()
}
