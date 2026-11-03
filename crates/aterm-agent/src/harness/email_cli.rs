// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Read-only, headless mail commands using the native account transports.

use super::{headless_oauth, mail_recovery};
use aterm_json::{Value, json};
#[cfg(unix)]
use aterm_mail::Mailbox;
use aterm_mail::{Authentication, Folder, Message, Secret};
use std::{
    ffi::OsString,
    io::Write,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

#[path = "email_session.rs"]
mod session;

#[path = "gmail_connect.rs"]
mod gmail_connect;

const HELP: &str = "aterm email connect-gmail ACCOUNT [--client-file FILE] [--timeout 1..300]\naterm email session ACCOUNT [--folder ID] [--limit 1..100] [--timeout 1..3600]\naterm email accounts\naterm email folders ACCOUNT\naterm email list ACCOUNT [--folder ID] [--limit 1..100]\naterm email read ACCOUNT MESSAGE_ID [--folder ID]\naterm email watch ACCOUNT [--folder ID] [--limit 1..100] [--timeout 1..3600]\n\nOutputs JSON Lines. Uses configured Proton, Gmail or IMAP accounts. No window opens.\nReading uses PEEK and does not mark messages read. Watch emits an initial snapshot,\nchanged snapshots, then a completion event at its deadline. --timeout also bounds\nconnection and reads (default: 60 seconds). No send or delete commands. Session defaults to a one-hour lifetime and accepts JSON Lines requests\n(folders, list, read, select, wait, ping, quit) over one authenticated connection.\n";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Accounts,
    Session,
    Folders,
    List,
    Read(String),
    Watch,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Options {
    command: Command,
    account: String,
    folder: Option<String>,
    limit: usize,
    timeout: Duration,
}

fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let Some(command) = args.first() else {
        return Ok(None);
    };
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        return Ok(None);
    }
    let mut options = Options {
        command: Command::Accounts,
        account: String::new(),
        folder: None,
        limit: 50,
        timeout: Duration::from_secs(60),
    };
    if command == "accounts" {
        return if args.len() == 1 {
            Ok(Some(options))
        } else {
            Err("accounts takes no arguments".into())
        };
    }
    options.command = match command.as_str() {
        "session" => Command::Session,
        "folders" => Command::Folders,
        "list" => Command::List,
        "watch" => Command::Watch,
        "read" => Command::Read(String::new()),
        _ => return Err("Unknown email command; use aterm email --help".into()),
    };
    if options.command == Command::Session {
        options.timeout = Duration::from_secs(3600);
    }
    options.account = args
        .get(1)
        .filter(|value| valid_account(value))
        .ok_or("A mailbox email address is required")?
        .to_ascii_lowercase();
    let mut index = 2;
    if let Command::Read(id) = &mut options.command {
        *id = args
            .get(index)
            .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
            .ok_or("A message ID is required")?
            .clone();
        index += 1;
    }
    let mut seen = std::collections::BTreeSet::new();
    while index < args.len() {
        let flag = args[index].as_str();
        if !seen.insert(flag) {
            return Err("Duplicate email option".into());
        }
        let value = args.get(index + 1).ok_or("Missing email option value")?;
        match flag {
            "--folder" if !matches!(options.command, Command::Folders) => {
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err("Invalid folder ID".into());
                }
                options.folder = Some(value.clone());
            }
            "--limit"
                if matches!(
                    options.command,
                    Command::List | Command::Watch | Command::Session
                ) =>
            {
                options.limit = value
                    .parse()
                    .ok()
                    .filter(|n| (1..=100).contains(n))
                    .ok_or("Limit must be between 1 and 100")?;
            }
            "--timeout" => {
                let seconds = value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=3600).contains(n))
                    .ok_or("Timeout must be between 1 and 3600 seconds")?;
                options.timeout = Duration::from_secs(seconds);
            }
            _ => return Err("Unsupported email option".into()),
        }
        index += 2;
    }
    Ok(Some(options))
}

fn valid_account(value: &str) -> bool {
    value.len() <= 320
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
        && value.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
}

#[derive(Debug)]
struct Failure {
    message: String,
    retry: bool,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            retry: false,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
fn transient(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        retry: true,
    }
}
fn remaining(end: Instant) -> Result<Duration, Failure> {
    let left = end.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(transient("Email operation deadline expired"))
    } else {
        Ok(left)
    }
}
fn pause(end: Instant, within: Duration) -> Result<(), Failure> {
    std::thread::sleep(remaining(end)?.min(within));
    Ok(())
}
fn emit(output: &mut impl Write, value: Value) -> Result<(), Failure> {
    let mut bytes =
        aterm_json::to_vec(&value).map_err(|_| Failure::from("Could not encode email result"))?;
    bytes.push(b'\n');
    output
        .write_all(&bytes)
        .and_then(|()| output.flush())
        .map_err(|_| Failure::from("Could not write email result"))
}
fn message_value(message: &Message, body: bool) -> Value {
    let mut value = json!({"id":message.id,"from":message.from,"to":message.to,"subject":message.subject,"date":message.date,"unread":message.unread});
    if body && let Some(object) = value.as_object_mut() {
        object.insert("text".into(), Value::String(message.text.clone()));
    }
    value
}

fn configured_account(config: &headless_oauth::MailboxConfig, account: &str) -> bool {
    config.route_for(Some(account)).is_some()
        || mail_recovery::accounts(config)
            .iter()
            .any(|known| known.eq_ignore_ascii_case(account))
}

fn is_proton_webmail_url(url: &str) -> bool {
    let trimmed = url.trim();
    let prefix = "https://mail.proton.me";
    trimmed
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        && trimmed[prefix.len()..]
            .chars()
            .next()
            .is_none_or(|ch| matches!(ch, '/' | '?' | '#'))
}

fn normalized_accounts(
    config: Option<&headless_oauth::MailboxConfig>,
    oauth_accounts: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut accounts: Vec<String> = config
        .map(mail_recovery::accounts)
        .unwrap_or_default()
        .into_iter()
        .chain(oauth_accounts)
        .map(|account| account.trim().to_ascii_lowercase())
        .filter(|account| valid_account(account))
        .collect();
    accounts.sort();
    accounts.dedup();
    accounts
}

struct Connection {
    source: Source,
    folder: String,
}
enum Source {
    #[cfg(unix)]
    Proton(super::mail_browser::SharedMailbox),
    Imap(Box<aterm_mail::imap::Reader>),
}
impl Connection {
    fn open(home: &Path, options: &Options, end: Instant) -> Result<Self, Failure> {
        remaining(end)?;
        let end = if options.command == Command::Session {
            end.min(Instant::now() + Duration::from_secs(30))
        } else {
            end
        };
        let discovered = headless_oauth::discover_mailbox_config(Some(home), None)
            .filter(|cfg| configured_account(cfg, &options.account));
        let proton_configured = discovered.as_ref().is_some_and(|cfg| {
            cfg.effective_webmail_url(Some(&options.account))
                .is_some_and(|url| is_proton_webmail_url(&url))
        });
        let explicit_non_gmail_imap = !proton_configured
            && discovered.as_ref().is_some_and(|cfg| {
                cfg.effective_imap_url(Some(&options.account))
                    .is_some_and(|url| {
                        headless_oauth::parse_imap_url(&url).is_none_or(|parsed| {
                            !parsed.host.eq_ignore_ascii_case("imap.gmail.com")
                        })
                    })
            });
        if !proton_configured
            && !explicit_non_gmail_imap
            && let Some(shared) = aterm_mail::oauth::session::shared(home, &options.account)?
        {
            let default_folder = discovered
                .as_ref()
                .and_then(|cfg| cfg.effective_imap_url(Some(&options.account)))
                .and_then(|url| headless_oauth::parse_imap_url(&url))
                .map_or_else(|| "INBOX".into(), |parsed| parsed.mailbox);
            let auth = loop {
                remaining(end)?;
                match shared.try_lock() {
                    Ok(mut session) => {
                        session.renew_until(end).map_err(|error| Failure {
                            retry: error.retryable(),
                            message: error.to_string(),
                        })?;
                        break session
                            .authentication()
                            .ok_or("Google mail authorization is unavailable")?;
                    }
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        return Err("Google mail authorization lock failed".into());
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        pause(end, Duration::from_millis(10))?
                    }
                }
            };
            return Self::imap(
                aterm_mail::Endpoint::parse("imap.gmail.com", 993)?,
                auth,
                options.folder.clone().unwrap_or(default_folder),
                end,
                options.command == Command::Session,
            );
        }
        let config = discovered.ok_or("No configured mailbox matches this account")?;
        let proton = proton_configured;
        if !proton && let Some(url) = config.effective_imap_url(Some(&options.account)) {
            let parsed =
                headless_oauth::parse_imap_url(&url).ok_or("Invalid configured IMAP URL")?;
            let login = parsed
                .user
                .or_else(|| config.webmail_user_for(Some(&options.account)))
                .filter(|u| !u.trim().is_empty())
                .ok_or("No mailbox login is configured")?;
            let mut password = parsed
                .password
                .or_else(|| config.password_for(Some(&options.account)))
                .ok_or("No mailbox credential is configured")?;
            if parsed.host.eq_ignore_ascii_case("imap.gmail.com") {
                password.retain(|c| !c.is_whitespace());
            }
            if password.is_empty() {
                return Err("No mailbox credential is configured".into());
            }
            let endpoint = aterm_mail::Endpoint {
                host: parsed.host,
                port: parsed.port,
                security: if parsed.tls {
                    aterm_mail::Security::Tls
                } else {
                    aterm_mail::Security::StartTls
                },
            };
            return Self::imap(
                endpoint,
                Authentication::Password {
                    username: login,
                    password: Secret::new(password),
                },
                options.folder.clone().unwrap_or(parsed.mailbox),
                end,
                options.command == Command::Session,
            );
        }
        if !proton {
            return Err("No supported native mailbox route is configured for this account".into());
        }
        #[cfg(unix)]
        {
            let login = config
                .webmail_user_for(Some(&options.account))
                .filter(|u| !u.trim().is_empty())
                .ok_or("No Proton login is configured")?;
            let password = config
                .password_for(Some(&options.account))
                .filter(|p| !p.is_empty())
                .ok_or("No Proton credential is configured")?;
            let mailbox_password = config.mailbox_password_for(Some(&options.account));
            let totp_secret = config.totp_secret_for(Some(&options.account));
            let shared = super::mail_browser::shared_mailbox_until(
                home,
                super::mail_browser::Credentials {
                    email: &options.account,
                    login: &login,
                    password: &password,
                    mailbox_password: mailbox_password.as_deref(),
                    totp_secret: totp_secret.as_deref(),
                },
                Some(end),
            )?;
            Ok(Self {
                source: Source::Proton(shared),
                folder: options.folder.clone().unwrap_or_else(|| "0".into()),
            })
        }
        #[cfg(not(unix))]
        Err("Native Proton reading is unavailable on this platform".into())
    }
    fn imap(
        endpoint: aterm_mail::Endpoint,
        authentication: Authentication,
        folder: String,
        end: Instant,
        persistent: bool,
    ) -> Result<Self, Failure> {
        let imap = aterm_mail::imap::Imap {
            endpoint,
            authentication,
        };
        let reader = if persistent {
            imap.open_with_connect_timeout(&folder, remaining(end)?)
        } else {
            imap.open_until(&folder, end)
        }
        .map_err(|error| match error {
            aterm_mail::imap::ConnectError::Authentication => {
                Failure::from("IMAP authentication was refused")
            }
            aterm_mail::imap::ConnectError::Other(message) => transient(message),
        })?;
        Ok(Self {
            source: Source::Imap(Box::new(reader)),
            folder,
        })
    }
    #[cfg(unix)]
    fn proton<T>(
        shared: &super::mail_browser::SharedMailbox,
        end: Instant,
        operation: impl FnOnce(&mut aterm_mail::proton::Proton) -> Result<T, String>,
    ) -> Result<T, Failure> {
        loop {
            remaining(end)?;
            match shared.try_lock() {
                Ok(mut mailbox) => {
                    let result = mailbox.run_until(end, operation);
                    return result.map_err(|message| Failure {
                        message,
                        retry: mailbox.last_failure().is_some_and(|failure| {
                            matches!(
                                failure.recovery(),
                                aterm_mail::proton_error::Recovery::RetryRead
                                    | aterm_mail::proton_error::Recovery::Reauthenticate
                            )
                        }),
                    });
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("Mail account lock failed".into());
                }
                Err(std::sync::TryLockError::WouldBlock) => pause(end, Duration::from_millis(10))?,
            }
        }
    }
    fn folders(&mut self, end: Instant) -> Result<Vec<Folder>, Failure> {
        remaining(end)?;
        match &mut self.source {
            #[cfg(unix)]
            Source::Proton(shared) => Self::proton(shared, end, |mailbox| mailbox.folders()),
            Source::Imap(reader) => reader
                .run_until(end, |reader| reader.folders())
                .map_err(transient),
        }
    }
    fn messages(&mut self, limit: usize, end: Instant) -> Result<Vec<Message>, Failure> {
        remaining(end)?;
        match &mut self.source {
            #[cfg(unix)]
            Source::Proton(shared) => {
                Self::proton(shared, end, |mailbox| mailbox.messages(&self.folder, limit))
            }
            Source::Imap(reader) => reader
                .run_until(end, |reader| reader.messages(limit))
                .map_err(transient),
        }
    }
    fn observe(
        &mut self,
        limit: usize,
        _previous: &[Message],
        end: Instant,
    ) -> Result<Vec<Message>, Failure> {
        remaining(end)?;
        match &mut self.source {
            #[cfg(unix)]
            Source::Proton(shared) => Self::proton(shared, end, |mailbox| {
                mailbox.observe_messages(&self.folder, limit, _previous)
            }),
            Source::Imap(reader) => reader
                .run_until(end, |reader| reader.messages(limit))
                .map_err(transient),
        }
    }

    fn read(&mut self, id: &str, end: Instant) -> Result<Message, Failure> {
        remaining(end)?;
        match &mut self.source {
            #[cfg(unix)]
            Source::Proton(shared) => {
                Self::proton(shared, end, |mailbox| mailbox.read(&self.folder, id))
            }
            Source::Imap(reader) => {
                reader
                    .run_until(end, |reader| reader.read(id))
                    .map_err(|error| Failure {
                        retry: error.retryable(),
                        message: String::from(error),
                    })
            }
        }
    }
    fn select(&mut self, folder: &str, end: Instant) -> Result<(), Failure> {
        remaining(end)?;
        match &mut self.source {
            #[cfg(unix)]
            Source::Proton(shared) => {
                let folders = Self::proton(shared, end, |mailbox| mailbox.folders())?;
                if !folders.iter().any(|value| value.id == folder) {
                    return Err("Unknown mail folder".into());
                }
            }
            Source::Imap(reader) => {
                let previous = self.folder.clone();
                match reader.run_until(end, |reader| reader.try_select_read_only(folder)) {
                    Ok(()) => {}
                    Err(aterm_mail::imap::SelectError::UnknownFolder) => {
                        reader
                            .run_until(end, |reader| reader.select_read_only(&previous))
                            .map_err(transient)?;
                        return Err("Unknown mail folder".into());
                    }
                    Err(error) => {
                        return Err(Failure {
                            retry: error.retryable(),
                            message: String::from(error),
                        });
                    }
                }
            }
        }
        self.folder = folder.into();
        Ok(())
    }

    fn wait_for(&mut self, within: Duration, end: Instant) -> Result<bool, Failure> {
        let within = remaining(end)?.min(within);
        match &mut self.source {
            #[cfg(unix)]
            Source::Proton(_) => pause(end, within.min(Duration::from_secs(2))).map(|()| true),
            Source::Imap(reader) => reader
                .run_until(end, |reader| reader.wait(within))
                .map_err(transient),
        }
    }

    fn wait(&mut self, end: Instant) -> Result<bool, Failure> {
        self.wait_for(remaining(end)?.min(Duration::from_secs(30)), end)
    }
}

fn run(home: &Path, options: &Options, output: &mut impl Write) -> Result<(), Failure> {
    if options.command == Command::Accounts {
        let config = headless_oauth::discover_mailbox_config(Some(home), None);
        let accounts =
            normalized_accounts(config.as_ref(), aterm_mail::oauth::session::accounts(home));
        return emit(
            output,
            json!({"schema":1,"type":"accounts","accounts":accounts}),
        );
    }
    let end = Instant::now() + options.timeout;
    let mut connection = match Connection::open(home, options, end) {
        Ok(connection) => Some(connection),
        Err(failure) if failure.retry && options.command == Command::Watch => None,
        Err(failure) => return Err(failure),
    };
    if options.command == Command::Session {
        return session::run(
            connection.take().ok_or("No mailbox connection")?,
            options.clone(),
            home,
            end,
            std::io::stdin(),
            output,
        );
    }
    if options.command == Command::Folders {
        let folders: Vec<_> = connection
            .as_mut()
            .ok_or("No mailbox connection")?
            .folders(end)?
            .into_iter()
            .map(|folder| json!({"id":folder.id,"name":folder.name}))
            .collect();
        return emit(
            output,
            json!({"schema":1,"type":"folders","account":options.account,"folders":folders}),
        );
    }
    if let Command::Read(id) = &options.command {
        let connection = connection.as_mut().ok_or("No mailbox connection")?;
        let message = connection.read(id, end)?;
        return emit(
            output,
            json!({"schema":1,"type":"message","account":options.account,"folder":connection.folder,"message":message_value(&message, true)}),
        );
    }
    let mut previous: Option<(String, Vec<Message>)> = None;
    let mut reconnecting = false;
    let mut failures = 0u32;
    let mut should_poll = true;
    loop {
        if end <= Instant::now() {
            break;
        }
        if connection.is_none() {
            if !reconnecting {
                emit(
                    output,
                    json!({"schema":1,"type":"status","state":"reconnecting"}),
                )?;
                reconnecting = true;
            }
            let delay = Duration::from_secs(2u64.saturating_pow(failures.min(5)).min(30));
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(delay));
            if end <= Instant::now() {
                break;
            }
            match Connection::open(home, options, end) {
                Err(_) if end <= Instant::now() => break,
                Ok(opened) => {
                    connection = Some(opened);
                    should_poll = true;
                }
                Err(failure) if failure.retry => {
                    failures = failures.saturating_add(1);
                    continue;
                }
                Err(failure) => return Err(failure),
            }
        }
        let current = connection.as_mut().ok_or("No mailbox connection")?;
        if should_poll {
            let poll_result = match previous.as_ref() {
                Some((prev_folder, prev_messages))
                    if options.command == Command::Watch && *prev_folder == current.folder =>
                {
                    current.observe(options.limit, prev_messages, end)
                }
                _ => current.messages(options.limit, end),
            };
            let messages = match poll_result {
                Err(_) if options.command == Command::Watch && end <= Instant::now() => break,
                Ok(messages) => messages,
                Err(failure) if failure.retry && options.command == Command::Watch => {
                    failures = failures.saturating_add(1);
                    connection = None;
                    continue;
                }
                Err(failure) => return Err(failure),
            };
            failures = 0;
            let snapshot = (current.folder.clone(), messages);
            if previous.as_ref() != Some(&snapshot) {
                let event = if previous.is_none() {
                    "snapshot"
                } else {
                    "changed"
                };
                let values: Vec<_> = snapshot
                    .1
                    .iter()
                    .map(|message| message_value(message, false))
                    .collect();
                emit(
                    output,
                    json!({"schema":1,"type":event,"account":options.account,"folder":snapshot.0,"messages":values}),
                )?;
                previous = Some(snapshot);
            }
        }
        reconnecting = false;
        if options.command == Command::List {
            return Ok(());
        }
        if end <= Instant::now() {
            break;
        }
        match current.wait(end) {
            Ok(notified) => should_poll = notified,
            Err(_) if end <= Instant::now() => break,
            Err(failure) if failure.retry => {
                failures = failures.saturating_add(1);
                connection = None;
                should_poll = true;
            }
            Err(failure) => return Err(failure),
        }
    }
    if previous.is_none() {
        return Err(transient(
            "No mailbox snapshot was available before the deadline",
        ));
    }
    emit(
        output,
        json!({"schema":1,"type":"complete","reason":"timeout"}),
    )
}

/// Reuse a Google grant, or enroll once after a missing or rejected authorization.
/// Transient renewal failures never start enrollment. Call only for an explicit connection request.
pub fn connect_google_mail(home: &Path, account: &str, within: Duration) -> Result<(), String> {
    if within < Duration::from_secs(1) || within > Duration::from_secs(300) {
        return Err("Gmail connection timeout must be 1..300 seconds".into());
    }
    let end = Instant::now() + within;
    if let Some(shared) = aterm_mail::oauth::session::shared(home, account)? {
        loop {
            remaining(end).map_err(|failure| failure.message)?;
            match shared.try_lock() {
                Ok(mut session) => {
                    match session.renew_until(end.min(Instant::now() + Duration::from_secs(30))) {
                        Ok(_) => return Ok(()),
                        Err(aterm_mail::oauth::Error {
                            kind: aterm_mail::oauth::ErrorKind::Rejected,
                        }) => break,
                        Err(error) => return Err(error.to_string()),
                    }
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("Google mail authorization lock failed".into());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    pause(end, Duration::from_millis(10)).map_err(|failure| failure.message)?;
                }
            }
        }
    }
    let left = remaining(end).map_err(|failure| failure.message)?;
    if left < Duration::from_secs(1) {
        return Err("Google mail connection deadline expired".into());
    }
    gmail_connect::connect_default(home, account, left).map_err(|failure| failure.message)
}

pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    aterm_uds::secret::no_core_dumps();
    let result = (|| {
        let args: Vec<String> = args
            .into_iter()
            .map(|arg| {
                arg.into_string()
                    .map_err(|_| Failure::from("Email arguments must be UTF-8"))
            })
            .collect::<Result<_, _>>()?;
        let mut output = std::io::stdout().lock();
        if args
            .first()
            .is_some_and(|command| command == "connect-gmail")
        {
            let home = aterm_types::dirs::home_dir().ok_or("Home directory unavailable")?;
            return gmail_connect::run(&home, &args[1..], &mut output);
        }
        let Some(options) = parse(&args)? else {
            return output
                .write_all(HELP.as_bytes())
                .map_err(|_| Failure::from("Could not write email help"));
        };
        let home = aterm_types::dirs::home_dir().ok_or("Home directory unavailable")?;
        run(&home, &options, &mut output)
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = emit(
                &mut std::io::stderr().lock(),
                json!({"schema":1,"type":"error","message":error.message,"retryable":error.retry}),
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(args: &[&str]) -> Result<Option<Options>, String> {
        parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }
    #[test]
    fn arguments_are_bounded_and_read_only() {
        assert!(options(&["send", "me@example.test"]).is_err());
        assert!(options(&["list", "me@example.test", "--limit", "101"]).is_err());
        assert!(options(&["watch", "me@example.test", "--timeout", "0"]).is_err());
        assert!(options(&["read", "me@example.test", "id", "--folder", "bad\nfolder"]).is_err());
        assert!(options(&["list", "me@example.test", "--limit", "2", "--limit", "3"]).is_err());
        assert!(options(&["list", "a@b@c"]).is_err());
        assert_eq!(
            options(&["session", "me@example.test"])
                .unwrap()
                .unwrap()
                .timeout,
            Duration::from_secs(3600)
        );
        assert!(options(&["read", "me@example.test", "-opaque-id"]).is_ok());
        let parsed = options(&[
            "watch",
            "Me@Example.Test",
            "--timeout",
            "2",
            "--limit",
            "100",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.account, "me@example.test");
        assert_eq!(parsed.timeout, Duration::from_secs(2));
    }
    #[test]
    fn a_default_password_cannot_be_sent_to_an_unconfigured_account() {
        let config = headless_oauth::MailboxConfig {
            user: Some("me@proton.me".into()),
            password: Some("do-not-send-to-another-provider".into()),
            ..Default::default()
        };
        assert!(configured_account(&config, "ME@proton.me"));
        assert!(!configured_account(&config, "other@gmail.com"));
    }

    #[test]
    fn expired_operation_is_retryable_without_network_work() {
        assert!(remaining(Instant::now()).unwrap_err().retry);
    }

    #[test]
    fn proton_urls_without_trailing_slash_and_mixed_case_accounts_normalize_cleanly() {
        assert!(is_proton_webmail_url("https://mail.proton.me"));
        assert!(is_proton_webmail_url("HTTPS://mail.proton.me/u/0/inbox"));
        assert!(!is_proton_webmail_url(
            "https://mail.proton.me.evil.example.invalid"
        ));
        let config = headless_oauth::MailboxConfig {
            user: Some("Me@Example.Invalid".into()),
            ..Default::default()
        };
        assert_eq!(
            normalized_accounts(
                Some(&config),
                vec!["me@example.invalid".into(), "bad@@invalid".into()],
            ),
            vec!["me@example.invalid".to_owned()],
        );
    }

    #[test]
    fn headers_exclude_body_and_output_remains_one_json_line() {
        let message = Message {
            id: "id".into(),
            from: "from".into(),
            to: "to".into(),
            subject: "line\nother".into(),
            date: String::new(),
            unread: true,
            text: "private-body".into(),
        };
        assert!(message_value(&message, false).get("text").is_none());
        let mut output = Vec::new();
        assert!(emit(&mut output, message_value(&message, true)).is_ok());
        assert_eq!(output.iter().filter(|b| **b == b'\n').count(), 1);
        let value: Value = aterm_json::from_slice(&output).unwrap();
        assert_eq!(value["text"].as_str(), Some("private-body"));
    }
    #[test]
    #[cfg(unix)]
    #[ignore = "requires ATERM_MAIL_TEST_ACCOUNT and its authenticated headless profile"]
    fn live_commands_list_read_and_finish_a_quiet_watch() {
        aterm_uds::secret::no_core_dumps();
        let account = std::env::var("ATERM_MAIL_TEST_ACCOUNT").expect("live test account");
        let home = aterm_types::dirs::home_dir().unwrap();
        let mut output = Vec::new();
        let list = options(&["list", &account, "--limit", "1"])
            .unwrap()
            .unwrap();
        run(&home, &list, &mut output)
            .map_err(|error| error.message)
            .unwrap();
        let value: Value = aterm_json::from_slice(&output).unwrap();
        let id = value["messages"][0]["id"].as_str().unwrap();
        assert!(value["messages"][0].get("text").is_none());
        let read = options(&["read", &account, id]).unwrap().unwrap();
        let mut body = Vec::new();
        run(&home, &read, &mut body)
            .map_err(|error| error.message)
            .unwrap();
        let value: Value = aterm_json::from_slice(&body).unwrap();
        assert!(!value["message"]["text"].as_str().unwrap().is_empty());
        let watch = options(&["watch", &account, "--timeout", "30", "--limit", "1"])
            .unwrap()
            .unwrap();
        let mut events = Vec::new();
        run(&home, &watch, &mut events)
            .map_err(|error| error.message)
            .unwrap();
        let lines: Vec<Value> = events
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| aterm_json::from_slice(line).unwrap())
            .collect();
        assert!(
            lines
                .iter()
                .any(|line| line["type"].as_str() == Some("snapshot"))
        );
        assert_eq!(lines.last().unwrap()["type"].as_str(), Some("complete"));
    }
}
