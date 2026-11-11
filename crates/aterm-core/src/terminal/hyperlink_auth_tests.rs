// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn authorize_scheme_stores_lowercased_and_is_idempotent() {
    let mut auth = HyperlinkAuth::new();
    assert!(auth.authorize_scheme("Orca"));
    assert_eq!(auth.extra_schemes().len(), 1);
    assert_eq!(&*auth.extra_schemes()[0], "orca");
    assert!(auth.authorize_scheme("ORCA"));
    assert_eq!(auth.extra_schemes().len(), 1);
}

#[test]
fn authorize_scheme_bounded_at_max_extra_schemes() {
    let mut auth = HyperlinkAuth::new();
    for s in ["a1", "b2", "c3", "d4"] {
        assert!(auth.authorize_scheme(s));
    }
    assert!(
        !auth.authorize_scheme("e5"),
        "slot {MAX_EXTRA_SCHEMES}+1 must be refused"
    );
    assert_eq!(auth.extra_schemes().len(), MAX_EXTRA_SCHEMES);
    auth.revoke_scheme("a1");
    assert!(auth.authorize_scheme("e5"));
}

#[test]
fn authorize_scheme_refuses_never_allow_incl_case_and_suffix_evasion() {
    let mut auth = HyperlinkAuth::new();
    for s in [
        "javascript",
        "JavaScript",
        "data",
        "file",
        "vbscript",
        "about",
        "blob",
        "javascript.",
        "javascript+",
        "DATA-",
    ] {
        assert!(!auth.authorize_scheme(s), "never-allow must refuse {s:?}");
    }
    assert!(auth.extra_schemes().is_empty());
    assert!(auth.authorize_scheme("javascriptx"));
}

#[test]
fn authorize_scheme_refuses_malformed_shapes_and_overlength() {
    let mut auth = HyperlinkAuth::new();
    for s in [
        "", "1orca", "+orca", ".orca", "-orca", "or ca", "orca ", "orca\t", "or%3aca", "ORCA%3A",
        "orc:a",
    ] {
        assert!(
            !auth.authorize_scheme(s),
            "malformed shape must refuse {s:?}"
        );
    }
    assert!(!auth.authorize_scheme(&"a".repeat(MAX_EXTRA_SCHEME_LEN + 1)));
    assert!(auth.extra_schemes().is_empty());
    assert!(auth.authorize_scheme("web+orca"));
}

#[test]
fn revoke_scheme_is_case_insensitive_and_noop_when_absent() {
    let mut auth = HyperlinkAuth::new();
    assert!(auth.authorize_scheme("orca"));
    auth.revoke_scheme("nonexistent");
    assert_eq!(auth.extra_schemes().len(), 1);
    auth.revoke_scheme("ORCA");
    assert!(auth.extra_schemes().is_empty());
}
