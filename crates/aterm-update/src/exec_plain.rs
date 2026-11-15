// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A plain `exec` that excludes process-table reads and restores SIGPIPE on failure.

use std::io;
use std::os::unix::process::CommandExt as _;

/// Replace this process with `command`'s program, restoring `SIGPIPE` on failure.
/// A process-table read that does not settle within the exec gate's bound refuses the exec
/// with `WouldBlock`, leaving the command and signal disposition untouched.
pub fn exec_keeping_sigpipe(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    let _closed = match aterm_sysprobe::exec_gate::close(aterm_sysprobe::exec_gate::SETTLES_WITHIN)
    {
        Ok(closed) => closed,
        Err(reading) => {
            return io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("refusing exec while {reading}"),
            );
        }
    };
    let saved = sigpipe_disposition();
    let error = command.exec();
    if let Some(saved) = saved.as_ref() {
        restore_sigpipe(saved);
    }
    error
}

/// SIGPIPE's disposition, read without changing it, so a failed exec can put it back.
pub(crate) fn sigpipe_disposition() -> Option<libc::sigaction> {
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value.
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null `act` only reads the current disposition into `old`.
    (unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut old) } == 0).then_some(old)
}

/// Put SIGPIPE back to `saved` (from [`sigpipe_disposition`]).
pub(crate) fn restore_sigpipe(saved: &libc::sigaction) {
    // SAFETY: `saved` is a disposition this process read; a null `oldact` is allowed.
    unsafe { libc::sigaction(libc::SIGPIPE, saved, std::ptr::null_mut()) };
}

#[cfg(test)]
#[path = "exec_plain_tests.rs"]
mod tests;
