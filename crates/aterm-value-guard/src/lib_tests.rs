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

/// A handle is words of lower-case letters and digits joined by one `-`, at most its column's
/// bytes, and a machine-shaped word is a word like any other; a token fits that grammar, so the
/// free-text refusals come first and name it as a credential.
/// NEGATIVE CONTROL: with `handle` checking only the length before the grammar (no `text`),
/// the Anthropic key's shape passes: `` assertion `left == right` failed: "sk-ant-" is refused ``.
#[test]
fn a_handle_is_lower_case_words_joined_by_one_dash_and_may_name_a_machine() {
    for ok in [
        "m2", "m2-work", "fb-gmail", "kimi-m16", "m106-m38", "a", "0",
    ] {
        assert_eq!(handle(ok, 32), Ok(()), "{ok:?} is a handle");
    }
    for bad in [
        "", "-", "m2-", "-m2", "m2--work", "team a", "Main", "m2_work", "m2.work", "ann@x",
    ] {
        assert!(handle(bad, 32).is_err(), "{bad:?} is no handle");
    }
    assert_eq!(handle("main-desk", 8), Err(Refusal::Length));
    assert_eq!(handle("team a", 32), Err(Refusal::Handle));
    // In halves, so no scan reads a credential here.
    let key = concat!("sk-ant-", "api03-abcdefghij");
    assert_eq!(
        handle(key, 32),
        Err(Refusal::CredentialShape),
        "{:?} is refused",
        &key[..7]
    );
}

/// The tracker's secret rule refuses a real secret and lets the tracker's own text stand: a
/// provider's prefix with a token's body, a PEM private key, an `otpauth://` seed, an upper-case
/// or grouped TOTP seed, a URL's password and a random key are refused; a commit, a long path or
/// test name, an `@`, a bare prefix named in prose, a KI number, words written without spaces and
/// a hex digest are not.
/// NEGATIVE CONTROL: with `seed_shaped` taking a lower-case run, `` "4alternatingpairs" stands ``
/// fails; with `TOKENS`' bodies dropped, `` "sk-ssh-ed25519" stands `` fails; with
/// `random_token` splitting runs at `/`, `` "AWS_SECRET" is a secret `` fails.
#[test]
fn the_tracker_rule_refuses_real_secrets_and_lets_tracker_text_stand() {
    // In halves, so no scan reads a token here.
    for secret in [
        concat!("ghp", "_", "R7w2Kq9ZbX4mVn8LpT3sYc6HdJ1fGa5Ue0Wi"),
        concat!(
            "token sk",
            "-ant-api03-",
            "Zx8Qw2Er4Ty6Ui8Op0As2Df4Gh6Jk8Lz0Xc2Vb4Nm6"
        ),
        concat!("key=AK", "IA", "Z7Q2W9X4C6V8B3N5"),
        concat!("-----BEGIN EC ", "PRIVATE KEY-----"),
        concat!("otpauth://totp/a?sec", "ret=JBSWY3DPEHPK3PXP"),
        concat!("seed JBSWY3DP", "EHPK3PXP"),
        concat!("jbsw y3dp ", "ehpk 3pxp"),
        concat!("https://ann:", "synthetic-pass-08", "@host.example.invalid"),
        concat!("epJL7EGWiFbCY2pB", "Vjb2A1NPXIyzSropDjP8HKn4"),
        concat!(
            "aws s3 ls failed with wJalrXUtnFEMI/K7MD",
            "ENG/bPxRfiCYEXAMPLEKEY in the env"
        ),
        concat!(
            "https://hooks.slack.com/services/",
            "T0123ABCD/B0123ABCD/Kq7Zp2Wx9Lm4Vb8Nc3Hj6Gf1"
        ),
        concat!(
            "AGE-SECRET-KEY-1",
            "QL3Z7HNZ0Y8X9W6V5U4T3S2R1Q0P9N8M7L6K5J4H3G2F1E0D9C8B7A6"
        ),
        concat!("scan otpauth://totp/lab:ann?sec", "ret=jbswy3dpehpk3pxp."),
        concat!("(otpauth://totp/x?sec", "ret=jbswy3dpehpk3pxp)"),
    ] {
        assert!(secret_shaped(secret), "{:?}… is a secret", &secret[..6]);
        assert_eq!(prose(secret, 4096), Err(Refusal::CredentialShape));
    }
    for text in [
        "landed as 0a386e53d70f1c2b3a4d5e6f708192a3b4c5d6e7 on main",
        "crates/aterm-db/src/typed/keys_slug_tests.rs::a_very_long_test_name_here",
        "claimed by s-ed8f5c@m60 with lean-init@v4.30.0",
        "ssh-ed25519 or sk-ssh-ed25519 by the key in policy/owner.pub",
        "the ghp_ and xoxb- prefixes are tokens",
        "ATERM-KI-1114-533 and +1 202 555 0149",
        "4alternatingpairs/all5samplesretained baseline25requests",
        "Nativecoldquery160columns161allocationrequests LANDED2ef0af5482c3a8e569727dfd922364e9d399e22e",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "a package about the vault's password and TOTP entries",
        "-----BEGIN CERTIFICATE-----",
        "otpauth: URIs import into the vault",
        "test ipv6 with real hosts, then pick gpt5 when tool runs fail",
        "npm_config_cache=/tmp/.ubd/npm.noindex npm ci",
        concat!(
            "Slack's xoxb-/xoxp-/xapp- tokens and the xox",
            "p-user-token flow"
        ),
        "sk-ssh-ed25519-cert-v01@openssh.com keys",
    ] {
        assert!(!secret_shaped(text), "{text:?} stands");
        assert_eq!(prose(text, 4096), Ok(()), "{text:?} is prose");
    }
    assert_eq!(prose("a\tb", 16), Err(Refusal::Control));
    assert_eq!(prose("abc", 2), Err(Refusal::Length));
    assert_eq!(command("  ", 16), Err(Refusal::Empty));
    assert_eq!(command("true", 16), Ok(()));
}
