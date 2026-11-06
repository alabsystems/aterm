// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Browser approval of a native agy Google request. Native agy retains PKCE and token exchange.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use super::agy_signin::{Authorization, Code};
use super::google_browser::{self, MemoryBrowser};
use super::google_credentials::{self, Credentials};
use super::google_federation::{self, Authenticator};
use super::google_identity::Identity;
use super::headless_oauth::{self as oauth, MailboxConfig};
use super::wake_deadline::WakeDeadline;
use aterm_secrets::SecretBytes;

/// Bounded, redacted approval failures; no request, account address or code is included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Deadline,
    Configuration,
    Credentials(google_credentials::Error),
    Browser(google_browser::Error),
    Redirect,
    Callback,
    SecurityCheck,
    Verification,
    Rejected,
    Mail(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Credentials(error) => return std::fmt::Display::fmt(error, f),
            Self::Browser(error) => return std::fmt::Display::fmt(error, f),
            Self::Mail(why) => why,
            Self::Deadline => "Google approval exceeded its authorization deadline",
            Self::Configuration => "Google identity binding changed during authorization",
            Self::Redirect => "Google approval left its admitted native flow",
            Self::Callback => "Google callback did not match the native request",
            Self::SecurityCheck => "Google sign-in requires a security check",
            Self::Verification => {
                "Google sign-in requires an unsupported or unconfigured verification step"
            }
            Self::Rejected => "Google rejected the submitted credential or verification code",
        })
    }
}

pub(super) struct Context<'a> {
    pub work: Option<super::account_work::Scope>,
    pub identity: &'a Identity,
    pub home: &'a Path,
    pub state: &'a Path,
    pub mailbox: Option<&'a MailboxConfig>,
    pub current: &'a dyn Fn() -> bool,
}

pub(super) fn approve(
    request: &Authorization,
    ctx: &Context<'_>,
    deadline: WakeDeadline,
) -> Result<Code, Error> {
    if !(ctx.current)() {
        return Err(Error::Configuration);
    }
    let mut credentials: Option<Credentials> = None;
    let mut browser = MemoryBrowser::start_scoped(
        ctx.identity,
        ctx.state,
        Some(ctx.home),
        deadline,
        ctx.work.clone(),
    )
    .map_err(Error::Browser)?;
    match browser.restore_session(ctx.home) {
        Ok(_) | Err(google_browser::Error::Session) => {}
        Err(error) => return Err(Error::Browser(error)),
    }
    let page = browser.open(request.url()).map_err(Error::Browser)?;
    let mut identifier_sent = false;
    let mut password_sent = false;
    let mut totp_sent = false;
    let mut mail_sent = false;
    let mut mail_code: Option<SecretBytes> = None;
    let mut seen = BTreeSet::new();
    let started = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok());
    loop {
        if deadline.passed() {
            return Err(Error::Deadline);
        }
        if !(ctx.current)() {
            return Err(Error::Configuration);
        }
        if let Some(credentials) = credentials.as_mut() {
            credentials.check_current().map_err(Error::Credentials)?;
        }
        for url in browser.navigation_commits(&page).map_err(Error::Browser)? {
            if url.split(['?', '#']).next() == Some("https://antigravity.google/oauth-callback") {
                let code = request.callback(&url).map_err(|_| Error::Callback)?;
                if !(ctx.current)() {
                    return Err(Error::Configuration);
                }
                let session = browser
                    .into_pending_session(ctx.home)
                    .map_err(Error::Browser)?;
                return Ok(code.with_session(session));
            }
        }
        let view = match browser.evaluate(&page, oauth::VENDOR_PAGE_JS, false) {
            Ok(value) => oauth::page_of(&value),
            Err(error) => {
                // Navigation can replace the execution context before its callback event is
                // drained. Retry only while the owned target and browser still exist.
                let targets = browser.pages().map_err(Error::Browser)?;
                if !targets
                    .iter()
                    .any(|target| target.target() == page.target())
                {
                    return Err(Error::Browser(error));
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        // The committed navigation, not a scrubbed address-bar URL, supplies callback state.
        if view.url.split(['?', '#']).next() == Some("https://antigravity.google/oauth-callback")
            || view.url == "about:blank"
        {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if !request.admits_google(&view.url) {
            return Err(Error::Redirect);
        }
        if oauth::bot_check(&view) {
            return Err(Error::SecurityCheck);
        }
        let mut act = |password: Option<&str>, totp: Option<&str>| {
            let otp = totp.and_then(|secret| {
                aterm_time::unix_secs().and_then(|now| oauth::generate_totp_at(secret, now))
            });
            let authenticator = if totp_sent {
                Authenticator::Submitted
            } else {
                otp.as_deref()
                    .map(Authenticator::Ready)
                    .unwrap_or(Authenticator::Unavailable)
            };
            let script = SecretBytes::from(google_federation::google_action(
                ctx.identity.email(),
                password,
                authenticator,
                identifier_sent,
                password_sent,
                ctx.identity
                    .verification_email()
                    .map(|target| (target, mail_code.as_ref().and_then(SecretBytes::as_str))),
                mail_sent,
            ));
            browser.evaluate(&page, script.as_str().unwrap_or_default(), true)
        };
        let result = match credentials.as_mut() {
            Some(credentials) => credentials
                .with_google_page(&view.url, |password, totp| act(Some(password), totp))
                .map_err(Error::Credentials)?,
            None => act(None, None),
        }
        .map_err(Error::Browser)?;
        match result.as_str() {
            Some("credentials") => {
                credentials =
                    Some(Credentials::load(ctx.identity, ctx.home).map_err(Error::Credentials)?);
            }
            Some("identifier") => identifier_sent = true,
            Some("password") => password_sent = true,
            Some("totp") => totp_sent = true,
            // The callback captures the final cookies; canonical native verification commits them.
            Some("consent") => {}
            Some("mailbox") => {
                let code = google_federation::verification_mail_from(
                    ctx.identity,
                    Some(ctx.home),
                    ctx.mailbox,
                    started,
                    deadline,
                    &mut seen,
                )
                .map_err(Error::Mail)?;
                mail_code = Some(SecretBytes::from(code));
            }
            Some("mailcode") => {
                mail_sent = true;
                mail_code = None;
            }
            Some("mailcode_rejected" | "totp_rejected" | "refused") => return Err(Error::Rejected),
            Some("mailbox_unconfigured" | "mailbox_refused" | "challenge") => {
                return Err(Error::Verification);
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
