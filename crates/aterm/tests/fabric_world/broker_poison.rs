// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end tests verifying that a poisoned broker log exits 75 and recovers on supervisor restart.

#![cfg(unix)]

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use crate::harness;

use aterm_link::broker_health::{FAULT_SEAM, POISONED_EXIT, POISONED_EXIT_WORDS};
use harness::{FLEET, SECRET, World, id_at_off, kv, until};

struct BrokerProcess {
    child: aterm_uds::spawnfd::Child,
    /// Held while it serves ([`harness::BrokerChild`]); taken, the broker ends itself at its
    /// next wake.
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
}

impl Drop for BrokerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Supervised {
    secret: PathBuf,
    err: PathBuf,
    fault: String,
}

impl Supervised {
    fn new(w: &World, fault: String) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&w.tmp, std::fs::Permissions::from_mode(0o700))
            .expect("the world's directory is private");
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

    fn start(&self, w: &World) -> BrokerProcess {
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.err)
            .expect("broker.err");
        let secret = self.secret.to_string_lossy().into_owned();
        let args = [
            w.broker_sock.as_str(),
            w.broker_log.as_str(),
            "--secret-file",
            secret.as_str(),
            "--durability",
            "relaxed",
        ];
        let mut started = harness::spawn_broker(&w.tmp, &args, |cmd| {
            cmd.env(FAULT_SEAM, &self.fault)
                .env("XPC_SERVICE_NAME", aterm_link::enable::LABEL)
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(err);
        });
        let out = started.child.stdout.take().expect("the broker's stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(out).read_line(&mut line);
            let _ = tx.send(line);
        });
        let broker = BrokerProcess {
            child: started.child,
            lifeline: Some(started.lifeline),
        };
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

    fn identity(&self, n: usize) -> (String, u64) {
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

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn sock_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atl-{tag}-"))
        .tempdir_in("/tmp")
        .expect("the broker's socket dir")
}

fn fault(to: &str, also: &str) -> String {
    format!("sync escalate=/f/{FLEET}/in/*/{to}/*/note {also}")
}

fn on_lane(w: &World, to: &str, text: &str) -> usize {
    let lane = format!("/f/{FLEET}/in/{}/{to}/{}/note", w.node, w.node);
    let needle = format!("text={}", aterm_link::pct::encode(text));
    let (rows, _) = w.read(|c| c.fetch(0, &lane, 256)).expect("fetch the lane");
    rows.iter()
        .filter(|(_, _, body)| String::from_utf8_lossy(body).contains(&needle))
        .count()
}

fn post(w: &World, from: &str, to: &str, text: &str) {
    let r = w.verb(&format!("@{from} post to=@{to} kind=note {text}"));
    assert!(
        r.ok(),
        "@{from}'s post: {} — log:\n{}",
        r.header(),
        w.log_tail()
    );
}

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

    w.post_landed(&a, &b, "note", "", "before the poison");
    assert_eq!(on_lane(&w, &b, "before the poison"), 1);

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

    drop(broker);
    let _restarted = sup.start(&w);
    let (id_after, generation_after) = sup.identity(1);
    assert_eq!(id_after, id, "the same log");
    assert_eq!(
        generation_after,
        generation + 1,
        "the run was recorded Lossy, so the reopen moves the generation"
    );

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
    w.post_landed(&b, &a, "note", "", "after the restart");
    assert_eq!(on_lane(&w, &a, "after the restart"), 1);
    assert_eq!(on_lane(&w, &a, "refused by the poison"), 1, "still once");
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

    drop(broker.lifeline.take());
    let ended = until("the stayed broker to end with its lifeline", || {
        broker.child.try_wait().ok().flatten()
    });
    let said = read(&sup.err);
    assert!(ended.success(), "a clean stop exits 0: {ended}\n{said}");
    let why = format!("aterm link broker: {}", aterm_link::cli::LIFELINE_CUT_WORDS);
    assert!(said.contains(&why), "it said why it ended:\n{said}");
}

fn on_kind_lane(w: &World, to: &str, kind: &str, needle: &str) -> usize {
    let lane = format!("/f/{FLEET}/in/{}/{to}/{}/{kind}", w.node, w.node);
    let (rows, _) = w.read(|c| c.fetch(0, &lane, 256)).expect("fetch the lane");
    rows.iter()
        .filter(|(_, _, body)| String::from_utf8_lossy(body).contains(needle))
        .count()
}

fn replies_to(w: &World, sid: &str, off: u64, kind: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| kv(r, "re") == Some(&off.to_string()) && kv(r, "kind") == Some(kind))
        .collect()
}

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

    let (_, asked) = w.post_landed(&b, &a, "ask", "", "which branch?");
    let row = until("the ask to reach @a", || id_at_off(&w, &a, asked));

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
