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
password_file = "{}"
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
                    let _ = writeln!(writer, "* SEARCH 1 2\r\n{tag} OK SEARCH completed\r");
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
        CdpClient, MailboxConfig, drive_cdp_oauth_with_webmail, generate_totp_at,
        poll_mailbox_or_webmail_for_credential, split_nul_messages,
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
                                "html": "<div><p>Your verification code is: 908172</p></div>",
                                "text": "Sign in to Claude\nYour verification code is: 908172"
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
        use crate::harness::headless_oauth::MailboxCredential;
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
        assert_eq!(std::fs::read(&probe_seen).unwrap(), b"existing_digest\n");

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
        assert_eq!(
            std::fs::read_to_string(&probe_seen).unwrap(),
            "existing_digest\nfresh_digest\n"
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
}
