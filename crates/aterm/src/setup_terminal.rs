// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transparent session's first-setup countdown, before it starts a shell.
//! This owns only the controlling terminal; it never probes or changes macOS settings.

use aterm_gui::WorkstationSetupDecision;
use aterm_types::macos_setup::{Plan, Setting};
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_secs(30);

/// Offer the actual interactive terminal thirty seconds to cancel first setup.
/// A missing, background, hung-up or unwritable terminal never authorizes setup.
pub(super) fn countdown(initial: Plan) -> WorkstationSetupDecision {
    let offered = (|| {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/tty")?;
        let start = Instant::now();
        countdown_on(file, initial, DELAY, || start.elapsed(), ensure_foreground)
    })();
    match offered {
        Ok(decision) => decision,
        Err(why) => {
            aterm_log::warn!("first-run macOS setup countdown unavailable: {why}");
            WorkstationSetupDecision::Unavailable
        }
    }
}

/// These messages are emitted before the shell starts, while the caller still
/// owns the tty. Background persistence never writes into the shell's output.
pub(super) fn saving_choice(cancel: bool) {
    status(if cancel {
        "Cancellation is not saved yet. aterm will retry while this session stays open."
    } else {
        "Your setup choices are not saved yet. aterm will retry while this session stays open."
    });
}

pub(super) fn worker_unavailable(offered: bool) {
    status(if offered {
        "The setup worker is unavailable. Your last setup choice may not be saved."
    } else {
        "The setup worker could not start. Automatic setup is skipped for this session."
    });
}

fn status(message: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/tty")
        && ensure_foreground(file.as_raw_fd()).is_ok()
    {
        let _ = without_job_control_signal(libc::SIGTTOU, || writeln!(file, "{message}"));
    }
}

fn ensure_foreground(fd: RawFd) -> io::Result<()> {
    // SAFETY: these calls inspect the open terminal and the current process.
    let foreground = unsafe { libc::tcgetpgrp(fd) };
    if foreground < 0 {
        Err(io::Error::last_os_error())
    } else if foreground != unsafe { libc::getpgrp() } {
        Err(io::Error::other("setup needs the foreground terminal"))
    } else {
        Ok(())
    }
}

/// A terminal restored before the caller can start its shell or setup worker.
struct Terminal {
    file: File,
    original: libc::termios,
    restored: bool,
}

impl Terminal {
    fn take(file: File) -> io::Result<Self> {
        let original = attributes(file.as_raw_fd())?;
        let mut raw = original;
        // SAFETY: `raw` is an initialized termios. Raw input makes any byte,
        // including Ctrl-C and Ctrl-S, a cancellation instead of a signal/flow stop.
        unsafe { libc::cfmakeraw(&mut raw) };
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        let mut terminal = Self {
            file,
            original,
            restored: false,
        };
        set_attributes(terminal.file.as_raw_fd(), &raw)?;
        // Even a failure writing the explanation drops the guard and restores.
        terminal
            .file
            .write_all(b"aterm can apply this macOS setup checklist:\r\n")?;
        Ok(terminal)
    }

    fn key(&mut self, wait: Duration) -> io::Result<bool> {
        let mut descriptor = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = wait
            .as_millis()
            .saturating_add(u128::from(!wait.subsec_nanos().is_multiple_of(1_000_000)));
        let timeout = i32::try_from(millis).unwrap_or(i32::MAX);
        // SAFETY: poll receives one live descriptor record for the stated length.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("setup terminal disconnected"));
        }
        if descriptor.revents & libc::POLLIN == 0 {
            return Ok(false);
        }
        // Read enough to recognize input. On cancellation, drain the entire
        // input queue before restoring the terminal, including a larger paste.
        let mut bytes = [0; 128];
        // Foreground ownership can change after poll. With SIGTTIN blocked,
        // that read fails with EIO instead of stopping us in raw mode.
        match without_job_control_signal(libc::SIGTTIN, || self.file.read(&mut bytes)) {
            Ok(0) => Err(io::Error::other("setup terminal reached EOF")),
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn discard_input(&self) -> io::Result<()> {
        without_job_control_signal(libc::SIGTTOU, || {
            // SAFETY: this is the live tty's input queue. No child shell exists yet;
            // all queued bytes belong to the offer whose any-key action canceled it.
            if unsafe { libc::tcflush(self.file.as_raw_fd(), libc::TCIFLUSH) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        set_attributes(self.file.as_raw_fd(), &self.original)?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.restored {
            let _ = set_attributes(self.file.as_raw_fd(), &self.original);
        }
    }
}

fn attributes(fd: RawFd) -> io::Result<libc::termios> {
    let mut attrs = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes attrs on success and receives its full allocation.
    if unsafe { libc::tcgetattr(fd, attrs.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful tcgetattr initialized the value.
    Ok(unsafe { attrs.assume_init() })
}

fn set_attributes(fd: RawFd, attrs: &libc::termios) -> io::Result<()> {
    // A process that lost the foreground must still restore its tty before
    // returning Unavailable. Blocking SIGTTOU permits that restoration instead
    // of suspending the process with the terminal left in raw mode.
    without_job_control_signal(libc::SIGTTOU, || {
        // SAFETY: the descriptor is borrowed and attrs is an initialized termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, attrs) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

fn without_job_control_signal<T>(
    signal: libc::c_int,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    let mut original = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes this allocation; sigaddset only mutates
    // that initialized set. Callers supply a job-control signal constant.
    if unsafe {
        libc::sigemptyset(blocked.as_mut_ptr()) != 0
            || libc::sigaddset(blocked.as_mut_ptr(), signal) != 0
    } {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: blocked was initialized above; original has room for the old mask.
    let mask =
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, blocked.as_ptr(), original.as_mut_ptr()) };
    if mask != 0 {
        return Err(io::Error::from_raw_os_error(mask));
    }
    let result = operation();
    // SAFETY: the successful pthread_sigmask initialized original. Only this
    // thread's previous mask is restored; the null output pointer is permitted.
    let restored = unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, original.as_ptr(), std::ptr::null_mut())
    };
    match result {
        Err(error) => Err(error),
        Ok(_) if restored != 0 => Err(io::Error::from_raw_os_error(restored)),
        Ok(value) => Ok(value),
    }
}

/// Ceil to the next second: zero is displayed only after the deadline is reached.
fn seconds_left(delay: Duration, elapsed: Duration) -> u64 {
    let remaining = delay.saturating_sub(elapsed);
    remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() != 0))
}

/// The injected monotonic clock lets a real pty test reach expiry without waiting.
fn countdown_on(
    file: File,
    initial: Plan,
    delay: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut foreground: impl FnMut(RawFd) -> io::Result<()>,
) -> io::Result<WorkstationSetupDecision> {
    foreground(file.as_raw_fd())?;
    let mut terminal = Terminal::take(file)?;
    for setting in Setting::ALL {
        if initial.selection.contains(setting) {
            writeln!(terminal.file, "  {}\r", setting.label())?;
        }
    }
    if initial.selection.is_empty() {
        terminal
            .file
            .write_all(b"  No macOS settings selected.\r\n")?;
    }
    let mut displayed = None;
    let decision = loop {
        foreground(terminal.file.as_raw_fd())?;
        let elapsed = elapsed();
        let remaining = seconds_left(delay, elapsed);
        if displayed != Some(remaining) {
            write!(
                terminal.file,
                "\rMac setup starts in {remaining:2}s. Press any key to cancel.  "
            )?;
            displayed = Some(remaining);
        }
        // Input wins at expiry, including a key already queued before this pass.
        let key = terminal.key(delay.saturating_sub(elapsed).min(Duration::from_secs(1)))?;
        // The user may have backgrounded the process while poll was waiting.
        // That terminal cannot authorize setup, even when the timer expired.
        foreground(terminal.file.as_raw_fd())?;
        if key {
            break WorkstationSetupDecision::Cancel;
        }
        if elapsed >= delay {
            break WorkstationSetupDecision::Begin(initial);
        }
    };
    if matches!(decision, WorkstationSetupDecision::Cancel) {
        terminal.discard_input()?;
    }
    terminal.file.write_all(match decision {
        WorkstationSetupDecision::Begin(_) => {
            b"\rSaving your macOS setup choices.                        \r\n"
        }
        _ => b"\rCanceling macOS setup.                                  \r\n",
    })?;
    terminal.restore()?;
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::macos_setup::Selection;
    use std::os::fd::FromRawFd as _;

    fn pty() -> (File, File) {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open test pty");
        // SAFETY: open_pty_pair returned two newly owned descriptors.
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    fn assert_restored(expected: &libc::termios, file: &File) {
        let actual = attributes(file.as_raw_fd()).expect("read restored attributes");
        assert_eq!(actual.c_iflag, expected.c_iflag);
        assert_eq!(actual.c_oflag, expected.c_oflag);
        assert_eq!(actual.c_cflag, expected.c_cflag);
        // Darwin marks pending input for reprocessing when canonical mode is
        // restored. PENDIN is transient kernel state, not a changed tty setting.
        assert_eq!(
            actual.c_lflag & !libc::PENDIN,
            expected.c_lflag & !libc::PENDIN
        );
        assert_eq!(actual.c_cc, expected.c_cc);
        assert_eq!(actual.c_ispeed, expected.c_ispeed);
        assert_eq!(actual.c_ospeed, expected.c_ospeed);
    }

    fn await_input(file: &File) {
        let mut descriptor = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized record for an open descriptor. One minute is
        // a hang detector for the test's byte delivery, not a latency assertion.
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 60_000) }, 1);
        assert_ne!(descriptor.revents & libc::POLLIN, 0);
    }

    fn signal_mask() -> libc::sigset_t {
        let mut mask = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: with a null input this reads the calling thread's mask into
        // the allocated output, without changing its signal state.
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), mask.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful read initialized mask.
        unsafe { mask.assume_init() }
    }

    #[test]
    fn any_key_cancels_even_at_expiry_and_restores_the_real_terminal() {
        for elapsed in [Duration::ZERO, DELAY] {
            let (mut master, slave) = pty();
            let saved = attributes(slave.as_raw_fd()).expect("original attributes");
            let observe = slave.try_clone().expect("observe slave");
            // Raw mode must treat Ctrl-C as a key; queue it after the guard takes
            // the terminal so canonical input processing cannot turn it into a signal.
            let mut terminal = Terminal::take(slave).expect("take terminal");
            master.write_all(b"\x03").expect("queue cancellation");
            assert!(
                terminal
                    .key(Duration::from_secs(60))
                    .expect("read cancellation")
            );
            terminal.restore().expect("restore cancellation");
            assert_restored(&saved, &observe);
            drop(terminal);

            // An ordinary queued key takes the actual countdown's Cancel route.
            master.write_all(b"x\n").expect("queue normal key");
            await_input(&observe);
            let result = countdown_on(
                observe.try_clone().unwrap(),
                Plan::default(),
                DELAY,
                || elapsed,
                |_| Ok(()),
            )
            .expect("countdown canceled");
            assert!(matches!(result, WorkstationSetupDecision::Cancel));
            assert_restored(&saved, &observe);
        }
    }

    #[test]
    fn thirty_seconds_is_the_boundary_and_expiry_restores_the_terminal() {
        assert_eq!(seconds_left(DELAY, Duration::ZERO), 30);
        assert_eq!(seconds_left(DELAY, Duration::from_millis(29_999)), 1);
        assert_eq!(seconds_left(DELAY, DELAY), 0);
        let (_master, slave) = pty();
        let saved = attributes(slave.as_raw_fd()).expect("original attributes");
        let observe = slave.try_clone().expect("observe slave");
        let signal_before = signal_mask();
        let plan = Plan {
            selection: Selection::from_bits(1).expect("one selected setting"),
            vault_url: "http://vault.local:7484/v1/team".into(),
        };
        let result = countdown_on(slave, plan.clone(), DELAY, || DELAY, |_| Ok(()))
            .expect("synthetic expiry");
        assert!(matches!(result, WorkstationSetupDecision::Begin(actual) if actual == plan));
        assert_restored(&saved, &observe);
        assert_eq!(signal_mask(), signal_before);
    }

    #[test]
    fn a_failed_offer_restores_terminal_and_authorizes_no_setup() {
        use std::os::unix::ffi::OsStrExt as _;

        let (_master, slave) = pty();
        let saved = attributes(slave.as_raw_fd()).expect("original attributes");
        let mut name = [0_u8; 1024];
        // SAFETY: the slave is live and the output buffer has the stated size.
        assert_eq!(
            unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr().cast(), name.len()) },
            0
        );
        let name = std::ffi::CStr::from_bytes_until_nul(&name).expect("slave device path");
        let read_only = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(std::ffi::OsStr::from_bytes(name.to_bytes()))
            .expect("open private slave read-only");
        // A read-only tty accepts termios changes but deterministically refuses
        // the first offer write. Closing its master instead races children that
        // can temporarily inherit that master during parallel process spawns.
        let error = countdown_on(read_only, Plan::default(), DELAY, || DELAY, |_| Ok(()))
            .expect_err("an unwritable offer cannot authorize setup");
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
        assert_restored(&saved, &slave);
    }

    #[test]
    fn losing_the_foreground_at_expiry_cannot_authorize_setup() {
        let (_master, slave) = pty();
        let saved = attributes(slave.as_raw_fd()).expect("original attributes");
        let observe = slave.try_clone().expect("observe slave");
        let mut reads = 0;
        let result = countdown_on(
            slave,
            Plan::default(),
            DELAY,
            || DELAY,
            |_| {
                reads += 1;
                if reads == 3 {
                    Err(io::Error::other("another job took the foreground"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(reads, 3, "the foreground is rechecked after the wait");
        assert_restored(&saved, &observe);
    }

    #[test]
    fn cancellation_discards_a_paste_larger_than_the_read_buffer() {
        let (mut master, slave) = pty();
        let saved = attributes(slave.as_raw_fd()).expect("original attributes");
        let observe = slave.try_clone().expect("observe slave");
        // Longer than our read, below Darwin's canonical line-buffer bound.
        let mut paste = vec![b'x'; 192];
        paste.push(b'\n');
        master.write_all(&paste).expect("queue a paste");
        await_input(&observe);
        let result = countdown_on(slave, Plan::default(), DELAY, || DELAY, |_| Ok(()))
            .expect("paste cancels countdown");
        assert!(matches!(result, WorkstationSetupDecision::Cancel));
        assert_restored(&saved, &observe);
        let mut descriptor = libc::pollfd {
            fd: observe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live tty record; queued input must have been discarded.
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
    }
}
