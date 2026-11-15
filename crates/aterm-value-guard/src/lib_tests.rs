// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guard's own rules, apart from the typed cells that apply them (aterm-db's `typed` tests).

use super::*;

/// A phone number as a person gives one is its digits, 7 to 15 of them, with an optional
/// leading `+` and nothing else: no space, dash, bracket or letter, no second `+`, no digit
/// outside ASCII.
/// NEGATIVE CONTROL: with `is_phone` dropping its upper bound (`digits.len() >= PHONE_DIGITS`),
/// sixteen digits pass: `` "1234567890123456" is no number ``.
#[test]
fn a_given_phone_number_is_seven_to_fifteen_digits_with_an_optional_plus() {
    for number in [
        "5550101",
        "+5550101",
        "15550101234",
        "123456789012345",
        "+123456789012345",
    ] {
        assert!(is_phone(number), "{number:?} is a number");
    }
    for other in [
        "",
        "+",
        "555010",
        "+555010",
        "1234567890123456",
        "+1234567890123456",
        "555 0101",
        "555-0101",
        "555.0101",
        "(202)5550149",
        "+1 202 555 0149",
        " 5550101",
        "5550101\n",
        "5550101x",
        "call5550101",
        "++5550101",
        "5550101+",
        "\u{665}\u{665}\u{665}\u{660}\u{661}\u{660}\u{661}",
    ] {
        assert!(!is_phone(other), "{other:?} is no number");
    }
}

/// What reads as a number some build stored: an importer's digits from four up, or a person's
/// spelling with separators and a leading `+`; nothing holding a letter or another sign, so no
/// word, token or seed that was sealed in a number's cell reads as one.
/// NEGATIVE CONTROL: with `stored_phone` skipping its character check (counting digits only),
/// `` "5550149 pin 4829" is no stored number `` fails.
#[test]
fn a_stored_phone_number_is_digits_and_separators_only() {
    for number in [
        "0149",
        "5550101",
        "2025550149",
        "+15550101234",
        "+1 (202) 555-0149",
        "202.555.0149",
        "123456789012345",
    ] {
        assert!(stored_phone(number), "{number:?} is a stored number");
    }
    for other in [
        "",
        "+",
        "149",
        "1234567890123456",
        "5550149 pin 4829",
        "2025550149x",
        "ext 5550101",
        "5550101+",
        "555\t0101",
        "555\n0101",
        "555_0101",
        "\u{665}\u{665}\u{665}\u{660}\u{661}\u{660}\u{661}",
    ] {
        assert!(!stored_phone(other), "{other:?} is no stored number");
    }
}

/// A URL that carries a password is shaped like a credential wherever it stands; a URL naming
/// only a user, an `@` in a path and an address in prose are not.
/// NEGATIVE CONTROL: with `credential_shaped` not asking `url_password`, the first assertion
/// fails: `` "imaps://ann:… carries a password ``.
#[test]
fn a_url_carrying_a_password_is_credential_shaped() {
    // In halves, so no scan reads a credential here.
    let secret = concat!("synthetic", "-pass-08");
    for url in [
        format!("imaps://ann:{secret}@imap.example.invalid:993"),
        format!("see redis://:{secret}@cache.example.invalid/0 for it"),
        format!("https://ann.lee:{secret}@example.invalid"),
    ] {
        assert!(
            credential_shaped(&url),
            "{:?} carries a password",
            &url[..12]
        );
    }
    for other in [
        "ssh://git@host.example.invalid/repo",
        "https://host.example.invalid:8443/a@b",
        "https://host.example.invalid/path?next=a:b@c",
        "https://ann:@example.invalid",
        "reach ann.lee@example.invalid",
        "https://[::1]:8443/index",
    ] {
        assert!(!credential_shaped(other), "{other:?} carries no password");
    }
}
