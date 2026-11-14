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

/// The login source a saved Google grant signs in on ([`run_gmail`]): a refusal there is held
/// against the account's grant, never against an IMAP address.
const GOOGLE_OAUTH_SOURCE: &str = "google-mail-oauth";

/// `account`'s saved Google mail session in `home`, as [`poll`] loads it: the grant in that
/// home's aterm mail folder, decrypted with that home's machine key, with no network request and
/// no renewal lease. `None` where no grant is saved; an error where one is saved and does not
/// load. Nothing of it is logged.
pub(super) fn saved_google_session(
    home: &Path,
    account: &str,
) -> Result<Option<aterm_mail::oauth::session::Shared>, String> {
    aterm_mail::oauth::session::shared(home, account)
}

/// What [`poll`] does with `target`'s mail, decided before it reads anything: [`poll`] acts on
/// it, and [`MailboxConfig::reads_without_browser`] judges by it, so whether the mailbox is
/// read and how are one decision. `S` is the saved Google session the loader gave.
pub(super) enum Plan<S> {
    /// [`poll`] answers nothing: the mailbox's other readers (its folder, IMAP, HTTP mailbox
    /// or the fleet's relay) read it.
    Defer,
    /// Every poll ends with this, before anything is read: a saved grant that does not load,
    /// or a Gmail route with neither a grant nor an app password.
    Refused(&'static str),
    /// Gmail, through the account's saved Google grant.
    Google {
        account: String,
        imap: ParsedImapUrl,
        session: S,
    },
    /// Gmail over IMAP, signed in to with a password.
    GmailImap {
        imap: ParsedImapUrl,
        login: String,
        password: String,
    },
    /// Proton's own API, signed in to with the mailbox's login and password.
    Proton { login: String, password: String },
}

/// [`Plan`] for `target`'s mail in `source_cfg`, the saved Google grant loaded by `load` (an
/// error: one is saved and does not load). A grant is asked for only where the route is an
/// admitted native Google one ([`managed_google_target`]).
pub(super) fn plan<S>(
    source_cfg: &MailboxConfig,
    target: Option<&str>,
    load: impl FnOnce(&str) -> Result<Option<S>, String>,
) -> Plan<S> {
    let routed = source_cfg.routed_for(target);
    let cfg = &*routed;
    if cfg.dir.is_some() || cfg.http_url.is_some() {
        return Plan::Defer;
    }
    let explicit_imap = cfg.effective_imap_url(target);
    if let Some(url) = &explicit_imap {
        let Some(imap) = parse_imap_url(url) else {
            return Plan::Defer;
        };
        if !imap.tls || !imap.host.eq_ignore_ascii_case("imap.gmail.com") {
            return Plan::Defer;
        }
    }
    match saved_google(source_cfg, target, load) {
        Ok(Some((account, imap, session))) => {
            return Plan::Google {
                account,
                imap,
                session,
            };
        }
        Err(why) => return Plan::Refused(why),
        Ok(None) => {}
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
        return Plan::Refused(GOOGLE_AUTH_MISSING);
    }
    if let Some(url) = explicit_imap {
        let Some(imap) = parse_imap_url(&url) else {
            return Plan::Defer;
        };
        let Some(login) = imap
            .user
            .clone()
            .or_else(|| cfg.webmail_user_for(target))
            .filter(|u| !u.trim().is_empty())
        else {
            return Plan::Defer;
        };
        let Some(password) = imap
            .password
            .clone()
            .or_else(|| cfg.password_for(target))
            .map(|pw| {
                pw.chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
            })
            .filter(|pw| !pw.is_empty())
        else {
            return Plan::Defer;
        };
        return Plan::GmailImap {
            imap,
            login,
            password,
        };
    }
    let Some(url) = cfg.effective_webmail_url(target) else {
        return Plan::Defer;
    };
    if !is_proton_webmail_url(&url) {
        return Plan::Defer;
    }
    let (Some(login), Some(password)) = (
        cfg.webmail_user_for(target)
            .filter(|u| !u.trim().is_empty()),
        cfg.password_for(target).filter(|pw| !pw.trim().is_empty()),
    ) else {
        return Plan::Defer;
    };
    Plan::Proton { login, password }
}

impl<S> Plan<S> {
    /// Whether a poll that took this plan reads `target`'s mail on this machine with no
    /// browser, as `cfg` (its routed view) holds it: `None` where [`poll`] defers to the
    /// mailbox's other readers. A login this Mac holds as refused on its source
    /// ([`super::super::mailbox_routes::login_held`]) is not tried, as [`run_gmail`] and the
    /// Proton reader decide, so it reads nothing until its wait ends; Proton's API is counted
    /// on macOS alone.
    pub(super) fn reads(&self, cfg: &MailboxConfig, target: Option<&str>) -> Option<bool> {
        let held = |source: &str, login: &str| {
            super::super::mailbox_routes::login_held(
                cfg.login_record.as_deref(),
                source,
                login,
                super::super::mailbox_routes::unix_now(),
            )
            .is_some()
        };
        match self {
            Self::Defer => None,
            Self::Refused(_) => Some(false),
            Self::Google { account, .. } => Some(!held(GOOGLE_OAUTH_SOURCE, account)),
            Self::GmailImap { login, .. } => {
                Some(cfg.effective_imap_url(target).is_some_and(|url| {
                    !held(&super::super::mailbox_routes::login_source(&url), login)
                }))
            }
            Self::Proton { login, .. } => Some(
                cfg!(target_os = "macos")
                    && cfg.effective_webmail_url(target).is_some_and(|url| {
                        !held(&super::super::mailbox_routes::login_source(&url), login)
                    }),
            ),
        }
    }
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
    let expired = |error: &'static str| {
        if deadline.expired() || error == super::super::auth_deadline::EXPIRED {
            expiry_words
        } else {
            error
        }
    };
    let home = source_cfg.grant_home();
    let plan = plan(source_cfg, target, |account| {
        home.as_deref()
            .map_or(Ok(None), |home| saved_google_session(home, account))
    });
    match plan {
        Plan::Defer => None,
        Plan::Refused(why) => Some(Err(why)),
        Plan::Google {
            account,
            imap,
            session,
        } => Some(
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
            .map_err(expired),
        ),
        Plan::GmailImap {
            imap,
            login,
            password,
        } => {
            if deadline.left().is_err() {
                return Some(Err(expiry_words));
            }
            Some(
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
                .map_err(expired),
            )
        }
        Plan::Proton { login, password } => {
            if deadline.left().is_err() {
                return Some(Err(expiry_words));
            }
            Some(
                run(
                    source_cfg, cfg, target, deadline, once, seen_otps, login, password,
                )
                .map_err(expired),
            )
        }
    }
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
        GOOGLE_OAUTH_SOURCE.to_owned()
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
#[path = "headless_native_mail_tests.rs"]
mod tests;
