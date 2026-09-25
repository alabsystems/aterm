// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Product-version identity shared by every surface of the one `aterm` binary.
//!
//! There is exactly ONE version: `[workspace.package] version` in Cargo.toml,
//! always `MAJOR.MINOR.0` (VERSIONING.md). Every build reports it as written —
//! a release, a development build and a public-source build alike — and it is
//! also the release's `vMAJOR.MINOR.0` tag and its `aterm-<version>.dmg` asset
//! name. A release differs from a development build by its ledger build number
//! and the cutter's release marker (`aterm-gui`'s `build_info::IS_RELEASE_BUILD`),
//! never by a second version.

/// Version shown by the application, command line, diagnostics, and terminal
/// protocol identity: `[workspace.package] version`, as written.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `[workspace.package] version` from a manifest's text, or `None`.
    fn workspace_package_version(manifest: &str) -> Option<&str> {
        let mut in_package = false;
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_package = line == "[workspace.package]";
            } else if in_package && let Some(value) = line.strip_prefix("version") {
                return value
                    .trim_start()
                    .strip_prefix('=')?
                    .trim()
                    .strip_prefix('"')?
                    .split('"')
                    .next();
            }
        }
        None
    }

    /// The binary reports the workspace version exactly as Cargo.toml writes
    /// it. Negative control: the same reader over a manifest whose workspace
    /// version differs from this build's reads that other version, so the
    /// equality below is a comparison and not a tautology.
    #[test]
    fn the_app_version_is_the_workspace_version_as_written() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let manifest = std::fs::read_to_string(workspace.join("Cargo.toml"))
            .expect("read the workspace manifest");
        assert_eq!(workspace_package_version(&manifest), Some(APP_VERSION));

        let other = "[package]\nversion = \"9.9.9\"\n\n[workspace.package]\nversion = \"0.1.0\"\n";
        assert_eq!(workspace_package_version(other), Some("0.1.0"));
        assert_ne!(workspace_package_version(other), Some(APP_VERSION));
    }

    fn rust_sources(root: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root).expect("read shipped crate source") {
            let path = entry.expect("read shipped crate entry").path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn shipped_crates_cannot_bypass_the_shared_app_version() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let mut sources = Vec::new();
        for name in ["aterm", "aterm-cli", "aterm-ctl", "aterm-core", "aterm-gui"] {
            let crate_root = workspace.join("crates").join(name);
            rust_sources(&crate_root.join("src"), &mut sources);
            let build_script = crate_root.join("build.rs");
            if build_script.is_file() {
                sources.push(build_script);
            }
        }
        let needle = "CARGO_PKG_VERSION";
        // AUTHORIZED second readers, each with the reason it cannot go through
        // APP_VERSION. A build script runs before its crate's dependencies are
        // built, so it CANNOT link aterm-types — and `CARGO_PKG_VERSION_*` in
        // a build script is the same workspace `version` APP_VERSION is, read
        // at the only time a build script can read anything. The allowlist is
        // exact paths, so a NEW bypass still fails loudly.
        let authorized: &[&str] = &[
            // The Windows VS_VERSION_INFO resource embedder.
            "crates/aterm/build.rs",
        ];
        let offenders: Vec<_> = sources
            .into_iter()
            .filter(|path| {
                !authorized
                    .iter()
                    .any(|ok| path.ends_with(std::path::Path::new(ok)))
            })
            .filter(|path| {
                std::fs::read_to_string(path)
                    .expect("read shipped Rust source")
                    .contains(needle)
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "shipped identity bypasses aterm_types::version::APP_VERSION: {offenders:?}"
        );
    }
}
