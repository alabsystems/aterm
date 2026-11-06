// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An owner source's remote: what is read, what is stored, what is refused.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

/// A token as a forge mints one, built at run time from parts so no literal of one is in the
/// tree (F9).
fn token() -> String {
    ["tok3n", "PLANT", &"q".repeat(30)].concat()
}

fn token_url() -> String {
    [
        "https://",
        "x-access-token:",
        &token(),
        "@github.com/o/r.git",
    ]
    .concat()
}

/// A URL WITH A TOKEN IS REFUSED, NEVER STRIPPED AND READ (S8): the user-info secret refuses
/// the whole URL, so nothing of it is ever stored or handed to git. NEGATIVE CONTROL
/// `StripUserinfo`: a reader that drops the user info and reads the rest admits it, and its
/// stored form would then be read with the owner's token lost and the URL still in use.
#[test]
fn a_url_with_a_token_is_refused_and_a_stripping_reader_would_read_it() {
    let url = token_url();
    assert_eq!(RemoteUrl::parse(&url), Err(UrlRefusal::Userinfo));
    let stripped = fault::with(Fault::StripUserinfo, || RemoteUrl::parse(&url))
        .expect("the stripping reader reads it");
    assert_eq!(stripped.stored(), "https://github.com/o/r.git");
    assert!(!stripped.stored().contains(&token()));
    for bare in [
        ["https://", &token(), "@github.com/o/r.git"].concat(),
        ["https://ghp_", &"a".repeat(36), "@github.com/o/r"].concat(),
        ["https://", "github_pat_", "x", "@github.com/o/r"].concat(),
        ["https://", "oauth2:", &token(), "@gitlab.com/o/r"].concat(),
    ] {
        assert_eq!(
            RemoteUrl::parse(&bare),
            Err(UrlRefusal::Userinfo),
            "a token as the user"
        );
    }
}

#[test]
fn an_https_user_is_dropped_and_an_ssh_or_scp_user_kept() {
    for (text, stored, auth) in [
        (
            "https://user@github.com/o/r",
            "https://github.com/o/r",
            AuthMode::AnonymousHttps,
        ),
        (
            "https://GitHub.com/o/r.git",
            "https://github.com/o/r.git",
            AuthMode::AnonymousHttps,
        ),
        (
            "https://h.example:8443/o/r/",
            "https://h.example:8443/o/r",
            AuthMode::AnonymousHttps,
        ),
        (
            "git@github.com:o/r.git",
            "git@github.com:o/r.git",
            AuthMode::SshAgent,
        ),
        (
            "github.com:o/r.git",
            "github.com:o/r.git",
            AuthMode::SshAgent,
        ),
        (
            "ssh://git@h:2222/r",
            "ssh://git@h:2222/r",
            AuthMode::SshAgent,
        ),
        ("ssh://h/~u/r.git", "ssh://h/~u/r.git", AuthMode::SshAgent),
    ] {
        let url = RemoteUrl::parse(text).unwrap_or_else(|why| panic!("{text}: {why:?}"));
        assert_eq!(url.stored(), stored, "{text}");
        assert_eq!(url.auth(), auth, "{text}");
        assert_eq!(
            RemoteUrl::parse(&url.stored()).as_ref(),
            Ok(&url),
            "{text}: the stored form reads back as itself"
        );
        assert_eq!(format!("{url:?}"), format!("RemoteUrl({stored})"));
    }
    assert_eq!(RemoteUrl::parse("git@h:o/r").expect("scp").host(), "h");
}

#[test]
fn each_refusal_has_its_fixture() {
    for (text, want) in [
        ("ssh://git:pw@h/r", UrlRefusal::Userinfo),
        ("u:pw@h:o/r", UrlRefusal::Userinfo),
        ("https://h/o/r?token=x", UrlRefusal::Query),
        ("https://h/o/r#main", UrlRefusal::Query),
        ("ext::sh -c x", UrlRefusal::Scheme),
        ("git://h/o/r", UrlRefusal::Scheme),
        ("http://h/o/r", UrlRefusal::Scheme),
        ("http://127.0.0.1:9/r", UrlRefusal::Scheme),
        ("ftp://h/o/r", UrlRefusal::Scheme),
        ("o/r", UrlRefusal::Scheme),
        ("-oProxyCommand=x:r", UrlRefusal::Dash),
        ("ssh://-oProxyCommand=x/r", UrlRefusal::Dash),
        ("ssh://-u@h/r", UrlRefusal::Dash),
        ("h:-r", UrlRefusal::Dash),
        ("file:///Users//u/r.git", UrlRefusal::Local),
        ("/Users//u/scratch.noindex/r", UrlRefusal::Local),
        ("file://h/r", UrlRefusal::Local),
        ("/-r", UrlRefusal::Local),
        ("ssh://h:abc/r", UrlRefusal::Port),
        ("ssh://h:0/r", UrlRefusal::Port),
        ("ssh://h:65536/r", UrlRefusal::Port),
        ("https://h/o%2Fr", UrlRefusal::Control),
        ("https://h/o r", UrlRefusal::Control),
        ("https://h/o\u{202e}r", UrlRefusal::Control),
        ("https://h/o\u{200b}r", UrlRefusal::Control),
        ("https://h/o\u{e0049}r", UrlRefusal::Control),
        ("https://h/o\u{fe0f}r", UrlRefusal::Control),
        ("https://h/o\nr", UrlRefusal::Control),
        ("https://github.com", UrlRefusal::Shape),
        ("https://h/o/../r", UrlRefusal::Shape),
        ("https://h//r", UrlRefusal::Shape),
        ("https://h_x/r", UrlRefusal::Shape),
        ("h:/srv/r.git", UrlRefusal::Shape),
        ("h:o/r@v1", UrlRefusal::Userinfo),
    ] {
        assert_eq!(RemoteUrl::parse(text), Err(want), "{text:?}");
    }
    let long = format!("https://h/{}", "a".repeat(URL_MAX));
    assert_eq!(RemoteUrl::parse(&long), Err(UrlRefusal::Long));
    let words: Vec<&str> = UrlRefusal::ALL.iter().map(|r| r.word()).collect();
    let mut unique = words.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(words.len(), unique.len(), "each refusal has its own word");
}

/// A REPOSITORY ON THIS MAC IS NO REMOTE: [`RemoteUrl::parse`] refuses `file://` and an absolute
/// path `local` (git would read it with its own config's includes, alternates and common dir),
/// and the tests' constructor reads those two forms alone, under every other refusal.
#[test]
fn a_local_url_is_refused_and_the_tests_seam_reads_it_alone() {
    for (text, stored) in [
        ("file:///Users//u/r.git", "file:///Users//u/r.git"),
        ("/Users//u/scratch.noindex/r/", "/Users//u/scratch.noindex/r"),
    ] {
        assert_eq!(RemoteUrl::parse(text), Err(UrlRefusal::Local), "{text}");
        let url = RemoteUrl::local_for_tests(text).unwrap_or_else(|why| panic!("{text}: {why:?}"));
        assert_eq!(url.stored(), stored);
        assert_eq!(url.auth(), AuthMode::Local);
        assert_eq!(RemoteUrl::local_for_tests(&url.stored()), Ok(url));
    }
    for (text, want) in [
        ("git@h:o/r.git", UrlRefusal::Scheme),
        ("https://h/o/r", UrlRefusal::Scheme),
        ("file://h/r", UrlRefusal::Shape),
        ("/Users//u/../r", UrlRefusal::Shape),
        ("/Users//u/r\u{202e}", UrlRefusal::Scheme),
    ] {
        assert_eq!(RemoteUrl::local_for_tests(text), Err(want), "{text:?}");
    }
}

/// WHAT A LINE OR AN ALERT WOULD NOT SHOW IS HIDDEN, ONE FIXTURE PER CLASS (a control, bidi, a
/// zero-width or tag character, a variation selector, the combining grapheme joiner, a Hangul
/// filler, a Mongolian free variation selector, an interlinear annotation, private use, an
/// unassigned code point, a line separator), and the width table's own zero columns agree: every
/// code point it draws in none, but a mark or a conjoining jamo drawn on the letter before it, is
/// hidden. NEGATIVE CONTROL: letters, a mark on its letter, CJK, an emoji, a dash and a space
/// are not.
#[test]
fn hidden_is_every_character_a_line_would_not_show() {
    for c in [
        '\u{7}',
        '\u{85}',
        '\u{202e}',
        '\u{2066}',
        '\u{200b}',
        '\u{200d}',
        '\u{ad}',
        '\u{e0049}',
        '\u{e0001}',
        '\u{fe0f}',
        '\u{e0100}',
        '\u{34f}',
        '\u{3164}',
        '\u{ffa0}',
        '\u{115f}',
        '\u{1160}',
        '\u{180b}',
        '\u{180f}',
        '\u{17b4}',
        '\u{fff9}',
        '\u{13430}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e000}',
        '\u{f0000}',
        '\u{378}',
        '\u{2065}',
        '\u{2028}',
        '\u{2029}',
    ] {
        assert!(hidden(c), "U+{:04X}", u32::from(c));
    }
    for c in [
        'a', 'Z', '\u{e9}', '\u{301}', '\u{4e2d}', '\u{2728}', '\u{2014}', ' ', '`', '\u{1161}',
    ] {
        assert!(!hidden(c), "U+{:04X}", u32::from(c));
    }
    use aterm_grapheme::GeneralCategory as G;
    let drawn_on_the_letter_before = |c: char| {
        matches!(
            aterm_grapheme::general_category(c),
            G::NonspacingMark | G::EnclosingMark | G::OtherLetter
        )
    };
    let missed: Vec<String> = (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter(|&c| aterm_grapheme::char_width(c) == 0 && !drawn_on_the_letter_before(c))
        .filter(|&c| !hidden(c))
        .map(|c| format!("U+{:04X}", u32::from(c)))
        .collect();
    assert!(
        missed.is_empty(),
        "drawn in no column yet not hidden: {missed:?}"
    );
}

/// THE LOOPBACK HTTP SEAM ADMITS ITS LISTENER AND NOTHING ELSE: plain HTTP is refused by
/// [`RemoteUrl::parse`] everywhere (the NEGATIVE CONTROL above), and the tests' constructor reads
/// only `127.0.0.1`.
#[test]
fn the_loopback_http_seam_reads_its_listener_alone() {
    let url = RemoteUrl::loopback_http_for_tests("http://127.0.0.1:9/r").expect("the listener");
    assert_eq!(url.stored(), "http://127.0.0.1:9/r");
    assert_eq!(url.auth(), AuthMode::LoopbackHttp);
    for text in [
        "http://localhost:9/r",
        "http://127.0.0.2:9/r",
        "https://127.0.0.1:9/r",
        "http://127.0.0.1/r",
    ] {
        assert!(RemoteUrl::loopback_http_for_tests(text).is_err(), "{text}");
    }
}
