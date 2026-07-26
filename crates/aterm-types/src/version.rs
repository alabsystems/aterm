// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Product-version identity shared by every surface of the one `aterm` binary.
//!
//! There is exactly ONE version lineage: Cargo's `[workspace.package]`
//! `MAJOR.MINOR.0`. Ordinary development builds report it verbatim; a
//! RELEASE reports the same number with the DEV component reset to `0`, which
//! is also its `vMAJOR.MINOR.PATCH` tag and its `aterm-<version>.dmg` asset
//! name. `cargo ship cut` derives that release version from Cargo.toml and
//! hands it to the build through `ATERM_APP_RELEASE_VERSION`.

/// Cargo package version for the source tree being compiled.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Select the release-cutter-owned app version when present, otherwise the
/// source package version.
#[must_use]
pub const fn select_app_version(
    package_version: &'static str,
    release_version: Option<&'static str>,
) -> &'static str {
    match release_version {
        Some(version) => {
            assert!(
                valid_private_app_version(version),
                "ATERM_APP_RELEASE_VERSION must be MAJOR.MINOR.PATCH"
            );
            version
        }
        None => package_version,
    }
}

/// Exactly three canonical numeric components: non-empty, ASCII digits only,
/// and no leading zero unless the component IS `"0"` — so one release version
/// has exactly ONE spelling and can never be admitted twice.
const fn valid_private_app_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    let mut index = 0;
    // Component-local state: where this component started, so the leading-zero
    // rule can be applied at each boundary.
    let mut start = 0;
    let mut components = 0;
    while index <= bytes.len() {
        let at_end = index == bytes.len();
        if at_end || bytes[index] == b'.' {
            let len = index - start;
            if len == 0 {
                return false;
            }
            if len > 1 && bytes[start] == b'0' {
                return false;
            }
            components += 1;
            start = index + 1;
        } else if !bytes[index].is_ascii_digit() {
            return false;
        }
        index += 1;
    }
    components == 3
}

/// Version shown by the application, command line, diagnostics, and terminal
/// protocol identity.
///
/// `ATERM_APP_RELEASE_VERSION` is set to the canonical `MAJOR.MINOR.PATCH`
/// release version (the workspace version with DEV reset to 0) by the release
/// cutter on both architecture builds. It is deliberately absent from ordinary
/// builds, which report the workspace's `MAJOR.MINOR.0` version as-is.
pub const APP_VERSION: &str =
    select_app_version(PACKAGE_VERSION, option_env!("ATERM_APP_RELEASE_VERSION"));

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn release_override_and_source_fallback_are_distinct() {
        assert_eq!(select_app_version("0.2.1", None), "0.2.1");
        assert_eq!(select_app_version("0.2.1", Some("0.2.0")), "0.2.0");
    }

    #[test]
    #[should_panic(expected = "ATERM_APP_RELEASE_VERSION must be MAJOR.MINOR.PATCH")]
    fn malformed_release_override_is_rejected() {
        // The retired two-component private-app spelling is no longer a
        // version — the scheme is MAJOR.MINOR.PATCH, period.
        let _ = select_app_version("0.2.1", Some("0.59"));
    }

    #[test]
    fn version_shape_is_exactly_three_canonical_components() {
        assert!(valid_private_app_version("0.2.0"));
        assert!(valid_private_app_version("10.20.30"));
        assert!(valid_private_app_version("0.0.0"));
        // Wrong component count.
        assert!(!valid_private_app_version("0.59"));
        assert!(!valid_private_app_version("1"));
        assert!(!valid_private_app_version("1.2.3.4"));
        // Empty components.
        assert!(!valid_private_app_version(""));
        assert!(!valid_private_app_version("..."));
        assert!(!valid_private_app_version("1..3"));
        assert!(!valid_private_app_version(".2.3"));
        assert!(!valid_private_app_version("1.2."));
        // Non-canonical leading zeros.
        assert!(!valid_private_app_version("01.2.3"));
        assert!(!valid_private_app_version("1.02.3"));
        assert!(!valid_private_app_version("1.2.03"));
        // Non-numeric.
        assert!(!valid_private_app_version("1.2.3-rc1"));
        assert!(!valid_private_app_version("v1.2.3"));
    }

    #[test]
    fn compiled_app_version_is_nonempty() {
        assert!(!APP_VERSION.is_empty());
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
        let offenders: Vec<_> = sources
            .into_iter()
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
