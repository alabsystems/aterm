// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn userinfo_ends_before_path_query_and_fragment() {
    for (url, embedded) in [
        ("https://user:password@mail.example", true),
        ("http://user@mail.example?to=a@b", true),
        ("https://user:password@mail.example#fragment@tail", true),
        ("https://mail.example?to=a@b", false),
        ("https://mail.example#fragment@tail", false),
        ("https://mail.example/path@tail", false),
        ("https://mail.example/?to=a@b", false),
        ("https://@mail.example", false),
        ("https://mail.example", false),
        ("https://?to=a@b", false),
        ("ftp://user:password@mail.example", false),
        ("HTTPS://user@mail.example", true),
    ] {
        assert_eq!(super::url_has_embedded_userinfo(url), embedded, "{url}");
    }

    for (url, selected) in [
        ("http://localhost:1777", true),
        ("http://user:password@127.0.0.1:1777?to=a@b", true),
        ("http://[::1]:1777?to=a@b", true),
        ("http://[2001:db8::1]:1777?to=a@b", false),
        ("http://remote.example?to=owner@127.0.0.1:1777", false),
        ("http://remote.example#owner@localhost:1777", false),
        ("http://remote.example/path@127.0.0.1:1777", false),
        ("http://remote.example\\@127.0.0.1:1777", false),
        ("http://127.0.0.1:1777\\path", false),
        ("http://::1:1777", false),
        ("https://localhost:1777", false),
    ] {
        assert_eq!(super::is_loopback_http_url(url), selected, "{url}");
    }
    assert!(super::cleartext_refusal("http://remote.example\\@127.0.0.1:1777/").is_some());
    assert!(super::cleartext_refusal("imap://remote.example\\@127.0.0.1/INBOX").is_some());

    for (url, authority, userinfo, target) in [
        ("http://localhost:1777", "localhost:1777", None, "/"),
        (
            "http://[::1]:1777?to=a@b#fragment@tail",
            "[::1]:1777",
            None,
            "/?to=a@b",
        ),
        (
            "http://user:password@127.0.0.1:1777?to=a@b#fragment@tail",
            "127.0.0.1:1777",
            Some("user:password"),
            "/?to=a@b",
        ),
        (
            "http://localhost:1777/mail?to=a@b#fragment@tail",
            "localhost:1777",
            None,
            "/mail?to=a@b",
        ),
        (
            "http://@localhost:1777#tail",
            "localhost:1777",
            Some(""),
            "/",
        ),
    ] {
        let parsed = super::parse_loopback_http_url(url).unwrap();
        assert_eq!(parsed.authority, authority, "{url}");
        assert_eq!(parsed.userinfo, userinfo, "{url}");
        assert_eq!(parsed.port, 1777, "{url}");
        assert_eq!(parsed.target, target, "{url}");
    }
    assert!(
        super::candidate_http_detail_urls("http://127.0.0.1:8025/api/v1/messages", ".").is_empty()
    );
    assert!(
        super::candidate_http_detail_urls("http://127.0.0.1:8025/api/v1/messages", "..").is_empty()
    );
    assert!(
        super::candidate_http_detail_urls("http://127.0.0.1:8025/api/v1/messages", "%2e%2e")
            .is_empty()
    );
    let plain =
        super::candidate_http_detail_urls("http://127.0.0.1:8025/api/v1/messages", "msg-42");
    assert_eq!(
        plain,
        vec![
            "http://127.0.0.1:8025/api/v1/message/msg-42".to_owned(),
            "http://127.0.0.1:8025/api/v1/messages/msg-42".to_owned(),
        ]
    );
    for suffix in ["#inbox", "?unread=1", "?unread=1#inbox"] {
        assert_eq!(
            super::candidate_http_detail_urls(
                &format!("http://127.0.0.1:8025/api/v1/messages{suffix}"),
                "msg-42",
            ),
            plain,
        );
    }
}
