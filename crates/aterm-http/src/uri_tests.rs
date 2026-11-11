// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn parses_a_loopback_and_a_remote_endpoint() {
    let u = Uri::parse("http://127.0.0.1:11434/api/chat").unwrap();
    assert_eq!(u.scheme(), Scheme::Http);
    assert_eq!(u.host(), "127.0.0.1");
    assert_eq!(u.port(), 11434);
    assert_eq!(u.path_and_query(), "/api/chat");
    assert_eq!(u.host_header(), "127.0.0.1:11434");

    let u = Uri::parse("https://llm.example.test/v1/chat/completions").unwrap();
    assert_eq!(u.scheme(), Scheme::Https);
    assert_eq!(u.port(), 443);
    assert_eq!(u.host_header(), "llm.example.test");
    assert_eq!(u.path_and_query(), "/v1/chat/completions");
}

#[test]
fn missing_path_becomes_root_and_a_bare_query_keeps_it() {
    assert_eq!(Uri::parse("http://h").unwrap().path_and_query(), "/");
    assert_eq!(Uri::parse("http://h/").unwrap().path_and_query(), "/");
    assert_eq!(
        Uri::parse("http://h?a=1").unwrap().path_and_query(),
        "/?a=1"
    );
    assert_eq!(
        Uri::parse("http://h/p?a=1").unwrap().path_and_query(),
        "/p?a=1"
    );
}

#[test]
fn a_fragment_is_dropped_and_never_reaches_the_request_line() {
    let u = Uri::parse("http://h/p?a=1#secret").unwrap();
    assert_eq!(u.path_and_query(), "/p?a=1");
}

#[test]
fn percent_encoding_is_carried_through_verbatim() {
    let u = Uri::parse("http://h/a%2Fb%20c?q=%41").unwrap();
    assert_eq!(u.path_and_query(), "/a%2Fb%20c?q=%41");
}

#[test]
fn userinfo_is_rejected_rather_than_stripped() {
    assert!(Uri::parse("http://user:pass@host/p").is_none());
    assert!(Uri::parse("https://token@host/p").is_none());
}

#[test]
fn control_characters_and_spaces_cannot_smuggle_a_header() {
    assert!(Uri::parse("http://h/p\r\nX-Evil: 1").is_none());
    assert!(Uri::parse("http://h/p q").is_none());
    assert!(Uri::parse("http://h\n/p").is_none());
    assert!(Uri::parse("http://h/p\0").is_none());
}

#[test]
fn only_http_and_https_are_accepted() {
    for bad in [
        "ftp://h/p",
        "file:///etc/passwd",
        "/relative/path",
        "h/p",
        "",
        "ws://h/p",
    ] {
        assert!(Uri::parse(bad).is_none(), "{bad} must not parse");
    }
    assert_eq!(Uri::parse("HtTpS://h/p").unwrap().scheme(), Scheme::Https);
}

#[test]
fn ipv6_literals_round_trip_through_brackets() {
    let u = Uri::parse("http://[::1]:8080/p").unwrap();
    assert_eq!(u.host(), "::1");
    assert_eq!(u.port(), 8080);
    assert_eq!(u.host_header(), "[::1]:8080");
    assert_eq!(u.authority(), "[::1]:8080");

    let u = Uri::parse("https://[2606:4700::1111]/p").unwrap();
    assert_eq!(u.host_header(), "[2606:4700::1111]");
}

#[test]
fn malformed_authorities_are_rejected() {
    for bad in [
        "http://",
        "http:///p",
        "http://h:/p",
        "http://h:0/p",
        "http://h:99999/p",
        "http://h:abc/p",
        "http://::1/p",  // unbracketed IPv6
        "http://[::1/p", // unclosed bracket
        "http://[zz]/p", // not an IPv6 address
    ] {
        assert!(Uri::parse(bad).is_none(), "{bad} must not parse");
    }
}

#[test]
fn the_raw_authority_and_bare_path_are_available_for_agreement_checks() {
    let u = Uri::parse("http://127.0.0.1:11434/api/chat").unwrap();
    assert_eq!(u.authority_as_written(), "127.0.0.1:11434");
    assert_eq!(u.path(), "/api/chat");

    let u = Uri::parse("https://api.test/v1/x?a=1").unwrap();
    assert_eq!(u.authority_as_written(), "api.test");
    assert_eq!(u.path(), "/v1/x");
    assert_eq!(u.path_and_query(), "/v1/x?a=1");

    let u = Uri::parse("http://[::1]:8080/p").unwrap();
    assert_eq!(u.authority_as_written(), "[::1]:8080");
}
