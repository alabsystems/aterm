// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Muse's fresh subscription usage read: `muse serve` over MSP (one JSON-RPC message
//! per line on its standard streams), `usage/refresh` (experimental, ADR 32563 D6 as the
//! binary's schema names it). The refresh mints at most once per host per 60 seconds and
//! spends no model call; a refused mint, an absent subscription with no usage object, and
//! a signed-out store are all plain absences here, never a figure. Nothing the host sent
//! is kept or written beyond the parsed numbers.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The host closed its output, or would not read.
    Gone,
    /// The deadline ran out.
    Late,
    /// It answered with an error, or something this client does not read.
    Unread,
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
                return message.get("result").cloned().ok_or(Fault::Unread);
            }
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), Fault> {
        self.send(&json!({"method": method}))
    }
}

/// Greets `server` (`initialize` with `hello`, then `initialized`) by `deadline`.
fn hello_with(server: AppServer, deadline: Instant, hello: Value) -> Option<Rpc> {
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
    rpc.call("initialize", hello).ok()?;
    rpc.notify("initialized").ok()?;
    Some(rpc)
}

/// The whole dialogue's budget: host start, the mint's network read, and the answer. A
/// hang detector, not a latency expectation (a live read answers in seconds).
pub const BUDGET: Duration = Duration::from_secs(20);

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
/// stale read resurrects it. `None` where neither answered one.
#[must_use]
pub fn read_usage(server: AppServer, deadline: Instant) -> Option<Reading> {
    let hello = json!({
        "clientInfo": {"name": "aterm", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi": true}
    });
    let mut rpc = hello_with(server, deadline, hello)?;
    let refreshed = match rpc.call("usage/refresh", json!({})) {
        Ok(result) if explicitly_inactive(&result) => return None,
        Ok(result) => parse(&result),
        Err(_) => None,
    };
    if refreshed
        .as_ref()
        .is_some_and(|reading| reading.weekly_used_x10.is_some())
    {
        return refreshed;
    }
    let read = rpc
        .call("usage/read", json!({}))
        .ok()
        .and_then(|result| parse(&result));
    if let Some(mut read) = read {
        if read.weekly_used_x10.is_some() {
            if let Some(plan) = refreshed.and_then(|reading| reading.plan) {
                read.plan = Some(plan);
            }
            return Some(read);
        }
        if refreshed.is_none() {
            return Some(read);
        }
    }
    refreshed
}

/// Runs [`read_usage`] over `exe serve --no-session-log --provider meta` with only `env`
/// (the caller's `PATH`, the signed-in home and any proxies among them), its standard
/// error discarded, within [`BUDGET`]; the host is ended once the reading returns.
/// Memory-only sessions: the fetch starts none and writes no session log.
#[must_use]
pub fn fetch(exe: &Path, env: Vec<(&str, std::ffi::OsString)>, cwd: &Path) -> Option<Reading> {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.arg("serve")
        .arg("--no-session-log")
        .arg("--provider")
        .arg("meta")
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().ok()?;
    let streams = (child.stdout.take(), child.stdin.take());
    let ran = match streams {
        (Some(from), Some(to)) => read_usage(
            AppServer {
                from: Box::new(from),
                to: Box::new(to),
            },
            Instant::now() + BUDGET,
        ),
        _ => None,
    };
    super::login_drive::end(&mut child);
    ran
}

#[cfg(test)]
#[path = "muse_usage_tests.rs"]
mod tests;
