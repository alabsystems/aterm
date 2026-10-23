// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-memory password buffer fed via pipe ([`Secret`], [`no_core_dumps`]).
//! Used by [`super::System::run_fed`] and returns [`std::io::ErrorKind::Interrupted`] on signals.

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

    /// Read one line from `input` into a new secret.
    ///
    /// # Errors
    /// The read's error, or [`io::ErrorKind::InvalidInput`] for a line longer than [`SECRET_MAX`].
    pub fn read_line(input: &mut dyn io::Read) -> io::Result<Option<Self>> {
        let mut secret = Self::empty();
        let mut any = false;
        loop {
            let mut byte = [0_u8; 1];
            let n = match input.read(&mut byte) {
                Ok(n) => n,
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

/// Overwrite `buf` with zeroes in a way the optimizer may not delete.
fn scrub(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely borrowed `u8` from a mutable slice: a one-byte volatile
        // write to it is in bounds and unaliased.
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
    // SAFETY: As in `lock`; unlocking a range that was never locked is a harmless error.
    unsafe {
        let _ = libc::munlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Sets `RLIMIT_CORE` to zero for this process and its children.
pub fn no_core_dumps() {
    let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: A live, initialized `rlimit`, which the call only reads.
    unsafe {
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const none);
    }
}

/// [`super::System::ask`]'s live arm ([`no_core_dumps`], [`io::ErrorKind::Interrupted`]).
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
        let _ = tty.write_all(b"\n");
    }
    Ok(line?.filter(|s| !s.is_empty()))
}

/// Reads one line from `tty` a byte at a time, polling for stop signals.
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
                // SAFETY: `pfd` is a valid initialized `pollfd`.
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

/// The payload that marks an `Interrupted` as a person's stop (Ctrl-C at the prompt).
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
        // SAFETY: The same descriptor, and a fully initialized copy with one flag cleared.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw const quiet) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: the descriptor is still open (the caller's `File` outlives this guard) and
        // `saved` is the snapshot `new` took.
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
        // SAFETY: All-zero is a valid `rlimit`; `getrlimit` fills it.
        let mut now: libc::rlimit = unsafe { std::mem::zeroed() };
        now.rlim_cur = libc::RLIM_INFINITY;
        // SAFETY: A live out-parameter of the right type.
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
