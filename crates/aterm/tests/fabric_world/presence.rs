// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Presence with meaning, on a real bus.**
//!
//! A real headless aterm, a real bridge, a real broker, and a session running
//! a Claude-shaped fake TUI (a shell script that paints Claude Code's live
//! zone — a spinner status row, the composer between its two rules, the
//! footer — and moves through busy → idle → question on each Enter), run
//! under the argv0 `claude` so aterm identifies it as an agent. What is
//! asserted is what lands on the BUS:
//!
//! * the session's presence row carries `role=` and `title=` from its `meta`,
//!   `detail=` from aterm's own `status`, and `phase=` — the SERVER's agent
//!   verdict (`status agent=`, pushed as `EVENT … agent`), relayed by a bridge
//!   that reads no screen — flipping busy → idle → question as
//!   the screen does, each flip measured and printed; a plain shell is
//!   `phase=-`, and no row carries `context=`;
//! * NEVER a word of the transcript: every screen the fake paints carries a
//!   sentinel string, and no presence row on the bus ever does;
//! * `aterm link ls` prints the new columns off the same rows;
//! * a `meta` change made after the row's first read reaches the bus by the
//!   `EVENT … meta` push alone — with that push held shut, a roster round
//!   republishes the row without it.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use harness::{DEADLINE, FLEET, World, kv, presence_body, until};

/// The words that must never leave the screen.
const SENTINEL: &str = "SECRET-TRANSCRIPT-TEXT";

/// **A PRESENCE ROW NOTHING HOSTS IS RETIRED, AND A LIVE ONE IS NOT.**
///
/// The ghost: a `state=live` session row under this machine's node that no
/// instance here hosts. `aterm fabric status` has always been able to SEE one
/// — it is the warning "the bus advertises @<sid> live on <node> … but no
/// aterm instance here hosts it: mail to it is undeliverable", which was
/// standing on the owner's own machine for s-d5a5de326f33873b6167 when this
/// round began — and nothing could ever clear it. An ordinary exit publishes
/// `state=exited` two ways over, but an instance that dies takes its bridge
/// with it, and the only `Will` in the crate is on the NODE face, so every
/// session row under it stays retained at `state=live` forever.
///
/// THE TWO ROWS THIS MUST TELL APART are both unhosted by THIS bridge. One
/// carries this bridge's writer mark and an `inc=` at or below its own. The
/// other carries a higher one: a
/// bridge that attached AFTER this one, on a node id they share, whose
/// sessions are alive and none of this bridge's business. Retiring the second
/// would take a live agent off the fleet, so the guard is not decoration.
/// The same current number with another mark, or no mark, also stays live.
#[test]
fn an_unhosted_presence_row_is_retired_and_a_newer_incarnations_is_left_alone() {
    let w = World::boot_with(
        "r21ghost",
        &[],
        &[("ATERM_LINK_FAULT", "sweep-ghosts-at-once-while-marked")],
    );
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let mark = until("the hosted session's writer mark", || {
        presence_body(&w, &a).and_then(|body| kv(&body, "statedir").map(str::to_owned))
    });
    let other_mark = format!("{:016x}", u64::from_str_radix(&mark, 16).unwrap() ^ 1);

    // TWO ROWS THIS BRIDGE DID NOT WRITE AND DOES NOT HOST. `inc=1` is at or
    // below any attached bridge's own; `inc=99999` is a successor's.
    //
    // THE GHOSTS' SIDS SORT LAST, and that is what makes the waits below
    // barriers. One sweep judges every row of one `Last` read and then
    // publishes its retirements in the read's subject order, so the ghost's
    // `exited` is the last publish of its sweep: once it is on the bus, a wrong
    // retirement of the sibling or of a hosted session in that sweep is too.
    let ghost = "s-fffffffffffffffffffe";
    let sibling = "s-cafecafecafecafecafe";
    let legacy = "s-abababababababababab";
    let same_inc_copy = "s-babababababababababa";
    let mut god = w.god();
    for (n, (sid, inc, mark)) in [
        (ghost, 1u64, Some(mark.as_str())),
        (sibling, 99_999, Some(mark.as_str())),
        (legacy, 1, None),
        (same_inc_copy, 1, Some(other_mark.as_str())),
    ]
    .iter()
    .enumerate()
    {
        let subject = format!("/f/{FLEET}/pub/{}/{sid}/presence", w.node);
        let mark = mark.map_or_else(String::new, |mark| format!(" statedir={mark}"));
        god.publish(
            7_100 + n as u64,
            1,
            &subject,
            format!("v=1 t=1 inc={inc} epoch=- gen=- state=live hold=0 holder=- attention=-{mark}")
                .as_bytes(),
        )
        .expect("publish the retained row");
    }
    for sid in [ghost, sibling, legacy, same_inc_copy] {
        assert_eq!(
            presence_body(&w, sid)
                .as_deref()
                .and_then(|b| kv(b, "state")),
            Some("live"),
            "the row starts live: {sid}"
        );
    }

    // THE SWEEP, WITH ITS FIVE-MINUTE PATIENCE COLLAPSED AND NOTHING ELSE
    // CHANGED.
    let marker = w.state.join("sweep-ghosts-now");
    std::fs::write(&marker, b"1\n").expect("arm the impatient sweep");
    until("the unhosted row to be retired", || {
        let body = presence_body(&w, ghost)?;
        (kv(&body, "state") == Some("exited")).then_some(())
    });

    // AND A LATER SWEEP, so what follows is "it ran again and left them
    // alone", not "it had not got there yet": a second ghost, sorting after the
    // first, is retired by a sweep that re-read the first one's `exited` row,
    // the sibling and the hosted session — and published any wrong retirement
    // of theirs before this one.
    let second = "s-ffffffffffffffffffff";
    god.publish(
        7_104,
        1,
        &format!("/f/{FLEET}/pub/{}/{second}/presence", w.node),
        format!(
            "v=1 t=1 inc=1 epoch=- gen=- state=live hold=0 holder=- attention=- statedir={mark}"
        )
        .as_bytes(),
    )
    .expect("publish the second ghost");
    until("the second ghost to be retired", || {
        let body = presence_body(&w, second)?;
        (kv(&body, "state") == Some("exited")).then_some(())
    });
    assert_eq!(
        presence_body(&w, sibling)
            .as_deref()
            .and_then(|b| kv(b, "state")),
        Some("live"),
        "a NEWER incarnation's session is not this bridge's to retire"
    );
    assert_eq!(
        presence_body(&w, &a)
            .as_deref()
            .and_then(|b| kv(b, "state")),
        Some("live"),
        "a session this bridge actually hosts is never swept"
    );
    for sid in [legacy, same_inc_copy] {
        assert_eq!(
            presence_body(&w, sid)
                .as_deref()
                .and_then(|body| kv(body, "state")),
            Some("live"),
            "the numeric current incarnation does not prove this writer: {sid}"
        );
    }
    std::fs::remove_file(&marker).expect("disarm the impatient sweep");

    // ONE RECORD PER GHOST, ONCE. Presence is a last-value face on an
    // append-forever log; a sweep that republished on its clock would be one
    // record per dead session per minute, forever.
    let subject = format!("/f/{FLEET}/pub/{}/{ghost}/presence", w.node);
    let retirements = {
        let mut c = w.god();
        let (rows, _) = c
            .fetch(0, &subject, 256)
            .expect("fetch the ghost's subject");
        rows.iter()
            .filter(|(_, _, b)| String::from_utf8_lossy(b).contains("state=exited"))
            .count()
    };
    assert_eq!(retirements, 1, "exactly one retirement record");
}

/// **A LATER BRIDGE OF THE STATE DIR RETIRES WHAT AN EARLIER ONE LEFT, AND
/// NEVER WHAT A COPY OF THE DIR LEFT UNDER THE SAME NUMBER** (the simulation's
/// finding F4, decided), on a real broker and a real state dir.
///
/// The ghost: a `live` row at the node's FIRST incarnation, a session it
/// carried that went with nobody to say so, carrying the state dir's mark
/// (`statedir=`, read off the row of a session this dir's bridge hosts). The
/// bridge is killed twice: the second incarnation's attach reads the first's
/// will on the node face and may retire its rows, but it dies too; the THIRD
/// reads only the second's will, so the first is a stranger to it but for the
/// mark its rows carry — before the mark the row stood until `aterm fabric
/// doctor --retire-ghosts`. The sweep's five-minute patience is collapsed by
/// the marker and nothing else.
///
/// NEGATIVE CONTROL, in the same sweep: a row at that same first incarnation
/// under ANOTHER mark — a copy of the state dir under the same node id, which
/// takes its numbers as this dir does and so can take one of this dir's — is
/// left `live`. Its sid sorts before the ghost's, so a wrong retirement of it
/// is on the bus before the ghost's own (the sweep publishes in subject
/// order). The copy that races an attach for one of this dir's numbers is the
/// simulation's (`crates/aterm-link/src/sim`).
#[test]
fn a_later_bridge_of_the_state_dir_retires_an_earlier_ones_ghost() {
    let w = World::boot_with(
        "f4chain",
        &[],
        &[("ATERM_LINK_FAULT", "sweep-ghosts-at-once-while-marked")],
    );
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let node_inc = |w: &World| -> Option<u64> {
        let body = presence_body(w, "node")?;
        (kv(&body, "state") == Some("live"))
            .then(|| kv(&body, "inc")?.parse().ok())
            .flatten()
    };
    let first = until("the node's first live row", || node_inc(&w));
    let mark = until(
        "a hosted session's row to carry the state dir's mark",
        || {
            let body = presence_body(&w, &a)?;
            kv(&body, "statedir").map(str::to_string)
        },
    );
    let copied = format!(
        "{:016x}",
        u64::from_str_radix(&mark, 16).expect("the mark is sixteen hex digits") ^ 1
    );
    let (copy, ghost) = ("s-fffffffffffffffffffc", "s-fffffffffffffffffffd");
    let mut god = w.god();
    for (n, (sid, by)) in [(ghost, &mark), (copy, &copied)].iter().enumerate() {
        god.publish(
            7_200 + n as u64,
            1,
            &format!("/f/{FLEET}/pub/{}/{sid}/presence", w.node),
            format!(
                "v=1 t=1 inc={first} epoch=- gen=- state=live hold=0 holder=- attention=- \
                 statedir={by}"
            )
            .as_bytes(),
        )
        .expect("publish a row of the first incarnation");
    }
    w.restart_bridge();
    let second = until("the second incarnation", || {
        node_inc(&w).filter(|inc| *inc > first)
    });
    w.restart_bridge();
    let _third = until("the third incarnation", || {
        node_inc(&w).filter(|inc| *inc > second)
    });
    for sid in [ghost, copy] {
        assert_eq!(
            presence_body(&w, sid)
                .as_deref()
                .and_then(|b| kv(b, "state")),
            Some("live"),
            "no sweep has had its five minutes yet: {sid}"
        );
    }
    let marker = w.state.join("sweep-ghosts-now");
    std::fs::write(&marker, b"1\n").expect("arm the impatient sweep");
    until(
        "the first incarnation's ghost to be retired by the third",
        || {
            let body = presence_body(&w, ghost)?;
            (kv(&body, "state") == Some("exited")).then_some(())
        },
    );
    assert_eq!(
        presence_body(&w, copy)
            .as_deref()
            .and_then(|b| kv(b, "state")),
        Some("live"),
        "a copy of the state dir wrote this row, at one of this dir's own incarnations: \
         not this bridge's to retire"
    );
    assert_eq!(
        presence_body(&w, &a)
            .as_deref()
            .and_then(|b| kv(b, "state")),
        Some("live"),
        "a session this bridge hosts is never swept"
    );
    std::fs::remove_file(&marker).expect("disarm the impatient sweep");
}

/// Wait for the row to carry `key=value`, asserting on every read that no
/// row ever carried the sentinel; answers how long it took. The wait is the
/// harness's [`DEADLINE`], a hang detector: nothing bounds the answer (it is
/// printed as the measurement), and the loop leaves the moment the row lands.
fn until_field(w: &World, sid: &str, key: &str, want: &str) -> Duration {
    let budget = DEADLINE;
    let started = Instant::now();
    let mut last = None;
    loop {
        if let Some(body) = presence_body(w, sid) {
            assert!(
                !body.contains(SENTINEL),
                "a presence row carried transcript text: {body}"
            );
            if kv(&body, key) == Some(want) {
                return started.elapsed();
            }
            last = Some(body);
        }
        assert!(
            started.elapsed() < budget,
            "timed out after {budget:?} waiting for {sid}'s presence row to say {key}={want}; \
             last row: {last:?}\n  gui/bridge log tail:\n{}",
            w.log_tail()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The fake Claude: Claude Code's live zone painted by `printf`, with the
/// terminal's cursor left on the prompt box's caret row as Claude Code keeps it
/// (measured 2026-09-27; the server reads `idle` only at a box holding the
/// cursor), advanced by
/// Enter. 120 columns (the harness's `--columns`), so the rules are
/// full-width and the context indicator ends two columns short of them,
/// where Claude Code parks it.
///
/// It answers the path of a `claude` link to `/bin/bash`: the script runs
/// under the argv0 `claude`, which is the foreground program aterm's server
/// resolves (`status program=`) and applies its agent verdict to. The server
/// never applies one to a plain shell, so without the name the row would
/// carry no `phase=` at all — the measured negative control at the top of the
/// test.
fn write_fake_claude(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let rule = "─".repeat(120);
    let indicator = format!("{}12% until auto-compact", " ".repeat(96));
    // THE SHELL-INTEGRATION MARKS FIRST, so aterm has an `Executing` block
    // whose command line (OSC 633;E) is `claude` — that is what `detail=`
    // reads (`session_status::executing_detail`); a shell with no integration
    // has no block and `ls` prints `-`. The tab's nonce, when the shell was
    // handed one, rides along as `id=`.
    let script = format!(
        "#!/bin/sh\n\
         nonce=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007\\033]133;B%s\\007\\033]633;E;claude%s\\007\\033]133;C%s\\007' \
         \"$nonce\" \"$nonce\" \"$nonce\" \"$nonce\"\n\
         printf '\\033[2J\\033[H'\n\
         printf '%s\\n' '⏺ {SENTINEL} the plan' '' '✶ Deliberating… (3s · esc to interrupt)' '' \
         '{rule}' '❯ ' '{rule}' '  ? for shortcuts'\n\
         printf '\\033[6;3H'\n\
         read _x\n\
         printf '\\033[2J\\033[H'\n\
         printf '%s\\n' '⏺ {SENTINEL} the plan' '' '✻ Cooked for 3s · done 2:41 PM' '{indicator}' \
         '{rule}' '❯ ' '{rule}' '  ? for shortcuts'\n\
         printf '\\033[6;3H'\n\
         read _y\n\
         printf '\\033[2J\\033[H'\n\
         printf '%s\\n' '⏺ {SENTINEL} — keep the harness or rewrite it?' '' \
         '{rule}' '❯ ' '{rule}' '  ? for shortcuts'\n\
         printf '\\033[4;3H'\n\
         read _z\n"
    );
    let path = dir.join("fake-claude.sh");
    std::fs::write(&path, script).expect("write the fake");
    let claude = dir.join("claude");
    let _ = std::fs::remove_file(&claude);
    std::os::unix::fs::symlink("/bin/bash", &claude).expect("link claude -> bash");
    (claude, path)
}

/// **THE ROW MEANS SOMETHING, IT FLIPS WITH THE SERVER'S VERDICT, AND IT
/// NEVER CARRIES THE SCREEN.**
#[test]
fn a_sessions_presence_row_relays_the_servers_agent_verdict_and_never_text() {
    // Every flip below lands inside the republish window of the row before it,
    // so each waits for the next round: at test pace ([`harness::FAST_TIMERS`]).
    let w = World::boot_with("r13-presence", &[], &[harness::FAST_TIMERS]);
    w.wait_ready();
    let (_boot, sid) = w.two_sessions();

    // `role=` AND `title=` FROM `meta`, prompted by the pushed meta digest.
    let set = w.verb(&format!("@{sid} meta set role worker satcomp"));
    assert!(set.ok(), "meta set role: {}", set.header());
    let set = w.verb(&format!("@{sid} meta set title satcomp run"));
    assert!(set.ok(), "meta set title: {}", set.header());
    let took = until_field(&w, &sid, "role", "worker%20satcomp");
    eprintln!(
        "MEASURED meta set role -> role= on the bus: {} ms",
        took.as_millis()
    );
    // The two `meta set`s are two digests, and the bridge may publish the row
    // between them: `role=` landing first says nothing of `title=` yet, so the
    // title is waited for too, not read off whichever row came first.
    until_field(&w, &sid, "title", "satcomp%20run");
    let body = presence_body(&w, &sid).expect("the row");
    assert_eq!(kv(&body, "title"), Some("satcomp%20run"), "{body}");
    // NEGATIVE CONTROL: a shell at its prompt is no agent, so the server
    // publishes no verdict and the row carries `phase=-` — the bridge invents
    // nothing (before 2026-09-23 it read `idle` off the screen itself).
    assert_eq!(kv(&body, "phase"), Some("-"), "{body}");
    assert_eq!(kv(&body, "detail"), Some("-"), "{body}");
    assert!(
        kv(&body, "context").is_none(),
        "no indicator, no token: {body}"
    );

    // THE FAKE CLAUDE, busy first.
    let (claude, fake) = write_fake_claude(&w.tmp);
    let sent = w.verb(&format!(
        "@{sid} send {} {}",
        claude.display(),
        fake.display()
    ));
    assert!(sent.ok(), "send: {}", sent.header());
    let started = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "busy");
    eprintln!(
        "MEASURED spinner painted -> phase=busy on the bus: {} ms",
        started.elapsed().as_millis()
    );
    // `detail=` is the foreground program as `ls` prints it, read off the
    // process table: it arrives on its own schedule, not with `phase=`, so a
    // row that says busy can still say `detail=-` for a moment (one full-suite
    // run under load read exactly that). Wait for it like the other fields.
    until_field(&w, &sid, "detail", "claude");
    let body = presence_body(&w, &sid).expect("the row");
    assert_eq!(
        kv(&body, "detail"),
        Some("claude"),
        "detail= is the running program as `ls` prints it: {body}"
    );
    assert!(kv(&body, "context").is_none(), "{body}");

    // ENTER: the turn ends, the done row and the context indicator appear —
    // and the indicator stays on the screen: the server publishes no context
    // figure, so no row carries `context=`.
    let flipped_at = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "idle");
    eprintln!(
        "MEASURED turn ended on screen -> phase=idle on the bus: {} ms",
        flipped_at.elapsed().as_millis()
    );
    let body = presence_body(&w, &sid).expect("the row");
    assert!(kv(&body, "context").is_none(), "{body}");

    // ENTER: a question.
    let asked_at = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "question");
    eprintln!(
        "MEASURED question painted -> phase=question on the bus: {} ms",
        asked_at.elapsed().as_millis()
    );
    let body = presence_body(&w, &sid).expect("the row");
    assert!(kv(&body, "context").is_none(), "{body}");
    assert!(!body.contains(SENTINEL), "{body}");

    // THE WHOLE ROSTER, not just this row: no presence body on the bus
    // carries a word of any screen.
    let mut c = w.god();
    let filter = format!("/f/{FLEET}/pub/*/*/presence");
    let (rows, _) = c.last(&filter, "", 64).expect("the roster");
    assert!(!rows.is_empty());
    for (_, subject, b) in &rows {
        let body = String::from_utf8_lossy(b);
        assert!(!body.contains(SENTINEL), "{subject}: {body}");
        assert!(
            !body.contains("plan") && !body.contains("harness"),
            "{subject}: {body}"
        );
    }

    // `aterm link ls` PRINTS THE SAME FIELDS AS COLUMNS.
    let mut cmd = Command::new(harness::aterm_to_launch());
    harness::scratch_cli(&mut cmd, &w.tmp)
        .args([
            "link",
            "ls",
            "--fleet",
            FLEET,
            "--broker",
            &w.broker_sock,
            "--cap-file",
        ])
        .arg(w.tmp.join("node.cap"));
    let out = harness::output_within(&mut cmd, "aterm link ls");
    let ls = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "ls: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let row = ls
        .lines()
        .find(|l| l.split_whitespace().nth(2) == Some(sid.as_str()))
        .unwrap_or_else(|| panic!("no ls row for {sid}:\n{ls}"));
    assert!(row.contains(" role=worker%20satcomp "), "{row}");
    assert!(row.contains(" phase=question "), "{row}");
    assert!(row.contains(" context=- "), "{row}");
    assert!(row.contains(" title=satcomp%20run"), "{row}");
    assert!(!ls.contains(SENTINEL), "{ls}");
    eprintln!("aterm link ls:\n{ls}");
}

/// A program that marks itself as the session's executing command `name`
/// (the same OSC 133/633 marks the fake Claude writes, so `status detail=`
/// becomes `name`) and then waits on stdin.
fn write_marked_program(dir: &std::path::Path, name: &str) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         nonce=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007\\033]133;B%s\\007\\033]633;E;{name}%s\\007\\033]133;C%s\\007' \
         \"$nonce\" \"$nonce\" \"$nonce\" \"$nonce\"\n\
         read _x\n"
    );
    let path = dir.join(format!("{name}.sh"));
    std::fs::write(&path, script).expect("write the marked program");
    path
}

/// **A `meta` CHANGE AFTER THE FIRST READ REACHES THE BUS BY THE PUSH, AND
/// ONLY BY THE PUSH.** Since 2026-09-23 the roster round reads `meta` only
/// for a slot never read (or one a `GAP` marked unread); `attention=` — the
/// field remote notifiers and `glance` read — `role=` and `title=` move on
/// the `EVENT <local> meta` push. A `meta set` that lands after the row is on
/// the bus is therefore the push's doing — which this test proves rather
/// than assumes, by holding the push shut
/// (the bridge's `Fault::DropMetaEventWhileMarked`) and showing that a roster round
/// that republishes the row (a new `detail=`) does NOT carry the change.
#[test]
fn a_meta_change_after_the_first_read_rides_the_push_alone() {
    // The witness `detail=` is the round's to publish, and every change lands
    // inside the republish window: at test pace ([`harness::FAST_TIMERS`]).
    let w = World::boot_with(
        "r13-meta-push",
        &[],
        &[
            ("ATERM_LINK_FAULT", "drop-meta-event-while-marked"),
            harness::FAST_TIMERS,
        ],
    );
    w.wait_ready();
    let (_boot, sid) = w.two_sessions();

    // THE FIRST READ: the row is on the bus and its slot is sampled.
    assert!(w.verb(&format!("@{sid} meta set role first-read")).ok());
    until_field(&w, &sid, "role", "first-read");

    // THE PUSH CARRIES A LATER CHANGE.
    let set_at = Instant::now();
    assert!(w.verb(&format!("@{sid} meta set attention pushed-a")).ok());
    until_field(&w, &sid, "attention", "pushed-a");
    eprintln!(
        "MEASURED meta set attention (sampled slot) -> attention= on the bus: {} ms",
        set_at.elapsed().as_millis()
    );

    // NEGATIVE CONTROL: with the push dropped, the change does not land —
    // even though a roster round republishes the row after it was made.
    let marker = w.state.join("drop-meta-event");
    std::fs::write(&marker, b"1\n").expect("arm the dropped push");
    assert!(w.verb(&format!("@{sid} meta set attention dropped-b")).ok());
    let witness = write_marked_program(&w.tmp, "r13witness");
    assert!(
        w.verb(&format!("@{sid} send /bin/sh {}", witness.display()))
            .ok()
    );
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "detail", "r13witness");
    let body = presence_body(&w, &sid).expect("the row");
    assert_eq!(
        kv(&body, "attention"),
        Some("pushed-a"),
        "a round republished the row after `meta set attention dropped-b` and must not have \
         carried it — the push is the only path: {body}"
    );

    // THE PUSH RESTORED CARRIES THE NEXT CHANGE.
    std::fs::remove_file(&marker).expect("disarm the dropped push");
    assert!(w.verb(&format!("@{sid} meta set attention pushed-c")).ok());
    until_field(&w, &sid, "attention", "pushed-c");
}
