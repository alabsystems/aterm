// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// The frameworks open and every function and constant the two calls take resolves; the
/// calls themselves read this Mac's privacy grants, which a test leaves alone.
#[test]
fn every_symbol_the_calls_take_resolves() {
    let ax = super::open(super::APPLICATION_SERVICES).unwrap();
    let cf = super::open(super::CORE_FOUNDATION).unwrap();
    for name in [
        c"AXIsProcessTrusted",
        c"AXIsProcessTrustedWithOptions",
        c"kAXTrustedCheckOptionPrompt",
    ] {
        assert!(super::symbol(ax, name).is_some(), "{name:?}");
    }
    for name in [
        c"kCFBooleanTrue",
        c"kCFTypeDictionaryKeyCallBacks",
        c"kCFTypeDictionaryValueCallBacks",
        c"CFDictionaryCreate",
        c"CFRelease",
    ] {
        assert!(super::symbol(cf, name).is_some(), "{name:?}");
    }
    assert!(super::symbol(ax, c"AXNoSuchFunction").is_none());
}
