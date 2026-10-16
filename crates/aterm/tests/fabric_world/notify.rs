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
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_spec::derive::notify_follow_checkpoint_model;
use harness::{FLEET, Fleet, until};

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
        let mut cmd = Command::new(harness::aterm_to_launch());
        cmd.arg("link").args(&args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = harness::output_within(&mut cmd, "aterm link notify");
        Run {
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Spawn the binary in FOLLOW mode, killed on every exit path.
    fn follow(&self, on: &str) -> Follower {
        self.follow_with(on, &self.appender(), &[])
    }

    fn follow_with(&self, on: &str, exec: &str, extra: &[&str]) -> Follower {
        let out = std::fs::File::create(self.state.join("follow.out")).expect("stdio");
        let err = out.try_clone().expect("stdio clone");
        Follower(Some(
            aterm_uds::spawnfd::command(harness::aterm_to_launch())
                .arg("link")
                .args(self.argv(on, exec, extra))
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .expect("spawn aterm link notify"),
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
/// Started by `spawnfd`: it holds its stdio and nothing else of this process,
/// not another test thread's pipe for as long as it follows.
struct Follower(Option<aterm_uds::spawnfd::Child>);

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
    presence_row(
        fleet,
        who,
        seq,
        &format!(
            "state=live inc=1 attention={}",
            aterm_link::pct::encode(text)
        ),
    )
}

/// Publish one presence row for `who` carrying `fields` verbatim, and answer
/// its offset. `seq` is the producer sequence, unique per row.
fn presence_row(fleet: &Fleet, who: &str, seq: u64, fields: &str) -> u64 {
    let subject = format!("/f/{FLEET}/pub/n-alpha/{who}/presence");
    let body = format!("v=1 t={seq} {fields}");
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
/// `aterm link notify` against the same state dir, the same broker and the same
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
        "a changed attention at a new offset is a new notification"
    );
}

/// **A STANDING ATTENTION PAGES ONCE, HOWEVER OFTEN ITS ROW IS REWRITTEN**
/// (r3-22).
///
/// Presence is a last-value face: the bridge writes a session's row again,
/// standing `attention=` and all, on every attach — its first, each
/// reconnect, the successor's after a seamless update — on each meta change,
/// and in its `exited` row. Paged once per offset, one open escalation
/// re-paged the human on every one of those and spent the budget a new
/// escalation then found empty. Here the republishes page nobody and charge
/// nothing — the negative control: offset dedup alone ran the command for
/// each — another session's identical text pages, a clear and the same text
/// again pages, and a fresh process remembers what stands.
#[test]
fn a_republished_standing_attention_pages_nobody_and_a_reraise_after_a_clear_does() {
    let fleet = Fleet::boot("notify-standing");
    let w = Watch::attention(&fleet, "standing");
    let key = format!("attention={}", aterm_link::pct::encode("needs a key"));
    let first = presence_row(&fleet, "s-one", 1, &format!("state=live inc=1 {key}"));
    presence_row(&fleet, "s-one", 2, &format!("state=live inc=2 {key}"));
    presence_row(
        &fleet,
        "s-one",
        3,
        &format!("state=live inc=2 phase=busy {key}"),
    );
    presence_row(&fleet, "s-one", 4, &format!("state=exited inc=2 {key}"));
    let other = presence_row(&fleet, "s-two", 5, &format!("state=live inc=1 {key}"));
    let run = w.once("attention", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(
        w.fired(),
        vec![
            format!("{first} dup=0 sup=0"),
            format!("{other} dup=0 sup=0")
        ],
        "one page per escalation, not per row"
    );
    assert!(
        run.stdout
            .contains("fired=2 dup=0 skipped=0 dropped=0 suppressed=0 standing=3"),
        "{}",
        run.stdout
    );

    presence_row(&fleet, "s-one", 6, "state=live inc=3 attention=-");
    let again = presence_row(&fleet, "s-one", 7, &format!("state=live inc=3 {key}"));
    presence_row(&fleet, "s-one", 8, &format!("state=live inc=4 {key}"));
    let run = w.once("attention", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(
        w.fired(),
        vec![
            format!("{first} dup=0 sup=0"),
            format!("{other} dup=0 sup=0"),
            format!("{again} dup=0 sup=0")
        ],
        "a clear re-arms the subject"
    );

    presence_row(&fleet, "s-one", 9, &format!("state=live inc=5 {key}"));
    let run = w.once("attention", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(w.fired().len(), 3, "a restart remembers what stands");
    assert!(
        run.stdout.contains("fired=0 dup=0") && run.stdout.contains("standing=1"),
        "{}",
        run.stdout
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
        .map(|n| raise_attention(&fleet, "s-one", n, &format!("escalation {n}")))
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
    let fourth = raise_attention(&fleet, "s-one", 4, "escalation 4");
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

/// A follow-mode reader may have a deep, slow backlog while another selector
/// has a fresh halt. Catching up all selectors serially before starting their
/// subscriptions put the halt behind every attention command; each reader now
/// pages independently and hands records to the one serial executor.
#[test]
fn a_slow_attention_catch_up_does_not_put_halt_behind_its_whole_backlog() {
    let fleet = Fleet::boot("notify-parallel-catchup");
    let w = Watch::new(
        &fleet,
        "parallel-catchup",
        &[
            format!("ro:/f/{FLEET}/pub/>"),
            format!("ro:/f/{FLEET}/fleet/>"),
        ],
    );
    let attention: Vec<u64> = (1..=16)
        .map(|seq| raise_attention(&fleet, "s-one", seq, &format!("needs a response {seq}")))
        .collect();
    let halt = fleet
        .god()
        .publish(
            9_300,
            1,
            &format!("/f/{FLEET}/fleet/h-andrew/halt"),
            b"v=1 t=1 state=on reason=blocked",
        )
        .expect("publish halt")
        .0;
    // The first attention is real work that gives both readers time to start.
    // The wait is part of the operator command, not test synchronisation.
    let exec = format!(
        "if [ \"$ATERM_NOTIFY_OFFSET\" = \"{}\" ]; then sleep 1; fi; {}",
        attention[0],
        w.appender()
    );
    let _daemon = w.follow_with("attention,halt", &exec, &["--rate", "100/1m"]);
    until("the halt notification to fire", || {
        w.fired()
            .iter()
            .any(|line| offset_of(line) == halt)
            .then_some(())
    });
    let fired = w.fired();
    assert!(
        !fired.iter().any(|line| offset_of(line) == attention[15]),
        "halt must not wait for the whole attention backlog: {fired:?}"
    );
}

/// `--since=head` is an initial cut, not a fresh cut on every restart. Follow
/// mode persists that first head before subscribing, even when the log is idle.
#[test]
fn follow_since_head_keeps_its_initial_cut_across_an_idle_restart() {
    let fleet = Fleet::boot("notify-head-restart");
    let w = Watch::attention(&fleet, "head-restart");
    let mut daemon = w.follow_with("attention", &w.appender(), &["--since", "head"]);
    until("the head cursor to be persisted", || {
        w.state.join("notify/cur/attention").exists().then_some(())
    });
    daemon.kill();
    let off = raise_attention(&fleet, "s-one", 1, "arrived while offline");
    let _restarted = w.follow_with("attention", &w.appender(), &["--since", "head"]);
    until("the previously unwatched attention to fire", || {
        w.fired()
            .iter()
            .any(|line| offset_of(line) == off)
            .then_some(())
    });
}

/// Tier-1 for `NotifyFollowCheckpoint`: the reader can offer a page mark after
/// handing its last record to the executor, while that record's command is
/// still running. The durable cursor must stop before that record until its
/// command and journal verdict complete. A premature page checkpoint would
/// skip it after a crash.
#[test]
fn a_page_checkpoint_waits_for_the_last_notification_command() {
    let fleet = Fleet::boot("notify-page-checkpoint");
    let w = Watch::attention(&fleet, "page-checkpoint");
    let first = raise_attention(&fleet, "s-one", 1, "first");
    let second = raise_attention(&fleet, "s-one", 2, "second");
    let after_page = fleet
        .god()
        .publish(
            9_301,
            1,
            &format!("/f/{FLEET}/fleet/h-andrew/halt"),
            b"v=1 t=1 state=off",
        )
        .expect("publish filtered-out row")
        .0;
    let entered = w.state.join("second-entered");
    let gate = w.state.join("release-second.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&gate)
            .status()
            .expect("mkfifo")
            .success()
    );
    let exec = format!(
        "if [ \"$ATERM_NOTIFY_OFFSET\" = \"{second}\" ]; then : > {}; read _ < {}; fi; {}",
        entered.display(),
        gate.display(),
        w.appender()
    );
    let _daemon = w.follow_with("attention", &exec, &[]);
    until("the second notification command to block", || {
        entered.exists().then_some(())
    });
    let cursor_path = w.state.join("notify/cur/attention");
    let cursor: u64 = std::fs::read_to_string(&cursor_path)
        .expect("first record cursor")
        .trim()
        .parse()
        .expect("numeric cursor");
    assert_eq!(
        cursor,
        first + 1,
        "the unfinished second row is not skipped"
    );
    assert!(
        w.journal()
            .iter()
            .any(|line| line == &format!("{second} firing on=attention")),
        "the second command is in doubt until released"
    );

    let model = notify_follow_checkpoint_model();
    let mut state = model.init_state();
    assert!(model.fire("ReceiveRecord", &mut state));
    assert!(model.fire("HandleRecord", &mut state));
    assert!(model.fire("ReceiveRecord", &mut state));
    let before_offer = state.clone();
    assert!(model.fire("OfferCheckpoint", &mut state));
    assert_eq!(cursor - first, state["cursor"] as u64);
    assert!(model.check_invariant("CursorNeverSkipsUnfinishedRecord", &state));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut premature = before_offer;
    assert!(buggy.fire("OfferCheckpoint", &mut premature));
    assert!(
        !buggy.check_invariant("CursorNeverSkipsUnfinishedRecord", &premature),
        "a cursor written on checkpoint offer would skip the blocked command"
    );

    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&gate)
        .expect("release the command")
        .write_all(b"go\n")
        .expect("write release");
    until("the page checkpoint to persist after the command", || {
        std::fs::read_to_string(&cursor_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|cursor| *cursor > after_page)
    });
    assert!(w.fired().iter().any(|line| offset_of(line) == second));
    assert!(model.fire("HandleRecord", &mut state));
    assert!(model.fire("PersistCheckpoint", &mut state));
    assert!(model.check_invariant("CursorNeverSkipsUnfinishedRecord", &state));
}

/// **Every selector is a real broker filter, read with the cap that holds
/// it.** `ask:h-andrew` is an ask ADDRESSED TO h-andrew — the subject a
/// session's `post to=h-andrew kind=ask` lands on — read with h-andrew's own
/// ring (`mint-for h-andrew`); `halt` with node.cap's fleet grant. Both are
/// matched by position at the broker and again by the reader, and both reach
/// the same journal and the same budget. And the guarded broker is the
/// negative control: node.cap alone grants no person's mail, and `notify`
/// says so at once, naming the cap to mint, instead of retrying for ever.
#[test]
fn ask_and_halt_select_the_records_they_name_and_nothing_else() {
    let fleet = Fleet::boot("notify-sel");
    let mut grants = aterm_link::enable::person_grants(FLEET, "h-andrew");
    grants.push(format!("ro:/f/{FLEET}/fleet/>"));
    let w = Watch::new(&fleet, "sel", &grants);
    let mut god = fleet.god();
    let mut publish = |seq: u64, subject: &str, body: &str| {
        god.publish(9_200, seq, subject, body.as_bytes())
            .expect("publish")
            .0
    };
    // Three decoys and two hits: an ask FROM h-andrew, another kind to
    // h-andrew, an ask to someone else, the ask to h-andrew, and a halt
    // coming on.
    publish(
        1,
        &format!("/f/{FLEET}/in/n-a/s-b/h-andrew/ask"),
        "v=1 t=1 text=no",
    );
    publish(
        2,
        &format!("/f/{FLEET}/in/p/h-andrew/n-a/note"),
        "v=1 t=1 from=s-b text=no",
    );
    publish(
        3,
        &format!("/f/{FLEET}/in/p/h-other/n-a/ask"),
        "v=1 t=1 from=s-b text=no",
    );
    let ask = publish(
        4,
        &format!("/f/{FLEET}/in/p/h-andrew/n-a/ask"),
        "v=1 t=1 from=s-b text=is%20this%20ok",
    );
    publish(
        5,
        &format!("/f/{FLEET}/fleet/h-andrew/halt"),
        "v=1 t=1 state=off",
    );
    let halt = publish(
        6,
        &format!("/f/{FLEET}/fleet/h-andrew/halt"),
        "v=1 t=1 state=on reason=main%20broken",
    );

    let run = w.once("ask:h-andrew,halt", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    let fired: Vec<u64> = w.fired().iter().map(|l| offset_of(l)).collect();
    assert_eq!(
        fired,
        vec![ask, halt],
        "only the ask addressed to h-andrew and the halt coming ON"
    );

    // node.cap alone: the ask selector reads a lane no grant covers.
    let node = Watch::new(
        &fleet,
        "sel-node",
        &aterm_link::enable::node_grants(FLEET, "n-a"),
    );
    let refused = node.once("ask:h-andrew", &[]);
    assert_eq!(refused.status.code(), Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("--on ask:h-andrew")
            && refused.stderr.contains("aterm fabric mint-for h-andrew"),
        "the refusal names the selector and the cap: {}",
        refused.stderr
    );
    assert!(node.fired().is_empty(), "a refused run fires nothing");
}

/// **A COMMAND THAT DID NOT DELIVER DOES NOT CLEAR THE SUPPRESSED COUNT.**
///
/// The count exists so that a human being rate-limited learns it from the
/// notification itself rather than from silence (the module header). A command
/// that exits 127 (`curl` gone from `PATH`) or fails to spawn must keep the
/// count, or the human is never told the dropped escalations existed.
///
/// `rc=0` is the only verdict that says the command was told. Everything else
/// keeps the count for the next command that answers it — told twice, never
/// untold, which is the direction every other choice in this file takes.
#[test]
fn a_command_that_did_not_deliver_does_not_clear_the_suppressed_count() {
    let fleet = Fleet::boot("notify-sup");
    let w = Watch::attention(&fleet, "sup");
    let offs: Vec<u64> = (1..=3)
        .map(|n| raise_attention(&fleet, "s-one", n, &format!("escalation {n}")))
        .collect();

    // One per hour: the first fires, two are refused and counted.
    let run = w.once("attention", &["--rate", "1/1h"]);
    assert!(run.status.success(), "{}", run.stderr);
    assert_eq!(w.fired(), vec![format!("{} dup=0 sup=0", offs[0])]);
    assert_eq!(w.suppressed(), 2, "two drops are on disk: {}", run.stdout);

    // THE OPERATOR'S COMMAND IS GONE. `--exec` is run through `/bin/sh`, so a
    // missing program is a shell exit of 127 rather than a failed spawn — the
    // finding's own scenario, and the one that actually happens.
    let fourth = raise_attention(&fleet, "s-one", 4, "escalation 4");
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
    let fifth = raise_attention(&fleet, "s-one", 5, "escalation 5");
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
