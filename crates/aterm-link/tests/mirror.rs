// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A9 — the file mirror, end to end.**
//!
//! One guarded broker, one headless `aterm-gui`, the real `aterm-link serve`
//! child it launches, and a real `aterm-link mirror` process beside them. The
//! three claims §11.2's A9 row pins:
//!
//! 1. a process confined to `<root>` reads its rows from
//!    `<root>/.aterm/<sid>/inbox.ndjson`, and that file is `off=`-IDEMPOTENT;
//! 2. a line appended to `outbox.ndjson` lands on a peer's inbox as an ordinary
//!    `post`;
//! 3. a RELAYED inner line arrives `via=<inner> kind=note demoted=task
//!    trust=relayed`.
//!
//! ## What "confined" means here, exactly
//!
//! The tests read and write ONLY paths under `<root>`, and never the control
//! socket, on the mirrored side. That is the whole of the claim T13 makes: the
//! sandboxed process reaches no socket, so if the files are enough, it
//! participates. It is not a claim that this test process is itself sandboxed —
//! it holds the broker in the same address space — and the way it is kept
//! honest is that every assertion about what the agent SEES reads a file, and
//! every message the agent SENDS is a line appended to a file.
//!
//! ## No sleeps as synchronisation
//!
//! Every wait is `until <observable state>` — a file's contents, a row in a
//! reply. The bound is a HANG DETECTOR, not a performance assertion.

#![cfg(unix)]

mod harness;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use harness::{kill, until, World};

/// The mirror process under test, killed on every exit path.
struct MirrorProc {
    child: Option<Child>,
    root: PathBuf,
    log: PathBuf,
    args: Vec<String>,
}

impl Drop for MirrorProc {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl MirrorProc {
    /// Start `aterm-link mirror` against a booted world. `sessions` restricts it
    /// (empty mirrors every session the instance hosts).
    fn start(w: &World, root: &Path, sessions: &[&str]) -> Self {
        std::fs::create_dir_all(root).expect("the sandbox root");
        let mut args = vec![
            root.to_string_lossy().into_owned(),
            "--sock".to_string(),
            w.ctl_sock.clone(),
            // The interval is the poll, and 25 ms keeps the hang detector's
            // bound from being mistaken for the mirror's latency. It is not a
            // synchronisation primitive: every wait below is on an observable.
            "--interval".to_string(),
            "25".to_string(),
        ];
        for sid in sessions {
            args.push("--session".to_string());
            args.push((*sid).to_string());
        }
        let log = root.join("mirror.log");
        let mut me = Self {
            child: None,
            root: root.to_path_buf(),
            log,
            args,
        };
        me.spawn();
        me
    }

    fn spawn(&mut self) {
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.log)
            .expect("mirror log");
        let err = out.try_clone().expect("mirror log clone");
        self.child = Some(
            Command::new(env!("CARGO_BIN_EXE_aterm-link"))
                .arg("mirror")
                .args(&self.args)
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .expect("launch aterm-link mirror"),
        );
    }

    /// SIGKILL the mirror and start a fresh one on the same root — the restart
    /// that makes the file's `off=` idempotency load-bearing, because a fresh
    /// process re-lists every row the ring still holds.
    fn restart(&mut self) {
        if let Some(mut c) = self.child.take() {
            kill(i32::try_from(c.id()).unwrap_or(0), 9);
            let _ = c.wait();
        }
        self.spawn();
    }

    fn dir(&self, sid: &str) -> PathBuf {
        self.root.join(".aterm").join(sid)
    }

    /// Every line of one of this session's mirror files, as text.
    fn lines(&self, sid: &str, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir(sid).join(file))
            .map(|b| b.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Append one line to a session's `outbox.ndjson` — the ONLY way this test
    /// ever sends on the mirrored side.
    fn append_outbox(&self, sid: &str, line: &str) {
        let path = self.dir(sid).join("outbox.ndjson");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        writeln!(f, "{line}").expect("append one outbox line");
    }

    /// Wait until the mirror has created a session's directory — the observable
    /// that says it is attached and listing.
    fn wait_attached(&self, sid: &str) {
        until("the mirror to create the session's directory", || {
            self.dir(sid).join("inbox.ndjson").exists().then_some(())
        });
    }

    fn log_tail(&self) -> String {
        let body = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    }
}

/// One field of one JSON object line, as text. Deliberately the crate's own
/// reader: the file the agent reads and the file the mirror wrote must be the
/// same file by ONE definition of "a line of JSON", not two.
fn field(line: &str, key: &str) -> Option<String> {
    aterm_link::mirror::json_object(line)?.get(key).cloned()
}

/// The harness's node id, recomputed. `World::boot` derives it from the tag with
/// this FNV-1a so the capability it mints is bound to an id the node already
/// has; `--accept-from` has to name the same principal, and it has to be passed
/// at BOOT, before `World.node` exists to read.
///
/// TWO CHECKS ON THIS COPY, both inside
/// [`a_relayed_line_is_demoted_however_the_allowlist_reads`]: the direct
/// `assert_eq!(w.node, node, …)` on the id itself, and the unrelayed `task`
/// keeping its kind — if the id drifted, the allowlist would never match and
/// that control assertion fails loudly rather than the relayed one passing
/// vacuously.
///
/// This sentence used to cite a test called an_unrelayed_task_keeps_its_kind
/// (unquoted here on purpose — see the pin below), which has never existed
/// under that name: an auditor running it gets
/// `0 passed; 0 filtered out` — a green run over an empty set. It is the same
/// class `bridge.rs`'s header records having been bitten by, and
/// [`every_test_name_this_file_cites_exists`] is why it cannot happen here
/// again.
fn node_id_for(tag: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in tag.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("n-{h:016x}")
}

/// **A confined process reads its rows from a file, and that file is
/// `off=`-idempotent.**
///
/// The row is delivered by the bridge into the endpoint exactly as it is for
/// any agent; the mirror is what makes it readable without a socket. Then the
/// mirror is SIGKILLed and restarted, which makes it re-list every row the ring
/// still holds — and the file must still carry each offset once.
#[test]
fn rows_reach_a_confined_process_and_the_file_is_off_idempotent() {
    let w = World::boot("mirror-in", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let root = w.tmp.join("sandbox");
    let mirror = MirrorProc::start(&w, &root, &[&b]);
    mirror.wait_attached(&b);

    let first = w.verb(&format!("@{a} post to=@{b} kind=note the first row"));
    assert!(first.ok(), "post: {}", first.header());

    let row = until("the row to reach the mirrored file", || {
        mirror
            .lines(&b, "inbox.ndjson")
            .into_iter()
            .find(|l| field(l, "text").as_deref() == Some("the first row"))
    });
    // The FILE says what the socket says. `off=` is the broker offset the
    // endpoint dedups on, so a mirror that invented its own identity would be
    // idempotent about the wrong thing.
    let off = field(&row, "off").expect("every mirrored row carries its offset");
    let listed = until("the endpoint to list the same row", || {
        w.inbox(&b).into_iter().find(|r| r.contains("kind=note"))
    });
    assert!(
        listed.contains(&format!("off={off}")),
        "the file's off= must be the endpoint's:\n  file {row}\n  socket {listed}"
    );
    assert_eq!(field(&row, "kind").as_deref(), Some("note"), "{row}");
    assert_eq!(field(&row, "trust").as_deref(), Some("agent"), "{row}");
    assert_eq!(
        field(&row, "from").as_deref(),
        Some(format!("{a}@{}", w.node).as_str()),
        "{row}"
    );

    // THE IDEMPOTENCY. A fresh mirror asks `since=0` and is handed the same row
    // again; the file is what tells it the row is already there. The second
    // message is the observable that proves a full listing pass ran after the
    // restart — without it, "still one line" would only mean "nothing happened".
    let mut mirror = mirror;
    mirror.restart();
    let second = w.verb(&format!("@{a} post to=@{b} kind=note the second row"));
    assert!(second.ok(), "post: {}", second.header());
    until("the second row to reach the file", || {
        mirror
            .lines(&b, "inbox.ndjson")
            .into_iter()
            .find(|l| field(l, "text").as_deref() == Some("the second row"))
    });
    let repeats = mirror
        .lines(&b, "inbox.ndjson")
        .into_iter()
        .filter(|l| field(l, "off").as_deref() == Some(off.as_str()))
        .count();
    assert_eq!(
        repeats,
        1,
        "off={off} was appended {repeats} times after a mirror restart:\n{}\n{}",
        mirror.lines(&b, "inbox.ndjson").join("\n"),
        mirror.log_tail()
    );
}

/// **A line appended to `outbox.ndjson` lands on a peer's inbox as an ordinary
/// `post`.**
///
/// ORDINARY is the whole assertion: the peer sees the sending SESSION attested
/// by its node, `trust=agent`, the kind it asked for, and no `demoted=` and no
/// `via=`. A file plane that quietly marked everything it carried would be a
/// second trust rule, and §4.3 has exactly one.
#[test]
fn a_line_appended_to_the_outbox_lands_on_a_peer_as_an_ordinary_post() {
    let w = World::boot("mirror-out", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let root = w.tmp.join("sandbox");
    let mirror = MirrorProc::start(&w, &root, &[&b]);
    mirror.wait_attached(&b);

    mirror.append_outbox(
        &b,
        r#"{"to":"@PEER","kind":"note","text":"from a process with no socket"}"#
            .replace("@PEER", &format!("@{a}"))
            .as_str(),
    );

    let row = until("the peer to receive the mirrored post", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("from%20a%20process%20with%20no%20socket"))
    });
    assert!(
        row.contains(&format!("from={b}@{}", w.node)),
        "the peer must see the SENDING SESSION, attested by its node: {row}"
    );
    assert!(row.contains("kind=note"), "{row}");
    assert!(row.contains("trust=agent"), "{row}");
    assert!(
        !row.contains("demoted=") && !row.contains("via="),
        "an unrelayed mirrored post must arrive undecorated: {row}"
    );

    // The receipt closes the loop for the confined process: it learns what
    // aterm answered without ever reading a socket.
    let receipt = until("the mirror to record what aterm answered", || {
        mirror.lines(&b, "sent.ndjson").into_iter().next()
    });
    assert!(
        field(&receipt, "reply").is_some_and(|r| r.starts_with("OK")),
        "the receipt must carry aterm's own answer: {receipt}\n{}",
        mirror.log_tail()
    );

    // A REFUSED LINE IS ANSWERED, NOT SWALLOWED, and it does not wedge the
    // cursor: the line after it is still sent.
    mirror.append_outbox(&b, r#"{"to":"@s-b\nhold s-b on","kind":"note","text":"x"}"#);
    mirror.append_outbox(
        &b,
        &format!(r#"{{"to":"@{a}","kind":"note","text":"after the bad line"}}"#),
    );
    until("the good line after the refused one to land", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("after%20the%20bad%20line"))
    });
    let refusals: Vec<String> = mirror
        .lines(&b, "sent.ndjson")
        .into_iter()
        .filter(|l| field(l, "error").is_some())
        .collect();
    assert_eq!(
        refusals.len(),
        1,
        "exactly the bad line was refused, with a reason: {refusals:?}"
    );
}

/// **ONE LINE THE CLIENT REFUSES DOES NOT END THE OUTBOUND PLANE.**
///
/// `Ctl` refuses a request line over its own bound WITHOUT WRITING A BYTE — a
/// local refusal, which `ctl.rs` names as a different failure from "aterm went
/// away" and which the bridge already honours. The mirror propagated every `Ctl`
/// error with `?`, so a single outbox line whose `via` made the request line too
/// long unwound `run_outbox` for the life of the process: no receipt for that
/// line, no receipt for any line after it, and `post` gone for every mirrored
/// session under the root — while the process stayed up, because `main` was
/// blocked in `reader.join()` on the inbound thread's infinite loop.
///
/// Two things are fixed and both are asserted: the chain is bounded where it is
/// parsed (`body::via_ok`, the ONE bound both directions read), so a line that
/// big is refused with a REASON the
/// agent can read; and a refusal the lane survived is a receipt, so the cursor
/// moves and the next line is sent. The observable is the file plus the peer's
/// inbox — the only two things the confined agent and its correspondent have.
#[test]
fn one_refused_line_does_not_end_the_outbound_plane() {
    let w = World::boot("mirror-refuse", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let root = w.tmp.join("sandbox");
    let mirror = MirrorProc::start(&w, &root, &[&b]);
    mirror.wait_attached(&b);

    // 2000 hops of a 34-byte principal: every element passes `is_principal`, and
    // the `post` line built from them is ~70 KB — past the 64 KiB the writer
    // refuses at. This is the line a confined agent (or a bug in one) appends.
    let hop = format!("n-{}", "a".repeat(32));
    let chain = vec![hop; 2000].join(",");
    mirror.append_outbox(
        &b,
        &format!(r#"{{"to":"@{a}","kind":"note","via":"{chain}","text":"x"}}"#),
    );
    mirror.append_outbox(
        &b,
        &format!(r#"{{"to":"@{a}","kind":"note","text":"after the refused line"}}"#),
    );

    // THE PLANE IS STILL THERE. Without the fix this never arrives: the thread
    // that would send it died on the line before.
    until("the line after the refused one to land on the peer", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("after%20the%20refused%20line"))
    });

    // AND THE REFUSED LINE GOT A VERDICT, which is the other half: an agent
    // whose only evidence is these files must be able to tell "refused" from
    // "not yet".
    let errors: Vec<String> = mirror
        .lines(&b, "sent.ndjson")
        .into_iter()
        .filter(|l| field(l, "error").is_some())
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "exactly the over-long chain was refused, with a reason: {errors:?}\n{}",
        mirror.log_tail()
    );
    assert!(
        field(&errors[0], "error").is_some_and(|e| e.contains("hop")),
        "the receipt must say WHY: {}",
        errors[0]
    );
}

/// **A relayed inner line is demoted, however the allowlist reads.**
///
/// The bridge is booted with this node on `--accept-from`, so a `task` from it
/// keeps its kind — that is the control, and it is what makes the second half
/// mean something. The relayed line, from the same sender over the same file,
/// carries `via=` and arrives `kind=note demoted=task trust=relayed`: §6.7's
/// rule is unconditional, and `via=` can never become an instruction.
#[test]
fn a_relayed_line_is_demoted_however_the_allowlist_reads() {
    let node = node_id_for("mirror-relay");
    let w = World::boot("mirror-relay", &[&node]);
    assert_eq!(
        w.node, node,
        "the node id this test computed must be the one booted"
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let root = w.tmp.join("sandbox");
    let mirror = MirrorProc::start(&w, &root, &[&b]);
    mirror.wait_attached(&b);

    mirror.append_outbox(
        &b,
        &format!(r#"{{"to":"@{a}","kind":"task","text":"an unrelayed task"}}"#),
    );
    mirror.append_outbox(
        &b,
        &format!(r#"{{"to":"@{a}","kind":"task","via":"s-inner01","text":"a relayed task"}}"#),
    );

    let direct = until("the unrelayed task to land", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("an%20unrelayed%20task"))
    });
    assert!(
        direct.contains("kind=task") && !direct.contains("demoted="),
        "an accepted principal's unrelayed task keeps its kind — if this fails the \
         allowlist never matched and the relay assertion below proves nothing: {direct}\n{}",
        mirror.log_tail()
    );

    let relayed = until("the relayed task to land", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("a%20relayed%20task"))
    });
    for want in [
        "kind=note",
        "demoted=task",
        "trust=relayed",
        "via=s-inner01",
    ] {
        assert!(
            relayed.contains(want),
            "a relayed line must arrive `{want}`, allowlist or no allowlist: {relayed}"
        );
    }
    // And it is the SAME sender: the demotion is a property of `via=`, not of
    // who wrote the line.
    assert!(
        relayed.contains(&format!("from={b}@{}", w.node)),
        "{relayed}"
    );
}

/// **A MIRRORED SESSION IS NOT UNREACHABLE AFTER 64 MESSAGES.**
///
/// The endpoint's per-sender quota counts UNLISTED rows: 64 of them from one
/// `from=` principal and the 65th `deliver` is answered `ERR quota`, with no
/// eviction below the cap (`fabric.rs`). Only listing relieves it, and for the
/// population A9 exists to serve — an agent that reaches no socket — the mirror
/// is the only thing that CAN list: the file protocol carries no inbound
/// acknowledgement, so before the mirror acknowledged what it had written, a
/// worker that had read and acted on every message it was ever sent was
/// nonetheless permanently unreachable by its own orchestrator.
///
/// The observable is the FILE, on purpose: what is being asserted is that the
/// 65th message reaches the agent, and for this agent the file is the only place
/// it can reach.
#[test]
fn a_mirrored_session_can_be_sent_more_than_the_per_sender_quota() {
    /// `fabric::SENDER_QUOTA`, which this crate cannot see. The test sends one
    /// past it; the pre-mirror-ack behaviour refused exactly here.
    const SENDER_QUOTA: usize = 64;

    let w = World::boot("mirror-quota", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let root = w.tmp.join("sandbox");
    let mirror = MirrorProc::start(&w, &root, &[&b]);
    mirror.wait_attached(&b);

    for n in 1..=SENDER_QUOTA + 1 {
        let sent = w.verb(&format!("@{a} post to=@{b} kind=note msg-{n}"));
        assert!(sent.ok(), "post {n}: {}", sent.header());
    }

    let last = format!("msg-{}", SENDER_QUOTA + 1);
    until(
        "the message past the quota to reach the mirrored file",
        || {
            mirror
                .lines(&b, "inbox.ndjson")
                .into_iter()
                .find(|l| field(l, "text").as_deref() == Some(last.as_str()))
        },
    );
    // Every one of them, exactly once: the ack must not have cost a row.
    let texts: Vec<Option<String>> = mirror
        .lines(&b, "inbox.ndjson")
        .iter()
        .map(|l| field(l, "text"))
        .collect();
    for n in 1..=SENDER_QUOTA + 1 {
        let want = Some(format!("msg-{n}"));
        assert_eq!(
            texts.iter().filter(|t| **t == want).count(),
            1,
            "msg-{n} is not in the file exactly once\n{}",
            mirror.log_tail()
        );
    }
    // AND THE QUOTA IS ACTUALLY RELIEVED at the endpoint, not merely survived:
    // the rows the mirror wrote are listed, so the ring is not holding anything
    // against the sender any more.
    let header = until("the endpoint to report every row acknowledged", || {
        let reply = w.verb(&format!("@{b} inbox --peek --meta"));
        reply
            .header()
            .contains(&format!("seen={}", SENDER_QUOTA + 1))
            .then(|| reply.header().to_string())
    });
    assert!(header.contains("pending=0"), "{header}");
}

/// **EVERY TEST NAME THIS FILE CITES EXISTS.**
///
/// aterm ships no evidence manifest, so a cited test name IS the claim, and a
/// name nothing answers to is worse than no name at all: `cargo test <name>`
/// answers `0 passed; 0 filtered out`, which reads exactly like a pass. This
/// file cited a test named an_unrelayed_task_keeps_its_kind (written bare here,
/// because backticking it is exactly what this test refuses) for the check on
/// the harness's hand-rolled node id; no function, test or const of it ever
/// existed. `bridge.rs`'s own header records being bitten by the identical
/// thing, so this is a scan rather than one more literal pin.
///
/// The rule is deliberately narrow, and it is this crate's citation convention:
/// a BACKTICKED lowercase identifier with two or more underscores, inside a doc
/// comment of this file, must exist as an `fn` somewhere in `aterm-link`. Write
/// prose about a test that does not exist and this fails and names it.
#[test]
fn every_test_name_this_file_cites_exists() {
    let me = include_str!("mirror.rs");
    let mut cited: Vec<String> = Vec::new();
    for line in me.lines() {
        let t = line.trim_start();
        if !t.starts_with("///") && !t.starts_with("//!") {
            continue;
        }
        for chunk in t.split('`').skip(1).step_by(2) {
            let name = chunk.rsplit("::").next().unwrap_or(chunk);
            let name = name.trim_start_matches('[').trim_end_matches(']');
            let shaped = !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && name.matches('_').count() >= 2;
            if shaped {
                cited.push(name.to_string());
            }
        }
    }
    assert!(
        !cited.is_empty(),
        "the scan found no citation at all; it has stopped guarding anything"
    );

    // Every `.rs` this crate ships, read from the directories rather than from a
    // list: a list stops covering the next file added, which is the same shape
    // of gap as citing a test that is not there.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = String::new();
    for dir in [
        root.join("src"),
        root.join("tests"),
        root.join("tests/harness"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "rs") {
                all.push_str(&std::fs::read_to_string(&path).expect("a source file"));
            }
        }
    }
    let missing: Vec<&String> = cited
        .iter()
        .filter(|n| !all.contains(&format!("fn {n}")))
        .collect();
    assert!(
        missing.is_empty(),
        "this file's docs cite names nothing answers to: {missing:?}"
    );
}
