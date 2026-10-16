// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use atpkg_publish::triple::{Arch, Os, Triple};

#[test]
fn every_client_target_is_a_triple_and_nothing_else_is() {
    let all: Vec<&str> = Triple::all().map(Triple::as_str).collect();
    assert_eq!(all, atpkg::manifest::TARGETS);
    for t in atpkg::manifest::TARGETS {
        assert_eq!(Triple::parse(t).expect("a client target").as_str(), *t);
    }
    for not in [
        "i686-unknown-linux-gnu",
        "wasm32-unknown-unknown",
        "x86_64",
        "",
    ] {
        assert!(
            Triple::parse(not).is_err(),
            "{not:?} is not a client target"
        );
    }
}

#[test]
fn a_triple_knows_its_os_and_arch() {
    let t = |s| Triple::parse(s).expect("target");
    assert_eq!(t("aarch64-apple-darwin").os(), Os::Darwin);
    assert_eq!(t("x86_64-unknown-linux-gnu").os(), Os::Linux);
    assert_eq!(t("aarch64-pc-windows-msvc").os(), Os::Windows);
    assert_eq!(t("aarch64-unknown-linux-gnu").arch(), Arch::Aarch64);
    assert_eq!(t("x86_64-apple-darwin").arch(), Arch::X86_64);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_host_is_a_client_target() {
    assert!(Triple::host().is_some());
}
