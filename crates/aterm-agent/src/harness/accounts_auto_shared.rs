// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compose automatic login scheduling with the executable's checked authority.
//! A relay or account proposal never supplies this callback.

use super::super::cli::{
    AccountsHost, RenewBrowser,
    shared_renewal::{Authority, Grant},
};
use super::*;
use std::sync::{Arc, OnceLock};

pub type CheckedAuthority = Arc<dyn Authority + Send + Sync>;
pub type Factory =
    dyn Fn(&Path, accounts::shared::Source, &Row) -> Result<CheckedAuthority, String> + Send + Sync;

static AUTHORITY: OnceLock<Box<Factory>> = OnceLock::new();

/// The composing executable installs its root adapter before starting upkeep.
/// There is no fallback from a refused shared approval to ordinary renewal.
pub fn install_authority(
    factory: impl Fn(&Path, accounts::shared::Source, &Row) -> Result<CheckedAuthority, String>
    + Send
    + Sync
    + 'static,
) {
    let _ = AUTHORITY.set(Box::new(factory));
}

#[derive(Clone, Default)]
pub(super) struct Effects<'a> {
    pub authority: Option<&'a Factory>,
    pub host: Option<Arc<dyn AccountsHost + Send + Sync>>,
}

impl Effects<'static> {
    pub(super) fn live() -> Self {
        Self {
            authority: AUTHORITY.get().map(|f| &**f),
            host: None,
        }
    }
}

struct Automatic {
    inner: CheckedAuthority,
    config: PathBuf,
}

impl Automatic {
    fn check_policy(&self) -> Result<(), String> {
        if sign_ins_on(&self.config) {
            Ok(())
        } else {
            Err("automatic account sign-ins are off".into())
        }
    }
}

impl Authority for Automatic {
    fn prepare(&self, row: &Row) -> Result<Grant, String> {
        self.check_policy()?;
        self.inner.prepare(row)
    }
    fn current(&self, grant: &Grant, row: &Row) -> Result<(), String> {
        self.check_policy()?;
        self.inner.current(grant, row)
    }
}

pub(super) fn automatic(inner: CheckedAuthority, config: PathBuf) -> CheckedAuthority {
    Arc::new(Automatic { inner, config })
}

fn environment(home: &Path, name: &str) -> Option<std::ffi::OsString> {
    if name == "HOME" {
        return Some(home.as_os_str().to_owned());
    }
    std::env::var_os(name).or_else(|| (name == "USER").then(|| std::ffi::OsString::from("aterm")))
}

/// One bounded background provider flow, shared with ordinary automatic renewal.
pub(super) fn sign_in(
    home: &Path,
    staging: &str,
    email: &str,
    browser: &RenewBrowser,
    headless: bool,
    mailbox: Option<&Path>,
) -> Result<
    (
        super::super::align::Captured,
        Option<super::super::open_url::Met>,
    ),
    String,
> {
    let pass_env = super::super::cli::auth_check_env(&|name| environment(home, name))?;
    let prog = resolve_claude_program(home);
    let mut cmd = super::super::cli::renew_command(prog.as_os_str(), staging, email, browser);
    cmd.env("HOME", home)
        .env(super::super::open_url::EMAIL_ENV, email)
        .envs(pass_env);
    for name in [
        super::super::open_url::MAILBOX_ENV,
        super::super::headless_oauth::BROWSER_BIN_ENV,
        super::super::headless_oauth::HEADLESS_ENV,
        super::super::headless_oauth::MAILBOX_URL_ENV,
        super::super::headless_oauth::MAILBOX_TOKEN_ENV,
        super::super::headless_oauth::IMAP_URL_ENV,
        super::super::headless_oauth::IMAP_USER_ENV,
        super::super::headless_oauth::IMAP_PASS_ENV,
        super::super::headless_oauth::WEBMAIL_URL_ENV,
        super::super::headless_oauth::WEBMAIL_PASS_ENV,
        super::super::headless_oauth::TOTP_SECRET_ENV,
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "ATERM_STATE_HOME",
    ] {
        if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
            cmd.env(name, value);
        }
    }
    if std::env::var_os(super::super::open_url::MAILBOX_ENV).is_none_or(|v| v.is_empty())
        && let Some(mailbox) = mailbox
    {
        cmd.env(super::super::open_url::MAILBOX_ENV, mailbox);
    }
    if headless {
        cmd.env(super::super::headless_oauth::HEADLESS_ENV, "1");
    }
    let outcome = aterm_tempfile::tempdir()
        .ok()
        .map(|dir| (dir.path().join("outcome"), dir));
    if let Some((path, _)) = &outcome {
        cmd.env(super::super::open_url::OUTCOME_ENV, path);
    }
    let ran = super::super::align::capture_bounded(cmd, Duration::from_secs(300), 64 * 1024)?;
    let met = outcome
        .as_ref()
        .and_then(|(path, _)| super::super::open_url::read_outcome(path));
    Ok((ran, met))
}

pub(super) struct BackgroundHost<'a> {
    pub home: &'a Path,
    pub mailbox: Option<&'a Path>,
    pub headless: bool,
}

impl AccountsHost for BackgroundHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
        Err("background shared renewal checks only its staging login".into())
    }
    fn unaskable(&self) -> Option<String> {
        super::super::cli::auth_check_env(&|name| environment(self.home, name)).err()
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("background shared renewal requires its approved named flow".into())
    }
    fn now(&self) -> i64 {
        crate::supervise::limit::unix_now()
    }
    fn renew_flow(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<i32, String> {
        sign_in(
            self.home,
            staging,
            email,
            browser,
            self.headless,
            self.mailbox,
        )?
        .0
        .code
        .ok_or_else(|| "background provider flow ended without an exit code".into())
    }
    fn status_in_home(&self, staging: &str) -> Result<super::super::align::Captured, String> {
        super::super::cli::status_in_home_within(
            resolve_claude_program(self.home).as_os_str(),
            staging,
            &|name| environment(self.home, name),
            super::super::cli::AUTH_CHECK_BUDGET,
        )
    }
    fn claude_principal_in_home(&self, staging: &str) -> Option<[u8; 32]> {
        super::super::profile::principal_in_home(Path::new(staging))
    }

    fn claude_identity_in_home(
        &self,
        staging: &str,
        email: &str,
    ) -> super::super::profile::IdentityObservation {
        super::super::profile::identity_in_home(Path::new(staging), email)
    }
}
