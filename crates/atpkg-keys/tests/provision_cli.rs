// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Subprocess CLI tests for `atpkg-keys` provisioning with and without a controlling terminal.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use aterm_update_core::roster::{Roster, verify_roster};
use atpkg_keys::master::parse_master;
use atpkg_keys::pins_edit::{MASTER_ANCHOR, read_anchor};
use atpkg_keys::roster_ops::{add, empty};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::pty::Pty;

use aterm_spec::wait::MUST_HAPPEN;

/// An obviously synthetic master.
const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

/// A realistic machine key for a roster entry this test writes by hand.
const HEAD_KEY: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";

/// 2026-08-04T00:00:00Z.
const NOW: u64 = 1_785_801_600;

/// A `pins.rs` master anchor.
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
    s
}

/// A private scratch tree for one test, removed when the test ends however it ends.
fn scratch(name: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-keys-cli-{name}-"))
        .tempdir()
        .expect("scratch dir")
}

fn p(dir: &Path, name: &str) -> String {
    dir.join(name).to_str().expect("utf-8 path").to_string()
}

/// The environment every child gets ([`settle_before_fork`]).
fn base_command(dir: &Path, args: &[&str]) -> Command {
    settle_before_fork();
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

/// Settle libSystem's libnotify state before this process's first fork.
fn settle_before_fork() {
    #[cfg(target_vendor = "apple")]
    {
        static SETTLED: std::sync::Once = std::sync::Once::new();
        SETTLED.call_once(|| {
            unsafe extern "C" {
                /// `notify.h`: whether `token` is registered.
                fn notify_is_valid_token(token: std::ffi::c_int) -> bool;
            }
            // SAFETY: takes an int and reads libnotify's own table; no pointer crosses.
            let _ = unsafe { notify_is_valid_token(0) };
        });
    }
}

/// Spawn the real binary with no controlling terminal and `stdin` taken from `stdin_path`.
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
    // SAFETY: `setsid` is async-signal-safe and touches no state shared with the parent; it
    // runs post-fork, pre-exec. Its result is ignored: failing because the child already
    // leads a session is harmless, and the assertions do not depend on how `/dev/tty` became
    // unavailable.
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

/// Write an armed `pins.rs` plus a roster the synthetic master signed.
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

/// What a pty-driven run produced.
struct TtyRun {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    terminal: String,
}

/// Run the binary on a real terminal.
fn run_on_a_terminal(dir: &Path, args: &[&str], reply: Option<(&str, &str)>) -> TtyRun {
    let mut pty = Pty::open();
    let mut child = pty.spawn(base_command(dir, args));
    let mut terminal = String::new();

    let deadline = std::time::Instant::now() + MUST_HAPPEN;
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
    pty.drain(&mut terminal);
    let out = child
        .wait_with_output()
        .expect("collect the child's output");
    TtyRun {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        terminal,
    }
}

/// The phrase line currently on the terminal.
fn phrase_on(terminal: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    terminal.lines().map(str::trim).find_map(|l| {
        (!l.is_empty()
            && l.bytes().all(|b| b == b' ' || ALPHABET.contains(&b))
            && l.bytes().filter(|b| *b != b' ').count() == 52)
            .then(|| l.to_string())
    })
}

/// Like [`run_on_a_terminal`], but types back the phrase on the terminal at `setup`'s retype
/// prompt.
fn run_on_a_terminal_retyping_phrase(dir: &Path, args: &[&str]) -> TtyRun {
    let mut pty = Pty::open();
    let mut child = pty.spawn(base_command(dir, args));
    let mut terminal = String::new();

    let deadline = std::time::Instant::now() + MUST_HAPPEN;
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
    let out = child
        .wait_with_output()
        .expect("collect the child's output");
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

#[test]
fn join_refuses_the_phrase_from_env_stdin_or_a_file() {
    let tmp = scratch("join-leak-vectors");
    let dir = tmp.path().to_path_buf();
    armed_tree(&dir);
    let phrase_file = p(&dir, "phrase.txt");
    std::fs::write(&phrase_file, PAPER).expect("the file the operator must not be able to use");

    let before_pins = std::fs::read(p(&dir, "pins.rs")).unwrap();
    let before_roster = std::fs::read(p(&dir, "roster.toml")).unwrap();

    let out = run(
        &dir,
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

    assert!(
        !Path::new(&p(&dir, "m11.key")).exists(),
        "a refused join must mint no key"
    );
    assert_eq!(std::fs::read(p(&dir, "pins.rs")).unwrap(), before_pins);
    assert_eq!(
        std::fs::read(p(&dir, "roster.toml")).unwrap(),
        before_roster
    );

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

#[test]
fn a_flag_that_would_carry_the_master_is_refused_and_calls_it_compromised() {
    let tmp = scratch("master-flag");
    let dir = tmp.path().to_path_buf();
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
        assert!(
            !err.contains(PAPER),
            "the refusal must not echo the value: {err}"
        );
    }
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before,
        "a refused run writes nothing"
    );

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
    assert!(
        !stderr_of(&out).contains("COMPROMISED"),
        "{}",
        stderr_of(&out)
    );
    assert!(stderr_of(&out).contains("/dev/tty"), "{}", stderr_of(&out));
}

#[test]
fn setup_without_a_terminal_refuses_and_writes_nothing() {
    let tmp = scratch("setup-headless");
    let dir = tmp.path().to_path_buf();
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

    assert!(
        !out.status.success(),
        "setup must refuse without a terminal"
    );
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

    let stdout = stdout_of(&out);
    assert!(
        phrase_on(&stdout).is_none(),
        "no phrase-shaped line anywhere on stdout: {stdout}"
    );
}

#[test]
fn an_unread_flag_spelling_is_refused_rather_than_silently_replaced_by_a_default() {
    let tmp = scratch("flag-equals");
    let dir = tmp.path().to_path_buf();
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

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "oops",
            "--pins",
            &named,
            "--roster",
            &p(&dir, "r.toml"),
        ],
    );
    assert!(
        !stderr_of(&out).contains("--name=value"),
        "{}",
        stderr_of(&out)
    );
    assert!(stderr_of(&out).contains("/dev/tty"), "{}", stderr_of(&out));
}

#[test]
fn unknown_and_malformed_arguments_are_refused_by_name() {
    let tmp = scratch("bad-args");
    let dir = tmp.path().to_path_buf();
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).unwrap();

    let out = run(
        &dir,
        None,
        &["setup", "--id", "m3", "--pinz", &p(&dir, "pins.rs")],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("unknown flag '--pinz'"), "{err}");
    assert!(
        err.contains("--pins"),
        "the message lists what IS accepted: {err}"
    );

    let out = run(
        &dir,
        None,
        &["setup", "--id", "--pins", &p(&dir, "pins.rs")],
    );
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

    let out = run(
        &dir,
        None,
        &["setup", "--id", "m3", "--pins", &p(&dir, "pins.rs")],
    );
    let err = stderr_of(&out);
    assert!(!err.contains("unknown flag"), "{err}");
    assert!(!err.contains("unexpected argument"), "{err}");
}

#[test]
fn machine_revoke_with_no_roster_refuses_before_asking_for_the_phrase() {
    let tmp = scratch("revoke-no-roster");
    let dir = tmp.path().to_path_buf();
    let roster = p(&dir, "dist/aterm-machines.toml");
    let out = run(
        &dir,
        None,
        &["machine-revoke", "--id", "m3", "--roster", &roster],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no roster at "), "{err}");
    assert!(
        !err.contains("/dev/tty"),
        "refused before the phrase is asked for: {err}"
    );
    assert!(
        !dir.join("dist").exists(),
        "no roster lock or directory is created: {err}"
    );

    armed_tree(&dir);
    let out = run(
        &dir,
        None,
        &[
            "machine-revoke",
            "--id",
            "m3",
            "--roster",
            &p(&dir, "roster.toml"),
        ],
    );
    let err = stderr_of(&out);
    assert!(!err.contains("no roster at "), "{err}");
    assert!(err.contains("/dev/tty"), "{err}");
}

#[test]
fn join_without_a_roster_refuses_instead_of_forking_one() {
    let tmp = scratch("join-no-roster");
    let dir = tmp.path().to_path_buf();
    let master_pub = armed_tree(&dir);
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
    assert!(
        err.contains("COPY"),
        "the refusal must say how to fix it: {err}"
    );
    assert!(
        err.contains("will not start a second roster"),
        "and why it will not do it for you: {err}"
    );
    assert!(!err.contains("/dev/tty"), "{err}");
    assert!(
        !Path::new(&p(&dir, "roster.toml")).exists(),
        "no roster was created"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());

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
    assert!(
        err.contains("/dev/tty"),
        "it got as far as the prompt: {err}"
    );
    let _ = master_pub;
}

#[test]
fn setup_over_a_real_terminal_shows_the_phrase_there_and_never_on_stdout() {
    let tmp = scratch("setup-tty");
    let dir = tmp.path().to_path_buf();
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
        ],
    );
    assert!(
        run.status.success(),
        "setup must succeed on a terminal.\nstderr: {}\nterminal: {}",
        run.stderr,
        run.terminal
    );

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

    assert!(!run.stdout.contains(&phrase), "stdout: {}", run.stdout);
    assert!(!run.stderr.contains(&phrase), "stderr: {}", run.stderr);
    for start in 0..48 {
        assert!(!run.stdout.contains(&phrase[start..start + 16]));
        assert!(!run.stderr.contains(&phrase[start..start + 16]));
    }

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

    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    assert_eq!(roster.machines.len(), 1);
    assert_eq!(roster.machines[0].id, "m3");
    let machine_pub = roster.machines[0].pubkey.clone();
    assert!(
        !src.contains(&machine_pub),
        "the minted key must appear nowhere in the anchor file: {src}"
    );

    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(p(&dir, "m3.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let record = std::fs::read_to_string(dir.join(".aterm/machine.toml")).unwrap();
    assert!(record.contains("id = \"m3\""), "{record}");

    assert!(
        run.stdout.contains("a commit makes it durable"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("the ONLY roster this master signs"),
        "{}",
        run.stdout
    );

    let checked = assert_nothing_on_disk_holds(&dir, &phrase);
    assert!(
        checked >= 5,
        "the run must have written five files; saw {checked}"
    );

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
    assert!(
        again.stderr.contains("ALREADY committed"),
        "{}",
        again.stderr
    );
    assert_eq!(
        std::fs::read_to_string(p(&dir, "pins.rs")).unwrap(),
        src,
        "the refused re-run left the anchor exactly as it was"
    );
}

#[test]
fn a_mistyped_retype_reshows_the_phrase_and_a_correct_one_arms() {
    const RETYPE_PROMPT: &str = "retype the phrase FROM YOUR PAPER (echo off; spaces ignored; Ctrl-C aborts, nothing is armed): ";
    let tmp = scratch("setup-retype-mismatch");
    let dir = tmp.path().to_path_buf();
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");

    let mut pty = Pty::open();
    let mut child = pty.spawn(base_command(
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
    ));
    let mut terminal = String::new();
    let deadline = std::time::Instant::now() + MUST_HAPPEN;
    let mut typed_wrong = false;
    let mut typed_right = false;
    loop {
        pty.drain(&mut terminal);
        if !typed_wrong && terminal.contains(RETYPE_PROMPT) {
            let wrong = "7".repeat(51) + "0";
            pty.type_line(&wrong);
            typed_wrong = true;
        }
        if typed_wrong
            && !typed_right
            && let Some((_, retry)) = terminal.split_once("NO MATCH")
            && retry.contains(RETYPE_PROMPT)
        {
            let phrase = phrase_on(retry).expect("the phrase is re-shown after NO MATCH");
            let unarmed = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
            assert!(
                read_anchor(&unarmed, MASTER_ANCHOR)
                    .unwrap()
                    .members
                    .is_empty(),
                "a wrong retype must leave the master anchor unarmed"
            );
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
    let out = child
        .wait_with_output()
        .expect("collect the child's output");

    assert!(
        out.status.success(),
        "a corrected retype must arm: {terminal}"
    );
    assert!(terminal.contains("NO MATCH"), "{terminal}");
    let phrase = phrase_on(&terminal).expect("phrase shown");
    assert!(
        terminal.matches(phrase.as_str()).count() >= 2,
        "the phrase is re-shown after a mismatch: {terminal}"
    );
    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    let master_pub = parse_master(&phrase)
        .expect("parses")
        .seed()
        .pubkey_b64()
        .unwrap();
    assert_eq!(
        read_anchor(&src, MASTER_ANCHOR).unwrap().members,
        vec![master_pub]
    );
}

#[test]
fn join_over_a_real_terminal_reads_the_phrase_and_extends_the_roster() {
    let tmp = scratch("join-tty");
    let dir = tmp.path().to_path_buf();
    let master_pub = armed_tree(&dir);
    let before_pins = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();

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
    assert!(
        !bad.terminal.contains(&wrong),
        "the phrase was echoed: {}",
        bad.terminal
    );

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
    assert!(
        !good.terminal.contains(PAPER),
        "echo stays off on the happy path too"
    );
    assert!(!good.stdout.contains(PAPER), "{}", good.stdout);

    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    assert_eq!(
        read_anchor(&src, MASTER_ANCHOR).unwrap().members,
        vec![master_pub.clone()]
    );

    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    let ids: Vec<&str> = roster.machines.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m3", "m11"], "the existing machine survives");
    assert_eq!(
        roster.roster_seq, 2,
        "the sequence advanced rather than restarting"
    );
    assert!(
        !good.stdout.contains("the ONLY roster this master signs"),
        "join extended a roster; it must not claim to have created one: {}",
        good.stdout
    );

    assert_nothing_on_disk_holds(&dir, PAPER);
}

#[test]
fn a_setup_that_cannot_write_the_anchor_leaves_it_unarmed_and_says_so() {
    let tmp = scratch("setup-unwritable");
    let dir = tmp.path().to_path_buf();
    let anchor_dir = dir.join("anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    let pins = anchor_dir.join("pins.rs").to_str().unwrap().to_string();
    std::fs::write(&pins, pins_fixture("")).unwrap();

    use std::os::unix::fs::PermissionsExt as _;
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
    assert!(
        phrase_on(&run.terminal).is_some(),
        "terminal: {}",
        run.terminal
    );
}

#[test]
fn join_on_an_unarmed_tree_refuses_before_the_prompt() {
    let tmp = scratch("join-unarmed");
    let dir = tmp.path().to_path_buf();
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
    assert!(!err.contains("/dev/tty"), "{err}");
}

#[test]
fn setup_refuses_on_an_armed_tree() {
    let tmp = scratch("setup-armed-refusal");
    let dir = tmp.path().to_path_buf();
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
    assert!(
        !err.contains(PAPER),
        "the refusal must not echo a phrase: {err}"
    );
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before_pins,
        "a refused setup writes nothing"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());
}
