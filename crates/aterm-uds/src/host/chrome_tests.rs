// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
