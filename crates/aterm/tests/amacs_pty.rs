// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs edits under a REAL terminal (design §5.1, §5.5): the built binary, run as `amacs`
//! (the argv0 alias every install lays), with a pty as its controlling terminal and stdio,
//! as over ssh or in Terminal.app. Outside an aterm window, and under tmux inside one, the
//! terminal face edits in the terminal itself: it probes the terminal, draws, takes keys,
//! saves through amacs-files, and on exit puts the terminal's settings back exactly.
//!
//! The pty pair is aterm-pty's `open_pty_pair`, both ends close-on-exec from the open; the
//! child makes the slave its controlling terminal (`setsid`, `TIOCSCTTY`), so its
//! `/dev/tty` is this pty and never the terminal the test runner was started from. The
//! settings are read through the master: a pair has one termios, and the BSDs revoke every
//! descriptor of the slave when the session leader the child became exits.
//!
//! A SIGTERM mid-edit puts the terminal back too (aterm-tty's guard), and amacs ends as
//! SIGTERM ends a program; with no draft journal yet (slice T5) the unsaved edit is lost. So
//! does a panic, which a debug build's development seam injects (`ATERM_AMACS_INJECT_PANIC`,
//! read only through `aterm_types::dev_seam!`): the terminal is put back before the panic's
//! message is printed, so the message lands on the shell's screen.
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::launch_isolation;

/// A wait for something that must happen: a hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// The terminal settings of the pty `fd` is either end of.
fn termios(fd: RawFd) -> libc::termios {
    // SAFETY: `termios` is plain data for which all-zero is a valid value.
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is an open terminal descriptor and `settings` a live termios.
    let got = unsafe { libc::tcgetattr(fd, &raw mut settings) };
    assert_eq!(got, 0, "tcgetattr: {}", std::io::Error::last_os_error());
    settings
}

/// The fields of a terminal's settings a program changes to go raw, and must put back.
fn settings(
    t: &libc::termios,
) -> (
    libc::tcflag_t,
    libc::tcflag_t,
    libc::tcflag_t,
    libc::tcflag_t,
    Vec<u8>,
) {
    (t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag, t.c_cc.to_vec())
}

/// The master end of the child's terminal, and what the child has written to it.
struct Terminal {
    master: std::fs::File,
    out: Vec<u8>,
}

impl Terminal {
    /// Read what the master holds now. The parent holds the slave open, so a dry read is
    /// `WouldBlock`, never the end.
    fn drain(&mut self) {
        let mut chunk = [0u8; 4096];
        loop {
            match self.master.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => self.out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) => panic!("reading the pty master failed: {e}"),
            }
        }
    }

    /// Wait until the child has written `needle` since byte `from` of its output.
    fn wait_for(&mut self, needle: &[u8], from: usize, child: &mut Child) -> usize {
        let start = Instant::now();
        loop {
            self.drain();
            if let Some(at) = self
                .out
                .get(from..)
                .and_then(|out| out.windows(needle.len()).position(|w| w == needle))
            {
                return from + at + needle.len();
            }
            let exited = child.try_wait().expect("wait on amacs");
            assert!(
                exited.is_none() && start.elapsed() < HANG,
                "amacs never wrote {:?} ({exited:?}); it wrote {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.out)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn type_bytes(&mut self, bytes: &[u8]) {
        self.master
            .write_all(bytes)
            .and_then(|()| self.master.flush())
            .expect("typing at the terminal");
    }
}

/// `amacs notes.txt` on a fresh pty in a scratch home, run until it has drawn its first
/// frame, with `env` beside the isolation's.
struct Session {
    root: aterm_tempfile::TempDir,
    child: Child,
    tty: Terminal,
    /// The terminal's settings before amacs ran.
    before: libc::termios,
    /// The slave end, held to the end so the master never reads a hangup.
    _slave: OwnedFd,
    /// Where the first frame's output ends.
    at: usize,
}

impl Session {
    fn start(aterm: &Path, env: &[(&str, &str)]) -> Self {
        let root = launch_isolation::scratch_root("amacs-pty");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("a bin dir");
        let alias = bin.join("amacs");
        std::os::unix::fs::symlink(aterm, &alias).expect("lay the amacs alias");

        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: two fresh descriptors this test alone owns.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("a non-blocking master");
        let before = termios(master.as_raw_fd());

        #[expect(
            clippy::disallowed_types,
            reason = "the controlling PTY needs pre_exec(TIOCSCTTY); launch_isolation::apply installs spawnfd::InheritNothing before that hook"
        )]
        let mut cmd = std::process::Command::new(&alias);
        launch_isolation::apply(&mut cmd, root.path());
        cmd.arg(root.path().join("notes.txt"))
            .env_remove("TMUX")
            .env_remove("STY")
            .env("TERM", "xterm-256color")
            .envs(env.iter().copied())
            .stdin(Stdio::from(slave.try_clone().expect("dup the slave")))
            .stdout(Stdio::from(slave.try_clone().expect("dup the slave")))
            .stderr(Stdio::from(slave.try_clone().expect("dup the slave")));
        // SAFETY: the hook runs in the forked child before `exec` and calls only
        // async-signal-safe functions (`setsid`, `ioctl`) on its own stdin, the slave.
        unsafe {
            cmd.pre_exec(|| {
                // `as _`: the request's type is the platform's (`c_uint` on macOS,
                // `c_ulong` on glibc, `c_int` on musl).
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("run amacs");
        // The parent's copies go with the `Command`; `slave` stays held until the end.
        drop(cmd);
        // Own the child before a readiness wait or write can panic, so a
        // failed startup takes the same kill/reap path as a failed test.
        let mut session = Self {
            root,
            child,
            tty: Terminal {
                master: std::fs::File::from(master),
                out: Vec::new(),
            },
            before,
            _slave: slave,
            at: 0,
        };

        // The probe ends with DA1; answer it as a VT220 does, so the probe ends at once.
        let at = session.tty.wait_for(b"\x1b[c", 0, &mut session.child);
        session.tty.type_bytes(b"\x1b[?62;22c");
        session.at = session.tty.wait_for(b"-UU-:", at, &mut session.child);
        session
    }

    fn file(&self) -> std::path::PathBuf {
        self.root.path().join("notes.txt")
    }

    /// Wait up to [`HANG`] for amacs to exit, reading the terminal meanwhile: a session
    /// leader's exit waits until its terminal's output is read (XNU's `ttywait`), as a
    /// terminal emulator always reads it.
    fn reap(&mut self) -> Option<std::process::ExitStatus> {
        let start = Instant::now();
        loop {
            self.tty.drain();
            if let Ok(Some(status)) = self.child.try_wait() {
                self.tty.drain();
                return Some(status);
            }
            if start.elapsed() >= HANG {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait for amacs to exit, and what it wrote in all.
    fn exit(&mut self, label: &str) -> (std::process::ExitStatus, String) {
        let status = self.reap();
        let shown = String::from_utf8_lossy(&self.tty.out).into_owned();
        let status = status.unwrap_or_else(|| panic!("{label}: amacs did not exit: {shown:?}"));
        (status, shown)
    }

    /// The terminal's settings are what they were before amacs ran.
    fn assert_put_back(&self, label: &str) {
        assert_eq!(
            settings(&termios(self.tty.master.as_raw_fd())),
            settings(&self.before),
            "{label}: the terminal's settings are put back"
        );
    }
}

impl Drop for Session {
    /// A test that failed while amacs runs kills it, and reads the terminal until it is
    /// gone (or the hang detector gives up on it).
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.reap();
        }
    }
}

/// `amacs NEWFILE` in a real terminal, typing `hello`, `C-x C-s` and `C-x C-c`: the file
/// holds `hello`, amacs exits 0, and the terminal's settings are what they were. Run as an
/// ssh login would (no aterm window), and as tmux inside an aterm window would (`TMUX` set
/// beside the window's session id), which also edits in the terminal and dials no window.
#[test]
fn amacs_edits_a_file_under_a_real_pty() {
    let aterm = Path::new(env!("CARGO_BIN_EXE_aterm"));
    // A fresh build's first exec can wait out the system's code-policy check: run it once,
    // unclocked, before the hang detector starts.
    aterm_spec::wait::first_exec(aterm, &["--help"]);
    for (label, env) in [
        ("outside a window", &[][..]),
        (
            "tmux inside a window",
            &[
                ("TMUX", "/tmp/tmux-1/default,1,0"),
                ("ATERM_PARENT_SESSION_ID", "s-1"),
            ][..],
        ),
    ] {
        let mut session = Session::start(aterm, env);
        let at = session.at;
        session.tty.type_bytes(b"hello");
        session.tty.type_bytes(b"\x18\x13");
        let at = session.tty.wait_for(b"Wrote ", at, &mut session.child);
        session.tty.type_bytes(b"\x18\x03");
        session.tty.wait_for(b"\x1b[?1049l", at, &mut session.child);
        let (status, shown) = session.exit(label);
        assert!(status.success(), "{label}: {status:?}; it wrote {shown:?}");
        assert_eq!(
            std::fs::read_to_string(session.file()).expect("the saved file"),
            "hello",
            "{label}"
        );
        session.assert_put_back(label);
        assert!(
            shown.contains("\x1b[?1049h"),
            "{label}: the alternate screen"
        );
        assert!(
            !shown.contains("amacs:"),
            "{label}: a clean run says nothing: {shown:?}"
        );
    }
}

/// SIGTERM while amacs edits an unsaved change: the terminal is put back (its settings, the
/// main screen), amacs ends as SIGTERM ends a program, and the file was never written.
#[test]
fn sigterm_puts_the_terminal_back() {
    use std::os::unix::process::ExitStatusExt as _;

    let aterm = Path::new(env!("CARGO_BIN_EXE_aterm"));
    aterm_spec::wait::first_exec(aterm, &["--help"]);
    let label = "SIGTERM";
    let mut session = Session::start(aterm, &[]);
    let at = session.at;
    session.tty.type_bytes(b"hello");
    // The modeline's `**` says the keys reached the buffer.
    let at = session.tty.wait_for(b"**", at, &mut session.child);
    let pid = libc::pid_t::try_from(session.child.id()).expect("a pid");
    // SAFETY: `pid` is this test's own child, not yet reaped.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "kill");
    let (status, shown) = session.exit(label);
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{label}: {status:?}");
    session.assert_put_back(label);
    let after = session.tty.out.get(at..).unwrap_or_default();
    assert!(
        after.windows(8).any(|w| w == b"\x1b[?1049l"),
        "{label}: the main screen is back: {shown:?}"
    );
    assert!(!session.file().exists(), "{label}: nothing was saved");
}

/// A panic while amacs edits an unsaved change (the seam's key): the terminal is put back
/// (its settings, the main screen) before the panic's message is printed, amacs exits as a
/// panic does (101), and the file was never written.
#[test]
fn a_panic_puts_the_terminal_back() {
    let aterm = Path::new(env!("CARGO_BIN_EXE_aterm"));
    aterm_spec::wait::first_exec(aterm, &["--help"]);
    let label = "panic";
    let mut session = Session::start(aterm, &[("ATERM_AMACS_INJECT_PANIC", "~")]);
    let at = session.at;
    session.tty.type_bytes(b"hello");
    let at = session.tty.wait_for(b"**", at, &mut session.child);
    session.tty.type_bytes(b"~");
    let (status, shown) = session.exit(label);
    assert_eq!(
        status.code(),
        Some(101),
        "{label}: {status:?}; it wrote {shown:?}"
    );
    session.assert_put_back(label);
    let after = session.tty.out.get(at..).unwrap_or_default();
    let main_screen = after
        .windows(8)
        .position(|w| w == b"\x1b[?1049l")
        .unwrap_or_else(|| panic!("{label}: the main screen is back: {shown:?}"));
    let message = b"ATERM_AMACS_INJECT_PANIC";
    let said = after
        .windows(message.len())
        .position(|w| w == message)
        .unwrap_or_else(|| panic!("{label}: the panic's message: {shown:?}"));
    assert!(
        main_screen < said,
        "{label}: the message is printed on the main screen: {shown:?}"
    );
    assert!(!session.file().exists(), "{label}: nothing was saved");
}
