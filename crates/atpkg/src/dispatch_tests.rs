// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn pairs_map_to_their_strategy_and_the_rest_fail_closed() {
    use ApplyStrategy::{AppBundle, Shim, SysrootBundle, Unknown, VendorApp};
    for (kind, protocol, want) in [
        ("binary", "github-release", Shim),
        ("elf-binary", "github-release", Shim),
        ("cargo-src", "github-release", Shim),
        ("sysroot-bundle", "github-release", SysrootBundle),
        ("app-bundle", "github-release", AppBundle),
        ("binary", "https", Shim),
        ("elf-binary", "https", Unknown),
        ("app-bundle", "https", VendorApp),
        ("sysroot-bundle", "https", Unknown),
        ("cargo-src", "https", Unknown),
        ("binary", "pkg", Unknown),
        ("system-package", "softwareupdate", Unknown),
        ("installer-pkg", "github-release", Unknown),
        ("binary", "HTTPS", Unknown),
        ("Binary", "github-release", Unknown),
        ("", "github-release", Unknown),
        ("binary", "", Unknown),
    ] {
        assert_eq!(strategy_for(kind, protocol), want, "{kind}/{protocol}");
    }
}
