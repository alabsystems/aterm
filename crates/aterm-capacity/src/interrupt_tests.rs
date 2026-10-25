// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An interrupt ends a wait for capacity (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! WAIT-INTERRUPT): a stand-in in a process of its own waits in a scratch ledger behind this
//! test's claim, on an injected machine, and is sent SIGTERM or SIGINT.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::floor::GIB;
use crate::{Admit, Error, Fixed, Ledger, Need, Pace, Reading, Volume, admit_with};

/// Set, the test binary is [`standin_waits_for_capacity`], its ledger under this directory.
const STANDIN: &str = "ATERM_CAPACITY_INTERRUPT_STANDIN";
/// What the stand-in waits for: `mem`, `disk` or `quiet` ([`Kind`]).
const STANDIN_KIND: &str = "ATERM_CAPACITY_INTERRUPT_KIND";
/// Set, the stand-in puts SIGINT back to its default action first, as a foreground process
/// has it.
const STANDIN_SIGINT: &str = "ATERM_CAPACITY_INTERRUPT_SIGINT";
/// Set, the stand-in first catches SIGINT and SIGTERM in a handler of its own that notes them,
/// as the release steps and the lab do.
const STANDIN_TOOL: &str = "ATERM_CAPACITY_INTERRUPT_TOOL";

/// What a wait is short of.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Mem,
    Disk,
    Quiet,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Mem => "mem",
            Kind::Disk => "disk",
            Kind::Quiet => "quiet",
        }
    }

    fn of(word: &str) -> Kind {
        match word {
            "disk" => Kind::Disk,
            "quiet" => Kind::Quiet,
            _ => Kind::Mem,
        }
    }

    /// The machine both sides read: 48 GiB with 40 available, 14 idle cores, a volume with 90
    /// GiB above its floor.
    fn machine() -> Fixed {
        let mut r = Reading::of_gib(48, 40, 14);
        r.volume = Some(Volume {
            mount: PathBuf::from("/vol"),
            total: 200 * GIB,
            free: 100 * GIB,
            floor: 10 * GIB,
            room: 2 * GIB,
        });
        Fixed::new(r)
    }

    /// The claim this test holds, which leaves the stand-in's short by 10 GiB (or behind it on
    /// the quiet machine).
    fn held(self) -> Need {
        match self {
            Kind::Mem => Need::new("holder").mem_gib(30),
            Kind::Disk => Need::new("holder").disk(80 * GIB, "/vol"),
            Kind::Quiet => Need::new("holder").quiet(28.0),
        }
    }

    /// The stand-in's need.
    fn wants(self) -> Need {
        match self {
            Kind::Mem => Need::new("stand-in").mem_gib(20),
            Kind::Disk => Need::new("stand-in").disk(20 * GIB, "/vol"),
            Kind::Quiet => Need::new("stand-in").quiet(28.0),
        }
    }

    /// What its wait line names.
    fn short(self) -> &'static str {
        match self {
            Kind::Mem => "memory short by 10 GiB",
            Kind::Disk => "disk short by 10 GiB",
            Kind::Quiet => "the quiet machine",
        }
    }
}

/// The signal the stand-in's own handler noted.
static TOOL_SAW: AtomicI32 = AtomicI32::new(0);

extern "C" fn tool_handler(sig: libc::c_int) {
    TOOL_SAW.store(sig, Ordering::SeqCst);
}

/// Run by [`an_interrupt_ends_each_kind_of_wait`] in a process of its own: waits for its
/// [`Kind`] behind the test's claim, looking every two minutes (twice the test's hang detector,
/// so only a nap the interrupt ends ends in time), writing `waiting` at its first wait line,
/// and exits 3, a tool's NOT RUN, naming the signal and the shortfall.
/// With [`STANDIN_TOOL`], its own handler is back once the wait has ended and never saw the
/// signal the wait took.
#[test]
fn standin_waits_for_capacity() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    let kind = Kind::of(&std::env::var(STANDIN_KIND).unwrap_or_default());
    let tool = tool_handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: SIG_DFL is a disposition; `tool_handler` is an `extern "C" fn(c_int)` that lives
    // for the whole program and stores one atomic, which is async-signal-safe.
    unsafe {
        if std::env::var_os(STANDIN_SIGINT).is_some() {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        if std::env::var_os(STANDIN_TOOL).is_some() {
            libc::signal(libc::SIGINT, tool);
            libc::signal(libc::SIGTERM, tool);
        }
    }
    let waiting = dir.join("waiting");
    let got = admit_with(
        &Ledger::at(dir.join("ledger")),
        &Kind::machine(),
        kind.wants(),
        Admit::Wait,
        Pace {
            poll: MUST_HAPPEN * 2,
            ..Pace::default()
        },
        &mut |line| {
            if !waiting.exists() {
                // Written aside and renamed in, so the test never reads the file between its
                // creation and its line.
                let part = waiting.with_extension("part");
                std::fs::write(&part, line).expect("said waiting");
                std::fs::rename(&part, &waiting).expect("said waiting");
            }
        },
    );
    match got {
        Err(Error::Stopped {
            signal: Some(sig),
            short: Some(short),
        }) => {
            if std::env::var_os(STANDIN_TOOL).is_some() {
                // SAFETY: a null new action only reads the current one into `now`, a zeroed
                // `sigaction` (a valid value) that outlives the call.
                let now = unsafe {
                    let mut now: libc::sigaction = std::mem::zeroed();
                    libc::sigaction(libc::SIGTERM, std::ptr::null(), &raw mut now);
                    now
                };
                assert_eq!(now.sa_sigaction, tool, "the tool's handler is back");
                assert_eq!(
                    TOOL_SAW.load(Ordering::SeqCst),
                    0,
                    "the wait took the signal"
                );
            }
            println!(
                "stopped by {}: {short}",
                aterm_toolchain::interrupt::name(sig)
            );
            std::process::exit(3);
        }
        other => panic!("the wait ended otherwise: {other:?}"),
    }
}

/// What a signalled stand-in did: its exit, what it printed, how long after the signal it
/// ended, and the files left in the ledger.
struct Ended {
    status: std::process::ExitStatus,
    out: String,
    took: Duration,
    left: Vec<String>,
}

/// A stand-in, killed when it is dropped still running (a test that failed before it ended),
/// so none outlives its test.
struct Standin(aterm_uds::spawnfd::Child);

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Hold `kind`'s claim, start [`standin_waits_for_capacity`] behind it with `env` set, and once
/// it waits send it `sig`.
fn interrupt(kind: Kind, env: &[&str], sig: libc::c_int) -> Ended {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("cap-int-{}-", kind.word()))
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let held = admit_with(
        &ledger,
        &Kind::machine(),
        kind.held(),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the holder's claim");
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    for key in env {
        cmd.env(key, "1");
    }
    let mut child = Standin(
        cmd.args([
            "--exact",
            "interrupt_tests::standin_waits_for_capacity",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(STANDIN, dir.path())
        .env(STANDIN_KIND, kind.word())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in"),
    );
    let line = until_within(MUST_HAPPEN, "the stand-in's first wait line", || {
        std::fs::read_to_string(dir.path().join("waiting")).ok()
    });
    assert!(line.contains(kind.short()), "{line}");
    let pid = i32::try_from(child.0.id()).expect("a pid");
    let sent = Instant::now();
    // SAFETY: `kill` takes two integers; the pid is this test's own child.
    assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "signalled the stand-in");
    let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
        child.0.try_wait().ok().flatten()
    });
    let took = sent.elapsed();
    let mut out = String::new();
    if let Some(mut o) = child.0.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    let left = ledger
        .dir()
        .read_dir()
        .expect("the ledger")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
        .collect();
    drop(held);
    Ended {
        status,
        out,
        took,
        left,
    }
}

/// Each kind of wait (memory, disk, the quiet machine), its next look two minutes off, ends on
/// SIGTERM as NOT RUN well inside the one-minute hang detector (the time is reported), naming
/// the signal and what it was short of, and leaves only the holder's claim in the ledger.
/// SIGINT does the same where it is not ignored, and a tool's own handler is set aside for the
/// wait and put back after. Without the wait's watch the stand-in dies of the signal (no exit 3)
/// and leaves its `.wait` file, or, with its own handler, waits on past the hang detector.
#[test]
fn an_interrupt_ends_each_kind_of_wait() {
    for (kind, env, sig, word) in [
        (Kind::Mem, &[][..], libc::SIGTERM, "SIGTERM"),
        (Kind::Disk, &[][..], libc::SIGTERM, "SIGTERM"),
        (Kind::Quiet, &[][..], libc::SIGTERM, "SIGTERM"),
        (Kind::Mem, &[STANDIN_SIGINT][..], libc::SIGINT, "SIGINT"),
        (Kind::Disk, &[STANDIN_TOOL][..], libc::SIGTERM, "SIGTERM"),
        (Kind::Quiet, &[STANDIN_TOOL][..], libc::SIGINT, "SIGINT"),
    ] {
        let ended = interrupt(kind, env, sig);
        let what = format!("{} {env:?} {word}", kind.word());
        assert_eq!(
            ended.status.code(),
            Some(3),
            "{what}: {} {}",
            ended.status,
            ended.out
        );
        assert!(
            ended.out.contains(&format!("stopped by {word}: ")) && ended.out.contains(kind.short()),
            "{what}: {}",
            ended.out
        );
        assert_eq!(
            ended.left.len(),
            1,
            "{what}: only the holder's claim: {:?}",
            ended.left
        );
        assert!(ended.left[0].ends_with(".claim"), "{:?}", ended.left);
        eprintln!("{what} ended the wait in {:?}", ended.took);
    }
}
