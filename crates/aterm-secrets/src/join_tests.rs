// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const RECIPIENT: &str = "age1zvkyg2lqzraa2lnjvqej32nkuu0ues2s82hzrye869xeexvn73equnujwj";

#[test]
fn a_proof_binds_the_vault_the_key_and_the_host() {
    let key = Key::derive("alab", b"correct horse battery");
    let proof = key.prove("alab", RECIPIENT, "m9");
    assert!(key.verify("alab", RECIPIENT, "m9", &proof));
    assert!(
        !key.verify("alab", RECIPIENT, "m10", &proof),
        "another host"
    );
    assert!(
        !key.verify("other", RECIPIENT, "m9", &proof),
        "another vault"
    );
    let other = RECIPIENT.replace("age1zv", "age1zw");
    assert!(!key.verify("alab", &other, "m9", &proof), "another key");
    let wrong = Key::derive("alab", b"correct horse battery staple");
    assert!(
        !wrong.verify("alab", RECIPIENT, "m9", &proof),
        "another password"
    );
    assert!(!key.verify("alab", RECIPIENT, "m9", "not base64"));
    assert!(!key.verify("alab", RECIPIENT, "m9", ""));
}

#[test]
fn a_line_break_inside_a_field_never_moves_a_proof_to_another_request() {
    let key = Key::derive("alab", b"correct horse battery");
    let proof = key.prove("alab", "age1x\nm9", "z");
    assert!(!key.verify("alab", "age1x", "m9\nz", &proof));
    assert!(!key.verify("alab", "age1x\nm9", "z", &proof));
    let proof = key.prove("a\nb", RECIPIENT, "m9");
    assert!(!key.verify("a", &format!("b\n{RECIPIENT}"), "m9", &proof));
}

#[test]
fn blanks_around_a_password_do_not_count() {
    let typed = Key::derive("alab", b"  correct horse battery\n");
    let stored = Key::derive("alab", b"correct horse battery");
    assert_eq!(
        typed.prove("alab", RECIPIENT, "m9"),
        stored.prove("alab", RECIPIENT, "m9")
    );
}

#[test]
fn generated_passwords_are_strong_distinct_and_grouped() {
    let a = generate().expect("random");
    let b = generate().expect("random");
    assert_ne!(a.as_slice(), b.as_slice());
    let text = a.as_str().expect("utf-8");
    assert_eq!(text.len(), 23);
    assert_eq!(text.split('-').count(), 4);
    assert!(strength(a.as_slice()).is_ok());
}

#[test]
fn weak_passwords_are_refused_without_echoing_them() {
    for weak in [&b"short"[..], b"   eleven-ch   ", b"line\nbreak-here!", b""] {
        let why = strength(weak).expect_err("refused");
        assert!(!why.contains("short") && !why.contains("eleven"), "{why}");
    }
    assert!(strength(b"twelve-chars").is_ok());
    assert!(strength(&[b'x'; 257]).is_err());
}

#[test]
fn debug_never_shows_the_key() {
    let key = Key::derive("alab", b"correct horse battery");
    assert_eq!(format!("{key:?}"), "join::Key(<redacted>)");
}
