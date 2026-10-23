// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration tests for session presence publication and ghost retirement over a real broker.

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
#[test]
fn an_unhosted_presence_row_is_retired_and_a_newer_incarnations_is_left_alone() {
    let w = World::boot_with(
        "r21ghost",
        &[],
        &[
            ("ATERM_LINK_FAULT", "sweep-ghosts-at-once-while-marked"),
            harness::FAST_TIMERS,
        ],
    );
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let mark = until("the hosted session's writer mark", || {
        presence_body(&w, &a).and_then(|body| kv(&body, "statedir").map(str::to_owned))
    });
    let other_mark = format!("{:016x}", u64::from_str_radix(&mark, 16).unwrap() ^ 1);

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

    let marker = w.state.join("sweep-ghosts-now");
    std::fs::write(&marker, b"1\n").expect("arm the impatient sweep");
    until("the unhosted row to be retired", || {
        let body = presence_body(&w, ghost)?;
        (kv(&body, "state") == Some("exited")).then_some(())
    });

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
/// NEVER WHAT A COPY OF THE DIR LEFT UNDER THE SAME NUMBER**,
/// on a real broker and a real state dir.
#[test]
fn a_later_bridge_of_the_state_dir_retires_an_earlier_ones_ghost() {
    let w = World::boot_with(
        "f4chain",
        &[],
        &[
            ("ATERM_LINK_FAULT", "sweep-ghosts-at-once-while-marked"),
            harness::FAST_TIMERS,
        ],
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
/// row ever carried the sentinel; answers how long it took.
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
/// (the server reads `idle` only at a box holding the cursor), advanced by Enter.
fn write_fake_claude(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let rule = "─".repeat(120);
    let indicator = format!("{}12% until auto-compact", " ".repeat(96));
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
    let w = World::boot_with("r13-presence", &[], &[harness::FAST_TIMERS]);
    w.wait_ready();
    let (_boot, sid) = w.two_sessions();

    let set = w.verb(&format!("@{sid} meta set role worker satcomp"));
    assert!(set.ok(), "meta set role: {}", set.header());
    let set = w.verb(&format!("@{sid} meta set title satcomp run"));
    assert!(set.ok(), "meta set title: {}", set.header());
    let took = until_field(&w, &sid, "role", "worker%20satcomp");
    eprintln!(
        "MEASURED meta set role -> role= on the bus: {} ms",
        took.as_millis()
    );
    until_field(&w, &sid, "title", "satcomp%20run");
    let body = presence_body(&w, &sid).expect("the row");
    assert_eq!(kv(&body, "title"), Some("satcomp%20run"), "{body}");
    assert_eq!(kv(&body, "phase"), Some("-"), "{body}");
    assert_eq!(kv(&body, "detail"), Some("-"), "{body}");
    assert!(
        kv(&body, "context").is_none(),
        "no indicator, no token: {body}"
    );

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
    until_field(&w, &sid, "detail", "claude");
    let body = presence_body(&w, &sid).expect("the row");
    assert_eq!(
        kv(&body, "detail"),
        Some("claude"),
        "detail= is the running program as `ls` prints it: {body}"
    );
    assert!(kv(&body, "context").is_none(), "{body}");

    let flipped_at = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "idle");
    eprintln!(
        "MEASURED turn ended on screen -> phase=idle on the bus: {} ms",
        flipped_at.elapsed().as_millis()
    );
    let body = presence_body(&w, &sid).expect("the row");
    assert!(kv(&body, "context").is_none(), "{body}");

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
/// ONLY BY THE PUSH.** The roster round reads `meta` only for a slot never read
/// (or one a `GAP` marked unread); `attention=` — the field remote notifiers and
/// `glance` read — `role=` and `title=` move on the `EVENT <local> meta` push.
#[test]
fn a_meta_change_after_the_first_read_rides_the_push_alone() {
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

    assert!(w.verb(&format!("@{sid} meta set role first-read")).ok());
    until_field(&w, &sid, "role", "first-read");

    let set_at = Instant::now();
    assert!(w.verb(&format!("@{sid} meta set attention pushed-a")).ok());
    until_field(&w, &sid, "attention", "pushed-a");
    eprintln!(
        "MEASURED meta set attention (sampled slot) -> attention= on the bus: {} ms",
        set_at.elapsed().as_millis()
    );

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

    std::fs::remove_file(&marker).expect("disarm the dropped push");
    assert!(w.verb(&format!("@{sid} meta set attention pushed-c")).ok());
    until_field(&w, &sid, "attention", "pushed-c");
}
