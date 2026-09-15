// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 13 C — presence with meaning, on a real bus.**
//!
//! A real headless aterm, a real bridge, a real broker, and a session running
//! a Claude-shaped fake TUI (a shell script that paints Claude Code's live
//! zone — a spinner status row, the composer between its two rules, the
//! footer — and moves through busy → idle-with-context → question on each
//! Enter). What is asserted is what lands on the BUS:
//!
//! * the session's presence row carries `role=` and `title=` from its `meta`,
//!   `detail=` from aterm's own `status`, `phase=` and `context=` from the
//!   screen — and flips busy → idle → question as the screen does, each flip
//!   measured and printed;
//! * NEVER a word of the transcript: every screen the fake paints carries a
//!   sentinel string, and no presence row on the bus ever does;
//! * `aterm link ls` prints the new columns off the same rows;
//! * a `--presence minimal` bridge writes today's row and nothing more.

mod harness;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use harness::{World, FLEET};

/// The words that must never leave the screen.
const SENTINEL: &str = "SECRET-TRANSCRIPT-TEXT";

/// The `key=` value of a whitespace-token body.
fn kv<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// The session's presence row on the bus, as its body.
fn presence(w: &World, sid: &str) -> Option<String> {
    let subject = format!("/f/{FLEET}/pub/{}/{sid}/presence", w.node);
    let mut c = w.god();
    let (rows, _) = c.last(&subject, "", 8).ok()?;
    rows.iter()
        .find(|(_, s, _)| *s == subject)
        .map(|(_, _, b)| String::from_utf8_lossy(b).into_owned())
}

/// Wait for the row to carry `key=value`, asserting on every read that no
/// row ever carried the sentinel; answers how long it took.
fn until_field(w: &World, sid: &str, key: &str, want: &str, budget: Duration) -> Duration {
    let started = Instant::now();
    let mut last = None;
    loop {
        if let Some(body) = presence(w, sid) {
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
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The fake Claude: Claude Code's live zone painted by `printf`, advanced by
/// Enter. 120 columns (the harness's `ATERM_COLUMNS`), so the rules are
/// full-width and the context indicator ends two columns short of them,
/// where Claude Code parks it.
fn write_fake_claude(dir: &std::path::Path) -> PathBuf {
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
         read _x\n\
         printf '\\033[2J\\033[H'\n\
         printf '%s\\n' '⏺ {SENTINEL} the plan' '' '✻ Cooked for 3s · done 2:41 PM' '{indicator}' \
         '{rule}' '❯ ' '{rule}' '  ? for shortcuts'\n\
         read _y\n\
         printf '\\033[2J\\033[H'\n\
         printf '%s\\n' '⏺ {SENTINEL} — keep the harness or rewrite it?' '' \
         '{rule}' '❯ ' '{rule}' '  ? for shortcuts'\n\
         read _z\n"
    );
    let path = dir.join("fake-claude.sh");
    std::fs::write(&path, script).expect("write the fake");
    path
}

/// **THE ROW MEANS SOMETHING, IT FLIPS WITH THE SCREEN, AND IT NEVER CARRIES
/// THE SCREEN.**
#[test]
fn a_sessions_presence_row_carries_role_detail_phase_context_and_title_and_never_text() {
    let w = World::boot("r13-presence", &[]);
    w.wait_ready();
    let (_boot, sid) = w.two_sessions();

    // `role=` AND `title=` FROM `meta`, on the next roster round.
    let set = w.verb(&format!("@{sid} meta set role worker satcomp"));
    assert!(set.ok(), "meta set role: {}", set.header());
    let set = w.verb(&format!("@{sid} meta set title satcomp run"));
    assert!(set.ok(), "meta set title: {}", set.header());
    let took = until_field(
        &w,
        &sid,
        "role",
        "worker%20satcomp",
        Duration::from_secs(15),
    );
    eprintln!(
        "MEASURED meta set role -> role= on the bus: {} ms",
        took.as_millis()
    );
    let body = presence(&w, &sid).expect("the row");
    assert_eq!(kv(&body, "title"), Some("satcomp%20run"), "{body}");
    // A shell at its prompt is `idle` with nothing running: the phase reader
    // has no composer frame and no busy signal, and `detail=` is `-`.
    assert_eq!(kv(&body, "phase"), Some("idle"), "{body}");
    assert_eq!(kv(&body, "detail"), Some("-"), "{body}");
    assert!(
        kv(&body, "context").is_none(),
        "no indicator, no token: {body}"
    );

    // THE FAKE CLAUDE, busy first.
    let fake = write_fake_claude(&w.tmp);
    let sent = w.verb(&format!("@{sid} send sh {}", fake.display()));
    assert!(sent.ok(), "send: {}", sent.header());
    let started = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "busy", Duration::from_secs(20));
    eprintln!(
        "MEASURED spinner painted -> phase=busy on the bus: {} ms",
        started.elapsed().as_millis()
    );
    let body = presence(&w, &sid).expect("the row");
    assert_eq!(
        kv(&body, "detail"),
        Some("claude"),
        "detail= is the running program as `ls` prints it: {body}"
    );
    assert!(kv(&body, "context").is_none(), "{body}");

    // ENTER: the turn ends, the done row and the context indicator appear.
    let flipped_at = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "idle", Duration::from_secs(20));
    eprintln!(
        "MEASURED turn ended on screen -> phase=idle on the bus: {} ms",
        flipped_at.elapsed().as_millis()
    );
    let body = presence(&w, &sid).expect("the row");
    assert_eq!(kv(&body, "context"), Some("12%"), "{body}");

    // ENTER: a question.
    let asked_at = Instant::now();
    assert!(w.verb(&format!("@{sid} key enter")).ok());
    until_field(&w, &sid, "phase", "question", Duration::from_secs(20));
    eprintln!(
        "MEASURED question painted -> phase=question on the bus: {} ms",
        asked_at.elapsed().as_millis()
    );
    let body = presence(&w, &sid).expect("the row");
    assert!(
        kv(&body, "context").is_none(),
        "the indicator is gone: {body}"
    );
    assert!(!body.contains(SENTINEL), "{body}");

    // THE WHOLE ROSTER, not just this row: no presence body on the bus
    // carries a word of any screen. (The screen face is opt-in and off.)
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
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-link"))
        .args([
            "ls",
            "--fleet",
            FLEET,
            "--broker",
            &w.broker_sock,
            "--cap-file",
        ])
        .arg(w.tmp.join("node.cap"))
        .output()
        .expect("run ls");
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

/// **`--presence minimal` IS TODAY'S ROW.** `attention=` is there; none of
/// the meaning fields are, even once a role is set and a screen is painted.
#[test]
fn a_minimal_bridge_writes_attention_alone() {
    let w = World::boot_flags("r13-minimal", &["--presence", "minimal"]);
    w.wait_ready();
    let (_boot, sid) = w.two_sessions();
    assert!(w.verb(&format!("@{sid} meta set role worker")).ok());
    assert!(w
        .verb(&format!("@{sid} meta set attention needs-a-key"))
        .ok());
    let took = until_field(
        &w,
        &sid,
        "attention",
        "needs-a-key",
        Duration::from_secs(15),
    );
    eprintln!(
        "MEASURED meta set attention -> attention= on the bus (minimal): {} ms",
        took.as_millis()
    );
    // Give the roster two more rounds to write anything else, then look.
    std::thread::sleep(Duration::from_secs(5));
    let body = presence(&w, &sid).expect("the row");
    for absent in ["role=", "detail=", "phase=", "context=", "title="] {
        assert!(!body.contains(absent), "{absent} on a minimal row: {body}");
    }
    assert!(body.contains(" attention=needs-a-key"), "{body}");
    assert!(body.contains(" state=live "), "{body}");
}
