// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn a_class_lookup_registers_the_framework_without_making_a_view() {
    assert!(!super::class(c"WKWebView").is_null());
    assert!(!super::class(c"WKWebViewConfiguration").is_null());
    assert_eq!(
        super::class(c"WKWebsiteDataStore"),
        aterm_objc::class(c"WKWebsiteDataStore")
    );
}
