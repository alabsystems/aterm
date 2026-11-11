// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Native Proton and persistent IMAP reads for authentication emails.

use super::*;
use aterm_mail::Mailbox;

pub(super) const BUSY: &str = "Native Proton mailbox is owned by another process";
const GOOGLE_AUTH_MISSING: &str = "No stored Gmail authorization or app password is available";

fn is_proton_webmail_url(url: &str) -> bool {
    strip_ascii_prefix_ci(url.trim(), "https://mail.proton.me")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

pub(super) fn managed_google_target(
    cfg: &MailboxConfig,
    target: Option<&str>,
) -> Option<(String, ParsedImapUrl)> {
    let route = cfg.route_for(target);
    let (imap, webmail, http) = if let Some(route) = &route {
        (&route.imap_url, &route.webmail_url, &route.http_url)
    } else {
        (&cfg.imap_url, &cfg.webmail_url, &cfg.http_url)
    };
    if http.is_some() {
        return None;
    }
    let (mut endpoint, explicit_user) = if let Some(url) = imap {
        let endpoint = parse_imap_url(url)?;
        if !endpoint.tls || !endpoint.host.eq_ignore_ascii_case("imap.gmail.com") {
            return None;
        }
        if endpoint
            .user
            .as_deref()
            .is_some_and(|u| u.trim().is_empty())
        {
            return None;
        }
        let user = endpoint.user.clone();
        (endpoint, user)
    } else {
        (parse_imap_url("imaps://imap.gmail.com/INBOX")?, None)
    };
    if webmail.as_deref().is_some_and(|url| {
        super::super::login_drive::https_host(url).as_deref() != Some("mail.google.com")
    }) {
        return None;
    }
    let declared_login = route.as_ref().map(|route| route.login.clone());
    if let (Some(explicit), Some(declared)) = (&explicit_user, &declared_login)
        && !explicit.eq_ignore_ascii_case(declared)
    {
        return None;
    }
    let login = explicit_user
        .or(declared_login)
        .or_else(|| cfg.webmail_user_for(target))
        .filter(|login| super::super::accounts::email_refusal(login).is_none())?;
    if imap.is_none()
        && webmail.is_none()
        && !login.rsplit_once('@').is_some_and(|(_, domain)| {
            domain.eq_ignore_ascii_case("gmail.com")
                || domain.eq_ignore_ascii_case("googlemail.com")
        })
    {
        return None;
    }
    endpoint.user = Some(login.clone());
    Some((login, endpoint))
}

fn saved_google<S>(
    cfg: &MailboxConfig,
    target: Option<&str>,
    load: impl FnOnce(&str) -> Result<Option<S>, String>,
) -> Result<Option<(String, ParsedImapUrl, S)>, &'static str> {
    let Some((account, endpoint)) = managed_google_target(cfg, target) else {
        return Ok(None);
    };
    let session = load(&account).map_err(|_| "Stored Google mail authorization is unavailable")?;
    Ok(session.map(|session| (account, endpoint, session)))
}

#[cfg(test)]
fn managed_google_login(cfg: &MailboxConfig, target: Option<&str>) -> Option<String> {
    managed_google_target(cfg, target).map(|(login, _)| login)
}

pub(super) fn poll(
    cfg: &MailboxConfig,
    target: Option<&str>,
    inherited: Option<Deadline<'_>>,
    once: bool,
    seen_otps: &mut BTreeSet<String>,
    expiry_words: &'static str,
) -> Option<Result<MailboxCredential, &'static str>> {
    let preview = cfg.routed_for(target);
    if preview.dir.is_some() || preview.http_url.is_some() {
        return None;
    }
    let clock = Instant::now;
    let deadline = inherited.unwrap_or_else(|| Deadline::new(Duration::from_secs(60), &clock));
    if deadline.expired() {
        return Some(Err(expiry_words));
    }
    let mut fresh_source = cfg.clone();
    if fresh_source.since.is_none() {
        fresh_source.since = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok());
        if fresh_source.since.is_none() {
            return Some(Err("Invalid clock for authentication mail"));
        }
    }
    let source_cfg = &fresh_source;
    let routed = source_cfg.routed_for(target);
    let cfg = &*routed;
    let explicit_imap = cfg.effective_imap_url(target);
    if let Some(ref url) = explicit_imap {
        let imap = parse_imap_url(url)?;
        if !imap.tls || !imap.host.eq_ignore_ascii_case("imap.gmail.com") {
            return None;
        }
    }
    if let Some(home) = aterm_types::dirs::home_dir() {
        match saved_google(source_cfg, target, |account| {
            aterm_mail::oauth::session::shared(&home, account)
        }) {
            Ok(Some((account, imap, session))) => {
                return Some(
                    run_gmail(
                        cfg,
                        target,
                        deadline,
                        once,
                        seen_otps,
                        imap,
                        Some(session),
                        account,
                        None,
                    )
                    .map_err(|error| {
                        if deadline.expired() || error == super::super::auth_deadline::EXPIRED {
                            expiry_words
                        } else {
                            error
                        }
                    }),
                );
            }
            Err(why) => return Some(Err(why)),
            Ok(None) => {}
        }
    }
    if managed_google_target(source_cfg, target).is_some()
        && cfg
            .password_for(target)
            .is_none_or(|password| password.chars().all(char::is_whitespace))
        && explicit_imap
            .as_deref()
            .and_then(parse_imap_url)
            .and_then(|url| url.password)
            .is_none_or(|password| password.chars().all(char::is_whitespace))
    {
        return Some(Err(GOOGLE_AUTH_MISSING));
    }
    if let Some(url) = explicit_imap {
        let imap = parse_imap_url(&url)?;
        if deadline.left().is_err() {
            return Some(Err(expiry_words));
        }
        let login = imap
            .user
            .clone()
            .or_else(|| cfg.webmail_user_for(target))
            .filter(|u| !u.trim().is_empty())?;
        let password = imap
            .password
            .clone()
            .or_else(|| cfg.password_for(target))
            .map(|pw| {
                pw.chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
            })
            .filter(|pw| !pw.is_empty())?;
        return Some(
            run_gmail(
                cfg,
                target,
                deadline,
                once,
                seen_otps,
                imap,
                None,
                login,
                Some(password),
            )
            .map_err(|error| {
                if deadline.expired() || error == super::super::auth_deadline::EXPIRED {
                    expiry_words
                } else {
                    error
                }
            }),
        );
    }
    let url = cfg.effective_webmail_url(target)?;
    if !is_proton_webmail_url(&url) {
        return None;
    }
    if deadline.left().is_err() {
        return Some(Err(expiry_words));
    }
    let login = cfg
        .webmail_user_for(target)
        .filter(|u| !u.trim().is_empty())?;
    let password = cfg
        .password_for(target)
        .filter(|pw| !pw.trim().is_empty())?;
    Some(
        run(
            source_cfg, cfg, target, deadline, once, seen_otps, login, password,
        )
        .map_err(|error| {
            if deadline.expired() || error == super::super::auth_deadline::EXPIRED {
                expiry_words
            } else {
                error
            }
        }),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "validated login, optional managed Google session, and optional app-password passed from poll"
)]
fn run_gmail(
    cfg: &MailboxConfig,
    target: Option<&str>,
    deadline: Deadline<'_>,
    once: bool,
    seen_otps: &mut BTreeSet<String>,
    imap: ParsedImapUrl,
    managed: Option<aterm_mail::oauth::session::Shared>,
    login: String,
    password: Option<String>,
) -> Result<MailboxCredential, &'static str> {
    deadline.left()?;
    if managed.is_none() && password.is_none() {
        return Err("No Gmail credential");
    }
    let mut seen = load_seen_digests(cfg.seen_file.as_deref());
    let source = if managed.is_some() {
        "google-mail-oauth".to_owned()
    } else {
        super::super::mailbox_routes::login_source(
            &cfg.effective_imap_url(target).ok_or("No Gmail endpoint")?,
        )
    };
    if !login_may_be_tried(cfg, &source, &login, &mut seen) {
        return Err(super::super::mailbox_routes::refused_words(&login));
    }
    let connect = || -> Result<aterm_mail::imap::Reader, &'static str> {
        loop {
            deadline.left()?;
            let authentication =
                match gmail_authentication(managed.as_ref(), &login, password.as_deref(), deadline)
                {
                    Ok(authentication) => authentication,
                    Err("Gmail authentication refused") => {
                        return Err("Gmail authentication refused");
                    }
                    Err(error) if once => return Err(error),
                    Err(_) => {
                        deadline
                            .pause(Duration::from_secs(2))
                            .map_err(|e| e.words())?;
                        continue;
                    }
                };
            let account = aterm_mail::imap::Imap {
                endpoint: aterm_mail::Endpoint {
                    host: imap.host.clone(),
                    port: imap.port,
                    security: aterm_mail::Security::Tls,
                },
                authentication,
            };
            match account.open_until(&imap.mailbox, deadline.end()) {
                Ok(reader) => return Ok(reader),
                Err(aterm_mail::imap::ConnectError::Authentication) => {
                    return Err("Gmail authentication refused");
                }
                Err(_) if !once => {
                    deadline
                        .pause(Duration::from_secs(2))
                        .map_err(|e| e.words())?;
                }
                Err(_) => return Err("Gmail IMAP connection failed"),
            }
        }
    };
    let mut reader = match connect() {
        Ok(reader) => reader,
        Err(error) => {
            if error == "Gmail authentication refused" {
                note_login(cfg, &source, &login, LoginOutcome::Refused, &mut seen);
                return Err(super::super::mailbox_routes::refused_words(&login));
            }
            return Err(error);
        }
    };
    note_login(cfg, &source, &login, LoginOutcome::Accepted, &mut seen);
    let mut inspected = BTreeSet::new();
    loop {
        let interrupted = 'scan: {
            let messages = match reader.messages(100) {
                Ok(messages) => messages,
                Err(_) => break 'scan true,
            };
            for message in messages {
                if inspected.contains(&message.id)
                    || !MailFilter::of(cfg).admits_envelope(&envelope(&message))
                {
                    continue;
                }
                deadline.left()?;
                let bytes = match reader.source(&message.id) {
                    Ok(bytes) => bytes,
                    Err(_) => break 'scan true,
                };
                let raw = String::from_utf8_lossy(&bytes);
                inspected.insert(message.id);
                deadline.left()?;
                if let Some(credential) = accept_raw(cfg, target, &raw, &mut seen, seen_otps) {
                    return Ok(credential);
                }
            }
            if once {
                return Err(MAILBOX_TIMEOUT);
            }
            reader
                .wait(deadline.left()?.min(Duration::from_secs(30)))
                .is_err()
        };
        if interrupted {
            drop(reader);
            if once {
                return Err("Gmail inbox read failed");
            }
            deadline
                .pause(Duration::from_secs(2))
                .map_err(|e| e.words())?;
            reader = match connect() {
                Ok(reader) => reader,
                Err(error) => {
                    if error == "Gmail authentication refused" {
                        note_login(cfg, &source, &login, LoginOutcome::Refused, &mut seen);
                        return Err(super::super::mailbox_routes::refused_words(&login));
                    }
                    return Err(error);
                }
            };
            inspected.clear();
        }
    }
}

fn gmail_authentication(
    session: Option<&aterm_mail::oauth::session::Shared>,
    login: &str,
    password: Option<&str>,
    deadline: Deadline<'_>,
) -> Result<aterm_mail::Authentication, &'static str> {
    if let Some(session) = session {
        let mut session = loop {
            deadline.left()?;
            match session.try_lock() {
                Ok(session) => break session,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("Google mail authorization lock failed");
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    deadline
                        .pause(Duration::from_millis(10))
                        .map_err(|e| e.words())?;
                }
            }
        };
        session.renew_until(deadline.end()).map_err(|error| {
            if error.retryable() {
                "Google mail authorization is temporarily unavailable"
            } else {
                "Gmail authentication refused"
            }
        })?;
        session
            .authentication()
            .ok_or("Google mail authorization is unavailable")
    } else {
        Ok(aterm_mail::Authentication::Password {
            username: login.into(),
            password: aterm_mail::Secret::new(
                password
                    .ok_or("No Gmail credential")?
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect(),
            ),
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "source and routed configs, deadline, seen state, and validated credentials passed from poll"
)]
fn run(
    source_cfg: &MailboxConfig,
    cfg: &MailboxConfig,
    target: Option<&str>,
    deadline: Deadline<'_>,
    once: bool,
    seen_otps: &mut BTreeSet<String>,
    login: String,
    password: String,
) -> Result<MailboxCredential, &'static str> {
    deadline.left()?;
    let home = aterm_types::dirs::home_dir().ok_or("No home directory for mail")?;
    let email = if login.contains('@') {
        login.clone()
    } else {
        format!("{login}@proton.me")
    };
    let mailbox_password = cfg.mailbox_password_for(target);
    let totp_secret = cfg.totp_secret_for(target);
    let mut seen = load_seen_digests(cfg.seen_file.as_deref());
    let source = super::super::mailbox_routes::login_source(
        &cfg.effective_webmail_url(target)
            .ok_or("No Proton endpoint")?,
    );
    if !login_may_be_tried(cfg, &source, &login, &mut seen) {
        return Err(super::super::mailbox_routes::refused_words(&login));
    }
    let connected = super::super::mail_browser::shared_mailbox_until(
        &home,
        super::super::mail_browser::Credentials {
            email: &email,
            login: &login,
            password: &password,
            mailbox_password: mailbox_password.as_deref(),
            totp_secret: totp_secret.as_deref(),
        },
        Some(deadline.end()),
    );
    let mailbox = match connected {
        Ok(mailbox) => {
            note_login(cfg, &source, &login, LoginOutcome::Accepted, &mut seen);
            // The mailbox is open to read this code: its own unlock read the address list, which
            // the lab's discovery records (design §4.10). No request is made for it.
            if let Some(listed) = super::super::mailbox_seen::of_open(&mailbox, &email, target) {
                super::super::mailbox_seen::note(&listed);
            }
            mailbox
        }
        Err(error) if error.message == "The mail browser profile is already in use" => {
            return Err(BUSY);
        }
        Err(error) if error.message == "Headless mailbox requires an unavailable second factor" => {
            super::super::mailbox_needs::record_observed_missing(
                source_cfg,
                target,
                aterm_secrets::mailbox_component::Part::Totp,
            );
            return Err("Headless mailbox requires an unavailable second factor");
        }
        Err(error)
            if error.message == "Headless mailbox requires an unavailable second password" =>
        {
            super::super::mailbox_needs::record_observed_missing(
                source_cfg,
                target,
                aterm_secrets::mailbox_component::Part::SecondPassword,
            );
            return Err("Headless mailbox requires an unavailable second password");
        }
        Err(error) if error.message == "Proton rejected the headless browser credentials" => {
            note_login(cfg, &source, &login, LoginOutcome::Refused, &mut seen);
            return Err(super::super::mailbox_routes::refused_words(&login));
        }
        Err(_) => return Err("Native Proton mailbox connection failed"),
    };
    let mut inspected = BTreeSet::new();
    'poll: loop {
        let messages = match read(&mailbox, deadline, |p| p.messages("0", 100)) {
            Ok(messages) => messages,
            Err(true) if !once && pause(deadline) => continue,
            Err(_) => return Err("Native Proton inbox read failed"),
        };
        for message in messages {
            if inspected.contains(&message.id)
                || !MailFilter::of(cfg).admits_envelope(&envelope(&message))
            {
                continue;
            }
            deadline.left()?;
            let (message, _) = match read(&mailbox, deadline, |p| p.read_source(&message.id)) {
                Ok(message) => message,
                Err(true) if !once && pause(deadline) => continue 'poll,
                Err(_) => return Err("Native Proton message read failed"),
            };
            inspected.insert(message.id.clone());
            deadline.left()?;
            if let Some(credential) = accept(cfg, target, &message, &mut seen, seen_otps) {
                return Ok(credential);
            }
        }
        if once || !pause(deadline) {
            return Err("timed out waiting for verification email in mailbox");
        }
    }
}

fn read<T>(
    mailbox: &super::super::mail_browser::SharedMailbox,
    deadline: Deadline<'_>,
    operation: impl FnMut(&mut aterm_mail::proton::Proton) -> Result<T, String>,
) -> Result<T, bool> {
    let mut mailbox = loop {
        let pause = super::super::mail_browser::operation_remaining(
            Some(deadline.end()),
            Duration::from_millis(10),
        )
        .map_err(|_| false)?;
        match mailbox.try_lock() {
            Ok(mailbox) => break mailbox,
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(false),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(pause),
        }
    };
    mailbox
        .run_until(deadline.end(), |p| {
            p.with_verification(operation, |_| Err("Mail verification unavailable".into()))
        })
        .map_err(|_| {
            mailbox.last_failure().is_some_and(|failure| {
                failure.recovery() == aterm_mail::proton_error::Recovery::RetryRead
            })
        })
}

fn pause(deadline: Deadline<'_>) -> bool {
    deadline.pause(Duration::from_secs(2)).is_ok() && !deadline.expired()
}

fn envelope(message: &aterm_mail::Message) -> String {
    aterm_json::to_string(&json!({
        "From": message.from, "To": message.to, "Subject": message.subject,
        "Date": message.date.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::String(message.date.clone())), "Html": message.text,
    }))
    .unwrap_or_default()
}

fn accept(
    cfg: &MailboxConfig,
    target: Option<&str>,
    message: &aterm_mail::Message,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    accept_raw(cfg, target, &envelope(message), seen, seen_otps)
}

fn accept_raw(
    cfg: &MailboxConfig,
    target: Option<&str>,
    raw: &str,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let digest = fnv1a_hex(raw);
    if seen.contains(&digest) {
        return None;
    }
    let (credential, exhausted) =
        extract_unseen_credential_from_email(raw, target, seen, seen_otps, &MailFilter::of(cfg))?;
    record_consumed_credential(
        cfg.seen_file.as_deref(),
        seen,
        seen_otps,
        target,
        &digest,
        &credential,
        exhausted,
    );
    Some(credential)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routed_gmail_uses_its_saved_encrypted_grant_without_a_password() {
        let home = aterm_tempfile::tempdir().unwrap();
        let cfg = MailboxConfig::from_toml_str(
            "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imaps://imap.gmail.com/Archive'\nreceives=['alias@example.com']\n",
            None,
        ).unwrap();
        let target = Some("alias@example.com");
        assert!(!cfg.route_for(target).unwrap().ready());
        assert!(cfg.is_configured_for(target));
        assert_eq!(cfg.unready_words(target), None);
        aterm_mail::oauth::session::register(
            home.path(),
            "saved@gmail.com",
            "fixture-client",
            None,
            aterm_mail::Secret::new("fixture-refresh".into()),
        )
        .unwrap();
        let mut requested = Vec::new();
        let (account, endpoint, _session) = saved_google(&cfg, target, |account| {
            requested.push(account.to_owned());
            aterm_mail::oauth::session::shared(home.path(), account)
        })
        .unwrap()
        .unwrap();
        assert_eq!(account, "saved@gmail.com");
        assert_eq!(requested, ["saved@gmail.com"]);
        assert_eq!(endpoint.host, "imap.gmail.com");
        assert_eq!(endpoint.mailbox, "Archive");
        assert!(endpoint.password.is_none());
        let missing=MailboxConfig::from_toml_str("[[mailboxes]]\nlogin='missing@gmail.com'\nwebmail_url='https://mail.google.com/'\nreceives=['alias@example.com']\n",None).unwrap();
        assert!(
            saved_google(&missing, target, |account| {
                aterm_mail::oauth::session::shared(home.path(), account)
            })
            .unwrap()
            .is_none(),
            "another account's stored grant is never a fallback"
        );
        for text in [
            "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imaps://other@gmail.com@imap.gmail.com/INBOX'\nreceives=['alias@example.com']\n",
            "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imap://imap.gmail.com/INBOX'\nreceives=['alias@example.com']\n",
            "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imaps://imap.gmail.com.evil.test/INBOX'\nreceives=['alias@example.com']\n",
        ] {
            let other = MailboxConfig::from_toml_str(text, None).unwrap();
            assert!(
                saved_google::<()>(&other, target, |_| panic!(
                    "unrelated or insecure endpoint reached the Google grant"
                ))
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn managed_google_mail_uses_the_configured_mailbox_identity() {
        let mut cfg = MailboxConfig {
            user: Some("mailbox-a@gmail.com".into()),
            imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
            ..Default::default()
        };
        assert_eq!(
            managed_google_login(&cfg, Some("target-b@gmail.com")).as_deref(),
            Some("mailbox-a@gmail.com")
        );
        cfg.imap_url = Some("imaps://embedded-c@gmail.com@imap.gmail.com/Archive".into());
        assert_eq!(
            managed_google_login(&cfg, Some("target-b@gmail.com")).as_deref(),
            Some("embedded-c@gmail.com")
        );
        cfg.imap_url = None;
        cfg.webmail_url = Some("https://mail.proton.me/u/0/inbox".into());
        assert!(managed_google_login(&cfg, Some("target-b@gmail.com")).is_none());
        cfg.webmail_url = Some("https://mail.google.com.evil.test/".into());
        assert!(managed_google_login(&cfg, Some("target-b@gmail.com")).is_none());

        let routed = MailboxConfig::from_toml_str(
            r#"
[[mailboxes]]
login = "owner@proton.me"
webmail_url = "https://mail.proton.me/u/0/inbox"
receives = ["*@proton.me"]
[[mailboxes]]
login = "me@gmail.com"
imap_url = "imaps://imap.example.test/INBOX"
receives = ["me@gmail.com"]
[[mailboxes]]
login = "shared@workspace.example.test"
imap_url = "imaps://imap.gmail.com/Archive"
receives = ["*@workspace.example.test"]
"#,
            None,
        )
        .unwrap();
        assert!(managed_google_login(&routed, Some("alias@proton.me")).is_none());
        assert!(managed_google_login(&routed, Some("me@gmail.com")).is_none());
        let (login, imap) =
            managed_google_target(&routed, Some("worker@workspace.example.test")).unwrap();
        assert_eq!(login, "shared@workspace.example.test");
        assert_eq!(imap.mailbox, "Archive");
        assert!(
            managed_google_login(
                &routed.routed_for(Some("alias@proton.me")),
                Some("alias@proton.me")
            )
            .is_none()
        );
    }

    #[test]
    fn expired_wait_never_acquires_a_busy_shared_mailbox() {
        struct UnusedTransport;
        impl aterm_mail::proton::ApiTransport for UnusedTransport {
            fn request(
                &mut self,
                _: aterm_http::Method,
                _: &str,
                _: Option<Value>,
            ) -> Result<aterm_mail::proton::ApiResponse, aterm_mail::proton::ApiError> {
                panic!("expired read reached transport")
            }
            fn renew(&mut self) -> Result<(), aterm_mail::proton::ApiError> {
                panic!("expired read renewed transport")
            }
        }
        let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
            aterm_mail::proton::Proton::with_transport(Box::new(UnusedTransport)),
        ));
        let held = mailbox.lock().unwrap();
        let clock = Instant::now;
        assert_eq!(
            read(&mailbox, Deadline::new(Duration::ZERO, &clock), |_| Ok(())),
            Err(false)
        );
        drop(held);
    }

    #[test]
    fn expired_native_wait_never_connects_or_consumes_a_credential() {
        let clock = Instant::now;
        let expired = Deadline::new(Duration::ZERO, &clock);
        for (webmail_url, imap_url) in [
            (Some("https://mail.proton.me/u/0/inbox".into()), None),
            (None, Some("imaps://imap.gmail.com/INBOX".into())),
        ] {
            let cfg = MailboxConfig {
                webmail_url,
                imap_url,
                since: Some(1),
                ..Default::default()
            };
            let mut seen = BTreeSet::new();
            assert_eq!(
                poll(&cfg, None, Some(expired), false, &mut seen, "expired"),
                Some(Err("expired"))
            );
            assert!(seen.is_empty());
        }
    }

    #[test]
    fn gmail_raw_mime_uses_the_same_freshness_recipient_and_consumption_rules() {
        let cfg = MailboxConfig {
            senders: vec!["anthropic".into()],
            since: Some(1_800_000_000),
            ..Default::default()
        };
        let raw = "From: login@anthropic.com\r\nTo: me@gmail.com\r\nDate: Tue, 01 Jan 2030 00:00:00 +0000\r\nSubject: Verification code\r\nContent-Type: text/plain\r\n\r\nYour verification code is 123456\r\n";
        let mut seen = BTreeSet::new();
        let mut otps = BTreeSet::new();
        assert!(
            matches!(accept_raw(&cfg, Some("me@gmail.com"), raw, &mut seen, &mut otps), Some(MailboxCredential::Otp(code)) if code == "123456")
        );
        assert!(accept_raw(&cfg, Some("me@gmail.com"), raw, &mut seen, &mut otps).is_none());
        assert!(
            accept_raw(
                &cfg,
                Some("other@gmail.com"),
                raw,
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            )
            .is_none()
        );
    }

    #[test]
    fn native_messages_keep_auth_links_and_reject_wrong_sender_recipient_and_old_mail() {
        let cfg = MailboxConfig {
            senders: vec!["anthropic".into()],
            since: Some(1_800_000_000),
            ..Default::default()
        };
        let mut message = aterm_mail::Message {
            id: "a".into(),
            from: "login@anthropic.com".into(),
            to: "me@proton.me".into(),
            subject: "Verification code".into(),
            date: "1800000001".into(),
            unread: true,
            text: "Your verification code is 123456".into(),
        };
        let mut seen = BTreeSet::new();
        let mut otps = BTreeSet::new();
        assert!(
            matches!(accept(&cfg, Some("me@proton.me"), &message, &mut seen, &mut otps), Some(MailboxCredential::Otp(code)) if code == "123456")
        );
        assert!(accept(&cfg, Some("me@proton.me"), &message, &mut seen, &mut otps).is_none());
        for (from, to, date) in [
            ("bad@evil.test", "me@proton.me", "1800000001"),
            ("login@anthropic.com", "other@proton.me", "1800000001"),
            ("login@anthropic.com", "me@proton.me", "1700000000"),
        ] {
            message.from = from.into();
            message.to = to.into();
            message.date = date.into();
            assert!(
                accept(
                    &cfg,
                    Some("me@proton.me"),
                    &message,
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new()
                )
                .is_none()
            );
        }
        message.from = "login@anthropic.com".into();
        message.to = "me@proton.me".into();
        message.date = "1800000001".into();
        message.date = "Tue, 01 Jan 2030 00:00:00 +0000".into();
        assert!(MailFilter::of(&cfg).admits(&envelope(&message)));
        message.text = "<a href=\"https://claude.ai/magic-link?token=secret\">Sign in</a>".into();
        assert!(matches!(
            accept(
                &cfg,
                Some("me@proton.me"),
                &message,
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            ),
            Some(MailboxCredential::MagicLink(_))
        ));
    }

    #[test]
    fn missing_passwords_and_non_gmail_imap_routes_defer_without_dialling() {
        let clock = Instant::now;
        let active = Deadline::new(Duration::from_secs(5), &clock);
        let mut seen = BTreeSet::new();

        // Native Gmail reports missing saved authorization; Proton retains its unready
        // password path. Neither dials without a credential, and the outer poll can still relay.
        for (cfg, expected) in [
            MailboxConfig {
                imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
                user: Some("unit-test-missing-pw@example.invalid".into()),
                password: None,
                relay_enabled: true,
                ..Default::default()
            },
            MailboxConfig {
                imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
                user: Some("unit-test-missing-pw@example.invalid".into()),
                password: Some("   ".into()),
                ..Default::default()
            },
            MailboxConfig {
                webmail_url: Some("https://mail.proton.me/u/0/inbox".into()),
                user: Some("unit-test-missing-pw@example.invalid".into()),
                password: None,
                relay_enabled: true,
                ..Default::default()
            },
            MailboxConfig {
                webmail_url: Some("https://mail.proton.me".into()),
                user: Some("unit-test-missing-pw@example.invalid".into()),
                password: Some("  ".into()),
                ..Default::default()
            },
        ]
        .into_iter()
        .zip([
            Some(Err(GOOGLE_AUTH_MISSING)),
            Some(Err(GOOGLE_AUTH_MISSING)),
            None,
            None,
        ]) {
            assert_eq!(
                poll(
                    &cfg,
                    Some("unit-test-missing-pw@example.invalid"),
                    Some(active),
                    true,
                    &mut seen,
                    "expired"
                ),
                expected
            );
        }

        // A local ProtonMail Bridge or loopback IMAP route must not be preempted by Proton webmail.
        let bridge_cfg = MailboxConfig {
            imap_url: Some("imap://127.0.0.1:1143/INBOX".into()),
            webmail_url: Some("https://mail.proton.me/u/0/inbox".into()),
            user: Some("unit-test-bridge@example.invalid".into()),
            password: Some("bridge-pw".into()),
            ..Default::default()
        };
        assert_eq!(
            poll(
                &bridge_cfg,
                Some("unit-test-bridge@example.invalid"),
                Some(active),
                true,
                &mut seen,
                "expired"
            ),
            None
        );

        assert!(is_proton_webmail_url("https://mail.proton.me"));
        assert!(is_proton_webmail_url("HTTPS://MAIL.PROTON.ME/u/0/inbox"));
        assert!(!is_proton_webmail_url("https://mail.proton.me.evil.test/"));
    }
}
