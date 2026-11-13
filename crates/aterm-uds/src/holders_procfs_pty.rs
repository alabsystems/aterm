// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's pseudo-terminal masters, answering [`crate::holders::pty_master_held`] from the
//! terminal alone. Closing a master hangs up its terminal (`pty_close` calls `tty_vhangup` on
//! the terminal end, `drivers/tty/pty.c`), and the hangup takes the terminal from every process
//! of its session (`tty_signal_session_leader` clears each one's `signal->tty`), as the session
//! leader's exit does (`disassociate_ctty`); a terminal whose master closed cannot be taken
//! again (a new open fails, `pty_open`'s `TTY_OTHER_CLOSED`, and the hangup left every open
//! descriptor of it `hung_up_tty_fops`). So a live process whose `tty_nr` still
//! names a devpts terminal has that terminal's master open somewhere, whoever holds it: another
//! user's process (`sudo` with `use_pty`, `su` from another user's terminal), one in another pid
//! namespace (`docker exec -t`), or none at all while a socket carries the master in flight. No
//! scan of this uid's descriptor tables sees all of those, so none is read.

/// The major number of every devpts terminal (`UNIX98_PTY_SLAVE_MAJOR`); its minor, up to 20
/// bits, is the terminal's index.
const PTS_MAJOR: u32 = 136;

/// Whether `tty_nr` (`/proc/<pid>/stat`'s encoding, the kernel's 32-bit `new_encode_dev`)
/// numbers a devpts terminal. A number past 32 bits is no `tty_nr`, so a 64-bit `dev_t` passed
/// unmasked is none.
fn is_devpts(tty_nr: u64) -> bool {
    u32::try_from(tty_nr).is_ok_and(|tty_nr| (tty_nr >> 8) & 0xfff == PTS_MAJOR)
}

/// Whether a live process holds the master of the terminal `tty_nr` numbers, a live process's
/// controlling terminal: `Some(true)` for a devpts terminal (this module's head says why), never
/// `Some(false)`, and `None` for a terminal of another driver (a virtual console, a serial line).
pub(in crate::holders) fn pty_master_held_of(tty_nr: u64) -> Option<bool> {
    is_devpts(tty_nr).then_some(true)
}

#[cfg(test)]
#[path = "holders_procfs_pty_tests.rs"]
mod tests;
