// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A POISONED BROKER LOG IS CURED BY THE BROKER'S OWN EXIT, END TO END.**
//!
//! astream poisons a log whose full flush fails over records no full flush
//! covers: every write is refused until the log is reopened, and only the
//! process that holds it can reopen it (`aterm_link::broker_health`). These
//! worlds run the broker as a real `aterm link broker --durability relaxed`
//! PROCESS, guarded by the harness's secret, under a headless aterm and its
//! real bridge child — the bus as `aterm fabric on` runs it, with this test
//! playing the supervisor.
//!
//! The poison is astream's own, reached the way a weak tier reaches it: the
//! broker fault seam (`$ATERM_LINK_BROKER_FAULT`, a dev build's only) fails
//! every full flush (`InjectedFaults { sync }`, armed once) and adds one
//! escalation rule naming one session's `note` lane, so traffic flows until
//! a post there owes the broker a full flush of its own — which fails and
//! poisons the log (astream's claim `broker.ordered-tier`: "the next open moves
//! the generation (... on Relaxed everywhere)").
//!
//! * THE CURE: the broker exits 75 with one line naming astream's reason, the
//!   test restarts it from the same argv as launchd's `KeepAlive` would (and
//!   with the `XPC_SERVICE_NAME` launchd sets, so the line says the supervisor
//!   reopens it), the log's generation moves (the run was recorded `Lossy`),
//!   and the post the poison refused is delivered EXACTLY ONCE, as is the next.
//! * THE NEGATIVE CONTROL: the seam's `stay` switches the exit off. The broker
//!   serves on over its poisoned log, the posts stay refused, and the bridge
//!   reads `fabric=stalled reason=poisoned` — not `connected` — inside the
//!   one-minute hang detector.
//! * THE ENDPOINT CLAMPS TOO: a reply queued and a verdict given while the
//!   poisoned broker is down survive the generation move its restart makes —
//!   the bridge tells this aterm where the log continues (bridge lane 3) —
//!   and land once each, threaded to the ask; an answer naming an offset past
//!   the fork is retired, never published.
//!
//! Every wait is an `until` over an observable state, a hang detector and never
//! a budget; nothing here sleeps.

#![cfg(unix)]

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use crate::harness;

use aterm_link::broker_health::{FAULT_SEAM, POISONED_EXIT, POISONED_EXIT_WORDS};
use harness::{FLEET, SECRET, World, id_at_off, kv, until};

/// A real `aterm link broker` process, killed and reaped on every exit path.
struct BrokerProcess {
    child: aterm_uds::spawnfd::Child,
}

impl Drop for BrokerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The world's broker, as a process the test supervises: the argv it starts
/// it from, the fault seam it starts it under, and the `broker.err` its stderr
/// is appended to (launchd's `StandardErrorPath`, systemd's `append:`).
struct Supervised {
    secret: PathBuf,
    err: PathBuf,
    fault: String,
}

impl Supervised {
    /// The secret file beside the world's socket, mode 0600, and the seam.
    fn new(w: &World, fault: String) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let secret = w.tmp.join("mint.secret");
        std::fs::write(&secret, SECRET).expect("write the mint secret");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
            .expect("the secret is owner-only");
        Self {
            secret,
            err: w.tmp.join("broker.err"),
            fault,
        }
    }

    /// Start `aterm link broker <sock> <log> --secret-file <secret>
    /// --durability relaxed` and return it once it prints `listening`. The
    /// same argv and environment every time, as a supervisor restarts a job.
    fn start(&self, w: &World) -> BrokerProcess {
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.err)
            .expect("broker.err");
        let mut child = aterm_uds::spawnfd::command(harness::aterm_to_launch())
            .args(["link", "broker", &w.broker_sock, &w.broker_log])
            .arg("--secret-file")
            .arg(&self.secret)
            .args(["--durability", "relaxed"])
            .env(FAULT_SEAM, &self.fault)
            // What launchd tells the job it runs (its `Label`): this harness
            // stands in for the `KeepAlive` job `aterm fabric on` installs.
            .env("XPC_SERVICE_NAME", aterm_link::enable::LABEL)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(err)
            .spawn()
            .expect("spawn the broker");
        let out = child.stdout.take().expect("the broker's stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(out).read_line(&mut line);
            let _ = tx.send(line);
        });
        let broker = BrokerProcess { child };
        let first = rx.recv_timeout(harness::DEADLINE).unwrap_or_else(|_| {
            panic!(
                "the broker printed no readiness line; broker.err:\n{}",
                read(&self.err)
            )
        });
        assert_eq!(
            first.trim_end(),
            format!("listening {}", w.broker_sock),
            "broker.err:\n{}",
            read(&self.err)
        );
        broker
    }

    /// The log identity (`<id hex>.<generation>`) the broker's `n`-th banner
    /// names (the stderr line it writes once it serves), once it is there.
    fn identity(&self, n: usize) -> (String, u64) {
        // WHOLE LINES ONLY: the broker may still be writing the banner when
        // the file is read, and a line without its newline is not one yet.
        let banner = until("the broker's banner on broker.err", || {
            read(&self.err)
                .split_inclusive('\n')
                .filter(|l| l.ends_with('\n'))
                .filter(|l| l.starts_with("aterm link broker: unix socket on "))
                .nth(n)
                .map(str::to_string)
        });
        let ident = banner
            .rsplit_once("; log ")
            .map(|(_, i)| i.trim())
            .unwrap_or_else(|| panic!("the banner names the log identity: {banner}"));
        let (id, generation) = ident
            .rsplit_once('.')
            .unwrap_or_else(|| panic!("an identity is <id>.<generation>: {ident}"));
        (
            id.to_string(),
            generation
                .parse()
                .unwrap_or_else(|_| panic!("a generation: {ident}")),
        )
    }
}

/// The text of `path`, or nothing.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// A directory of the test's own for the broker it serves, short enough for a
/// socket's `sun_path` and held for the test's life (declared before the world,
/// so it goes after it). The world's scratch is an exclusive directory the
/// harness names when it boots, after `boot_at` needs this path.
fn sock_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atl-{tag}-"))
        .tempdir_in("/tmp")
        .expect("the broker's socket dir")
}

/// The fault seam for a world whose sessions are `to` (the escalated one)
/// and another: every full flush fails, and only `to`'s `note` lane owes one.
fn fault(to: &str, also: &str) -> String {
    format!("sync escalate=/f/{FLEET}/in/*/{to}/*/note {also}")
}

/// How many records on `to`'s `note` lane from this node carry `text`, read
/// by `Fetch` from zero: the authority a duplicate cannot hide behind (the
/// endpoint ring dedups on `off=`, the log does not).
fn on_lane(w: &World, to: &str, text: &str) -> usize {
    let lane = format!("/f/{FLEET}/in/{}/{to}/{}/note", w.node, w.node);
    let needle = format!("text={}", aterm_link::pct::encode(text));
    let (rows, _) = w.read(|c| c.fetch(0, &lane, 256)).expect("fetch the lane");
    rows.iter()
        .filter(|(_, _, body)| String::from_utf8_lossy(body).contains(&needle))
        .count()
}

/// A post queued without waiting on its landing: the poison refuses it, and
/// what becomes of it is the property.
fn post(w: &World, from: &str, to: &str, text: &str) {
    let r = w.verb(&format!("@{from} post to=@{to} kind=note {text}"));
    assert!(
        r.ok(),
        "@{from}'s post: {} — log:\n{}",
        r.header(),
        w.log_tail()
    );
}

/// **THE BROKER EXITS 75 OVER ITS POISONED LOG, ITS SUPERVISOR'S RESTART
/// REOPENS IT, AND THE REFUSED POST LANDS ONCE.**
#[test]
fn a_poisoned_log_exits_75_and_the_restarted_broker_lands_the_post_once() {
    let tag = "poison";
    let dir = sock_dir(tag);
    let sock = dir.path().join("b.sock").to_string_lossy().into_owned();
    let w = World::boot_at(tag, &[], &[], Some(&sock));
    let (a, b) = w.two_sessions();
    let sup = Supervised::new(&w, fault(&a, ""));
    let mut broker = sup.start(&w);
    let (id, generation) = sup.identity(0);
    w.wait_ready();

    // TRAFFIC FLOWS: a post on a lane no rule escalates is acked on the page
    // cache and lands, the armed fault notwithstanding (Relaxed flushes
    // nothing of its own).
    w.post_landed(&a, &b, "note", "", "before the poison");
    assert_eq!(on_lane(&w, &b, "before the poison"), 1);

    // THE POISON: the escalated post owes a full flush, which fails.
    post(&w, &b, &a, "refused by the poison");
    let status = until("the broker to exit over its poisoned log", || {
        broker.child.try_wait().ok().flatten()
    });
    let said = read(&sup.err);
    assert_eq!(
        status.code(),
        Some(i32::from(POISONED_EXIT)),
        "broker.err:\n{said}"
    );
    let last = said.lines().last().unwrap_or_default();
    assert!(
        last.starts_with(&format!(
            "aterm link broker: {}: {POISONED_EXIT_WORDS} so the supervisor reopens it (astream: \
             a flush failed (",
            w.broker_log
        )) && last.ends_with(')'),
        "the exit line is broker.err's last word and names astream's reason:\n{said}"
    );

    // THE SUPERVISOR'S RESTART: the same argv and environment. `sync` is
    // spent (its marker is beside the log), so this broker's flushes succeed.
    drop(broker);
    let _restarted = sup.start(&w);
    let (id_after, generation_after) = sup.identity(1);
    assert_eq!(id_after, id, "the same log");
    assert_eq!(
        generation_after,
        generation + 1,
        "the run was recorded Lossy, so the reopen moves the generation"
    );

    // EXACTLY ONCE, end to end: the refused post is resent under its pinned
    // sequence, and the lane holds it once (`on_lane == 1`). This counts
    // copies; it does not see which branch fired. astream proves both
    // (`broker.poisoned-refusal`): a same-boot reopen kept the held record and
    // the resend dedups to it, and a record a power loss cut is appended once.
    // That claim also names what neither this test nor astream reaches: a real
    // device's failed flush that left the record's pages clean but unwritten,
    // where a later power loss makes the next open refuse, loud, not silent.
    until("the refused post to land after the restart", || {
        (on_lane(&w, &a, "refused by the poison") > 0).then_some(())
    });
    assert_eq!(on_lane(&w, &a, "refused by the poison"), 1, "once");
    until("the bridge to say connected again", || {
        w.verb("status")
            .header()
            .contains(" fabric=connected ")
            .then_some(())
    });
    // And the next post, on the same escalated lane, lands once too.
    w.post_landed(&b, &a, "note", "", "after the restart");
    assert_eq!(on_lane(&w, &a, "after the restart"), 1);
    assert_eq!(on_lane(&w, &a, "refused by the poison"), 1, "still once");
    // DELIVERED to the session, once each: the bridge hands a landed record to
    // the endpoint after it reads it back off the bus, so the inbox is waited
    // for, then counted.
    let texts = ["refused by the poison", "after the restart"];
    let needles: Vec<String> = texts
        .iter()
        .map(|t| format!("text={}", aterm_link::pct::encode(t)))
        .collect();
    let inbox = until("both posts in @a's inbox", || {
        let rows = w.inbox(&a);
        needles
            .iter()
            .all(|n| rows.iter().any(|r| r.contains(n)))
            .then_some(rows)
    });
    for (text, needle) in texts.iter().zip(&needles) {
        assert_eq!(
            inbox.iter().filter(|r| r.contains(needle.as_str())).count(),
            1,
            "{text} is delivered to @{a} once: {inbox:?}"
        );
    }
}

/// **NEGATIVE CONTROL: WITH THE EXIT SWITCHED OFF, THE POISON STAYS.** The
/// same world under the seam's `stay`: the broker keeps serving its poisoned
/// log, every post to it stays refused, and the bridge says so —
/// `fabric=stalled reason=poisoned`, never `connected` — within the
/// one-minute hang detector.
#[test]
fn with_the_exit_switched_off_posts_stay_refused_and_the_link_reads_poisoned() {
    let tag = "poistay";
    let dir = sock_dir(tag);
    let sock = dir.path().join("b.sock").to_string_lossy().into_owned();
    let w = World::boot_at(tag, &[], &[], Some(&sock));
    let (a, b) = w.two_sessions();
    let sup = Supervised::new(&w, fault(&a, "stay"));
    let mut broker = sup.start(&w);
    w.wait_ready();
    w.post_landed(&a, &b, "note", "", "before the poison");

    post(&w, &b, &a, "refused by the poison");
    until("the broker to find its log poisoned and stay", || {
        read(&sup.err).contains("NOT exiting").then_some(())
    });
    // A second post from the same sender queues behind the first and wakes
    // the drain that resends it into the poisoned log.
    post(&w, &b, &a, "behind the refused one");
    let header = until("the bridge to read the link poisoned", || {
        let fs = w.verb("fabric status").header().to_string();
        (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("poisoned")).then_some(fs)
    });
    assert!(
        broker.child.try_wait().ok().flatten().is_none(),
        "the broker stayed: {header}\nbroker.err:\n{}",
        read(&sup.err)
    );
    assert!(
        !w.verb("status").header().contains(" fabric=connected "),
        "a poisoned broker is not a connected link"
    );
    for text in ["refused by the poison", "behind the refused one"] {
        assert_eq!(on_lane(&w, &a, text), 0, "{text} stays refused");
    }
}

/// How many records on `to`'s `kind` lane from this node carry `needle` in
/// their body, read by `Fetch` from zero.
fn on_kind_lane(w: &World, to: &str, kind: &str, needle: &str) -> usize {
    let lane = format!("/f/{FLEET}/in/{}/{to}/{}/{kind}", w.node, w.node);
    let (rows, _) = w.read(|c| c.fetch(0, &lane, 256)).expect("fetch the lane");
    rows.iter()
        .filter(|(_, _, body)| String::from_utf8_lossy(body).contains(needle))
        .count()
}

/// The `msg` rows of `sid`'s inbox of `kind` that name `off` as their `re=`.
fn replies_to(w: &World, sid: &str, off: u64, kind: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| kv(r, "re") == Some(&off.to_string()) && kv(r, "kind") == Some(kind))
        .collect()
}

/// **A REPLY QUEUED AND A VERDICT GIVEN WHILE THE POISONED BROKER IS DOWN
/// LAND ONCE IT IS BACK, THREADED TO THE ASK** — the lineage reaches the
/// endpoint (bridge lane 3, `link log <id> continues=<f> was=<id>`).
///
/// @b asks @a on a lane no rule escalates, and the ask lands. The poison
/// stops the broker, and while it is down @a decides the ask (`inbox seen
/// <id> handled`, a receipt owed) and answers it (`re=<the ask>`), and
/// queues a second answer naming an offset the log never reached. The
/// supervisor's restart moves the log's generation (the run was recorded
/// `Lossy`) and astream records where the two agree — every record, in the
/// same boot — and the bridge tells this aterm so. So the answer and the
/// receipt are published once each and reach @b threaded to its own ask
/// (`re-id=`: @b's endpoint kept the landing of its post across the move
/// too), and only the answer naming an offset at or past the fork is retired
/// `log-replaced`, never published — the in-world control. Before lane 3 the
/// endpoint forgot every offset at the move, so the answer and the receipt
/// were retired as well (NEGATIVE CONTROL, run: this test against an
/// endpoint that ignores `continues=` fails at the answer's hang detector),
/// and the asker's bridge said `expired` for an ask that had been answered.
#[test]
fn a_reply_and_a_verdict_made_while_the_poisoned_broker_is_down_land_after_its_restart() {
    let tag = "poisrep";
    let dir = sock_dir(tag);
    let sock = dir.path().join("b.sock").to_string_lossy().into_owned();
    let w = World::boot_at(tag, &[], &[], Some(&sock));
    let (a, b) = w.two_sessions();
    let sup = Supervised::new(&w, fault(&a, ""));
    let mut broker = sup.start(&w);
    let (id, generation) = sup.identity(0);
    w.wait_ready();

    // THE ASK, on @a's `ask` lane, which no rule escalates: it lands.
    let (_, asked) = w.post_landed(&b, &a, "ask", "", "which branch?");
    let row = until("the ask to reach @a", || id_at_off(&w, &a, asked));

    // THE POISON stops the broker.
    post(&w, &b, &a, "refused by the poison");
    let status = until("the broker to exit over its poisoned log", || {
        broker.child.try_wait().ok().flatten()
    });
    assert_eq!(
        status.code(),
        Some(i32::from(POISONED_EXIT)),
        "broker.err:\n{}",
        read(&sup.err)
    );

    // WHILE IT IS DOWN: the verdict, the answer, and an answer naming an
    // offset no record of the log has.
    let seen = w.verb(&format!("@{a} inbox seen {row} handled"));
    assert!(seen.ok(), "inbox seen {row} handled: {}", seen.header());
    let answer = w.verb(&format!("@{a} post to=@{b} kind=answer re={asked} on main"));
    assert!(answer.ok(), "the answer: {}", answer.header());
    let far = asked + 1_000_000;
    let stray = w.verb(&format!(
        "@{a} post to=@{b} kind=answer re={far} past the end"
    ));
    assert!(stray.ok(), "the stray answer: {}", stray.header());
    let stray_id: u64 = stray
        .header()
        .strip_prefix("OK ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a post id: {}", stray.header()));

    // THE RESTART moves the generation, and the bridge clamps at the fork.
    drop(broker);
    let _restarted = sup.start(&w);
    let (id_after, generation_after) = sup.identity(1);
    assert_eq!(id_after, id, "the same log");
    assert_eq!(generation_after, generation + 1, "the next generation");

    let answered = until("@a's answer, queued across the move, to reach @b", || {
        replies_to(&w, &b, asked, "answer").into_iter().next()
    });
    assert!(
        answered.contains("re-id="),
        "the answer threads to @b's own ask, whose landing @b's endpoint kept: {answered}"
    );
    let receipt = until("@a's receipt, owed across the move, to reach @b", || {
        replies_to(&w, &b, asked, "ack").into_iter().next()
    });
    assert!(
        receipt.contains("verdict=handled") && receipt.contains("re-id="),
        "the receipt carries the verdict and threads to the ask: {receipt}"
    );
    let evs = w.ev();
    assert!(
        evs.iter()
            .any(|e| e.starts_with("log-replaced ") && e.contains(" continues=")),
        "the bridge clamped a moved generation: {evs:#?}"
    );

    // ONCE EACH, and the stray answer never: it named an offset at or past
    // the fork, so the endpoint retired it at the move, before any drain.
    let text = aterm_link::pct::encode("on main");
    assert_eq!(on_kind_lane(&w, &b, "answer", &format!("text={text}")), 1);
    assert_eq!(
        on_kind_lane(&w, &b, "ack", &format!(" re={asked} ")),
        1,
        "one receipt"
    );
    let stray_text = aterm_link::pct::encode("past the end");
    assert_eq!(
        on_kind_lane(&w, &b, "answer", &format!("text={stray_text}")),
        0
    );
    let listing = w.verb(&format!("@{a} inbox --peek"));
    assert!(
        !listing
            .rows()
            .iter()
            .any(|r| r.starts_with(&format!("post {stray_id} "))),
        "the stray answer is no longer in flight: {:?}",
        listing.rows()
    );
}
