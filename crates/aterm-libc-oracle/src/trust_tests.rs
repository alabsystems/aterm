// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_host_is_the_verbose_versions_host_line() {
    let vv = "rustc 1.99.0-dev (321aaeda7 2026-09-17) (trustc 0.1.0)\nbinary: trustc\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\n";
    assert_eq!(parse_host(vv).as_deref(), Some("aarch64-apple-darwin"));
    assert_eq!(parse_host("host: \n"), None);
    assert_eq!(parse_host("binary: trustc\n"), None);
}
