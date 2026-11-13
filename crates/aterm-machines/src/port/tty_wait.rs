// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Forward a cancellation aimed at an attached CLI to its own unreaped child.

use aterm_uds::spawnfd::Child;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

/// A deferred signal can target the waiting CLI alone (System State's exact-child Stop).
/// Forward it once to this direct child, never a discovered foreground process group. sudo(8)
/// relays user-process signals to its command; a shared foreground group may already have
/// delivered an ordinary keyboard interrupt, whose forwarding sudo handles itself.
/// A wait error remains an error, but returns only after this actual child positively exits
/// and is reaped. Lost numeric custody stays retained without forwarding a signal.
pub(super) fn wait(
    child: &mut Child,
    stopped: impl FnMut() -> Option<i32>,
) -> io::Result<ExitStatus> {
    wait_with(child, stopped, Child::try_wait)
}

fn wait_with(
    child: &mut Child,
    mut stopped: impl FnMut() -> Option<i32>,
    mut poll: impl FnMut(&mut Child) -> io::Result<Option<ExitStatus>>,
) -> io::Result<ExitStatus> {
    let mut forwarded = false;
    let mut failed = None;
    loop {
        match poll(child) {
            Ok(Some(status)) => return failed.map_or(Ok(status), Err),
            Ok(None) => {}
            Err(error) => {
                failed.get_or_insert(error);
            }
        }
        if !forwarded
            && child.owns_unreaped()
            && let Some(signal) = stopped()
        {
            match i32::try_from(child.id()) {
                Ok(pid) => {
                    // SAFETY: this is the direct child just spawned by run_tty, and its
                    // actual handle still owns its unreaped identity. Its pid cannot be
                    // reused before this owning wait reaps it.
                    let sent = unsafe { libc::kill(pid, signal) };
                    if sent != 0 {
                        aterm_log::info!(
                            "the attached child's cancellation signal was refused: {}",
                            io::Error::last_os_error()
                        );
                    }
                }
                Err(error) => {
                    failed.get_or_insert_with(|| io::Error::other(error));
                }
            }
            forwarded = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
#[path = "tty_wait_tests.rs"]
mod tests;
