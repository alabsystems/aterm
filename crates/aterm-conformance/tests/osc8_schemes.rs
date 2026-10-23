// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end OSC 8 hyperlink scheme allowlist and URI guard tests.

use aterm_conformance::Screen;

fn screen_with_link(uri: &str) -> Screen {
    let mut s = Screen::new(24, 80);
    let seq = format!("\x1b]8;;{uri}\x1b\\X\x1b]8;;\x1b\\");
    s.feed(seq.as_bytes());
    s
}

fn link_retained(uri: &str) -> Option<String> {
    let s = screen_with_link(uri);
    assert_eq!(s.row(0), "X", "printed glyph must be on screen for {uri:?}");
    s.hyperlink_at(0, 0)
}

#[test]
fn osc8_allowed_schemes_retained() {
    let allowed: &[(&str, &str)] = &[
        ("http://example.com/a", "plain web link"),
        ("https://example.com/path?q=1", "TLS web link"),
        ("mailto:user@example.com", "mail compose"),
        ("sftp://host.example.com/file", "sftp transfer"),
        ("tel:+15551234567", "telephone"),
    ];
    for (uri, why) in allowed {
        assert_eq!(
            link_retained(uri).as_deref(),
            Some(*uri),
            "allowed scheme must be retained ({why}): {uri}"
        );
    }
}

#[test]
fn osc8_allowed_scheme_is_case_insensitive() {
    let uri = "HTTPS://EXAMPLE.COM/Path";
    assert_eq!(
        link_retained(uri).as_deref(),
        Some(uri),
        "scheme matching must be ASCII-case-insensitive"
    );
}

#[test]
fn osc8_dangerous_schemes_rejected() {
    let dangerous: &[(&str, &str)] = &[
        (
            "javascript:alert(1)",
            "XSS / script execution in the opener",
        ),
        (
            "data:text/html;base64,PHNjcmlwdD4=",
            "origin-less phishing/XSS payload",
        ),
        ("file:///etc/passwd", "local file disclosure / local exec"),
        (
            "ssh://-oProxyCommand=evil/host",
            "ssh argument injection / local exec",
        ),
        (
            "git://example.com/repo.git",
            "git->ssh argument injection class",
        ),
        (
            "ftp://example.com/file",
            "cleartext fetch / legacy handler abuse",
        ),
        ("slack://settings", "attacker-registered app URL handler"),
        ("vscode://payload", "attacker-registered app URL handler"),
    ];
    for (uri, vector) in dangerous {
        assert_eq!(
            link_retained(uri),
            None,
            "dangerous scheme must be rejected ({vector}): {uri}"
        );
    }
}

#[test]
fn osc8_garbage_schemes_rejected() {
    let garbage: &[(&str, &str)] = &[
        (
            "1http://example.com",
            "scheme must start with ALPHA, not a digit",
        ),
        (
            "ht tp://example.com",
            "space is not a legal scheme character",
        ),
        ("ht~tp://example.com", "'~' is not a legal scheme character"),
        ("://example.com", "empty scheme"),
        ("http//example.com", "no ':' — no scheme at all"),
        ("example.com/path", "scheme-less relative URI"),
    ];
    for (uri, why) in garbage {
        assert_eq!(
            link_retained(uri),
            None,
            "non-RFC-3986 scheme must be rejected ({why}): {uri}"
        );
    }
}

#[test]
fn osc8_overlength_uri_rejected_independently() {
    let long_uri = format!("https://example.com/{}", "a".repeat(8200));
    assert_eq!(
        link_retained(&long_uri),
        None,
        "over-length URI must be rejected by the length guard alone"
    );
    let ok_uri = format!("https://example.com/{}", "a".repeat(100));
    assert_eq!(link_retained(&ok_uri).as_deref(), Some(ok_uri.as_str()));
}

#[test]
fn osc8_control_char_in_uri_rejected_independently() {
    assert_eq!(
        link_retained("https://example.com/\x7Ffoo"),
        None,
        "DEL inside URI must be rejected by the control-char guard alone"
    );
    assert_eq!(
        link_retained("https://example.com/\u{0085}foo"),
        None,
        "C1 control inside URI must be rejected by the control-char guard alone"
    );
}

#[test]
fn osc8_bidi_override_in_uri_rejected_independently() {
    assert_eq!(
        link_retained("https://safe.example\u{202E}moc.live"),
        None,
        "RLO inside URI must be rejected by the BiDi guard alone"
    );
    assert_eq!(
        link_retained("https://example.com/\u{2066}x"),
        None,
        "LRI inside URI must be rejected by the BiDi guard alone"
    );
}

#[test]
fn osc8_link_applies_only_while_open() {
    let mut s = Screen::new(24, 80);
    s.feed(b"A\x1b]8;;https://example.com\x1b\\B\x1b]8;;\x1b\\C");
    assert_eq!(s.row(0), "ABC");
    assert_eq!(s.hyperlink_at(0, 0), None, "printed before open");
    assert_eq!(
        s.hyperlink_at(0, 1).as_deref(),
        Some("https://example.com"),
        "printed while link open"
    );
    assert_eq!(s.hyperlink_at(0, 2), None, "printed after close");
}
