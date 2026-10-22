// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The Redactor (design §6.6): token shapes, the credential header, `bearer` runs, and the
//! exact values it learns. No test spells a token: each is built from `agh_fake::token`.

use agh::redact::Redactor;

/// The six token prefixes.
const PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];

/// A run of token characters, from a fake token.
fn run(seed: &str) -> String {
    agh_fake::token(seed)["ghp_".len()..].to_string()
}

#[test]
fn every_token_prefix_with_a_run_is_redacted() {
    for prefix in PREFIXES {
        let text = format!("held {prefix}{} here", run(prefix));
        assert_eq!(Redactor::shapes(&text), "held [redacted] here", "{prefix}");
        let bare = format!("the prefix {prefix} alone");
        assert_eq!(Redactor::shapes(&bare), bare, "{prefix}");
    }
}

#[test]
fn the_credential_header_is_redacted_in_any_case() {
    let secret = run("header");
    for header in [
        format!("Authorization: Bearer {secret}"),
        format!("authorization: token {secret}"),
        format!("AUTHORIZATION:basic {secret}"),
        format!("Authorization : {secret}"),
    ] {
        let out = Redactor::shapes(&header);
        assert!(!out.contains(&secret), "{out}");
        assert!(out.ends_with("[redacted]"), "{out}");
    }
}

#[test]
fn bearer_runs_of_eight_bytes_or_more() {
    assert_eq!(
        Redactor::shapes("sent bearer abcdefgh then"),
        "sent bearer [redacted] then"
    );
    assert_eq!(
        Redactor::shapes("sent bearer abcdefg then"),
        "sent bearer abcdefg then"
    );
}

#[test]
fn prose_is_left_alone() {
    for prose in ["the token file is loose", "the bearer of the message"] {
        assert_eq!(Redactor::shapes(prose), prose);
    }
}

#[test]
fn learned_values_are_redacted_exactly() {
    let redactor = Redactor::new();
    redactor.learn(b"opaque-secret-value");
    redactor.learn(b"short");
    assert_eq!(
        redactor.apply("a opaque-secret-value and short"),
        "a [redacted] and short"
    );
}

#[test]
fn the_longest_learned_value_goes_first() {
    let redactor = Redactor::new();
    redactor.learn(b"overlap-1234");
    redactor.learn(b"overlap-1234-longer");
    assert_eq!(redactor.apply("x overlap-1234-longer y"), "x [redacted] y");
}

#[test]
fn clones_share_what_is_learned() {
    let redactor = Redactor::new();
    let clone = redactor.clone();
    clone.learn(b"shared-secret-value");
    assert_eq!(redactor.apply("shared-secret-value"), "[redacted]");
}

#[test]
fn debug_names_the_count_and_no_value() {
    let redactor = Redactor::new();
    redactor.learn(b"debug-secret-value");
    let debug = format!("{redactor:?}");
    assert!(debug.contains('1'), "{debug}");
    assert!(!debug.contains("debug-secret-value"), "{debug}");
}

#[test]
fn a_quoted_value_keeps_its_closing_quote() {
    let secret = run("quoted");
    let line = format!("why=\"sent bearer {secret}\" next=\"x\"");
    assert_eq!(
        Redactor::shapes(&line),
        "why=\"sent bearer [redacted]\" next=\"x\""
    );
}

#[test]
fn an_escaped_quote_does_not_cut_the_run() {
    let secret = run("escaped");
    let (head, tail) = secret.split_at(3);
    let line = format!("why=\"bearer {head}\\\"{tail}\"");
    assert_eq!(Redactor::shapes(&line), "why=\"bearer [redacted]\"");
}

#[test]
fn redacting_twice_changes_nothing() {
    let secret = run("twice");
    for text in [
        format!("why=\"sent bearer {secret}\""),
        format!("Authorization: token {secret}"),
        format!("{{\"authorization\":\"Bearer {secret}\"}}"),
    ] {
        let once = Redactor::shapes(&text);
        assert!(!once.contains(&secret), "{once}");
        assert_eq!(Redactor::shapes(&once), once);
    }
}

#[test]
fn the_header_with_more_blanks_or_as_json() {
    let secret = run("blanks");
    for (text, redacted) in [
        (
            format!("Authorization: token  {secret}"),
            "Authorization: token  [redacted]",
        ),
        (
            format!("Authorization: Bearer\t{secret}"),
            "Authorization: Bearer\t[redacted]",
        ),
        (
            format!("Authorization\t:\tbasic {secret}"),
            "Authorization\t:\tbasic [redacted]",
        ),
        (
            format!("{{\"authorization\":\"token {secret}\"}}"),
            "{\"authorization\":\"token [redacted]\"}",
        ),
        (
            format!("{{\"Authorization\": \"{secret}\", \"x\": 1}}"),
            "{\"Authorization\": \"[redacted]\", \"x\": 1}",
        ),
        (
            format!("sent bearer   {secret} then"),
            "sent bearer   [redacted] then",
        ),
        (
            format!("sent Bearer\t{secret} then"),
            "sent Bearer\t[redacted] then",
        ),
    ] {
        assert_eq!(Redactor::shapes(&text), redacted, "{text}");
    }
}

#[test]
fn a_value_that_opens_with_a_quote_is_redacted() {
    let secret = run("opening");
    for (text, redacted) in [
        (
            format!("Authorization: Bearer \"{secret}\""),
            "Authorization: Bearer \"[redacted]\"",
        ),
        (
            format!("sent bearer \"{secret}\" then"),
            "sent bearer \"[redacted]\" then",
        ),
        (
            format!("{{\\\"authorization\\\":\\\"token {secret}\\\"}}"),
            "{\\\"authorization\\\":\\\"token [redacted]\\\"}",
        ),
        (
            format!("sent bearer \\\"{secret}\\\" then"),
            "sent bearer \\\"[redacted]\\\" then",
        ),
    ] {
        let once = Redactor::shapes(&text);
        assert_eq!(once, redacted, "{text}");
        assert_eq!(Redactor::shapes(&once), once, "{text}");
    }
}

#[test]
fn a_json_escaped_blank_is_a_blank() {
    let secret = run("escaped");
    for (text, redacted) in [
        (
            format!("{{\"note\":\"bearer\\t{secret}\"}}"),
            "{\"note\":\"bearer\\t[redacted]\"}",
        ),
        (
            format!("{{\"note\":\"Bearer\\u0009{secret}\"}}"),
            "{\"note\":\"Bearer\\u0009[redacted]\"}",
        ),
        (
            format!("{{\"authorization\":\"token\\u0020{secret}\"}}"),
            "{\"authorization\":\"token\\u0020[redacted]\"}",
        ),
        (
            format!("{{\"authorization\":\\t\"{secret}\"}}"),
            "{\"authorization\":\\t\"[redacted]\"}",
        ),
    ] {
        let once = Redactor::shapes(&text);
        assert_eq!(once, redacted, "{text}");
        assert_eq!(Redactor::shapes(&once), once, "{text}");
    }
}

/// A CLI context over a scripted transport that is never called, and its captured output.
fn note_ctx(root: &std::path::Path) -> (agh::cli::Ctx, agh::cli::Captured) {
    agh::cli::Ctx::for_tests(
        root,
        std::sync::Arc::new(agh::wire::scripted::ScriptedTransport::new()),
        agh::wire::Endpoints::github_com(),
        agh::identity::MapEnv::new(),
    )
}

#[test]
fn a_receipt_note_keeps_the_field_after_a_value_ending_in_bearer() {
    use agh::cli::out::{note, receipt_line};
    use agh::error::IdentityRef;

    let dir = crate::common::scratch("note");
    let (mut ctx, captured) = note_ctx(dir.path());
    let mut receipt = agh::cli::Receipt::new("repo.view");
    receipt.repo = Some("acme/torchbearer".into());
    receipt.as_ = Some(IdentityRef::Anonymous);
    receipt.requests = 1000;
    let line = receipt_line(&receipt, &ctx.redactor);
    note(&mut ctx, &line);
    let stderr = String::from_utf8(captured.stderr()).expect("utf-8");
    assert!(
        stderr.starts_with("agh: ok verb=repo.view repo=acme/torchbearer as=anonymous reqs=1000 "),
        "{stderr}"
    );
    receipt.as_ = None;
    let line = receipt_line(&receipt, &ctx.redactor);
    note(&mut ctx, &line);
    let stderr = String::from_utf8(captured.stderr()).expect("utf-8");
    assert!(
        stderr.contains("repo=acme/torchbearer reqs=1000 "),
        "{stderr}"
    );
}

#[test]
fn a_note_still_redacts_tokens_and_learned_values() {
    let dir = crate::common::scratch("note-secrets");
    let (mut ctx, captured) = note_ctx(dir.path());
    let token = agh_fake::token("note");
    let learned = run("note-learned");
    ctx.redactor.learn(learned.as_bytes());
    agh::cli::out::note(&mut ctx, &format!("held {token} and {learned}"));
    let stderr = String::from_utf8(captured.stderr()).expect("utf-8");
    assert_eq!(stderr, "held [redacted] and [redacted]\n");
}
