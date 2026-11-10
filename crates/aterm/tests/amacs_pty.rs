// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration tests for `amacs` running under a real PTY controlling terminal,
//! verifying terminal mode restoration on normal exit, SIGTERM, and panic.
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::launch_isolation;

const HANG: Duration = Duration::from_secs(60);
const DRIVER: &str = "ATERM_AMACS_PTY_PROGRAM";
const DRIVER_FILE: &str = "ATERM_AMACS_PTY_FILE";
const DRIVER_TEST: &str = "amacs_pty::session_driver";
const PID_MARK: &[u8] = b"<amacs-pid:";
const STATUS_MARK: &[u8] = b"<amacs-status:";

struct DriverChild(aterm_uds::spawnfd::Child);

impl Drop for DriverChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Keep the controlling session alive after the editor exits, as its shell would.
/// The parent checks restoration before killing this owned session leader.
#[test]
fn session_driver() {
    let Some(program) = std::env::var_os(DRIVER) else {
        return;
    };
    let file = std::env::var_os(DRIVER_FILE).expect("the editor file");
    // The editor's pid goes out whole, never split by the editor's own output: on Darwin the
    // editor starts stopped at its exec until the pid is out; Linux has no such start, so the
    // editor runs at once and the pid goes out in one write, which a Linux tty keeps whole
    // against every other writer (`tty_write` holds the terminal's `atomic_write_lock`).
    let suspended = cfg!(target_vendor = "apple");
    let child = aterm_uds::spawnfd::command(program)
        .arg(file)
        .env_remove(DRIVER)
        .env_remove(DRIVER_FILE)
        .stdin(aterm_uds::spawnfd::Stdio::inherit())
        .stdout(aterm_uds::spawnfd::Stdio::inherit())
        .stderr(aterm_uds::spawnfd::Stdio::inherit())
        .start_suspended(suspended)
        .spawn()
        .expect("start the editor inside its controlling session");
    let mut child = DriverChild(child);
    let mut output = std::io::stdout();
    output
        .write_all(format!("<amacs-pid:{}>", child.0.id()).as_bytes())
        .unwrap();
    output.flush().unwrap();
    if suspended {
        child
            .0
            .resume()
            .expect("the editor may run after its identity is published");
    }
    let status = child.0.wait().expect("the editor exits");
    output
        .write_all(format!("<amacs-status:{}>", status.into_raw()).as_bytes())
        .unwrap();
    output.flush().unwrap();
    // This is the fixture's hang detector if its parent disappears, not an exit delay.
    std::thread::sleep(HANG);
    aterm_uds::process::kill_self_now();
}

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
    libc::speed_t,
    libc::speed_t,
) {
    (
        t.c_iflag,
        t.c_oflag,
        t.c_cflag,
        t.c_lflag,
        t.c_cc.to_vec(),
        t.c_ispeed,
        t.c_ospeed,
    )
}

/// The master end of the child's terminal, and what the child has written to it.
struct Terminal {
    master: Option<std::fs::File>,
    out: Vec<u8>,
}

impl Terminal {
    /// Read what the master holds now. The parent holds the slave open, so a dry read is
    /// `WouldBlock`, never the end.
    fn drain(&mut self) {
        let master = self.master.as_mut().expect("the terminal is open");
        let mut chunk = [0u8; 4096];
        loop {
            match master.read(&mut chunk) {
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
        let master = self.master.as_mut().expect("the terminal is open");
        master
            .write_all(bytes)
            .and_then(|()| master.flush())
            .expect("typing at the terminal");
    }

    fn fd(&self) -> RawFd {
        self.master
            .as_ref()
            .expect("the terminal is open")
            .as_raw_fd()
    }
}

/// `amacs notes.txt` on a fresh pty in a scratch home, run until it has drawn its first
/// frame, with `env` beside the isolation's.
struct Session {
    root: aterm_tempfile::TempDir,
    child: Child,
    program: libc::pid_t,
    tty: Terminal,
    /// The terminal's settings before amacs ran.
    before: libc::termios,
    /// What the terminal held after the first editor frame, to localize restore failures.
    running: libc::termios,
    /// Held open so the master never sees a hangup and can read termios after child exit.
    slave: Option<OwnedFd>,
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
        let driver = std::env::current_exe().expect("the fixture executable");

        let mut before = None;
        let (master, slave, child) = aterm_pty::spawn_session_leader_on_pty(|slave| {
            before = Some(termios(slave.as_raw_fd()));
            #[expect(
                clippy::disallowed_types,
                reason = "spawn_session_leader_on_pty installs spawnfd::InheritNothing and pre_exec(TIOCSCTTY)"
            )]
            let mut cmd = std::process::Command::new(&driver);
            launch_isolation::apply(&mut cmd, root.path());
            cmd.args(["--exact", DRIVER_TEST, "--nocapture", "--test-threads", "1"])
                .env(DRIVER, &alias)
                .env(DRIVER_FILE, root.path().join("notes.txt"))
                .env_remove("TMUX")
                .env_remove("STY")
                .env("TERM", "xterm-256color")
                .envs(env.iter().copied())
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave.try_clone()?));
            Ok(cmd)
        })
        .expect("run amacs");
        aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("a non-blocking master");
        let before = before.expect("build captured termios before spawn");
        let mut session = Self {
            root,
            child,
            program: 0,
            tty: Terminal {
                master: Some(std::fs::File::from(master)),
                out: Vec::new(),
            },
            before,
            running: before,
            slave: Some(slave),
            at: 0,
        };

        session.program = session.number(PID_MARK);

        // Answer the DA1 probe as a VT220 so startup finishes immediately.
        let at = session.tty.wait_for(b"\x1b[c", 0, &mut session.child);
        session.tty.type_bytes(b"\x1b[?62;22c");
        session.at = session.tty.wait_for(b"-UU-:", at, &mut session.child);
        session.running = termios(session.tty.fd());
        session
    }

    fn number(&mut self, mark: &[u8]) -> libc::c_int {
        let start = self.tty.wait_for(mark, 0, &mut self.child);
        let end = self.tty.wait_for(b">", start, &mut self.child) - 1;
        std::str::from_utf8(&self.tty.out[start..end])
            .unwrap()
            .parse()
            .unwrap()
    }

    fn file(&self) -> std::path::PathBuf {
        self.root.path().join("notes.txt")
    }

    /// Read the editor's result while its parent still owns the controlling terminal.
    fn exit(&mut self, label: &str) -> (std::process::ExitStatus, String) {
        let status = std::process::ExitStatus::from_raw(self.number(STATUS_MARK));
        let shown = String::from_utf8_lossy(&self.tty.out).into_owned();
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "{label}: the fixture's controlling session must outlive the editor"
        );
        (status, shown)
    }

    fn assert_put_back(&self, label: &str) {
        let after = termios(self.tty.fd());
        assert_eq!(
            settings(&after),
            settings(&self.before),
            "{label}: the terminal's settings are put back; running={:?}; speeds before={:?}, running={:?}, after={:?}",
            settings(&self.running),
            (self.before.c_ispeed, self.before.c_ospeed),
            (self.running.c_ispeed, self.running.c_ospeed),
            (after.c_ispeed, after.c_ospeed),
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(master) = self.tty.master.take() {
            let ended = aterm_pty::end_session_on_pty(
                master.into(),
                self.slave.take(),
                &mut self.child,
                HANG,
                &mut |bytes| self.tty.out.extend_from_slice(bytes),
            );
            if let Err(error) = ended {
                if std::thread::panicking() {
                    eprintln!("amacs fixture cleanup: {error}");
                } else {
                    panic!("amacs fixture cleanup: {error}");
                }
            }
        }
    }
}

/// `amacs` edits and saves a file under a real PTY and restores termios on exit.
#[test]
fn amacs_edits_a_file_under_a_real_pty() {
    let aterm = launch_isolation::aterm_executable();
    let aterm = aterm.as_path();
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

/// `SIGTERM` restores termios and the main screen without saving unsaved changes.
#[test]
fn sigterm_puts_the_terminal_back() {
    use std::os::unix::process::ExitStatusExt as _;

    let aterm = launch_isolation::aterm_executable();
    let aterm = aterm.as_path();
    aterm_spec::wait::first_exec(aterm, &["--help"]);
    let label = "SIGTERM";
    let mut session = Session::start(aterm, &[]);
    let at = session.at;
    session.tty.type_bytes(b"hello");
    let at = session.tty.wait_for(b"**", at, &mut session.child);
    // SAFETY: `program` is the running editor whose frame this fixture just observed.
    assert_eq!(
        unsafe { libc::kill(session.program, libc::SIGTERM) },
        0,
        "kill"
    );
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

/// A panic restores termios and the main screen before printing its message.
#[test]
fn a_panic_puts_the_terminal_back() {
    let aterm = launch_isolation::aterm_executable();
    let aterm = aterm.as_path();
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
