// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use atpkg_publish::triple::Triple;

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
