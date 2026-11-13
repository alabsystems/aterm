// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::session_reach`] on the live system: whether a login session can take clicks
//! an automatic step makes in its apps now, read from two `ioreg` runs through the port (the
//! registry root's console sessions, and the HID system's idle time), so nothing here reaches
//! WindowServer or asks anything.

use super::{KEYS_REST, SessionReach, System};
use crate::agent::lock::{ConsoleSession, console_session, ioreg_root_cmd};

/// The reach of the session of the account `uid`, read now on `sys`.
pub(super) fn read(sys: &dyn System, uid: u32) -> SessionReach {
    let root = match sys.run_complete_stdout(&ioreg_root_cmd()) {
        Ok(ran) if ran.success() => ran.stdout,
        _ => return SessionReach::Unread,
    };
    let idle_ns = sys
        .run(&crate::facts::idle_cmd())
        .ok()
        .filter(super::Ran::success)
        .and_then(|ran| crate::facts::parse_idle_ns(&ran.out()));
    reach_of(&root, idle_ns, uid)
}

/// The reach of the session of `uid` in what [`ioreg_root_cmd`] printed (`root`, its console
/// entry as `lock-screen` reads it back, [`console_session`]), with the HID system's idle time
/// in nanoseconds (`idle_ns`, `None` unread).
fn reach_of(root: &[u8], idle_ns: Option<u64>, uid: u32) -> SessionReach {
    match console_session(root, uid) {
        Err(_) => SessionReach::Unread,
        Ok(
            None
            | Some(ConsoleSession {
                on_console: false, ..
            }),
        ) => SessionReach::SwitchedAway,
        Ok(Some(ConsoleSession { locked: true, .. })) => SessionReach::Locked,
        Ok(Some(_)) => match idle_ns {
            None => SessionReach::Unread,
            Some(ns) if u128::from(ns) < KEYS_REST.as_nanos() => SessionReach::KeysInUse,
            Some(_) => SessionReach::Open,
        },
    }
}

#[cfg(test)]
#[path = "session_reach_tests.rs"]
mod tests;
