// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HANDBACK LANE SENDS EACH KEY TO THE HOLDER IT IS MEANT FOR.
//!
//! `tools/test-foreground-handback.sh` (the merge contract's "foreground
//! handback" stage) drives a real zsh's job control. On 2026-09-27 it failed
//! 4-6 of its rows on a loaded machine (load 57-69) with the binary of that day
//! AND with the binary of 2026-09-26 that had passed the same stage: the rows
//! were the lane's, not aterm's. It guessed who held the terminal from a quiet
//! screen (`await idle 500`) and sent Ctrl-C; zsh's `fg` prints the job line
//! and only then `tcsetpgrp`s the job, so the Ctrl-C reached zsh's own group,
//! the job lived on holding the terminal (measured: `sleep 30` S+, zsh's
//! tpgid its group), nothing was handed back because nothing died, and the
//! next row's command line went into the job's cooked tty, where its echo —
//! `parent-back` spelled out in the typed line — satisfied the row's
//! `await match`.
//!
//! A live run can only sample that race. This drives the REAL lane against
//! `tests/fixtures/handback-fake-aterm.sh`, which plays the shell's job
//! control under the adversarial schedule — a handover of the terminal is
//! visible only to a driver that READS the holder (`who`'s `fgpgid=`), never
//! to one that waits on a quiet screen — and records every key that reaches
//! the wrong holder and every `await match` its own typed line satisfies.
//! Before the fix the lane made 8 such sends and waits (6 control keys to the
//! shell, the `inside` and `parent-back` echoes); after it, none.
//!
//! The live-holder row releases its holder with a line on the holder's stdin
//! once it has read the holder's modes (2026-09-27: a 2 s self-kill timer was
//! outlasted at load ~50, and the row read a dead holder); the fake counts
//! that release as a key the JOB must hold the terminal for, and its log line
//! is one of the non-vacuity checks.
//!
//! The forced-miss row (the lane SIGSTOPs its own instance across a one-shot
//! `?1000h`, leaving a shell-owned mouse=normal when the miss takes) runs LAST
//! in the zsh session, followed only by the recovery row (`armed-4`, a job that
//! re-arms mouse reporting and dies). From 2026-09-27 until the fix it ran
//! before the Ctrl-Z, pipeline and live-holder rows, so each of those started
//! from the stale mode and none covered its scenario from a clean state. The
//! fake logs every `await match`, so the order of the rows is pinned here too.
//!
//! NEGATIVE CONTROL: a driver in the lane's old shape (Ctrl-C right after the
//! command line; a wait on a marker its own line spells out) is caught by the
//! same fake, so the lane's clean record is not a fake that catches nothing.
//!
//! The lane's own rows FAIL here — the fake answers `modes` with a dummy row —
//! and that is not read: the live lane is what judges aterm's handback.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-verify sits two levels under the root")
        .to_path_buf()
}

struct Fake {
    root: PathBuf,
    bin: PathBuf,
    state: PathBuf,
}

impl Fake {
    fn new(tag: &str) -> Self {
        let root = aterm_verify::mktemp_dir(tag).expect("scratch root");
        let bin = root.join("aterm");
        fs::write(&bin, include_str!("fixtures/handback-fake-aterm.sh")).expect("write the fake");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("chmod the fake");
        let state = root.join("state");
        fs::create_dir_all(&state).expect("state dir");
        Self { root, bin, state }
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.state.join(name)).unwrap_or_default()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn have_python3() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("pass")
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn the_handback_lane_sends_every_key_to_the_holder_it_means_and_never_waits_on_its_own_echo() {
    if !have_python3() {
        // The lane itself answers NOT RUN without python3; so does this.
        eprintln!("NOT RUN: python3 is required by the lane and by the fake's socket");
        return;
    }
    let fake = Fake::new("atv-fh-keys");
    let lane = workspace_root().join("tools/test-foreground-handback.sh");
    let out = Command::new("bash")
        .arg(&lane)
        .arg("--binary")
        .arg(&fake.bin)
        .arg("--dir")
        .arg(fake.root.join("lane"))
        .env("FAKE_ATERM_STATE", &fake.state)
        .output()
        .expect("the lane runs");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Not a NOT RUN, and not stopped at a boot: the lane drove the fake through.
    assert_ne!(out.status.code(), Some(2), "{transcript}");
    let violations = fake.read("violations");
    assert!(
        violations.is_empty(),
        "the lane sent keys to the wrong holder or waited on its own echo:\n{violations}\n\
         lane transcript:\n{transcript}"
    );
    let log = fake.read("log");
    for want in [
        "typed fg",
        "typed bg",
        "await parent-back",
        "released the holder",
    ] {
        assert!(
            log.lines().any(|l| l == want),
            "the lane reached `{want}` (non-vacuity): {log}\n{transcript}"
        );
    }
    // The rows in the order the lane owes: every pre-existing zsh row, then the
    // forced miss, then the one recovery row; nothing of zsh's after it.
    let order = [
        "await armed-1",
        "await armed-2",
        "await armed-3",
        "await parent-back",
        "await missed-done",
        "await armed-4",
    ];
    let at: Vec<usize> = order
        .iter()
        .map(|want| {
            log.lines()
                .position(|l| l == *want)
                .unwrap_or_else(|| panic!("the lane never reached `{want}`: {log}\n{transcript}"))
        })
        .collect();
    assert!(
        at.windows(2).all(|w| w[0] < w[1]),
        "the zsh rows ran out of order (want {order:?}, at log lines {at:?}): the \
         forced miss must come after every pre-existing row and before only the \
         recovery row:\n{log}\n{transcript}"
    );
}

#[test]
fn the_fake_catches_a_driver_that_guesses_the_holder_or_waits_on_its_own_echo() {
    if !have_python3() {
        eprintln!("NOT RUN: python3 is required by the fake's socket");
        return;
    }
    let fake = Fake::new("atv-fh-neg");
    // The lane's old shape: a job's command line, a quiet-screen guess, Ctrl-C
    // (which the shell absorbs, so the job goes on to take the terminal); then
    // a command line — into the job — whose echo spells the marker it waits for.
    let script = r#"
set -u
F=$1 S=$2/s.sock
"$F" --headless --control-sock "$S" </dev/null >/dev/null 2>&1 &
P=$!
trap 'kill $P 2>/dev/null' EXIT
for i in $(seq 1 80); do [ -S "$S" ] && break; sleep 0.1; done
c() { "$F" ctl --sock "$S" --timeout 20 "$@"; }
printf '%s' "sh -c 'sleep 30'" | c send --stdin
c key enter
c await idle 500
printf '\003' | c send --stdin
c who >/dev/null
c who >/dev/null
printf '%s' "echo marker-1" | c send --stdin
c key enter
c await match marker-1
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .arg("driver")
        .arg(&fake.bin)
        .arg(&fake.root)
        .env("FAKE_ATERM_STATE", &fake.state)
        .output()
        .expect("the driver runs");
    assert!(out.status.success(), "{out:?}");
    let violations = fake.read("violations");
    for want in [
        "VIOLATION: Ctrl-C sent while the shell held the terminal",
        "VIOLATION: a command line typed while a job held the terminal: echo marker-1",
        "VIOLATION: await match /marker-1/ is satisfied by the echo of a typed line: echo marker-1",
    ] {
        assert!(
            violations.lines().any(|l| l == want),
            "missing `{want}` in:\n{violations}"
        );
    }
}
