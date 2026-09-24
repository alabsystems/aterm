// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A PERSON'S VERB WAITS FOR THE STORE (2026-09-23), at the real process edge.
//!
//! `store_lock_wait.rs` pins the verbs nobody watches: a typed verb whose stdin is not a
//! terminal still refuses at once with exit 75. This is the other half. A verb whose
//! stdin and stderr are a terminal — a pseudo-terminal here, which `isatty` cannot tell
//! from a person's — meets a held store, says nothing for the 2 s grace, then draws ONE
//! spinner line on stderr ("Waiting for aterm's background update to finish…"), and runs
//! when the holder lets go, the line cleared and nothing on stdout. The negative control
//! is the same verb with stdin off the terminal: exit 75 at once, the old sentence.
//!
//! NEVER THE REAL STORE: the child's HOME is a temp directory, its XDG_CONFIG_HOME an
//! absent one, and `<prefix>/declined` makes `seed` exit 0 with one sentence before any
//! index work, as in `store_lock_wait.rs`.

#![cfg(unix)]

use std::io::Read as _;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAITING: &str = "Waiting for aterm's background update to finish";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "atpkg-typed-lock-tty-{}-{case}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(prefix.starts_with(&home), "{}", prefix.display());
        Self {
            root,
            home,
            layout: atpkg::store::Layout { prefix },
        }
    }

    /// Hold the store lock in THIS process, and decline the toolset so the child's
    /// `seed` exits 0 the moment it runs.
    fn hold(&self) -> atpkg::lock::StoreLock {
        let guard = atpkg::lock::try_lock_store(&self.layout).expect("the fixture holds the lock");
        std::fs::write(self.layout.declined(), b"declined by the test fixture\n").unwrap();
        guard
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg("seed")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.join("registry").display()),
            )
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8")
            .stdout(Stdio::piped());
        cmd
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A pseudo-terminal: the master end (read by the test) and the slave end (the child's
/// terminal).
fn pty() -> (std::fs::File, OwnedFd) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `openpty` writes two descriptors it opened into the two ints; the name
    // and termios arguments are null (not wanted), and the window size is read only.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors were just opened by `openpty` and are owned by nothing else.
    unsafe {
        (
            std::fs::File::from(OwnedFd::from_raw_fd(master)),
            OwnedFd::from_raw_fd(slave),
        )
    }
}

/// Everything the child draws on its terminal, each read stamped with when it arrived.
type Screen = Arc<Mutex<Vec<(Duration, Vec<u8>)>>>;

fn drain(mut master: std::fs::File, started: Instant) -> Screen {
    let screen: Screen = Arc::default();
    let sink = Arc::clone(&screen);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Ends at EOF or EIO — what the master reads once the child's end is closed.
        while let Ok(n) = master.read(&mut buf)
            && n > 0
        {
            sink.lock()
                .unwrap()
                .push((started.elapsed(), buf[..n].to_vec()));
        }
    });
    screen
}

fn text(screen: &Screen) -> String {
    let bytes: Vec<u8> = screen
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, b)| b.clone())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// When `needle` first reached the terminal, if it has.
fn first_seen(screen: &Screen, needle: &str) -> Option<Duration> {
    let chunks = screen.lock().unwrap();
    let mut seen = Vec::new();
    for (at, bytes) in chunks.iter() {
        seen.extend_from_slice(bytes);
        if String::from_utf8_lossy(&seen).contains(needle) {
            return Some(*at);
        }
    }
    None
}

#[test]
fn a_verb_typed_on_a_terminal_waits_behind_a_spinner_then_runs() {
    let fx = Fixture::new("waits");
    let guard = fx.hold();
    let (master, slave) = pty();
    // Kept open here until the end: with no slave end left open the master may drop what
    // a child that has exited wrote before it was read.
    let _held = slave.try_clone().unwrap();
    let started = Instant::now();
    let screen = drain(master, started);
    let mut child = fx
        .command()
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .expect("spawn dev atpkg on a pseudo-terminal");

    // The spinner arrives, and not before the grace.
    let deadline = Instant::now() + Duration::from_secs(15);
    let seen = loop {
        if let Some(at) = first_seen(&screen, WAITING) {
            break at;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "the verb exited ({status}) instead of waiting: {:?}",
                text(&screen)
            );
        }
        assert!(
            Instant::now() < deadline,
            "no spinner within 15 s: {:?}",
            text(&screen)
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        seen >= atpkg::lock::WAIT_ANNOUNCE_GRACE,
        "the spinner came {seen:?} after the spawn, inside the grace"
    );
    assert!(
        text(&screen).contains("Ctrl-C to stop"),
        "{:?}",
        text(&screen)
    );
    assert!(child.try_wait().unwrap().is_none(), "still waiting");

    // The holder lets go: the verb runs and ends as it would have with a free store.
    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the verb did not run once the lock was free"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(status.success(), "{status}: {stdout} / {:?}", text(&screen));
    assert!(
        !stdout.contains(atpkg::cli::LOCK_WAITING_MARKER)
            && !stdout.contains(atpkg::cli::LOCK_ACQUIRED_MARKER),
        "a person's wait prints no marker on stdout: {stdout}"
    );
    // The spinner's line was cleared: after its last draw comes a clear.
    let drawn = text(&screen);
    let last = drawn.rfind(WAITING).expect("drawn");
    assert!(
        drawn[last..].contains("\r\u{1b}[2K"),
        "the line is cleared when the wait ends: {drawn:?}"
    );
}

/// NEGATIVE CONTROL: the same verb, stderr still the terminal, stdin not — nobody is
/// there to have typed it, so it refuses at once, word for word as before.
#[test]
fn the_same_verb_off_a_terminal_still_refuses_at_once() {
    let fx = Fixture::new("refuses");
    let _guard = fx.hold();
    let (master, slave) = pty();
    // Kept open until the end, for the reason the test above gives.
    let _held = slave.try_clone().unwrap();
    let started = Instant::now();
    let screen = drain(master, started);
    let output = fx
        .command()
        .stdin(Stdio::null())
        .stderr(Stdio::from(slave))
        .output()
        .expect("run dev atpkg");
    assert_eq!(
        output.status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT)),
        "{:?}",
        text(&screen)
    );
    assert!(
        started.elapsed() < atpkg::lock::WAIT_ANNOUNCE_GRACE + Duration::from_secs(8),
        "no ten-minute wait"
    );
    // The reader may still be draining; give it a moment to see the refusal.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !text(&screen).contains("holds the store lock") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let said = text(&screen);
    assert!(said.contains("holds the store lock"), "{said:?}");
    assert!(!said.contains(WAITING), "{said:?}");
}
