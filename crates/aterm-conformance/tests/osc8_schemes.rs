// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OSC 8 hyperlink URI scheme allowlist — conformance table (#7919, #7989).
//!
//! Locks the allowlist in `handler_osc.rs::is_allowed_scheme` and the
//! orthogonal URI guards in `handle_osc_8` behaviorally: feed
//! `OSC 8 ; ; URI ST  X  OSC 8 ; ; ST` and observe whether the printed
//! cell carries the hyperlink (`Screen::hyperlink_at`). A retained link
//! means the URI passed EVERY check; a missing link means it was refused
//! at parse time and never reached the grid.
//!
//! Why an allowlist (not a blocklist): F01-4 (HN-P1) demonstrated that
//! attacker-registered macOS URL handlers (`slack:`, `vscode:`, ...) slip
//! past any blocklist and launch native apps via `NSWorkspace.open`. The
//! default-safe set is exactly: http, https, mailto, sftp, tel.

use aterm_conformance::Screen;

/// Feed `OSC 8 ;;<uri> ST`, print "X" at (0,0), close the link.
/// Returns the screen for hyperlink inspection.
fn screen_with_link(uri: &str) -> Screen {
    let mut s = Screen::new(24, 80);
    let seq = format!("\x1b]8;;{uri}\x1b\\X\x1b]8;;\x1b\\");
    s.feed(seq.as_bytes());
    s
}

fn link_retained(uri: &str) -> Option<String> {
    let s = screen_with_link(uri);
    // The glyph must always land regardless of link acceptance.
    assert_eq!(s.row(0), "X", "printed glyph must be on screen for {uri:?}");
    s.hyperlink_at(0, 0)
}

// =============================================================================
// Allowed schemes — each must survive end-to-end and be exposed per-cell.
// =============================================================================

#[test]
fn osc8_allowed_schemes_retained() {
    // (uri, why this scheme is on the allowlist)
    let allowed: &[(&str, &str)] = &[
        // http/https: the web; the entire point of OSC 8.
        ("http://example.com/a", "plain web link"),
        ("https://example.com/path?q=1", "TLS web link"),
        // mailto: opens a compose window; no code execution surface.
        ("mailto:user@example.com", "mail compose"),
        // sftp: file transfer URL; unlike ssh: it does not feed a hostname
        // into a shell-adjacent command line (CVE-2023-51385 class).
        ("sftp://host.example.com/file", "sftp transfer"),
        // tel: dialer hand-off; no filesystem or shell surface.
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
    // RFC 3986 §3.1: schemes are case-insensitive. The allowlist must
    // match HTTPS:// as https:// — rejecting it would break real emitters.
    let uri = "HTTPS://EXAMPLE.COM/Path";
    assert_eq!(
        link_retained(uri).as_deref(),
        Some(uri),
        "scheme matching must be ASCII-case-insensitive"
    );
}

// =============================================================================
// Dangerous schemes — each must be refused at parse time (no cell link).
// =============================================================================

#[test]
fn osc8_dangerous_schemes_rejected() {
    // (uri, attack vector the rejection blocks)
    let dangerous: &[(&str, &str)] = &[
        // javascript: executes attacker JS in whatever opens the link (XSS).
        (
            "javascript:alert(1)",
            "XSS / script execution in the opener",
        ),
        // data: smuggles attacker-controlled documents (phishing pages,
        // drive-by HTML) with no origin and no network fetch to inspect.
        (
            "data:text/html;base64,PHNjcmlwdD4=",
            "origin-less phishing/XSS payload",
        ),
        // file: discloses or opens local files; on macOS can launch
        // documents with registered handlers (local-exec surface).
        ("file:///etc/passwd", "local file disclosure / local exec"),
        // ssh: CVE-2023-51385 class — a hostname like `-oProxyCommand=...`
        // becomes an argument/command injection when handed to ssh.
        (
            "ssh://-oProxyCommand=evil/host",
            "ssh argument injection / local exec",
        ),
        // git: same argument-injection class as ssh (git shells out to ssh).
        (
            "git://example.com/repo.git",
            "git->ssh argument injection class",
        ),
        // ftp: cleartext fetch; removed from the safe set with file: (#7989).
        (
            "ftp://example.com/file",
            "cleartext fetch / legacy handler abuse",
        ),
        // Attacker-registered app handlers — the F01-4 (HN-P1) finding that
        // forced the blocklist->allowlist conversion (#7919).
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

// =============================================================================
// Non-RFC-3986 garbage schemes — refused before any allowlist comparison.
// =============================================================================

#[test]
fn osc8_garbage_schemes_rejected() {
    // (uri, why it is not a valid RFC 3986 scheme)
    let garbage: &[(&str, &str)] = &[
        // RFC 3986 §3.1: scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )
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

// =============================================================================
// Guard composition — each orthogonal guard must reject INDEPENDENTLY:
// one bad property at a time, on an otherwise perfectly allowed https URI.
// =============================================================================

#[test]
fn osc8_overlength_uri_rejected_independently() {
    // Valid scheme, no control chars, no BiDi — ONLY too long.
    // MAX_HYPERLINK_URL_BYTES = 8192 (terminal/mod.rs); memory-exhaustion
    // guard against OSC 8 spam with megabyte URIs.
    let long_uri = format!("https://example.com/{}", "a".repeat(8200));
    assert_eq!(
        link_retained(&long_uri),
        None,
        "over-length URI must be rejected by the length guard alone"
    );
    // Control: the same shape under the cap is accepted, proving the
    // rejection above came from length, not scheme/charset.
    let ok_uri = format!("https://example.com/{}", "a".repeat(100));
    assert_eq!(link_retained(&ok_uri).as_deref(), Some(ok_uri.as_str()));
}

#[test]
fn osc8_control_char_in_uri_rejected_independently() {
    // Valid scheme, under-length, no BiDi — ONLY an embedded control char.
    // Control chars in URIs enable log/status-bar spoofing and splitting
    // attacks when the URI is later rendered or passed to an opener.
    //
    // DEL (0x7F): the parser puts 0x20-0xFF in OSC strings, so it reaches
    // the handler and must be refused there (is_control()).
    assert_eq!(
        link_retained("https://example.com/\x7Ffoo"),
        None,
        "DEL inside URI must be rejected by the control-char guard alone"
    );
    // C1 control U+0085 (NEL) arriving as raw UTF-8 bytes (0xC2 0x85):
    // same guard, multi-byte path.
    assert_eq!(
        link_retained("https://example.com/\u{0085}foo"),
        None,
        "C1 control inside URI must be rejected by the control-char guard alone"
    );
}

#[test]
fn osc8_bidi_override_in_uri_rejected_independently() {
    // Valid scheme, under-length, no control chars — ONLY a BiDi override.
    // Trojan Source (#7958, CVE-2021-42574): U+202E visually reorders
    // "https://safe.example\u{202E}moc.live" in previews/status bars to
    // spoof the destination hostname. Reject outright, never strip.
    assert_eq!(
        link_retained("https://safe.example\u{202E}moc.live"),
        None,
        "RLO inside URI must be rejected by the BiDi guard alone"
    );
    // Isolate form (U+2066 LRI) — same guard, different codepoint family.
    assert_eq!(
        link_retained("https://example.com/\u{2066}x"),
        None,
        "LRI inside URI must be rejected by the BiDi guard alone"
    );
}

// =============================================================================
// Oracle sanity — cell-level retention matches link open/close semantics.
// =============================================================================

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
