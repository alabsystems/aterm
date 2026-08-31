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
/// share it, so after the first Cargo freshness check the other suite is warm.
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
