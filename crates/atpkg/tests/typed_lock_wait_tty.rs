// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A PERSON'S VERB WAITS FOR THE STORE, at the real process edge.
//!
//! `store_lock_wait.rs` pins the verbs nobody watches: a typed verb whose stdin is not a
//! terminal still refuses at once with exit 75. This is the other half. A verb whose
//! stdin and stderr are a terminal — a pseudo-terminal here, which `isatty` cannot tell
//! from a person's — meets a held store, says nothing for the grace (none here), then
//! draws ONE spinner line on stderr ("Waiting for aterm's background update to finish…"),
//! and runs when the holder lets go, the line cleared and nothing on stdout. The negative
//! control is the same verb with stdin off the terminal: exit 75 at once, the old sentence.
//!
//! NEVER THE REAL STORE: the child's HOME is a temp directory, its XDG_CONFIG_HOME an
//! absent one, and `<prefix>/declined` makes `seed` exit 0 with one sentence before any
//! index work, as in `store_lock_wait.rs`.

#![cfg(unix)]

use std::io::Read as _;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::PathBuf;
// `spawnfd`'s: the waiter holds its terminal and nothing else of this process,
// not a pipe another test thread had open unflagged at the spawn.
use aterm_uds::spawnfd::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;

const WAITING: &str = "Waiting for aterm's background update to finish";

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-typed-lock-tty-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
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
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
            )
            .env(atpkg::lock::WAIT_ANNOUNCE_GRACE_SEAM, "0")
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8")
            .stdout(Stdio::piped());
        cmd
    }
}

/// A pseudo-terminal: the master end (read by the test) and the slave end (the child's
/// terminal).
fn pty() -> (std::fs::File, OwnedFd) {
    // Both ends close-on-exec from birth: the verb holds its terminal and never a
    // copy of its master, and no sibling test's child inherits either end.
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    aterm_pty::resize(master, 24, 100);
    // SAFETY: both descriptors were just opened above and are owned by nothing else.
    unsafe {
        (
            std::fs::File::from(OwnedFd::from_raw_fd(master)),
            OwnedFd::from_raw_fd(slave),
        )
    }
}

/// Everything the child draws on its terminal.
type Screen = Arc<Mutex<Vec<u8>>>;

fn drain(mut master: std::fs::File) -> Screen {
    let screen: Screen = Arc::default();
    let sink = Arc::clone(&screen);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Ends at EOF or EIO — what the master reads once the child's end is closed.
        while let Ok(n) = master.read(&mut buf)
            && n > 0
        {
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    screen
}

fn text(screen: &Screen) -> String {
    String::from_utf8_lossy(&screen.lock().unwrap()).into_owned()
}

#[test]
fn a_verb_typed_on_a_terminal_waits_behind_a_spinner_then_runs() {
    let fx = Fixture::new("waits");
    let guard = fx.hold();
    let (master, slave) = pty();
    // Kept open here until the end: with no slave end left open the master may drop what
    // a child that has exited wrote before it was read.
    let _held = slave.try_clone().unwrap();
    let screen = drain(master);
    let mut child = fx
        .command()
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .expect("spawn dev atpkg on a pseudo-terminal");

    // The spinner arrives.
    let deadline = Instant::now() + MUST_HAPPEN;
    loop {
        if text(&screen).contains(WAITING) {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "the verb exited ({status}) instead of waiting: {:?}",
                text(&screen)
            );
        }
        assert!(
            Instant::now() < deadline,
            "no spinner within a minute: {:?}",
            text(&screen)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(child.try_wait().unwrap().is_none(), "still waiting");

    // The holder lets go: the verb runs and ends as it would have with a free store.
    drop(guard);
    let deadline = Instant::now() + MUST_HAPPEN;
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
    let screen = drain(master);
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
    // The regression is a TEN-MINUTE wait, so the bound is two minutes: the interval
    // also holds the spawn and first exec of a freshly linked dev binary, which macOS
    // assesses before it runs (26 s measured once under load). The refusal's own
    // promptness is the no-spinner assert below, which no clock can flake.
    assert!(
        started.elapsed() < Duration::from_secs(120),
        "no ten-minute wait"
    );
    // The reader may still be draining; give it a moment to see the refusal
    // (a poll for text already written: generous costs nothing when it is there).
    let deadline = Instant::now() + MUST_HAPPEN;
    while !text(&screen).contains("holds the store lock") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let said = text(&screen);
    assert!(said.contains("holds the store lock"), "{said:?}");
    assert!(!said.contains(WAITING), "{said:?}");
}
