// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A password held in memory only: `enroll` asks the admin's password once, on the terminal,
//! and feeds it to each Mac's `ssh` login and `sudo` through pipes (design §4.2).
//!
//! Where a password could leak, and what keeps it out:
//!
//! - **argv and the environment** (`ps` shows both): a [`Secret`] reaches a child only as its
//!   standard input, a pipe written before the child starts ([`super::System::run_fed`]). The
//!   `SSH_ASKPASS` helper `ssh` runs (`crate::askpass`) reads it from there too.
//! - **disk**: nothing writes it. The buffer is `mlock`ed, best effort (`RLIMIT_MEMLOCK`
//!   bounds it), so it stays out of swap where the kernel allows; macOS encrypts swap anyway.
//!   Before it is read, `RLIMIT_CORE` goes to zero ([`no_core_dumps`]) in the controller and
//!   in the askpass helper, so neither, nor `ssh`, which inherits it, can leave a core file.
//! - **logs, errors, `Debug`**: `Debug` prints `Secret(<redacted>)`; there is no `Display`,
//!   `Clone` or `Serialize`, and the test arm records a fed command by its display alone.
//! - **memory after use**: one fixed boxed buffer, never a growing `String`, whose
//!   reallocations would leave copies behind; it is scrubbed with volatile writes on drop
//!   (the hand-rolled `zeroize`, as `atpkg-keys`' master phrase is: this workspace carries
//!   no `zeroize`).
//! - **the screen**: the terminal's echo is off while it is typed, and put back by a guard on
//!   every ordinary path; Ctrl-C (or SIGTERM, SIGHUP) during the prompt is caught, the echo
//!   put back, and the prompt answers [`std::io::ErrorKind::Interrupted`].

use std::io;

/// The longest password a [`Secret`] holds, in bytes.
pub const SECRET_MAX: usize = 1024;

/// A password (or another line a person typed), held in memory only.
pub struct Secret {
    bytes: Box<[u8; SECRET_MAX]>,
    len: usize,
}

impl Secret {
    /// An empty secret, its buffer locked.
    fn empty() -> Self {
        let secret = Self {
            bytes: Box::new([0_u8; SECRET_MAX]),
            len: 0,
        };
        lock(&secret.bytes[..]);
        secret
    }

    /// A secret holding `bytes` (test fixtures, and the askpass helper's answer).
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`] when `bytes` is longer than [`SECRET_MAX`].
    pub fn new(bytes: &[u8]) -> io::Result<Self> {
        let mut secret = Self::empty();
        let Some(slot) = secret.bytes.get_mut(..bytes.len()) else {
            return Err(too_long());
        };
        slot.copy_from_slice(bytes);
        secret.len = bytes.len();
        Ok(secret)
    }

    /// The bytes, for the one place they go: a child's standard input.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    /// Nothing was typed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read one line from `input` into a new secret: up to the first `\n` (not kept), or to
    /// the end of `input`. `None` when `input` ended before any byte. Bytes after the line
    /// are left unread only as far as `input` buffers nothing itself (a pipe or a terminal
    /// read one byte at a time).
    ///
    /// # Errors
    /// The read's error, or [`io::ErrorKind::InvalidInput`] for a line longer than
    /// [`SECRET_MAX`].
    pub fn read_line(input: &mut dyn io::Read) -> io::Result<Option<Self>> {
        let mut secret = Self::empty();
        let mut any = false;
        loop {
            let mut byte = [0_u8; 1];
            let n = match input.read(&mut byte) {
                Ok(n) => n,
                // A signal the read was interrupted by, not a person's stop ([`StopSignal`]).
                Err(e) if e.kind() == io::ErrorKind::Interrupted && e.get_ref().is_none() => {
                    continue;
                }
                Err(e) => return Err(e),
            };
            if n == 0 {
                break;
            }
            any = true;
            if byte[0] == b'\n' {
                break;
            }
            let Some(slot) = secret.bytes.get_mut(secret.len) else {
                return Err(too_long());
            };
            *slot = byte[0];
            scrub(&mut byte);
            secret.len += 1;
        }
        // A line typed at a terminal in CRLF mode, or pasted from one.
        if secret.bytes().last() == Some(&b'\r') {
            secret.len -= 1;
            if let Some(cr) = secret.bytes.get_mut(secret.len) {
                *cr = 0;
            }
        }
        Ok(any.then_some(secret))
    }
}

impl std::fmt::Debug for Secret {
    /// Never the bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        scrub(&mut self.bytes[..]);
        unlock(&self.bytes[..]);
    }
}

fn too_long() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("longer than {SECRET_MAX} bytes"),
    )
}

/// Overwrite `buf` with zeroes in a way the optimizer may not delete: a loop of plain stores
/// into a buffer about to be freed is a dead store the compiler may remove; volatile writes
/// are not, and the fence keeps them before the free.
fn scrub(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely borrowed `u8` from a mutable slice: a one-byte
        // volatile write to it is in bounds and unaliased.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Keep `buf` out of swap, best effort (the result is ignored: `RLIMIT_MEMLOCK` may refuse).
fn lock(buf: &[u8]) {
    // SAFETY: `buf` is a live borrow, so its pointer and length name mapped memory for the
    // call; `mlock` neither reads nor writes it.
    unsafe {
        let _ = libc::mlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Undo [`lock`] before the buffer is freed.
fn unlock(buf: &[u8]) {
    // SAFETY: as in `lock`; unlocking a range that was never locked is a harmless error.
    unsafe {
        let _ = libc::munlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Keep this process, and every child it starts from now on (`ssh`, the askpass helper `ssh`
/// runs: a child inherits the limit), from writing a core file, which would hold the password
/// in the clear (`mlock` keeps memory out of swap, not out of a core). Soft and hard limits
/// both go to zero, so no child raises it again; a process holding a password wants no core.
/// Best effort: the result is ignored.
pub fn no_core_dumps() {
    let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a live, initialized `rlimit`, which the call only reads.
    unsafe {
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const none);
    }
}

/// [`super::System::ask`]'s live arm: write `prompt` to the controlling terminal
/// (`/dev/tty`, never standard input or output, so no redirection captures the answer) and
/// read one line there, with echo off when `secret` (and, first, no core file from this
/// process or its children: [`no_core_dumps`]). `None` for an empty line or the end of
/// input. A stop signal (Ctrl-C) during the read puts the echo back and answers
/// [`io::ErrorKind::Interrupted`].
pub(super) fn ask_tty(prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
    use std::io::Write as _;
    use std::os::unix::io::AsRawFd as _;
    if secret {
        no_core_dumps();
    }
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let fd = tty.as_raw_fd();
    // SAFETY: `fd` is the open descriptor `tty` owns.
    if unsafe { libc::isatty(fd) } != 1 {
        return Err(io::Error::other("/dev/tty is not a terminal"));
    }
    // Echo off before the prompt shows: the person (or a pty driver) types only once it has,
    // and the flush that turning echo off does then discards nothing typed for it. Turned off
    // after the prompt, an answer quick enough was echoed and then thrown away (measured with
    // `expect` in the VM: the read then waited forever).
    let echo = if secret {
        Some(EchoOff::new(fd)?)
    } else {
        None
    };
    tty.write_all(prompt.as_bytes())?;
    tty.flush()?;
    let stops = super::stops::catch();
    let line = read_tty_line(&mut tty, fd);
    drop(stops);
    drop(echo);
    if secret {
        // The Enter that ended the line was not echoed.
        let _ = tty.write_all(b"\n");
    }
    Ok(line?.filter(|s| !s.is_empty()))
}

/// One line from the terminal `tty`, a byte at a time, waking every quarter second to notice
/// a stop signal the handler noted (it sets `SA_RESTART`, so a blocked read would not return).
fn read_tty_line(tty: &mut std::fs::File, fd: i32) -> io::Result<Option<Secret>> {
    struct Polled<'a>(&'a mut std::fs::File, i32);
    impl io::Read for Polled<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                if super::stops::caught().is_some() {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, StopSignal));
                }
                let mut pfd = libc::pollfd {
                    fd: self.1,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one live, initialized `pollfd`, for a call that only writes its
                // `revents`.
                let ready = unsafe { libc::poll(&raw mut pfd, 1, 250) };
                if ready > 0 {
                    return self.0.read(buf);
                }
                if ready < 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::Interrupted {
                        return Err(e);
                    }
                }
            }
        }
    }
    Secret::read_line(&mut Polled(tty, fd))
}

/// The payload that marks an `Interrupted` as a person's stop (Ctrl-C at the prompt), which
/// [`Secret::read_line`] does not retry as it does a bare interrupted read.
#[derive(Debug)]
struct StopSignal;

impl std::fmt::Display for StopSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped at the prompt")
    }
}

impl std::error::Error for StopSignal {}

/// The terminal's echo, off for the life of the value and put back on drop.
struct EchoOff {
    fd: i32,
    saved: libc::termios,
}

impl EchoOff {
    fn new(fd: i32) -> io::Result<Self> {
        // SAFETY: all-zero is a valid `termios` (plain integers and an array); it is read
        // only after `tcgetattr` filled it.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is an open terminal and `saved` a live out-parameter of its type.
        if unsafe { libc::tcgetattr(fd, &raw mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        // SAFETY: the same descriptor, and a fully initialized copy with one flag cleared.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw const quiet) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: the descriptor is still open (the caller's `File` outlives this guard) and
        // `saved` is the snapshot `new` took. `TCSANOW`: what was typed after the line (an
        // answer to the next prompt) is kept.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_read_without_its_newline_and_nothing_after_it() {
        let mut input: &[u8] = b"hunter2\nnext";
        let secret = Secret::read_line(&mut input).unwrap().unwrap();
        assert_eq!(secret.bytes(), b"hunter2");
        assert_eq!(
            input, b"next",
            "read one byte at a time, so nothing past the line"
        );
        let mut crlf: &[u8] = b"pw\r\n";
        assert_eq!(
            Secret::read_line(&mut crlf).unwrap().unwrap().bytes(),
            b"pw"
        );
        let mut empty: &[u8] = b"";
        assert!(Secret::read_line(&mut empty).unwrap().is_none());
        let mut blank: &[u8] = b"\n";
        assert!(Secret::read_line(&mut blank).unwrap().unwrap().is_empty());
    }

    #[test]
    fn a_process_that_holds_a_password_writes_no_core_and_its_children_neither() {
        no_core_dumps();
        // SAFETY: all-zero is a valid `rlimit`; `getrlimit` fills it.
        let mut now: libc::rlimit = unsafe { std::mem::zeroed() };
        now.rlim_cur = libc::RLIM_INFINITY;
        // SAFETY: a live out-parameter of the right type.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_CORE, &raw mut now) },
            0
        );
        assert_eq!(
            (now.rlim_cur, now.rlim_max),
            (0, 0),
            "the hard limit too, so a child cannot raise it"
        );
    }

    #[test]
    fn a_secret_never_prints_and_refuses_what_it_cannot_hold() {
        let secret = Secret::new(b"hunter2").unwrap();
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(Secret::new(&[b'x'; SECRET_MAX + 1]).is_err());
        let mut long: &[u8] = &[b'x'; SECRET_MAX + 1];
        assert!(Secret::read_line(&mut long).is_err());
    }
}
