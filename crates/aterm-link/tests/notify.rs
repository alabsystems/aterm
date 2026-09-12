// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A10 — `notify`: the push to a human, and the failure it chooses.**
//!
//! The rung's claim is one sentence: `notify --on attention --exec <cmd>` runs
//! the command exactly once per matching offset ACROSS A RESTART, and never more
//! than the rate limit. Everything here is the real binary against a real broker
//! on the sealed wire, with a real `/bin/sh` child doing the notifying.
//!
//! ## No sleeps as synchronisation
//!
//! `--once` catches the bus up to its head and exits, so most of this file is
//! run-assert-run with nothing to race. The one test that follows the bus live
//! waits on the DURABLE JOURNAL — the file written after the command returns —
//! and never on the command's own output, because the window between those two
//! writes is precisely the in-doubt window another test crashes inside on
//! purpose. Waiting on the wrong one of the two would be a coin flip.

#![cfg(unix)]

mod harness;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use harness::{until, Fleet, FLEET};

/// A `notify` run's captured output.
struct Run {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// One notifier under test: a state dir, a cap, and the command it runs.
struct Watch {
    state: PathBuf,
    cap: PathBuf,
    log: PathBuf,
    addr: String,
    key_file: PathBuf,
}

impl Watch {
    fn new(fleet: &Fleet, tag: &str, grants: &[String]) -> Self {
        let state = fleet.tmp.join(format!("{tag}-state"));
        std::fs::create_dir_all(&state).expect("state dir");
        Self {
            cap: fleet.cap_file(tag, grants),
            log: fleet.tmp.join(format!("{tag}.log")),
            state,
            addr: fleet.addr.clone(),
            key_file: fleet.key_file.clone(),
        }
    }

    /// A notifier watching `attention` with a read cap on the presence face.
    fn attention(fleet: &Fleet, tag: &str) -> Self {
        Self::new(fleet, tag, &[format!("ro:/f/{FLEET}/pub/>")])
    }

    /// The `--exec` line every test uses unless it says otherwise: one line per
    /// notification, carrying the three facts the assertions read.
    fn appender(&self) -> String {
        format!(
            "printf '%s dup=%s sup=%s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_DUP\" \
             \"$ATERM_NOTIFY_SUPPRESSED\" >> {}",
            self.log.display()
        )
    }

    fn argv(&self, on: &str, exec: &str, extra: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = ["notify", "--fleet", FLEET, "--broker", &self.addr]
            .into_iter()
            .map(str::to_string)
            .collect();
        // The fixture's transport, not a spelled one — it serves plaintext
        // loopback without `--features sealed`, and a child handed `--key-file`
        // against it is refused by its own `transport::connect`.
        v.extend(harness::fleet_child_flags(&self.key_file));
        let rest: Vec<String> = [
            "--cap-file",
            self.cap.to_string_lossy().as_ref(),
            "--state",
            self.state.to_string_lossy().as_ref(),
            "--on",
            on,
            "--exec",
            exec,
            "--since",
            "start",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        v.extend(rest);
        v.extend(extra.iter().map(|s| (*s).to_string()));
        v
    }

    /// Run the real binary to completion in `--once` mode.
    fn once(&self, on: &str, extra: &[&str]) -> Run {
        self.once_with(on, &self.appender(), extra, &[])
    }

    fn once_with(&self, on: &str, exec: &str, extra: &[&str], env: &[(&str, &str)]) -> Run {
        let mut args = self.argv(on, exec, extra);
        args.push("--once".to_string());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm-link"));
        cmd.args(&args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run aterm-link notify");
        Run {
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Spawn the binary in FOLLOW mode, killed on every exit path.
    fn follow(&self, on: &str) -> Follower {
        let out = std::fs::File::create(self.state.join("follow.out")).expect("stdio");
        let err = out.try_clone().expect("stdio clone");
        Follower(Some(
            Command::new(env!("CARGO_BIN_EXE_aterm-link"))
                .args(self.argv(on, &self.appender(), &[]))
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .expect("spawn aterm-link notify"),
        ))
    }

    /// The lines the notification command has appended.
    fn fired(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The persisted count of rate-dropped notifications the human has not been
    /// told about yet.
    fn suppressed(&self) -> u64 {
        std::fs::read_to_string(self.state.join("notify/suppressed"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    /// The durable journal, one verdict per line.
    fn journal(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("notify/journal"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// A follow-mode notifier that is killed when the test leaves, panic included.
struct Follower(Option<Child>);

impl Follower {
    fn kill(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Publish one presence row carrying an `attention=`, and answer its offset.
fn raise_attention(fleet: &Fleet, who: &str, seq: u64, text: &str) -> u64 {
    let subject = format!("/f/{FLEET}/pub/n-alpha/{who}/presence");
    let body = format!(
        "v=1 t=1 state=live inc=1 attention={}",
        aterm_link::pct::encode(text)
    );
    fleet
        .god()
        .publish(9_100, seq, &subject, body.as_bytes())
        .expect("publish a presence row")
        .0
}

/// The offset a fired line names.
fn offset_of(line: &str) -> u64 {
    line.split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("a fired line starts with its offset: {line}"))
}

/// **THE RUNG.** One command per matching offset, and the second run of the
/// notifier over the same records runs nothing at all.
///
/// The restart here is the real one: a whole process exit and a fresh
/// `aterm-link notify` against the same state dir, the same broker and the same
/// records. Nothing is remembered in memory, and the broker's cursor is not what
/// is being trusted — the journal in the state dir is.
#[test]
fn the_command_runs_exactly_once_per_matching_offset_across_a_restart() {
    let fleet = Fleet::boot("notify-once");
    let w = Watch::attention(&fleet, "once");
    let first = raise_attention(&fleet, "s-one", 1, "needs a key");

    let run = w.once("attention", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(w.fired(), vec![format!("{first} dup=0 sup=0")]);
    assert!(
        run.stdout.contains("fired=1 dup=0 skipped=0 dropped=0"),
        "{}",
        run.stdout
    );

    // THE RESTART. Same records, same state dir, nothing new on the bus.
    let again = w.once("attention", &[]);
    assert!(again.status.success(), "{}", again.stderr);
    assert_eq!(
        w.fired().len(),
        1,
        "a restart must not re-run the command for an offset it already fired"
    );
    assert!(
        again.stdout.contains("fired=0 dup=0"),
        "the second run runs nothing: {}",
        again.stdout
    );

    // AND THE JOURNAL IS WHAT DOES IT, not the cursor. Take the cursor away —
    // the file a lost disk, a copied state dir or a hand-edited `--since` can
    // each produce — and re-scan the same records from zero. The verdicts are
    // still there, so the command still does not run.
    std::fs::remove_file(w.state.join("notify/cur/attention")).expect("drop the cursor");
    let rescan = w.once("attention", &[]);
    assert!(rescan.status.success(), "{}", rescan.stderr);
    assert_eq!(
        w.fired().len(),
        1,
        "a re-scan of an offset with a verdict must not re-run the command"
    );
    assert!(
        rescan.stdout.contains("fired=0 dup=0 skipped=1"),
        "the re-scan skips on the journal's verdict: {}",
        rescan.stdout
    );

    // A NEW record is a new notification — the dedup is by offset, not a latch.
    let second = raise_attention(&fleet, "s-one", 2, "still needs a key");
    assert_ne!(second, first);
    let third = w.once("attention", &[]);
    assert!(third.status.success(), "{}", third.stderr);
    assert_eq!(
        w.fired(),
        vec![
            format!("{first} dup=0 sup=0"),
            format!("{second} dup=0 sup=0")
        ],
        "the same attention text at a new offset is a new notification"
    );
}

/// **The in-doubt window, entered on purpose.**
///
/// The journal says `firing` before the command is spawned and `fired` after it
/// returns. A crash between the two leaves an entry nobody can resolve, and this
/// rung resolves it by RUNNING THE COMMAND AGAIN — the duplicate is the failure
/// it chooses, and the re-run says `dup=1` rather than pretending to be the
/// first. The crash is real (`abort`, no destructors) and is timed from inside
/// the process, because the window is microseconds wide.
#[test]
fn a_crash_between_the_exec_and_its_journal_entry_refires_and_says_dup() {
    let fleet = Fleet::boot("notify-doubt");
    let w = Watch::attention(&fleet, "doubt");
    let off = raise_attention(&fleet, "s-one", 1, "the disk is full");

    let crashed = w.once_with(
        "attention",
        &w.appender(),
        &[],
        &[("ATERM_LINK_NOTIFY_FAULT", "kill-after-exec")],
    );
    assert!(
        !crashed.status.success(),
        "the fault kills the notifier: {}",
        crashed.stdout
    );
    assert_eq!(
        w.fired(),
        vec![format!("{off} dup=0 sup=0")],
        "the command DID run before the crash"
    );
    assert!(
        w.journal()
            .iter()
            .any(|l| l == &format!("{off} firing on=attention")),
        "the journal is left in doubt: {:?}",
        w.journal()
    );

    // The fault is one-shot (its marker is in the state dir), so the restart is
    // an ordinary run against an entry it cannot resolve.
    let back = w.once("attention", &[]);
    assert!(back.status.success(), "{}", back.stderr);
    assert_eq!(
        w.fired(),
        vec![format!("{off} dup=0 sup=0"), format!("{off} dup=1 sup=0")],
        "an in-doubt offset fires again, and the re-fire is labelled"
    );
    assert!(back.stdout.contains("dup=1"), "{}", back.stdout);

    // And ONCE it is resolved it stays resolved: the third run does nothing.
    let settled = w.once("attention", &[]);
    assert!(settled.status.success(), "{}", settled.stderr);
    assert_eq!(w.fired().len(), 2, "a resolved offset never fires again");
}

/// **The rate limit drops nothing silently.**
///
/// Over budget, a matching record gets a durable `dropped` verdict, a line on
/// stderr, and a bump of the persisted `suppressed` counter — which the next
/// command that DOES run is told about in `$ATERM_NOTIFY_SUPPRESSED`. A human
/// being rate-limited learns it from the notification itself.
#[test]
fn the_rate_limit_records_every_drop_and_tells_the_next_notification() {
    let fleet = Fleet::boot("notify-rate");
    let w = Watch::attention(&fleet, "rate");
    let offs: Vec<u64> = (1..=3)
        .map(|n| raise_attention(&fleet, "s-one", n, "escalation"))
        .collect();

    // One per hour: the first fires, the other two are refused.
    let run = w.once("attention", &["--rate", "1/1h"]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(w.fired(), vec![format!("{} dup=0 sup=0", offs[0])]);
    assert!(
        run.stdout
            .contains("fired=1 dup=0 skipped=0 dropped=2 suppressed=2"),
        "{}",
        run.stdout
    );
    for off in &offs[1..] {
        assert!(
            run.stderr.contains(&format!("DROPPED off={off}")),
            "every drop says so on stderr: {}",
            run.stderr
        );
        assert!(
            w.journal()
                .iter()
                .any(|l| l.starts_with(&format!("{off} dropped on=attention reason=rate"))),
            "every drop has a durable verdict: {:?}",
            w.journal()
        );
    }

    // A DROPPED OFFSET IS TERMINAL — it is not re-offered when the budget is
    // raised, because a queue of deferred 3 a.m. pushes arriving later is its own
    // kind of noise. What the human gets instead is the count, on the next real
    // notification.
    let fourth = raise_attention(&fleet, "s-one", 4, "escalation");
    let after = w.once("attention", &["--rate", "3/1h"]);
    assert!(after.status.success(), "{}", after.stderr);
    assert_eq!(
        w.fired(),
        vec![
            format!("{} dup=0 sup=0", offs[0]),
            format!("{fourth} dup=0 sup=2")
        ],
        "the next notification carries the count of what was dropped"
    );
    assert!(
        after.stdout.contains("suppressed=0"),
        "and the counter is cleared once it has been told: {}",
        after.stdout
    );
}

/// **A record's body never becomes a command.**
///
/// `--exec` is the operator's own shell line; every fact about the record
/// arrives in the environment, flattened to printable ASCII and capped. A
/// stranger holding a cap can put any bytes in an `attention=`, and none of them
/// can reach `/bin/sh` as syntax.
#[test]
fn an_attention_string_cannot_become_a_shell_word() {
    let fleet = Fleet::boot("notify-inject");
    let w = Watch::attention(&fleet, "inject");
    let pwned = fleet.tmp.join("pwned");
    let payload = format!("; touch {}; echo owned", pwned.display());
    raise_attention(&fleet, "s-one", 1, &payload);

    let exec = format!(
        "printf '%s\\n' \"$ATERM_NOTIFY_TEXT\" >> {}",
        w.log.display()
    );
    let run = w.once_with("attention", &exec, &[], &[]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(
        w.fired(),
        vec![payload],
        "the text arrives verbatim, as DATA"
    );
    assert!(
        !Path::new(&pwned).exists(),
        "nothing in a record's body reached the shell as syntax"
    );
}

/// **Follow mode, a `SIGKILL`, and a restart.**
///
/// The daemon shape: one subscription per selector, live. It is killed with no
/// warning while it is idle, and the restart neither re-runs what already fired
/// nor misses what arrived while it was dead.
///
/// The wait before the kill is on the JOURNAL, not on the command's own output:
/// the journal line is written after the command returns, so waiting on it puts
/// the kill outside the in-doubt window rather than racing it.
#[test]
fn a_follow_mode_notifier_survives_a_kill_and_neither_repeats_nor_misses() {
    let fleet = Fleet::boot("notify-follow");
    let w = Watch::attention(&fleet, "follow");
    let mut daemon = w.follow("attention");

    let first = raise_attention(&fleet, "s-one", 1, "one");
    until("the follower to record a completed notification", || {
        w.journal()
            .iter()
            .any(|l| l.starts_with(&format!("{first} fired")))
            .then_some(())
    });
    assert_eq!(w.fired(), vec![format!("{first} dup=0 sup=0")]);

    daemon.kill();

    // Published while nothing was watching: the cursor is what catches it up.
    let second = raise_attention(&fleet, "s-one", 2, "two");
    let back = w.once("attention", &[]);
    assert!(back.status.success(), "{}", back.stderr);
    let fired = w.fired();
    assert_eq!(
        fired.len(),
        2,
        "one line per offset across the kill: {fired:?}"
    );
    assert_eq!(offset_of(&fired[1]), second);
    assert!(
        fired[1].contains("dup=0"),
        "an offset that was never in doubt is not a duplicate: {fired:?}"
    );
}

/// **Every selector is a real broker filter.** `ask:<p>` and `halt` are matched
/// by position at the broker — a wildcard per segment — and then again by the
/// reader, and both selectors reach the same journal and the same budget.
#[test]
fn ask_and_halt_select_the_records_they_name_and_nothing_else() {
    let fleet = Fleet::boot("notify-sel");
    let w = Watch::new(
        &fleet,
        "sel",
        &[
            format!("ro:/f/{FLEET}/in/>"),
            format!("ro:/f/{FLEET}/fleet/>"),
        ],
    );
    let mut god = fleet.god();
    let mut publish = |seq: u64, subject: &str, body: &str| {
        god.publish(9_200, seq, subject, body.as_bytes())
            .expect("publish")
            .0
    };
    // Two decoys and two hits: another sender's ask, another kind from the
    // watched sender, the watched sender's ask, and a halt coming on.
    publish(
        1,
        &format!("/f/{FLEET}/in/n-a/s-b/s-other/ask"),
        "v=1 t=1 text=no",
    );
    publish(
        2,
        &format!("/f/{FLEET}/in/n-a/s-b/h-andrew/note"),
        "v=1 t=1 text=no",
    );
    let ask = publish(
        3,
        &format!("/f/{FLEET}/in/n-a/s-b/h-andrew/ask"),
        "v=1 t=1 text=is%20this%20ok",
    );
    publish(
        4,
        &format!("/f/{FLEET}/fleet/h-andrew/halt"),
        "v=1 t=1 state=off",
    );
    let halt = publish(
        5,
        &format!("/f/{FLEET}/fleet/h-andrew/halt"),
        "v=1 t=1 state=on reason=main%20broken",
    );

    let run = w.once("ask:h-andrew,halt", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    let fired: Vec<u64> = w.fired().iter().map(|l| offset_of(l)).collect();
    assert_eq!(
        fired,
        vec![ask, halt],
        "only the watched sender's ask and the halt coming ON"
    );
}

/// **A COMMAND THAT DID NOT DELIVER DOES NOT CLEAR THE SUPPRESSED COUNT.**
///
/// The count exists so that a human being rate-limited learns it from the
/// notification itself rather than from silence (the module header). It used to
/// be cleared after `exec` returned WHATEVER the verdict was — the string was
/// never read — so a command that exited 127 because a package upgrade took
/// `curl` off `PATH`, or one that failed to spawn at all, took the whole count
/// to zero with it. Twenty dropped escalations, and the human is never told any
/// of them existed: the silence the rate limiter is built to avoid, reached
/// through its own bookkeeping.
///
/// `rc=0` is the only verdict that says the command was told. Everything else
/// keeps the count for the next command that answers it — told twice, never
/// untold, which is the direction every other choice in this file takes.
#[test]
fn a_command_that_did_not_deliver_does_not_clear_the_suppressed_count() {
    let fleet = Fleet::boot("notify-sup");
    let w = Watch::attention(&fleet, "sup");
    let offs: Vec<u64> = (1..=3)
        .map(|n| raise_attention(&fleet, "s-one", n, "escalation"))
        .collect();

    // One per hour: the first fires, two are refused and counted.
    let run = w.once("attention", &["--rate", "1/1h"]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(w.fired(), vec![format!("{} dup=0 sup=0", offs[0])]);
    assert_eq!(w.suppressed(), 2, "two drops are on disk: {}", run.stdout);

    // THE OPERATOR'S COMMAND IS GONE. `--exec` is run through `/bin/sh`, so a
    // missing program is a shell exit of 127 rather than a failed spawn — the
    // finding's own scenario, and the one that actually happens.
    let fourth = raise_attention(&fleet, "s-one", 4, "escalation");
    let broken = w.once_with(
        "attention",
        "aterm-link-no-such-notifier-9f3c",
        &["--rate", "3/1h"],
        &[],
    );
    assert!(
        broken.status.success(),
        "a broken command never fails the run: {}",
        broken.stderr
    );
    assert!(
        w.journal()
            .iter()
            .any(|l| l.starts_with(&format!("{fourth} fired on=attention rc=127"))),
        "the verdict says the command did not succeed: {:?}",
        w.journal()
    );
    assert_eq!(
        w.suppressed(),
        2,
        "the count must survive a command that told nobody: {}",
        broken.stdout
    );

    // AND THE NEXT COMMAND THAT DOES RUN IS TOLD, which is the whole point of
    // keeping it.
    let fifth = raise_attention(&fleet, "s-one", 5, "escalation");
    let ok = w.once("attention", &["--rate", "9/1h"]);
    assert!(ok.status.success(), "{}", ok.stderr);
    assert_eq!(
        w.fired(),
        vec![
            format!("{} dup=0 sup=0", offs[0]),
            format!("{fifth} dup=0 sup=2")
        ],
        "the count reached the human on the first notification that worked"
    );
    assert_eq!(w.suppressed(), 0, "and only then is it cleared");
}
