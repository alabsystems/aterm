// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `aterm-askpass` role: the `SUDO_ASKPASS` helper aterm points every
//! macOS session at (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.3).
//!
//! `sudo`, with no controlling terminal, runs this with argv `[path, prompt]`.
//! The prompt (`argv[1]`) is caller-controlled and is NOT relayed anywhere. The
//! helper attests its own parent is Apple's `/usr/bin/sudo` and that its stdout
//! is the anonymous pipe `sudo` handed it, then asks the hosting aterm (the
//! `askpass` control verb) for the password. On success it writes the bytes
//! and one newline to fd 1 — what `sudo` reads — and nothing else ever reaches
//! fd 1. On any refusal it writes nothing to fd 1 (so `sudo` reports "no
//! password was provided") and a short, secret-free reason to fd 2.

use aterm_uds::sudo::{Secret, WireBuffer};
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::Duration;

/// The wait for the server's answer: longer than the server's own 300 s sheet
/// deadline, so the server's reply (or timeout line) always arrives first.
const REPLY_WAIT: Duration = Duration::from_secs(360);
/// The connect phase: a running aterm answers at once.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// Refuse: a reason to fd 2, nothing to fd 1, exit nonzero.
fn refuse(reason: &str) -> ExitCode {
    eprintln!("aterm-askpass: refused: {reason}");
    ExitCode::FAILURE
}

/// The `aterm-askpass` argv0 alias entry (`crates/aterm/src/main.rs`). `_args`
/// is `[prompt]`, deliberately unused (the prompt is caller-controlled).
#[must_use]
pub fn main_entry(_args: Vec<std::ffi::OsString>) -> ExitCode {
    // 1. Attest the parent is the genuine setuid-root `/usr/bin/sudo`, and that
    //    fd 1 is the pipe sudo dups in — not a file or a named FIFO an attacker
    //    redirected in.
    let sudo = aterm_uds::sudo::parent_pid();
    if let Err(why) = aterm_uds::sudo::attest_sudo(sudo) {
        return refuse(&why);
    }
    if let Err(why) = aterm_uds::sudo::stdout_is_anon_pipe() {
        return refuse(&why);
    }

    // 2. Find the hosting aterm. ATERM_PARENT_SESSION_ID is only a routing hint
    //    (a tool such as Homebrew may drop it); the server attributes the
    //    request to a session by kernel ancestry and refuses a mismatch, so a
    //    wrong instance refuses rather than prompts.
    let sid = std::env::var("ATERM_PARENT_SESSION_ID").ok();
    let sock = match crate::resolve_sock_for(sid.as_deref()) {
        Ok(sock) => sock,
        Err(e) => return refuse(&format!("no aterm instance to ask ({e})")),
    };

    // 3. Authenticate through the ordinary, secret-free connect probe, then
    // keep the answer out of the generic String/BufReader transport.
    let reply = match send_askpass(&sock, CONNECT_WAIT, REPLY_WAIT) {
        Ok(reply) => reply,
        Err(e) => return refuse(&format!("the aterm did not answer ({e})")),
    };

    // 4. Borrow the guarded wire bytes. Neither the hex nor decoded password
    // is copied through an ordinary String, Vec, or stdout buffer.
    let line = reply
        .as_bytes()
        .strip_suffix(b"\n")
        .unwrap_or(reply.as_bytes());
    if let Some(hex) = line.strip_prefix(b"OK askpass secret=") {
        match Secret::decode_hex(hex) {
            Some(secret) if secret.is_empty() => ExitCode::SUCCESS,
            Some(secret) => {
                let ok = unbuffered_stdout().and_then(|mut out| write_password(&mut out, &secret));
                if ok.is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            None => refuse("the aterm sent a reply this helper cannot read"),
        }
    } else {
        eprintln!("aterm-askpass: {}", refusal_reply(line));
        ExitCode::FAILURE
    }
}

/// Return only public protocol outcomes, never arbitrary server text.
fn refusal_reply(reply: &[u8]) -> &'static str {
    match reply {
        b"ERR askpass declined" => "ERR askpass declined",
        b"ERR askpass timeout" => "ERR askpass timeout",
        b"ERR askpass headless" => "ERR askpass headless",
        b"ERR askpass busy" => "ERR askpass busy",
        b"ERR askpass refused" => "ERR askpass refused",
        line if line.starts_with(b"ERR askpass busy ") => "ERR askpass busy",
        line if line.starts_with(b"ERR askpass refused ") => "ERR askpass refused",
        _ => "the aterm sent a reply this helper cannot read",
    }
}

fn send_askpass(path: &str, accept: Duration, reply: Duration) -> io::Result<WireBuffer> {
    let stream = match crate::open_accepted(path, crate::TargetOrigin::Pinned, Some(accept))? {
        crate::Acceptance::Served(stream) => stream,
        crate::Acceptance::Refused(_) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connection refused",
            ));
        }
        crate::Acceptance::NotAccepted(_) => return Err(io::ErrorKind::TimedOut.into()),
        crate::Acceptance::Dropped => return Err(io::ErrorKind::UnexpectedEof.into()),
    };
    (&stream).write_all(b"askpass\n")?;
    read_answer_within(&stream, reply)
}

fn read_answer_within(stream: &crate::CtlStream, wait: Duration) -> io::Result<WireBuffer> {
    struct DeadlineReader<'a> {
        stream: &'a crate::CtlStream,
        deadline: std::time::Instant,
    }
    impl io::Read for DeadlineReader<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let remaining = self
                .deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?;
            self.stream.set_read_timeout(Some(remaining))?;
            self.stream.read(bytes)
        }
    }
    let deadline = std::time::Instant::now()
        .checked_add(wait)
        .ok_or(io::ErrorKind::InvalidInput)?;
    WireBuffer::read_line(&mut DeadlineReader { stream, deadline })
}

fn write_password(out: &mut impl io::Write, secret: &Secret) -> io::Result<()> {
    if !secret.is_empty() {
        out.write_all(WireBuffer::password_line(secret).as_bytes())?;
    }
    Ok(())
}

/// A File writes straight to the pipe. std::io::stdout's reusable LineWriter
/// would retain a copy after a successful flush. Close only our duplicated fd.
#[cfg(unix)]
fn unbuffered_stdout() -> io::Result<std::fs::File> {
    use std::os::fd::BorrowedFd;
    // SAFETY: fd 1 was attested as sudo's anonymous pipe before this call and
    // remains owned by the process; only its new duplicate is closed here.
    let fd = unsafe { BorrowedFd::borrow_raw(1) }.try_clone_to_owned()?;
    Ok(std::fs::File::from(fd))
}

#[cfg(not(unix))]
fn unbuffered_stdout() -> io::Result<std::fs::File> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
fn decode_secret(hex: &str) -> Option<Secret> {
    Secret::decode_hex(hex.as_bytes())
}

#[cfg(test)]
#[path = "askpass_tests.rs"]
mod tests;
