// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `/usr/bin/ssh` invocation with fleet keys, UUID-pinned host keys, and bounded fan-out. Every
//! connection `ssh` opens is made by `aterm lan fdpass`, through the root LAN broker on macOS
//! ([`super::lan_proxy`]).

use super::inventory::{Host, Paths};
use crate::port::{Cmd, Ran, RunFailed, System};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Path to Apple's OpenSSH client.
pub const SSH: &str = aterm_uds::os_path!("ssh");

/// OpenSSH stderr substring when a pinned host key does not match.
pub const HOST_KEY_CHANGED: &str = "REMOTE HOST IDENTIFICATION HAS CHANGED!";

/// OpenSSH stderr substring when no host key is pinned for the alias.
pub const HOST_KEY_UNKNOWN: &str = " host key is known for ";

/// OpenSSH final stderr line on host-key verification failure.
pub const HOST_KEY_FAILED: &str = "Host key verification failed.";

/// OpenSSH stderr substring when public-key authentication is denied.
pub const DENIED: &str = ": Permission denied (";

/// OpenSSH stderr substring when the remote port refuses the connection.
pub const CONNECTION_REFUSED: &str = ": Connection refused";

/// OpenSSH stderr substring when the connection attempt times out.
pub const CONNECT_TIMED_OUT: &str = ": Operation timed out";

/// OpenSSH stderr substring when the SSH banner exchange times out.
pub const BANNER_TIMED_OUT: &str = "Connection timed out during banner exchange";

/// OpenSSH stderr substring on `EHOSTUNREACH` (`No route to host`).
pub const NO_ROUTE: &str = ": No route to host";

/// OpenSSH stderr substring on `EHOSTDOWN` (`Host is down`).
pub const HOST_DOWN: &str = ": Host is down";

/// OpenSSH stderr substring when hostname resolution fails.
pub const UNRESOLVED: &str = "Could not resolve hostname";

/// SSH connection target for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Admin account name.
    pub user: String,
    /// Host address or hostname.
    pub address: String,
    /// Host UUID used as `HostKeyAlias` (`None` before enrollment).
    pub alias: Option<String>,
}

impl Target {
    /// Builds a [`Target`] from an inventory [`Host`].
    #[must_use]
    pub fn of(host: &Host) -> Self {
        Self {
            user: host.user.clone(),
            address: host.address.clone(),
            alias: Some(host.uuid.clone()),
        }
    }

    /// Formats `user@address` for `ssh` after `--`.
    #[must_use]
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.address)
    }

    /// Validates that `user` and `address` are safe for `ssh`.
    ///
    /// # Errors
    /// One line naming the bad word: `user`, `address` or the host key alias, when it is
    /// empty or malformed.
    pub fn check(&self) -> Result<(), String> {
        let word_ok = |w: &str, extra: &[u8]| {
            !w.is_empty()
                && !w.starts_with('-')
                && w.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b) || extra.contains(&b))
        };
        if !word_ok(&self.user, b"") {
            return Err(format!("ssh: {:?} is not an account name", self.user));
        }
        if !word_ok(&self.address, b":%") {
            return Err(format!("ssh: {:?} is not a host address", self.address));
        }
        if let Some(alias) = &self.alias
            && !word_ok(alias, b"")
        {
            return Err(format!("ssh: {alias:?} is not a host key alias"));
        }
        Ok(())
    }
}

/// OpenSSH control-master mode for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mux<'a> {
    /// No control master and no interactive prompts (`BatchMode=yes`).
    Off,
    /// Starts a control master at this socket path for interactive enrollment.
    Master(&'a Path),
    /// Connects through an existing control master at this socket path.
    Through(&'a Path),
}

/// How long one connection the controller's `ssh` opens may take (`ConnectTimeout`, and the
/// broker's connect behind it, [`super::lan_proxy::options`]).
const CONNECT_WITHIN: Duration = Duration::from_secs(5);

/// Base OpenSSH `-o` options shared by all fleet connections.
#[must_use]
pub fn base_options(paths: &Paths, target: &Target) -> Vec<OsString> {
    options(paths, target, Mux::Off)
}

/// OpenSSH arguments for `target` under `mux`.
#[must_use]
pub fn options(paths: &Paths, target: &Target, mux: Mux<'_>) -> Vec<OsString> {
    build(paths, target, mux)
}

fn build(paths: &Paths, target: &Target, mux: Mux<'_>) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec!["-F".into(), "/dev/null".into()];
    let mut set = |option: &str| {
        out.push("-o".into());
        out.push(option.into());
    };
    set("IdentitiesOnly=yes");
    if matches!(mux, Mux::Off) {
        set(&format!("IdentityFile={}", config_path(&paths.ssh_key())));
    }
    set(&format!(
        "UserKnownHostsFile={}",
        config_path(&paths.known_hosts())
    ));
    set("GlobalKnownHostsFile=/dev/null");
    if let Some(alias) = &target.alias {
        set(&format!("HostKeyAlias={alias}"));
    }
    set("StrictHostKeyChecking=yes");
    set("UpdateHostKeys=no");
    set(if matches!(mux, Mux::Master(_)) {
        "BatchMode=no"
    } else {
        "BatchMode=yes"
    });
    set(&format!("ConnectTimeout={}", CONNECT_WITHIN.as_secs()));
    set("ServerAliveInterval=15");
    set("ServerAliveCountMax=4");
    set("ForwardAgent=no");
    set("ForwardX11=no");
    set("ClearAllForwardings=yes");
    set("LogLevel=ERROR");
    match mux {
        Mux::Off => {
            set("ControlMaster=no");
            set("ControlPath=none");
        }
        Mux::Master(path) => {
            set("ControlMaster=yes");
            set(&format!("ControlPath={}", config_path(path)));
            set("ControlPersist=60");
            set("PubkeyAuthentication=no");
        }
        Mux::Through(path) => {
            set("ControlMaster=no");
            set(&format!("ControlPath={}", config_path(path)));
            set("PubkeyAuthentication=no");
            set("PasswordAuthentication=no");
            set("KbdInteractiveAuthentication=no");
        }
    }
    // A rider of a master opens no connection while the master lives; one that finds it gone
    // connects through the proxy too.
    out.extend(super::lan_proxy::options(CONNECT_WITHIN));
    out
}

/// `path` as an `-o` value. `ssh` reads `-o` as a config line: it splits a value at blanks
/// unless it is double-quoted, and expands `%` tokens in these paths.
pub(crate) fn config_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('%', "%%");
    if text
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '#'))
    {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text
    }
}

/// Builds the [`Cmd`] for a forced-command request on `target`.
///
/// # Errors
/// [`RunFailed::Refused`] for a target or command `ssh` could misread.
pub fn request_cmd(paths: &Paths, target: &Target, command: &str) -> Result<Cmd, RunFailed> {
    target.check().map_err(RunFailed::Refused)?;
    if command.is_empty() || !command.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return Err(RunFailed::Refused(format!(
            "ssh: {command:?} is not a one-line request"
        )));
    }
    let mut args: Vec<OsString> = vec!["-T".into()];
    args.extend(base_options(paths, target));
    args.push("--".into());
    args.push(target.destination().into());
    args.push(command.into());
    Ok(ssh_cmd(args))
}

/// Runs one forced-command request on `target` with `stdin`.
///
/// # Errors
/// [`RunFailed`] when ssh did not run to completion, or [`RunFailed::Refused`] before it
/// starts for a target or command `ssh` could misread.
pub fn request(
    sys: &dyn System,
    paths: &Paths,
    target: &Target,
    command: &str,
    stdin: Option<&Path>,
    budget: Duration,
) -> Result<Ran, RunFailed> {
    let mut cmd = request_cmd(paths, target, command)?.budget(budget);
    if let Some(path) = stdin {
        cmd = cmd.stdin_file(path);
    }
    sys.run(&cmd)
}

/// Classification of an OpenSSH transport failure (exit status 255).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The host's key is not the one pinned for it.
    HostKeyChanged,
    /// No key is pinned for it.
    HostKeyUnknown,
    /// It refused the fleet key.
    Denied,
    /// Nothing listens on its SSH port.
    Refused,
    /// No answer: to the connect, to the SSH banner, or to the address at all (`Host is
    /// down`).
    TimedOut,
    /// No route to it.
    NoRoute,
    /// Its name does not resolve.
    Unresolved,
    /// The LAN broker could not make the connection (`aterm lan fdpass`'s line, naming why:
    /// not installed, another protocol, refused): nothing about the host, and the same for
    /// every host this controller reaches on the LAN.
    Broker(String),
    /// Anything else: `ssh`'s first line, or that it printed nothing.
    Other(String),
}

impl Failure {
    /// Returns whether the failure means the host could not be reached.
    #[must_use]
    pub const fn unreachable(&self) -> bool {
        matches!(
            self,
            Self::Refused | Self::TimedOut | Self::NoRoute | Self::Unresolved
        )
    }

    /// Formats a single-line summary for a report row.
    #[must_use]
    pub fn why(&self) -> String {
        match self {
            Self::HostKeyChanged => "its host key is not the one pinned at enrollment (a \
                                     reinstalled Mac, or not the same Mac): check it at the \
                                     Mac; if it was erased or reinstalled, `aterm machines \
                                     unenroll <host> --unreachable`, then enroll it again"
                .into(),
            Self::HostKeyUnknown => "no host key is pinned for it: `aterm machines unenroll \
                                     <host> --unreachable`, remove at the Mac what that \
                                     lists as left on it, then enroll it again"
                .into(),
            Self::Denied => "it refused the fleet key (its authorized_keys line is gone): if \
                             it was erased or reinstalled, `aterm machines unenroll <host> \
                             --unreachable`, then enroll it again"
                .into(),
            Self::Refused => "nothing answers on its SSH port: Remote Login is off".into(),
            Self::TimedOut => "no answer: asleep, off, or not on this network".into(),
            Self::NoRoute => "no route to it".into(),
            Self::Unresolved => "its name does not resolve".into(),
            Self::Broker(line) | Self::Other(line) => line.clone(),
        }
    }
}

/// Diagnostic message when `ssh` exits 255 with empty standard error.
pub const EMPTY_255: &str = "exit 255 and nothing said: the connection closed, or the command on the host died by \
     a signal";

/// Classifies an OpenSSH failure when `ran` exited with status 255.
#[must_use]
pub fn classify(ran: &Ran) -> Option<Failure> {
    if ran.status != Some(255) {
        return None;
    }
    let err = ran.err();
    let failure = if let Some(line) = err
        .lines()
        .find(|line| line.contains(super::lan_proxy::BROKER_SAID))
    {
        Failure::Broker(line.trim().to_owned())
    } else if err.contains(HOST_KEY_CHANGED) {
        Failure::HostKeyChanged
    } else if err.contains(HOST_KEY_UNKNOWN) && err.contains(HOST_KEY_FAILED) {
        Failure::HostKeyUnknown
    } else if err.contains(DENIED) {
        Failure::Denied
    } else if err.contains(CONNECTION_REFUSED) {
        Failure::Refused
    } else if err.contains(CONNECT_TIMED_OUT)
        || err.contains(BANNER_TIMED_OUT)
        || err.contains(HOST_DOWN)
    {
        Failure::TimedOut
    } else if err.contains(NO_ROUTE) {
        Failure::NoRoute
    } else if err.contains(UNRESOLVED) {
        Failure::Unresolved
    } else {
        Failure::Other(
            err.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map_or_else(|| EMPTY_255.into(), str::to_owned),
        )
    };
    Some(failure)
}

/// Runs `work` across `items` with at most `parallel` concurrent workers, preserving input order.
pub fn fan_out<T: Sync, R: Send>(
    items: &[T],
    parallel: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let workers = parallel.max(1).min(items.len());
    if workers <= 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    let result = work(item);
                    slots
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)[i] = Some(result);
                }
            });
        }
    });
    slots
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every index was taken by a worker that finished"))
        .collect()
}

/// Builds a [`Cmd`] for `/usr/bin/ssh` with `args`.
#[must_use]
pub fn ssh_cmd(args: Vec<OsString>) -> Cmd {
    Cmd::new(SSH).args(args)
}

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod tests;
