// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Muse's fresh subscription usage read: `muse serve` over MSP (one JSON-RPC message
//! per line on its standard streams), `usage/refresh` (experimental, ADR 32563 D6 as the
//! binary's schema names it). The refresh mints at most once per host per 60 seconds and
//! spends no model call; a refused mint and an absent subscription with no usage object are
//! plain absences here, never a figure, and a host that says its login is gone is told apart
//! from them ([`Missed::SignedOut`]), so the account's record can say so. Nothing the host sent
//! is kept or written beyond the parsed numbers.
//!
//! The host runs where it can put nothing on the screen ([`serve_command`]): whether a
//! signed-out `muse serve` opens a sign-in page was never measured, and is not found out here.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

/// The largest host message read.
const MESSAGE_MAX: usize = 256 * 1024;

/// The most host messages read in one dialogue.
const MESSAGES_MAX: usize = 4096;

/// The host's two streams: what it writes, and what it reads.
pub struct AppServer {
    /// Its standard output.
    pub from: Box<dyn Read + Send>,
    /// Its standard input.
    pub to: Box<dyn Write + Send>,
}

/// Why a call came to nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fault {
    /// The host closed its output, or would not read.
    Gone,
    /// The deadline ran out.
    Late,
    /// It answered with something this client does not read.
    Unread,
    /// It answered with an error, whose message this is (one bounded line).
    Refused(String),
}

/// Why a read gave no reading ([`read_usage`], [`fetch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missed {
    /// The host said its Meta login is gone or no longer accepted ([`says_signed_out`]).
    SignedOut,
    /// It did not run, did not answer, or answered nothing this client reads.
    Unanswered,
}

/// The most of a host's error message or standard error that is read for its sign-in words.
const SAID_MAX: usize = 4096;

/// Whether `text`, the host's own words (an RPC error's message, its standard error), says its
/// Meta login is gone: Muse Code's sign-in walls ([`aterm_phase::muse::says_signed_out`]) and
/// its connector's sign-in words ([`crate::ai::connector::MUSE`]'s `auth_words`).
#[must_use]
pub fn says_signed_out(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    aterm_phase::muse::says_signed_out(text)
        || crate::ai::connector::MUSE
            .auth_words
            .iter()
            .any(|words| lower.contains(words))
}

/// The JSON-RPC client over an [`AppServer`]: one message per line, each stamped
/// `"jsonrpc": "2.0"` (Muse's MSP refuses a message without the member — its
/// `initialize` answer names it and everything after an unstamped greeting is
/// rejected as not initialized).
struct Rpc {
    to: Box<dyn Write + Send>,
    from: mpsc::Receiver<Value>,
    next: u64,
    deadline: Instant,
}

impl Rpc {
    fn send(&mut self, message: &Value) -> Result<(), Fault> {
        let stamped;
        let message = if let Value::Object(mut map) = message.clone() {
            map.entry("jsonrpc".to_owned())
                .or_insert_with(|| Value::from("2.0"));
            stamped = Value::Object(map);
            &stamped
        } else {
            message
        };
        let mut line = aterm_json::to_string(message).map_err(|_| Fault::Unread)?;
        line.push('\n');
        self.to
            .write_all(line.as_bytes())
            .and_then(|()| self.to.flush())
            .map_err(|_| Fault::Gone)
    }

    fn next_message(&mut self) -> Result<Value, Fault> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Fault::Late);
        }
        match self.from.recv_timeout(left) {
            Ok(message) => Ok(message),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Fault::Late),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Fault::Gone),
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Fault> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"id": id, "method": method, "params": params}))?;
        loop {
            let message = self.next_message()?;
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").is_none()
            {
                if let Some(result) = message.get("result") {
                    return Ok(result.clone());
                }
                return Err(message
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .map_or(Fault::Unread, |said| {
                        Fault::Refused(super::one_line(said, SAID_MAX))
                    }));
            }
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), Fault> {
        self.send(&json!({"method": method}))
    }
}

impl Fault {
    /// What a fault says of the read: a refusal in sign-in words is a login that is gone.
    fn missed(&self) -> Missed {
        match self {
            Fault::Refused(said) if says_signed_out(said) => Missed::SignedOut,
            Fault::Gone | Fault::Late | Fault::Unread | Fault::Refused(_) => Missed::Unanswered,
        }
    }
}

/// Greets `server` (`initialize` with `hello`, then `initialized`) by `deadline`.
fn hello_with(server: AppServer, deadline: Instant, hello: Value) -> Result<Rpc, Fault> {
    let (tx, rx) = mpsc::channel();
    let from = server.from;
    std::thread::spawn(move || {
        super::login_drive::read_lines_within(from, MESSAGE_MAX, MESSAGES_MAX, |line| {
            if let Ok(message) = aterm_json::from_str::<Value>(line.trim()) {
                let _ = tx.send(message);
            }
        });
    });
    let mut rpc = Rpc {
        to: server.to,
        from: rx,
        next: 0,
        deadline,
    };
    rpc.call("initialize", hello)?;
    rpc.notify("initialized")?;
    Ok(rpc)
}

/// The whole dialogue's budget: host start, the mint's network read, and the answer. A
/// hang detector, not a latency expectation (a live read answers in seconds).
#[cfg(not(test))]
pub const BUDGET: Duration = Duration::from_secs(20);
/// The whole dialogue's budget: the unit tests' stand-in hosts end on their own, so one a loaded
/// machine holds is waited for by the one-minute hang detector, not killed before it ran.
#[cfg(test)]
pub const BUDGET: Duration = aterm_spec::wait::MUST_HAPPEN;

/// One fresh Muse subscription reading ([`fetch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The tier's display name (`tierName`), one bounded line, where the mint named one.
    pub plan: Option<String>,
    /// Percent of the weekly budget used, in tenths (clamped to the 0..=1000 words read).
    pub weekly_used_x10: Option<u32>,
    /// When the weekly window resets, Unix seconds.
    pub weekly_resets_at: Option<i64>,
    /// When the provider stamped the reading, Unix seconds.
    pub observed_at: Option<i64>,
}

/// Whether an answer explicitly says there is no active subscription. An absent
/// member asserts nothing (the `usage/read` shape carries none).
fn explicitly_inactive(result: &Value) -> bool {
    result.get("subscriptionActive").and_then(Value::as_bool) == Some(false)
}

/// Reads a `usage/refresh` (or `usage/read`) result: the plan's words and the weekly
/// window, each only where the answer names them in range. An explicitly inactive
/// subscription is no reading, whatever stale tier or usage words ride along. A
/// result whose `usage` is absent (a login with no subscription, or a subscriber
/// whose mint carried none) keeps the plan words where named and no figure.
#[must_use]
pub fn parse(result: &Value) -> Option<Reading> {
    if explicitly_inactive(result) {
        return None;
    }
    let plan = result
        .get("tierName")
        .and_then(Value::as_str)
        .map(|name| super::one_line(name, 48))
        .filter(|name| !name.is_empty());
    let Some(usage) = result.get("usage") else {
        return plan.map(|plan| Reading {
            plan: Some(plan),
            weekly_used_x10: None,
            weekly_resets_at: None,
            observed_at: None,
        });
    };
    let weekly = usage.get("weekly");
    let pct = weekly
        .and_then(|w| w.get("usedPercent"))
        .and_then(Value::as_i64)
        .filter(|pct| *pct >= 0);
    let resets_ms = weekly
        .and_then(|w| w.get("resetsAtMs"))
        .and_then(Value::as_i64)
        .filter(|ms| *ms > 0);
    let observed_ms = usage
        .get("observedAtMs")
        .and_then(Value::as_i64)
        .filter(|ms| *ms > 0);
    let (used_x10, resets_at) = match (pct, resets_ms) {
        (Some(pct), Some(ms)) => (
            Some(
                u32::try_from(pct.saturating_mul(10))
                    .unwrap_or(u32::MAX)
                    .min(1000),
            ),
            Some(ms / 1000),
        ),
        _ => (None, None),
    };
    if plan.is_none() && used_x10.is_none() {
        return None;
    }
    Some(Reading {
        plan,
        weekly_used_x10: used_x10,
        weekly_resets_at: resets_at,
        observed_at: observed_ms.map(|ms| ms / 1000),
    })
}

/// `usage/refresh` over `server` (after `initialize`), by `deadline`: one fresh reading,
/// falling back to the host's last-observed `usage/read` where the mint refused (its
/// rate bound) or answered without a weekly figure. A complete read is named by the
/// mint's fresher plan words where it named any. An explicitly inactive mint is final: no
/// stale read resurrects it.
///
/// # Errors
///
/// Neither call answered a reading: [`Missed::SignedOut`] where any of the host's refusals
/// said its login is gone, else [`Missed::Unanswered`].
pub fn read_usage(server: AppServer, deadline: Instant) -> Result<Reading, Missed> {
    let hello = json!({
        "clientInfo": {"name": "aterm", "version": aterm_stamp::app_version()},
        "capabilities": {"experimentalApi": true}
    });
    let mut rpc = hello_with(server, deadline, hello).map_err(|fault| fault.missed())?;
    let mut missed = Missed::Unanswered;
    let refreshed = match rpc.call("usage/refresh", json!({})) {
        Ok(result) if explicitly_inactive(&result) => return Err(Missed::Unanswered),
        Ok(result) => parse(&result),
        Err(fault) => {
            missed = fault.missed();
            None
        }
    };
    if refreshed
        .as_ref()
        .is_some_and(|reading| reading.weekly_used_x10.is_some())
    {
        return refreshed.ok_or(missed);
    }
    let read = match rpc.call("usage/read", json!({})) {
        Ok(result) => parse(&result),
        Err(fault) => {
            if fault.missed() == Missed::SignedOut {
                missed = Missed::SignedOut;
            }
            None
        }
    };
    if let Some(mut read) = read {
        if read.weekly_used_x10.is_some() {
            if let Some(plan) = refreshed.and_then(|reading| reading.plan) {
                read.plan = Some(plan);
            }
            return Ok(read);
        }
        if refreshed.is_none() {
            return Ok(read);
        }
    }
    refreshed.ok_or(missed)
}

/// The command that runs `exe serve --no-session-log --provider meta` where it can put nothing
/// on this machine's screen, whatever a signed-out host decides to do:
///
/// - macOS: under [`super::login_drive::NO_DESK`] (LaunchServices, Apple events, `open`,
///   `osascript` and the launch of an installed app denied; the network and the Keychain left
///   alone, so a signed-in host still answers), the sandbox vendor sign-ins run under.
/// - Elsewhere nothing here keeps a sign-in page closed, so no command is made.
///
/// # Errors
///
/// This system has no way to keep the host off the screen, or the sandbox cannot apply here
/// ([`super::login_drive::sandbox_refusal`]: this process already runs under another).
pub fn serve_command(exe: &Path) -> Result<aterm_uds::spawnfd::Command, String> {
    let serve = ["serve", "--no-session-log", "--provider", "meta"].map(std::ffi::OsString::from);
    #[cfg(test)]
    if ON_DESK.get() {
        let mut cmd = aterm_uds::spawnfd::command(exe);
        cmd.args(serve);
        return Ok(cmd);
    }
    #[cfg(target_os = "macos")]
    {
        let sandbox = Path::new(super::login_drive::SANDBOX_EXEC);
        if let Some(why) = sandbox_refusal_once(sandbox) {
            return Err(why);
        }
        let mut cmd = aterm_uds::spawnfd::command(sandbox);
        cmd.args(super::login_drive::no_desk_argv(exe, &serve));
        Ok(cmd)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (exe, serve);
        Err(String::from(
            "muse serve is not run for a usage read on this system: nothing here keeps a sign-in \
             page off the screen",
        ))
    }
}

/// [`super::login_drive::sandbox_refusal`], tried once a process: whether this process runs
/// under another sandbox does not change while it lives.
#[cfg(target_os = "macos")]
fn sandbox_refusal_once(sandbox: &Path) -> Option<String> {
    static TRIED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TRIED
        .get_or_init(|| super::login_drive::sandbox_refusal(sandbox))
        .clone()
}

#[cfg(test)]
thread_local! {
    /// Negative control, on this thread: the host runs as any child does, under no sandbox
    /// ([`serve_command`]). Per thread, so a test that sets it never reaches another test's
    /// command.
    pub(super) static ON_DESK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs [`read_usage`] over [`serve_command`]'s host with only `env` (the caller's `PATH`, the
/// signed-in home and any proxies among them) within [`BUDGET`]; the host is ended once the
/// reading returns. Memory-only sessions: the fetch starts none and writes no session log.
///
/// # Errors
///
/// No reading: [`Missed::SignedOut`] where the host's refusal or its standard error said its
/// login is gone, else [`Missed::Unanswered`] (the permit is another read's, no host could be
/// run off-screen, or it answered nothing).
pub fn fetch(
    exe: &Path,
    env: Vec<(&str, std::ffi::OsString)>,
    cwd: &Path,
    permit: super::provider_read::Permit,
) -> Result<Reading, Missed> {
    let home = env
        .iter()
        .rev()
        .find(|(key, _)| *key == "HOME")
        .map(|(_, value)| std::path::PathBuf::from(value))
        .ok_or(Missed::Unanswered)?;
    let config = env
        .iter()
        .rev()
        .find(|(key, _)| *key == "XDG_CONFIG_HOME")
        .map(|(_, value)| std::path::PathBuf::from(value))
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("muse");
    if !permit.matches(super::provider_read::Read::MuseUsage, &config) {
        return Err(Missed::Unanswered);
    }
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = match serve_command(exe) {
        Ok(cmd) => cmd,
        Err(why) => {
            aterm_log::info!("muse usage: not read: {why}");
            return Err(Missed::Unanswered);
        }
    };
    cmd.env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|_| Missed::Unanswered)?;
    // Its standard error, read to its end on a thread of its own (the host never blocks on a
    // full pipe) and kept only as whether it said the login is gone.
    let said = child.stderr.take().map(|from| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut signed_out = false;
            super::login_drive::read_lines_within(from, SAID_MAX, MESSAGES_MAX, |line| {
                signed_out |= says_signed_out(&line);
            });
            let _ = tx.send(signed_out);
        });
        rx
    });
    let streams = (child.stdout.take(), child.stdin.take());
    let now = Instant::now();
    let deadline = now.checked_add(BUDGET).unwrap_or(now);
    let ran = match streams {
        (Some(from), Some(to)) => read_usage(
            AppServer {
                from: Box::new(from),
                to: Box::new(to),
            },
            deadline,
        ),
        _ => Err(Missed::Unanswered),
    };
    super::login_drive::end(&mut child);
    match ran {
        Ok(reading) => Ok(reading),
        Err(Missed::SignedOut) => Err(Missed::SignedOut),
        // The host is ended, so its standard error has closed: what it said there is read by
        // the hang detector's bound, never waited on for longer.
        Err(Missed::Unanswered) => match said.map(|rx| rx.recv_timeout(SAID_WAIT)) {
            Some(Ok(true)) => Err(Missed::SignedOut),
            _ => Err(Missed::Unanswered),
        },
    }
}

/// How long an ended host's standard error is waited for: it closed with the host, so this is a
/// hang detector for a grandchild that kept the pipe.
const SAID_WAIT: Duration = Duration::from_secs(5);

#[cfg(test)]
#[path = "muse_usage_tests.rs"]
mod tests;
