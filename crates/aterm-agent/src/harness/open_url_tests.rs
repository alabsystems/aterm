// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm-open-url` profile resolution, execution, and URL-free refusals.

use super::*;

/// Chrome's `Local State`, as much of it as the opener reads, for four profiles.
const LOCAL_STATE: &str = r#"{
  "browser": {"enabled_labs_experiments": []},
  "profile": {
    "info_cache": {
      "Default": {"name": "Person 1", "user_name": "x@example.com"},
      "Profile 2": {"name": "Claude m7-a"},
      "Profile 3": {"name": "Twin"},
      "Profile 4": {"name": "Twin"}
    },
    "last_used": "Default"
  }
}"#;

/// A URL of the shape Claude Code hands its opener.
const URL: &str = "https://claude.ai/oauth/authorize?login_hint=a%40example.com&state=s";

#[test]
fn a_profile_is_found_by_its_display_name_and_only_when_it_is_one() {
    assert_eq!(
        profile_dir_of(LOCAL_STATE, "Claude m7-a").as_deref(),
        Some("Profile 2")
    );
    assert_eq!(
        profile_dir_of(LOCAL_STATE, "Person 1").as_deref(),
        Some("Default")
    );
    assert_eq!(profile_dir_of(LOCAL_STATE, "Profile 2"), None);
    assert_eq!(profile_dir_of(LOCAL_STATE, "Twin"), None);
    assert_eq!(profile_dir_of(LOCAL_STATE, "Nobody"), None);
    assert_eq!(profile_dir_of("not json", "Claude m7-a"), None);
    let odd = r#"{"profile":{"info_cache":{"../x":{"name":"A"},"-y":{"name":"B"}}}}"#;
    assert_eq!(profile_dir_of(odd, "A"), None);
    assert_eq!(profile_dir_of(odd, "B"), None);
}

#[test]
fn the_url_opens_in_that_profile_and_is_never_said() {
    let mut ran = Vec::new();
    let (code, said) = open_with(
        &[URL.to_string()],
        Some("Claude m7-a"),
        &|| Some(LOCAL_STATE.to_string()),
        &mut |cmd| {
            ran.push((
                cmd.get_program().to_string_lossy().into_owned(),
                cmd.get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            ));
            true
        },
    );
    if !cfg!(target_os = "macos") {
        assert_eq!(code, 1);
        assert!(ran.is_empty());
        return;
    }
    assert_eq!((code, said), (0, None));
    assert_eq!(
        ran,
        vec![(
            "open".to_string(),
            vec![
                "-na".to_string(),
                "Google Chrome".to_string(),
                "--args".to_string(),
                "--profile-directory=Profile 2".to_string(),
                URL.to_string(),
            ]
        )]
    );
}

/// One refusal case.
type Case<'a> = (
    &'a [String],
    Option<&'a str>,
    &'a dyn Fn() -> Option<String>,
    u8,
);

/// Every failure is exit 1 (a usage error 2) with one line that names no
/// URL, and nothing run: Claude Code then prints the URL itself.
#[test]
fn every_refusal_is_exit_one_and_names_no_url() {
    let mut never = |_cmd: aterm_uds::spawnfd::Command| -> bool {
        panic!("nothing is run");
    };
    let state = || Some(LOCAL_STATE.to_string());
    let none = || None;
    let cases: [Case<'_>; 6] = [
        (&[], Some("Claude m7-a"), &state, 2),
        (
            &["http://claude.ai/x".to_string()],
            Some("Claude m7-a"),
            &state,
            1,
        ),
        (
            &["https://claude.ai/x y".to_string()],
            Some("Claude m7-a"),
            &state,
            1,
        ),
        (&[URL.to_string()], None, &state, 1),
        (&[URL.to_string()], Some("Claude m7-a"), &none, 1),
        (&[URL.to_string()], Some("Twin"), &state, 1),
    ];
    for (args, profile, local_state, want) in cases {
        let (code, said) = open_with(args, profile, local_state, &mut never);
        assert_eq!(code, want, "{args:?} {profile:?}");
        let said = said.expect("a line");
        assert!(said.starts_with(OPENER), "{said}");
        assert!(!said.contains("claude.ai"), "the URL is never said: {said}");
    }
    if cfg!(target_os = "macos") {
        let (code, said) = open_with(&[URL.to_string()], Some("Claude m7-a"), &state, &mut |_| {
            false
        });
        assert_eq!(code, 1);
        assert!(!said.expect("a line").contains("claude.ai"));
    }
}

#[test]
fn mailbox_links_and_approval_keep_their_recipient_origin_and_callback() {
    let raw_eml = concat!(
        "From: Claude <no-reply@claude.ai>\r\n",
        "To: <m15-3@example.test>\r\n",
        "Subject: Sign in to Claude.ai\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
        "<html><body>\r\n",
        "<a href=3D\"https://claude.ai/unsubscribe\">Unsubscribe</a>\r\n",
        "<a href=3D\"https://claude.ai/magic?token=3Dabc123&amp;=\r\n",
        "state=3Dst_99\">Sign in</a>\r\n",
        "</body></html>\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(raw_eml, Some("other@example.test")),
        None,
        "must ignore emails addressed to a different account"
    );
    for spoofed_to in [
        "From: Claude <no-reply@claude.ai>\r\nTo: <not-m15-3@example.test>\r\n\r\nhttps://claude.ai/magic?token=spoof1\r\n",
        "From: Claude <no-reply@claude.ai>\r\nTo: <m15-3@example.test.evil.example>\r\n\r\nhttps://claude.ai/magic?token=spoof2\r\n",
    ] {
        assert_eq!(
            extract_magic_link_from_email(spoofed_to, Some("m15-3@example.test")),
            None,
            "substring recipient spoofing must not match target account: {spoofed_to}"
        );
    }
    let folded_eml = concat!(
        "From: Claude <no-reply@claude.ai>\r\n",
        "To: Claude Fleet Member\r\n",
        " \t<m15-3@example.test>\r\n\r\n",
        "https://claude.ai/magic?token=folded_ok\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(folded_eml, Some("m15-3@example.test")).as_deref(),
        Some("https://claude.ai/magic?token=folded_ok"),
        "RFC 5322 folded To header must match recipient"
    );
    let unsubscribe_only_eml = concat!(
        "From: Claude <no-reply@claude.ai>\r\n",
        "To: <m15-3@example.test>\r\n\r\n",
        "<html><body>",
        "<img src=\"https://claude.ai/static/logo.png\">",
        "<a href=\"https://claude.ai/unsubscribe?id=123\">Unsubscribe</a>",
        "</body></html>\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(unsubscribe_only_eml, Some("m15-3@example.test")),
        None,
        "notification email with only unsubscribe and asset links must never be clicked"
    );
    assert_eq!(
        extract_magic_link_from_email(raw_eml, Some("m15-3@example.test")).as_deref(),
        Some("https://claude.ai/magic?token=abc123&state=st_99")
    );

    let b64_mime_eml = concat!(
        "From: Claude <no-reply@claude.ai>\r\n",
        "To: <m15-3@example.test>\r\n",
        "Content-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n",
        "--b1\r\n",
        "Content-Type: text/html; charset=\"utf-8\"\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "PGEgaHJlZj0iaHR0cHM6Ly9jbGF1ZGUuYWkvbWFnaWM/dG9rZW49YjY0X29rJmFtcDtz\r\n",
        "dGF0ZT1zOSI+U2lnbiBpbjwvYT4=\r\n",
        "--b1--\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(b64_mime_eml, Some("m15-3@example.test")).as_deref(),
        Some("https://claude.ai/magic?token=b64_ok&state=s9")
    );

    let json_eml = r#"{"to":["m15-3@example.test"],"html":"<p><a href=\"https://claude.ai/magic?token=json_html_ok&amp;state=s2\">Log in</a></p>"}"#;
    assert_eq!(
        extract_magic_link_from_email(json_eml, Some("m15-3@example.test")).as_deref(),
        Some("https://claude.ai/magic?token=json_html_ok&state=s2")
    );
    assert_eq!(
        extract_magic_link_from_email(json_eml, Some("other@example.test")),
        None
    );

    let consent_html = r#"
        <html><body>
        <form method="POST" action="/oauth/approve">
          <input type="hidden" name="session_token" value="sess_42">
          <input type="hidden" name="state" value="st_99">
          <input type="hidden" name="approve" value="allow">
        </form>
        </body></html>
    "#;
    let (method, action, body) =
        parse_html_approval_action(consent_html, "http://127.0.0.1:9000/magic?token=abc123")
            .expect("consent form parsed");
    assert_eq!(method, "POST");
    assert_eq!(action, "http://127.0.0.1:9000/oauth/approve");
    assert_eq!(
        body.as_deref(),
        Some("session_token=sess_42&state=st_99&approve=allow")
    );

    type Request = (String, String, BTreeMap<String, String>, Option<String>);
    fn run(
        authorize: &str,
        magic: &str,
        responses: &[&str],
    ) -> (Result<(), &'static str>, Vec<Request>) {
        let mut remaining = responses.iter();
        let mut requests = Vec::new();
        let result = authorize_using(
            authorize,
            Some("a@example.com"),
            |email| {
                assert_eq!(email, Some("a@example.com"));
                Ok(magic.to_owned())
            },
            &mut |method, url, cookies, body| {
                requests.push((
                    method.to_owned(),
                    url.to_owned(),
                    cookies.clone(),
                    body.map(str::to_owned),
                ));
                parse_raw_http_response(remaining.next().expect("no unadmitted request"), cookies)
                    .ok_or("injected malformed response")
            },
        );
        assert!(
            remaining.next().is_none(),
            "every expected request happened"
        );
        (result, requests)
    }

    let callback = "http://localhost:9000/callback";
    let authorize = format!(
        "https://claude.ai/oauth/authorize?redirect_uri={}&state=expected",
        percent_encode(callback)
    );
    let begin = "HTTP/1.1 200 OK\r\nSet-Cookie: session=secret; Secure; Path=/\r\n\r\nsign in";
    let consent = concat!(
        "HTTP/1.1 200 OK\r\n\r\n",
        "<form method=\"POST\" action=\"/oauth/approve\">",
        "<input name=\"token\" value=\"private&amp;token\"></form>"
    );
    let landing = "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9000/callback?code=authorized&state=expected\r\n\r\n";
    let success = "HTTP/1.1 200 OK\r\nSet-Cookie: callback=not-a-provider-cookie\r\n\r\ndone";
    let (result, requests) = run(
        &authorize,
        "https://claude.ai/magic?token=mail",
        &[begin, consent, landing, success],
    );
    assert_eq!(result, Ok(()));
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].0, "GET");
    assert!(requests[0].2.is_empty());
    assert_eq!(requests[1].1, "https://claude.ai/magic?token=mail");
    assert_eq!(
        requests[1].2.get("session").map(String::as_str),
        Some("secret")
    );
    assert_eq!(requests[2].0, "POST");
    assert_eq!(requests[2].1, "https://claude.ai/oauth/approve");
    assert_eq!(requests[2].3.as_deref(), Some("token=private%26token"));
    assert_eq!(requests[2].2, requests[1].2);
    assert_eq!(requests[3].0, "GET");
    assert!(
        requests[3].2.is_empty(),
        "no provider cookie at local callback"
    );
    assert_eq!(requests[3].3, None, "no consent body at local callback");

    let confirm_magic = concat!(
        "HTTP/1.1 200 Connection established\r\nProxy-Agent: test\r\n\r\n",
        "HTTP/1.1 200 OK\r\nSet-Cookie: magic_ok=1; Path=/\r\n\r\n",
        "<form method=\"POST\" action=\"/magic/confirm\">",
        "<input name=\"nonce\" value=\"n_77\"></form>"
    );
    let (result2, requests2) = run(
        &authorize,
        "https://claude.ai/magic?token=mail",
        &[begin, confirm_magic, consent, landing, success],
    );
    assert_eq!(result2, Ok(()));
    assert_eq!(requests2.len(), 5);
    assert_eq!(requests2[2].0, "POST");
    assert_eq!(requests2[2].1, "https://claude.ai/magic/confirm");
    assert_eq!(requests2[2].3.as_deref(), Some("nonce=n_77"));
    assert_eq!(
        requests2[2].2.get("magic_ok").map(String::as_str),
        Some("1")
    );
    assert_eq!(requests2[3].0, "POST");
    assert_eq!(requests2[3].1, "https://claude.ai/oauth/approve");
    assert!(requests2[4].2.is_empty());

    for magic in [
        "https://attacker.example/magic",
        "https://claude.ai.attacker.example/magic",
        "https://claude.ai@attacker.example/magic",
        "https://claude.ai:444/magic",
        "http://claude.ai/magic",
        "https://claude.ai/\\attacker.example/magic",
        "http://127.0.0.1:9001/callback?code=x&state=expected",
    ] {
        let (result, requests) = run(&authorize, magic, &[begin]);
        assert!(result.is_err(), "{magic}");
        assert_eq!(requests.len(), 1, "no cookie or request sent to {magic}");
    }
    for location in ["https://attacker.example/next", "//attacker.example/next"] {
        let redirect = format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\n\r\n");
        let (result, requests) = run(&authorize, "https://claude.ai/magic", &[begin, &redirect]);
        assert!(result.is_err());
        assert_eq!(
            requests.len(),
            2,
            "redirect did not send provider cookies away"
        );
        let (result, requests) = run(&authorize, "unused", &[&redirect]);
        assert!(result.is_err());
        assert_eq!(
            requests.len(),
            1,
            "first response cannot expand the origin either"
        );
    }
    for action in [
        "https://attacker.example/approve",
        "//attacker.example/approve",
    ] {
        let page = format!(
            "HTTP/1.1 200 OK\r\n\r\n<form method=\"POST\" action=\"{action}\"><input name=\"token\" value=\"private\"></form>"
        );
        let (result, requests) = run(&authorize, "https://claude.ai/magic", &[begin, &page]);
        assert!(result.is_err());
        assert_eq!(requests.len(), 2, "neither consent body nor cookie leaked");
    }

    for destination in [
        "http://127.0.0.1:9000/callback/other?code=x&state=expected",
        "http://127.0.0.1:9001/callback?code=x&state=expected",
        "http://localhost.attacker.example:9000/callback?code=x&state=expected",
        "http://127.0.0.1:9000/callback?code=x&state=other",
        "http://127.0.0.1:9000/callback?code=&state=expected",
        "http://127.0.0.1:9000/callback?code=x&state=expected&state=other",
        "http://127.0.0.1:9000/callback?code=x&state=expected&%73tate=other",
        "http://127.0.0.1:9000/callback?code=x&code=y&state=expected",
    ] {
        let redirect = format!("HTTP/1.1 302 Found\r\nLocation: {destination}\r\n\r\n");
        let (result, requests) = run(
            &authorize,
            "https://claude.ai/magic",
            &[begin, consent, &redirect],
        );
        assert!(result.is_err(), "{destination}");
        assert_eq!(requests.len(), 3, "no request to a different callback");
    }

    for unknown in [
        "https://attacker.example/oauth/authorize",
        "https://claude.ai.attacker.example/oauth/authorize",
        "https://claude.com/cai/oauth/authorize",
        "http://127.0.0.1:9000/oauth/authorize",
        "https://claude.ai/oauth/authorize?state=x",
    ] {
        assert!(
            authorize_using(
                unknown,
                None,
                |_| panic!("unadmitted authorize URL read the mailbox"),
                &mut |_, _, _, _| panic!("unadmitted authorize URL reached transport"),
            )
            .is_err()
        );
    }
}
