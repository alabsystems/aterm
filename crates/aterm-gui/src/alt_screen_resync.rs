// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window-side alternate-screen resync for OpenConsole
//! ([`apply_posture`], [`start`], [`mainscreen_via_host`]).

use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::Terminal;
use aterm_pty::ConsoleHostKind;

/// Hang detector for the required host-forwarded alternate-screen switch.
const MAINSCREEN_HANG: Duration = Duration::from_secs(60);

/// Whether a session on `host` recovers a dead app's alternate screen through the host.
pub(crate) fn host_resyncs(host: Option<ConsoleHostKind>) -> bool {
    host.is_some_and(ConsoleHostKind::passes_screen_switches_through)
}

/// The console-host posture of one session, in one hold.
pub(crate) fn apply_posture(t: &mut Terminal, policy: ResizePolicy, host: Option<ConsoleHostKind>) {
    t.set_console_host_posture(policy);
    t.set_win32_input_openconsole(host == Some(ConsoleHostKind::OpenConsole));
    t.set_host_resyncs_alt_screen(host_resyncs(host));
}

/// What the host's leave writes after its switch: the prompt on the dead screen
/// (`Terminal::dead_alt_screen_carry`) and the cursor it was composed for.
type Carry = Option<((u16, u16), String)>;

/// `t`'s carry, read in one hold.
fn carry_of(t: &Terminal) -> Carry {
    t.dead_alt_screen_carry().map(|text| {
        let cursor = t.cursor();
        ((cursor.row, cursor.col), text)
    })
}

/// The drain's second step, after the hold that parsed the prompt.
pub(crate) fn start(term: &Mutex<Terminal>, master: i32) {
    start_on(term, master, start_host_client);
}

/// [`start`] with its client. A client that did not start gives the request back
/// (`Terminal::abandon_alt_screen_resync`), so the next prompt on the stuck screen asks again.
fn start_on(
    term: &Mutex<Terminal>,
    master: i32,
    client: impl FnOnce(i32, Carry) -> io::Result<()>,
) {
    let carry = carry_of(&crate::term_lock(term));
    if !start_with(master, carry, client) {
        crate::term_lock(term).abandon_alt_screen_resync();
    }
}

/// Whether the host's leave client started. One still running from an earlier
/// request (`ResourceBusy`: the session runs one at a time) is no fault to log.
fn start_with(
    master: i32,
    carry: Carry,
    client: impl FnOnce(i32, Carry) -> io::Result<()>,
) -> bool {
    match client(master, carry) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::ResourceBusy => false,
        Err(e) => {
            aterm_log::warn!(
                "console host: a dead app's alternate screen stays up until the next prompt; \
                 the host's resync client did not start ({e})"
            );
            false
        }
    }
}

/// `mainscreen` for a session whose host forwards screen switches.
pub(crate) fn mainscreen_via_host(term: &Mutex<Terminal>, master: i32) -> Option<String> {
    mainscreen_with(term, aterm_pty::conpty_host(master), |carry| {
        start_host_client(master, carry)
    })
}

fn mainscreen_with(
    term: &Mutex<Terminal>,
    host: Option<ConsoleHostKind>,
    client: impl FnOnce(Carry) -> io::Result<()>,
) -> Option<String> {
    mainscreen_waiting(term, host, client, MAINSCREEN_HANG)
}

fn mainscreen_waiting(
    term: &Mutex<Terminal>,
    host: Option<ConsoleHostKind>,
    client: impl FnOnce(Carry) -> io::Result<()>,
    wait: Duration,
) -> Option<String> {
    mainscreen_with_clock(term, host, client, wait, Instant::now)
}

fn mainscreen_with_clock(
    term: &Mutex<Terminal>,
    host: Option<ConsoleHostKind>,
    client: impl FnOnce(Carry) -> io::Result<()>,
    wait: Duration,
    mut now: impl FnMut() -> Instant,
) -> Option<String> {
    if !host_resyncs(host) {
        return None;
    }
    let carry = {
        let mut t = crate::term_lock(term);
        if !t.expect_host_alt_screen_leave() {
            return Some("OK left=0\n".to_string());
        }
        carry_of(&t)
    };
    match client(carry) {
        // A leave client still running from an earlier request is the one to wait for.
        Err(e) if e.kind() != io::ErrorKind::ResourceBusy => {
            aterm_log::warn!("mainscreen: the console host's resync client did not start ({e})");
            return None;
        }
        _ => {}
    }
    let started = now();
    let deadline = started
        .checked_add(wait)
        .unwrap_or_else(|| started + Duration::from_secs(365 * 24 * 3600));
    loop {
        if !crate::term_lock(term).is_alternate_screen() {
            return Some("OK left=1\n".to_string());
        }
        if now() >= deadline {
            // Whichever client was meant to leave, none did: the next prompt asks again.
            crate::term_lock(term).abandon_alt_screen_resync();
            return Some(format!(
                "ERR timeout: the console host did not leave its alternate screen within {} ms; \
                 the tab is still on it\n",
                wait.as_millis()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
fn start_host_client(master: i32, carry: Carry) -> io::Result<()> {
    match carry {
        Some((cursor, text)) => aterm_pty::resync_alt_screen_carrying(master, cursor, &text),
        None => aterm_pty::resync_alt_screen(master),
    }
}

/// No console host off Windows: the posture is never on there, so nothing
/// asks; the parity keeps the drain's one call shape.
#[cfg(not(windows))]
fn start_host_client(_master: i32, _carry: Carry) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no console host off Windows",
    ))
}

#[cfg(test)]
#[path = "alt_screen_resync_tests.rs"]
mod tests;
