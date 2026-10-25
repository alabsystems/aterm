// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Codex's browser sign-in through its app server (`codex app-server`, JSON-RPC over its
//! standard input and output, one message per line), as a renewal runs it in a staging
//! `CODEX_HOME` (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md` §6, lab gate (d) on codex-old
//! 0.164.0): `initialize`, then `initialized`; `account/login/start` with `type = "chatgpt"`,
//! which answers a `loginId` and an `authUrl` and opens no browser; the page handed to the
//! [`Approver`] on the pinned hosts only; the `account/login/completed` notification for that
//! login; and `account/read`, whose ChatGPT account's email is the identity proof. Nothing here
//! writes the URL, the login's id or any message the server sent.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use super::login_drive::{Approval, Approver, Drove, Handoff, Outcome, https_host, pinned};
use crate::ai::connector::Completion;

/// The most of one app-server message that is read.
pub const MESSAGE_MAX: usize = 256 * 1024;

/// The most messages of one sign-in that are read.
pub const MESSAGES_MAX: usize = 4096;

/// The method of the notification that ends a login.
pub const COMPLETED: &str = "account/login/completed";

/// The app server's two streams: what it writes, and what it reads.
pub struct AppServer {
    /// Its standard output.
    pub from: Box<dyn Read + Send>,
    /// Its standard input.
    pub to: Box<dyn Write + Send>,
}

/// The JSON-RPC client over an [`AppServer`].
struct Rpc {
    to: Box<dyn Write + Send>,
    from: mpsc::Receiver<Value>,
    stash: Vec<Value>,
    next: u64,
    deadline: Instant,
}

/// Why a call came to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The server closed its output, or would not read.
    Gone,
    /// The deadline ran out.
    Late,
    /// It answered with an error, or something this client does not read.
    Unread,
}

impl Rpc {
    fn send(&mut self, message: &Value) -> Result<(), Fault> {
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
                return message.get("result").cloned().ok_or(Fault::Unread);
            }
            if message.get("method").is_some() {
                self.stash.push(message);
            }
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), Fault> {
        self.send(&json!({"method": method}))
    }

    fn notification(&mut self, method: &str, of: &dyn Fn(&Value) -> bool) -> Result<Value, Fault> {
        if let Some(at) = self
            .stash
            .iter()
            .position(|m| m.get("method").and_then(Value::as_str) == Some(method) && of(m))
        {
            return Ok(self.stash.remove(at));
        }
        loop {
            let message = self.next_message()?;
            if message.get("method").and_then(Value::as_str) == Some(method) && of(&message) {
                return Ok(message);
            }
        }
    }
}

/// The login `account/login/start` began: its id and its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    /// `loginId`.
    pub login_id: String,
    /// `authUrl`.
    pub auth_url: String,
}

/// Reads `account/login/start`'s ChatGPT answer.
#[must_use]
pub fn started_of(result: &Value) -> Option<Started> {
    (result.get("type")?.as_str()? == "chatgpt").then_some(())?;
    Some(Started {
        login_id: result.get("loginId")?.as_str()?.to_owned(),
        auth_url: result.get("authUrl")?.as_str()?.to_owned(),
    })
}

/// The ChatGPT account `account/read` answers: its email (where it names one) and plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// `email`.
    pub email: Option<String>,
    /// `planType`, one bounded word.
    pub plan: Option<String>,
}

/// Reads `account/read`'s answer: `None` unless it names a ChatGPT account.
#[must_use]
pub fn account_of(result: &Value) -> Option<Account> {
    let account = result.get("account")?;
    (account.get("type")?.as_str()? == "chatgpt").then_some(())?;
    Some(Account {
        email: account
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_owned),
        plan: account
            .get("planType")
            .and_then(Value::as_str)
            .map(|p| super::one_line(p, 32))
            .filter(|p| !p.is_empty()),
    })
}

/// The browser sign-in over `server`, its page handed to `approver` on `hosts` only, all by
/// `deadline`: [`Outcome::SignedIn`] with the email `account/read` names, else how it ended.
pub fn login(
    server: AppServer,
    approver: &mut dyn Approver,
    hosts: &[&str],
    email: Option<&str>,
    deadline: Instant,
) -> Drove {
    let (tx, rx) = mpsc::channel();
    let completed = Arc::new(std::sync::Mutex::new(Vec::<Option<String>>::new()));
    let closed = Arc::new(AtomicBool::new(false));
    {
        let completed = Arc::clone(&completed);
        let closed = Arc::clone(&closed);
        let from = server.from;
        std::thread::spawn(move || {
            super::login_drive::read_lines_within(from, MESSAGE_MAX, MESSAGES_MAX, |line| {
                let Ok(message) = aterm_json::from_str::<Value>(line.trim()) else {
                    return;
                };
                if message.get("method").and_then(Value::as_str) == Some(COMPLETED) {
                    let id = message
                        .pointer("/params/loginId")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if let Ok(mut ids) = completed.lock() {
                        ids.push(id);
                    }
                }
                let _ = tx.send(message);
            });
            closed.store(true, Ordering::SeqCst);
        });
    }
    let mut rpc = Rpc {
        to: server.to,
        from: rx,
        stash: Vec::new(),
        next: 0,
        deadline,
    };
    let ended = |fault: Fault, host: Option<String>| Drove {
        outcome: match fault {
            Fault::Late => Outcome::Hung,
            Fault::Gone => Outcome::Exited(None),
            Fault::Unread => Outcome::Failed,
        },
        host,
    };
    let hello = json!({"clientInfo": {"name": "aterm", "title": "aterm", "version": env!("CARGO_PKG_VERSION")}});
    if let Err(fault) = rpc
        .call("initialize", hello)
        .and_then(|_| rpc.notify("initialized"))
    {
        return ended(fault, None);
    }
    let started = match rpc.call("account/login/start", json!({"type": "chatgpt"})) {
        Ok(result) => match started_of(&result) {
            Some(started) => started,
            None => return ended(Fault::Unread, None),
        },
        Err(fault) => return ended(fault, None),
    };
    let host = https_host(&started.auth_url);
    let cancel = |rpc: &mut Rpc| {
        let _ = rpc.call(
            "account/login/cancel",
            json!({"loginId": started.login_id.clone()}),
        );
    };
    if !pinned(&started.auth_url, hosts) {
        cancel(&mut rpc);
        return Drove {
            outcome: Outcome::OffPin,
            host,
        };
    }
    let login_id = started.login_id.clone();
    let finished = || {
        closed.load(Ordering::SeqCst)
            || completed.lock().is_ok_and(|ids| {
                ids.iter()
                    .any(|id| id.as_deref().is_none_or(|id| id == login_id))
            })
    };
    let page = match approver.approve(&Handoff {
        url: &started.auth_url,
        code: None,
        completion: Completion::Loopback,
        hosts,
        email,
        finished: &finished,
        deadline,
    }) {
        Approval::Approved { page } => page,
        Approval::NeedsYou(why) => {
            rpc.deadline = Instant::now() + Duration::from_secs(5);
            cancel(&mut rpc);
            return Drove {
                outcome: Outcome::NeedsYou(why),
                host,
            };
        }
        Approval::NeedsSignIn(why) => {
            rpc.deadline = Instant::now() + Duration::from_secs(5);
            cancel(&mut rpc);
            return Drove {
                outcome: Outcome::NeedsSignIn(why),
                host,
            };
        }
        Approval::Refused(why) => {
            rpc.deadline = Instant::now() + Duration::from_secs(5);
            cancel(&mut rpc);
            return Drove {
                outcome: Outcome::Refused(why),
                host,
            };
        }
    };
    let login_id = started.login_id.clone();
    let done = match rpc.notification(COMPLETED, &|m| {
        m.pointer("/params/loginId")
            .and_then(Value::as_str)
            .is_none_or(|id| id == login_id)
    }) {
        Ok(done) => done,
        Err(fault) => return ended(fault, host),
    };
    if done.pointer("/params/success").and_then(Value::as_bool) != Some(true) {
        return Drove {
            outcome: Outcome::Failed,
            host,
        };
    }
    let account = match rpc.call("account/read", json!({})) {
        Ok(result) => account_of(&result),
        Err(fault) => return ended(fault, host),
    };
    match account {
        Some(account) => Drove {
            outcome: Outcome::SignedIn {
                email: account.email,
                page,
            },
            host,
        },
        None => Drove {
            outcome: Outcome::Failed,
            host,
        },
    }
}

/// One Codex login's usage as `account/rateLimits/read` answers it in that login's own home
/// (lab-measured on codex-old 0.164.0: `rateLimits` holds `primary` and `secondary`, each a
/// `usedPercent`, `windowDurationMins` and `resetsAt`, beside `rateLimitReachedType`; the
/// result's `ordinaryUsageAllowed` says whether the plan still serves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// The pool's sample of it: its five-hour window as the session, its week as the week, each
    /// share taken up to the next whole percent.
    pub sample: crate::harness::accounts::Sample,
    /// The wall it stands at, where the server says ordinary use is not allowed, names a reached
    /// limit, or shows a full window: the limit's name ([`crate::harness::accounts::SESSION_LIMIT`]
    /// or [`crate::harness::accounts::WEEK_LIMIT`]) and the reset it named.
    pub wall: Option<(&'static str, Option<i64>)>,
}

/// Reads `account/rateLimits/read`'s answer at `now`: `None` where it holds no `rateLimits`.
#[must_use]
pub fn usage_of(result: &Value, now: i64) -> Option<Usage> {
    use crate::ai::codex_limits::{Bucket, Slot, Window};
    use crate::harness::accounts::{SESSION_LIMIT, Sample, WEEK_LIMIT};
    let limits = result.get("rateLimits")?.as_object()?;
    let windows: Vec<Window> = [("primary", Slot::Primary), ("secondary", Slot::Secondary)]
        .into_iter()
        .filter_map(|(key, slot)| {
            let w = limits.get(key)?.as_object()?;
            Some(Window {
                slot,
                used_percent: w.get("usedPercent")?.as_f64()?,
                window_minutes: w.get("windowDurationMins").and_then(Value::as_u64),
                resets_at: w.get("resetsAt").and_then(Value::as_i64),
            })
        })
        .collect();
    // Clamped to 0..=100 first, so the cast neither truncates nor wraps.
    let whole = |share: f64| share.ceil().clamp(0.0, 100.0) as u32;
    let of = |bucket: Bucket| windows.iter().find(|w| w.bucket() == bucket);
    let (session, week) = (of(Bucket::FiveHour), of(Bucket::Week));
    let sample = Sample {
        session_pct: session.map(|w| whole(w.used_percent)),
        session_reset: session.and_then(|w| w.resets_at),
        week_pct: week.map(|w| whole(w.used_percent)),
        week_reset: week.and_then(|w| w.resets_at),
        session_read_at: session.map(|_| now),
        week_read_at: week.map(|_| now),
        at: now,
        by: String::new(),
    };
    let full = [(session, SESSION_LIMIT), (week, WEEK_LIMIT)]
        .into_iter()
        .find_map(|(w, name)| {
            w.filter(|w| w.used_percent >= 100.0)
                .map(|w| (name, w.resets_at))
        });
    let refused = result.get("ordinaryUsageAllowed").and_then(Value::as_bool) == Some(false)
        || limits
            .get("rateLimitReachedType")
            .is_some_and(|t| !t.is_null());
    let wall = full.or_else(|| {
        refused.then(|| {
            let latest = [session, week]
                .into_iter()
                .flatten()
                .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
            match latest {
                Some(w) if Some(w) == week => (WEEK_LIMIT, w.resets_at),
                Some(w) => (SESSION_LIMIT, w.resets_at),
                None => (SESSION_LIMIT, None),
            }
        })
    });
    Some(Usage { sample, wall })
}

/// A JSON-RPC client over `server`, its `initialize` and `initialized` sent, by `deadline`.
fn hello(server: AppServer, deadline: Instant) -> Option<Rpc> {
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
        stash: Vec::new(),
        next: 0,
        deadline,
    };
    let hello = json!({"clientInfo": {"name": "aterm", "title": "aterm", "version": env!("CARGO_PKG_VERSION")}});
    rpc.call("initialize", hello).ok()?;
    rpc.notify("initialized").ok()?;
    Some(rpc)
}

/// `account/rateLimits/read` over `server` (after `initialize`), by `deadline`, at `now`: the
/// usage of the login of the home the server runs in, or `None` where it did not answer one.
/// Nothing it sent is kept or written.
pub fn read_usage(server: AppServer, deadline: Instant, now: i64) -> Option<Usage> {
    let mut rpc = hello(server, deadline)?;
    let result = rpc.call("account/rateLimits/read", json!({})).ok()?;
    usage_of(&result, now)
}

/// `account/read` over `server` (after `initialize`), by `deadline`: the ChatGPT account the
/// login of the home the server runs in names ([`account_of`]), or `None`.
pub fn read_account(server: AppServer, deadline: Instant) -> Option<Account> {
    let mut rpc = hello(server, deadline)?;
    let result = rpc.call("account/read", json!({})).ok()?;
    account_of(&result)
}

/// Runs `f` over Codex's app server (`codex app-server`, the program its sign-in runs:
/// [`super::login_drive::program_of`]) started in `home` with only
/// [`super::login_drive::PASSED`], the caller's `PATH` and `CODEX_HOME`, its standard error
/// discarded, given `wait` (`f`'s deadline); the server is ended once `f` returns. `None` where
/// the server did not start, or `cli` is not Codex.
pub fn run_app_server<T>(
    cli: crate::harness::accounts::Cli,
    home: &std::path::Path,
    wait: Duration,
    f: impl FnOnce(AppServer, Instant) -> T,
) -> Option<T> {
    use aterm_uds::spawnfd::Stdio;
    if cli != crate::harness::accounts::Cli::Codex {
        return None;
    }
    let user_home = aterm_types::dirs::home_dir()?;
    let program = super::login_drive::program_of(cli, &user_home);
    let mut cmd = aterm_uds::spawnfd::command(&program);
    cmd.arg("app-server")
        .env_clear()
        .envs(super::login_drive::PASSED.iter().filter_map(|name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(|v| (*name, v))
        }))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("CODEX_HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().ok()?;
    let streams = (child.stdout.take(), child.stdin.take());
    let ran = match streams {
        (Some(from), Some(to)) => Some(f(
            AppServer {
                from: Box::new(from),
                to: Box::new(to),
            },
            Instant::now() + wait,
        )),
        _ => None,
    };
    super::login_drive::end(&mut child);
    ran
}

#[cfg(test)]
#[path = "codex_app_login_tests.rs"]
pub(crate) mod tests;
