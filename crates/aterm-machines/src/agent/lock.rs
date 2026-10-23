// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lock-screen` agent command using [`System::lock_screen`].

use crate::model::PValue;
use crate::port::{Cmd, System};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long the session may take to read locked once loginwindow took the request.
pub const LOCK_HANG: Duration = Duration::from_secs(10);

/// The pause between two readings.
const READ_PAUSE: Duration = Duration::from_millis(250);

/// `ioreg -n Root -d1 -a`: the registry root as a property list.
#[must_use]
pub fn ioreg_root_cmd() -> Cmd {
    Cmd::new("/usr/sbin/ioreg").args(["-n", "Root", "-d1", "-a"])
}

/// Whether the login session of the account `uid` reads locked in what [`ioreg_root_cmd`] printed.
///
/// # Errors
/// One line when the output is not that property list, or lists no session for `uid`.
pub fn session_locked(xml: &[u8], uid: u32) -> Result<bool, String> {
    let root = crate::plist::parse_domain(xml).map_err(|e| format!("ioreg: {e}"))?;
    let Some((_, Ok(PValue::Array(sessions)))) = root.iter().find(|(k, _)| k == "IOConsoleUsers")
    else {
        return Err("ioreg lists no IOConsoleUsers".into());
    };
    let want = PValue::Int(i64::from(uid));
    for session in sessions {
        let PValue::Dict(entries) = session else {
            continue;
        };
        let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        if get("kCGSSessionUserIDKey") == Some(&want) {
            return Ok(get("CGSSessionScreenIsLocked") == Some(&PValue::Bool(true)));
        }
    }
    Err(format!("ioreg lists no login session for uid {uid}"))
}

/// `aterm machines agent lock-screen` ([`System::lock_screen`], [`ioreg_root_cmd`]).
pub fn lock_screen(sys: &dyn System, out: &mut dyn Write, hang: Duration) -> ExitCode {
    let mut done = |line: String, code: ExitCode| {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
        code
    };
    if let Err(why) = sys.lock_screen() {
        return done(format!("not locked: {why}"), ExitCode::FAILURE);
    }
    let uid = sys.euid();
    let start = Instant::now();
    loop {
        let last = match sys.run_complete_stdout(&ioreg_root_cmd()) {
            Ok(ran) if ran.success() => match session_locked(&ran.stdout, uid) {
                Ok(true) => return done("locked".into(), ExitCode::SUCCESS),
                Ok(false) => format!(
                    "loginwindow took the request, and the session of uid {uid} still reads \
                     unlocked"
                ),
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
mod tests {
    use super::*;
    use crate::agent::{Layout, run};
    use crate::port::{Fake, Ran};
    use std::path::Path;

    /// `ioreg -n Root -d1 -a` in the macOS 27.0 VM with the admin (uid 501) logged in and locked.
    const IOREG_LOCKED: &[u8] = include_bytes!("../../fixtures/ioreg/vm-27.0-root-locked.plist");

    /// The same, another boot, the session logged in and unlocked.
    const IOREG_UNLOCKED: &[u8] =
        include_bytes!("../../fixtures/ioreg/vm-27.0-root-unlocked.plist");

    fn admin_fake() -> Fake {
        let mut fake = Fake::root();
        fake.euid = 501;
        fake
    }

    fn ioreg_key() -> String {
        ioreg_root_cmd().display()
    }

    #[test]
    fn the_lock_reads_back_from_the_sessions_own_console_entry() {
        assert_eq!(session_locked(IOREG_LOCKED, 501), Ok(true));
        assert_eq!(session_locked(IOREG_UNLOCKED, 501), Ok(false));
        let err = session_locked(IOREG_LOCKED, 502).unwrap_err();
        assert!(err.contains("no login session for uid 502"), "{err}");
        assert!(session_locked(b"not a plist", 501).is_err());
    }

    #[test]
    fn lock_screen_is_an_agent_verb_that_locks_through_the_port_and_reads_it_back() {
        let fake = admin_fake();
        fake.on(
            &ioreg_key(),
            Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
        );
        let mut out = Vec::new();
        let code = run(
            &fake,
            &Layout::installed(),
            Path::new("/x"),
            &["lock-screen"],
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(String::from_utf8_lossy(&out), "locked\n");
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(fake.calls(), ["lock-screen".to_owned(), ioreg_key()]);
    }

    #[test]
    fn a_refused_lock_is_not_locked_and_reads_nothing() {
        let mut fake = admin_fake();
        fake.lock_refused = Some("SACLockScreenImmediate answered 22".into());
        let mut out = Vec::new();
        let code = run(
            &fake,
            &Layout::installed(),
            Path::new("/x"),
            &["lock-screen"],
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "not locked: SACLockScreenImmediate answered 22\n"
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(fake.calls(), ["lock-screen"]);
    }

    #[test]
    fn a_lock_taken_but_never_read_back_is_not_locked() {
        let fake = admin_fake();
        fake.on(
            &ioreg_key(),
            Ran::ok(&String::from_utf8_lossy(IOREG_UNLOCKED)),
        );
        let mut out = Vec::new();
        let code = lock_screen(&fake, &mut out, Duration::ZERO);
        let out = String::from_utf8_lossy(&out);
        assert!(
            out.starts_with("not locked: loginwindow took the request"),
            "{out}"
        );
        assert_eq!(code, ExitCode::FAILURE);
        let fake = admin_fake();
        fake.on(
            &ioreg_key(),
            Ran::ok(&String::from_utf8_lossy(IOREG_UNLOCKED)),
        );
        fake.on(
            &ioreg_key(),
            Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
        );
        let mut out = Vec::new();
        assert_eq!(
            lock_screen(&fake, &mut out, Duration::from_secs(60)),
            ExitCode::SUCCESS
        );
        assert_eq!(fake.calls().len(), 3, "{:?}", fake.calls());
    }

    /// A diagnostic tail is not a complete console-user record, even if its
    /// remaining bytes form a plist which says locked.
    #[test]
    fn a_lock_report_requires_a_complete_console_reading() {
        let fake = admin_fake();
        fake.on(
            &ioreg_key(),
            Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
        );
        let mut out = Vec::new();
        assert_eq!(
            lock_screen(&fake, &mut out, Duration::ZERO),
            ExitCode::SUCCESS
        );
        assert_eq!(out, b"locked\n");
        assert_eq!(fake.calls(), ["lock-screen".to_owned(), ioreg_key()]);

        let fake = admin_fake();
        fake.on(
            &ioreg_key(),
            Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
        );
        fake.refuse_complete_stdout(&ioreg_key(), "stdout did not reach EOF");
        let mut out = Vec::new();
        let code = lock_screen(&fake, &mut out, Duration::ZERO);
        let out = String::from_utf8_lossy(&out);
        assert_eq!(fake.calls(), ["lock-screen".to_owned(), ioreg_key()]);
        assert!(
            code == ExitCode::FAILURE && out.contains("stdout did not reach EOF"),
            "MACHINES_LOCK_ACCEPTED_INCOMPLETE_CONSOLE_RECORD: {code:?} {out}"
        );
    }
}
