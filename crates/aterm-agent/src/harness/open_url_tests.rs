// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm-open-url` profile resolution, execution, and URL-free refusals.

use super::*;

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

const URL: &str = "https://claude.ai/oauth/authorize?login_hint=a%40example.com&state=s";

#[test]
fn curl_keeps_credential_options_and_urls_off_its_command_line() {
    let args = [
        "--disable",
        "--globoff",
        "--proto",
        "=https",
        "--max-time",
        "10",
    ];
    let options = [
        ("user", "fake-user:fake-password"),
        ("header", "Authorization: Bearer fake-token"),
        ("cookie", "session=fake-cookie"),
        ("data-raw", "code=fake-code"),
        ("url", "https://example.test/callback?code=fake-url-code"),
    ];
    let (cmd, _) = curl_command(aterm_uds::os_path!("curl"), &args, &options).unwrap();
    let expected: Vec<&std::ffi::OsStr> = args
        .iter()
        .copied()
        .chain(["--config", "-"])
        .map(std::ffi::OsStr::new)
        .collect();
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), expected);
    for (_, value) in options {
        assert!(!cmd.get_args().any(|arg| arg == std::ffi::OsStr::new(value)));
    }
    assert_eq!(
        curl_command(
            aterm_uds::os_path!("curl"),
            &args,
            &[("user", "bad\0value")]
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn curl_reads_escaped_stdin_config_as_one_private_file_transfer() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-curl-stdin-")
        .tempdir()
        .expect("private data directory");
    let payload = dir.path().join("payload.txt");
    let decoy = dir.path().join("decoy.txt");
    std::fs::write(&payload, "one private payload\n").unwrap();
    std::fs::write(&decoy, "a second transfer must not appear\n").unwrap();
    let file_url = |path: &Path| {
        format!(
            "file://{}",
            path.to_str()
                .unwrap()
                .split('/')
                .map(percent_encode)
                .collect::<Vec<_>>()
                .join("/")
        )
    };
    let url = file_url(&payload);
    let injected = format!("fake\"\\\nurl = \"{}\"\r\n#\tend", file_url(&decoy));
    let user = format!("fake-user:{injected}");
    let header = format!("X-Synthetic: {injected}");
    let cookie = format!("session={injected}");
    let form = format!("synthetic={injected}");
    let out = curl_output_with(
        aterm_uds::os_path!("curl"),
        &[
            "--disable",
            "--globoff",
            "--proto",
            "=file",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
        ],
        &[
            ("user", &user),
            ("header", &header),
            ("cookie", &cookie),
            ("data-raw", &form),
            ("url", &url),
        ],
    )
    .expect("installed curl consumed private config");
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(out.stdout, b"one private payload\n");
    assert_eq!(
        std::fs::read(&decoy).unwrap(),
        b"a second transfer must not appear\n"
    );
}

#[test]
fn encoded_word_spacing_preserves_plain_text_and_joins_only_encoded_neighbors() {
    let first = "=?UTF-8?Q?A?=";
    let second = "=?UTF-8?B?Qg==?=";
    for separator in ["", " ", "  \t ", "\t", "\r\n\t", " \r\n \t", "\n "] {
        assert_eq!(
            decode_rfc2047_words(&format!("{first}{separator}{second}")),
            "AB",
            "{separator:?}"
        );
    }
    for separator in ["\r", "\r\n", "\n", "\u{a0}", " text "] {
        assert_eq!(
            decode_rfc2047_words(&format!("{first}{separator}{second}")),
            format!("A{separator}B"),
            "{separator:?}"
        );
    }
    for (encoded, expected) in [
        ("plain text", "plain text"),
        ("=?UTF-8?Q?Hello?= world", "Hello world"),
        ("=?UTF-8?Q?Hello?=  world", "Hello  world"),
        ("=?UTF-8?Q?Hello?=\tworld", "Hello\tworld"),
        ("=?UTF-8?Q?Hello?= \t", "Hello \t"),
        ("prefix =?UTF-8?Q?Hello?= suffix", "prefix Hello suffix"),
        ("=?UTF-8?Q?A_?= \t=?UTF-8?Q?B?=", "A B"),
        ("=?UTF-8?Q?A?=  =?UTF-8?X?B?=", "A  =?UTF-8?X?B?="),
        ("=?UTF-8?Q?A?=\t=?unfinished", "A\t=?unfinished"),
        ("=?UTF-8?X?A?=  =?UTF-8?Q?B?=", "=?UTF-8?X?A?=  B"),
    ] {
        assert_eq!(decode_rfc2047_words(encoded), expected, "{encoded:?}");
    }

    let mail = concat!(
        "From: Anthropic <no-reply@claude.ai>\r\n",
        "To: owner@example.test\r\n",
        "Subject: =?UTF-8?Q?Your_Claude_verification_code_is_918?=  \t\r\n",
        "\t=?UTF-8?B?NDcy?=\r\n\r\n"
    );
    assert_eq!(
        extract_rfc5322_subject(mail).as_deref(),
        Some("Your Claude verification code is 918472")
    );
    assert_eq!(
        super::super::headless_oauth::extract_otps_from_email(mail, Some("owner@example.test")),
        vec!["918472"]
    );
    assert!(
        super::super::headless_oauth::extract_otps_from_email(mail, Some("other@example.test"))
            .is_empty()
    );
}

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
            aterm_uds::os_path!("open").to_string(),
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

type Case<'a> = (
    &'a [String],
    Option<&'a str>,
    &'a dyn Fn() -> Option<String>,
    u8,
);

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

#[test]
fn claude_com_authorize_origin_and_headless_cdp_oauth_complete_without_human_in_the_loop() {
    use super::super::headless_oauth::{
        CdpClient, MailboxCredential, drive_cdp_oauth, extract_otp_from_email, split_nul_messages,
    };

    let otp_eml = concat!(
        "From: Anthropic <no-reply@claude.com>\r\n",
        "To: <m27@example.test>\r\n",
        "Subject: Your Claude verification code\r\n\r\n",
        "<html><body><p>Use this verification code to sign in to Claude:</p>",
        "<div style=\"color:#123456\">849-204</div></body></html>\r\n"
    );
    assert_eq!(
        extract_otp_from_email(otp_eml, Some("other@example.test")),
        None,
        "OTP extractor must reject mismatched recipient"
    );
    assert_eq!(
        extract_otp_from_email(otp_eml, Some("m27@example.test")).as_deref(),
        Some("849204")
    );

    let (to_browser_r, to_browser_w) = std::io::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = std::io::pipe().expect("pipe");

    let authorize_url = concat!(
        "https://claude.com/cai/oauth/authorize",
        "?code=true",
        "&redirect_uri=http%3A%2F%2Flocalhost%3A9123%2Fcallback",
        "&state=st_cdp_77",
        "&login_hint=m27%40example.test"
    );

    let browser_thread = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        let mut eval_step = 0usize;
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let result = match method {
                    "Target.createTarget" => aterm_json::json!({"targetId": "t1"}),
                    "Target.attachToTarget" => aterm_json::json!({"sessionId": "s1"}),
                    "Page.enable" | "Runtime.enable" | "Page.navigate" | "Target.closeTarget" => {
                        aterm_json::json!({})
                    }
                    "Browser.close" => {
                        let reply = format!("{{\"id\":{id},\"result\":{{}}}}\0");
                        let _ = from_browser_w.write_all(reply.as_bytes());
                        return;
                    }
                    "Runtime.evaluate" => {
                        eval_step += 1;
                        let val = match eval_step {
                            1 => aterm_json::json!({
                                "state": "submitted_email",
                                "url": "https://claude.com/login"
                            }),
                            2 => aterm_json::json!({
                                "state": "submitted_otp",
                                "url": "https://claude.com/login/verify"
                            }),
                            3 => aterm_json::json!({
                                "state": "clicked_consent",
                                "url": "https://claude.com/cai/oauth/authorize"
                            }),
                            _ => aterm_json::json!({
                                "state": "callback",
                                "url": "http://127.0.0.1:9123/callback?code=auth_code_99&state=st_cdp_77"
                            }),
                        };
                        aterm_json::json!({"result": {"type": "object", "value": val}})
                    }
                    _ => aterm_json::json!({}),
                };
                let reply = aterm_json::json!({"id": id, "result": result});
                let mut bytes = aterm_json::to_vec(&reply).unwrap();
                bytes.push(0);
                if from_browser_w.write_all(&bytes).is_err() {
                    return;
                }
            }
        }
    });

    let mut cdp = CdpClient::new(to_browser_w, from_browser_r);
    let mut delivered = Vec::new();
    let res = drive_cdp_oauth(
        &mut cdp,
        authorize_url,
        None,
        Duration::from_secs(10),
        Duration::ZERO,
        |em| {
            assert_eq!(em, Some("m27@example.test"));
            Ok(MailboxCredential::Otp("849204".to_owned()))
        },
        |cb| {
            delivered.push(cb.to_owned());
            Ok(200)
        },
    );
    browser_thread.join().expect("stand-in browser thread");
    assert_eq!(res, Ok(()));
    assert_eq!(
        delivered,
        vec!["http://127.0.0.1:9123/callback?code=auth_code_99&state=st_cdp_77".to_owned()]
    );
}

#[cfg(unix)]
#[test]
fn headless_profiles_keep_persistent_data_after_authorization_errors() {
    use crate::harness::headless_oauth::{browser_profile_dir, with_browser_profile};

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let store = tmp.path().join("accounts");
    let label = browser_profile_dir(&store, Some("m27-2"));
    std::fs::create_dir_all(&label).unwrap();
    std::fs::write(label.join("Cookies"), b"existing label cookies").unwrap();
    let no_mailbox = "headless browser requires sign-in, but no mailbox is configured";
    assert_eq!(
        with_browser_profile(
            Some(&store),
            Some("m27-2"),
            Some("user@example.test"),
            |dir| {
                assert_eq!(dir, label);
                std::fs::write(dir.join("Local State"), b"partially refreshed state").unwrap();
                Err(no_mailbox)
            }
        ),
        Err(no_mailbox),
    );
    assert_eq!(
        std::fs::read(label.join("Cookies")).unwrap(),
        b"existing label cookies"
    );
    assert_eq!(
        std::fs::read(label.join("Local State")).unwrap(),
        b"partially refreshed state"
    );

    let email = browser_profile_dir(&store, Some("user@example.test"));
    std::fs::create_dir_all(&email).unwrap();
    std::fs::write(email.join("Cookies"), b"existing email cookies").unwrap();
    assert_eq!(
        with_browser_profile(
            Some(&store),
            Some("m27-2"),
            Some("user@example.test"),
            |dir| {
                assert_eq!(dir, email);
                Err(no_mailbox)
            }
        ),
        Err(no_mailbox),
    );
    assert_eq!(
        std::fs::read(email.join("Cookies")).unwrap(),
        b"existing email cookies"
    );
    assert_eq!(
        std::fs::read(label.join("Cookies")).unwrap(),
        b"existing label cookies"
    );

    let fresh = browser_profile_dir(&store, Some("fresh@example.test"));
    assert!(!fresh.exists());
    let timeout = "headless browser OAuth exceeded maximum steps without reaching callback";
    assert_eq!(
        with_browser_profile(
            Some(&store),
            Some("new-row"),
            Some("fresh@example.test"),
            |dir| {
                assert_eq!(dir, fresh);
                std::fs::write(dir.join("Cookies"), b"newly acquired cookies").unwrap();
                Err(timeout)
            }
        ),
        Err(timeout),
    );
    assert_eq!(
        std::fs::read(fresh.join("Cookies")).unwrap(),
        b"newly acquired cookies"
    );

    let wrong_account = "headless browser profile is signed in to a different account";
    assert_eq!(
        with_browser_profile(
            Some(&store),
            Some("m27-2"),
            Some("user@example.test"),
            |dir| {
                assert_eq!(dir, email);
                Err(wrong_account)
            }
        ),
        Err(wrong_account),
    );
    assert_eq!(
        std::fs::read(email.join("Cookies")).unwrap(),
        b"existing email cookies"
    );
    assert_eq!(
        std::fs::read(label.join("Cookies")).unwrap(),
        b"existing label cookies"
    );

    for result in [Err(no_mailbox), Err(wrong_account), Ok(())] {
        let mut scratch = None;
        assert_eq!(
            with_browser_profile(None, None, None, |dir| {
                assert!(dir.is_dir());
                std::fs::write(dir.join("Cookies"), b"scratch cookies").unwrap();
                scratch = Some(dir.to_path_buf());
                result
            }),
            result,
        );
        assert!(
            !scratch.unwrap().exists(),
            "the owned scratch profile is released"
        );
    }
}

#[cfg(unix)]
#[test]
fn mailbox_overrides_preserve_local_path_bytes_and_trim_urls() {
    use crate::harness::headless_oauth::{
        MailboxCredential, mailbox_config_from_override, poll_mailbox_config_for_credential,
    };
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let message = "To: <raw-path@example.test>\r\n\r\nYour verification code: 583921";
    let byte_path = tmp
        .path()
        .join(OsString::from_vec(b"maildir-\xff".to_vec()));
    let cfg = mailbox_config_from_override(byte_path.as_os_str(), Some(tmp.path()));
    assert_eq!(cfg.dir.as_deref(), Some(byte_path.as_path()));
    assert!(cfg.imap_url.is_none() && cfg.http_url.is_none());

    let decoy = tmp.path().join("maildir");
    std::fs::create_dir(&decoy).unwrap();
    std::fs::write(decoy.join("wrong.eml"), message.replace("583921", "999999")).unwrap();
    let dir = tmp.path().join("maildir ");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("code.eml"), message).unwrap();
    let cfg = mailbox_config_from_override(dir.as_os_str(), Some(tmp.path()));
    assert_eq!(cfg.dir.as_deref(), Some(dir.as_path()));
    assert_eq!(
        poll_mailbox_config_for_credential(
            &cfg,
            Some("raw-path@example.test"),
            Duration::from_secs(60),
        ),
        Ok(MailboxCredential::Otp("583921".to_owned()))
    );
    assert!(dir.join("code.eml.clicked").exists());
    assert!(!decoy.join("wrong.eml.clicked").exists());

    std::fs::write(
        tmp.path().join("mailbox.toml"),
        "[mailbox]\nhttp_url = 'https://wrong.example.test/inbox'\n",
    )
    .unwrap();
    let dir = tmp.path().join("configured-maildir");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("code.eml"), message).unwrap();
    let path = tmp.path().join("mailbox.toml ");
    std::fs::write(&path, "[mailbox]\ndir = '~/configured-maildir'\n").unwrap();
    let cfg = mailbox_config_from_override(path.as_os_str(), Some(tmp.path()));
    assert_eq!(cfg.dir.as_deref(), Some(dir.as_path()));
    assert!(cfg.http_url.is_none());
    assert_eq!(
        poll_mailbox_config_for_credential(
            &cfg,
            Some("raw-path@example.test"),
            Duration::from_secs(60),
        ),
        Ok(MailboxCredential::Otp("583921".to_owned()))
    );
    for (url, imap) in [
        (" https://mailbox.example.test/inbox \n", false),
        (" imaps://mailbox.example.test/INBOX \n", true),
    ] {
        let cfg = mailbox_config_from_override(OsStr::new(url), None);
        assert_eq!(cfg.imap_url.as_deref(), imap.then_some(url.trim()));
        assert_eq!(cfg.http_url.as_deref(), (!imap).then_some(url.trim()));
        assert!(cfg.dir.is_none());
    }
    assert!(!mailbox_config_from_override(OsStr::new(""), None).is_configured());
}

#[cfg(unix)]
#[test]
fn mailbox_secret_files_require_complete_regular_contents() {
    use super::super::headless_oauth::{MailboxConfig, discover_mailbox_config_files};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, symlink};

    let home = aterm_tempfile::tempdir().expect("private secret home");
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).unwrap();
    let secret = secrets.join("ai_mailbox_password");
    let discovered = || {
        discover_mailbox_config_files(Some(home.path()), None, None).and_then(|cfg| cfg.password)
    };
    let configured = || {
        MailboxConfig::from_toml_str(
            "password_file = '~/.secrets/ai_mailbox_password'\n",
            Some(home.path()),
        )
        .and_then(|cfg| cfg.password)
    };

    assert!(discovered().is_none() && configured().is_none());
    for (bytes, expected_discovery, expected_config) in [
        (
            b"  complete-secret\r\n".to_vec(),
            Some("complete-secret".to_owned()),
            Some("complete-secret".to_owned()),
        ),
        (
            vec![b'x'; 4096],
            Some("x".repeat(4096)),
            Some("x".repeat(4096)),
        ),
        (vec![b'x'; 4097], None, None),
        ([vec![b'x'; 4096], vec![0xff]].concat(), None, None),
        (vec![0xff], None, None),
        (b"\r\n  \r\n".to_vec(), None, None),
        (
            b"first\nsecond\n".to_vec(),
            None,
            Some("first\nsecond".to_owned()),
        ),
    ] {
        std::fs::write(&secret, &bytes).unwrap();
        assert_eq!(discovered(), expected_discovery);
        assert_eq!(configured(), expected_config);
        assert_eq!(std::fs::read(&secret).unwrap(), bytes);
    }
    std::fs::remove_file(&secret).unwrap();
    std::fs::create_dir(&secret).unwrap();
    assert!(discovered().is_none() && configured().is_none());
    assert!(secret.is_dir());
    std::fs::remove_dir(&secret).unwrap();

    let fifo_name = std::ffi::CString::new(secret.as_os_str().as_bytes()).unwrap();
    // SAFETY: one NUL-terminated private path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    assert!(discovered().is_none() && configured().is_none());
    assert!(
        std::fs::symlink_metadata(&secret)
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert_eq!(
        MailboxConfig::from_toml_str(
            "password = 'explicit-secret'\npassword_file = '~/.secrets/ai_mailbox_password'\n",
            Some(home.path()),
        )
        .and_then(|cfg| cfg.password)
        .as_deref(),
        Some("explicit-secret")
    );
    std::fs::remove_file(&secret).unwrap();

    let target = home.path().join("regular-secret");
    std::fs::write(&target, "aliased-secret\n").unwrap();
    symlink(&target, &secret).unwrap();
    assert_eq!(discovered().as_deref(), Some("aliased-secret"));
    assert_eq!(configured().as_deref(), Some("aliased-secret"));
    assert_eq!(std::fs::read_link(&secret).unwrap(), target);
    assert_eq!(std::fs::read(&target).unwrap(), b"aliased-secret\n");
}

#[test]
fn flat_webmail_password_aliases_keep_the_requested_account() {
    use super::super::headless_oauth::MailboxConfig;

    let home = aterm_tempfile::tempdir().expect("private password alias home");
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).unwrap();
    let secret = secrets.join("webmail-password");
    std::fs::write(&secret, "file-secret\r\n").unwrap();

    for (suffix, value, expected) in [
        (
            "_webmail_password_file",
            "~/.secrets/webmail-password",
            "file-secret",
        ),
        (
            "-webmail-password-file",
            "~/.secrets/webmail-password",
            "file-secret",
        ),
        (
            "_password_file",
            "~/.secrets/webmail-password",
            "file-secret",
        ),
        (
            "-password-file",
            "~/.secrets/webmail-password",
            "file-secret",
        ),
        ("_pass_file", "~/.secrets/webmail-password", "file-secret"),
        ("-pass-file", "~/.secrets/webmail-password", "file-secret"),
        ("_webmail_password", "direct-secret", "direct-secret"),
        ("-webmail-password", "direct-secret", "direct-secret"),
        ("_password", "direct-secret", "direct-secret"),
        ("-password", "direct-secret", "direct-secret"),
        ("_pass", "direct-secret", "direct-secret"),
        ("-pass", "direct-secret", "direct-secret"),
    ] {
        let mut cfg =
            MailboxConfig::from_toml_str(&format!("m27{suffix} = '{value}'\n"), Some(home.path()))
                .expect("parse flat per-account alias");
        assert_eq!(
            cfg.password_for(Some("m27")).as_deref(),
            Some(expected),
            "{suffix}"
        );
        assert_eq!(cfg.password_for(Some("m27_webmail")), None, "{suffix}");
        assert_eq!(cfg.password_for(Some("m27-webmail")), None, "{suffix}");
        assert_eq!(cfg.password_for(Some("m28")), None, "{suffix}");
        cfg.alias_label_to_email(Some("m27"), Some("opaque@example.test"));
        assert_eq!(
            cfg.password_for(Some("opaque@example.test")).as_deref(),
            Some(expected)
        );
    }
    assert_eq!(std::fs::read(&secret).unwrap(), b"file-secret\r\n");
}

#[test]
fn maildir_threads_keep_consumed_links_across_authorizations() {
    use super::super::headless_oauth::{
        MailboxConfig, MailboxCredential, poll_mailbox_config_for_credential,
    };
    use std::time::Duration;

    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-maildir-thread-history-")
        .tempdir()
        .unwrap();
    let thread = concat!(
        "To: <owner@example.test>\r\n\r\n",
        "Earlier link: https://claude.ai/magic-link#first\r\n",
        "Latest link: https://claude.ai/magic-link#second\r\n",
    );
    let first = MailboxCredential::MagicLink("https://claude.ai/magic-link#first".to_owned());
    let second = MailboxCredential::MagicLink("https://claude.ai/magic-link#second".to_owned());
    let poll = |cfg: &MailboxConfig, timeout| {
        // Each public call starts a new authorization's in-memory credential history.
        poll_mailbox_config_for_credential(cfg, Some("owner@example.test"), timeout)
    };

    let mailbox = tmp.path().join("persistent");
    std::fs::create_dir(&mailbox).unwrap();
    std::fs::write(mailbox.join("thread.eml"), thread).unwrap();
    let cfg = MailboxConfig::from_dir(&mailbox);
    assert_eq!(poll(&cfg, Duration::from_secs(60)), Ok(first.clone()));
    assert!(!mailbox.join("thread.eml.clicked").exists());
    let seen = cfg.seen_file.as_deref().unwrap();
    let first_history = std::fs::read(seen).unwrap();
    assert!(!first_history.is_empty());
    assert_eq!(poll(&cfg, Duration::from_secs(60)), Ok(second));
    assert!(mailbox.join("thread.eml.clicked").is_file());
    assert!(std::fs::read(seen).unwrap().starts_with(&first_history));
    assert!(poll(&cfg, Duration::ZERO).is_err());

    let mailbox = tmp.path().join("no-history");
    std::fs::create_dir(&mailbox).unwrap();
    std::fs::write(mailbox.join("thread.eml"), thread).unwrap();
    let cfg = MailboxConfig {
        dir: Some(mailbox.clone()),
        ..MailboxConfig::default()
    };
    assert_eq!(poll(&cfg, Duration::from_secs(60)), Ok(first.clone()));
    assert_eq!(poll(&cfg, Duration::from_secs(60)), Ok(first.clone()));
    assert!(!mailbox.join("thread.eml.clicked").exists());
    assert!(!mailbox.join(".mailbox.seen").exists());

    let mailbox = tmp.path().join("no-history-single");
    std::fs::create_dir(&mailbox).unwrap();
    std::fs::write(
        mailbox.join("single.eml"),
        "To: <owner@example.test>\r\n\r\nhttps://claude.ai/magic-link#first\r\n",
    )
    .unwrap();
    let cfg = MailboxConfig {
        dir: Some(mailbox.clone()),
        ..MailboxConfig::default()
    };
    assert_eq!(poll(&cfg, Duration::from_secs(60)), Ok(first));
    assert!(mailbox.join("single.eml.clicked").is_file());
    assert!(!mailbox.join(".mailbox.seen").exists());
    assert!(poll(&cfg, Duration::ZERO).is_err());

    let mailbox = tmp.path().join("recurring-otp");
    std::fs::create_dir(&mailbox).unwrap();
    let cfg = MailboxConfig::from_dir(&mailbox);
    for name in ["first.eml", "new-message.eml"] {
        std::fs::write(
            mailbox.join(name),
            format!(
                "To: <owner@example.test>\r\n\r\nVerification code: 111111\r\nMessage: {name}\r\n"
            ),
        )
        .unwrap();
        assert_eq!(
            poll(&cfg, Duration::from_secs(60)),
            Ok(MailboxCredential::Otp("111111".to_owned())),
        );
        assert!(mailbox.join(format!("{name}.clicked")).is_file());
    }
}

#[test]
fn a_plaintext_mailbox_with_credentials_is_refused_off_loopback() {
    use super::super::headless_oauth::{
        MailboxConfig, plaintext_credential_refusal, poll_mailbox_config_for_credential,
    };
    let imap = |url: &str| MailboxConfig {
        imap_url: Some(url.to_owned()),
        password: Some("pw".to_owned()),
        ..MailboxConfig::default()
    };
    let http = |url: &str, token: Option<&str>| MailboxConfig {
        http_url: Some(url.to_owned()),
        bearer_token: token.map(str::to_owned),
        ..MailboxConfig::default()
    };
    for spoken in [
        imap("imap://127.0.0.1:1143/INBOX"),
        imap("imap://localhost:1143/INBOX"),
        imap("imap://[::1]:1143/INBOX"),
        imap("imaps://mail.example.test/INBOX"),
        http("http://127.0.0.1:9/m", Some("t")),
        http("http://[::1]:9/m", Some("t")),
        http("http://mail.example.test/m", None),
        http("https://mail.example.test/m", Some("t")),
    ] {
        assert_eq!(
            plaintext_credential_refusal(&spoken, None),
            None,
            "{spoken:?}"
        );
    }
    for remote in [
        imap("imap://mail.example.test/INBOX"),
        imap("imap://127.0.0.1.example.test:143/INBOX"),
        http("http://mail.example.test/m", Some("t")),
        http("http://u:p@mail.example.test/m", None),
    ] {
        let why = plaintext_credential_refusal(&remote, None).expect("refused");
        assert_eq!(
            poll_mailbox_config_for_credential(&remote, None, Duration::ZERO),
            Err(why),
            "{remote:?}"
        );
    }
}

#[test]
fn imap_bridge_and_http_json_mailbox_fetch_credentials_and_track_seen_digests() {
    use super::super::headless_oauth::{
        MailboxConfig, MailboxCredential, poll_mailbox_config_for_credential,
    };
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let pass_file = tmp.path().join("imap.pass");
    std::fs::write(&pass_file, "bridge-secret-pass\n").expect("write pass");

    let toml_text = format!(
        r#"
[ai.mailbox]
imap_url = "imap://127.0.0.1:1143/INBOX"
user = "m27@example.test"
password_file = '{}'
"#,
        pass_file.display()
    );
    let parsed_cfg =
        MailboxConfig::from_toml_str(&toml_text, Some(tmp.path())).expect("parse mailbox toml");
    assert_eq!(
        parsed_cfg.imap_url.as_deref(),
        Some("imap://127.0.0.1:1143/INBOX")
    );
    assert_eq!(parsed_cfg.user.as_deref(), Some("m27@example.test"));
    assert_eq!(parsed_cfg.password.as_deref(), Some("bridge-secret-pass"));

    let imap_listener = TcpListener::bind("127.0.0.1:0").expect("bind imap");
    let imap_port = imap_listener.local_addr().expect("addr").port();
    let imap_thread = std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((stream, _)) = imap_listener.accept() else {
                break;
            };
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = BufReader::new(stream);
            let _ = writer.write_all(b"* OK [CAPABILITY IMAP4rev1] Local Bridge Ready\r\n");
            let _ = writer.flush();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim();
                let tag = trimmed.split_whitespace().next().unwrap_or("a");
                if trimmed.contains("LOGIN ") {
                    assert!(
                        trimmed.contains("\"m27@example.test\" \"bridge-secret-pass\""),
                        "unexpected IMAP LOGIN: {trimmed}"
                    );
                    let _ = writeln!(writer, "{tag} OK LOGIN completed\r");
                } else if trimmed.contains("SELECT ") {
                    let _ = writeln!(
                        writer,
                        "* 2 EXISTS\r\n{tag} OK [READ-WRITE] SELECT completed\r"
                    );
                } else if trimmed.contains("SEARCH ") {
                    // Unknown UTF-8 lines can split a byte-counted tag or SEARCH
                    // prefix. Ignore them, then read the mixed-case valid result.
                    let _ = writeln!(
                        writer,
                        "ééééé\r\n* 12345é\r\n* SeArCh 1 2\r\n{tag} OK SEARCH completed\r"
                    );
                } else if trimmed.contains("FETCH 2 ") {
                    let msg = concat!(
                        "From: Anthropic <no-reply@claude.com>\r\n",
                        "To: <m27@example.test>\r\n",
                        "Subject: Your Claude verification code\r\n\r\n",
                        "Your verification code is 739-182\r\n"
                    );
                    let _ = write!(
                        writer,
                        "* 2 FETCH (BODY[] {{{}}}\r\n{}\r\n)\r\n{tag} OK FETCH completed\r\n",
                        msg.len(),
                        msg
                    );
                } else if trimmed.contains("FETCH 1 ") {
                    let msg = concat!(
                        "From: Anthropic <no-reply@claude.com>\r\n",
                        "To: <other@example.test>\r\n\r\n",
                        "https://claude.com/cai/oauth/magic?token=wrong_user\r\n"
                    );
                    let _ = write!(
                        writer,
                        "* 1 FETCH (BODY[] {{{}}}\r\n{}\r\n)\r\n{tag} OK FETCH completed\r\n",
                        msg.len(),
                        msg
                    );
                } else if trimmed.contains("STORE ") {
                    let _ = writeln!(writer, "{tag} OK STORE completed\r");
                } else if trimmed.contains("LOGOUT") {
                    let _ = writeln!(writer, "* BYE Bridge logging out\r\n{tag} OK LOGOUT\r");
                    break;
                }
                let _ = writer.flush();
            }
        }
    });

    let seen_file = tmp.path().join("mailbox.seen");
    let imap_cfg = MailboxConfig {
        imap_url: Some(format!("imap://127.0.0.1:{imap_port}/INBOX")),
        user: Some("m27@example.test".to_owned()),
        password: Some("bridge-secret-pass".to_owned()),
        seen_file: Some(seen_file.clone()),
        ..MailboxConfig::default()
    };
    let cred = poll_mailbox_config_for_credential(
        &imap_cfg,
        Some("m27@example.test"),
        Duration::from_secs(5),
    )
    .expect("fetched OTP from IMAP");
    assert_eq!(cred, MailboxCredential::Otp("739182".to_owned()));

    let second = poll_mailbox_config_for_credential(
        &imap_cfg,
        Some("m27@example.test"),
        Duration::from_millis(80),
    );
    assert!(
        second.is_err(),
        "already-consumed IMAP message must not be reused"
    );
    imap_thread.join().expect("imap thread");

    let http_listener = TcpListener::bind("127.0.0.1:0").expect("bind http");
    let http_port = http_listener.local_addr().expect("addr").port();
    let http_thread = std::thread::spawn(move || {
        let Ok((mut stream, _)) = http_listener.accept() else {
            return;
        };
        let mut req_buf = [0u8; 4096];
        let n = stream.read(&mut req_buf).unwrap_or(0);
        let req_str = String::from_utf8_lossy(&req_buf[..n]);
        assert!(
            req_str.contains("Authorization: Bearer tok-99"),
            "missing bearer token: {req_str}"
        );
        let body = r#"{"messages":[{"To":[{"Address":"other@example.test"}],"HTML":"<a href=\"https://claude.com/cai/oauth/magic?token=wrong\">Sign in</a>"},{"To":[{"Address":"m27@example.test"}],"HTML":"<p><a href=\"https://claude.com/cai/oauth/magic?token=http_magic_ok&amp;state=s1\">Sign in to Claude</a></p>"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    });

    let http_cfg = MailboxConfig {
        http_url: Some(format!(
            "http://127.0.0.1:{http_port}/api/v1/messages?email={{email}}"
        )),
        bearer_token: Some("tok-99".to_owned()),
        seen_file: Some(tmp.path().join("http_mailbox.seen")),
        ..MailboxConfig::default()
    };
    let http_cred = poll_mailbox_config_for_credential(
        &http_cfg,
        Some("m27@example.test"),
        Duration::from_secs(5),
    )
    .expect("fetched magic link from HTTP mailbox");
    assert_eq!(
        http_cred,
        MailboxCredential::MagicLink(
            "https://claude.com/cai/oauth/magic?token=http_magic_ok&state=s1".to_owned()
        )
    );
    http_thread.join().expect("http thread");
}

#[test]
fn headless_webmail_cdp_and_totp_complete_oauth_without_human_in_the_loop() {
    use crate::harness::headless_oauth::{
        CdpClient, MailboxConfig, MailboxCredential, drive_cdp_oauth_with_webmail,
        generate_totp_at, poll_mailbox_or_webmail_for_credential, split_nul_messages,
    };
    use std::io::{Read, Write};

    let rfc_secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"; // gitleaks:allow
    assert_eq!(generate_totp_at(rfc_secret, 59).as_deref(), Some("287082"));
    assert_eq!(
        generate_totp_at(rfc_secret, 1_111_111_109).as_deref(),
        Some("081804")
    );

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let toml_str = r#"
[ai.mailbox]
webmail_url = "https://mail.example.test/u/0/inbox"

[ai.mailbox.passwords]
"user-a@example.test" = "pw-alpha"

[ai.mailbox.accounts."user-b@example.test"]
password = "pw-beta"
mailbox_password = "mpw-beta"
totp_secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ" # gitleaks:allow
"#;
    let mut cfg = MailboxConfig::from_toml_str(toml_str, Some(tmp.path())).expect("parse toml");
    cfg.seen_file = Some(tmp.path().join("webmail.seen"));
    assert!(cfg.is_configured());
    assert!(cfg.is_webmail_only(Some("user-b@example.test")));
    assert_eq!(
        cfg.password_for(Some("user-a@example.test")).as_deref(),
        Some("pw-alpha")
    );
    assert_eq!(
        cfg.password_for(Some("user-b@example.test")).as_deref(),
        Some("pw-beta")
    );
    assert_eq!(
        cfg.mailbox_password_for(Some("user-b@example.test"))
            .as_deref(),
        Some("mpw-beta")
    );
    assert_eq!(
        cfg.totp_secret_for(Some("user-b@example.test")).as_deref(),
        Some(rfc_secret)
    );

    let (to_browser_r, to_browser_w) = std::io::pipe().expect("pipe1");
    let (from_browser_r, mut from_browser_w) = std::io::pipe().expect("pipe2");

    let authorize_url = "https://claude.com/cai/oauth/authorize?client_id=c1&redirect_uri=http%3A%2F%2F127.0.0.1%3A9123%2Fcallback&state=st_webmail_42";

    let browser_thread = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        let mut target_seq = 0usize;
        let mut oauth_step = 0usize;
        let mut webmail_step = 0usize;
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let session_id = msg.get("sessionId").and_then(Value::as_str).unwrap_or("");
                let result = match method {
                    "Target.createTarget" => {
                        target_seq += 1;
                        aterm_json::json!({"targetId": format!("t{target_seq}")})
                    }
                    "Target.attachToTarget" => {
                        let tid = msg
                            .pointer("/params/targetId")
                            .and_then(Value::as_str)
                            .unwrap_or("t1");
                        aterm_json::json!({"sessionId": format!("s_{tid}")})
                    }
                    "Page.enable" | "Runtime.enable" | "Page.navigate" | "Target.closeTarget" => {
                        aterm_json::json!({})
                    }
                    "Browser.close" => {
                        let reply = format!("{{\"id\":{id},\"result\":{{}}}}\0");
                        let _ = from_browser_w.write_all(reply.as_bytes());
                        return;
                    }
                    "Runtime.evaluate" if session_id == "s_t2" || session_id == "s_t3" => {
                        webmail_step += 1;
                        let expr = msg
                            .pointer("/params/expression")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        assert!(
                            expr.contains("pw-beta"),
                            "webmail JS must carry per-account password"
                        );
                        let val = match webmail_step {
                            1 => aterm_json::json!({
                                "state": "submitted_user",
                                "url": "https://mail.example.test/login"
                            }),
                            2 => aterm_json::json!({
                                "state": "submitted_login",
                                "url": "https://mail.example.test/login/password"
                            }),
                            3 => aterm_json::json!({
                                "state": "submitted_totp",
                                "url": "https://mail.example.test/login/2fa"
                            }),
                            4 => aterm_json::json!({
                                "state": "inbox",
                                "url": "https://mail.example.test/u/0/inbox",
                                "clicked_row": true,
                                "html": "<div><span>1 minute ago</span><p>Your verification code is: 111111</p></div>",
                                "text": "Sign in to Claude\nYour verification code is: 111111"
                            }),
                            5 => aterm_json::json!({
                                "state": "inbox",
                                "url": "https://mail.example.test/u/0/inbox",
                                "clicked_row": true,
                                "html": "<div><span>2 minutes ago</span><p>Your verification code is: 111111</p></div>",
                                "text": "Sign in to Claude (2m ago)\nYour verification code is: 111111"
                            }),
                            _ => aterm_json::json!({
                                "state": "inbox",
                                "url": "https://mail.example.test/u/0/inbox",
                                "clicked_row": true,
                                "html": "<div><p>Your verification code is: 111111</p><p>Your verification code is: 908172</p></div>",
                                "text": "Sign in to Claude\nYour verification code is: 111111\nYour verification code is: 908172"
                            }),
                        };
                        aterm_json::json!({"result": {"type": "object", "value": val}})
                    }
                    "Runtime.evaluate" => {
                        oauth_step += 1;
                        let expr = msg
                            .pointer("/params/expression")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let val = match oauth_step {
                            1 => aterm_json::json!({
                                "state": "submitted_email",
                                "url": "https://claude.com/login"
                            }),
                            2 => {
                                assert!(
                                    expr.contains("111111"),
                                    "OAuth tab must first try initial unseen OTP"
                                );
                                aterm_json::json!({
                                    "state": "submitted_otp",
                                    "url": "https://claude.com/login/verify"
                                })
                            }
                            3..=5 => aterm_json::json!({
                                "state": "needs_mailbox",
                                "url": "https://claude.com/login/verify"
                            }),
                            6 => {
                                assert!(
                                    expr.contains("908172"),
                                    "OAuth tab must re-poll mailbox and receive fresh OTP after stale OTP fails"
                                );
                                aterm_json::json!({
                                    "state": "submitted_otp",
                                    "url": "https://claude.com/login/verify"
                                })
                            }
                            7 => {
                                assert!(
                                    expr.contains("continue as "),
                                    "OAuth JS must support account/workspace selection before final consent"
                                );
                                aterm_json::json!({
                                    "state": "clicked_consent",
                                    "url": "https://claude.com/cai/oauth/select-account",
                                    "target": "continue as user-b@example.test"
                                })
                            }
                            8 => aterm_json::json!({
                                "state": "clicked_consent",
                                "url": "https://claude.com/cai/oauth/authorize"
                            }),
                            _ => aterm_json::json!({
                                "state": "callback",
                                "url": "http://127.0.0.1:9123/callback?code=auth_code_webmail&state=st_webmail_42"
                            }),
                        };
                        aterm_json::json!({"result": {"type": "object", "value": val}})
                    }
                    _ => aterm_json::json!({}),
                };
                let reply = aterm_json::json!({"id": id, "result": result});
                let mut bytes = aterm_json::to_vec(&reply).unwrap();
                bytes.push(0);
                if from_browser_w.write_all(&bytes).is_err() {
                    return;
                }
            }
        }
    });

    let empty_maildir = tmp.path().join("empty_maildir");
    std::fs::create_dir_all(&empty_maildir).expect("mkdir empty_maildir");
    cfg.dir = Some(empty_maildir);

    let mut cdp = CdpClient::new(to_browser_w, from_browser_r);
    let mut delivered = Vec::new();
    let mut seen_otps = std::collections::BTreeSet::new();
    let res = drive_cdp_oauth_with_webmail(
        &mut cdp,
        authorize_url,
        Some("user-b@example.test"),
        Duration::from_secs(60),
        Duration::ZERO,
        |cdp_inner, em| {
            poll_mailbox_or_webmail_for_credential(
                cdp_inner,
                &cfg,
                em,
                Duration::from_secs(60),
                Duration::ZERO,
                &mut seen_otps,
            )
        },
        |cb_url| {
            delivered.push(cb_url.to_owned());
            Ok(200)
        },
    );
    assert_eq!(res, Ok(()));
    assert_eq!(
        delivered,
        vec![
            "http://127.0.0.1:9123/callback?code=auth_code_webmail&state=st_webmail_42".to_owned()
        ]
    );
    browser_thread.join().expect("browser thread");

    fn poll_webmail_fixture(
        cfg: &MailboxConfig,
        email: &str,
        message: &str,
        seen_otps: &mut std::collections::BTreeSet<String>,
    ) -> Result<MailboxCredential, &'static str> {
        let (to_browser_r, to_browser_w) = std::io::pipe().unwrap();
        let (from_browser_r, mut from_browser_w) = std::io::pipe().unwrap();
        let message = message.to_owned();
        let browser = std::thread::spawn(move || {
            let mut reader = to_browser_r;
            let mut pending = Vec::new();
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    return;
                }
                pending.extend_from_slice(&buf[..n]);
                for msg in split_nul_messages(&mut pending) {
                    let id = msg.get("id").and_then(Value::as_u64).unwrap();
                    let method = msg.get("method").and_then(Value::as_str).unwrap();
                    let result = match method {
                        "Target.createTarget" => aterm_json::json!({"targetId": "fresh"}),
                        "Target.attachToTarget" => aterm_json::json!({"sessionId": "s_fresh"}),
                        "Runtime.evaluate" => aterm_json::json!({
                            "result": {"value": {
                                "state": "inbox",
                                "html": format!("<p>Your verification code: 111111</p><p>{message}</p>"),
                                "text": format!("Sign in to Claude: 111111\n{message}")
                            }}
                        }),
                        _ => aterm_json::json!({}),
                    };
                    let mut reply =
                        aterm_json::to_vec(&aterm_json::json!({"id": id, "result": result}))
                            .unwrap();
                    reply.push(0);
                    if from_browser_w.write_all(&reply).is_err() || method == "Target.closeTarget" {
                        return;
                    }
                }
            }
        });
        let mut client = CdpClient::new(to_browser_w, from_browser_r);
        let result = poll_mailbox_or_webmail_for_credential(
            &mut client,
            cfg,
            Some(email),
            Duration::from_secs(60),
            Duration::ZERO,
            seen_otps,
        );
        drop(client);
        browser.join().expect("fresh-message browser fixture");
        result
    }

    // A legacy seen-file line (the raw hash of OTP 111111) stays in the history, and a fresh
    // message's code is still accepted in its own scope.
    let webmail_cfg = MailboxConfig {
        dir: None,
        ..cfg.clone()
    };
    let seen_file = cfg.seen_file.as_ref().unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(seen_file)
        .unwrap()
        .write_all(b"d0176b48bafb572b\n")
        .unwrap();
    let legacy_history = std::fs::read(seen_file).unwrap();
    assert_eq!(
        poll_webmail_fixture(
            &webmail_cfg,
            "user-a@example.test",
            "new message for another account",
            &mut seen_otps,
        ),
        Ok(MailboxCredential::Otp("111111".to_owned())),
        "another account may receive the same code within this flow"
    );
    let mut next_flow_otps = std::collections::BTreeSet::new();
    assert_eq!(
        poll_webmail_fixture(
            &webmail_cfg,
            "user-b@example.test",
            "new message in a later authorization",
            &mut next_flow_otps,
        ),
        Ok(MailboxCredential::Otp("111111".to_owned())),
        "a later authorization may receive the same code for the same account"
    );
    let magic_link = "https://claude.ai/magic?token=fresh_message_link";
    assert_eq!(
        poll_webmail_fixture(
            &webmail_cfg,
            "user-a@example.test",
            magic_link,
            &mut seen_otps
        ),
        Ok(MailboxCredential::MagicLink(magic_link.to_owned())),
    );
    assert!(
        poll_webmail_fixture(
            &webmail_cfg,
            "user-a@example.test",
            &format!("new DOM timestamp, same consumed link: {magic_link}"),
            &mut std::collections::BTreeSet::new(),
        )
        .is_err(),
        "a unique consumed magic link remains refused in a later flow"
    );
    let maildir = cfg.dir.as_ref().unwrap();
    std::fs::write(
        maildir.join("flow.eml"),
        "To: <user-b@example.test>\r\n\r\nYour verification code: 111111",
    )
    .unwrap();
    let mut maildir_flow_otps = std::collections::BTreeSet::new();
    assert_eq!(
        poll_webmail_fixture(
            &cfg,
            "user-b@example.test",
            "Maildir source has priority",
            &mut maildir_flow_otps,
        ),
        Ok(MailboxCredential::Otp("111111".to_owned())),
    );
    assert!(maildir.join("flow.eml.clicked").is_file());
    assert!(
        poll_webmail_fixture(
            &webmail_cfg,
            "user-b@example.test",
            "same flow, stale Maildir code in a new DOM",
            &mut maildir_flow_otps,
        )
        .is_err(),
        "a code already delivered from Maildir is also stale in webmail"
    );
    assert!(
        std::fs::read(seen_file)
            .unwrap()
            .starts_with(&legacy_history)
    );

    {
        let (to_browser_r, to_browser_w) = std::io::pipe().unwrap();
        let (from_browser_r, mut from_browser_w) = std::io::pipe().unwrap();
        let browser = std::thread::spawn(move || {
            let mut reader = to_browser_r;
            let mut pending = Vec::new();
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    return;
                }
                pending.extend_from_slice(&buf[..n]);
                for msg in split_nul_messages(&mut pending) {
                    let id = msg.get("id").and_then(Value::as_u64).unwrap();
                    let method = msg.get("method").and_then(Value::as_str).unwrap();
                    let result = match method {
                        "Target.createTarget" => aterm_json::json!({"targetId": "totp_missing"}),
                        "Target.attachToTarget" => {
                            aterm_json::json!({"sessionId": "s_totp_missing"})
                        }
                        "Runtime.evaluate" => aterm_json::json!({
                            "result": {"value": {
                                "state": "needs_totp",
                                "url": "https://mail.example.test/login/2fa"
                            }}
                        }),
                        _ => aterm_json::json!({}),
                    };
                    let mut reply =
                        aterm_json::to_vec(&aterm_json::json!({"id": id, "result": result}))
                            .unwrap();
                    reply.push(0);
                    if from_browser_w.write_all(&reply).is_err() || method == "Target.closeTarget" {
                        return;
                    }
                }
            }
        });
        let mut client = CdpClient::new(to_browser_w, from_browser_r);
        let no_totp_cfg = MailboxConfig {
            dir: None,
            totp_secret: None,
            totp_secrets: BTreeMap::new(),
            ..cfg.clone()
        };
        let mut empty_otps = std::collections::BTreeSet::new();
        assert_eq!(
            poll_mailbox_or_webmail_for_credential(
                &mut client,
                &no_totp_cfg,
                Some("user-a@example.test"),
                Duration::from_secs(60),
                Duration::ZERO,
                &mut empty_otps,
            ),
            Err("webmail sign-in requires a TOTP 2FA secret in mailbox.toml or ai-mailbox.toml"),
            "missing TOTP secret on a 2FA webmail login must fail fast with a clear diagnostic"
        );
        drop(client);
        browser.join().expect("totp-missing browser fixture");
    }

    let store_dir = tmp.path().join("accounts");
    let label_profile =
        super::super::headless_oauth::browser_profile_dir(&store_dir, Some("m27-2"));
    std::fs::create_dir_all(&label_profile).expect("mkdir label_profile");
    assert_eq!(
        super::super::headless_oauth::resolve_browser_profile_dir(
            &store_dir,
            Some("m27-2"),
            Some("user-b@example.test"),
        ),
        label_profile,
        "resolve_browser_profile_dir must reuse existing label profile directory when email profile does not exist"
    );
    let email_profile =
        super::super::headless_oauth::browser_profile_dir(&store_dir, Some("user-b@example.test"));
    std::fs::create_dir_all(&email_profile).expect("mkdir email_profile");
    assert_eq!(
        super::super::headless_oauth::resolve_browser_profile_dir(
            &store_dir,
            Some("m27-2"),
            Some("user-b@example.test"),
        ),
        email_profile,
        "resolve_browser_profile_dir must prefer existing email profile directory when present"
    );

    let styled_otp_eml = concat!(
        "From: Anthropic <no-reply@claude.com>\r\n",
        "To: <user-b@example.test>\r\n\r\n",
        "<html><head><style>.verification-code { font-weight: 600; z-index: 100000; }</style></head>",
        "<body><div>Use this verification code to sign in to Claude:</div>",
        "<div><span>582914</span> (expires in 10 minutes)</div>",
        "<div>548 Market St #999888, San Francisco</div></body></html>"
    );
    assert_eq!(
        super::super::headless_oauth::extract_otp_from_email(
            styled_otp_eml,
            Some("user-b@example.test")
        )
        .as_deref(),
        Some("582914")
    );

    let thread_links_eml = concat!(
        "To: <user-b@example.test>\r\n\r\n",
        "<div><a href=\"https://claude.ai/magic?token=old_tok\">Sign in</a></div>",
        "<div><a href=\"https://claude.ai/magic?token=new_tok\">Sign in</a></div>"
    );
    assert_eq!(
        super::super::headless_oauth::extract_all_credentials_from_email(
            thread_links_eml,
            Some("user-b@example.test")
        ),
        vec![
            super::super::headless_oauth::MailboxCredential::MagicLink(
                "https://claude.ai/magic?token=old_tok".to_owned()
            ),
            super::super::headless_oauth::MailboxCredential::MagicLink(
                "https://claude.ai/magic?token=new_tok".to_owned()
            ),
        ]
    );
    let thread_otps_eml = concat!(
        "To: <user-b@example.test>\r\n\r\n",
        "<div>Your verification code is: 111111</div>",
        "<div>Your verification code is: 222222</div>"
    );
    assert_eq!(
        super::super::headless_oauth::extract_all_credentials_from_email(
            thread_otps_eml,
            Some("user-b@example.test")
        ),
        vec![
            super::super::headless_oauth::MailboxCredential::Otp("111111".to_owned()),
            super::super::headless_oauth::MailboxCredential::Otp("222222".to_owned()),
        ]
    );

    let autodiscover_cfg = MailboxConfig {
        password: Some("app-pw".to_owned()),
        ..MailboxConfig::default()
    };
    assert_eq!(
        autodiscover_cfg
            .effective_imap_url(Some("alice@fastmail.com"))
            .as_deref(),
        Some("imaps://imap.fastmail.com:993/INBOX")
    );
    assert_eq!(
        autodiscover_cfg
            .effective_webmail_url(Some("alice@fastmail.com"))
            .as_deref(),
        Some("https://app.fastmail.com/mail/Inbox")
    );
    assert_eq!(
        autodiscover_cfg
            .effective_imap_url(Some("alice@gmail.com"))
            .as_deref(),
        Some("imaps://imap.gmail.com:993/INBOX")
    );
    let mut scoped_passwords = BTreeMap::new();
    scoped_passwords.insert("only@custom-domain.test".to_owned(), "pw".to_owned());
    let scoped_cfg = MailboxConfig {
        webmail_url: Some("https://mail.custom-domain.test".to_owned()),
        passwords: scoped_passwords,
        ..MailboxConfig::default()
    };
    assert_eq!(
        super::super::headless_oauth::poll_mailbox_config_for_credential(
            &scoped_cfg,
            Some("only@custom-domain.test"),
            Duration::from_secs(60),
        ),
        Err("webmail inbox requires headless browser")
    );
    let mut proton_only = BTreeMap::new();
    proton_only.insert("only@example.test".to_owned(), "pw".to_owned());
    let unconfigured_other_cfg = MailboxConfig {
        passwords: proton_only,
        ..MailboxConfig::default()
    };
    assert_eq!(
        super::super::headless_oauth::poll_mailbox_config_for_credential(
            &unconfigured_other_cfg,
            Some("other@example.test"),
            Duration::from_secs(60),
        ),
        Err("headless browser requires sign-in, but no mailbox is configured")
    );

    let std_maildir = tmp.path().join("std_maildir");
    std::fs::create_dir_all(std_maildir.join("new")).expect("mkdir new");
    std::fs::create_dir_all(std_maildir.join("cur")).expect("mkdir cur");
    let new_msg = std_maildir.join("new").join("1700000000.123_1.host");
    std::fs::write(
        &new_msg,
        "To: <user-b@example.test>\r\n\r\nYour verification code: 778899",
    )
    .expect("write maildir new msg");
    let std_maildir_cfg = MailboxConfig::from_dir(&std_maildir);
    assert_eq!(
        super::super::headless_oauth::poll_mailbox_config_for_credential(
            &std_maildir_cfg,
            Some("user-b@example.test"),
            Duration::from_millis(50),
        ),
        Ok(super::super::headless_oauth::MailboxCredential::Otp(
            "778899".to_owned()
        ))
    );
    let cur_msg = std_maildir.join("cur").join("1700000000.123_1.host:2,S");
    std::fs::rename(&new_msg, &cur_msg).expect("move new to cur with :2,S flag");
    assert_eq!(
        super::super::headless_oauth::poll_mailbox_config_for_credential(
            &std_maildir_cfg,
            Some("user-b@example.test"),
            Duration::from_millis(50),
        ),
        Err("timed out waiting for verification email in mailbox"),
        "a message moved from new/ to cur/ with :2,S flags retains its clicked marker"
    );

    for tag in ["style", "script"] {
        for space in [" ", "\t", "\r", "\n", "\x0c"] {
            let raw = format!(
                "To: <user-b@example.test>\r\n\r\n<{tag} data-fixture='raw'>100000</{tag}{space}>\
                 <div>Your verification code:</div><div>582914</div>"
            );
            assert_eq!(
                super::super::headless_oauth::extract_otp_from_email(
                    &raw,
                    Some("user-b@example.test")
                )
                .as_deref(),
                Some("582914"),
                "{tag} closing whitespace {space:?}"
            );
        }
    }
    for tag in ["styleguide", "scripture"] {
        let raw = format!(
            "To: <user-b@example.test>\r\n\r\n<{tag}>Your verification code: 582914</{tag}>"
        );
        assert_eq!(
            super::super::headless_oauth::extract_otp_from_email(&raw, Some("user-b@example.test"))
                .as_deref(),
            Some("582914"),
            "{tag} is not a raw-text block"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions(PathBuf, std::fs::Permissions);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }

        let probe_dir = tmp.path().join("probe_mb");
        std::fs::create_dir_all(&probe_dir).expect("mkdir probe_mb");
        let probe_seen = probe_dir.join(".mailbox.seen");
        std::fs::write(&probe_seen, b"existing_digest\n").expect("seed seen");
        let pre_clicked = probe_dir.join("old.eml.clicked");
        std::fs::write(&pre_clicked, b"1\n").expect("seed old clicked");
        let probe_cfg = MailboxConfig {
            dir: Some(probe_dir.clone()),
            seen_file: Some(probe_seen.clone()),
            ..MailboxConfig::default()
        };
        std::fs::write(
            probe_dir.join("first.eml"),
            "To: <user-b@example.test>\r\n\r\nYour verification code: 111111",
        )
        .expect("write initial OTP");
        let seen_permissions = RestorePermissions(
            probe_seen.clone(),
            std::fs::metadata(&probe_seen).unwrap().permissions(),
        );
        std::fs::set_permissions(&probe_seen, std::fs::Permissions::from_mode(0o0)).unwrap();
        let seen_unreadable = std::fs::read(&probe_seen).is_err();
        let mut pending = None;
        let mut seen_otps = std::collections::BTreeSet::new();
        let failed_probe = super::authorize_using_with_page(
            authorize_url,
            Some("user-b@example.test"),
            |email, _| {
                super::poll_http_mailbox_credential(
                    &probe_cfg,
                    email,
                    Duration::from_secs(60),
                    &mut pending,
                )
            },
            &mut |method, url, _, _| {
                assert_eq!((method, url), ("GET", authorize_url));
                Ok(HttpResponse {
                    status: 200,
                    location: None,
                    body: "<input type='email'>".to_owned(),
                })
            },
        );
        assert_eq!(
            failed_probe,
            Err("verification email contained a 6-digit OTP requiring headless browser")
        );
        let first_clicked = probe_dir.join("first.eml.clicked");
        assert!(first_clicked.exists());
        assert!(probe_seen.exists(), "existing seen history is retained");
        if seen_unreadable {
            assert!(std::fs::read(&probe_seen).is_err());
        }
        std::fs::set_permissions(&probe_seen, seen_permissions.1.clone()).unwrap();
        // Maildir consumption may append a message digest; earlier history stays intact.
        assert!(
            std::fs::read(&probe_seen)
                .unwrap()
                .starts_with(b"existing_digest\n")
        );

        std::fs::write(
            probe_dir.join("other.eml"),
            "To: <other@example.test>\r\n\r\nYour verification code: 222222",
        )
        .expect("write another flow's OTP");
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_config_for_credential(
                &probe_cfg,
                Some("other@example.test"),
                Duration::from_secs(60),
            ),
            Ok(MailboxCredential::Otp("222222".to_owned()))
        );
        let other_clicked = probe_dir.join("other.eml.clicked");
        std::fs::write(&probe_seen, b"existing_digest\nfresh_digest\n").expect("append seen");
        std::fs::write(
            probe_dir.join("fresh.eml"),
            "To: <user-b@example.test>\r\n\r\nYour verification code: 908172",
        )
        .expect("write replacement OTP");
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_or_pending_credential(
                &mut pending,
                &mut cdp,
                &probe_cfg,
                Some("user-b@example.test"),
                Duration::from_secs(60),
                Duration::ZERO,
                &mut seen_otps,
            ),
            Ok(MailboxCredential::Otp("111111".to_owned())),
            "first browser request reuses the already-consumed HTTP credential"
        );
        assert!(pending.is_none());
        assert!(!probe_dir.join("fresh.eml.clicked").exists());
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_or_pending_credential(
                &mut pending,
                &mut cdp,
                &probe_cfg,
                Some("user-b@example.test"),
                Duration::from_secs(60),
                Duration::ZERO,
                &mut seen_otps,
            ),
            Ok(MailboxCredential::Otp("908172".to_owned())),
            "later browser request polls a fresh credential"
        );
        assert!(probe_dir.join("fresh.eml.clicked").exists());
        assert!(pre_clicked.exists());
        assert!(first_clicked.exists());
        assert!(
            other_clicked.exists(),
            "another flow's consumption is retained"
        );
        assert!(
            std::fs::read_to_string(&probe_seen)
                .unwrap()
                .starts_with("existing_digest\nfresh_digest\n")
        );
    }
    assert!(super::page_needs_browser_before_email(
        "<html><head><script src='/app.js'></script></head><body><div id='root'></div></body></html>"
    ));
    assert!(super::page_needs_browser_before_email(
        "<html><body><form><input type='email' name='email'></form></body></html>"
    ));
    assert!(!super::page_needs_browser_before_email(
        "<html><body><p>Check your email for a magic link</p></body></html>"
    ));

    {
        use super::super::headless_oauth::{
            base32_decode, discover_mailbox_config_files, generate_totp_at,
        };
        let otpauth_uri = "otpauth://totp/Proton:sol-m27%40proton.me?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Proton&digits=6&period=30"; // gitleaks:allow
        assert_eq!(
            base32_decode(otpauth_uri).as_deref(),
            Some(b"12345678901234567890".as_slice())
        );
        assert_eq!(generate_totp_at(otpauth_uri, 59).as_deref(), Some("287082"));
        let otpauth_8digit = "otpauth://totp/Example:user@example.test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=8&period=30"; // gitleaks:allow
        assert_eq!(
            generate_totp_at(otpauth_8digit, 59).as_deref(),
            Some("94287082")
        );

        let split_home = tmp.path().join("split_home");
        std::fs::create_dir_all(split_home.join(".config/aterm")).expect("config dir");
        std::fs::create_dir_all(split_home.join(".secrets")).expect("secrets dir");
        let pw_file = split_home.join(".secrets/m27.pass");
        std::fs::write(&pw_file, "file-pass-m27\n").expect("write pw file");
        std::fs::write(
            split_home.join(".config/aterm/mailbox.toml"),
            r#"
"direct-user@proton.me" = "direct-pw"

[password_files]
"sol-m27@proton.me" = "~/.secrets/m27.pass"
"#,
        )
        .expect("write config mailbox.toml");
        std::fs::write(
            split_home.join(".secrets/ai-mailbox.toml"),
            format!(
                r#"
[totp_secrets]
"m27" = "{otpauth_uri}"

[mailbox_passwords]
"sol-m27@proton.me" = "second-pw-m27"
"#
            ),
        )
        .expect("write secrets ai-mailbox.toml");

        let merged = discover_mailbox_config_files(Some(&split_home), None, None)
            .expect("merged split mailbox config");
        assert_eq!(
            merged
                .password_for(Some("direct-user@proton.me"))
                .as_deref(),
            Some("direct-pw")
        );
        assert_eq!(
            merged.password_for(Some("sol-m27@proton.me")).as_deref(),
            Some("file-pass-m27")
        );
        assert_eq!(
            merged
                .mailbox_password_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some("second-pw-m27")
        );
        let resolved_totp = merged
            .totp_secret_for(Some("sol-m27@proton.me"))
            .expect("totp derived from otpauth label email");
        assert_eq!(
            generate_totp_at(&resolved_totp, 59).as_deref(),
            Some("287082")
        );

        let step_js = super::super::headless_oauth::oauth_step_js(
            Some("sol-m27@proton.me"),
            None,
            false,
            false,
        );
        assert!(step_js.contains("switched_account"));
        assert!(step_js.contains("wrong_account"));

        let (to_r, to_w) = std::io::pipe().expect("pipe1");
        let (from_r, mut from_w) = std::io::pipe().expect("pipe2");
        let wrong_thread = std::thread::spawn(move || {
            let mut reader = to_r;
            let mut pending = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                pending.extend_from_slice(&buf[..n]);
                for req in super::super::headless_oauth::split_nul_messages(&mut pending) {
                    let id = req.get("id").and_then(aterm_json::Value::as_u64).unwrap();
                    let method = req
                        .get("method")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or("");
                    let result = match method {
                        "Target.createTarget" => aterm_json::json!({"targetId": "t_wrong"}),
                        "Target.attachToTarget" => aterm_json::json!({"sessionId": "s_wrong"}),
                        "Runtime.evaluate" => aterm_json::json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "state": "wrong_account",
                                    "url": "https://claude.com/cai/oauth/authorize"
                                }
                            }
                        }),
                        _ => aterm_json::json!({}),
                    };
                    let mut out = aterm_json::to_vec(&aterm_json::json!({
                        "id": id,
                        "result": result
                    }))
                    .unwrap();
                    out.push(0);
                    let _ = from_w.write_all(&out);
                    let _ = from_w.flush();
                    if method == "Browser.close" {
                        return;
                    }
                }
            }
        });
        let mut wrong_cdp = super::super::headless_oauth::CdpClient::new(to_w, from_r);
        let wrong_res = super::super::headless_oauth::drive_cdp_oauth_with_webmail(
            &mut wrong_cdp,
            "https://claude.com/cai/oauth/authorize?client_id=c1&redirect_uri=http%3A%2F%2F127.0.0.1%3A9123%2Fcallback&state=st_wrong",
            Some("sol-m27@proton.me"),
            Duration::from_secs(5),
            Duration::ZERO,
            |_, _| Err("mailbox must not be polled on wrong_account"),
            |_| Ok(200),
        );
        assert_eq!(
            wrong_res,
            Err("headless browser profile is signed in to a different account")
        );
        wrong_thread.join().expect("wrong_account browser fixture");

        assert_eq!(
            super::super::headless_oauth::derive_account_label_from_email("sol-m27@proton.me")
                .as_deref(),
            Some("m27")
        );
        assert_eq!(
            super::super::headless_oauth::derive_account_label_from_email("sun.m27.2@proton.me")
                .as_deref(),
            Some("m27-2")
        );
        assert_eq!(
            super::super::headless_oauth::derive_account_label_from_email("solar.m27.3@proton.me")
                .as_deref(),
            Some("m27-3")
        );

        let one_line_cfg =
            super::super::headless_oauth::MailboxConfig::from_toml_str("raw-secret-pass\n", None)
                .expect("single-line password in ai-mailbox.toml");
        assert_eq!(
            one_line_cfg
                .password_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some("raw-secret-pass")
        );

        let label_home = tmp.path().join("label_secrets_home");
        std::fs::create_dir_all(label_home.join(".secrets")).expect("create .secrets");
        std::fs::write(label_home.join(".secrets/m27_password"), "m27-file-pw\n")
            .expect("write m27_password");
        std::fs::write(
            label_home.join(".secrets/m27-2_password"),
            "m27-2-file-pw\n",
        )
        .expect("write m27-2_password");
        std::fs::write(
            label_home.join(".secrets/m27_totp"),
            format!("{otpauth_uri}\n"),
        )
        .expect("write m27_totp");
        let label_cfg = discover_mailbox_config_files(Some(&label_home), None, None)
            .expect("discover from single-line secret files");
        assert_eq!(
            label_cfg.password_for(Some("sol-m27@proton.me")).as_deref(),
            Some("m27-file-pw")
        );
        assert_eq!(
            label_cfg
                .password_for(Some("sun.m27.2@proton.me"))
                .as_deref(),
            Some("m27-2-file-pw")
        );
        assert_eq!(
            label_cfg
                .totp_secret_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some(otpauth_uri)
        );

        let thread_dir = tmp.path().join("multi_otp_thread_mb");
        std::fs::create_dir_all(&thread_dir).expect("mkdir multi_otp_thread_mb");
        let thread_cfg = MailboxConfig {
            dir: Some(thread_dir.clone()),
            ..MailboxConfig::default()
        };
        std::fs::write(
            thread_dir.join("thread.eml"),
            "To: <sol-m27@proton.me>\r\n\r\nEarlier verification code: 111111\r\nLatest verification code: 908172\r\n",
        )
        .expect("write two-OTP thread");
        let mut thread_seen_otps = std::collections::BTreeSet::new();
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_or_webmail_for_credential(
                &mut cdp,
                &thread_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_secs(5),
                Duration::ZERO,
                &mut thread_seen_otps,
            ),
            Ok(MailboxCredential::Otp("111111".to_owned()))
        );
        assert!(
            !thread_dir.join("thread.eml.clicked").exists(),
            "thread file with remaining unseen OTP must not be marked clicked yet"
        );
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_or_webmail_for_credential(
                &mut cdp,
                &thread_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_secs(5),
                Duration::ZERO,
                &mut thread_seen_otps,
            ),
            Ok(MailboxCredential::Otp("908172".to_owned()))
        );
        assert!(
            thread_dir.join("thread.eml.clicked").exists(),
            "thread file is marked clicked once its final unseen OTP is consumed"
        );

        let link_thread_dir = tmp.path().join("multi_link_thread_mb");
        std::fs::create_dir_all(&link_thread_dir).expect("mkdir multi_link_thread_mb");
        let link_thread_cfg = MailboxConfig {
            dir: Some(link_thread_dir.clone()),
            ..MailboxConfig::default()
        };
        std::fs::write(
            link_thread_dir.join("thread_links.eml"),
            "To: <sol-m27@proton.me>\r\n\r\nEarlier link: https://claude.ai/magic-link#tok_old\r\nLatest link: https://claude.ai/magic-link#tok_new\r\n",
        )
        .expect("write two-link thread");
        let mut link_thread_seen = std::collections::BTreeSet::new();
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_or_webmail_for_credential(
                &mut cdp,
                &link_thread_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_secs(5),
                Duration::ZERO,
                &mut link_thread_seen,
            ),
            Ok(MailboxCredential::MagicLink(
                "https://claude.ai/magic-link#tok_old".to_owned()
            ))
        );
        assert!(
            !link_thread_dir.join("thread_links.eml.clicked").exists(),
            "thread file with remaining unseen magic link must not be marked clicked yet"
        );
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_or_webmail_for_credential(
                &mut cdp,
                &link_thread_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_secs(5),
                Duration::ZERO,
                &mut link_thread_seen,
            ),
            Ok(MailboxCredential::MagicLink(
                "https://claude.ai/magic-link#tok_new".to_owned()
            ))
        );
        assert!(
            link_thread_dir.join("thread_links.eml.clicked").exists(),
            "thread file is marked clicked once its final unseen magic link is consumed"
        );

        let scoped_cfg = super::super::headless_oauth::MailboxConfig::from_toml_str(
            r#"
webmail_url = "https://mail.proton.me/u/0/inbox"
imap_url = "imaps://imap.fastmail.com:993/INBOX"

[passwords]
"sol-m27@proton.me" = "pw-m27"
"#,
            None,
        )
        .expect("parse scoped mailbox config");
        assert!(
            scoped_cfg
                .effective_webmail_url(Some("sol-m27@proton.me"))
                .is_some()
        );
        assert!(
            scoped_cfg
                .effective_imap_url(Some("sol-m27@proton.me"))
                .is_some()
        );
        assert!(
            scoped_cfg
                .effective_webmail_url(Some("other@example.test"))
                .is_none(),
            "scoped webmail_url must not activate for unconfigured account"
        );
        assert!(
            scoped_cfg
                .effective_imap_url(Some("other@example.test"))
                .is_none(),
            "scoped imap_url without embedded password must not activate for unconfigured account"
        );

        let embedded_imap_cfg = super::super::headless_oauth::MailboxConfig::from_toml_str(
            r#"
imap_url = "imap://bridge:secret@127.0.0.1:1143/INBOX" # gitleaks:allow

[passwords]
"sol-m27@proton.me" = "pw-m27"
"#,
            None,
        )
        .expect("parse embedded-password imap config");
        assert!(embedded_imap_cfg.is_configured_for(Some("other@example.test")));
        assert!(
            embedded_imap_cfg
                .effective_imap_url(Some("other@example.test"))
                .is_some()
        );
        assert!(step_js.contains("link has expired"));

        let srcdoc_thread = concat!(
            "To: <sol-m27@proton.me>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
            "<iframe sandbox=\"\" srcdoc=\"&lt;p&gt;Your&amp;nbsp;verification&amp;nbsp;code:&amp;nbsp;111111&lt;/p&gt;\"></iframe>",
            "<iframe sandbox=\"\" srcdoc=\"&lt;p&gt;Your&amp;nbsp;verification&amp;nbsp;code:&amp;nbsp;&lt;strong&gt;908172&lt;/strong&gt;&lt;/p&gt;\"></iframe>",
        );
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                srcdoc_thread,
                Some("sol-m27@proton.me")
            ),
            vec!["908172".to_owned(), "111111".to_owned()],
            "sandboxed srcdoc iframes are decoded with the newest (bottom-most) message prioritized first"
        );
        let srcdoc_link = concat!(
            "To: <sol-m27@proton.me>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
            "<iframe sandbox=\"\" srcdoc=\"&lt;a href=&quot;https://claude.ai/magic-link?token=abc&amp;amp;state=s1&quot;&gt;Sign in&lt;/a&gt;\"></iframe>",
        );
        assert_eq!(
            super::extract_magic_link_from_email(srcdoc_link, Some("sol-m27@proton.me")).as_deref(),
            Some("https://claude.ai/magic-link?token=abc&state=s1")
        );

        let split_boxes_email = concat!(
            "To: <sol-m27@proton.me>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
            "<p>Your verification code:</p>",
            "<div>9</div><div>0</div><div>8</div><div>1</div><div>7</div><div>2</div>",
            "<p>Earlier code: <span>1</span><span>2</span><span>3</span><span>4</span><span>5</span><span>6</span></p>",
            "<p>Backup login code: 654 321.</p>",
        );
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                split_boxes_email,
                Some("sol-m27@proton.me")
            ),
            vec![
                "908172".to_owned(),
                "123456".to_owned(),
                "654321".to_owned()
            ],
            "split-box div/span digits and spaced 3-digit OTP groups are extracted in order"
        );

        let us_home = tmp.path().join("underscore_secrets_home");
        std::fs::create_dir_all(us_home.join(".secrets")).expect("create .secrets");
        std::fs::write(us_home.join(".secrets/m27_2_pass"), "us-m27-2-pw\n")
            .expect("write m27_2_pass");
        std::fs::write(
            us_home.join(".secrets/m27_2_otpauth"),
            format!("{otpauth_uri}\n"),
        )
        .expect("write m27_2_otpauth");
        let mut us_cfg = discover_mailbox_config_files(Some(&us_home), None, None)
            .expect("discover underscore secret files");
        assert_eq!(
            us_cfg.password_for(Some("sun.m27.2@proton.me")).as_deref(),
            Some("us-m27-2-pw")
        );
        assert_eq!(
            us_cfg
                .totp_secret_for(Some("sun.m27.2@proton.me"))
                .as_deref(),
            Some(otpauth_uri)
        );
        us_cfg.alias_label_to_email(Some("m27-2"), Some("alias-m27-2@example.test"));
        assert_eq!(
            us_cfg
                .password_for(Some("alias-m27-2@example.test"))
                .as_deref(),
            Some("us-m27-2-pw")
        );

        let flat_toml = format!(
            r#"
m27_password = "flat-m27-pw"
m27_totp = "{otpauth_uri}"
m27_2_pass = "flat-m27-2-pw"
"#
        );
        let flat_cfg = super::super::headless_oauth::MailboxConfig::from_toml_str(&flat_toml, None)
            .expect("parse top-level suffixed keys in ai-mailbox.toml");
        assert_eq!(
            flat_cfg.password_for(Some("sol-m27@proton.me")).as_deref(),
            Some("flat-m27-pw")
        );
        assert_eq!(
            flat_cfg
                .totp_secret_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some(otpauth_uri)
        );
        assert_eq!(
            flat_cfg
                .password_for(Some("sun.m27.2@proton.me"))
                .as_deref(),
            Some("flat-m27-2-pw")
        );

        let raw_multiline = format!(
            "# raw credentials\nraw-m27-pw\nraw-m27-second-pw\n{otpauth_uri}\nhttps://mail.proton.me/u/0/inbox\n"
        );
        let raw_cfg =
            super::super::headless_oauth::MailboxConfig::from_toml_str(&raw_multiline, None)
                .expect("parse raw multi-line credentials");
        assert_eq!(
            raw_cfg.password_for(Some("sol-m27@proton.me")).as_deref(),
            Some("raw-m27-pw")
        );
        assert_eq!(
            raw_cfg
                .mailbox_password_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some("raw-m27-second-pw")
        );
        assert_eq!(
            raw_cfg
                .totp_secret_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some(otpauth_uri)
        );
        assert_eq!(
            raw_cfg
                .effective_webmail_url(Some("sol-m27@proton.me"))
                .as_deref(),
            Some("https://mail.proton.me/u/0/inbox")
        );

        let bare_home = tmp.path().join("bare_label_secrets_home");
        std::fs::create_dir_all(bare_home.join(".secrets")).expect("create .secrets");
        std::fs::write(
            bare_home.join(".secrets/m27"),
            format!("bare-m27-pw\n{otpauth_uri}\n"),
        )
        .expect("write bare m27 secret file");
        let bare_cfg = discover_mailbox_config_files(Some(&bare_home), None, None)
            .expect("discover bare label secret file");
        assert_eq!(
            bare_cfg.password_for(Some("sol-m27@proton.me")).as_deref(),
            Some("bare-m27-pw")
        );
        assert_eq!(
            bare_cfg
                .totp_secret_for(Some("sol-m27@proton.me"))
                .as_deref(),
            Some(otpauth_uri)
        );

        let mailhog_msg = r#"{
            "ID": "998877",
            "To": [{"Relays": null, "Mailbox": "sol-m27", "Domain": "proton.me", "Params": ""}],
            "Content": {
                "Headers": {
                    "Content-Transfer-Encoding": ["quoted-printable"],
                    "Subject": ["Sign in to Claude"]
                },
                "Body": "<a href=3D\"https://claude.ai/magic-link?token=3Dmhog_tok&amp;state=3Ds7\">Sign in</a>"
            }
        }"#;
        assert_eq!(
            super::extract_magic_link_from_email(mailhog_msg, Some("sol-m27@proton.me")).as_deref(),
            Some("https://claude.ai/magic-link?token=mhog_tok&state=s7")
        );
        assert_eq!(
            super::extract_magic_link_from_email(mailhog_msg, Some("other@proton.me")),
            None
        );

        let graph_msg = r#"{
            "id": "182930",
            "subject": "Your Claude verification code",
            "toRecipients": [{"emailAddress": {"name": "Sol", "address": "sol-m27@proton.me"}}],
            "body": {
                "contentType": "html",
                "content": "<html><body><p>Your verification code is <strong>731904</strong></p></body></html>"
            }
        }"#;
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                graph_msg,
                Some("sol-m27@proton.me")
            ),
            vec!["731904".to_owned()],
            "Microsoft Graph nested body.content OTP is extracted without polluting with JSON id field"
        );
        assert!(
            super::super::headless_oauth::extract_otps_from_email(
                graph_msg,
                Some("other@proton.me")
            )
            .is_empty()
        );

        let gmail_msg = r#"{
            "id": "654321",
            "payload": {
                "headers": [
                    {"name": "To", "value": "sol-m27@proton.me"},
                    {"name": "Subject", "value": "Sign in to Claude"}
                ],
                "parts": [
                    {
                        "mimeType": "text/html",
                        "body": {
                            "data": "PGh0bWw-PGJvZHk-VmVyaWZpY2F0aW9uIGNvZGU6IDxzdHJvbmc-NjE5NDAyPC9zdHJvbmc-PC9ib2R5PjwvaHRtbD4="
                        }
                    }
                ]
            }
        }"#;
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                gmail_msg,
                Some("sol-m27@proton.me")
            ),
            vec!["619402".to_owned()]
        );

        let jmap_msg = r#"{
            "id": "112233",
            "to": [{"email": "sol-m27@proton.me"}],
            "bodyValues": {
                "part1": {
                    "value": "Your verification code: 528193"
                }
            }
        }"#;
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                jmap_msg,
                Some("sol-m27@proton.me")
            ),
            vec!["528193".to_owned()]
        );

        let mbox_file = tmp.path().join("inbox.mbox");
        std::fs::write(
            &mbox_file,
            concat!(
                "From no-reply@claude.ai Fri Oct 23 11:00:00 2026\n",
                "To: <other@proton.me>\n\n",
                "Your verification code: 333333\n\n",
                "From no-reply@claude.ai Fri Oct 23 11:01:00 2026\n",
                "To: <sol-m27@proton.me>\n\n",
                "Your verification code: 111111\n\n",
                "From no-reply@claude.ai Fri Oct 23 11:02:00 2026\n",
                "To: <sol-m27@proton.me>\n\n",
                "Your verification code: 908172\n",
            ),
        )
        .expect("write mbox file");
        let mbox_cfg = MailboxConfig::from_dir(&mbox_file);
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_config_for_credential(
                &mbox_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_millis(50),
            ),
            Ok(MailboxCredential::Otp("908172".to_owned())),
            "mbox file returns newest appended message for target recipient"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&mbox_file)
            .expect("open mbox for append")
            .write_all(
                b"\nFrom no-reply@claude.ai Fri Oct 23 11:03:00 2026\nTo: <sol-m27@proton.me>\n\nYour verification code: 432109\n",
            )
            .expect("append mbox message");
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_config_for_credential(
                &mbox_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_millis(50),
            ),
            Ok(MailboxCredential::Otp("432109".to_owned())),
            "appending a new message to an mbox file is picked up on subsequent polls"
        );

        let mpit_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mailpit mock");
        let mpit_port = mpit_listener.local_addr().unwrap().port();
        let mpit_thread = std::thread::spawn(move || {
            for expected_path in ["/api/v1/messages", "/api/v1/message/mpit-42"] {
                let Ok((mut stream, _)) = mpit_listener.accept() else {
                    return;
                };
                let mut req_buf = [0u8; 4096];
                let n = stream.read(&mut req_buf).unwrap_or(0);
                let req_str = String::from_utf8_lossy(&req_buf[..n]);
                assert!(
                    req_str.contains(expected_path),
                    "expected {expected_path}, got: {req_str}"
                );
                let body = if expected_path == "/api/v1/messages" {
                    r#"{"messages":[{"ID":"mpit-other","To":[{"Address":"other@proton.me"}],"Subject":"Sign in","Snippet":"Click below"},{"ID":"mpit-42","To":[{"Address":"sol-m27@proton.me"}],"Subject":"Sign in to Claude","Snippet":"Click the link below to sign in"}]}"#
                } else {
                    r#"{"ID":"mpit-42","To":[{"Address":"sol-m27@proton.me"}],"HTML":"<p><a href=\"https://claude.ai/magic-link?token=mpit_detail_ok&amp;state=s9\">Sign in</a></p>"}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        let mpit_cfg = MailboxConfig {
            http_url: Some(format!("http://127.0.0.1:{mpit_port}/api/v1/messages")),
            seen_file: Some(tmp.path().join("mpit.seen")),
            ..MailboxConfig::default()
        };
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_config_for_credential(
                &mpit_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_secs(5),
            ),
            Ok(MailboxCredential::MagicLink(
                "https://claude.ai/magic-link?token=mpit_detail_ok&state=s9".to_owned()
            )),
            "Mailpit summary list fetches /api/v1/message/<ID> detail when summary omits HTML body"
        );
        mpit_thread.join().expect("mailpit mock thread");

        let rfc2047_q_eml = concat!(
            "From: Anthropic <no-reply@claude.ai>\r\n",
            "To: =?UTF-8?Q?Sol_M27?= <sol-m27@proton.me>\r\n",
            "Subject: =?UTF-8?Q?Your_Claude_verification_code_is_814209?=\r\n\r\n",
            "Enter the verification code shown in the subject line.\r\n"
        );
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                rfc2047_q_eml,
                Some("sol-m27@proton.me")
            ),
            vec!["814209".to_owned()],
            "RFC 2047 Q-encoded Subject header OTP is decoded and extracted"
        );
        let rfc2047_b_eml = concat!(
            "From: Anthropic <no-reply@claude.ai>\r\n",
            "To: =?UTF-8?B?c29sLW0yN0Bwcm90b24ubWU=?=\r\n",
            "Subject: =?UTF-8?B?OTA4MTcyIGlzIHlvdXIgQ2xhdWRlIHZlcmlmaWNhdGlvbiBjb2Rl?=\r\n\r\n",
            "Sign in with the code in the subject.\r\n"
        );
        assert_eq!(
            super::super::headless_oauth::extract_otps_from_email(
                rfc2047_b_eml,
                Some("sol-m27@proton.me")
            ),
            vec!["908172".to_owned()],
            "RFC 2047 B-encoded To and Subject headers are decoded and extracted"
        );

        let basic_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind basic auth mock");
        let basic_port = basic_listener.local_addr().unwrap().port();
        let basic_thread = std::thread::spawn(move || {
            let Ok((mut stream, _)) = basic_listener.accept() else {
                return;
            };
            let mut req_buf = [0u8; 4096];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_str = String::from_utf8_lossy(&req_buf[..n]);
            assert!(
                req_str.contains("Authorization: Basic YWRtaW46c2VjcmV0"),
                "expected Basic auth header for admin:secret, got: {req_str}"
            );
            assert!(
                req_str.contains(&format!("Host: 127.0.0.1:{basic_port}\r\n")),
                "Host header must strip embedded userinfo, got: {req_str}"
            );
            let body = r#"{"to":"sol-m27@proton.me","otp":"604821"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        });
        let basic_cfg = MailboxConfig {
            http_url: Some(format!(
                "http://admin:secret@127.0.0.1:{basic_port}/api/v1/inbox"
            )),
            seen_file: Some(tmp.path().join("basic.seen")),
            ..MailboxConfig::default()
        };
        assert_eq!(
            super::super::headless_oauth::poll_mailbox_config_for_credential(
                &basic_cfg,
                Some("sol-m27@proton.me"),
                Duration::from_secs(5),
            ),
            Ok(MailboxCredential::Otp("604821".to_owned())),
            "loopback HTTP client sends Basic auth for embedded user:pass@127.0.0.1 URLs"
        );
        basic_thread.join().expect("basic auth mock thread");

        let provider_cfg = MailboxConfig {
            password: Some("pw".to_owned()),
            ..MailboxConfig::default()
        };
        assert_eq!(
            provider_cfg
                .effective_imap_url(Some("sol@outlook.com"))
                .as_deref(),
            Some("imaps://outlook.office365.com:993/INBOX")
        );
        assert_eq!(
            provider_cfg
                .effective_webmail_url(Some("sol@outlook.com"))
                .as_deref(),
            Some("https://outlook.live.com/mail/0/inbox")
        );
        assert_eq!(
            provider_cfg
                .effective_imap_url(Some("sol@purelymail.com"))
                .as_deref(),
            Some("imaps://imap.purelymail.com:993/INBOX")
        );
        assert_eq!(
            provider_cfg
                .effective_webmail_url(Some("sol@tuta.com"))
                .as_deref(),
            Some("https://app.tuta.com/mail")
        );

        let oauth_js = super::super::headless_oauth::oauth_step_js(
            Some("sol-m27@proton.me"),
            Some("908172"),
            false,
            false,
        );
        assert!(oauth_js.contains(".find(visibleControl)"));
        assert!(oauth_js.contains("el.focus()"));

        let wm_js = super::super::headless_oauth::webmail_step_js(
            (
                Some("sol-m27@proton.me"),
                Some("pw"),
                Some("mpw"),
                Some("123456"),
            ),
            (true, true, false, false),
            0,
            &[],
        );
        assert!(wm_js.contains(":not(#mailboxPassword):not([name='mailboxPassword'])"));
        assert!(wm_js.contains("input[name='loginfmt']"));
        assert!(wm_js.contains("input[name='otc']"));

        let home_dir = tmp.path().join("home-fleet");
        let secrets_dir = home_dir.join(".secrets");
        std::fs::create_dir_all(&secrets_dir).expect("create secrets dir");
        std::fs::write(secrets_dir.join("m27_pw_file"), "file-pw-m27\n").expect("write pw file");
        std::fs::write(secrets_dir.join("m27_mpw_file"), "file-mpw-m27\n").expect("write mpw file");
        std::fs::write(secrets_dir.join("m27_totp_file"), "JBSWY3DPEHPK3PXP\n")
            .expect("write totp file");
        std::fs::write(
            secrets_dir.join("ai-mailbox.toml"),
            "m27_password_file = \"~/.secrets/m27_pw_file\"\n\
             m27_mailbox_password_file = \"~/.secrets/m27_mpw_file\"\n\
             m27_totp_secret_file = \"~/.secrets/m27_totp_file\"\n\n\
             [work]\n\
             email = \"custom-team@fastmail.com\"\n\
             password = \"work-secret\"\n",
        )
        .expect("write ai-mailbox.toml");
        std::fs::write(
            secrets_dir.join("fleet-accounts.toml"),
            "[[account]]\n\
             label = \"m27\"\n\
             machines = [\"m27\"]\n\
             class = \"dedicated\"\n\
             email = \"opaque-alias@proton.me\"\n",
        )
        .expect("write fleet-accounts.toml");
        let discovered_fleet = super::super::headless_oauth::discover_mailbox_config_files(
            Some(&home_dir),
            None,
            None,
        )
        .expect("discover fleet mailbox config");
        assert_eq!(
            discovered_fleet
                .password_for(Some("opaque-alias@proton.me"))
                .as_deref(),
            Some("file-pw-m27")
        );
        assert_eq!(
            discovered_fleet
                .mailbox_password_for(Some("opaque-alias@proton.me"))
                .as_deref(),
            Some("file-mpw-m27")
        );
        assert_eq!(
            discovered_fleet
                .totp_secret_for(Some("opaque-alias@proton.me"))
                .as_deref(),
            Some("JBSWY3DPEHPK3PXP")
        );
        assert_eq!(
            discovered_fleet
                .password_for(Some("custom-team@fastmail.com"))
                .as_deref(),
            Some("work-secret")
        );
    }
}

#[test]
fn recovered_callbacks_require_delivery_but_reached_callbacks_allow_duplicates() {
    use crate::harness::headless_oauth::{
        CdpClient, drive_cdp_oauth_with_webmail, split_nul_messages,
    };
    use std::io::{Read, Write};

    #[derive(Clone, Copy, Debug)]
    enum Observation {
        Event,
        RecoveredPage,
        ReachedPage,
    }

    const CALLBACK: &str = "http://127.0.0.1:9124/callback?code=cb_unreach_ok&state=st_unreach";
    for observation in [
        Observation::Event,
        Observation::RecoveredPage,
        Observation::ReachedPage,
    ] {
        for delivery in [Ok(200), Ok(500), Err("callback delivery failed")] {
            let (to_r, to_w) = std::io::pipe().expect("pipe1");
            let (from_r, mut from_w) = std::io::pipe().expect("pipe2");
            let browser_thread = std::thread::spawn(move || {
                let mut reader = to_r;
                let mut pending = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buf[..n]);
                    for req in split_nul_messages(&mut pending) {
                        let id = req.get("id").and_then(Value::as_u64).unwrap();
                        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
                        if method == "Runtime.evaluate"
                            && !matches!(observation, Observation::RecoveredPage)
                        {
                            let frame = match observation {
                                Observation::Event => aterm_json::json!({
                                    "url": "chrome-error://chromewebdata/",
                                    "unreachableUrl": CALLBACK
                                }),
                                _ => aterm_json::json!({"url": CALLBACK}),
                            };
                            let mut evt = aterm_json::to_vec(&aterm_json::json!({
                                "sessionId": "s_unreach",
                                "method": "Page.frameNavigated",
                                "params": {"frame": frame}
                            }))
                            .unwrap();
                            evt.push(0);
                            from_w.write_all(&evt).unwrap();
                        }
                        let result = match method {
                            "Target.createTarget" => aterm_json::json!({"targetId": "t_unreach"}),
                            "Target.attachToTarget" => {
                                aterm_json::json!({"sessionId": "s_unreach"})
                            }
                            "Runtime.evaluate" => {
                                let (state, url) = match observation {
                                    Observation::Event => {
                                        ("waiting", "chrome-error://chromewebdata/")
                                    }
                                    Observation::RecoveredPage => ("callback_recovered", CALLBACK),
                                    Observation::ReachedPage => ("callback", CALLBACK),
                                };
                                aterm_json::json!({"result": {"type": "object", "value": {"state": state, "url": url}}})
                            }
                            _ => aterm_json::json!({}),
                        };
                        let mut out =
                            aterm_json::to_vec(&aterm_json::json!({"id": id, "result": result}))
                                .unwrap();
                        out.push(0);
                        from_w.write_all(&out).unwrap();
                        from_w.flush().unwrap();
                        if method == "Browser.close" {
                            return;
                        }
                    }
                }
            });
            let mut cdp = CdpClient::new(to_w, from_r);
            let mut delivered = Vec::new();
            let result = drive_cdp_oauth_with_webmail(
                &mut cdp,
                "https://claude.com/cai/oauth/authorize?client_id=c1&redirect_uri=http%3A%2F%2F127.0.0.1%3A9124%2Fcallback&state=st_unreach",
                Some("sol-m27@proton.me"),
                Duration::from_secs(60),
                Duration::ZERO,
                |_, _| Err("mailbox must not be polled when callback is observed"),
                |url| {
                    delivered.push(url.to_owned());
                    delivery
                },
            );
            let expected = if matches!(observation, Observation::ReachedPage) {
                Ok(())
            } else {
                match delivery {
                    Ok(200) => Ok(()),
                    Ok(_) => {
                        Err("the local OAuth callback listener rejected the authorization code")
                    }
                    Err(err) => Err(err),
                }
            };
            assert_eq!(result, expected, "{observation:?}, {delivery:?}");
            assert_eq!(delivered, [CALLBACK]);
            drop(cdp);
            browser_thread.join().expect("callback browser fixture");
        }
    }
}

#[test]
fn gmail_stub_ids_do_not_masquerade_as_otps_and_forwarded_headers_match_target() {
    use super::super::headless_oauth::{
        MailboxConfig, MailboxCredential, extract_otp_from_email,
        poll_mailbox_config_for_credential,
    };

    let forwarded_raw = "To: team-alias@example.test\r\n\
X-Forwarded-To: <sol-m27@proton.me>\r\n\
Subject: Sign in to Claude\r\n\r\n\
Your verification code is: 814209\r\n";
    assert_eq!(
        extract_otp_from_email(forwarded_raw, Some("sol-m27@proton.me")).as_deref(),
        Some("814209"),
        "X-Forwarded-To must match target even when To: names an alias"
    );

    let undisclosed_raw = "Received: from mail.anthropic.com by mx.proton.me for <sol-m27@proton.me>; Fri, 23 Oct 2026 12:00:00 +0000\r\n\
To: undisclosed-recipients:;\r\n\
Subject: Sign in to Claude\r\n\r\n\
<a href=\"https://claude.com/cai/oauth/magic?token=undisclosed_ok\">Sign in</a>\r\n";
    assert_eq!(
        extract_magic_link_from_email(undisclosed_raw, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.com/cai/oauth/magic?token=undisclosed_ok"),
        "To: undisclosed-recipients:; must not block matching Received: for <target>"
    );

    let orig_rcpt_raw = "To: catchall@example.test\r\n\
Original-Recipient: rfc822;sol-m27@proton.me\r\n\
Subject: Sign in\r\n\r\n\
Verification code: 390174\r\n";
    assert_eq!(
        extract_otp_from_email(orig_rcpt_raw, Some("sol-m27@proton.me")).as_deref(),
        Some("390174")
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock gmail api");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        for expected_path in [
            "/gmail/v1/users/me/messages",
            "/gmail/v1/users/me/messages/999888",
            "/gmail/v1/users/me/messages/482910",
        ] {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut req_buf = [0u8; 2048];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_str = String::from_utf8_lossy(&req_buf[..n]);
            assert!(
                req_str.starts_with(&format!("GET {expected_path} ")),
                "unexpected request: {req_str}"
            );
            let body = match expected_path {
                "/gmail/v1/users/me/messages" => {
                    r#"{"messages":[{"id":"482910","threadId":"654321"},{"id":"999888","threadId":"777666"}]}"#
                }
                "/gmail/v1/users/me/messages/999888" => {
                    r#"{"id":"999888","payload":{"headers":[{"name":"To","value":"other@proton.me"},{"name":"Subject","value":"Sign in"}],"body":{"data":"Verification code: 111222"}}}"#
                }
                _ => {
                    r#"{"id":"482910","payload":{"headers":[{"name":"To","value":"sol-m27@proton.me"},{"name":"Subject","value":"Sign in to Claude"}],"body":{"data":"Your verification code is 741852"}}}"#
                }
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });

    let cfg = MailboxConfig {
        http_url: Some(format!(
            "http://127.0.0.1:{port}/gmail/v1/users/me/messages"
        )),
        ..MailboxConfig::default()
    };
    let cred =
        poll_mailbox_config_for_credential(&cfg, Some("sol-m27@proton.me"), Duration::from_secs(5))
            .expect("poll gmail stub list and detail");
    assert_eq!(cred, MailboxCredential::Otp("741852".to_owned()));
    server.join().unwrap();
}

#[test]
fn mailpit_detail_loop_stops_after_200_and_secret_file_extensions_strip_cleanly() {
    use super::super::headless_oauth::{
        MailboxConfig, MailboxCredential, derive_account_label_from_email,
        discover_mailbox_config_files, poll_mailbox_config_for_credential,
    };

    assert_eq!(
        derive_account_label_from_email("sol.m27.2.claude@proton.me").as_deref(),
        Some("m27-2"),
        "trailing non-numeric words after slot digit must still derive m27-2"
    );
    assert_eq!(
        derive_account_label_from_email("sol.m27.28.2.claude@proton.me").as_deref(),
        Some("m27-m28-2")
    );

    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-secret-stems-")
        .tempdir()
        .expect("tempdir");
    let secrets = tmp.path().join(".secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(
        secrets.join("m27-2.toml"),
        "password = \"pw-m27-2-toml\"\nmailbox_password = \"mpw-m27-2\"\n",
    )
    .unwrap();
    std::fs::write(secrets.join("sun.m27.3.txt"), "pw-m27-3-txt\n").unwrap();

    let cfg = discover_mailbox_config_files(Some(tmp.path()), None, None)
        .expect("discover mailbox config from .toml/.txt secret files");
    assert_eq!(
        cfg.password_for(Some("sun.m27.2@proton.me")).as_deref(),
        Some("pw-m27-2-toml")
    );
    assert_eq!(
        cfg.mailbox_password_for(Some("sun.m27.2@proton.me"))
            .as_deref(),
        Some("mpw-m27-2")
    );
    assert_eq!(
        cfg.password_for(Some("star.m27.3@proton.me")).as_deref(),
        Some("pw-m27-3-txt")
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock mailpit");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        // The newer mpit-2 is not a sign-in email: after reading `/api/v1/message/mpit-2` the
        // poller moves to mpit-1 without requesting `/api/v1/messages/mpit-2`.
        for expected_path in [
            "/api/v1/messages",
            "/api/v1/message/mpit-2",
            "/api/v1/message/mpit-1",
        ] {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut req_buf = [0u8; 2048];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_str = String::from_utf8_lossy(&req_buf[..n]);
            assert!(
                req_str.starts_with(&format!("GET {expected_path} ")),
                "unexpected request: {req_str}"
            );
            let body = match expected_path {
                "/api/v1/messages" => {
                    r#"{"messages":[{"ID":"mpit-1","To":[{"Address":"sol-m27@proton.me"}],"Subject":"Sign in to Claude"},{"ID":"mpit-2","To":[{"Address":"sol-m27@proton.me"}],"Subject":"Weekly digest"}]}"#
                }
                "/api/v1/message/mpit-2" => {
                    r#"{"ID":"mpit-2","To":[{"Address":"sol-m27@proton.me"}],"Subject":"Weekly digest","Text":"Welcome to your weekly summary."}"#
                }
                _ => {
                    r#"{"ID":"mpit-1","To":[{"Address":"sol-m27@proton.me"}],"Subject":"Sign in to Claude","HTML":"<a href=\"https://claude.com/cai/oauth/magic?token=mpit1_ok\">Sign in</a>"}"#
                }
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });

    let mpit_cfg = MailboxConfig {
        http_url: Some(format!("http://127.0.0.1:{port}/api/v1/messages")),
        ..MailboxConfig::default()
    };
    let cred = poll_mailbox_config_for_credential(
        &mpit_cfg,
        Some("sol-m27@proton.me"),
        Duration::from_secs(60),
    )
    .expect("poll mailpit with non-auth message ahead of auth message");
    assert_eq!(
        cred,
        MailboxCredential::MagicLink(
            "https://claude.com/cai/oauth/magic?token=mpit1_ok".to_owned()
        )
    );
    server.join().unwrap();
}

#[test]
fn approval_form_selection_root_loopback_callback_and_csv_slot_inference() {
    use super::super::accounts_sync::parse_accounting_csv;
    use super::super::headless_oauth::{
        MailboxConfig, MailboxCredential, mailbox_config_from_override,
        poll_mailbox_config_for_credential,
    };

    // CSV rows whose email encodes a slot (`.2.claude`) must retain their slot even when the
    // `computer` column is `m27` and row 2 appears before row 1.
    let csv = concat!(
        "email,computer,claude\n",
        "sun.m27.2.claude@example.test,m27,1\n",
        "sol-m27@example.test,m27,1\n",
    );
    let fleet = parse_accounting_csv(csv, "slots.csv").expect("parse csv");
    assert_eq!(fleet.accounts.len(), 2);
    assert_eq!(fleet.accounts[0].label, "m27-2");
    assert_eq!(fleet.accounts[0].machines, vec!["m27".to_owned()]);
    assert_eq!(fleet.accounts[1].label, "m27");

    // Webmail UI URLs in `url = ...` or `mailbox_config_from_override` populate `webmail_url`.
    let toml_cfg = MailboxConfig::from_toml_str(
        "url = \"https://mail.proton.me/u/0/inbox\"\npassword = \"pw\"\n",
        None,
    )
    .expect("parse proton url");
    assert_eq!(
        toml_cfg.webmail_url.as_deref(),
        Some("https://mail.proton.me/u/0/inbox")
    );
    assert!(toml_cfg.http_url.is_none());
    let override_cfg =
        mailbox_config_from_override(std::ffi::OsStr::new("https://app.fastmail.com"), None);
    assert_eq!(
        override_cfg.webmail_url.as_deref(),
        Some("https://app.fastmail.com")
    );
    assert!(override_cfg.http_url.is_none());

    // Single `.eml` file as `dir` must write `<file>.clicked` in its parent directory.
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-single-eml-")
        .tempdir()
        .expect("tempdir");
    let eml_path = tmp.path().join("single.eml");
    std::fs::write(
        &eml_path,
        concat!(
            "To: <sol-m27@proton.me>\r\n",
            "Subject: Sign in to Claude\r\n\r\n",
            "https://claude.ai/magic?token=single_eml_tok\r\n",
        ),
    )
    .unwrap();
    let eml_cfg = MailboxConfig::from_dir(&eml_path);
    let eml_cred = poll_mailbox_config_for_credential(
        &eml_cfg,
        Some("sol-m27@proton.me"),
        Duration::from_secs(5),
    )
    .expect("poll single eml file");
    assert_eq!(
        eml_cred,
        MailboxCredential::MagicLink("https://claude.ai/magic?token=single_eml_tok".to_owned())
    );
    assert!(
        tmp.path().join("single.eml.clicked").is_file(),
        "single .eml file must record .clicked marker in parent directory"
    );

    // Multi-form consent page with leading logout form, disabled/unchecked inputs, and
    // deny/allow submit `<button>` elements.
    let multi_form_html = r#"
        <html><body>
        <form method="POST" action="/logout">
          <input type="hidden" name="logout" value="1">
        </form>
        <form method="POST" action="approve">
          <input type="hidden" name="csrf" value="c_123">
          <input type="hidden" name="ignored_disabled" value="bad" disabled>
          <input type="checkbox" name="opt_in" value="yes" checked>
          <input type="checkbox" name="unselected_box" value="no">
          <button type="submit" name="decision" value="deny">Deny</button>
          <button type="submit" name="decision" value="allow">Allow access</button>
        </form>
        </body></html>
    "#;
    let (method, action, body) = parse_html_approval_action(
        multi_form_html,
        "https://claude.ai/oauth/authorize?client_id=abc&state=st_root",
    )
    .expect("parse multi-form approval page");
    assert_eq!(method, "POST");
    assert_eq!(action, "https://claude.ai/oauth/approve");
    assert_eq!(
        body.as_deref(),
        Some("csrf=c_123&opt_in=yes&decision=allow")
    );

    // Root loopback callback URL without trailing slash (`http://127.0.0.1:9123`).
    let authorize_root = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123&state=st_root";
    let responses = [
        "HTTP/1.1 200 OK\r\n\r\nCheck your email",
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9123?code=root_code&state=st_root\r\n\r\n",
        "HTTP/1.1 200 OK\r\n\r\nOK",
    ];
    let mut iter = responses.iter();
    let mut seen = Vec::new();
    authorize_using(
        authorize_root,
        Some("sol-m27@proton.me"),
        |_| Ok("https://claude.ai/magic?token=root_magic".to_owned()),
        &mut |_m, u, cookies, _b| {
            seen.push(u.to_owned());
            parse_raw_http_response(iter.next().expect("response"), cookies).ok_or("bad response")
        },
    )
    .expect("root loopback callback without slash must succeed");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("http://127.0.0.1:9123?code=root_code&state=st_root")
    );
}

#[test]
fn approval_attributes_ignore_names_inside_quoted_values() {
    let html = r#"
      <form title='action=/logout method=GET' METHOD = 'POST' ACTION = 'approve'>
        <input type=hidden title='name=forged value=bad' name = csrf value = 'c&amp;1'>
        <input type=checkbox name=extra value=bad title="checked by administrator">
        <input type=checkbox name=opt_in value=yes title="disabled until tomorrow" CHECKED = false>
        <input type=radio name=extra_radio value=bad title='checked by administrator'>
        <input type=radio name=chosen value=yes checked>
        <input type=hidden name=enabled value=ok title='Préfixe disabled until tomorrow'>
        <input type=hidden name=disabled_field value=bad DISABLED=false>
        <input type=hidden data-name=wrong name=first value=one value=two>
        <button name=decision value=allow title="disabled until tomorrow">Allow</button>
      </form>
    "#;
    let base = "https://claude.ai/oauth/authorize";
    let expected_body = "csrf=c%261&opt_in=yes&chosen=yes&enabled=ok&first=one&decision=allow";
    assert_eq!(
        parse_html_approval_action(html, base),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            Some(expected_body.to_owned()),
        ))
    );
    for misleading in [
        r#"<input title="checked by administrator" type=checkbox name=extra value=bad>"#,
        r#"<input title='checked by administrator' type=radio name=extra value=bad>"#,
        r#"<input title="&quot; checked disabled name=forged" type=checkbox name=extra value=bad>"#,
    ] {
        let form = format!(
            "<form method=POST action=/approve>{misleading}<input name=csrf value=kept></form>"
        );
        assert_eq!(
            parse_html_approval_action(&form, base)
                .unwrap()
                .2
                .as_deref(),
            Some("csrf=kept")
        );
    }
    assert!(
        parse_html_approval_action(
            r#"<a title="href=https://example.test/authorize">Allow</a>"#,
            base
        )
        .is_none()
    );
    assert_eq!(
        parse_html_approval_action(
            r#"<a title='href=https://example.test/authorize' HREF = '/approve?ok=1'>Allow</a>"#,
            base
        ),
        Some((
            "GET".to_owned(),
            "https://claude.ai/approve?ok=1".to_owned(),
            None
        ))
    );

    // The actual headless HTTP flow sends only the selected successful controls.
    let authorize =
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123&state=attr";
    let replies = [
        "HTTP/1.1 200 OK\r\n\r\nCheck your email".to_owned(),
        format!("HTTP/1.1 200 OK\r\n\r\n{html}"),
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9123?code=ok&state=attr\r\n\r\n"
            .to_owned(),
        "HTTP/1.1 200 OK\r\n\r\nOK".to_owned(),
    ];
    let mut replies = replies.iter();
    let mut posts = Vec::new();
    authorize_using(
        authorize,
        Some("owner@example.test"),
        |_| Ok("https://claude.ai/oauth/magic?token=attributes".to_owned()),
        &mut |method, url, cookies, body| {
            if method == "POST" {
                posts.push((url.to_owned(), body.map(str::to_owned)));
            }
            parse_raw_http_response(replies.next().expect("response"), cookies).ok_or("response")
        },
    )
    .expect("quoted attribute text cannot change the approval request");
    assert_eq!(
        posts,
        vec![(
            "https://claude.ai/oauth/approve".to_owned(),
            Some(expected_body.to_owned())
        )]
    );
    assert!(replies.next().is_none());
}

#[test]
fn html_comments_and_gt_inside_quoted_attributes_are_handled_cleanly() {
    use super::super::headless_oauth::extract_otp_from_email;

    let base = "https://claude.ai/oauth/authorize";
    let html = r#"
      <!--
        <form method="POST" action="/oauth/approve">
          <input type="hidden" name="csrf" value="commented-out">
        </form>
        <a href="https://claude.ai/oauth/approve?commented=1">Old approve</a>
      -->
      <form data-rule="1 > 0" method="POST" action="/oauth/approve">
        <!-- <input type="hidden" name="csrf" value="stale"> -->
        <input title="step 1 > step 2" type="hidden" name="csrf" value="live">
        <button data-label="go > now" type="submit" name="decision" value="allow">Allow</button>
      </form>
    "#;
    assert_eq!(
        parse_html_approval_action(html, base),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            Some("csrf=live&decision=allow".to_owned()),
        ))
    );

    let anchor_html = r#"
      <!-- <a href="/oauth/approve?bad=1">Bad</a> -->
      <a data-hint="Click > Continue" href="/oauth/approve?good=1">Approve</a>
    "#;
    assert_eq!(
        parse_html_approval_action(anchor_html, base),
        Some((
            "GET".to_owned(),
            "https://claude.ai/oauth/approve?good=1".to_owned(),
            None,
        ))
    );

    let magic_email = concat!(
        "To: sol-m27@example.test\r\n\r\n",
        "<!-- https://claude.ai/magic-link?token=commented-out -->\r\n",
        "<!-- <iframe srcdoc=\"<a href='https://claude.ai/magic-link?token=commented-iframe'>Old</a>\"></iframe> -->\r\n",
        "<a href=\"https://claude.ai/magic-link?token=live-token\">Sign in</a>\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(magic_email, Some("sol-m27@example.test")).as_deref(),
        Some("https://claude.ai/magic-link?token=live-token")
    );

    let otp_email = concat!(
        "To: sol-m27@example.test\r\n\r\n",
        "<!-- if x > 0: verification code is 111111 -->\r\n",
        "<div data-expr=\"x > 0 && code == 222222\">Your verification code is 654321</div>\r\n"
    );
    assert_eq!(
        extract_otp_from_email(otp_email, Some("sol-m27@example.test")).as_deref(),
        Some("654321")
    );
}

#[test]
fn http_103_early_hints_307_redirects_and_subaddressed_lookup_work() {
    use super::super::accounts_sync::parse_accounting_csv;
    use super::super::headless_oauth::{MailboxConfig, derive_account_label_from_email};

    let mut cookies = BTreeMap::from([
        ("pre_auth".to_owned(), "temp".to_owned()),
        ("legacy".to_owned(), "old".to_owned()),
    ]);
    let raw = concat!(
        "HTTP/2 103 Early Hints\r\nLink: </app.css>; rel=preload\r\n\r\n",
        "HTTP/2 200 OK\r\n",
        "Set-Cookie: pre_auth=deleted; Max-Age=0; Path=/\r\n",
        "Set-Cookie: legacy=; Path=/\r\n",
        "Set-Cookie: session=active; Path=/; HttpOnly\r\n\r\n",
        "Check your email"
    );
    let resp = parse_raw_http_response(raw, &mut cookies).expect("skip 103 Early Hints");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "Check your email");
    assert_eq!(cookies.get("session").map(String::as_str), Some("active"));
    assert!(!cookies.contains_key("pre_auth"));
    assert!(!cookies.contains_key("legacy"));

    let authorize =
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123&state=s307";
    let mut requests = Vec::new();
    authorize_using(
        authorize,
        Some("owner@example.test"),
        |_| Ok("https://claude.ai/oauth/magic?token=t307".to_owned()),
        &mut |method, url, cookies, body| {
            requests.push((method.to_owned(), url.to_owned(), body.map(str::to_owned)));
            let raw_resp = match requests.len() {
                1 => "HTTP/1.1 200 OK\r\n\r\nCheck your email",
                2 => concat!(
                    "HTTP/1.1 200 OK\r\n\r\n",
                    "<form method=\"POST\" action=\"/oauth/approve\">",
                    "<input type=\"hidden\" name=\"csrf\" value=\"k1\">",
                    "<button type=\"submit\" name=\"decision\" value=\"allow\">Allow</button>",
                    "</form>"
                ),
                3 => {
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: https://claude.ai/oauth/approve-v2\r\n\r\n"
                }
                4 => {
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9123?code=ok&state=s307\r\n\r\n"
                }
                5 => "HTTP/1.1 200 OK\r\n\r\nOK",
                _ => unreachable!("unexpected HTTP request"),
            };
            parse_raw_http_response(raw_resp, cookies).ok_or("response")
        },
    )
    .expect("307 preserves POST method and body");
    assert_eq!(
        requests[2],
        (
            "POST".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            Some("csrf=k1&decision=allow".to_owned()),
        )
    );
    assert_eq!(
        requests[3],
        (
            "POST".to_owned(),
            "https://claude.ai/oauth/approve-v2".to_owned(),
            Some("csrf=k1&decision=allow".to_owned()),
        )
    );

    assert_eq!(
        derive_account_label_from_email("sol_m27_2+claude@proton.me").as_deref(),
        Some("m27-2")
    );
    assert_eq!(
        derive_account_label_from_email("sol_m27+claude@proton.me").as_deref(),
        Some("m27")
    );
    let cfg = MailboxConfig::from_toml_str(
        "[passwords]\n\"sol_m27_2@proton.me\" = \"base-pw\"\nm27_3 = \"slot3-pw\"\n",
        None,
    )
    .expect("parse mailbox config");
    assert_eq!(
        cfg.password_for(Some("sol_m27_2+claude@proton.me"))
            .as_deref(),
        Some("base-pw")
    );
    assert_eq!(cfg.password_for(Some("m27-3")).as_deref(), Some("slot3-pw"));

    let roster = parse_accounting_csv(
        "email,computer,claude\nsol_m27_2+claude@proton.me,m27,1\n",
        "m27",
    )
    .expect("parse accounting csv");
    assert_eq!(roster.accounts.len(), 1);
    assert_eq!(roster.accounts[0].label, "m27-2");
}

#[test]
fn approval_formaction_meta_refresh_and_multi_link_fallback() {
    let base = "https://claude.ai/oauth/consent?req=1";

    // Button inner text ("Deny" vs "Allow") disambiguates numeric/opaque values.
    let button_text_html = r#"
      <form method="post" action="/oauth/decision">
        <input type="hidden" name="csrf" value="tok_1">
        <button type="submit" name="decision" value="0"><span>Deny</span></button>
        <button type="submit" name="decision" value="1"><span>Allow</span></button>
      </form>
    "#;
    assert_eq!(
        parse_html_approval_action(button_text_html, base),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/decision".to_owned(),
            Some("csrf=tok_1&decision=1".to_owned()),
        ))
    );

    // HTML5 `formaction` and `formmethod` on the positive submit button override the form tag.
    let formaction_html = r#"
      <form method="get" action="/oauth/default">
        <input type="hidden" name="state" value="st_fa">
        <button type="submit" formaction="/oauth/deny" formmethod="post">Deny</button>
        <button type="submit" formaction="approve" formmethod="post">Authorize</button>
      </form>
    "#;
    assert_eq!(
        parse_html_approval_action(formaction_html, base),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            Some("state=st_fa".to_owned()),
        ))
    );

    // A standalone form whose only submit controls are negative is skipped.
    let cancel_first_html = r#"
      <form method="post" action="/oauth/session">
        <button type="submit" name="op" value="cancel">Cancel</button>
      </form>
      <form method="post" action="/oauth/session">
        <input type="hidden" name="csrf" value="c_ok">
        <button type="submit" name="op" value="proceed">Submit</button>
      </form>
    "#;
    assert_eq!(
        parse_html_approval_action(cancel_first_html, base),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/session".to_owned(),
            Some("csrf=c_ok&op=proceed".to_owned()),
        ))
    );

    // Negative `<a>` links with `authorize` in their URL are skipped in favor of positive ones.
    let anchor_html = r#"
      <a href="https://claude.ai/oauth/authorize?decision=deny">Deny</a>
      <a href="/oauth/step2?tok=abc">Allow access</a>
    "#;
    assert_eq!(
        parse_html_approval_action(anchor_html, base),
        Some((
            "GET".to_owned(),
            "https://claude.ai/oauth/step2?tok=abc".to_owned(),
            None,
        ))
    );

    // `<meta http-equiv="refresh">` landing pages resolve their redirect URL.
    let meta_html = r#"<html><head><meta http-equiv="Refresh" content="0; url='/oauth/consent?step=2'"></head></html>"#;
    assert_eq!(
        parse_html_approval_action(meta_html, base),
        Some((
            "GET".to_owned(),
            "https://claude.ai/oauth/consent?step=2".to_owned(),
            None,
        ))
    );

    // JSON redirect_url and nested data.approve_url responses.
    assert_eq!(
        parse_html_approval_action(r#"{"redirect_url":"/oauth/callback?code=c1"}"#, base),
        Some((
            "GET".to_owned(),
            "https://claude.ai/oauth/callback?code=c1".to_owned(),
            None,
        ))
    );
    assert_eq!(
        parse_html_approval_action(r#"{"data":{"approve_url":"approve"}}"#, base),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            None,
        ))
    );

    // HTML OTP email with standard header/footer/font links must extract the 6-digit OTP
    // rather than treating template links as magic links.
    let html_otp_email = concat!(
        "From: Claude <no-reply@anthropic.com>\r\n",
        "To: <sol-m27@proton.me>\r\n",
        "Subject: Your Claude verification code\r\n\r\n",
        "<html><head>",
        "<link href=\"https://fonts.googleapis.com/css2?family=Inter\" rel=\"stylesheet\">",
        "</head><body>",
        "<a href=\"https://claude.ai\">Claude</a>",
        "<a href=\"https://claude.ai/login\">Log in</a>",
        "<p>Your verification code is <strong>482910</strong></p>",
        "<a href=\"https://www.anthropic.com/privacy\">Privacy Policy</a>",
        "<a href=\"https://support.anthropic.com/en/articles/123456\">Help Center</a>",
        "</body></html>\r\n"
    );
    assert!(
        extract_magic_link_from_email(html_otp_email, Some("sol-m27@proton.me")).is_none(),
        "static template/footer links in an OTP email must not be mistaken for magic links"
    );
    assert_eq!(
        super::super::headless_oauth::extract_all_credentials_from_email(
            html_otp_email,
            Some("sol-m27@proton.me")
        ),
        vec![super::super::headless_oauth::MailboxCredential::Otp(
            "482910".to_owned()
        )]
    );

    // JSON email with a REST API message self-link (`"url"`) and an HTML magic link or OTP.
    let json_with_self_url_and_magic = r#"{
        "id": "msg_1",
        "url": "https://mail.example.test/api/v1/messages/msg_1",
        "to": "sol-m27@proton.me",
        "html": "<p><a href=\"https://claude.ai/magic?token=real_tok\">Sign in</a></p>"
    }"#;
    assert_eq!(
        extract_magic_link_from_email(json_with_self_url_and_magic, Some("sol-m27@proton.me"))
            .as_deref(),
        Some("https://claude.ai/magic?token=real_tok")
    );
    let json_with_self_url_and_otp = r#"{
        "id": "msg_2",
        "url": "https://mail.example.test/api/v1/messages/msg_2",
        "to": "sol-m27@proton.me",
        "text": "Your verification code is 654321"
    }"#;
    assert_eq!(
        super::super::headless_oauth::extract_all_credentials_from_email(
            json_with_self_url_and_otp,
            Some("sol-m27@proton.me")
        ),
        vec![super::super::headless_oauth::MailboxCredential::Otp(
            "654321".to_owned()
        )]
    );

    // `authorize_using_with_page` moves to the email's next candidate link when one has no
    // approval action.
    let authorize_url = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9124%2Fcb&state=st_multi";
    let mut candidate_links = [
        "https://claude.ai/login?return_to=home".to_owned(),
        "https://claude.ai/magic?token=good_second".to_owned(),
    ]
    .into_iter();
    let http_replies = [
        "HTTP/1.1 200 OK\r\n\r\nCheck your email",
        "HTTP/1.1 200 OK\r\n\r\n<html><body>Static info page without form</body></html>",
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9124/cb?code=multi_ok&state=st_multi\r\n\r\n",
        "HTTP/1.1 200 OK\r\n\r\nOK",
    ];
    let mut reply_iter = http_replies.iter();
    super::authorize_using_with_page(
        authorize_url,
        Some("sol-m27@proton.me"),
        |_, _| candidate_links.next().ok_or("no more links"),
        &mut |_m, _u, cookies, _b| {
            parse_raw_http_response(reply_iter.next().expect("http reply"), cookies)
                .ok_or("bad reply")
        },
    )
    .expect("second candidate link in email should complete OAuth flow");
}

#[test]
fn webmail_user_for_and_all_webmail_ui_urls_route_cleanly() {
    use super::super::headless_oauth::{MailboxConfig, oauth_step_js};

    let default_cfg = MailboxConfig::default();
    assert_eq!(
        default_cfg
            .webmail_user_for(Some("sol_m27_2+claude@proton.me"))
            .as_deref(),
        Some("sol_m27_2@proton.me")
    );
    let explicit_cfg = MailboxConfig {
        user: Some("master@proton.me".to_owned()),
        ..MailboxConfig::default()
    };
    assert_eq!(
        explicit_cfg
            .webmail_user_for(Some("sol_m27_2+claude@proton.me"))
            .as_deref(),
        Some("master@proton.me")
    );

    for ui_url in [
        "https://mail.proton.me/u/0/inbox",
        "https://app.fastmail.com/mail/Inbox",
        "https://mail.zoho.com/zm/",
        "https://outlook.live.com/mail/0/inbox",
        "https://purelymail.com/webmail/",
        "https://webmail.migadu.com/",
        "https://app.tuta.com/mail",
    ] {
        let cfg = MailboxConfig::from_toml_str(&format!("url = \"{ui_url}\"\n"), None)
            .expect("parse webmail ui url");
        assert_eq!(cfg.webmail_url.as_deref(), Some(ui_url));
        assert!(cfg.http_url.is_none());
    }

    let step_js = oauth_step_js(Some("sol_m27_2+claude@proton.me"), None, false, false);
    assert!(step_js.contains("const canonEmail ="));
    assert!(step_js.contains("canonEmail(e) === targetCanon"));
}

#[test]
fn a_pin_admits_only_its_own_vendors_pages() {
    use crate::ai::connector::{KIMI_SIGNIN, Region};
    let kimi = Pin::Hosts(
        KIMI_SIGNIN
            .route(None)
            .expect("kimi's route")
            .hosts
            .of(Some(Region::Global))
            .expect("global hosts"),
    );
    let claude_authorize = "https://claude.ai/oauth/authorize?code=true&client_id=x\
        &redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state=s";
    assert!(CLAUDE_PIN.admits(claude_authorize));
    assert!(
        !kimi.admits(claude_authorize),
        "kimi's pin never admits claude.ai"
    );
    assert!(MailboxFlow::pinned(claude_authorize, kimi).is_err());
    let kimi_page = "https://auth.kimi.ai/oauth/authorize?redirect_uri=\
        http%3A%2F%2F127.0.0.1%3A7777%2Fcb&state=s";
    assert!(kimi.admits(kimi_page));
    assert!(
        !CLAUDE_PIN.admits(kimi_page),
        "Claude Code's pin never admits auth.kimi.ai"
    );
    assert!(MailboxFlow::new(kimi_page).is_err());
    let flow = MailboxFlow::pinned(kimi_page, kimi).expect("a kimi flow");
    assert!(flow.admits_provider_url("https://www.kimi.ai/code/authorize_device"));
    assert!(!flow.admits_provider_url("https://auth.kimi.com/code"));
    assert!(!flow.admits_provider_url("https://claude.ai/oauth/authorize"));
    assert!(flow.is_callback("http://127.0.0.1:7777/cb?code=c&state=s"));
}

#[test]
fn an_account_run_names_its_cli_and_label_before_its_home_does() {
    use std::ffi::OsString;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let store = tmp.path().join("accounts");
    let login = store.join("logins/sol+2");
    std::fs::create_dir_all(&login).expect("mkdir");
    let login_text = login.to_string_lossy().into_owned();
    let with = |pairs: Vec<(&'static str, String)>| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    };
    let claude = with(vec![(super::super::accounts::HOME_ENV, login_text.clone())]);
    assert_eq!(account_label_of(&claude).as_deref(), Some("sol"));
    assert_eq!(store_dir_of(&claude, None), Some(store.clone()));
    let named = with(vec![
        (super::super::accounts::HOME_ENV, login_text.clone()),
        (ACCOUNT_LABEL_ENV, "kimi-one".to_owned()),
        (ACCOUNT_CLI_ENV, "kimi".to_owned()),
    ]);
    assert_eq!(account_label_of(&named).as_deref(), Some("kimi-one"));
    assert_eq!(
        store_dir_of(&named, None),
        None,
        "another CLI's run never reads Claude Code's home for its store"
    );
    let other = with(vec![
        (super::super::accounts::HOME_ENV, login_text),
        (ACCOUNT_CLI_ENV, "grok".to_owned()),
    ]);
    assert_eq!(account_label_of(&other), None);
    let opener = store.join("bin").join(OPENER);
    assert_eq!(store_dir_of(&other, Some(&opener)), Some(store));
}

#[test]
fn hex_3d_tokens_multipart_qp_dot_segments_and_subaddressed_profiles_work() {
    use super::super::headless_oauth::{
        browser_profile_dir, extract_otp_from_email, has_browser_profile,
        resolve_browser_profile_dir,
    };

    // Non-QP email whose hex query token starts with `3d` / `3D` must not be corrupted by
    // false-positive Quoted-Printable decoding.
    let raw_hex_token_eml = concat!(
        "From: Claude <no-reply@claude.ai>\r\n",
        "To: <sol-m27@proton.me>\r\n",
        "Subject: Sign in to Claude\r\n\r\n",
        "https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(raw_hex_token_eml, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99"),
        "hex query token starting with 3d must not trigger QP decoding of =3d and =41"
    );

    // Multipart email mixing folded Quoted-Printable and 8bit MIME parts decodes QP per-part
    // without corrupting the 8bit part.
    let multipart_mixed_eml = concat!(
        "From: Claude <no-reply@claude.ai>\r\n",
        "To: <sol-m27@proton.me>\r\n",
        "Content-Type: multipart/alternative; boundary=\"b_qp\"\r\n\r\n",
        "--b_qp\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding:\r\n",
        " quoted-printable\r\n\r\n",
        "Sign in: https://claude.ai/magic-link?token=3Dqp_folded_ok&state=3Ds1\r\n",
        "--b_qp\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "Content-Transfer-Encoding: 8bit\r\n\r\n",
        "<a href=\"https://claude.ai/magic-link?token=3d8f9a1b2c&amp;state=41bc99\">Sign in</a>\r\n",
        "--b_qp--\r\n"
    );
    let links = extract_magic_links_from_email(multipart_mixed_eml, Some("sol-m27@proton.me"));
    assert_eq!(
        links,
        vec![
            "https://claude.ai/magic-link?token=qp_folded_ok&state=s1".to_owned(),
            "https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99".to_owned(),
        ]
    );

    // Child spans with `>` in quoted attributes inside `<button>` must not hide negative text,
    // and relative `../` form actions normalize dot segments per RFC 3986.
    let consent_html = r#"
      <form method="POST" action="../approve">
        <input type="hidden" name="csrf" value="c1">
        <button type="submit" name="decision" value="0"><span data-icon="1 > 0">Deny</span></button>
        <button type="submit" name="decision" value="1"><span data-icon="1 > 0">Allow</span></button>
      </form>
    "#;
    assert_eq!(
        parse_html_approval_action(
            consent_html,
            "https://claude.ai/oauth/v2/authorize?state=s1"
        ),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            Some("csrf=c1&decision=1".to_owned()),
        ))
    );

    // `<meta http-equiv="refresh">` handles `>` inside quoted attributes, spaces around `=`,
    // and skips negative refresh URLs.
    let meta_html = r#"
      <html><head>
        <meta data-rule="1 > 0" http-equiv="refresh" content="0; url = '/oauth/logout'">
        <meta data-rule="2 > 1" http-equiv="refresh" content="0; URL = '../consent?step=2'">
      </head></html>
    "#;
    assert_eq!(
        parse_html_approval_action(meta_html, "https://claude.ai/oauth/v2/landing"),
        Some((
            "GET".to_owned(),
            "https://claude.ai/oauth/consent?step=2".to_owned(),
            None,
        ))
    );

    // JSON OTP fields accept hyphenated `"482-910"` and 6-digit integer `654321`.
    assert_eq!(
        extract_otp_from_email(
            r#"{"to":"sol-m27@proton.me","otp":"482-910"}"#,
            Some("sol-m27@proton.me")
        )
        .as_deref(),
        Some("482910")
    );
    assert_eq!(
        extract_otp_from_email(
            r#"{"to":"sol-m27@proton.me","code":200,"verification_code":654321}"#,
            Some("sol-m27@proton.me")
        )
        .as_deref(),
        Some("654321")
    );

    // Subaddressed emails reuse existing base-email or derived-label browser profile directories.
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let store = tmp.path().join("accounts");
    let base_dir = browser_profile_dir(&store, Some("sol_m27_2@proton.me"));
    assert_eq!(
        resolve_browser_profile_dir(&store, None, Some("sol_m27_2+claude@proton.me")),
        base_dir,
        "fresh subaddressed email defaults to base email profile directory"
    );
    let label_dir = browser_profile_dir(&store, Some("m27-2"));
    std::fs::create_dir_all(&label_dir).unwrap();
    assert!(has_browser_profile(
        &store,
        "m27_2",
        Some("sol_m27_2+claude@proton.me")
    ));
    assert_eq!(
        resolve_browser_profile_dir(&store, Some("m27_2"), Some("sol_m27_2+claude@proton.me")),
        label_dir
    );
    std::fs::create_dir_all(&base_dir).unwrap();
    assert_eq!(
        resolve_browser_profile_dir(&store, Some("m27_2"), Some("sol_m27_2+claude@proton.me")),
        base_dir
    );
}

#[test]
fn oauth_dot_segments_preserve_empty_path_segments_and_query_data() {
    let base = "https://claude.ai/base/page";
    for (target, expected_path) in [
        ("/a//b/../c", "/a//c"),
        ("/a//../b", "/a/b"),
        ("/a//./b", "/a//b"),
        ("/a//.", "/a//"),
        ("/a//..", "/a/"),
        ("/a///b/../../c", "/a//c"),
        ("/../../approve", "/approve"),
        ("/a/./", "/a/"),
        ("/a/../", "/"),
        ("/a//b", "/a//b"),
        ("/a/.well-known//b", "/a/.well-known//b"),
        ("/a/%2e//b", "/a/%2e//b"),
        ("/a//λ/../c", "/a//c"),
        (
            "/a//b/../c?token=x//../y&code=%2e%2e",
            "/a//c?token=x//../y&code=%2e%2e",
        ),
    ] {
        assert_eq!(
            resolve_relative_url(base, target),
            format!("https://claude.ai{expected_path}")
        );
    }
    for (page, expected) in [
        (
            r#"<form method="POST" action="../approve?token=s//../q"><button>Allow</button></form>"#,
            (
                "POST",
                "https://claude.ai/a//approve?token=s//../q",
                Some(""),
            ),
        ),
        (
            r#"{"approval_url":"/a//../approve?token=s//../q"}"#,
            ("POST", "https://claude.ai/a/approve?token=s//../q", None),
        ),
        (
            r#"<meta http-equiv="refresh" content="0; url=/a//b/../confirm">"#,
            ("GET", "https://claude.ai/a//confirm", None),
        ),
        (
            r#"<a href="/a//./allow">Allow</a>"#,
            ("GET", "https://claude.ai/a//allow", None),
        ),
    ] {
        assert_eq!(
            parse_html_approval_action(page, "https://claude.ai/a//b/current"),
            Some((
                expected.0.to_owned(),
                expected.1.to_owned(),
                expected.2.map(str::to_owned)
            ))
        );
    }

    let authorize = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123%2Fcallback&state=dot";
    let flow = MailboxFlow::new(authorize).unwrap();
    let mut requests = Vec::new();
    let outcome = follow_until_page_or_callback(
        &flow,
        &mut |method, url, _, body| {
            assert_eq!((method, body), ("POST", Some("token=private")));
            requests.push(url.to_owned());
            match url {
                "https://claude.ai/a//b/start" => Ok(HttpResponse {
                    status: 307,
                    location: Some("../approve?state=dot".to_owned()),
                    body: String::new(),
                }),
                "https://claude.ai/a//approve?state=dot" => Ok(HttpResponse {
                    status: 200,
                    location: None,
                    body: "done".to_owned(),
                }),
                other => panic!("wrong OAuth endpoint: {other}"),
            }
        },
        "POST",
        "https://claude.ai/a//b/start",
        &mut BTreeMap::new(),
        Some("token=private"),
    )
    .unwrap();
    let StepOutcome::Page { url, body } = outcome else {
        panic!("the injected endpoint is a page");
    };
    assert_eq!(url, "https://claude.ai/a//approve?state=dot");
    assert_eq!(body, "done");
    assert_eq!(
        requests,
        [
            "https://claude.ai/a//b/start",
            "https://claude.ai/a//approve?state=dot"
        ]
    );
}

#[test]
fn multipart_mime_boundary_and_consent_negation_hardening() {
    use crate::harness::headless_oauth::extract_otp_from_email;

    // 1. Multipart email with LF top-level headers (`\n\n`), CRLF MIME part headers (`\r\n\r\n`),
    // an 8bit part containing hex `=3d` query values (`?token=3d8f9a1b2c`), and a quoted-printable
    // part containing `-->` at line start and `---` dividers before the magic link and OTP.
    let mixed_multipart = concat!(
        "From: Anthropic <no-reply@anthropic.com>\n",
        "To: sol-m27@proton.me\n",
        "Subject: Sign in to Claude\n",
        "Content-Type: multipart/alternative; boundary=\"part-bound\"\n",
        "\n",
        "--part-bound\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: 8bit\r\n",
        "\r\n",
        "----------------\r\n",
        "Use https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99\r\n",
        "-- \r\n",
        "Anthropic Team\r\n",
        "--part-bound\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n",
        "\r\n",
        "<!-- header comment\r\n",
        "-->\r\n",
        "<p>Or click <a href=3D\"https://claude.ai/magic-link?token=3Dsecond-qp&=\r\n",
        "state=3D99\">Sign in</a></p>\r\n",
        "<p>Your verification code is: 739=3204</p>\r\n",
        "--part-bound--\r\n"
    );
    let links = extract_magic_links_from_email(mixed_multipart, Some("sol-m27@proton.me"));
    assert_eq!(
        links,
        vec![
            "https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99".to_owned(),
            "https://claude.ai/magic-link?token=second-qp&state=99".to_owned(),
        ],
        "8bit hex `=3d` is preserved intact while quoted-printable part after `-->` is decoded"
    );
    assert_eq!(
        extract_otp_from_email(mixed_multipart, Some("sol-m27@proton.me")).as_deref(),
        Some("739204"),
        "OTP after `-->` in quoted-printable part with LF top-level header is decoded"
    );

    // 2. JSON-wrapped raw MIME with per-part quoted-printable and `-->` / `---` lines.
    let json_multipart = r#"{"to":"sol-m27@proton.me","body":"--b1\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<!--\r\n-->\r\n<a href=3D\"https://claude.ai/magic-link?token=3Djson-qp-tok&state=3D77\">Log in</a>\r\n--b1--"}"#;
    assert_eq!(
        extract_magic_link_from_email(json_multipart, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=json-qp-tok&state=77")
    );

    // 3. Multi-word negative consent buttons (`"Do not allow"`, `"Don't allow"`) are rejected,
    // and forms with an explicit positive submit button beat earlier forms whose surrounding HTML
    // merely mentions `"allow"` or `"continue"`.
    let html_multi_form = r#"
        <html><body>
          <form method="POST" action="/account/switch">
            <p>Allow switching accounts to continue with another user</p>
            <input type="hidden" name="switch" value="1">
            <button type="submit">Switch account</button>
          </form>
          <form method="POST" action="/oauth/decision">
            <input type="hidden" name="req" value="req-99">
            <button type="submit" name="decision" value="do_not_allow">Do not allow</button>
            <button type="submit" name="decision" value="dont_allow">Don't allow</button>
            <button type="submit" name="decision" value="allow_access">Allow access</button>
          </form>
        </body></html>
    "#;
    assert_eq!(
        parse_html_approval_action(html_multi_form, "https://claude.ai/oauth/authorize"),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/decision".to_owned(),
            Some("req=req-99&decision=allow_access".to_owned()),
        ))
    );

    // 4. Form with only `"Do not allow"` / `"Don't allow"` buttons is rejected completely.
    let html_only_neg = r#"
        <form method="POST" action="/oauth/consent">
          <button type="submit" name="act" value="no">Do not allow</button>
          <button type="submit" name="act" value="no2">Don't authorize</button>
        </form>
    "#;
    assert_eq!(
        parse_html_approval_action(html_only_neg, "https://claude.ai/oauth/authorize"),
        None
    );

    // 5. `parse_raw_http_response` splits LF headers (`\n\n`) before CRLF body (`\r\n\r\n`).
    let mut cookies = BTreeMap::new();
    let raw_http = "HTTP/1.1 200 OK\nSet-Cookie: sid=abc; Path=/\n\nline1\r\n\r\nline2";
    let resp = super::parse_raw_http_response(raw_http, &mut cookies).expect("http response");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "line1\r\n\r\nline2");
    assert_eq!(cookies.get("sid").map(String::as_str), Some("abc"));

    // Declared boundaries, including all-hyphen names, end only their own encoded part.
    // Quoted unrelated parameters and folded headers must not hide the real boundary.
    for boundary in ["-", "---", "quoted:boundary"] {
        let raw = format!(
            "To: fixture@example.invalid\r\n\
             Content-Type: multipart/mixed; name=\"a; boundary=wrong\";\r\n\
             \tboundary=\"{boundary}\"\r\n\r\n\
             --{boundary} \t\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\r\n\
             <!--\r\n-->\r\n--not-a-boundary\r\n\
             https://claude.ai/magic-link?token=3Dqp-tok&state=3D77\r\n\
             --{boundary}\r\nContent-Transfer-Encoding: 8bit\r\n\r\n\
             https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99\r\n\
             --{boundary}--\r\n"
        );
        assert_eq!(
            extract_magic_links_from_email(&raw, Some("fixture@example.invalid")),
            [
                "https://claude.ai/magic-link?token=qp-tok&state=77",
                "https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99",
            ],
            "{boundary}: QP decodes once; the later 8bit token stays exact, with no corrupted duplicate"
        );
    }

    let mut base64_link = "https://claude.ai/magic-link?token=base64".to_owned();
    while !base64_link.len().is_multiple_of(3) {
        base64_link.push('x');
    }
    let encoded = super::encode_base64(base64_link.as_bytes());
    let raw = format!(
        "Content-Type: multipart/mixed; boundary=\"---\"\r\n\r\n\
         -----\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n-------\r\n"
    );
    assert_eq!(extract_magic_links_from_email(&raw, None), [base64_link]);

    let nested = concat!(
        "To: fixture@example.invalid\r\n",
        "Content-Type: multipart/mixed; boundary=outer\r\n\r\n",
        "--outer\r\nContent-Type: multipart/alternative;\r\n\tboundary=\"---\"\r\n\r\n",
        "-----\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nordinary text\r\n",
        "-----\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
        "https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99\r\n",
        "-------\r\n--outer\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n",
        "-----\r\nYour verification code is: 739=3204\r\n--outer--\r\n",
    );
    assert_eq!(
        extract_magic_links_from_email(nested, Some("fixture@example.invalid")),
        ["https://claude.ai/magic-link?token=3d8f9a1b2c&state=41bc99"]
    );
    assert_eq!(
        extract_otp_from_email(nested, Some("fixture@example.invalid")).as_deref(),
        Some("739204"),
        "a closed inner boundary is ordinary text in a later outer part"
    );

    let mut too_deep = String::new();
    for depth in 0..17 {
        too_deep.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=n{depth}\r\n\r\n--n{depth}\r\n"
        ));
    }
    too_deep.push_str("Content-Transfer-Encoding: quoted-printable\r\n\r\nkeep=3Dexact");
    assert_eq!(super::decode_qp_mime_parts(&too_deep), too_deep);
    assert!(super::decode_mime_base64_parts(&too_deep).is_empty());
}

#[test]
fn css_custom_props_angle_bracket_entities_otp_priority_and_get_form_dedup() {
    use crate::harness::headless_oauth::{extract_otp_from_email, extract_otps_from_email};

    // 1. Multipart QP email containing CSS custom properties (`--brand-color: #d97757;`),
    // CLI flags (`--headless=new`), and signature lines (`-- Anthropic Team`) at column 0,
    // plus an angle-bracketed HTML entity fallback link (`&lt;...&#038;...&gt;`).
    let css_and_entity_multipart = concat!(
        "From: Anthropic <no-reply@anthropic.com>\r\n",
        "To: sol-m27@proton.me\r\n",
        "Subject: Sign in to Claude\r\n",
        "Content-Type: multipart/alternative; boundary=\"bound-css\"\r\n\r\n",
        "--bound-css\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
        "<html><head><style>\r\n",
        ":root {\r\n",
        "--brand-color: #d97757;\r\n",
        "--font-sans: Inter, sans-serif;\r\n",
        "}\r\n",
        "</style></head><body>\r\n",
        "<pre>\r\n",
        "--headless=3Dnew\r\n",
        "</pre>\r\n",
        "<p>Copy link: &lt;https://claude.ai/magic-link?token=3Dentity-ok&#038;=\r\n",
        "state=3D42&gt;</p>\r\n",
        "<p>Or enter passcode: 482&#45;910</p>\r\n",
        "-- Anthropic Team\r\n",
        "</body></html>\r\n",
        "--bound-css--\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(css_and_entity_multipart, Some("sol-m27@proton.me"))
            .as_deref(),
        Some("https://claude.ai/magic-link?token=entity-ok&state=42")
    );
    assert_eq!(
        extract_otp_from_email(css_and_entity_multipart, Some("sol-m27@proton.me")).as_deref(),
        Some("482910")
    );

    // 2. Contextual OTP outranks an earlier uncontextualized standalone 6-digit number.
    let header_num_then_otp = concat!(
        "From: Anthropic <no-reply@anthropic.com>\r\n",
        "To: sol-m27@proton.me\r\n",
        "Subject: Claude Account Notification\r\n\r\n",
        "<div>941070</div>\r\n",
        "<p>San Francisco, CA</p>\r\n",
        "<p>Hello,</p>\r\n",
        "<p>Please enter the following verification code in your browser:</p>\r\n",
        "<div>654321</div>\r\n"
    );
    assert_eq!(
        extract_otps_from_email(header_num_then_otp, Some("sol-m27@proton.me")),
        vec!["654321".to_owned(), "941070".to_owned()]
    );

    // 3. An OTP box just above its caption outranks an earlier bare 6-digit number, and a
    // 6-line vertical OTP box survives a 7th single-digit footnote line.
    let hero_above_caption = concat!(
        "From: Anthropic <no-reply@anthropic.com>\r\n",
        "To: sol-m27@proton.me\r\n\r\n",
        "<div>100200</div>\r\n",
        "<p>Welcome back</p>\r\n",
        "<div>839201</div>\r\n",
        "<p>Enter this one-time passcode to sign in to Claude.</p>\r\n"
    );
    assert_eq!(
        extract_otp_from_email(hero_above_caption, Some("sol-m27@proton.me")).as_deref(),
        Some("839201")
    );

    let vertical_with_footer_digit = concat!(
        "From: Anthropic <no-reply@anthropic.com>\r\n",
        "To: sol-m27@proton.me\r\n\r\n",
        "<p>Your verification code:</p>\r\n",
        "<div>3</div><div>1</div><div>4</div><div>1</div><div>5</div><div>9</div>\r\n",
        "<div>7</div>\r\n"
    );
    assert_eq!(
        extract_otp_from_email(vertical_with_footer_digit, Some("sol-m27@proton.me")).as_deref(),
        Some("314159")
    );

    // 4. Icon-only submit buttons with `aria-label`, OAuth `error=access_denied` cancel links,
    // and GET form query-parameter deduplication against `base_url`.
    let icon_buttons_html = r#"
        <form method="POST" action="/oauth/consent">
          <input type="hidden" name="csrf" value="t1">
          <button type="submit" name="decision" value="0" aria-label="Deny access"><svg></svg></button>
          <button type="submit" name="decision" value="1" aria-label="Allow access"><svg></svg></button>
        </form>
    "#;
    assert_eq!(
        parse_html_approval_action(icon_buttons_html, "https://claude.ai/oauth/authorize"),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/consent".to_owned(),
            Some("csrf=t1&decision=1".to_owned()),
        ))
    );

    let cancel_callback_then_approve_link = r#"
        <a href="http://127.0.0.1:9000/callback?error=access_denied&amp;state=st_1">Return to app</a>
        <a href="http://127.0.0.1:9000/callback?code=auth_ok&amp;state=st_1">Continue</a>
    "#;
    assert_eq!(
        parse_html_approval_action(
            cancel_callback_then_approve_link,
            "https://claude.ai/oauth/authorize?state=st_1"
        ),
        Some((
            "GET".to_owned(),
            "http://127.0.0.1:9000/callback?code=auth_ok&state=st_1".to_owned(),
            None,
        ))
    );

    let get_form_with_duplicate_state = r#"
        <form method="GET" action="http://127.0.0.1:9000/callback?state=st_1">
          <input type="hidden" name="state" value="st_1">
          <input type="hidden" name="code" value="code_77">
          <button type="submit">Allow</button>
        </form>
    "#;
    let (m, cb_url, body) = parse_html_approval_action(
        get_form_with_duplicate_state,
        "https://claude.ai/oauth/authorize?state=st_1",
    )
    .expect("GET form approval action");
    assert_eq!(m, "GET");
    assert_eq!(body, None);
    assert_eq!(
        cb_url,
        "http://127.0.0.1:9000/callback?state=st_1&code=code_77"
    );
    let flow = super::MailboxFlow::new(
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcallback&state=st_1",
    )
    .expect("mailbox flow");
    assert!(
        flow.is_callback(&cb_url),
        "deduplicated GET form callback URL must satisfy MailboxFlow::is_callback"
    );
}

#[test]
fn tracked_magic_urls_chunked_http_and_email_input_detection() {
    let safelinks_email = concat!(
        "To: sol-m27@proton.me\r\n",
        "Subject: Sign in to Claude\r\n\r\n",
        "Click here: https://nam04.safelinks.protection.outlook.com/?url=https%3A%2F%2Fclaude.ai%2Fmagic-link%3Ftoken%3Dsafelink_tok%26state%3Ds1&data=05%7C01\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(safelinks_email, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=safelink_tok&state=s1")
    );

    let ppv3_email = concat!(
        "To: sol-m27@proton.me\r\n\r\n",
        "https://urldefense.com/v3/__https://claude.ai/magic-link?token=ppv3_tok&state=s1__;!!abc!def$\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(ppv3_email, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=ppv3_tok&state=s1")
    );

    let ppv2_email = concat!(
        "To: sol-m27@proton.me\r\n\r\n",
        "https://urldefense.proofpoint.com/v2/url?u=https-3A__claude.ai_magic-2Dlink-3Ftoken-3Dppv2-5Ftok-26state-3Ds1&d=DwMFaQ\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(ppv2_email, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=ppv2_tok&state=s1")
    );

    let ses_email = concat!(
        "To: sol-m27@proton.me\r\n\r\n",
        "https://xyz.r.us-east-1.awstrack.me/L0/https:%2F%2Fclaude.ai%2Fmagic-link%3Ftoken%3Dses+tok%26state%3Ds1/1/0100018f\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(ses_email, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=ses+tok&state=s1")
    );

    let mut cookies = std::collections::BTreeMap::new();
    let raw_chunked = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Content-Type: application/json\r\n\r\n",
        "1b;ext=1\r\n",
        "{\"approve_url\":\"/oauth/appr\r\n",
        "10\r\n",
        "ove?ok=chunked\"}\r\n",
        "0\r\n\r\n"
    );
    let parsed = super::parse_raw_http_response(raw_chunked, &mut cookies).expect("chunked resp");
    assert_eq!(parsed.status, 200);
    assert_eq!(
        parsed.body,
        "{\"approve_url\":\"/oauth/approve?ok=chunked\"}"
    );
    assert_eq!(
        parse_html_approval_action(&parsed.body, "https://claude.ai/oauth/authorize"),
        Some((
            "POST".to_owned(),
            "https://claude.ai/oauth/approve?ok=chunked".to_owned(),
            None,
        ))
    );

    let already_dechunked = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Transfer-Encoding: chunked\r\n\r\n",
        "{\"approve_url\":\"/oauth/approve?ok=curl\"}"
    );
    let parsed_curl =
        super::parse_raw_http_response(already_dechunked, &mut cookies).expect("curl resp");
    assert_eq!(
        parsed_curl.body,
        "{\"approve_url\":\"/oauth/approve?ok=curl\"}"
    );

    assert!(!super::page_needs_browser_before_email(
        "<html><body><form action='/approve'><input type='hidden' name='email' value='sol-m27@proton.me'><button type='submit'>Approve</button></form></body></html>"
    ));
    assert!(!super::page_needs_browser_before_email(
        "<html><body><form><input type='email' name='email' disabled></form></body></html>"
    ));
    assert!(super::page_needs_browser_before_email(
        "<html><body><form><input type=email name=email></form></body></html>"
    ));

    let js = super::super::headless_oauth::oauth_step_js(
        Some("sol-m27@proton.me"),
        Some("123456"),
        false,
        false,
    );
    let otp_pos = js.find("otpInputs.length > 0").expect("otpInputs check");
    let email_pos = js
        .find("emailInput && emailVal && !emailDone")
        .expect("emailInput check");
    let approve_form_pos = js.find("const approveForm").expect("approveForm check");
    assert!(
        otp_pos < approve_form_pos && email_pos < approve_form_pos,
        "otpInputs and emailInput must be evaluated before fallback approveForm"
    );
}

#[test]
fn nested_trackers_anchor_ranking_and_mime_otp_isolation() {
    use super::super::headless_oauth::{
        MailboxCredential, extract_credential_from_email, extract_otps_from_email,
    };

    // 1. SafeLinks wrapping AWS SES /L0/ wrapping a Claude magic link.
    let double_wrapped = concat!(
        "To: sol-m27@proton.me\r\n",
        "Subject: Sign in to Claude\r\n\r\n",
        "https://nam04.safelinks.protection.outlook.com/?url=https%3A%2F%2Fxyz.r.us-east-1.awstrack.me%2FL0%2Fhttps%3A%252F%252Fclaude.ai%252Fmagic-link%253Ftoken%253Dnested_ok%2526state%253Ds9%2F1%2F0100&data=05\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(double_wrapped, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=nested_ok&state=s9")
    );

    // 2. Proofpoint v3 `*` placeholders decode from the `__;<base64>!` table (`Lz8m` is `/?&`).
    let ppv3_star = concat!(
        "To: sol-m27@proton.me\r\n\r\n",
        "https://urldefense.com/v3/__https://claude.ai*magic-link*token=ppv3_star*state=s2__;Lz8m!abc$\r\n"
    );
    assert_eq!(
        extract_magic_link_from_email(ppv3_star, Some("sol-m27@proton.me")).as_deref(),
        Some("https://claude.ai/magic-link?token=ppv3_star&state=s2")
    );

    // 3. Wrapped or opaquely tracked footer links do not shadow the OTP.
    let otp_with_tracked_footers = concat!(
        "To: sol-m27@proton.me\r\n",
        "Subject: Your Claude verification code\r\n\r\n",
        "<html><body>\r\n",
        "<a href=\"https://clicks.mailgun.org/c/view_in_browser_123\">View in browser</a>\r\n",
        "<p>Your verification code is: <strong>649201</strong></p>\r\n",
        "<p>Not you? <a href=\"https://claude.ai/auth/revoke?id=99\">Revoke this request</a></p>\r\n",
        "<footer>\r\n",
        "<a href=\"https://nam04.safelinks.protection.outlook.com/?url=https%3A%2F%2Fclaude.ai%2Fprivacy&data=05\">Privacy Policy</a>\r\n",
        "<a href=\"https://urldefense.com/v3/__https://support.anthropic.com/en/articles/123__;!!abc!def$\">Help Center</a>\r\n",
        "<a href=\"https://clicks.mailgun.org/c/unsub_456\">Unsubscribe</a>\r\n",
        "</footer></body></html>\r\n"
    );
    assert_eq!(
        extract_credential_from_email(otp_with_tracked_footers, Some("sol-m27@proton.me")),
        Some(MailboxCredential::Otp("649201".to_owned())),
        "tracked non-auth footer links and revoke links must not shadow the 6-digit OTP"
    );

    // 4. Anchor text picks the opaque tracked sign-in link over an earlier "View in browser".
    let opaque_tracker_magic = concat!(
        "To: sol-m27@proton.me\r\n",
        "Subject: Sign in to Claude\r\n\r\n",
        "<html><body>\r\n",
        "<p><a href=\"https://clicks.sendgrid.net/ls/click?upn=view_browser_tok\">View in browser</a></p>\r\n",
        "<p><a href=\"https://clicks.sendgrid.net/ls/click?upn=signin_cta_tok\">Sign in to Claude</a></p>\r\n",
        "<p><a href=\"https://clicks.sendgrid.net/ls/click?upn=unsub_tok\">Unsubscribe</a></p>\r\n",
        "</body></html>\r\n"
    );
    assert_eq!(
        extract_credential_from_email(opaque_tracker_magic, Some("sol-m27@proton.me")),
        Some(MailboxCredential::MagicLink(
            "https://clicks.sendgrid.net/ls/click?upn=signin_cta_tok".to_owned()
        )),
        "CTA anchor text promotes the opaque tracker sign-in link and rejects View in browser / Unsubscribe"
    );

    // Opaque token/state text is not an instruction to cancel the sign-in.
    for query in [
        "token=cancel_123",
        "token=deny_reject_decline_revoke_logout_signout",
        "token=opaque&state=access_denied",
        "token=email-preferences&state=unsubscribe",
    ] {
        let url = format!("https://claude.ai/magic-link?{query}");
        let email = format!("To: sol-m27@proton.me\r\n\r\n{url}\r\n");
        assert_eq!(
            extract_credential_from_email(&email, Some("sol-m27@proton.me")),
            Some(MailboxCredential::MagicLink(url)),
            "opaque query {query}"
        );
    }
    for tail in [
        "auth/revoke?token=opaque",
        "magic-link?token=opaque&error=access_denied",
        "magic-link?token=opaque&action=deny",
        "magic-link?token=opaque&action=approve&action=cancel",
        "magic-link?token=opaque&%65rror=denied",
        "magic-link?token=opaque&%45rror=denied",
    ] {
        let email = format!("To: sol-m27@proton.me\r\n\r\nhttps://claude.ai/{tail}\r\n");
        assert_eq!(
            extract_credential_from_email(&email, Some("sol-m27@proton.me")),
            None,
            "explicit refusal {tail}"
        );
    }

    // 5. A top-level base64 body yields its decoded OTP (739104), never a raw base64 line.
    let top_level_b64_otp = concat!(
        "To: sol-m27@proton.me\r\n",
        "Subject: Your verification code\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "WW91ciB2ZXJpZmljYXRpb24gY29kZSBpcyA3MzkxMDQg\r\n",
        "482910==\r\n"
    );
    assert_eq!(
        extract_otps_from_email(top_level_b64_otp, Some("sol-m27@proton.me")),
        vec!["739104".to_owned()],
        "top-level base64 OTP email ignores raw base64 lines and extracts only the decoded OTP"
    );

    // 6. A MIME boundary resets OTP context: context words before `--b1` promote no number
    // in the next part's headers or top lines.
    let multipart_boundary_bleed = concat!(
        "To: sol-m27@proton.me\r\n",
        "Content-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n",
        "--b1\r\n",
        "Content-Type: text/plain\r\n\r\n",
        "Please enter your verification code below:\r\n",
        "--b1\r\n",
        "Content-Type: text/html\r\n",
        "Content-ID: <112233>\r\n\r\n",
        "<div>Office address: 941070 Market St</div>\r\n",
        "<div>Your one-time passcode is 582019</div>\r\n",
        "--b1--\r\n"
    );
    assert_eq!(
        extract_otps_from_email(multipart_boundary_bleed, Some("sol-m27@proton.me"))
            .first()
            .map(String::as_str),
        Some("582019"),
        "MIME boundary resets context_lines_left so part 2's Content-ID and top address are not promoted over the real OTP"
    );
}
