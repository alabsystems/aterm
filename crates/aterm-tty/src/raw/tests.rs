// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guard on a real terminal, a pty pair from aterm-pty: what it sets, and that a
//! drop, a panic, a SIGTERM and a suspension each leave the pty's settings as they were
//! and no private mode set, whatever order the modes were set in. The panic, the signal
//! and the suspension happen in a child process (this test binary run again on one
//! test, its stdin the pty's slave), whose death or stop the parent watches; the
//! terminal it wrote to is read back through aterm's own engine. [`DevTty`], the
//! terminal a program's loop reads and draws on, is driven on a pty here too.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::ExitStatusExt as _;
use std::sync::{MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_spec::wait::MUST_HAPPEN;

use super::{RawGuard, TEST_TERMINAL, Termios};
use crate::decode::{Decoder, Event, Reply};
use crate::signals::Signals;
use crate::tty::{DevTty, Mode, Modes, Readiness, Size, Tty};

/// Every mode a terminal program sets.
const ALL_MODES: [Mode; 7] = [
    Mode::AltScreen,
    Mode::KittyKeys(5),
    Mode::Mouse,
    Mode::Paste,
    Mode::Focus,
    Mode::InBandResize,
    Mode::Cursor,
];

/// The child's orders: `panic`, `sigterm` or `suspend`. The child's stdin is the pty.
const CHILD: &str = "ATERM_TTY_RAW_CHILD";
/// The test the child runs.
const CHILD_TEST: &str = "raw::tests::child";
/// What the child writes once raw with every mode set, and again once back from a stop.
const RAW_MARK: &[u8] = b"<raw>";
const BACK_MARK: &[u8] = b"<back>";

/// A pty pair `(master, slave)`.
fn pty() -> (OwnedFd, OwnedFd) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
    // SAFETY: both are open descriptors the call just returned and nothing else owns.
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// A terminal's settings, field by field: the padding in a termios is whatever the
/// kernel's copy left there, so two equal settings can differ in their bytes.
fn settings(termios: &Termios) -> String {
    // SAFETY: an aligned termios `tcgetattr` filled.
    let t = unsafe { &*termios.as_ptr() };
    format!(
        "{:?}",
        (
            t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag, t.c_cc, t.c_ispeed, t.c_ospeed
        )
    )
}

/// What the program wrote to a pty's slave, read from its master.
struct Seen {
    master: OwnedFd,
    bytes: Vec<u8>,
    /// Where the next wait starts looking: past what the last one waited for.
    from: usize,
}

impl Seen {
    fn new(master: OwnedFd) -> Self {
        Self {
            master,
            bytes: Vec::new(),
            from: 0,
        }
    }

    /// Read until `end` appears past what the last wait waited for; everything written
    /// up to and including it. A read may bring more, which the next wait looks at.
    fn until(&mut self, end: &[u8]) -> &[u8] {
        let deadline = Instant::now() + MUST_HAPPEN;
        loop {
            if let Some(at) = self.bytes[self.from..]
                .windows(end.len())
                .position(|window| window == end)
            {
                self.from += at + end.len();
                return &self.bytes[..self.from];
            }
            assert!(
                Instant::now() < deadline,
                "the terminal never saw {:?}; it saw {:?}",
                String::from_utf8_lossy(end),
                String::from_utf8_lossy(&self.bytes)
            );
            let master = self.master.as_raw_fd();
            let mut pfd = libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live pollfd.
            if unsafe { libc::poll(&raw mut pfd, 1, 100) } <= 0 {
                continue;
            }
            let mut chunk = [0u8; 4096];
            // SAFETY: a read into a local buffer from the open master.
            let n = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
            if n > 0 {
                self.bytes.extend_from_slice(&chunk[..n.unsigned_abs()]);
            }
        }
    }
}

/// The terminal's private modes as aterm's engine holds them after `bytes`: each mode's
/// DECRPM state, the kitty flags, and the cursor.
fn modes_after(bytes: &[u8]) -> Vec<String> {
    let mut term = Terminal::new(24, 80);
    term.process(bytes);
    let _ = term.take_response();
    term.process(
        b"\x1b[?1049$p\x1b[?1002$p\x1b[?1006$p\x1b[?2004$p\x1b[?1004$p\x1b[?2048$p\
          \x1b[?2026$p\x1b[?25$p\x1b[?u",
    );
    let reply = term.take_response().expect("the engine answers");
    let mut decoder = Decoder::new();
    let mut seen: Vec<String> = decoder
        .feed(&reply, Instant::now())
        .into_iter()
        .filter_map(|event| match event {
            Event::Reply(Reply::Mode { mode, state }) => Some(format!("{mode}={state:?}")),
            Event::Reply(Reply::KittyFlags(flags)) => Some(format!("kitty={flags}")),
            _ => None,
        })
        .collect();
    seen.push(format!("cursor={:?}", term.cursor_style()));
    seen
}

/// The modes of a terminal no program changed.
fn untouched() -> Vec<String> {
    modes_after(b"")
}

/// This process's one guard and signal wake, for this test alone.
fn alone() -> MutexGuard<'static, ()> {
    TEST_TERMINAL.lock().unwrap_or_else(PoisonError::into_inner)
}

#[test]
fn guard_restores_termios_on_drop() {
    let _alone = alone();
    let (master, slave) = pty();
    let fd = slave.as_raw_fd();
    let before = Termios::read(fd).expect("the slave's settings");
    let mut guard = RawGuard::enter(fd).expect("raw mode");
    assert_eq!(
        RawGuard::enter(fd).expect_err("one guard at a time").kind(),
        std::io::ErrorKind::ResourceBusy
    );

    let raw = Termios::read(fd).expect("the raw settings");
    // SAFETY: an aligned termios `tcgetattr` filled.
    let t = unsafe { &*raw.as_ptr() };
    assert_eq!(
        t.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN),
        0
    );
    assert_eq!(t.c_iflag & (libc::ICRNL | libc::IXON), 0);
    assert_eq!(t.c_oflag & libc::OPOST, 0);
    assert_eq!((t.c_cc[libc::VMIN], t.c_cc[libc::VTIME]), (1, 0));

    for mode in ALL_MODES {
        guard.enable(mode).expect("set");
    }
    // As a frame does: the cursor hidden, a style, synchronized output begun.
    guard
        .write(b"\x1b[?2026h\x1b[?25l\x1b[1;31m\x1b[5 q")
        .expect("drawn");
    let mut seen = Seen::new(master);
    assert_ne!(
        modes_after(seen.until(b"\x1b[5 q")),
        untouched(),
        "the modes took"
    );

    drop(guard);
    let after = Termios::read(fd).expect("the settings after");
    assert_eq!(settings(&after), settings(&before), "the settings are back");
    assert_eq!(modes_after(seen.until(b"\x1b[?1049l")), untouched());

    // The kitty entry pushed before the alternate screen is entered goes there with it,
    // and is popped there: neither screen keeps the flags.
    let mut guard = RawGuard::enter(fd).expect("a guard again once the first is dropped");
    guard.enable(Mode::KittyKeys(5)).expect("kitty first");
    guard
        .enable(Mode::AltScreen)
        .expect("then the alternate screen");
    assert_eq!(guard.modes(), Modes::KITTY_KEYS.union(Modes::ALT_SCREEN));
    let shown = modes_after(seen.until(b"\x1b[?1049h\x1b[>5u"));
    assert!(
        shown.contains(&"1049=Set".to_owned()) && shown.contains(&"kitty=5".to_owned()),
        "the alternate screen shown, with the flags: {shown:?}"
    );
    drop(guard);
    assert_eq!(modes_after(seen.until(b"\x1b[?1049l")), untouched());
    assert_eq!(
        settings(&Termios::read(fd).expect("settings")),
        settings(&before)
    );
}

/// A panic on another thread, which the program survives by joining it, leaves the
/// terminal raw with every mode set: only the guard's own thread puts it back.
#[test]
fn a_worker_panic_leaves_the_terminal_raw() {
    let _alone = alone();
    let (_master, slave) = pty();
    let fd = slave.as_raw_fd();
    let mut guard = RawGuard::enter(fd).expect("raw mode");
    guard.enable(Mode::AltScreen).expect("set");
    let raw = settings(&Termios::read(fd).expect("the raw settings"));
    let worker = std::thread::spawn(|| panic!("a worker fails"));
    assert!(worker.join().is_err(), "the worker panicked");
    assert_eq!(guard.modes(), Modes::ALT_SCREEN, "the mode is still set");
    assert_eq!(
        settings(&Termios::read(fd).expect("settings")),
        raw,
        "still raw"
    );
}

/// The size the pty's master gives its slave.
fn set_size(master: &OwnedFd, cols: u16, rows: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads one `winsize` through the pointer.
    let set = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const ws) };
    assert_eq!(set, 0, "the size set");
}

/// Write `bytes` to the pty's master: typed at the terminal.
fn type_in(master: &OwnedFd, bytes: &[u8]) {
    // SAFETY: a write(2) from a live slice to the open master.
    let n = unsafe { libc::write(master.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(usize::try_from(n).ok(), Some(bytes.len()), "typed");
}

/// Send `signal` to this process.
fn signal_self(signal: libc::c_int) {
    // SAFETY: a signal this process's signal wake catches.
    assert_eq!(unsafe { libc::kill(libc::getpid(), signal) }, 0);
}

/// A wait on `tty` for what must come: a minute's hang detector, not a budget.
fn wait(tty: &mut DevTty, buf: &mut [u8]) -> Readiness {
    tty.read(buf, Some(Instant::now() + MUST_HAPPEN))
        .expect("a wait")
}

/// The terminal a loop reads: a wait ends at its deadline, with what was typed (raw: no
/// echo, no CR translation, C-c a byte), or with a signal, reported until it is taken.
/// After SIGCONT the terminal is raw again with its modes set again in place, though
/// the shell reset it meanwhile; dropped, it is as it was; hung up, a wait ends Closed.
#[test]
fn dev_tty_waits_reads_takes_signals_and_hangs_up() {
    let _alone = alone();
    let (master, slave) = pty();
    let fd = slave.as_raw_fd();
    set_size(&master, 100, 30);
    let before = Termios::read(fd).expect("the slave's settings");
    let mut tty = DevTty::from_fd(slave.try_clone().expect("the slave again")).expect("raw");
    assert_eq!(
        tty.size().expect("the size"),
        Size {
            cols: 100,
            rows: 30
        }
    );
    let mut buf = [0u8; 64];

    let deadline = Instant::now() + Duration::from_millis(20);
    assert_eq!(
        tty.read(&mut buf, Some(deadline)).expect("a wait"),
        Readiness::Timeout
    );
    assert!(Instant::now() >= deadline, "not before the deadline");

    type_in(&master, b"a\r\x03");
    let mut typed = Vec::new();
    while typed.len() < 3 {
        match wait(&mut tty, &mut buf) {
            Readiness::Bytes(n) => typed.extend_from_slice(&buf[..n]),
            other => panic!("the typed bytes, not {other:?}"),
        }
    }
    assert_eq!(typed, b"a\r\x03");

    // A resize: reported until taken, then the new size.
    set_size(&master, 120, 40);
    signal_self(libc::SIGWINCH);
    assert_eq!(wait(&mut tty, &mut buf), Readiness::Signal);
    assert_eq!(wait(&mut tty, &mut buf), Readiness::Signal, "until taken");
    assert_eq!(tty.take_signals().expect("taken"), Signals::WINCH);
    assert_eq!(
        tty.size().expect("the size"),
        Size {
            cols: 120,
            rows: 40
        }
    );
    assert_eq!(
        tty.read(&mut buf, Some(Instant::now())).expect("a wait"),
        Readiness::Timeout,
        "nothing pending"
    );

    // Stopped and continued, the shell having put the settings back meanwhile.
    tty.enable(Mode::KittyKeys(5)).expect("set");
    tty.enable(Mode::AltScreen).expect("set");
    let raw = settings(&Termios::read(fd).expect("the raw settings"));
    before
        .apply(fd, libc::TCSANOW)
        .expect("cooked, as a shell leaves it");
    signal_self(libc::SIGCONT);
    assert_eq!(wait(&mut tty, &mut buf), Readiness::Signal);
    assert_eq!(tty.take_signals().expect("taken"), Signals::CONT);
    assert_eq!(
        settings(&Termios::read(fd).expect("settings")),
        raw,
        "raw again"
    );
    let mut seen = Seen::new(master);
    let written = seen.until(b"\x1b[?1049h\x1b[=5;1u");
    assert!(
        !written.contains(&b'a'),
        "nothing typed was echoed: {written:?}"
    );
    let shown = modes_after(written);
    assert!(
        shown.contains(&"1049=Set".to_owned()) && shown.contains(&"kitty=5".to_owned()),
        "every mode set again: {shown:?}"
    );

    drop(tty);
    assert_eq!(modes_after(seen.until(b"\x1b[?1049l")), untouched());
    assert_eq!(
        settings(&Termios::read(fd).expect("settings")),
        settings(&before),
        "as it was"
    );

    // Hung up: the terminal's other side gone.
    let (master, slave) = pty();
    let mut tty = DevTty::from_fd(slave).expect("raw");
    drop(master);
    assert_eq!(wait(&mut tty, &mut buf), Readiness::Closed);
}

/// Run this test binary on [`CHILD_TEST`] with `how` as its orders and the slave as its
/// stdin, to its end.
fn run_child(how: &str, slave: &OwnedFd) -> std::process::Output {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
        .env(CHILD, how);
    let stdin = slave.try_clone().expect("the slave again");
    aterm_uds::deadline::output_until_from(
        &mut cmd,
        aterm_uds::spawnfd::Stdio::from(stdin),
        Instant::now() + MUST_HAPPEN,
    )
    .expect("the child ran")
}

#[test]
fn restores_on_panic_and_on_sigterm() {
    for how in ["panic", "sigterm"] {
        let (master, slave) = pty();
        let before = Termios::read(slave.as_raw_fd()).expect("the slave's settings");
        let out = run_child(how, &slave);
        let stderr = String::from_utf8_lossy(&out.stderr);
        match how {
            "panic" => assert_eq!(out.status.code(), Some(101), "{how}: {stderr}"),
            _ => assert_eq!(out.status.signal(), Some(libc::SIGTERM), "{how}: {stderr}"),
        }
        let after = Termios::read(slave.as_raw_fd()).expect("the settings after");
        assert_eq!(
            settings(&after),
            settings(&before),
            "{how}: the settings are back"
        );
        let mut reader = Seen::new(master);
        let seen = reader.until(b"\x1b[?1049l");
        let mark = seen
            .windows(RAW_MARK.len())
            .position(|w| w == RAW_MARK)
            .unwrap_or_else(|| panic!("{how}: the child set raw mode first: {stderr}"));
        assert_ne!(
            modes_after(&seen[..mark]),
            untouched(),
            "{how}: the modes took"
        );
        assert_eq!(modes_after(seen), untouched(), "{how}");
    }
}

/// A child killed and reaped if a failed assertion leaves it running or stopped.
struct Reap(aterm_uds::spawnfd::Child);

impl Drop for Reap {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn suspend_puts_back_then_reenters() {
    let (master, slave) = pty();
    let fd = slave.as_raw_fd();
    let before = Termios::read(fd).expect("the slave's settings");
    let exe = std::env::current_exe().expect("the test binary");
    // In a process group of its own: the kernel drops a stop signal sent to an orphaned
    // group, and the group this test runs in may be one (its leader's parent in another
    // session), while the child's, whose parent is this test, is not.
    let child = aterm_uds::spawnfd::command(exe)
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
        .env(CHILD, "suspend")
        .process_group(0)
        .stdin(slave.try_clone().expect("the slave again"))
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the child starts");
    let mut child = Reap(child);
    let pid = libc::pid_t::try_from(child.0.id()).expect("a pid");

    // Stopped by its own SIGTSTP, with its terminal put back.
    let stopped = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        let mut status = 0;
        // SAFETY: asks after this test's own child without waiting; a stop is not reaped.
        let got = unsafe { libc::waitpid(pid, &raw mut status, libc::WUNTRACED | libc::WNOHANG) };
        (got == pid).then_some(status)
    })
    .expect("the child stopped or ended");
    assert!(
        libc::WIFSTOPPED(stopped),
        "the child stopped rather than ended: {stopped:#x}"
    );
    assert_eq!(libc::WSTOPSIG(stopped), libc::SIGTSTP);
    let mut seen = Seen::new(master);
    assert_eq!(
        modes_after(seen.until(b"\x1b[?1049l")),
        untouched(),
        "no mode while stopped"
    );
    assert_eq!(
        settings(&Termios::read(fd).expect("settings")),
        settings(&before),
        "put back while stopped"
    );

    // Continued: raw again, every mode set again, then put back by the drop.
    // SAFETY: SIGCONT to this test's own stopped child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    assert_ne!(
        modes_after(seen.until(BACK_MARK)),
        untouched(),
        "the modes are set again"
    );
    let status = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        child.0.try_wait().expect("the child's status")
    })
    .expect("the child ended");
    assert!(status.success(), "the child passed: {status:?}");
    assert_eq!(modes_after(seen.until(b"\x1b[?1049l")), untouched());
    assert_eq!(
        settings(&Termios::read(fd).expect("settings")),
        settings(&before)
    );
}

/// The child half of the tests above; an ordinary run (no orders) passes at once.
#[test]
fn child() {
    let Ok(how) = std::env::var(CHILD) else {
        return;
    };
    let mut guard = RawGuard::enter(0).expect("raw mode on the pty");
    // On SIGTERM the modes are set in the other order: the kitty entry before the
    // alternate screen, which carries it.
    let mut modes = ALL_MODES;
    if how == "sigterm" {
        modes.reverse();
    }
    for mode in modes {
        guard.enable(mode).expect("set");
    }
    guard
        .write(b"\x1b[?2026h\x1b[?25l\x1b[1;31m\x1b[5 q")
        .expect("drawn");
    guard.write(RAW_MARK).expect("marked");
    match how.as_str() {
        "panic" => {
            // The drop never runs: only the panic hook can put the terminal back.
            std::mem::forget(guard);
            panic!("the child panics with its terminal raw");
        }
        "sigterm" => {
            // SAFETY: SIGTERM to this process; the guard's handler takes it.
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
            std::thread::sleep(MUST_HAPPEN);
            panic!("SIGTERM did not end the child");
        }
        "suspend" => {
            let wake = crate::signals::SignalWake::install().expect("the wake");
            crate::signals::suspend(&mut guard).expect("back from the stop");
            guard.write(BACK_MARK).expect("marked");
            drop(guard);
            drop(wake);
        }
        other => panic!("unknown orders {other}"),
    }
}
