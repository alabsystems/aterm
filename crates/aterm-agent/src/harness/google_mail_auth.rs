// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Google-only account selection and password entry around the bounded OAuth reader.

pub(super) fn step_js(
    fallback: &str,
    email: Option<&str>,
    password: Option<&str>,
    submitted: bool,
    code_is_authenticator: bool,
) -> String {
    let quoted = |text: &str| aterm_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
      const url = String(window.location.href);
      if (window.location.origin !== 'https://accounts.google.com') {{
        if (url.startsWith('http://127.0.0.1:') || url.startsWith('chrome-error://')) return {fallback};
        return {{state:'google_refused',url}};
      }}
      const visible = e => e && !e.disabled && e.getClientRects().length > 0;
      if (Array.from(document.querySelectorAll('iframe[src*="recaptcha"], input[name="ca"], #captchaimg')).some(visible)) return {{state:'google_refused',url}};
      const email = {email};
      const choices = Array.from(document.querySelectorAll('[data-identifier]'));
      const choice = choices.find(e => visible(e) && String(e.getAttribute('data-identifier')).toLowerCase() === email.toLowerCase());
      if (choice) {{ choice.click(); return {{state:'clicked_consent',url}}; }}
      const identities = Array.from(document.querySelectorAll('[data-email], [data-identifier], #profileIdentifier'));
      const matches = email.length > 0 && identities.some(e => visible(e) && String(e.getAttribute('data-email') || e.getAttribute('data-identifier') || e.textContent).trim().toLowerCase() === email.toLowerCase());
      const field = Array.from(document.querySelectorAll('input[type="password"]')).find(visible);
      if (field) {{
        if (!matches) return {{state:'google_refused',url}};
        if ({submitted}) return {{state:'loading',url}};
        const password = {password};
        if (!password) return {{state:'google_no_password',url}};
        const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
        setter.call(field,password);
        field.dispatchEvent(new Event('input',{{bubbles:true}}));
        field.dispatchEvent(new Event('change',{{bubbles:true}}));
        const next = document.querySelector('#passwordNext');
        if (!visible(next)) return {{state:'loading',url}};
        next.click(); return {{state:'submitted_password',url}};
      }}
      const authenticator = Array.from(document.querySelectorAll('input[name="totpPin"], #totpPin')).some(visible);
      const sms = Array.from(document.querySelectorAll('input[name="smsPin"], #smsPin')).some(visible) || /\/challenge\/(?:sms|ipp)(?:[/?#]|$)/.test(url);
      if (sms || (authenticator && !matches)) return {{state:'google_refused',url}};
      if (authenticator && !{code_is_authenticator}) return {{state:'google_needs_totp',url}};
      const emailCode = Array.from(document.querySelectorAll('input[autocomplete="one-time-code" i], input[name*="code" i], input[name*="otp" i], input[inputmode="numeric" i], input[data-testid*="otp" i], input[maxlength="1"], input[name="idvPin"], #idvPin')).some(visible);
      if (emailCode && !matches) return {{state:'google_refused',url}};
      if (!authenticator && emailCode && {code_is_authenticator}) return {{state:'needs_mailbox',url}};
      const result = {fallback};
      if (authenticator && result.state === 'needs_mailbox') return {{state:'google_needs_totp',url}};
      return result;
    }})()"#,
        email = quoted(email.unwrap_or("")),
        password = quoted(password.unwrap_or(""))
    )
}

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn google_credentials_require_the_matching_gmail_route() {
        let mut config = MailboxConfig {
            passwords: BTreeMap::from([("me@gmail.com".into(), "fake-password".into())]),
            ..MailboxConfig::default()
        };
        assert!(google_credentials_config(&config, "me@gmail.com"));
        assert!(!google_credentials_config(&config, "other@gmail.com"));
        config.webmail_url = Some("https://mail.proton.me/u/0/inbox".into());
        assert!(!google_credentials_config(&config, "me@gmail.com"));
        config.webmail_url = None;
        config.imap_url = Some("imaps://imap.example.test/INBOX".into());
        assert!(!google_credentials_config(&config, "me@gmail.com"));
        config.imap_url = None;
        config.passwords.clear();
        config.password = Some("unscoped-password".into());
        assert!(!google_credentials_config(&config, "me@gmail.com"));
    }

    #[test]
    fn google_credentials_keep_unready_routes_and_reject_other_logins() {
        let config = MailboxConfig::from_toml_str(
            "[[mailboxes]]\nlogin = 'me@gmail.com'\nimap_url = 'imaps://imap.gmail.com/INBOX'\nreceives = ['me@gmail.com']\n",
            None,
        ).unwrap();
        assert!(config.password_for(Some("me@gmail.com")).is_none());
        assert!(google_credentials_config(&config, "me@gmail.com"));
        for endpoint in [
            "imaps://other%40gmail.com@imap.gmail.com/INBOX",
            "imap://imap.gmail.com/INBOX",
        ] {
            let config = MailboxConfig::from_toml_str(
                &format!("user = 'me@gmail.com'\nimap_url = '{endpoint}'\n"),
                None,
            )
            .unwrap();
            assert!(
                !google_credentials_config(&config, "me@gmail.com"),
                "{endpoint}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn google_observed_password_need_and_email_code_follow_distinct_paths() {
        use aterm_secrets::mailbox_component::{Component, Part, SOURCE_ENTRY};
        use aterm_secrets::{client, need, text::Audience};
        use std::io::{Read, Write};
        for missing_password in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path().join("member");
            let paths = client::Paths::for_home(&home);
            client::create(
                &paths,
                root.path().join("fixture.vault").to_str().unwrap(),
                None,
                Some("member"),
            )
            .unwrap();
            let text = if missing_password {
                "user = 'me@gmail.com'\nwebmail_url = 'https://mail.google.com/'\n"
            } else {
                "user = 'me@gmail.com'\nwebmail_url = 'https://mail.google.com/'\npassword = 'fixture-password'\ntotp_secret = 'JBSWY3DPEHPK3PXP'\n"
            };
            let admin = client::admin_open(&paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, text.as_bytes())
                .unwrap();
            client::admin_publish(&paths, &admin, &doc).unwrap();
            let mut config = MailboxConfig::from_toml_str(text, None).unwrap();
            crate::harness::mailbox_needs::load_components(Some(&home), &mut config);
            assert!(google_credentials_config(&config, "me@gmail.com"));
            let (commands, to_browser) = std::io::pipe().unwrap();
            let (from_browser, mut replies) = std::io::pipe().unwrap();
            let browser = std::thread::spawn(move || {
                let mut commands = commands;
                let mut pending = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = commands.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buf[..n]);
                    for msg in split_nul_messages(&mut pending) {
                        let method = msg["method"].as_str().unwrap();
                        let result = match method {
                            "Target.createTarget" => json!({"targetId":"google"}),
                            "Target.attachToTarget" => json!({"sessionId":"google"}),
                            "Runtime.evaluate" => json!({"result":{"value":{
                                "state":if missing_password { "google_no_password" } else { "needs_mailbox" },
                                "url":"https://accounts.google.com/signin"
                            }}}),
                            _ => json!({}),
                        };
                        let mut reply =
                            aterm_json::to_vec(&json!({"id":msg["id"],"result":result})).unwrap();
                        reply.push(0);
                        replies.write_all(&reply).unwrap();
                    }
                }
            });
            let mut cdp = CdpClient::new(to_browser, from_browser);
            let start = "https://accounts.google.com/o/oauth2/v2/auth?client_id=ours&redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Foauth%2Fcallback&state=nonce";
            let clock = Instant::now;
            let mut mailbox_calls = 0;
            let result = drive_cdp_oauth_flow_until(
                &mut cdp,
                (start, google_mail_flow(start).unwrap(), Some(&config)),
                Some("me@gmail.com"),
                (
                    Deadline::new(Duration::from_secs(60), &clock),
                    Duration::ZERO,
                ),
                Duration::from_secs(60),
                |_, _| {
                    mailbox_calls += 1;
                    Err("fixture mailbox callback")
                },
                |_| panic!("no callback expected"),
            );
            drop(cdp);
            browser.join().unwrap();
            let needs = need::peek(&paths).unwrap();
            if missing_password {
                assert!(result.unwrap_err().contains("No stored password"));
                assert_eq!(mailbox_calls, 0);
                assert_eq!(needs.len(), 1);
                assert_eq!(
                    needs[0].want,
                    need::Want::MailboxComponent(
                        Component::for_mailbox(
                            "https://mail.google.com",
                            "me@gmail.com",
                            Part::Password
                        )
                        .unwrap()
                    )
                );
            } else {
                assert_eq!(result, Err("fixture mailbox callback"));
                assert_eq!(
                    mailbox_calls, 1,
                    "email challenge must read mail despite stored TOTP"
                );
                assert!(needs.is_empty());
            }
        }
    }

    /// NEGATIVE CONTROL: an already-signed-in Google profile facing `google_needs_totp` uses the
    /// route's configured `totp_secret` even when the route has no stored password, without
    /// raising a spurious `Part::Totp` or `Part::Password` need.
    #[cfg(unix)]
    #[test]
    fn google_totp_step_up_uses_route_totp_without_password_and_records_no_spurious_need() {
        use aterm_secrets::mailbox_component::SOURCE_ENTRY;
        use aterm_secrets::{client, need, text::Audience};
        use std::io::{Read, Write};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("member");
        let paths = client::Paths::for_home(&home);
        client::create(
            &paths,
            root.path().join("fixture.vault").to_str().unwrap(),
            None,
            Some("member"),
        )
        .unwrap();
        let text = concat!(
            "[[mailboxes]]\n",
            "login = 'me@gmail.com'\n",
            "webmail_url = 'https://mail.google.com/'\n",
            "totp_secret = 'JBSWY3DPEHPK3PXP'\n",
            "receives = ['me@gmail.com']\n",
        );
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, text.as_bytes())
            .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let mut config = MailboxConfig::from_toml_str(text, None).unwrap();
        crate::harness::mailbox_needs::load_components(Some(&home), &mut config);
        assert!(config.password_for(Some("me@gmail.com")).is_none());
        let callback_url = "http://127.0.0.1:9000/oauth/callback?state=nonce&code=stepup-ok";
        let (commands, to_browser) = std::io::pipe().unwrap();
        let (from_browser, mut replies) = std::io::pipe().unwrap();
        let browser = std::thread::spawn(move || {
            let mut commands = commands;
            let mut pending = Vec::new();
            let mut buf = [0u8; 8192];
            let mut eval_calls = 0usize;
            loop {
                let n = commands.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                pending.extend_from_slice(&buf[..n]);
                for msg in split_nul_messages(&mut pending) {
                    let method = msg["method"].as_str().unwrap();
                    let result = match method {
                        "Target.createTarget" => json!({"targetId":"google"}),
                        "Target.attachToTarget" => json!({"sessionId":"google"}),
                        "Runtime.evaluate" => {
                            eval_calls += 1;
                            let expr = msg["params"]["expression"].as_str().unwrap();
                            if eval_calls == 1 {
                                json!({"result":{"value":{
                                    "state":"google_needs_totp",
                                    "url":"https://accounts.google.com/signin/v2/challenge/totp"
                                }}})
                            } else {
                                assert!(
                                    expr.contains("true") && !expr.contains("const otp = null"),
                                    "expected authenticator TOTP code in second evaluate: {expr}"
                                );
                                json!({"result":{"value":{
                                    "state":"callback_recovered",
                                    "url":callback_url
                                }}})
                            }
                        }
                        _ => json!({}),
                    };
                    let mut reply =
                        aterm_json::to_vec(&json!({"id":msg["id"],"result":result})).unwrap();
                    reply.push(0);
                    replies.write_all(&reply).unwrap();
                }
            }
        });
        let mut cdp = CdpClient::new(to_browser, from_browser);
        let start = "https://accounts.google.com/o/oauth2/v2/auth?client_id=ours&redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Foauth%2Fcallback&state=nonce";
        let clock = Instant::now;
        let mut delivered = Vec::new();
        let result = drive_cdp_oauth_flow_until(
            &mut cdp,
            (start, google_mail_flow(start).unwrap(), Some(&config)),
            Some("me@gmail.com"),
            (
                Deadline::new(Duration::from_secs(60), &clock),
                Duration::ZERO,
            ),
            Duration::from_secs(60),
            |_, _| panic!("authenticator challenge must not read mailbox"),
            |url| {
                delivered.push(url.to_owned());
                Ok(200)
            },
        );
        drop(cdp);
        browser.join().unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(delivered, vec![callback_url]);
        assert!(need::peek(&paths).unwrap().is_empty());
    }

    #[test]
    fn google_flow_keeps_its_own_origin_callback_and_identity() {
        let start = "https://accounts.google.com/o/oauth2/v2/auth?client_id=ours&redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Foauth%2Fcallback&state=nonce";
        let flow = google_mail_flow(start).unwrap();
        assert!(flow.admits_provider_url("https://accounts.google.com/v3/signin/identifier"));
        assert!(!flow.admits_provider_url("https://accounts.google.com.evil.test/v3/signin"));
        assert!(
            !flow.admits_provider_url(
                "https://accounts.google.com/o/oauth2/v2/auth?client_id=other"
            )
        );
        assert!(!flow.admits_provider_url("https://mail.google.com/"));
        assert!(flow.is_callback("http://127.0.0.1:9000/oauth/callback?state=nonce&code=code"));
        assert!(!flow.is_callback("http://127.0.0.1:9000/oauth/callback?state=wrong&code=code"));
        assert!(
            MailboxFlow::new(start).is_err(),
            "Claude's origin pin is unchanged"
        );
        assert!(google_mail_flow(&start.replace("/o/oauth2/v2/auth", "/elsewhere")).is_err());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the provisioned headless browser"]
    fn google_password_script_live_browser_enforces_origin_identity_and_one_submission() {
        use std::os::fd::OwnedFd;
        let browser = find_headless_browser(aterm_types::dirs::home_dir().as_deref())
            .expect("headless browser");
        let profile = aterm_tempfile::tempdir().unwrap();
        let (command_read, command_write) = std::io::pipe().unwrap();
        let (event_read, event_write) = std::io::pipe().unwrap();
        let command =
            headless_browser_command(&browser, profile.path(), &|name| std::env::var_os(name));
        let child = start_browser(
            command,
            OwnedFd::from(command_read),
            OwnedFd::from(event_write),
        )
        .unwrap();
        let _child = BrowserChild::new(child);
        let mut cdp = CdpClient::new(command_write, event_read);
        let within = Duration::from_secs(60);
        let target = cdp
            .call(
                None,
                "Target.createTarget",
                json!({"url":"about:blank"}),
                within,
            )
            .unwrap();
        let attached = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":target["targetId"],"flatten":true}),
                within,
            )
            .unwrap();
        let session = attached["sessionId"].as_str().unwrap();
        for (origin, identity, password, submitted, expected, action_count) in [
            (
                "https://accounts.google.com",
                "me@gmail.com",
                Some("fake-password"),
                false,
                "submitted_password",
                2,
            ),
            (
                "https://accounts.google.com.evil.test",
                "me@gmail.com",
                Some("fake-password"),
                false,
                "google_refused",
                0,
            ),
            (
                "https://accounts.google.com",
                "other@gmail.com",
                Some("fake-password"),
                false,
                "google_refused",
                0,
            ),
            (
                "https://accounts.google.com",
                "me@gmail.com",
                None,
                false,
                "google_no_password",
                0,
            ),
            (
                "https://accounts.google.com",
                "other@gmail.com",
                None,
                false,
                "google_refused",
                0,
            ),
            (
                "https://accounts.google.com",
                "me@gmail.com",
                Some("fake-password"),
                true,
                "loading",
                0,
            ),
        ] {
            let step = super::step_js(
                "({state:'fallback'})",
                Some("me@gmail.com"),
                password,
                submitted,
                false,
            );
            let script = format!(
                r#"(() => {{
                const actions=[];
                class HTMLInputElement {{ getClientRects() {{ return [1]; }} dispatchEvent() {{}} }}
                Object.defineProperty(HTMLInputElement.prototype,'value',{{set(value){{actions.push(value);}}}});
                class Event {{}}
                const input=new HTMLInputElement();
                const window={{location:{{origin:{origin},href:{origin}+'/signin'}}}};
                const identity={{getAttribute:()=>{identity},getClientRects:()=>[1]}};
                const document={{
                  querySelectorAll: selector => selector.startsWith('input[type=') ? [input] : selector.startsWith('[data-email]') ? [identity] : [],
                  querySelector: () => ({{getClientRects:()=>[1],click:()=>actions.push('next')}})
                }};
                const result={step}; return {{result,actions}};
            }})()"#,
                origin = aterm_json::to_string(origin).unwrap(),
                identity = aterm_json::to_string(identity).unwrap()
            );
            let result = cdp
                .call(
                    Some(session),
                    "Runtime.evaluate",
                    json!({"expression":script,"returnByValue":true}),
                    within,
                )
                .unwrap();
            let value = &result["result"]["value"];
            assert_eq!(value["result"]["state"].as_str(), Some(expected));
            assert_eq!(value["actions"].as_array().unwrap().len(), action_count);
        }
        for (kind, identity, fallback, authenticator_code, expected, fallback_calls) in [
            (
                "totp",
                "me@gmail.com",
                "needs_mailbox",
                false,
                "google_needs_totp",
                0,
            ),
            (
                "totp",
                "me@gmail.com",
                "submitted_otp",
                false,
                "google_needs_totp",
                0,
            ),
            (
                "totp",
                "me@gmail.com",
                "submitted_otp",
                true,
                "submitted_otp",
                1,
            ),
            (
                "totp",
                "other@gmail.com",
                "needs_mailbox",
                false,
                "google_refused",
                0,
            ),
            (
                "email",
                "me@gmail.com",
                "needs_mailbox",
                false,
                "needs_mailbox",
                1,
            ),
            (
                "email",
                "me@gmail.com",
                "submitted_otp",
                true,
                "needs_mailbox",
                0,
            ),
            (
                "email-idv",
                "me@gmail.com",
                "needs_mailbox",
                false,
                "needs_mailbox",
                1,
            ),
            (
                "sms",
                "me@gmail.com",
                "needs_mailbox",
                false,
                "google_refused",
                0,
            ),
            (
                "email",
                "other@gmail.com",
                "submitted_otp",
                false,
                "google_refused",
                0,
            ),
        ] {
            let step = super::step_js(
                &format!(
                    "(() => {{ fallbackCalls += 1; return {{state:{}}}; }})()",
                    aterm_json::to_string(fallback).unwrap()
                ),
                Some("me@gmail.com"),
                None,
                false,
                authenticator_code,
            );
            let script = format!(
                r#"(() => {{
                  let fallbackCalls = 0;
                  const kind={kind};
                  const window={{location:{{origin:'https://accounts.google.com',href:'https://accounts.google.com/' + (kind === 'sms' ? 'challenge/ipp' : 'signin')}}}};
                  const visible={{getClientRects:()=>[1]}};
                  const identity={{getAttribute:()=>{identity},getClientRects:()=>[1]}};
                  const document={{querySelectorAll: selector =>
                    selector.startsWith('[data-email]') ? [identity] :
                    selector.startsWith('input[name="totpPin"]') && kind === 'totp' ? [visible] :
                    selector.startsWith('input[autocomplete=') && (kind === 'email' || kind === 'email-idv') ? [visible] : []}};
                  const result = {step}; return {{result,fallbackCalls}};
                }})()"#,
                identity = aterm_json::to_string(identity).unwrap(),
                kind = aterm_json::to_string(kind).unwrap(),
            );
            let result = cdp
                .call(
                    Some(session),
                    "Runtime.evaluate",
                    json!({"expression":script,"returnByValue":true}),
                    within,
                )
                .unwrap();
            assert_eq!(
                result["result"]["value"]["result"]["state"].as_str(),
                Some(expected),
                "{kind}/{identity}/{fallback}"
            );
            assert_eq!(
                result["result"]["value"]["fallbackCalls"].as_u64(),
                Some(fallback_calls),
                "{kind}/{identity}/{fallback}: code submission must be gated before fallback"
            );
        }
    }
}
