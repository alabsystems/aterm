// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PINNED CHROME-HEADLESS-SHELL, ONE SOURCE: the Chrome for Testing release, and each
//! platform's archive held to its sha256 and size, that every fetch of it is checked against:
//! the browser lane's `verify provision` (`aterm-verify`) and the harness's own fetch for its
//! headless sign-ins (`aterm-agent`'s `harness::headless_fetch`). A pin bump here moves both.

use std::path::{Path, PathBuf};

/// The Chrome for Testing release the pin names, a Stable-channel build.
pub const PIN_VERSION: &str = "155.0.8059.84";

/// One platform's pinned `chrome-headless-shell-<platform>.zip`: its sha256 and size, measured on
/// the download and cross-checked against the bucket's own MD5 (`x-goog-hash`) when pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned {
    /// Chrome for Testing's platform name ([`platform_for`]).
    pub platform: &'static str,
    /// The archive's sha256, lowercase hex.
    pub sha256: &'static str,
    /// The archive's size in bytes.
    pub bytes: u64,
}

/// The pinned archives, one per platform the pin covers.
pub const PINNED: [Pinned; 4] = [
    Pinned {
        platform: "mac-arm64",
        sha256: "7f111980bcb41e908d0fa3e969a625e7777ff683c1fc43fd7759d05ec3ffe52b",
        bytes: 102_441_485,
    },
    Pinned {
        platform: "mac-x64",
        sha256: "3c5bf34eb8daa874e8b50edf900eab618b74c1b9596e596340278eeba4ff1ae1",
        bytes: 108_105_176,
    },
    Pinned {
        platform: "linux64",
        sha256: "a5d71b91aa996fef00517cd212b2dfb816b43055edefbe727e6fd562d4ae1f9c",
        bytes: 125_033_980,
    },
    Pinned {
        platform: "linux-arm64",
        sha256: "0c83c2520b513835c712590781adbdbcab4c14ca04d4809be5055ff2c8937a51",
        bytes: 125_197_892,
    },
];

/// Chrome for Testing's platform name for `(os, arch)` as Rust names them, where a pin exists.
#[must_use]
pub fn platform_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        ("linux", "x86_64") => Some("linux64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        _ => None,
    }
}

/// The pinned archive of `(os, arch)`, if the pin covers it.
#[must_use]
pub fn pin_for(os: &str, arch: &str) -> Option<&'static Pinned> {
    let platform = platform_for(os, arch)?;
    PINNED.iter().find(|p| p.platform == platform)
}

/// This host's pinned archive, if the pin covers it.
#[must_use]
pub fn host_pin() -> Option<&'static Pinned> {
    pin_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Where Chrome for Testing serves `platform`'s pinned archive.
#[must_use]
pub fn archive_url(platform: &str) -> String {
    format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{PIN_VERSION}/{platform}/\
         chrome-headless-shell-{platform}.zip"
    )
}

/// The unpacked pin's directory under a browser cache `root`.
#[must_use]
pub fn pinned_dir(root: &Path, platform: &str) -> PathBuf {
    root.join(format!("chrome-headless-shell-{PIN_VERSION}-{platform}"))
}

/// The executable inside an unpacked archive's directory `dir` ([`pinned_dir`]).
#[must_use]
pub fn pinned_binary(dir: &Path, platform: &str) -> PathBuf {
    dir.join(format!(
        "chrome-headless-shell-{platform}/chrome-headless-shell"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_pinned_host_maps_to_its_archive() {
        assert_eq!(platform_for("macos", "aarch64"), Some("mac-arm64"));
        assert_eq!(platform_for("macos", "x86_64"), Some("mac-x64"));
        assert_eq!(platform_for("linux", "x86_64"), Some("linux64"));
        assert_eq!(platform_for("linux", "aarch64"), Some("linux-arm64"));
        assert_eq!(platform_for("windows", "x86_64"), None);
        assert_eq!(pin_for("windows", "x86_64"), None);
        for pin in &PINNED {
            assert_eq!(pin.sha256.len(), 64);
            assert!(
                pin.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert!(pin.bytes > 0);
        }
        assert_eq!(
            pin_for("macos", "aarch64").map(|p| p.platform),
            Some("mac-arm64")
        );
        assert_eq!(
            archive_url("mac-arm64"),
            format!(
                "https://storage.googleapis.com/chrome-for-testing-public/{PIN_VERSION}/\
                 mac-arm64/chrome-headless-shell-mac-arm64.zip"
            )
        );
        assert_eq!(
            pinned_binary(&pinned_dir(Path::new("/c"), "mac-arm64"), "mac-arm64"),
            Path::new(&format!(
                "/c/chrome-headless-shell-{PIN_VERSION}-mac-arm64/chrome-headless-shell-mac-arm64/\
                 chrome-headless-shell"
            ))
        );
    }
}
