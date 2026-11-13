// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lock-screen` agent command using [`System::lock_screen`].

use crate::model::PValue;
use crate::port::{Cmd, System};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long the session may take to read locked once loginwindow took the request.
pub const LOCK_HANG: Duration = Duration::from_secs(60);

/// The pause between two readings.
const READ_PAUSE: Duration = Duration::from_millis(250);

/// `ioreg -n Root -d1 -a`: the registry root as a property list.
#[must_use]
pub fn ioreg_root_cmd() -> Cmd {
    Cmd::new("/usr/sbin/ioreg").args(["-n", "Root", "-d1", "-a"])
}

/// One login session's entry among the registry root's console users ([`console_session`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsoleSession {
    /// It holds the console (`kCGSSessionOnConsoleKey`): no other login was switched to.
    pub on_console: bool,
    /// Its screen is locked (`CGSSessionScreenIsLocked`).
    pub locked: bool,
}

/// The console entry of the login session of the account `uid` in what [`ioreg_root_cmd`]
/// printed: what the `lock-screen` verb reads back and what a step that clicks into the session
/// reads first (`System::session_reach`). `Ok(None)` where it lists no session for `uid`.
///
/// # Errors
/// One line when the output is not that property list, or lists no console users.
pub fn console_session(xml: &[u8], uid: u32) -> Result<Option<ConsoleSession>, String> {
    let root = crate::plist::parse_domain(xml).map_err(|e| format!("ioreg: {e}"))?;
    let Some((_, Ok(PValue::Array(sessions)))) = root.iter().find(|(k, _)| k == "IOConsoleUsers")
    else {
        return Err("ioreg lists no IOConsoleUsers".into());
    };
    let want = PValue::Int(i64::from(uid));
    Ok(sessions.iter().find_map(|session| {
        let PValue::Dict(entries) = session else {
            return None;
        };
        let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        (get("kCGSSessionUserIDKey") == Some(&want)).then(|| ConsoleSession {
            on_console: get("kCGSSessionOnConsoleKey") == Some(&PValue::Bool(true)),
            locked: get("CGSSessionScreenIsLocked") == Some(&PValue::Bool(true)),
        })
    }))
}

/// `aterm machines agent lock-screen` ([`System::lock_screen`], [`ioreg_root_cmd`]).
pub fn lock_screen(sys: &dyn System, out: &mut dyn Write, hang: Duration) -> ExitCode {
    lock_screen_with_start(sys, out, hang, Instant::now)
}

fn lock_screen_with_start(
    sys: &dyn System,
    out: &mut dyn Write,
    hang: Duration,
    started: impl FnOnce() -> Instant,
) -> ExitCode {
    let mut done = |line: String, code: ExitCode| {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
        code
    };
    if let Err(why) = sys.lock_screen() {
        return done(format!("not locked: {why}"), ExitCode::FAILURE);
    }
    let uid = sys.euid();
    let start = started();
    loop {
        let last = match sys.run_complete_stdout(&ioreg_root_cmd()) {
            Ok(ran) if ran.success() => match console_session(&ran.stdout, uid) {
                Ok(Some(ConsoleSession { locked: true, .. })) => {
                    return done("locked".into(), ExitCode::SUCCESS);
                }
                Ok(Some(_)) => format!(
                    "loginwindow took the request, and the session of uid {uid} still reads \
                     unlocked"
                ),
                Ok(None) => format!("ioreg lists no login session for uid {uid}"),
                Err(why) => why,
            },
            Ok(ran) => format!("ioreg: {}", ran.describe()),
            Err(e) => format!("ioreg: {e}"),
        };
        if start.elapsed() >= hang {
            return done(format!("not locked: {last}"), ExitCode::FAILURE);
        }
        std::thread::sleep(READ_PAUSE);
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
