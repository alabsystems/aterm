// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE VERBS AS THE OWNER RUNS THEM — the real binary, in a real process, against a real
//! terminal and a real absence of one.
//!
//! The library tests in `provision.rs` drive the provisioning logic directly with a
//! synthetic seed. This file exists for the properties that only a PROCESS can
//! demonstrate: where the master is allowed to come from, where it is allowed to GO, and
//! what an argument the tool does not understand is allowed to do.
//!
//! # Both terminal conditions are manufactured, neither is inherited
//!
//! A test harness's own terminal is not a fixture: `cargo test` may or may not have one,
//! and a test that passed because it did would be lying. So this file builds both
//! conditions itself.
//!
//! * **No controlling terminal** — `setsid(2)` in a `pre_exec` hook. `open("/dev/tty")`
//!   then fails with ENXIO no matter what the harness inherited, so the refusals are
//!   deterministic instead of environmental.
//! * **A real controlling terminal** — a pseudo-terminal pair (`posix_openpt`/`grantpt`/
//!   `unlockpt`), with the child doing `setsid` and `TIOCSCTTY` on the slave so the pty
//!   becomes its controlling terminal. `/dev/tty` inside the child then resolves to
//!   something this test holds the other end of, which is what makes it possible to assert
//!   the two things that matter most: the phrase APPEARS on the terminal, and it appears
//!   NOWHERE else — not on the captured stdout, not on stderr, not in any file.
//!
//! `/dev/tty` resolves through the controlling terminal, not through fd 0, which is why
//! the child's stdout can be a pipe at the same time. That is not a trick to make the test
//! work; it is exactly the shape of `atpkg-keys setup > setup.log`, and the reason that
//! invocation can no longer put a master into a file.
//!
//! Nothing here needs a network, an Apple account, or a real key: the masters are
//! obviously synthetic and the machine keys are generated on the spot.

#![cfg(unix)]

use aterm_update_core::roster::{Roster, verify_roster};
use atpkg_keys::master::parse_master;
use atpkg_keys::pins_edit::{CHANNEL_ANCHOR, MASTER_ANCHOR, read_anchor};
use atpkg_keys::roster_ops::{add, empty};
use std::io::Read as _;
use std::io::Write as _;
use std::os::unix::io::FromRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// An obviously synthetic master. It appears nowhere outside tests.
const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

/// The real head of the shipped keyset, used here purely as a realistic incumbent whose
/// survival is being asserted.
const HEAD_KEY: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";

/// 2026-08-04T00:00:00Z.
const NOW: u64 = 1_785_801_600;

/// A `pins.rs` in the two shapes the real file uses. The argument is either empty (the
/// unarmed anchor) or a single key to arm it with.
fn pins_fixture(master_entry: &str) -> String {
    let mut s = String::from(
        "// Copyright 2026 Andrew Yates\n\
         // SPDX-License-Identifier: Apache-2.0\n\
         \n\
         /// The paper master. Empty here means INERT.\n",
    );
    if master_entry.is_empty() {
        s.push_str("pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n");
    } else {
        s.push_str("pub const PAPER_MASTER_PUBKEYS: &[&str] = &[\n    \"");
        s.push_str(master_entry);
        s.push_str("\",\n];\n");
    }
    s.push_str(
        "\n\
         /// The channel keyset. ORDER IS A CONTRACT: index 0 is the head.\n\
         pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n\
         \x20   // K1 — HEAD.\n\
         \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
         ];\n",
    );
    s
}

/// A private scratch tree for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("atpkg-keys-cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn p(dir: &Path, name: &str) -> String {
    dir.join(name).to_str().expect("utf-8 path").to_string()
}

/// The environment every child gets: a `$HOME` inside the scratch tree, plus several
/// plausible "master" variables, because one of the properties under test is that no
/// environment variable is consulted for the phrase (leak vector 2).
fn base_command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg-keys"));
    cmd.args(args)
        .env("HOME", dir)
        .env("ATPKG_MASTER", PAPER)
        .env("ATERM_MASTER", PAPER)
        .env("MASTER", PAPER)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Spawn the real binary with NO controlling terminal and `stdin` taken from `stdin_path`.
fn run(dir: &Path, stdin_path: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = base_command(dir, args);
    match stdin_path {
        Some(path) => {
            let f = std::fs::File::open(path).expect("stdin fixture");
            cmd.stdin(Stdio::from(f));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    // SAFETY: `setsid` is async-signal-safe and is the documented way to detach a child
    // from the parent's controlling terminal. It runs post-fork, pre-exec, and touches no
    // state shared with the parent. Its return value is deliberately ignored: failing
    // because the child is already a session leader is harmless, and the assertions below
    // do not depend on which of the two ways `/dev/tty` becomes unavailable.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.output().expect("the atpkg-keys binary runs")
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Write an armed `pins.rs` plus a roster the synthetic master signed, and return the
/// master's public key.
fn armed_tree(dir: &Path) -> String {
    let seed = parse_master(PAPER).expect("synthetic phrase").seed();
    let master_pub = seed.pubkey_b64().expect("master identity");
    std::fs::write(p(dir, "pins.rs"), pins_fixture(&master_pub)).expect("pins fixture");

    let (_, m3_pub) = atpkg_keys::generate().expect("a machine key");
    let roster = add(empty(NOW), "m3", &m3_pub, NOW).expect("m3 joins");
    let bytes = roster.to_toml().expect("valid roster").into_bytes();
    let sig = seed.sign(&bytes).expect("the master signs");
    std::fs::write(p(dir, "roster.toml"), &bytes).expect("roster");
    std::fs::write(p(dir, "roster.toml.sig"), &sig).expect("roster sig");
    master_pub
}

// ---------------------------------------------------------------------------
// A REAL TERMINAL, built rather than inherited.
// ---------------------------------------------------------------------------

/// The master side of a pseudo-terminal, and what the child must open to get the slave.
struct Pty {
    master: std::fs::File,
    slave: String,
}

/// `ptsname(3)` returns a pointer into a static buffer, so two tests calling it at the same
/// time can read each other's answer. `cargo test` runs tests in threads by default, so the
/// open-and-name sequence is serialised here rather than left to luck — a flaky terminal
/// fixture would discredit every assertion built on it.
static PTY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Pty {
    /// Open a pty pair and return the master end plus the slave's path.
    fn open() -> Self {
        let _guard = PTY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the three calls are the POSIX pty-opening sequence, each checked. They
        // take no pointers from us except in `ptsname`, whose result is copied out before
        // the lock is released.
        unsafe {
            let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(fd >= 0, "posix_openpt: {}", std::io::Error::last_os_error());
            assert_eq!(libc::grantpt(fd), 0, "grantpt");
            assert_eq!(libc::unlockpt(fd), 0, "unlockpt");
            let name = libc::ptsname(fd);
            assert!(!name.is_null(), "ptsname");
            let slave = std::ffi::CStr::from_ptr(name)
                .to_str()
                .expect("a pty path is ASCII")
                .to_string();
            // Non-blocking, so the drain loop below can poll with a deadline instead of
            // parking forever if the child never says anything.
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            Self {
                master: std::fs::File::from_raw_fd(fd),
                slave,
            }
        }
    }

    /// Spawn the binary with this pty as its CONTROLLING TERMINAL, while stdout and stderr
    /// stay pipes.
    ///
    /// That split is the whole point: `/dev/tty` resolves through the controlling terminal,
    /// so the child has one even though its stdout is a pipe — which is precisely the shape
    /// of `atpkg-keys setup | tee log`, the invocation that used to write a master into a
    /// file.
    fn spawn(&self, dir: &Path, args: &[&str]) -> Child {
        let slave = std::ffi::CString::new(self.slave.clone()).expect("no NUL in a pty path");
        let mut cmd = base_command(dir, args);
        cmd.stdin(Stdio::null());
        // SAFETY: everything in the hook is async-signal-safe (`setsid`, `open`, `ioctl`)
        // and touches no state shared with the parent. `slave` is a CString that lives in
        // the closure, so its pointer is valid for the call.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let fd = libc::open(slave.as_ptr(), libc::O_RDWR);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Make it the controlling terminal of this brand-new session. The
                // descriptor is deliberately left open: the association survives a close,
                // but keeping it costs nothing and makes the intent obvious.
                if libc::ioctl(fd, libc::c_ulong::from(libc::TIOCSCTTY), 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().expect("the atpkg-keys binary runs")
    }

    /// Read whatever the child has written to the terminal so far, appending to `into`.
    fn drain(&mut self, into: &mut String) {
        let mut buf = [0u8; 4096];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => into.push_str(&String::from_utf8_lossy(&buf[..n])),
                // EAGAIN: nothing more right now. EIO: the slave closed, i.e. the child is
                // gone — both mean "stop reading", neither is a failure.
                Err(_) => return,
            }
        }
    }

    /// Type a line at the terminal, the way a human would.
    fn type_line(&mut self, line: &str) {
        self.master
            .write_all(line.as_bytes())
            .and_then(|()| self.master.write_all(b"\n"))
            .and_then(|()| self.master.flush())
            .expect("typing at the terminal");
    }
}

/// What a pty-driven run produced: the child's exit status, its captured stdout/stderr, and
/// everything that appeared ON THE TERMINAL.
struct TtyRun {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    terminal: String,
}

/// Run the binary against a real terminal, optionally typing `phrase` once `prompt_marker`
/// appears on it.
///
/// The wait for the prompt is what makes typing safe: `prompt_for_master` disables echo with
/// `TCSAFLUSH`, which DISCARDS pending input, so a phrase typed before the prompt is thrown
/// away by the tool's own (correct) hardening. Waiting for the prompt is what a human does
/// too.
fn run_on_a_terminal(
    dir: &Path,
    args: &[&str],
    reply: Option<(&str, &str)>,
) -> TtyRun {
    let mut pty = Pty::open();
    let mut child = pty.spawn(dir, args);
    let mut terminal = String::new();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut replied = reply.is_none();
    loop {
        pty.drain(&mut terminal);
        if let Some((marker, phrase)) = reply
            && !replied
            && terminal.contains(marker)
        {
            pty.type_line(phrase);
            replied = true;
        }
        match child.try_wait().expect("wait on the child") {
            Some(_) => break,
            None => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("the child never exited; terminal so far:\n{terminal}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    // One last drain: the child may have written and exited between two polls.
    pty.drain(&mut terminal);
    let out = child.wait_with_output().expect("collect the child's output");
    TtyRun {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        terminal,
    }
}

/// The phrase line currently on the terminal — 52 alphabet characters in space-separated
/// groups, returned WITH the grouping (`parse_master` strips it).
fn phrase_on(terminal: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    terminal.lines().map(str::trim).find_map(|l| {
        (!l.is_empty()
            && l.bytes().all(|b| b == b' ' || ALPHABET.contains(&b))
            && l.bytes().filter(|b| *b != b' ').count() == 52)
            .then(|| l.to_string())
    })
}

/// Like [`run_on_a_terminal`], but answers `setup`'s transcription gate: when the retype
/// prompt appears, the phrase already on the terminal is typed back — the scripted
/// equivalent of an operator reading their own paper.
fn run_on_a_terminal_retyping_phrase(dir: &Path, args: &[&str]) -> TtyRun {
    let mut pty = Pty::open();
    let mut child = pty.spawn(dir, args);
    let mut terminal = String::new();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut replied = false;
    loop {
        pty.drain(&mut terminal);
        if !replied && terminal.contains("retype the phrase FROM YOUR PAPER") {
            let phrase = phrase_on(&terminal).expect("the phrase precedes the retype prompt");
            pty.type_line(&phrase);
            replied = true;
        }
        match child.try_wait().expect("wait on the child") {
            Some(_) => break,
            None => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("the child never exited; terminal so far:\n{terminal}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    pty.drain(&mut terminal);
    let out = child.wait_with_output().expect("collect the child's output");
    TtyRun {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        terminal,
    }
}

/// Every file under `dir`, scanned for `needle` and for any 16-character run of it.
fn assert_nothing_on_disk_holds(dir: &Path, needle: &str) -> usize {
    let mut checked = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("readable scratch tree") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            checked += 1;
            let raw = std::fs::read(&path).expect("readable file");
            let text = String::from_utf8_lossy(&raw);
            assert!(!text.contains(needle), "{path:?} holds the phrase");
            for start in 0..needle.len().saturating_sub(16) {
                assert!(
                    !text.contains(&needle[start..start + 16]),
                    "{path:?} holds a 16-character run of the phrase"
                );
            }
        }
    }
    checked
}

// ---------------------------------------------------------------------------
// WHERE THE MASTER MAY COME FROM.
// ---------------------------------------------------------------------------

/// THE LEAK-VECTOR TEST. `join` is handed the phrase three ways at once — two environment
/// variables, stdin, and a file — and must take it from none of them.
#[test]
fn join_refuses_the_phrase_from_env_stdin_or_a_file() {
    let dir = scratch("join-leak-vectors");
    armed_tree(&dir);
    let phrase_file = p(&dir, "phrase.txt");
    std::fs::write(&phrase_file, PAPER).expect("the file the operator must not be able to use");

    let before_pins = std::fs::read(p(&dir, "pins.rs")).unwrap();
    let before_roster = std::fs::read(p(&dir, "roster.toml")).unwrap();

    let out = run(
        &dir,
        // stdin redirected FROM THE FILE holding the phrase: `join < phrase.txt`.
        Some(&phrase_file),
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );

    assert!(!out.status.success(), "join must refuse: {out:?}");
    let err = stderr_of(&out);
    assert!(
        err.contains("/dev/tty"),
        "the refusal must name where the master actually comes from: {err}"
    );
    assert!(
        !stdout_of(&out).contains(PAPER) && !err.contains(PAPER),
        "a refusal must never echo the phrase back"
    );

    // NOTHING WAS WRITTEN. Not the key, not the anchor, not the roster.
    assert!(
        !Path::new(&p(&dir, "m11.key")).exists(),
        "a refused join must mint no key"
    );
    assert_eq!(std::fs::read(p(&dir, "pins.rs")).unwrap(), before_pins);
    assert_eq!(std::fs::read(p(&dir, "roster.toml")).unwrap(), before_roster);

    // ...and with stdin at /dev/null, which is the shape the retired `machine-mint`
    // property is stated in, the refusal is the same one.
    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("/dev/tty"), "{}", stderr_of(&out));
    assert!(!Path::new(&p(&dir, "m11.key")).exists());
}

/// A FLAG THAT WOULD CARRY THE MASTER IS REFUSED BY NAME, AND SAYS WHY IT MATTERS.
///
/// It used to be accepted and ignored, which is the worst of the three options: the run
/// proceeded, so nothing told the operator that the phrase they had just typed on a command
/// line was now in their shell history and had been visible to every process on the machine
/// via `ps`. Ignoring a secret is not the same as refusing one.
#[test]
fn a_flag_that_would_carry_the_master_is_refused_and_calls_it_compromised() {
    let dir = scratch("master-flag");
    armed_tree(&dir);
    let before = std::fs::read(p(&dir, "pins.rs")).unwrap();

    for flag in ["--master", "--master-file", "--phrase", "--seed"] {
        let out = run(
            &dir,
            None,
            &[
                "join",
                "--id",
                "m11",
                "--pins",
                &p(&dir, "pins.rs"),
                "--roster",
                &p(&dir, "roster.toml"),
                flag,
                PAPER,
            ],
        );
        assert!(!out.status.success(), "{flag} must be refused");
        let err = stderr_of(&out);
        assert!(err.contains(flag), "the refusal names the flag: {err}");
        assert!(
            err.contains("COMPROMISED"),
            "the refusal must say what the operator has to do about it: {err}"
        );
        assert!(err.contains("/dev/tty"), "{err}");
        assert!(!err.contains(PAPER), "the refusal must not echo the value: {err}");
    }
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before,
        "a refused run writes nothing"
    );

    // NEGATIVE CONTROL: the same command WITHOUT the flag gets past argument vetting and
    // fails at the terminal instead — so the refusal above is about the flag.
    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
        ],
    );
    assert!(!stderr_of(&out).contains("COMPROMISED"), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("/dev/tty"), "{}", stderr_of(&out));
}

/// SETUP WITHOUT A TERMINAL REFUSES, AND GENERATES NOTHING.
///
/// This is the redirect case: `setup > setup.log`, `| tee`, `| head -0`, or a wrapper that
/// captures stdout. It used to succeed — writing the 64 hex characters into whatever was on
/// fd 1, which is a permanent plaintext copy of the fleet's root key if the stream was a
/// file and a total loss of it if the stream was discarded, WHILE ARMING THE ANCHOR either
/// way. There is no terminal here, so there is nowhere to deliver a master, so no master is
/// generated at all.
#[test]
fn setup_without_a_terminal_refuses_and_writes_nothing() {
    let dir = scratch("setup-headless");
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");
    let before = std::fs::read(p(&dir, "pins.rs")).unwrap();

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );

    assert!(!out.status.success(), "setup must refuse without a terminal");
    let err = stderr_of(&out);
    assert!(err.contains("/dev/tty"), "{err}");
    assert!(
        err.contains("nothing was written and no phrase was shown"),
        "the refusal must state that no master exists: {err}"
    );
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before,
        "the anchor is untouched"
    );
    assert!(!Path::new(&p(&dir, "m3.key")).exists());
    assert!(!Path::new(&p(&dir, "roster.toml")).exists());

    // And nothing that looks like a phrase reached either captured stream.
    let stdout = stdout_of(&out);
    assert!(
        phrase_on(&stdout).is_none(),
        "no phrase-shaped line anywhere on stdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// WHAT AN ARGUMENT THE TOOL DOES NOT READ IS ALLOWED TO DO: nothing.
// ---------------------------------------------------------------------------

/// `--pins=<path>` IS REFUSED, NOT IGNORED.
///
/// The old parser read only `--name value`, so this spelling fell through to the default:
/// the `pins.rs` found by walking up from the current directory. The operator named one
/// file and the tool armed a trust anchor in another, reporting success. The decoy here is
/// the file the operator named; the checkout is what the tool would have edited instead.
#[test]
fn an_unread_flag_spelling_is_refused_rather_than_silently_replaced_by_a_default() {
    let dir = scratch("flag-equals");
    let checkout = dir.join("checkout/crates/aterm-update-core/src");
    std::fs::create_dir_all(&checkout).expect("fake checkout");
    let discovered = checkout.join("pins.rs");
    std::fs::write(&discovered, pins_fixture("")).expect("the file the tool would find");
    let named = p(&dir, "decoy-pins.rs");
    std::fs::write(&named, pins_fixture("")).expect("the file the operator named");

    let mut pins_eq = String::from("--pins=");
    pins_eq.push_str(&named);
    let mut cmd = base_command(&dir, &["setup", "--id", "oops", &pins_eq]);
    cmd.current_dir(dir.join("checkout")).stdin(Stdio::null());
    // SAFETY: see `run`.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let out = cmd.output().expect("the binary runs");

    assert!(!out.status.success(), "the unread spelling must be refused");
    let err = stderr_of(&out);
    assert!(err.contains("--name=value"), "{err}");
    assert!(err.contains("two tokens"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&discovered).unwrap(),
        pins_fixture(""),
        "THE DISCOVERED CHECKOUT MUST NOT HAVE BEEN ARMED"
    );
    assert_eq!(std::fs::read_to_string(&named).unwrap(), pins_fixture(""));

    // NEGATIVE CONTROL: the two-token form of the same flag is read, and the run gets far
    // enough to fail on the terminal instead of on the argument.
    let out = run(
        &dir,
        None,
        &["setup", "--id", "oops", "--pins", &named, "--roster", &p(&dir, "r.toml")],
    );
    assert!(!stderr_of(&out).contains("--name=value"), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("/dev/tty"), "{}", stderr_of(&out));
}

/// Unknown flags, flags that swallow another flag, and surplus arguments are all refused
/// with a message that names them.
#[test]
fn unknown_and_malformed_arguments_are_refused_by_name() {
    let dir = scratch("bad-args");
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).unwrap();

    let out = run(&dir, None, &["setup", "--id", "m3", "--pinz", &p(&dir, "pins.rs")]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("unknown flag '--pinz'"), "{err}");
    assert!(err.contains("--pins"), "the message lists what IS accepted: {err}");

    let out = run(&dir, None, &["setup", "--id", "--pins", &p(&dir, "pins.rs")]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("as its value, which is another flag"),
        "{}",
        stderr_of(&out)
    );

    let out = run(&dir, None, &["setup", "stray", "--id", "m3"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("unexpected argument 'stray'"),
        "{}",
        stderr_of(&out)
    );

    // NEGATIVE CONTROL: the well-formed invocation is not refused for its arguments.
    let out = run(&dir, None, &["setup", "--id", "m3", "--pins", &p(&dir, "pins.rs")]);
    let err = stderr_of(&out);
    assert!(!err.contains("unknown flag"), "{err}");
    assert!(!err.contains("unexpected argument"), "{err}");
}

// ---------------------------------------------------------------------------
// THE ROSTER FORK.
// ---------------------------------------------------------------------------

/// `join` WITH NO ROSTER REFUSES, and says where to get one.
///
/// This is the DEFAULT state of every second machine: the roster lives under `dist/`,
/// `dist/` is gitignored, and a fresh clone therefore has an armed anchor and no roster.
/// Treating that as "first mint" started a second roster at sequence 1 naming only this
/// machine and signed it with the real master — two valid, same-sequence rosters, each
/// de-authorizing the other's machines, with no fallback for a client that meets a release
/// it cannot attribute.
#[test]
fn join_without_a_roster_refuses_instead_of_forking_one() {
    let dir = scratch("join-no-roster");
    let master_pub = armed_tree(&dir);
    // The roster does not travel with the checkout — remove it to model machine #2.
    std::fs::remove_file(p(&dir, "roster.toml")).unwrap();
    std::fs::remove_file(p(&dir, "roster.toml.sig")).unwrap();

    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no roster at"), "{err}");
    assert!(err.contains("COPY"), "the refusal must say how to fix it: {err}");
    assert!(
        err.contains("will not start a second roster"),
        "and why it will not do it for you: {err}"
    );
    // The refusal comes BEFORE the prompt: the operator is not asked to type 64 characters
    // only to be told the file is missing.
    assert!(!err.contains("/dev/tty"), "{err}");
    assert!(!Path::new(&p(&dir, "roster.toml")).exists(), "no roster was created");
    assert!(!Path::new(&p(&dir, "m11.key")).exists());

    // NEGATIVE CONTROL: put the roster back and the same command reaches the prompt.
    let seed = parse_master(PAPER).unwrap().seed();
    let roster = add(empty(NOW), "m3", HEAD_KEY, NOW).unwrap();
    let bytes = roster.to_toml().unwrap().into_bytes();
    std::fs::write(p(&dir, "roster.toml"), &bytes).unwrap();
    std::fs::write(p(&dir, "roster.toml.sig"), seed.sign(&bytes).unwrap()).unwrap();
    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    let err = stderr_of(&out);
    assert!(!err.contains("no roster at"), "{err}");
    assert!(err.contains("/dev/tty"), "it got as far as the prompt: {err}");
    let _ = master_pub;
}

// ---------------------------------------------------------------------------
// THE HAPPY PATHS, ON A REAL TERMINAL.
// ---------------------------------------------------------------------------

/// `setup` END TO END, WITH THE PHRASE GOING TO THE TERMINAL AND NOWHERE ELSE.
///
/// One command; the human's only job is to copy what appears on the screen. Everything
/// asserted here is what the operator would otherwise have done by hand — and the phrase
/// is checked to be on the terminal, absent from both captured streams, and absent from
/// every byte written to disk.
#[test]
fn setup_over_a_real_terminal_shows_the_phrase_there_and_never_on_stdout() {
    let dir = scratch("setup-tty");
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");

    let run = run_on_a_terminal_retyping_phrase(
        &dir,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
            "--head-id",
            "m21",
        ],
    );
    assert!(
        run.status.success(),
        "setup must succeed on a terminal.\nstderr: {}\nterminal: {}",
        run.stderr,
        run.terminal
    );

    // THE PHRASE IS ON THE TERMINAL, exactly once, under the heading.
    let phrase = phrase_on(&run.terminal).expect("the phrase on the terminal");
    assert_eq!(
        run.terminal.matches(phrase.as_str()).count(),
        1,
        "shown once and never repeated"
    );
    assert!(
        run.terminal.contains("MASTER PHRASE — write it on paper"),
        "the heading shares the surface with the line it is about: {}",
        run.terminal
    );
    assert!(
        run.terminal.contains("paper proven"),
        "the transcription gate must confirm the paper before arming: {}",
        run.terminal
    );

    // AND IT IS ON NO OTHER SURFACE. This is the property the redirect broke.
    assert!(!run.stdout.contains(&phrase), "stdout: {}", run.stdout);
    assert!(!run.stderr.contains(&phrase), "stderr: {}", run.stderr);
    for start in 0..48 {
        assert!(!run.stdout.contains(&phrase[start..start + 16]));
        assert!(!run.stderr.contains(&phrase[start..start + 16]));
    }

    // THE ANCHOR IS ARMED WITH THE MASTER THAT PHRASE DERIVES.
    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    let master_pub = parse_master(&phrase)
        .expect("the shown phrase parses")
        .seed()
        .pubkey_b64()
        .unwrap();
    assert_eq!(
        read_anchor(&src, MASTER_ANCHOR).unwrap().members,
        vec![master_pub.clone()],
        "the tool wrote the anchor the operator would otherwise have pasted"
    );

    // THE KEYSET IS UNTOUCHED. The minted machine is authorized by the ROSTER; a keyset
    // entry would be a grant no `machine-revoke` could ever take back, made to clients
    // this tool cannot reach anyway.
    let channel = read_anchor(&src, CHANNEL_ANCHOR).unwrap();
    assert_eq!(channel.members, vec![HEAD_KEY.to_string()]);
    assert_eq!(channel.head(), Some(HEAD_KEY), "the incumbent head survives");
    // THE ROSTER NAMES THE INCUMBENT FIRST, THEN THIS MACHINE. Without the first entry,
    // committing this anchor would leave the machine holding the head key — the one key
    // clients that predate the roster can verify — unable to cut.
    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    assert_eq!(roster.machines.len(), 2);
    assert_eq!(roster.machines[0].id, "m21", "--head-id named the incumbent");
    assert_eq!(roster.machines[0].pubkey, HEAD_KEY);
    assert_eq!(roster.machines[1].id, "m3");
    // THE MINTED KEY IS ON THE ROSTER AND IN NO ANCHOR — read from the roster, because
    // the anchor file is no longer a place it could be read from.
    let machine_pub = roster.machines[1].pubkey.clone();
    assert!(
        !src.contains(&machine_pub),
        "the minted key must appear nowhere in the anchor file: {src}"
    );

    // THE MACHINE'S OWN FILES.
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(p(&dir, "m3.key")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let record = std::fs::read_to_string(dir.join(".aterm/machine.toml")).unwrap();
    assert!(record.contains("id = \"m3\""), "{record}");

    // THE CLOSING OUTPUT — on stdout, where it belongs — says what is not yet true.
    assert!(run.stdout.contains("a commit makes it durable"), "{}", run.stdout);
    assert!(
        run.stdout.contains("no pre-roster client is left"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("--strand-pre-roster-clients"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("the ONLY roster this master signs"), "{}", run.stdout);
    assert!(run.stdout.contains("incumbent keyset head"), "{}", run.stdout);

    // THE PHRASE REACHED NO FILE.
    let checked = assert_nothing_on_disk_holds(&dir, &phrase);
    assert!(checked >= 5, "the run must have written five files; saw {checked}");

    // AND A SECOND `setup` IS REFUSED, because the anchor it just wrote is committed.
    let again = run_on_a_terminal(
        &dir,
        &[
            "setup",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
        None,
    );
    assert!(!again.status.success());
    assert!(again.stderr.contains("ALREADY committed"), "{}", again.stderr);
    assert_eq!(
        std::fs::read_to_string(p(&dir, "pins.rs")).unwrap(),
        src,
        "the refused re-run left the anchor exactly as it was"
    );
}

/// A WRONG RETYPE ARMS NOTHING AND RE-SHOWS THE PHRASE; a correct one then arms. The gate
/// exists because of the 2026-08-14 ceremony failure — a shown-but-never-copied master —
/// and this proves the loop is survivable: mistype, fix the paper, proceed.
#[test]
fn a_mistyped_retype_reshows_the_phrase_and_a_correct_one_arms() {
    let dir = scratch("setup-retype-mismatch");
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");

    let mut pty = Pty::open();
    let mut child = pty.spawn(
        &dir,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    let mut terminal = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut typed_wrong = false;
    let mut typed_right = false;
    loop {
        pty.drain(&mut terminal);
        if !typed_wrong && terminal.contains("retype the phrase FROM YOUR PAPER") {
            // A valid 52-character phrase that is not the shown one.
            let wrong = "7".repeat(51) + "0";
            pty.type_line(&wrong);
            typed_wrong = true;
        }
        if typed_wrong && !typed_right && terminal.contains("NO MATCH") {
            // The re-shown phrase is the fix path: read it as the operator would.
            let phrase = phrase_on(&terminal).expect("the phrase is re-shown after NO MATCH");
            pty.type_line(&phrase);
            typed_right = true;
        }
        match child.try_wait().expect("wait on the child") {
            Some(_) => break,
            None => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("the child never exited; terminal so far:\n{terminal}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    pty.drain(&mut terminal);
    let out = child.wait_with_output().expect("collect the child's output");

    assert!(out.status.success(), "a corrected retype must arm: {terminal}");
    assert!(terminal.contains("NO MATCH"), "{terminal}");
    let phrase = phrase_on(&terminal).expect("phrase shown");
    assert!(
        terminal.matches(phrase.as_str()).count() >= 2,
        "the phrase is re-shown after a mismatch: {terminal}"
    );
    // The wrong retype armed nothing until the right one landed: the anchor holds exactly
    // the master the shown phrase derives.
    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    let master_pub = parse_master(&phrase).expect("parses").seed().pubkey_b64().unwrap();
    assert_eq!(read_anchor(&src, MASTER_ANCHOR).unwrap().members, vec![master_pub]);
}

/// `join` END TO END, WITH THE PHRASE TYPED AT THE TERMINAL.
///
/// The second machine's whole experience: type the 64 characters, and the tool proves them
/// against the committed anchor and the roster the real master signed before it writes
/// anything. A wrong phrase is refused here too, with the tree untouched.
#[test]
fn join_over_a_real_terminal_reads_the_phrase_and_extends_the_roster() {
    let dir = scratch("join-tty");
    let master_pub = armed_tree(&dir);
    let before_pins = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();

    // A WRONG phrase first, so the acceptance below is not a verifier waving anything
    // through. One character differs.
    let mut wrong = String::from(PAPER);
    wrong.replace_range(0..1, "f");
    let bad = run_on_a_terminal(
        &dir,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
        Some(("master phrase", &wrong)),
    );
    assert!(!bad.status.success(), "a wrong phrase must be refused");
    assert!(
        bad.stderr.contains("is NOT the master committed"),
        "stderr: {}\nterminal: {}",
        bad.stderr,
        bad.terminal
    );
    assert_eq!(
        std::fs::read_to_string(p(&dir, "pins.rs")).unwrap(),
        before_pins,
        "a refused join writes nothing"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());
    // ECHO WAS OFF: what the operator typed is not on the terminal.
    assert!(!bad.terminal.contains(&wrong), "the phrase was echoed: {}", bad.terminal);

    // Now the right one.
    let good = run_on_a_terminal(
        &dir,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
        Some(("master phrase", PAPER)),
    );
    assert!(
        good.status.success(),
        "stderr: {}\nterminal: {}",
        good.stderr,
        good.terminal
    );
    assert!(!good.terminal.contains(PAPER), "echo stays off on the happy path too");
    assert!(!good.stdout.contains(PAPER), "{}", good.stdout);

    // `join` edited NO trust anchor: neither the master nor the keyset moved.
    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    assert_eq!(read_anchor(&src, MASTER_ANCHOR).unwrap().members, vec![master_pub.clone()]);
    let keyset = read_anchor(&src, CHANNEL_ANCHOR).unwrap();
    assert_eq!(keyset.head(), Some(HEAD_KEY));
    assert_eq!(
        keyset.members.len(),
        1,
        "the joining machine is ROSTER-ONLY — that is the whole point of the tier"
    );

    // The roster was EXTENDED, not replaced: m3 (put there by `armed_tree`) survives.
    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    let ids: Vec<&str> = roster.machines.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m3", "m11"], "the existing machine survives");
    assert_eq!(roster.roster_seq, 2, "the sequence advanced rather than restarting");
    assert!(
        !good.stdout.contains("the ONLY roster this master signs"),
        "join extended a roster; it must not claim to have created one: {}",
        good.stdout
    );

    assert_nothing_on_disk_holds(&dir, PAPER);
}

/// A `setup` THAT CANNOT WRITE THE ANCHOR LEAVES IT UNARMED AND SAYS TO DESTROY THE PAPER.
///
/// This is the failure window between showing the phrase and arming the anchor, and it is
/// the reason those two steps are in this order: the paper is real but references nothing,
/// which is recoverable by destroying it. The opposite order — anchor first — fails the
/// other way, leaving a committed anchor for a master nobody holds, which is not
/// recoverable at all.
#[test]
fn a_setup_that_cannot_write_the_anchor_leaves_it_unarmed_and_says_so() {
    let dir = scratch("setup-unwritable");
    let anchor_dir = dir.join("anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    let pins = anchor_dir.join("pins.rs").to_str().unwrap().to_string();
    std::fs::write(&pins, pins_fixture("")).unwrap();

    use std::os::unix::fs::PermissionsExt as _;
    // Readable, not writable: preflight can read the anchor, the atomic write cannot create
    // its temporary beside it.
    std::fs::set_permissions(&anchor_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let run = run_on_a_terminal_retyping_phrase(
        &dir,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &pins,
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    // Restore before asserting, so a failing assertion cannot leave an unremovable tree.
    std::fs::set_permissions(&anchor_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(!run.status.success(), "the run must fail: {}", run.terminal);
    assert!(
        run.stderr.contains("THE PHRASE ABOVE ARMS NOTHING"),
        "stderr: {}",
        run.stderr
    );
    assert!(run.stderr.contains("DESTROY"), "{}", run.stderr);
    assert_eq!(
        std::fs::read_to_string(&pins).unwrap(),
        pins_fixture(""),
        "THE ANCHOR IS UNARMED — this is the whole point of the ordering"
    );
    assert!(!Path::new(&p(&dir, "roster.toml")).exists());
    assert!(!Path::new(&p(&dir, "m3.key")).exists());
    // A phrase WAS shown — the failure is after it, which is what makes the instruction to
    // destroy the paper meaningful rather than boilerplate.
    assert!(
        phrase_on(&run.terminal).is_some(),
        "terminal: {}",
        run.terminal
    );
}

/// `join` on an UNARMED tree refuses before it ever reaches the prompt — the counterpart
/// of the refusal above, so neither verb can be used where the other belongs.
#[test]
fn join_on_an_unarmed_tree_refuses_before_the_prompt() {
    let dir = scratch("join-unarmed");
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).unwrap();
    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no paper master is committed"), "{err}");
    assert!(err.contains("setup"), "{err}");
    // The refusal is NOT the tty one: it happened before the prompt was ever reached.
    assert!(!err.contains("/dev/tty"), "{err}");
}

/// `setup` on an already-armed tree refuses, and never considers a supplied phrase.
#[test]
fn setup_refuses_on_an_armed_tree() {
    let dir = scratch("setup-armed-refusal");
    armed_tree(&dir);
    let phrase_file = p(&dir, "phrase.txt");
    std::fs::write(&phrase_file, PAPER).unwrap();
    let before_pins = std::fs::read(p(&dir, "pins.rs")).unwrap();

    let out = run(
        &dir,
        Some(&phrase_file),
        &[
            "setup",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );

    assert!(!out.status.success(), "setup must refuse an armed tree");
    let err = stderr_of(&out);
    assert!(err.contains("ALREADY committed"), "{err}");
    assert!(err.contains("join"), "{err}");
    assert!(!err.contains(PAPER), "the refusal must not echo a phrase: {err}");
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before_pins,
        "a refused setup writes nothing"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());
}

/// The verbs are discoverable: an unknown verb and a bare invocation both name them.
#[test]
fn the_usage_line_names_the_two_verbs_an_owner_actually_runs() {
    let dir = scratch("usage");
    let out = run(&dir, None, &[]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("setup"), "{err}");
    assert!(err.contains("join"), "{err}");

    let out = run(&dir, None, &["nonsense"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("setup"), "{}", stderr_of(&out));

    // `--id` is required, and the usage names the verb that was actually run.
    let out = run(&dir, None, &["setup"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("usage: atpkg-keys setup --id"),
        "{}",
        stderr_of(&out)
    );
}
