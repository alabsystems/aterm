// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PINNED CHROME FOR TESTING, ONE SOURCE: the release, and each platform's archive held to
//! its sha256 and size, that every fetch of it is checked against. Two builds of the one
//! release are pinned: the chrome-headless-shell ([`PINNED`]), which the browser lane's `verify
//! provision` (`aterm-verify`) and the harness's own fetch for its headless sign-ins
//! (`aterm-agent`'s `harness::headless_fetch`) place; and the full Chrome for Testing that opens
//! a window ([`WINDOW_PINNED`]), which the harness fetches for `aterm harness accounts browser`
//! where no such Chromium is installed. A pin bump here moves all of them.

use std::path::{Path, PathBuf};

/// The Chrome for Testing release the pin names, a Stable-channel build.
pub const PIN_VERSION: &str = "155.0.8059.84";

/// One platform's pinned archive (`chrome-headless-shell-<platform>.zip` in [`PINNED`],
/// `chrome-<platform>.zip` in [`WINDOW_PINNED`]): its sha256 and size, measured on the download
/// and cross-checked against the bucket's own MD5 (`x-goog-hash`) when pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned {
    /// Chrome for Testing's platform name ([`platform_for`]).
    pub platform: &'static str,
    /// The archive's sha256, lowercase hex.
    pub sha256: &'static str,
    /// The archive's size in bytes.
    pub bytes: u64,
}

/// The pinned chrome-headless-shell archives, one per platform the pin covers.
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

/// One platform's pinned full Chrome for Testing, `chrome-<platform>.zip` of [`PIN_VERSION`]:
/// the build that opens a window. Pinned 2026-10-25 on m21 (macos-aarch64) by downloading each
/// archive once from [`window_archive_url`] with `/usr/bin/curl` (https only, TLS 1.2): each
/// size is the download's and the bucket's `content-length`, each sha256 the download's, and
/// each download's MD5 matched the bucket's own (`x-goog-hash: md5=`, base64): mac-arm64
/// `8XxMUieCfrIHkqZkSqEGhA==`, mac-x64 `OUxAUJMlEwhVDqWzJTZQrQ==`, linux64
/// `rYY+iIyK2e7jbGu/IYAIGQ==`, linux-arm64 `cLnDWrmf5pMQd2uyHd8rjw==`. Unpacked they hold
/// 379 MB (mac-arm64), 390 MB (mac-x64), 418 MB (linux64) and 416 MB (linux-arm64).
pub const WINDOW_PINNED: [Pinned; 4] = [
    Pinned {
        platform: "mac-arm64",
        sha256: "13b9e98ce56f5ce18e100a009f455c9285f399e7c9dc8a76fdf2f3a9755bb5f2",
        bytes: 192_672_771,
    },
    Pinned {
        platform: "mac-x64",
        sha256: "d805a173118daa5f6f5cf464500789e1e00022a9a6aed8abfec821ab828fa862",
        bytes: 203_150_281,
    },
    Pinned {
        platform: "linux64",
        sha256: "cf0f049892dea22e6a6db26d47ed0d7776b7158edcbec4634086633df8bafcbc",
        bytes: 199_014_491,
    },
    Pinned {
        platform: "linux-arm64",
        sha256: "1692a407908867de88eb1f1b2a93ca47c348d8fb2e3c04752d966e0f2e0344b4",
        bytes: 198_807_353,
    },
];

/// The pinned full Chrome for Testing of `(os, arch)`, if the pin covers it.
#[must_use]
pub fn window_pin_for(os: &str, arch: &str) -> Option<&'static Pinned> {
    let platform = platform_for(os, arch)?;
    WINDOW_PINNED.iter().find(|p| p.platform == platform)
}

/// Where Chrome for Testing serves `platform`'s pinned full build.
#[must_use]
pub fn window_archive_url(platform: &str) -> String {
    format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{PIN_VERSION}/{platform}/\
         chrome-{platform}.zip"
    )
}

/// The unpacked full build's directory under a browser cache `root`: `chrome-<version>-<platform>`,
/// the versioned folder the window browser's finder reads.
#[must_use]
pub fn window_pinned_dir(root: &Path, platform: &str) -> PathBuf {
    root.join(format!("chrome-{PIN_VERSION}-{platform}"))
}

/// The executable inside an unpacked full build's directory `dir` ([`window_pinned_dir`]): the
/// app bundle's on macOS, `chrome` on Linux.
#[must_use]
pub fn window_pinned_binary(dir: &Path, platform: &str) -> PathBuf {
    if platform.starts_with("mac-") {
        dir.join(format!(
            "chrome-{platform}/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for \
             Testing"
        ))
    } else {
        dir.join(format!("chrome-{platform}/chrome"))
    }
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

    /// Every platform the headless shell's pin covers has a full build pinned beside it, from
    /// the same release, with its own sha256 and size, served beside the shell and laid out as
    /// the archive unpacks.
    #[test]
    fn each_pinned_host_has_a_window_build() {
        for shell in &PINNED {
            let full = WINDOW_PINNED
                .iter()
                .find(|p| p.platform == shell.platform)
                .expect("a full build beside each shell");
            assert_eq!(full.sha256.len(), 64);
            assert!(
                full.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert_ne!(full.sha256, shell.sha256);
            assert!(full.bytes > shell.bytes, "the full build is the larger");
        }
        assert_eq!(WINDOW_PINNED.len(), PINNED.len());
        assert_eq!(
            window_pin_for("macos", "aarch64").map(|p| p.platform),
            Some("mac-arm64")
        );
        assert_eq!(window_pin_for("windows", "x86_64"), None);
        assert_eq!(
            window_archive_url("mac-arm64"),
            format!(
                "https://storage.googleapis.com/chrome-for-testing-public/{PIN_VERSION}/\
                 mac-arm64/chrome-mac-arm64.zip"
            )
        );
        assert_eq!(
            window_pinned_binary(
                &window_pinned_dir(Path::new("/c"), "mac-arm64"),
                "mac-arm64"
            ),
            Path::new(&format!(
                "/c/chrome-{PIN_VERSION}-mac-arm64/chrome-mac-arm64/Google Chrome for Testing.app/\
                 Contents/MacOS/Google Chrome for Testing"
            ))
        );
        assert_eq!(
            window_pinned_binary(&window_pinned_dir(Path::new("/c"), "linux64"), "linux64"),
            Path::new(&format!(
                "/c/chrome-{PIN_VERSION}-linux64/chrome-linux64/chrome"
            ))
        );
    }
}
