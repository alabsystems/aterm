// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Native mail views with serial foreground commands and cancellable background observation.

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, Command, CommonViewState, DamageRegion, EventResult, NativeAppModel,
    SemanticInput, TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::TextInputState;
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, TextFieldSpec, TextSpec, UiNode, UiTree,
};
use aterm_mail::{Authentication, Draft, Endpoint, Folder, Mailbox, Message, Secret};
use std::sync::mpsc;

mod watch;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Provider {
    #[default]
    Proton,
    Gmail,
    Imap,
}

enum Work {
    #[cfg_attr(
        test,
        expect(
            dead_code,
            reason = "Credential discovery is disabled in test fixtures"
        )
    )]
    DiscoverStored,
    Observed {
        epoch: u64,
        account: usize,
        folder: String,
        result: Result<Vec<Message>, String>,
    },
    Watch {
        account: usize,
        folder: String,
        baseline: Vec<Message>,
    },
    Stored(String),
    Google(String),
    Connect {
        provider: Provider,
        email: String,
        login: String,
        password: Secret,
        mailbox_password: Option<Secret>,
        code: Secret,
        totp_secret: Option<Secret>,
        host: String,
        smtp: String,
    },
    List {
        account: usize,
        folder: String,
    },
    Read {
        account: usize,
        folder: String,
        id: String,
    },
    Send {
        account: usize,
        draft: Draft,
        draft_revision: u64,
    },
}
enum Reply {
    Available(Vec<String>),
    Progress(String),
    Connected {
        account: usize,
        email: String,
        folders: Vec<Folder>,
        folder: String,
        messages: Vec<Message>,
    },
    Listed {
        folder: String,
        messages: Vec<Message>,
    },
    Read(Message),
    Synced {
        account: usize,
        folder: String,
        messages: Vec<Message>,
    },
    SyncWaiting {
        account: usize,
        reason: String,
    },
    Sent(u64),
    Error(String),
}
enum Account {
    Proton(std::sync::Arc<std::sync::Mutex<aterm_mail::proton::Proton>>),
    Standard(std::sync::Arc<std::sync::Mutex<StandardAccount>>),
}
struct StandardAccount {
    imap: aterm_mail::imap::Imap,
    smtp: Endpoint,
    refresh: Option<aterm_mail::oauth::session::Shared>,
    generation: u64,
}
impl Account {
    fn refresh(&mut self) -> Result<(), String> {
        if let Self::Proton(proton) = self {
            proton
                .lock()
                .map_err(|_| "Mail reader lock failed")?
                .refresh_if_needed()?;
        }
        if let Self::Standard(account) = self {
            account
                .lock()
                .map_err(|_| "Mail account lock failed")?
                .refresh()
                .map_err(String::from)?;
        }
        Ok(())
    }
}
impl StandardAccount {
    fn refresh(&mut self) -> Result<(), aterm_mail::oauth::Error> {
        if let Some(shared) = &self.refresh {
            let mut session = shared.lock().map_err(|_| aterm_mail::oauth::Error {
                kind: aterm_mail::oauth::ErrorKind::Storage,
            })?;
            session.renew()?;
            if session.generation() == self.generation {
                return Ok(());
            }
            self.imap.authentication =
                session.authentication().ok_or(aterm_mail::oauth::Error {
                    kind: aterm_mail::oauth::ErrorKind::InvalidResponse,
                })?;
            self.generation = session.generation();
        }
        Ok(())
    }
}

pub(crate) struct MailApp {
    work: mpsc::SyncSender<(u64, Work)>,
    _watcher: watch::Watcher,
    replies: mpsc::Receiver<(u64, Reply)>,
    revision: u64,
    pending: Option<u64>,
    stored: std::collections::VecDeque<String>,
    accounts: Vec<(usize, String, Vec<Folder>)>,
    account: Option<usize>,
    folders: Vec<Folder>,
    folder: String,
    messages: Vec<Message>,
    message: Option<Message>,
    notice: String,
    leave_setup: bool,
    stale_watch: bool,
    draft_dirty: bool,
    draft_revision: u64,
    draft_from: Option<(usize, String)>,
    to: TextInputState,
    subject: TextInputState,
    body: TextInputState,
}

impl MailApp {
    pub(crate) fn field_input<'a>(
        &'a self,
        view: &'a MailViewState,
        key: &str,
    ) -> Option<&'a TextInputState> {
        match key {
            "mail/to" => Some(&self.to),
            "mail/subject" => Some(&self.subject),
            "mail/body" => Some(&self.body),
            "mail/email" => Some(&view.email),
            "mail/password" => Some(&view.password),
            "mail/code" => Some(&view.code),
            "mail/host" => Some(&view.host),
            "mail/smtp" => Some(&view.smtp),
            "mail/search" => Some(&view.search),
            _ => None,
        }
    }
    pub(crate) fn cancel_preedit(
        &mut self,
        view: &mut MailViewState,
        field: Option<&crate::native_ui::UiKey>,
    ) -> bool {
        let Some(key) = field
            .or(view.common.last_focus.as_ref())
            .map(|key| key.as_str().to_owned())
        else {
            return false;
        };
        let input = match key.as_str() {
            "mail/to" => Some(&mut self.to),
            "mail/subject" => Some(&mut self.subject),
            "mail/body" => Some(&mut self.body),
            _ => view.input(&key),
        };
        let Some(input) = input else {
            return false;
        };
        let changed = input.preedit().is_some();
        input.cancel_preedit();
        changed
    }
    pub(crate) fn connected(&self) -> bool {
        self.account.is_some()
    }
    pub(crate) fn take_leave_setup(&mut self) -> bool {
        std::mem::take(&mut self.leave_setup)
    }
    pub(crate) fn new(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        // Restore and host fixtures instantiate this model too. Only the real
        // application may discover credentials and start network connections.
        #[allow(unused_mut)]
        let mut app = Self::with_stored(wake, Default::default())?;
        #[cfg(not(test))]
        app.request(Work::DiscoverStored);
        Ok(app)
    }
    fn with_stored(
        wake: impl Fn() + Send + 'static,
        stored: std::collections::VecDeque<String>,
    ) -> Result<Self, String> {
        let (work_tx, work_rx) = mpsc::sync_channel::<(u64, Work)>(8);
        let (reply_tx, reply_rx) = mpsc::sync_channel(8);
        let watcher = watch::Watcher::start(work_tx.clone())?;
        let control = watcher.control();
        crate::qos::spawn_lane_thread("aterm-mail", crate::qos::Role::Background, move || {
            let mut accounts: Vec<(String, Account)> = Vec::new();
            let mut watched: Option<(usize, String)> = None;
            let mut last_messages = Vec::new();
            let mut failures = 0u32;
            let mut last_error = String::new();
            while let Ok((revision, work)) = work_rx.recv() {
                if let Work::Observed {
                    epoch,
                    account,
                    folder,
                    result,
                } = work
                {
                    if !control.current(epoch)
                        || watched.as_ref() != Some(&(account, folder.clone()))
                    {
                        continue;
                    }
                    let update = match result {
                        Ok(messages) => {
                            let changed = messages != last_messages || failures != 0;
                            failures = 0;
                            last_error.clear();
                            last_messages = messages.clone();
                            changed.then_some(Reply::Synced {
                                account,
                                folder,
                                messages,
                            })
                        }
                        Err(reason) => {
                            failures = failures.saturating_add(1);
                            if reason == last_error {
                                None
                            } else {
                                last_error = reason.clone();
                                Some(Reply::SyncWaiting { account, reason })
                            }
                        }
                    };
                    if let Some(update) = update {
                        if reply_tx.send((0, update)).is_err() {
                            break;
                        }
                        wake();
                    }
                    continue;
                }
                if let Work::Watch {
                    account,
                    folder,
                    baseline,
                } = work
                {
                    if watched.as_ref() != Some(&(account, folder.clone()))
                        && let Some((_, entry)) = accounts.get(account)
                    {
                        watched = Some((account, folder.clone()));
                        last_messages = baseline.clone();
                        failures = 0;
                        last_error.clear();
                        control.watch(account, folder, entry, baseline);
                    }
                    continue;
                }
                let target = match &work {
                    Work::List { account, folder }
                    | Work::Read {
                        account, folder, ..
                    } => Some((*account, folder.clone())),
                    _ => None,
                };
                let mut verify = |url| {
                    let _ = reply_tx.send((revision, Reply::Progress(url)));
                    wake();
                };
                let result = perform(work, &mut accounts, &mut verify).unwrap_or_else(Reply::Error);
                let mut change = false;
                match &result {
                    Reply::Connected {
                        account,
                        folder,
                        messages,
                        ..
                    } => {
                        watched = Some((*account, folder.clone()));
                        last_messages = messages.clone();
                        failures = 0;
                        change = true;
                    }
                    Reply::Listed { messages, .. } => {
                        watched = target;
                        last_messages = messages.clone();
                        failures = 0;
                        change = true;
                    }
                    Reply::Read(_) if watched != target => {
                        watched = target;
                        change = true;
                    }
                    _ => {}
                }
                if change {
                    last_error.clear();
                }
                if change && let Some((index, folder)) = &watched {
                    control.watch(
                        *index,
                        folder.clone(),
                        &accounts[*index].1,
                        last_messages.clone(),
                    );
                }
                if reply_tx.send((revision, result)).is_err() {
                    break;
                }
                wake();
            }
        })
        .map_err(|e| e.to_string())?;
        let mut app = Self {
            work: work_tx,
            _watcher: watcher,
            replies: reply_rx,
            revision: 0,
            pending: None,
            stored,
            accounts: Vec::new(),
            account: None,
            folders: Vec::new(),
            folder: String::new(),
            messages: Vec::new(),
            message: None,
            notice: "Add an account to get started".into(),
            leave_setup: false,
            stale_watch: false,
            draft_dirty: false,
            draft_revision: 0,
            draft_from: None,
            to: TextInputState::new(String::new()),
            subject: TextInputState::new(String::new()),
            body: TextInputState::new(String::new()),
        };
        app.connect_next_stored();
        Ok(app)
    }

    fn connect_next_stored(&mut self) {
        if self.pending.is_none()
            && let Some(email) = self.stored.pop_front()
        {
            self.request(Work::Stored(email));
        }
    }

    fn request(&mut self, work: Work) {
        self.revision = self.revision.wrapping_add(1);
        match self.work.try_send((self.revision, work)) {
            Ok(()) => {
                self.pending = Some(self.revision);
                self.notice = "Connecting…".into();
            }
            Err(_) => self.notice = "Mail worker is unavailable; reopen Mail to reconnect".into(),
        }
    }

    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((revision, reply)) = self.replies.try_recv() {
            changed = true;
            let reply = match reply {
                Reply::Available(accounts) => {
                    for email in accounts {
                        let already_queued = self
                            .stored
                            .iter()
                            .any(|queued| queued.eq_ignore_ascii_case(&email));
                        let already_connected = self
                            .accounts
                            .iter()
                            .any(|(_, connected, _)| connected.eq_ignore_ascii_case(&email));
                        if !already_queued && !already_connected {
                            self.stored.push_back(email);
                        }
                    }
                    if self.pending == Some(revision) {
                        self.pending = None;
                        if self.stored.is_empty() && self.accounts.is_empty() {
                            self.notice = "Add an account to get started".into();
                        }
                    }
                    continue;
                }
                Reply::Synced {
                    account,
                    folder,
                    messages,
                } => {
                    if self.account == Some(account) && self.folder == folder {
                        self.messages = messages;
                        if self.notice.starts_with("Sync waiting:") {
                            self.notice.clear();
                        }
                    }
                    continue;
                }
                Reply::SyncWaiting { account, reason } => {
                    if self.account == Some(account) && self.pending.is_none() {
                        self.notice = format!("Sync waiting: {reason}");
                    }
                    continue;
                }
                reply => reply,
            };
            if let Reply::Connected {
                account,
                email,
                folders,
                ..
            } = &reply
            {
                if let Some(existing) = self.accounts.iter_mut().find(|(id, existing_email, _)| {
                    *id == *account || existing_email.eq_ignore_ascii_case(email)
                }) {
                    *existing = (*account, email.clone(), folders.clone());
                } else {
                    self.accounts
                        .push((*account, email.clone(), folders.clone()));
                }
            }
            if self.pending != Some(revision) {
                if matches!(
                    reply,
                    Reply::Connected { .. } | Reply::Listed { .. } | Reply::Read(_)
                ) {
                    self.stale_watch = true;
                }
                continue;
            }
            if let Reply::Progress(url) = reply {
                self.notice = url;
                continue;
            }
            self.pending = None;
            match reply {
                Reply::Connected {
                    account,
                    email,
                    folders,
                    folder,
                    messages,
                } => {
                    let _ = email;
                    self.account = Some(account);
                    self.folders = folders;
                    self.folder = folder;
                    self.messages = messages;
                    self.message = None;
                    self.leave_setup = true;
                    self.stale_watch = false;
                    self.notice = "Connected".into();
                }
                Reply::Listed { folder, messages } => {
                    self.folder = folder;
                    self.messages = messages;
                    self.message = None;
                    self.stale_watch = false;
                    self.notice = "Up to date".into();
                }
                Reply::Read(message) => {
                    self.message = Some(message);
                    self.stale_watch = false;
                    self.notice.clear();
                }
                Reply::Sent(revision) => {
                    self.notice = "Message accepted by the mail server".into();
                    if revision == self.draft_revision {
                        self.draft_dirty = false;
                    }
                }
                Reply::Error(message) => self.notice = message,
                Reply::Available(_)
                | Reply::Progress(_)
                | Reply::Synced { .. }
                | Reply::SyncWaiting { .. } => {
                    unreachable!()
                }
            }
        }
        self.connect_next_stored();
        if self.stale_watch && self.pending.is_none() {
            if let Some(account) = self.account
                && !self.folder.is_empty()
            {
                if self
                    .work
                    .try_send((
                        0,
                        Work::Watch {
                            account,
                            folder: self.folder.clone(),
                            baseline: self.messages.clone(),
                        },
                    ))
                    .is_ok()
                {
                    self.stale_watch = false;
                }
            } else {
                self.stale_watch = false;
            }
        }
        changed
    }
}

fn dedup_accounts(found: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut by_key = std::collections::BTreeMap::new();
    for email in found {
        let trimmed = email.trim();
        if !trimmed.is_empty() {
            by_key
                .entry(trimmed.to_ascii_lowercase())
                .or_insert_with(|| trimmed.to_owned());
        }
    }
    by_key.into_values().collect()
}

fn stored_provider(email: &str) -> Option<Provider> {
    match email.rsplit_once('@')?.1.to_ascii_lowercase().as_str() {
        "proton.me" | "protonmail.com" | "protonmail.ch" | "pm.me" => Some(Provider::Proton),
        "gmail.com" | "googlemail.com" => Some(Provider::Gmail),
        _ => None,
    }
}

fn retry_interval(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs(
        2u64.saturating_mul(1u64 << failures.saturating_sub(1).min(9))
            .min(900),
    )
}

fn verify_proton(
    challenge: &aterm_mail::human::Challenge,
    notify: &mut impl FnMut(String),
) -> Result<aterm_mail::human::Verified, String> {
    let recovery = aterm_mail::human::Recovery::prepare(challenge)?;
    if !recovery.is_email() {
        return Err("No configured unattended SMS recovery route".into());
    }
    let config = aterm_agent::harness::mail_recovery::configured()?;
    if aterm_agent::harness::mail_recovery::candidates(&config, &recovery.destination).is_empty() {
        return Err("No stored mailbox route matches Proton's recovery address".into());
    }
    notify("Retrieving Proton verification from the configured recovery mailbox".into());
    let since = aterm_time::unix_secs().ok_or("System clock is invalid")?;
    recovery.request_code()?;
    let code = aterm_agent::harness::mail_recovery::code(&config, &recovery.destination, since)?;
    recovery.complete(&Secret::new(code))
}

fn read_with_verification<T>(
    account: &mut Account,
    mut operation: impl FnMut(&mut dyn Mailbox) -> Result<T, String>,
    notify: &mut impl FnMut(String),
) -> Result<T, String> {
    match account {
        Account::Proton(proton) => {
            let mut proton = proton.lock().map_err(|_| "Mail reader lock failed")?;
            let external = proton.externally_authenticated();
            proton.with_verification(
                |proton| operation(proton),
                |challenge| {
                    if external {
                        Err("The authenticated mail session requires verification".into())
                    } else {
                        verify_proton(challenge, notify)
                    }
                },
            )
        }
        Account::Standard(account) => {
            let mut account = account.lock().map_err(|_| "Mail account lock failed")?;
            operation(&mut account.imap)
        }
    }
}

#[cfg(unix)]
fn connect_browser(
    home: &std::path::Path,
    credentials: aterm_agent::harness::mail_browser::Credentials<'_>,
    notify: &mut impl FnMut(String),
) -> Result<Account, String> {
    notify("Connecting to Proton Mail…".into());
    aterm_agent::harness::mail_browser::shared_mailbox(home, credentials).map(Account::Proton)
}

fn permits_browser_fallback(kind: aterm_mail::proton_error::Kind) -> bool {
    use aterm_mail::proton_error::Kind;
    matches!(
        kind,
        Kind::Verification | Kind::DeviceVerification | Kind::Scope | Kind::ClientRejected
    )
}

fn failed_proton_connection(proton: &aterm_mail::proton::Proton, error: String) -> String {
    match proton.sign_out_if_temporary() {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; temporary session revocation also failed: {cleanup}"),
    }
}

fn perform(
    work: Work,
    accounts: &mut Vec<(String, Account)>,
    verify: &mut impl FnMut(String),
) -> Result<Reply, String> {
    match work {
        Work::DiscoverStored => {
            let mut found = aterm_agent::harness::mail_recovery::configured()
                .map(|config| aterm_agent::harness::mail_recovery::accounts(&config))
                .unwrap_or_default();
            found.retain(|email| stored_provider(email).is_some());
            if let Some(home) = aterm_types::dirs::home_dir() {
                found.extend(aterm_mail::oauth::session::accounts(&home));
            }
            Ok(Reply::Available(dedup_accounts(found)))
        }
        Work::Observed { .. } | Work::Watch { .. } => {
            Err("Background observation reached the command handler".into())
        }
        Work::Google(email) => {
            let email = email.trim().to_ascii_lowercase();
            let home = aterm_types::dirs::home_dir().ok_or("Home directory unavailable")?;
            aterm_agent::harness::email_cli::connect_google_mail(
                &home,
                &email,
                std::time::Duration::from_secs(180),
            )?;
            perform(Work::Stored(email), accounts, verify)
        }
        Work::Stored(email) => {
            if let Some(home) = aterm_types::dirs::home_dir()
                && let Some(session) = aterm_mail::oauth::session::shared(&home, &email)?
            {
                let account = Account::Standard(std::sync::Arc::new(std::sync::Mutex::new(
                    StandardAccount {
                        imap: aterm_mail::imap::Imap {
                            endpoint: Endpoint::parse("imap.gmail.com", 993)?,
                            authentication: Authentication::OAuth {
                                username: email.clone(),
                                access_token: Secret::new(String::new()),
                            },
                        },
                        smtp: Endpoint::parse("smtp.gmail.com", 465)?,
                        refresh: Some(session),
                        generation: 0,
                    },
                )));
                return finish_connect(email, account, accounts, verify);
            }
            let config = aterm_agent::harness::mail_recovery::configured()?;
            let provider = stored_provider(&email).ok_or("Stored account needs server settings")?;
            let password = Secret::new(
                config
                    .password_for(Some(&email))
                    .ok_or("Stored account has no password")?,
            );
            let mailbox_password = config.mailbox_password_for(Some(&email)).map(Secret::new);
            let totp_secret = config.totp_secret_for(Some(&email)).map(Secret::new);
            perform(
                Work::Connect {
                    provider,
                    login: config
                        .webmail_user_for(Some(&email))
                        .ok_or("Stored account has no login")?,
                    email,
                    password,
                    mailbox_password,
                    code: Secret::new(String::new()),
                    totp_secret,
                    host: String::new(),
                    smtp: String::new(),
                },
                accounts,
                verify,
            )
        }
        Work::Connect {
            provider,
            email,
            login,
            password,
            mailbox_password,
            code,
            totp_secret,
            host,
            smtp,
        } => {
            let password = if provider == Provider::Gmail {
                Secret::new(
                    password
                        .expose()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect(),
                )
            } else {
                password
            };
            let account = if provider == Provider::Proton {
                let home = aterm_types::dirs::home_dir()
                    .ok_or("Home directory unavailable for mail session")?;
                #[cfg(unix)]
                let browser_credentials = || aterm_agent::harness::mail_browser::Credentials {
                    email: &email,
                    login: &login,
                    password: password.expose(),
                    mailbox_password: mailbox_password.as_ref().map(Secret::expose),
                    totp_secret: totp_secret.as_ref().map(Secret::expose),
                };
                #[cfg(unix)]
                let saved_browser =
                    aterm_agent::harness::mail_browser::HeadlessMail::has_session(&home, &email);
                #[cfg(not(unix))]
                let saved_browser = false;
                if saved_browser {
                    #[cfg(unix)]
                    {
                        connect_browser(&home, browser_credentials(), verify)?
                    }
                    #[cfg(not(unix))]
                    {
                        unreachable!()
                    }
                } else {
                    let mut fallback_needed = false;
                    let api = (|| -> Result<Account, String> {
                        let lease = aterm_mail::session::acquire(&home, &email)?;
                        let saved = aterm_mail::session::load(&home, &email)?;
                        let resumed = match saved {
                            Some(credentials) => {
                                aterm_mail::proton::Proton::resume(credentials, |challenge| {
                                    verify_proton(challenge, verify)
                                        .inspect_err(|_| fallback_needed = true)
                                })?
                            }
                            None => None,
                        };
                        let was_resumed = resumed.is_some();
                        let mut proton = match resumed {
                            Some(proton) => proton,
                            None => aterm_mail::proton::Proton::sign_in_with_verifier(
                                &login,
                                &password,
                                |challenge| {
                                    verify_proton(challenge, verify)
                                        .inspect_err(|_| fallback_needed = true)
                                },
                            )?,
                        };
                        if was_resumed {
                            // Rotation must reach disk before slower mailbox setup begins.
                            proton.persist(&home, &email)?;
                        }
                        let mut prepare = |proton: &mut aterm_mail::proton::Proton| {
                            if proton.needs_two_factor {
                                let fresh_code = totp_secret
                                    .as_ref()
                                    .map(|secret| {
                                        aterm_agent::harness::headless_oauth::generate_totp_at(
                                            secret.expose(),
                                            aterm_time::unix_secs()
                                                .ok_or("Invalid system clock")?,
                                        )
                                        .map(Secret::new)
                                        .ok_or("Invalid stored TOTP secret")
                                    })
                                    .transpose()?;
                                proton.two_factor(fresh_code.as_ref().unwrap_or(&code))?;
                            }
                            if !proton.has_address(&email)? {
                                return Err("Signed-in account does not own this address".into());
                            }
                            // A mailbox-key or listing failure must not discard an
                            // authenticated, identity-checked session and force a new login.
                            proton.persist(&home, &email)?;
                            proton.unlock_mailbox(mailbox_password.as_ref().unwrap_or(&password))
                        };
                        let mut setup = proton.with_verification(&mut prepare, |challenge| {
                            verify_proton(challenge, verify).inspect_err(|_| fallback_needed = true)
                        });
                        if setup.is_err()
                            && was_resumed
                            && proton
                                .last_failure()
                                .is_some_and(|failure| failure.rejects_refresh_token())
                        {
                            // Only explicit refresh-token rejection permits a password
                            // login. The account lease remains held across both attempts.
                            proton = aterm_mail::proton::Proton::sign_in_with_verifier(
                                &login,
                                &password,
                                |challenge| {
                                    verify_proton(challenge, verify)
                                        .inspect_err(|_| fallback_needed = true)
                                },
                            )?;
                            setup = proton.with_verification(&mut prepare, |challenge| {
                                verify_proton(challenge, verify)
                                    .inspect_err(|_| fallback_needed = true)
                            });
                        }
                        proton.hold_session(lease);
                        if let Err(error) = setup {
                            fallback_needed |= proton
                                .last_failure()
                                .is_some_and(|failure| permits_browser_fallback(failure.kind));
                            return Err(failed_proton_connection(&proton, error));
                        }
                        Ok(Account::Proton(std::sync::Arc::new(std::sync::Mutex::new(
                            proton,
                        ))))
                    })();
                    match api {
                        Ok(account) => account,
                        Err(error) => {
                            #[cfg(unix)]
                            if fallback_needed {
                                connect_browser(&home, browser_credentials(), verify).map_err(
                                    |fallback| format!("{error}; headless fallback: {fallback}"),
                                )?
                            } else {
                                return Err(error);
                            }
                            #[cfg(not(unix))]
                            {
                                return Err(error);
                            }
                        }
                    }
                }
            } else {
                let host = if provider == Provider::Gmail {
                    "imap.gmail.com".to_string()
                } else {
                    host
                };
                let smtp = if provider == Provider::Gmail {
                    "smtp.gmail.com".to_string()
                } else {
                    smtp
                };
                Account::Standard(std::sync::Arc::new(std::sync::Mutex::new(
                    StandardAccount {
                        imap: aterm_mail::imap::Imap {
                            endpoint: Endpoint::parse(&host, 993)?,
                            authentication: Authentication::Password {
                                username: login,
                                password,
                            },
                        },
                        smtp: Endpoint::parse(&smtp, 465)?,
                        refresh: None,
                        generation: 0,
                    },
                )))
            };
            finish_connect(email, account, accounts, verify)
        }

        Work::List { account, folder } => {
            let (_, account) = accounts
                .get_mut(account)
                .ok_or("Account is no longer connected")?;
            account.refresh()?;
            Ok(Reply::Listed {
                messages: read_with_verification(
                    account,
                    |mailbox| mailbox.messages(&folder, 50),
                    verify,
                )?,
                folder,
            })
        }
        Work::Read {
            account,
            folder,
            id,
        } => {
            let (_, account) = accounts
                .get_mut(account)
                .ok_or("Account is no longer connected")?;
            account.refresh()?;
            Ok(Reply::Read(read_with_verification(
                account,
                |mailbox| mailbox.read(&folder, &id),
                verify,
            )?))
        }
        Work::Send {
            account,
            draft,
            draft_revision,
        } => {
            let (_, account) = accounts
                .get_mut(account)
                .ok_or("Account is no longer connected")?;
            account.refresh()?;
            match account {
                Account::Standard(account) => {
                    let account = account.lock().map_err(|_| "Mail account lock failed")?;
                    aterm_mail::smtp::send(&account.smtp, &account.imap.authentication, &draft)?
                }
                Account::Proton(proton) => proton
                    .lock()
                    .map_err(|_| "Mail reader lock failed")?
                    .send(&draft)?,
            }
            Ok(Reply::Sent(draft_revision))
        }
    }
}

fn upsert_account(
    accounts: &mut Vec<(String, Account)>,
    email: String,
    account: Account,
) -> (usize, String) {
    let email = email.trim().to_owned();
    if let Some((index, slot)) = accounts
        .iter_mut()
        .enumerate()
        .find(|(_, (known, _))| known.eq_ignore_ascii_case(&email))
    {
        *slot = (email.clone(), account);
        (index, email)
    } else {
        let index = accounts.len();
        accounts.push((email.clone(), account));
        (index, email)
    }
}

fn finish_connect(
    email: String,
    mut account: Account,
    accounts: &mut Vec<(String, Account)>,
    verify: &mut impl FnMut(String),
) -> Result<Reply, String> {
    account.refresh()?;
    let folder: String = match &account {
        Account::Proton(_) => "0",
        Account::Standard(_) => "INBOX",
    }
    .into();
    let listing = read_with_verification(
        &mut account,
        |mailbox| Ok((mailbox.folders()?, mailbox.messages(&folder, 50)?)),
        verify,
    );
    let (folders, messages) = match listing {
        Ok(listing) => listing,
        Err(error) => {
            if let Account::Proton(proton) = &account {
                let proton = proton.lock().map_err(|_| "Mail reader lock failed")?;
                return Err(failed_proton_connection(&proton, error));
            }
            return Err(error);
        }
    };
    let (index, email) = upsert_account(accounts, email, account);
    Ok(Reply::Connected {
        account: index,
        email,
        folders,
        folder,
        messages,
    })
}

pub(crate) struct MailViewState {
    pub(crate) common: CommonViewState,
    provider: Provider,
    pub(crate) setup: bool,
    compose: bool,
    email: TextInputState,
    password: TextInputState,
    code: TextInputState,
    host: TextInputState,
    smtp: TextInputState,
    search: TextInputState,
    scroll: f32,
    scroll_limit: std::cell::Cell<f32>,
}
impl Default for MailViewState {
    fn default() -> Self {
        let blank = || TextInputState::new(String::new());
        Self {
            common: CommonViewState::default(),
            provider: Provider::Proton,
            setup: true,
            compose: false,
            email: blank(),
            password: blank(),
            code: blank(),
            host: blank(),
            smtp: blank(),
            search: blank(),
            scroll: 0.0,
            scroll_limit: std::cell::Cell::new(0.0),
        }
    }
}
impl Drop for MailViewState {
    fn drop(&mut self) {
        self.password.wipe();
        self.code.wipe();
    }
}

impl MailViewState {
    pub(crate) fn leave_setup(&mut self) {
        if self.setup {
            self.setup = false;
            self.scroll = 0.0;
        }
    }

    fn input(&mut self, key: &str) -> Option<&mut TextInputState> {
        match key {
            "mail/email" => Some(&mut self.email),
            "mail/password" => Some(&mut self.password),
            "mail/code" => Some(&mut self.code),
            "mail/host" => Some(&mut self.host),
            "mail/smtp" => Some(&mut self.smtp),
            "mail/search" => Some(&mut self.search),
            _ => None,
        }
    }
}

impl NativeAppModel for MailApp {
    type ViewState = MailViewState;
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Mail,
            name: "Mail",
            icon: AppIcon::Mail,
            singleton: true,
        }
    }
    fn update(
        &mut self,
        view: &mut MailViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::FocusChanged(key) => view.common.last_focus = key,
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll =
                    (view.scroll + lines as f32 * 32.0).clamp(0.0, view.scroll_limit.get())
            }
            AppEvent::ScrollPage(direction) => {
                view.scroll =
                    (view.scroll + direction as f32 * 480.0).clamp(0.0, view.scroll_limit.get())
            }
            AppEvent::TextInput(event) => {
                let key = view
                    .common
                    .last_focus
                    .as_ref()
                    .map(|k| k.as_str().to_owned())
                    .unwrap_or_default();
                let is_draft = matches!(key.as_str(), "mail/to" | "mail/subject" | "mail/body");
                let input = match key.as_str() {
                    "mail/to" => Some(&mut self.to),
                    "mail/subject" => Some(&mut self.subject),
                    "mail/body" => Some(&mut self.body),
                    _ => view.input(&key),
                };
                if let Some(input) = input {
                    let before = input.value().to_owned();
                    edit(input, event, key == "mail/body");
                    if is_draft && before != input.value() {
                        self.draft_dirty = true;
                        self.draft_revision = self.draft_revision.wrapping_add(1);
                    }
                } else {
                    return EventResult::Bubble;
                }
            }
            AppEvent::Action(action) => {
                let key = action.id.as_str();
                if let Some(SemanticInput::TextPosition { byte, extend }) = action.value.as_ref() {
                    let input = match key {
                        "mail/to" => Some(&mut self.to),
                        "mail/subject" => Some(&mut self.subject),
                        "mail/body" => Some(&mut self.body),
                        _ => view.input(key),
                    };
                    if let Some(input) = input {
                        let anchor = if *extend {
                            input.selection().anchor
                        } else {
                            *byte
                        };
                        input.set_selection(anchor, *byte);
                    }
                }
                if let Some(SemanticInput::Text(value)) = action.value.as_ref() {
                    let is_draft = matches!(key, "mail/to" | "mail/subject" | "mail/body");
                    let input = match key {
                        "mail/to" => Some(&mut self.to),
                        "mail/subject" => Some(&mut self.subject),
                        "mail/body" => Some(&mut self.body),
                        _ => view.input(key),
                    };
                    if let Some(input) = input
                        && input.value() != value
                    {
                        *input = TextInputState::new(value.clone());
                        if is_draft {
                            self.draft_dirty = true;
                            self.draft_revision = self.draft_revision.wrapping_add(1);
                        }
                    }
                }
                match key {
                    "mail/search" => {
                        view.common.last_focus = Some(crate::native_ui::UiKey::new("mail/search"));
                    }
                    "mail/back" => {
                        self.message = None;
                        view.scroll = 0.0;
                    }
                    "mail/copy" => {
                        if let Some(message) = &self.message {
                            cx.clipboard(crate::native_app::ClipboardRequest::CopyText {
                                text: message.text.clone(),
                                sensitive: true,
                            });
                        }
                    }
                    "mail/reply" | "mail/forward" => {
                        if self.draft_dirty {
                            self.notice = "Send or discard the current draft first".into();
                        } else if let Some(message) = &self.message {
                            let reply = key == "mail/reply";
                            self.draft_from = self
                                .accounts
                                .iter()
                                .find(|(id, ..)| Some(*id) == self.account)
                                .map(|(id, email, _)| (*id, email.clone()));
                            self.to = TextInputState::new(if reply {
                                message
                                    .from
                                    .rsplit_once('<')
                                    .and_then(|(_, address)| address.trim().strip_suffix('>'))
                                    .unwrap_or(&message.from)
                                    .trim()
                                    .to_owned()
                            } else {
                                String::new()
                            });
                            let prefix = if reply { "Re: " } else { "Fwd: " };
                            self.subject = TextInputState::new(
                                if message
                                    .subject
                                    .to_lowercase()
                                    .starts_with(&prefix.to_lowercase())
                                {
                                    message.subject.clone()
                                } else {
                                    format!("{prefix}{}", message.subject)
                                },
                            );
                            self.body = TextInputState::new(format!(
                                "\n\nFrom: {}\nSubject: {}\n\n{}",
                                message.from, message.subject, message.text
                            ));
                            self.draft_dirty = true;
                            self.draft_revision = self.draft_revision.wrapping_add(1);
                            view.compose = true;
                            view.setup = false;
                            view.scroll = 0.0;
                        }
                    }
                    "mail/add" => {
                        view.setup = true;
                        view.compose = false;
                    }
                    "mail/proton" => view.provider = Provider::Proton,
                    "mail/gmail" => view.provider = Provider::Gmail,
                    "mail/imap" => view.provider = Provider::Imap,
                    "mail/connect" if self.pending.is_none() => {
                        if view.provider == Provider::Gmail {
                            self.request(Work::Google(view.email.value().trim().into()));
                        } else {
                            self.request(Work::Connect {
                                provider: view.provider,
                                email: view.email.value().trim().into(),
                                login: view.email.value().trim().into(),
                                password: Secret::new(view.password.value().into()),
                                mailbox_password: None,
                                code: Secret::new(view.code.value().into()),
                                totp_secret: None,
                                host: view.host.value().trim().into(),
                                smtp: view.smtp.value().trim().into(),
                            });
                        }
                        view.password.wipe();
                        view.code.wipe();
                    }
                    "mail/inbox" => {
                        view.setup = false;
                        view.compose = false;
                    }
                    "mail/compose" if self.account.is_some() => {
                        if !self.draft_dirty {
                            self.to = TextInputState::new(String::new());
                            self.subject = TextInputState::new(String::new());
                            self.body = TextInputState::new(String::new());
                            self.draft_from = self
                                .accounts
                                .iter()
                                .find(|(id, ..)| Some(*id) == self.account)
                                .map(|(id, email, _)| (*id, email.clone()));
                        }
                        view.setup = false;
                        view.compose = true;
                        view.scroll = 0.0;
                    }
                    "mail/discard" if self.pending.is_none() => {
                        self.to = TextInputState::new(String::new());
                        self.subject = TextInputState::new(String::new());
                        self.body = TextInputState::new(String::new());
                        self.draft_dirty = false;
                        self.draft_revision = self.draft_revision.wrapping_add(1);
                        view.compose = false;
                    }
                    "mail/send" if self.pending.is_none() && self.draft_dirty => {
                        if let Some((account, from)) = self.draft_from.clone() {
                            self.request(Work::Send {
                                account,
                                draft: Draft {
                                    from,
                                    to: self
                                        .to
                                        .value()
                                        .split(',')
                                        .map(str::trim)
                                        .filter(|s| !s.is_empty())
                                        .map(str::to_owned)
                                        .collect(),
                                    subject: self.subject.value().into(),
                                    text: self.body.value().into(),
                                    ..Draft::default()
                                },
                                draft_revision: self.draft_revision,
                            });
                        }
                    }
                    "mail/refresh" if self.pending.is_none() => {
                        if let Some(account) = self.account {
                            self.request(Work::List {
                                account,
                                folder: self.folder.clone(),
                            });
                        }
                    }
                    _ => {
                        if let Some(id) = key
                            .strip_prefix("mail/account/")
                            .and_then(|s| s.parse::<usize>().ok())
                        {
                            if let Some((_, _, folders)) =
                                self.accounts.iter().find(|(account, ..)| *account == id)
                            {
                                let switched = self.account != Some(id);
                                self.account = Some(id);
                                self.folders = folders.clone();
                                if switched {
                                    self.folder = self
                                        .folders
                                        .first()
                                        .map(|folder| folder.id.clone())
                                        .unwrap_or_default();
                                    self.messages.clear();
                                    self.message = None;
                                    view.scroll = 0.0;
                                }
                                view.setup = false;
                                view.compose = false;
                                if let Some(folder) = self.folders.first() {
                                    self.request(Work::List {
                                        account: id,
                                        folder: folder.id.clone(),
                                    });
                                }
                            }
                        } else if let Some(index) = key
                            .strip_prefix("mail/folder/")
                            .and_then(|s| s.parse::<usize>().ok())
                        {
                            if let (Some(account), Some(folder)) =
                                (self.account, self.folders.get(index))
                            {
                                self.request(Work::List {
                                    account,
                                    folder: folder.id.clone(),
                                });
                                view.scroll = 0.0;
                            }
                        } else if let Some(index) = key
                            .strip_prefix("mail/message/")
                            .and_then(|s| s.parse::<usize>().ok())
                            && let (Some(account), Some(message)) =
                                (self.account, self.messages.get(index))
                        {
                            self.request(Work::Read {
                                account,
                                folder: self.folder.clone(),
                                id: message.id.clone(),
                            });
                            view.scroll = 0.0;
                        }
                    }
                }
            }
            _ => return EventResult::Bubble,
        }
        view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
        cx.repaint_instance(DamageRegion::All);
        EventResult::Handled
    }
    fn view(&self, view: &MailViewState, cx: &ViewCx<'_>) -> UiTree {
        let scale = crate::native_appearance::text_scale();
        let actions = vec![
            button("mail/add", "Add account", true),
            button("mail/compose", "Compose", self.account.is_some()),
            button(
                "mail/refresh",
                "Refresh",
                self.account.is_some() && self.pending.is_none(),
            ),
        ];
        let header = vec![
            group(
                "mail/heading",
                "Mail status",
                Layout::row().height(Length::Fixed(30.0 * scale)).gap(12.0),
                vec![
                    text("mail/title", "Mail", true),
                    text("mail/status", &self.notice, false),
                ],
            ),
            group(
                "mail/actions",
                "Mail actions",
                Layout::row().height(Length::Fixed(36.0 * scale)).gap(12.0),
                actions,
            ),
        ];
        let toolbar_height = 74.0 * scale;
        let content_height = (cx.viewport.height - toolbar_height - 36.0).max(0.0);
        let toolbar = group(
            "mail/toolbar",
            "Mail",
            Layout::column()
                .height(Length::Fixed(toolbar_height))
                .gap(8.0),
            header,
        );
        let content = if view.setup {
            let providers = group(
                "mail/providers",
                "Account provider",
                Layout::row().height(Length::Fixed(40.0 * scale)).gap(8.0),
                vec![
                    selected_button(
                        "mail/proton",
                        "Proton Mail",
                        true,
                        view.provider == Provider::Proton,
                    ),
                    selected_button(
                        "mail/gmail",
                        "Gmail",
                        true,
                        view.provider == Provider::Gmail,
                    ),
                    selected_button(
                        "mail/imap",
                        "Other mail",
                        true,
                        view.provider == Provider::Imap,
                    ),
                ],
            );
            let mut fields = vec![
                text("mail/setup/title", "Connect an account", true),
                providers,
                field("mail/email", "Email address", &view.email, false, view),
            ];
            if view.provider != Provider::Gmail {
                fields.push(field(
                    "mail/password",
                    "Password",
                    &view.password,
                    true,
                    view,
                ));
            }
            if view.provider == Provider::Proton {
                fields.push(field(
                    "mail/code",
                    "Verification code, if required",
                    &view.code,
                    true,
                    view,
                ));
            }
            if view.provider == Provider::Imap {
                fields.push(field(
                    "mail/host",
                    "IMAP server (optional :port)",
                    &view.host,
                    false,
                    view,
                ));
                fields.push(field(
                    "mail/smtp",
                    "SMTP server (optional :port)",
                    &view.smtp,
                    false,
                    view,
                ));
            }
            fields.push(button(
                "mail/connect",
                if view.provider == Provider::Gmail {
                    "Connect with Google"
                } else {
                    "Connect"
                },
                self.pending.is_none(),
            ));
            if self.account.is_some() {
                fields.push(button("mail/inbox", "Back to mail", true));
            }
            view.scroll_limit
                .set((fixed_extent(&fields, 12.0, 40.0) - content_height).max(0.0));
            group(
                "mail/setup",
                "Connect an account",
                Layout::column()
                    .width(Length::Fixed((cx.viewport.width - 48.0).clamp(1.0, 620.0)))
                    .gap(12.0)
                    .padding(Insets::all(20.0))
                    .scrolled(view.scroll.min(view.scroll_limit.get()))
                    .clipped(),
                fields,
            )
        } else if view.compose {
            let mut body = paragraphs(
                "mail/body-preview",
                self.body.value(),
                cx.viewport.width - 48.0,
                0.0,
            );
            body.layout.height = Length::Fixed(fixed_extent(&body.children, 0.0, 0.0));
            let fields = vec![
                text(
                    "mail/from-account",
                    &format!(
                        "From: {}",
                        self.draft_from
                            .as_ref()
                            .map_or("", |(_, email)| email.as_str())
                    ),
                    false,
                ),
                field("mail/to", "To", &self.to, false, view),
                field("mail/subject", "Subject", &self.subject, false, view),
                field(
                    "mail/body",
                    "Message — Enter adds a line",
                    &self.body,
                    false,
                    view,
                ),
                body,
                button(
                    "mail/send",
                    "Send",
                    self.pending.is_none() && self.draft_dirty,
                ),
                button("mail/discard", "Discard draft", self.pending.is_none()),
                button("mail/inbox", "Back to mail", true),
            ];
            view.scroll_limit
                .set((fixed_extent(&fields, 12.0, 40.0) - content_height).max(0.0));
            group(
                "mail/composer",
                "Compose message",
                Layout::column()
                    .gap(12.0)
                    .padding(Insets::all(20.0))
                    .scrolled(view.scroll.min(view.scroll_limit.get()))
                    .clipped(),
                fields,
            )
        } else {
            let mut sidebar = Vec::new();
            for (id, email, _) in &self.accounts {
                sidebar.push(selected_button(
                    &format!("mail/account/{id}"),
                    email,
                    true,
                    Some(*id) == self.account,
                ));
            }
            for (index, folder) in self.folders.iter().enumerate() {
                sidebar.push(selected_button(
                    &format!("mail/folder/{index}"),
                    &folder.name,
                    self.pending.is_none(),
                    folder.id == self.folder,
                ));
            }
            let sidebar = group(
                "mail/accounts",
                "Accounts and folders",
                Layout::column()
                    .width(Length::Fixed(190.0 * scale))
                    .gap(6.0)
                    .clipped(),
                sidebar,
            );
            let query = view.search.value().to_lowercase();
            let mut rows = vec![field(
                "mail/search",
                "Search this list",
                &view.search,
                false,
                view,
            )];
            for (index, message) in self.messages.iter().enumerate().filter(|(_, m)| {
                format!("{} {}", m.subject, m.from)
                    .to_lowercase()
                    .contains(&query)
            }) {
                rows.push(button(
                    &format!("mail/message/{index}"),
                    &format!(
                        "{}{} · {}",
                        if message.unread { "● " } else { "" },
                        message.from,
                        message.subject
                    ),
                    self.pending.is_none(),
                ));
            }
            if self.messages.is_empty() {
                rows.push(text("mail/empty", "No messages in this folder", false));
            }
            let narrow = cx.viewport.width < 850.0 * scale;
            if self.message.is_none() {
                view.scroll_limit
                    .set((fixed_extent(&rows, 6.0, 0.0) - content_height).max(0.0));
            }
            let list = group(
                "mail/list",
                "Messages",
                Layout::column()
                    .width(if narrow {
                        Length::Fill
                    } else {
                        Length::Fixed(330.0 * scale)
                    })
                    .gap(6.0)
                    .clipped()
                    .scrolled(if self.message.is_none() {
                        view.scroll.min(view.scroll_limit.get())
                    } else {
                        0.0
                    }),
                rows,
            );
            let mut columns = vec![sidebar];
            if !narrow || self.message.is_none() {
                columns.push(list);
            }
            if let Some(message) = &self.message {
                let mut body = paragraphs(
                    "mail/message-body",
                    &message.text,
                    (cx.viewport.width - if narrow { 230.0 } else { 580.0 }).max(100.0),
                    0.0,
                );
                let room = (content_height - 36.0 - 126.0 * scale - 48.0).max(0.0);
                view.scroll_limit
                    .set((fixed_extent(&body.children, 0.0, 0.0) - room).max(0.0));
                body.layout = body
                    .layout
                    .scrolled(view.scroll.min(view.scroll_limit.get()));
                let reader = group(
                    "mail/reader",
                    "Message",
                    Layout::column()
                        .gap(12.0)
                        .padding(Insets::all(18.0))
                        .clipped(),
                    vec![
                        text("mail/subject-title", &message.subject, true),
                        text("mail/from", &format!("From: {}", message.from), false),
                        text("mail/to-value", &format!("To: {}", message.to), false),
                        group(
                            "mail/message-actions",
                            "Message actions",
                            Layout::row().height(Length::Fixed(36.0 * scale)).gap(8.0),
                            vec![
                                button("mail/back", "Back", true),
                                button("mail/reply", "Reply", true),
                                button("mail/forward", "Forward", true),
                                button("mail/copy", "Copy message", true),
                            ],
                        ),
                        body,
                    ],
                );
                columns.push(reader);
            }
            group(
                "mail/columns",
                "Mailboxes",
                Layout::row().gap(14.0).clipped(),
                columns,
            )
        };
        let mut root = group(
            "mail/root",
            "Mail",
            Layout::column()
                .padding(Insets::all(12.0))
                .gap(12.0)
                .clipped(),
            vec![toolbar, content],
        );
        if let crate::native_ui::UiContent::Group(spec) = &mut root.content {
            spec.role = SemanticRole::Application;
        }
        UiTree::new(root)
    }
    fn commands(&self, _: &MailViewState, out: &mut Vec<Command>) {
        for (id, title) in [
            ("mail/compose", "Mail: Compose"),
            ("mail/refresh", "Mail: Refresh"),
            ("mail/add", "Mail: Add account"),
        ] {
            out.push(Command {
                id: ActionId::new(id),
                title: title.into(),
                shortcut: None,
                enabled: true,
            });
        }
    }
    fn presentation(&self, _: &MailViewState) -> AppPresentation {
        AppPresentation {
            title: "Mail".into(),
            icon: AppIcon::Mail,
            indicators: AppIndicators::default(),
            closable: true,
            tooltip: Some("Mail".into()),
        }
    }
    fn prepare_close(&mut self, _: CloseRequest, _: &mut UpdateCx<'_>) -> CloseReadiness {
        if self.draft_dirty {
            CloseReadiness::Blocked {
                recovery: vec![Command {
                    id: ActionId::new("mail/compose"),
                    title: "Return to unsent draft".into(),
                    shortcut: None,
                    enabled: true,
                }],
            }
        } else {
            CloseReadiness::Ready
        }
    }
}

fn group(key: &str, label: &str, layout: Layout, children: Vec<UiNode>) -> UiNode {
    UiNode::new(
        key,
        crate::native_ui::UiContent::Group(GroupSpec::new(label)),
    )
    .layout(layout)
    .children(children)
}

fn fixed_extent(children: &[UiNode], gap: f32, padding: f32) -> f32 {
    padding
        + gap * children.len().saturating_sub(1) as f32
        + children
            .iter()
            .map(|child| match child.layout.height {
                Length::Fixed(height) => height,
                _ => 0.0,
            })
            .sum::<f32>()
}
fn button(key: &str, label: &str, enabled: bool) -> UiNode {
    UiNode::new(
        key,
        crate::native_ui::UiContent::Button(
            Control::new(ButtonSpec::new(label), ActionId::new(key))
                .state(ControlState {
                    enabled,
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
        ),
    )
    .layout(Layout::default().height(Length::Fixed(36.0 * crate::native_appearance::text_scale())))
}

fn selected_button(key: &str, label: &str, enabled: bool, selected: bool) -> UiNode {
    let mut node = button(key, label, enabled);
    if let crate::native_ui::UiContent::Button(control) = &mut node.content {
        control.state.selected = selected;
    }
    node
}
fn text(key: &str, label: &str, heading: bool) -> UiNode {
    UiNode::new(
        key,
        crate::native_ui::UiContent::Text(if heading {
            TextSpec::heading(label)
        } else {
            TextSpec::text(label)
        }),
    )
    .layout(Layout::default().height(Length::Fixed(30.0 * crate::native_appearance::text_scale())))
}
fn field(
    key: &str,
    label: &str,
    input: &TextInputState,
    secret: bool,
    view: &MailViewState,
) -> UiNode {
    UiNode::new(
        key,
        crate::native_ui::UiContent::TextField(
            Control::new(
                TextFieldSpec {
                    label: label.into(),
                    placeholder: Some(label.into()),
                    secret,
                    visual_value: None,
                    input: Some(input.projection()),
                    swatch: None,
                    swatch_line: false,
                    trailing_hint: None,
                    description: None,
                    end_button: false,
                },
                ActionId::new(key),
            )
            .value(SemanticValue::Text(input.value().into()))
            .state(ControlState {
                focused: view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|k| k.as_str() == key),
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
    .layout(Layout::default().height(Length::Fixed(40.0 * crate::native_appearance::text_scale())))
}
fn paragraphs(key: &str, body: &str, width: f32, scroll: f32) -> UiNode {
    let columns = (width / (8.0 * crate::native_appearance::text_scale())).max(10.0) as usize;
    let mut lines = Vec::new();
    for paragraph in body.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > columns {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    let children = lines
        .iter()
        .enumerate()
        .map(|(n, line)| text(&format!("{key}/{n}"), line, false))
        .collect();
    group(
        key,
        "Message text",
        Layout::column().scrolled(scroll).clipped(),
        children,
    )
}
fn edit(input: &mut TextInputState, event: TextInputEvent, multiline: bool) {
    match event {
        TextInputEvent::Preedit { text, selection } => input.set_preedit(text, selection),
        TextInputEvent::Commit(text) => input.commit_preedit(&text),
        TextInputEvent::Backspace => input.delete_backward(),
        TextInputEvent::Delete => input.delete_forward(),
        TextInputEvent::Left { extend } => input.move_left(extend),
        TextInputEvent::Right { extend } => input.move_right(extend),
        TextInputEvent::Home { extend } => input.move_to_start(extend),
        TextInputEvent::End { extend } => input.move_to_end(extend),
        TextInputEvent::WordLeft { extend } => input.move_word_left(extend),
        TextInputEvent::WordRight { extend } => input.move_word_right(extend),
        TextInputEvent::KillToEnd => input.kill_to_end(),
        TextInputEvent::KillToStart => input.kill_to_start(),
        TextInputEvent::DeleteWordBackward => input.delete_word_backward(),
        TextInputEvent::DeleteWordForward => input.delete_word_forward(),
        TextInputEvent::SelectAll => input.select_all(),
        TextInputEvent::Undo => {
            input.undo();
        }
        TextInputEvent::Redo => {
            input.redo();
        }
        TextInputEvent::Submit if multiline => input.insert("\n"),
        TextInputEvent::Submit | TextInputEvent::Cancel => input.cancel_preedit(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_discovery_finishes_the_connecting_notice() {
        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        app.replies = rx;
        app.pending = Some(3);
        app.notice = "Connecting…".into();
        tx.send((3, Reply::Available(Vec::new()))).unwrap();
        assert!(app.poll());
        assert!(app.pending.is_none());
        assert_eq!(app.notice, "Add an account to get started");
    }

    #[test]
    fn late_discovery_preserves_a_newer_request_and_selected_account() {
        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        app.replies = rx;
        app.account = Some(0);
        app.pending = Some(9);
        tx.send((2, Reply::Available(vec!["stored@example.test".into()])))
            .unwrap();
        assert!(app.poll());
        assert_eq!(app.pending, Some(9));
        assert_eq!(app.account, Some(0));
        assert_eq!(
            app.stored.front().map(String::as_str),
            Some("stored@example.test")
        );
    }

    #[test]
    fn background_refresh_preserves_open_message_and_pending_draft() {
        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        app.replies = rx;
        app.account = Some(0);
        app.folder = "0".into();
        app.draft_dirty = true;
        app.pending = Some(7);
        app.message = Some(Message {
            id: "open".into(),
            from: String::new(),
            to: String::new(),
            subject: "Open".into(),
            date: String::new(),
            unread: false,
            text: "Body".into(),
        });
        tx.send((
            0,
            Reply::Synced {
                account: 0,
                folder: "0".into(),
                messages: Vec::new(),
            },
        ))
        .unwrap();
        app.poll();
        assert_eq!(app.message.as_ref().unwrap().id, "open");
        assert!(app.draft_dirty);
        assert_eq!(app.pending, Some(7));
        assert_eq!(retry_interval(0).as_secs(), 2);
        assert_eq!(retry_interval(1).as_secs(), 2);
        assert_eq!(retry_interval(u32::MAX).as_secs(), 900);
    }

    #[test]
    fn send_acknowledgement_does_not_discard_newer_edits() {
        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        app.replies = rx;
        app.pending = Some(7);
        app.draft_revision = 4;
        app.draft_dirty = true;
        tx.send((7, Reply::Sent(3))).unwrap();
        assert!(app.poll());
        assert!(app.draft_dirty);
        app.pending = Some(8);
        tx.send((8, Reply::Sent(4))).unwrap();
        app.poll();
        assert!(!app.draft_dirty);
    }

    #[test]
    #[ignore = "requires ATERM_MAIL_TEST_ACCOUNT and an authenticated headless profile"]
    fn headless_account_connects_and_reads_through_native_worker() {
        aterm_uds::secret::no_core_dumps();
        let email = std::env::var("ATERM_MAIL_TEST_ACCOUNT").expect("set the live test account");
        let mut accounts = Vec::new();
        let connected = perform(Work::Stored(email.clone()), &mut accounts, &mut |_| {}).unwrap();
        let Reply::Connected {
            account,
            folder,
            messages,
            ..
        } = connected
        else {
            panic!("mail worker did not connect");
        };
        let message = messages.first().expect("the live test needs one message");
        let read = perform(
            Work::Read {
                account,
                folder,
                id: message.id.clone(),
            },
            &mut accounts,
            &mut |_| {},
        )
        .unwrap();
        let Reply::Read(read) = read else {
            panic!("mail worker did not read");
        };
        assert_eq!(read.id, message.id);
        assert!(read.subject == message.subject);
        assert!(!read.text.is_empty());
        let mut config = aterm_agent::harness::mail_recovery::configured().unwrap();
        config.senders = vec!["aterm-native-probe.invalid".into()];
        let outcome = aterm_agent::harness::headless_oauth::poll_mailbox_config_for_credential(
            &config,
            Some(&email),
            std::time::Duration::from_secs(2),
        );
        assert!(matches!(
            outcome,
            Err("timed out waiting for verification email in mailbox")
        ));
    }

    #[test]
    fn browser_fallback_does_not_repeat_credentials_or_network_failures() {
        use aterm_mail::proton_error::Kind;
        for kind in [
            Kind::Verification,
            Kind::DeviceVerification,
            Kind::Scope,
            Kind::ClientRejected,
        ] {
            assert!(permits_browser_fallback(kind));
        }
        for kind in [
            Kind::Credentials,
            Kind::RateLimited,
            Kind::Network,
            Kind::Timeout,
            Kind::AccessDenied,
            Kind::Configuration,
            Kind::AccountRestricted,
            Kind::Service,
            Kind::SessionExpired,
            Kind::Protocol,
            Kind::Refused,
        ] {
            assert!(!permits_browser_fallback(kind));
        }
    }

    #[test]
    fn late_connection_retains_account_without_replacing_selection() {
        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        app.replies = rx;
        app.pending = Some(9);
        app.account = Some(0);
        tx.send((
            8,
            Reply::Connected {
                account: 1,
                email: "second@example.test".into(),
                folders: vec![],
                folder: "INBOX".into(),
                messages: vec![],
            },
        ))
        .unwrap();
        app.poll();
        assert_eq!(app.account, Some(0));
        assert_eq!(app.pending, Some(9));
        assert_eq!(app.accounts[0].0, 1);
        assert!(!app.take_leave_setup());
    }

    fn dummy_standard_account(username: &str) -> Account {
        Account::Standard(std::sync::Arc::new(std::sync::Mutex::new(
            StandardAccount {
                imap: aterm_mail::imap::Imap {
                    endpoint: Endpoint::parse("imap.example.test", 993).unwrap(),
                    authentication: Authentication::Password {
                        username: username.into(),
                        password: Secret::new("test-pass".into()),
                    },
                },
                smtp: Endpoint::parse("smtp.example.test", 465).unwrap(),
                refresh: None,
                generation: 0,
            },
        )))
    }

    #[test]
    fn second_account_and_case_variant_reconnect_leave_setup_without_duplicates() {
        assert_eq!(
            dedup_accounts(vec![
                " Alice@Example.Test ".into(),
                "alice@example.test".into(),
                "bob@example.test".into(),
                "   ".into(),
            ]),
            vec![
                "Alice@Example.Test".to_string(),
                "bob@example.test".to_string()
            ]
        );

        let mut worker_accounts = Vec::new();
        let (first_idx, first_email) = upsert_account(
            &mut worker_accounts,
            "alice@example.test".into(),
            dummy_standard_account("alice"),
        );
        assert_eq!((first_idx, first_email.as_str()), (0, "alice@example.test"));
        let (reconnected_idx, reconnected_email) = upsert_account(
            &mut worker_accounts,
            " Alice@Example.Test ".into(),
            dummy_standard_account("alice2"),
        );
        assert_eq!(
            (reconnected_idx, reconnected_email.as_str()),
            (0, "Alice@Example.Test")
        );
        assert_eq!(worker_accounts.len(), 1);
        let (second_idx, _) = upsert_account(
            &mut worker_accounts,
            "bob@example.test".into(),
            dummy_standard_account("bob"),
        );
        assert_eq!(second_idx, 1);

        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(4);
        app.replies = rx;
        app.account = Some(0);
        app.accounts = vec![(0, "alice@example.test".into(), Vec::new())];
        app.pending = Some(4);
        tx.send((
            4,
            Reply::Connected {
                account: 1,
                email: "bob@example.test".into(),
                folders: vec![Folder {
                    id: "INBOX".into(),
                    name: "Inbox".into(),
                }],
                folder: "INBOX".into(),
                messages: Vec::new(),
            },
        ))
        .unwrap();
        assert!(app.poll());
        assert!(app.take_leave_setup());
        assert!(!app.take_leave_setup());
        assert_eq!(app.account, Some(1));
        assert_eq!(app.accounts.len(), 2);

        app.pending = Some(5);
        tx.send((
            5,
            Reply::Connected {
                account: 0,
                email: "Alice@Example.Test".into(),
                folders: Vec::new(),
                folder: "INBOX".into(),
                messages: Vec::new(),
            },
        ))
        .unwrap();
        assert!(app.poll());
        assert!(app.take_leave_setup());
        assert_eq!(app.accounts.len(), 2);
        assert_eq!(app.accounts[0].0, 0);
        assert_eq!(app.accounts[0].1, "Alice@Example.Test");

        tx.send((
            0,
            Reply::Available(vec![
                "alice@example.test".into(),
                "BOB@example.test".into(),
                "carol@example.test".into(),
            ]),
        ))
        .unwrap();
        app.pending = Some(6);
        assert!(app.poll());
        assert_eq!(
            app.stored.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["carol@example.test"]
        );

        let mut view = MailViewState::default();
        view.setup = true;
        view.scroll = 96.0;
        view.leave_setup();
        assert!(!view.setup);
        assert_eq!(view.scroll, 0.0);
    }

    #[test]
    fn stale_reply_rearms_active_folder_watch_once_foreground_work_settles() {
        let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
        let (work_tx, work_rx) = mpsc::sync_channel(4);
        let (reply_tx, reply_rx) = mpsc::sync_channel(4);
        app.work = work_tx;
        app.replies = reply_rx;
        app.account = Some(0);
        app.folder = "INBOX".into();
        app.messages = vec![Message {
            id: "1".into(),
            from: "sender@example.test".into(),
            to: "alice@example.test".into(),
            subject: "Current".into(),
            date: String::new(),
            unread: true,
            text: String::new(),
        }];
        app.pending = Some(11);

        reply_tx
            .send((
                10,
                Reply::Connected {
                    account: 1,
                    email: "second@example.test".into(),
                    folders: Vec::new(),
                    folder: "Archive".into(),
                    messages: Vec::new(),
                },
            ))
            .unwrap();
        assert!(app.poll());
        assert!(work_rx.try_recv().is_err());

        reply_tx.send((11, Reply::Sent(0))).unwrap();
        assert!(app.poll());
        let Ok((
            0,
            Work::Watch {
                account,
                folder,
                baseline,
            },
        )) = work_rx.try_recv()
        else {
            panic!("expected active folder watch to re-arm after foreground send");
        };
        assert_eq!(account, 0);
        assert_eq!(folder, "INBOX");
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].id, "1");
    }
}
