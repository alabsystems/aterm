// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the menu-bar fleet glance.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn peer(label: &str, header: &[(&str, &str)], rows: &[&[(&str, &str)]]) -> String {
    let object = |pairs: &[(&str, &str)]| {
        let members: Vec<String> = pairs
            .iter()
            .map(|(k, v)| format!("\"{k}\":\"{v}\""))
            .collect();
        format!("{{{}}}", members.join(","))
    };
    let rows: Vec<String> = rows.iter().map(|r| object(r)).collect();
    format!(
        "{{\"label\":\"{label}\",\"fleet\":true,\"instance\":{},\"sessions\":[{}]}}",
        object(header),
        rows.join(",")
    )
}

fn unanswered(label: &str, why: &str) -> String {
    format!("{{\"label\":\"{label}\",\"fleet\":true,\"error\":\"{why}\"}}")
}

fn header(node: &str) -> Vec<(&'static str, String)> {
    vec![
        ("pid", "77".to_string()),
        ("host", "mac.local".to_string()),
        ("sessions", "1".to_string()),
        ("level", "facts".to_string()),
        ("cut", "0".to_string()),
        ("node", node.to_string()),
    ]
}

fn pairs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, s)| (*k, s.as_str())).collect()
}

const QUIET: &[(&str, &str)] = &[
    ("sid", "t000000000q"),
    ("state", "alive"),
    ("program", "zsh"),
    ("program_kind", "shell"),
    ("agent", "-"),
    ("needs", "-"),
];

#[test]
fn a_fleet_read_parses_to_its_peers() {
    let mut hosts = vec![
        "{\"label\":\"pid 1\",\"instance\":{\"pid\":\"1\"},\"sessions\":[]}".to_string(),
        "{\"label\":\"pid 2\",\"instance\":{\"pid\":\"2\"},\"sessions\":[]}".to_string(),
        peer("self", &pairs(&header("own-node")), &[QUIET]),
    ];
    let stalled: &[(&str, &str)] = &[
        ("sid", "t000000000w"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("task", "stalled"),
        ("task_from", "t1e91aaaaa0"),
        ("task_ms", "120000"),
        ("needs", "stalled"),
    ];
    let manager: &[(&str, &str)] = &[
        ("sid", "t1e91aaaaa0"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("role", "lab-sweep"),
        ("needs", "-"),
    ];
    hosts.push(peer("mac-17", &pairs(&header("n17")), &[stalled, manager]));
    let older_row: &[(&str, &str)] = &[
        ("sid", "t0000000010"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "prompt"),
        ("attention", "1"),
    ];
    hosts.push(peer("mac-old", &pairs(&header("nold")), &[older_row]));
    hosts.push(peer(
        "mac-old2",
        &pairs(&header("nold2")),
        &[older_row, QUIET],
    ));
    let mut cut = header("ncut");
    cut[2].1 = "5".to_string();
    hosts.push(peer("mac-cut", &pairs(&cut), &[QUIET, QUIET]));
    for n in 0..24 {
        hosts.push(peer(
            &format!("mac-{n:02}"),
            &pairs(&header(&format!("q{n}"))),
            &[QUIET],
        ));
    }
    for n in 0..3 {
        hosts.push(unanswered(&format!("mac-t{n}"), "no connection in 3 s"));
    }
    let json = format!("[{}]", hosts.join(","));
    let snap = parse(&json, Some("own-node"), 1_000).expect("a survey");
    assert_eq!(snap.macs.len() + snap.unanswered.len(), 31);
    assert_eq!(snap.unanswered.len(), 3);
    assert!(
        !snap
            .macs
            .iter()
            .any(|m| m.dial == "self" || m.dial.starts_with("pid "))
    );
    let m17 = snap
        .macs
        .iter()
        .find(|m| m.dial == "mac-17")
        .expect("mac-17");
    assert_eq!(m17.needs.len(), 1);
    assert_eq!(m17.needs[0].word, NeedWord::Stalled);
    assert_eq!(m17.needs[0].age_ms, Some(120_000));
    assert_eq!(
        m17.activity,
        Some(AgentTally {
            agents: 2,
            busy: 1,
            idle: 1,
            ..AgentTally::default()
        })
    );
    assert_eq!(m17.managers.len(), 1);
    assert_eq!(m17.managers[0].workers, 1);
    let old = snap
        .macs
        .iter()
        .find(|m| m.dial == "mac-old")
        .expect("older");
    assert!(old.older && old.needs.is_empty(), "never counted");
    assert_eq!(snap.macs.iter().filter(|m| m.older).count(), 2);
    assert!(
        snap.macs
            .iter()
            .filter(|m| m.older)
            .all(|m| m.needs.is_empty())
    );
    let cut = snap.macs.iter().find(|m| m.dial == "mac-cut").expect("cut");
    assert_eq!(cut.left_out, 3);
    // Only the Macs whose activity counts: mac-17's two; the older Macs' agents are not read.
    assert!(
        snap.macs
            .iter()
            .filter(|m| m.older)
            .all(|m| m.activity.is_none())
    );
    let with_self = parse(&json, None, 1_000).expect("a survey");
    assert_eq!(with_self.macs.len(), snap.macs.len() + 1);
    assert!(parse("not json", None, 0).is_err());
}

/// A PEER'S UNREAD INSTANCES ARE ITS NODE'S WORD: a node service that read every window says so
/// (`instances_read=`), and only instances past those go unread; a window that answered alone read
/// itself.
#[test]
fn a_peers_unread_instances_are_those_past_its_nodes_word() {
    let with = |dial: &str, extra: &[(&'static str, &str)]| {
        let mut header = header(&format!("n-{dial}"));
        header.extend(extra.iter().map(|(k, v)| (*k, (*v).to_string())));
        peer(dial, &pairs(&header), &[QUIET])
    };
    let json = format!(
        "[{},{},{}]",
        with("mac-all", &[("instances", "2"), ("instances_read", "2")]),
        with("mac-some", &[("instances", "3"), ("instances_read", "2")]),
        with("mac-one", &[("instances", "2")]),
    );
    let snap = parse(&json, None, 1_000).expect("a survey");
    let read = |dial: &str| {
        let mac = snap.macs.iter().find(|m| m.dial == dial).expect(dial);
        mac.instances_unread
    };
    assert_eq!(read("mac-all"), 0);
    assert_eq!(read("mac-some"), 1);
    assert_eq!(read("mac-one"), 1);
}

fn unproven(entry: &str) -> String {
    entry.replacen("\"fleet\":true", "\"fleet\":true,\"unproven\":true", 1)
}

#[test]
fn an_unproven_peer_is_listed_on_its_word_alone() {
    let stalled: &[(&str, &str)] = &[
        ("sid", "t000000000w"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("task", "stalled"),
        ("task_ms", "1000"),
        ("needs", "stalled"),
    ];
    let claims = header("n9");
    let me = peer("self.local", &pairs(&header("own-node")), &[QUIET]);
    let studio = peer("studio.local", &pairs(&claims), &[stalled]);
    type Row = (bool, usize, Option<&'static str>, bool);
    let table: [Row; 2] = [(false, 1, Some("n9"), true), (true, 2, None, false)];
    for (marked, listed, node, counts) in table {
        let entries = [me.clone(), studio.clone()];
        let entries = entries.map(|e| if marked { unproven(&e) } else { e });
        let snap =
            parse(&format!("[{}]", entries.join(",")), Some("own-node"), 0).expect("a survey");
        assert_eq!(snap.macs.len(), listed, "unproven={marked}: {snap:?}");
        let m = snap
            .macs
            .iter()
            .find(|m| m.dial == "studio.local")
            .expect("studio");
        assert_eq!(m.unproven, marked);
        assert_eq!(m.node.as_deref(), node);
        assert_eq!(m.counts(), counts, "unproven={marked}");
        assert_eq!(m.needs.len(), 1, "its needs are read either way");
        if marked {
            let me = snap.macs.iter().find(|m| m.dial == "self.local");
            assert!(
                me.is_some_and(|m| m.unproven && m.node.is_none()),
                "a claim of this Mac's node is listed, not dropped: {me:?}"
            );
        }
    }
}

#[test]
fn a_fleet_need_names_its_tab_as_a_local_one_does() {
    let need_of = |level: &str, needs: &str, extra: &[(&str, &str)]| {
        let mut row: Vec<(&str, &str)> = vec![
            ("sid", "s-a"),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "idle"),
            ("agent_ms", "10800000"),
            ("task_ms", "5000"),
            ("needs", needs),
        ];
        row.extend_from_slice(extra);
        let mut head = header("n1");
        head[3].1 = level.to_string();
        let json = format!("[{}]", peer("mac-1", &pairs(&head), &[row.as_slice()]));
        let snap = parse(&json, None, 1_000).expect("a survey");
        let need = snap.macs[0].needs[0].clone();
        (need.who, need.age_ms)
    };
    let spun = need_of("content", "ask", &[("title", "\u{25d0} fix the build")]);
    assert_eq!(spun, ("fix the build".to_string(), None));
    assert_eq!(
        need_of("content", "ask", &[("title", "\u{25d1} fix the build")]),
        spun,
        "the next spinner frame is the same row"
    );
    let idle = need_of("content", "ask", &[("title", "\u{2733} fix the build")]);
    assert_eq!(idle.0, "fix the build");
    let roles = [("role", "review"), ("user_title", "mine"), ("title", "x")];
    assert_eq!(need_of("content", "ask", &roles).0, "review");
    assert_eq!(need_of("content", "ask", &roles[1..]).0, "mine");
    assert_eq!(need_of("content", "ask", &[]).0, "claude", "the program");
    assert_eq!(
        need_of("facts", "ask", &[("title", "\u{25d0} secret")]).0,
        "claude",
        "facts: no title"
    );
    assert_eq!(need_of("facts", "stalled", &[]).1, Some(5000));
}

/// A need's age is the fact it stands on: a frozen or stopped program that is no agent has none
/// (its row's `agent_ms` says nothing of it), and an agent's is its agent word's.
#[test]
fn a_freeze_dates_only_an_agent() {
    let need_of = |needs: &str, row: &[(&str, &str)]| {
        let mut fields: Vec<(&str, &str)> = vec![
            ("sid", "s-a"),
            ("state", "alive"),
            ("program", "claude"),
            ("agent_ms", "10800000"),
            ("needs", needs),
        ];
        fields.extend_from_slice(row);
        let json = format!(
            "[{}]",
            peer("mac-1", &pairs(&header("n1")), &[fields.as_slice()])
        );
        parse(&json, None, 1_000).expect("a survey").macs[0].needs[0].clone()
    };
    let plain = [("program_kind", "program"), ("agent", "-")];
    assert_eq!(need_of("frozen", &plain).age_ms, None);
    assert_eq!(need_of("stopped", &plain).age_ms, None);
    let frozen_agent = [("program_kind", "agent"), ("agent", "wall:unresponsive")];
    assert_eq!(need_of("frozen", &frozen_agent).age_ms, Some(10_800_000));
}

/// WHAT THE BOOK SAYS OF THIS MAC (`read_book`): it is in a fleet where it holds a fleet key,
/// and in none without one.
#[test]
fn the_book_says_whether_this_mac_is_in_a_fleet() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-read-book")
        .tempdir()
        .expect("scratch dir");
    let dir = dir.path();
    assert_eq!(read_book(dir), BookFacts::default(), "no key: no fleet");
    std::fs::write(dir.join(aterm_net::fleet::KEY_FILE), b"key").unwrap();
    assert_eq!(read_book(dir), BookFacts { in_fleet: true });
}

#[test]
fn a_peer_known_by_name_is_dialled_by_it() {
    let exited: &[(&str, &str)] = &[("sid", "s-x"), ("state", "exited"), ("needs", "-")];
    let at = |addr: &str| {
        let mut head = header("AB12CD34EF56AB12CD34EF56AB12CD34");
        head[1].1 = "mac-17.local".to_string();
        head[2].1 = "3".to_string();
        format!(
            "[{},{},{},{}]",
            peer(
                &format!("mac-17.local at {addr}"),
                &pairs(&head),
                &[QUIET, QUIET, exited]
            ),
            peer("lab-mini:7101 at 10.0.0.5", &pairs(&header("n2")), &[QUIET]),
            peer("10.0.0.9", &pairs(&header("n3")), &[QUIET]),
            unanswered("mac-22.local", "no connection in 3 s"),
        )
    };
    let snap = parse(&at("192.168.1.17"), None, 0).expect("a survey");
    let m17 = &snap.macs[0];
    assert_eq!(
        (m17.dial.as_str(), m17.name.as_str()),
        ("mac-17.local", "mac-17")
    );
    assert_eq!(
        m17.node.as_deref(),
        Some("ab12cd34ef56ab12cd34ef56ab12cd34")
    );
    assert_eq!(m17.left_out, 0, "the exited row was sent");
    let lab = &snap.macs[1];
    assert_eq!(
        (lab.dial.as_str(), lab.name.as_str()),
        ("lab-mini:7101", "lab-mini")
    );
    assert_eq!(snap.macs[2].name, "10.0.0.9", "an address is shown whole");
    assert_eq!(
        (
            snap.unanswered[0].dial.as_str(),
            snap.unanswered[0].name.as_str()
        ),
        ("mac-22.local", "mac-22")
    );
    let moved = parse(&at("192.168.1.40"), None, 0).expect("a survey");
    assert_eq!(
        (&moved.macs[0].dial, &moved.macs[0].node),
        (&m17.dial, &m17.node),
        "a new address is the same Mac"
    );
}

#[test]
fn the_fleet_word_tells_no_fleet_from_not_read_yet() {
    let mut slot = FleetSlot::default();
    assert_eq!(slot.word(), "off");
    slot.book.in_fleet = true;
    assert_eq!(slot.word(), "unread");
    slot.reading = true;
    assert_eq!(slot.word(), "reading");
    slot.reading = false;
    slot.snapshot = Some(Arc::new(FleetSnapshot {
        read_at_ms: 42,
        ..FleetSnapshot::default()
    }));
    assert_eq!(slot.word(), "as_of:42");
}

#[test]
fn should_poll_reads_on_the_minute_under_watch_and_never_out_of_a_fleet() {
    let member = BookFacts { in_fleet: true };
    let none = BookFacts::default();
    let at = |s: u64| s * 1000;
    type Case<'a> = (&'a BookFacts, bool, Option<u64>, u64, bool);
    let cases: [Case<'_>; 7] = [
        (&none, true, None, at(100), false),
        (&member, false, None, at(100), false),
        (&member, false, Some(at(0)), at(100), false),
        (&member, true, None, at(1), true),
        (&member, true, Some(at(0)), at(59), false),
        (&member, true, Some(at(0)), at(60), true),
        (&member, true, Some(at(10)), at(70), true),
    ];
    for (n, (book, watch, last, now, want)) in cases.into_iter().enumerate() {
        assert_eq!(
            should_poll(book, watch, last, now, POLL_EVERY),
            want,
            "case {n}"
        );
    }
}

/// A read where every peer the book names was dark.
struct Nobody {
    runs: Sender<()>,
}

impl FleetRunner for Nobody {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        let _ = self.runs.send(());
        parse(
            r#"[{"fleet":true,"label":"m7.local at 192.168.1.7:7100","error":"did not answer in time"}]"#,
            own_node,
            now_ms(),
        )
    }
}

/// THE ISSUER'S GLANCE BACKS OFF WHILE IT REACHES NOBODY (docs/DESIGN-fluid-swarm-2026-10-28.md
/// F4): after a read that reached no peer the next waits twice as long, up to 30 minutes; a move
/// of this Mac's network reads at once and starts the wait again from the minute; a read that
/// reached a peer starts it again too.
/// NEGATIVE CONTROL: a constant 60 s wait (no `Quiet`) reads again at the minute, failing the
/// first "no read" look; a `Kick::Moved` that ends no backoff fails the move's read.
#[test]
fn a_glance_that_reached_nobody_backs_off_and_a_move_resets_it() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let start = 1_000_000;
    let clock = Arc::new(AtomicU64::new(start));
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let now = Arc::clone(&clock);
    let glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            now_ms: Box::new(move || {
                let _ = looks_tx.send(());
                now.load(Ordering::SeqCst)
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    // Until the read just made is over and dated `at_ms`: the reader dates a read by the clock
    // after it, so the clock moves only once it has.
    let read_over = |at_ms: u64| {
        while glance.copy().tried_ms != Some(at_ms) {
            looks.recv_timeout(hang).expect("the read ends");
        }
    };
    runs.recv_timeout(hang).expect("the issuer reads at once");
    read_over(start);
    // Whether the reader, looking three times at `at` seconds past the start (its decision is the
    // first or second look, and a read it makes runs before the third), read.
    let read_at = |at: u64| {
        let now = start + at * 1000;
        clock.store(now, Ordering::SeqCst);
        while looks.try_recv().is_ok() {}
        for _ in 0..3 {
            looks.recv_timeout(hang).expect("the reader looks again");
        }
        let read = runs.try_recv().is_ok();
        if read {
            read_over(now);
        }
        read
    };
    assert!(!read_at(60), "reached nobody: not again at the minute");
    assert!(read_at(120), "but at two");
    assert!(!read_at(120 + 239), "then four");
    assert!(read_at(120 + 240));
    let mut at = 120 + 240;
    let mut wait = 480;
    while wait < QUIET_MAX.as_secs() {
        assert!(!read_at(at + wait - 1), "wait {wait}");
        assert!(read_at(at + wait), "wait {wait}");
        at += wait;
        wait *= 2;
    }
    assert!(!read_at(at + QUIET_MAX.as_secs() - 1), "never past 30 min");
    assert!(read_at(at + QUIET_MAX.as_secs()));
    // The move comes a second after that read, so its own read is dated apart from it.
    at += QUIET_MAX.as_secs() + 1;
    clock.store(start + at * 1000, Ordering::SeqCst);
    glance.kick(Kick::Moved);
    runs.recv_timeout(hang).expect("a move reads at once");
    read_over(start + at * 1000);
    assert!(!read_at(at + 119), "the move started the wait again");
    assert!(read_at(at + 120));
}

/// AWAY, A GLANCE THAT REACHED NOBODY WAITS THE LONGEST AT ONCE: off the ALab LAN, after a read
/// that reached no peer the next waits the full 30 minutes (no doubling that dials the members
/// behind the ALab LAN five more times); the move back reads at once.
/// NEGATIVE CONTROL: `Quiet::after` ignoring `away` doubles the wait, reads again at two minutes,
/// and "away: the longest wait at once" fails.
#[test]
fn away_a_glance_that_reached_nobody_waits_the_longest_at_once() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let start = 1_000_000;
    let clock = Arc::new(AtomicU64::new(start));
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let now = Arc::clone(&clock);
    let glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            now_ms: Box::new(move || {
                let _ = looks_tx.send(());
                now.load(Ordering::SeqCst)
            }),
            away: Box::new(|| true),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    let read_over = |at_ms: u64| {
        while glance.copy().tried_ms != Some(at_ms) {
            looks.recv_timeout(hang).expect("the read ends");
        }
    };
    runs.recv_timeout(hang).expect("the issuer reads at once");
    read_over(start);
    let read_at = |at: u64| {
        let now = start + at * 1000;
        clock.store(now, Ordering::SeqCst);
        while looks.try_recv().is_ok() {}
        for _ in 0..3 {
            looks.recv_timeout(hang).expect("the reader looks again");
        }
        let read = runs.try_recv().is_ok();
        if read {
            read_over(now);
        }
        read
    };
    assert!(!read_at(120), "away: the longest wait at once");
    assert!(!read_at(QUIET_MAX.as_secs() - 1));
    assert!(read_at(QUIET_MAX.as_secs()));
    let at = QUIET_MAX.as_secs() + 1;
    clock.store(start + at * 1000, Ordering::SeqCst);
    glance.kick(Kick::Moved);
    runs.recv_timeout(hang)
        .expect("the move back reads at once");
}

/// THE GLANCE WAITS FOR THE LAN BROKER: a read dials the fleet's Macs through the broker, so
/// while the broker does not answer, the glance reads nothing (its tick, a move of the network);
/// once it answers, it reads.
#[test]
fn the_glance_reads_nothing_by_itself_while_the_lan_broker_does_not_answer() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let broker_answers = Arc::new(AtomicBool::new(false));
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let asked = Arc::clone(&broker_answers);
    let glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            sharing: Box::new(|| true),
            lan: Box::new(move || {
                let _ = looks_tx.send(());
                asked.load(Ordering::SeqCst)
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    let looks_three_times = || {
        while looks.try_recv().is_ok() {}
        for _ in 0..3 {
            looks
                .recv_timeout(hang)
                .expect("the reader looks for the broker");
        }
    };
    looks_three_times();
    assert!(
        runs.try_recv().is_err(),
        "it read by itself before the broker answered"
    );
    glance.kick(Kick::Moved);
    looks_three_times();
    assert!(
        runs.try_recv().is_err(),
        "a move read before the broker answered"
    );
    broker_answers.store(true, Ordering::SeqCst);
    runs.recv_timeout(hang)
        .expect("it reads once the broker answers");
}

struct Gated {
    runs: Arc<AtomicUsize>,
    gate: Receiver<()>,
    started: Sender<()>,
}

impl FleetRunner for Gated {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let _ = self.started.send(());
        let _ = self.gate.recv();
        parse("[]", own_node, now_ms())
    }
}

fn seams(watch: bool, posts: Sender<()>) -> ReaderSeams {
    ReaderSeams {
        book: Box::new(|| BookFacts { in_fleet: true }),
        watch: Arc::new(AtomicBool::new(watch)),
        own_node: None,
        now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
        post: Box::new(move || posts.send(()).is_ok()),
        tick: Duration::from_secs(3600),
        moved: None,
        sharing: Box::new(|| true),
        lan: Box::new(|| true),
        away: Box::new(|| false),
    }
}

#[test]
fn the_reader_says_when_a_read_began_and_ended_and_each_writes_the_slot_once() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let runs = Arc::new(AtomicUsize::new(0));
    let (open, gate) = std::sync::mpsc::channel();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(Gated {
            runs: Arc::clone(&runs),
            gate,
            started: started_tx,
        }),
        seams(true, posts_tx),
    )
    .expect("the thread starts");
    started
        .recv_timeout(hang)
        .expect("the issuer reads at once");
    assert!(glance.copy().reading);
    open.send(()).unwrap();
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert!(!glance.copy().reading);
    assert_eq!(glance.copy().generation, 1);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    glance.kick(Kick::Moved);
    started.recv_timeout(hang).expect("a move reads again");
    open.send(()).unwrap();
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(glance.copy().generation, 2);
}

/// A KICK THAT COMES ONCE A READ IS OVER IS ANSWERED: a move of the network sent once a read's
/// result is in the slot asks for a read of its own, though the clock says the last was just now.
/// NEGATIVE CONTROL: dropping the kicks queued once the slot is written loses that one.
#[test]
fn a_kick_once_a_read_is_in_the_slot_reads_again() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let runs = Arc::new(AtomicUsize::new(0));
    let (open, gate) = std::sync::mpsc::channel();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let (looked_tx, looked) = std::sync::mpsc::channel();
    let (go_on_tx, go_on) = std::sync::mpsc::channel::<()>();
    let looks = AtomicUsize::new(0);
    let glance = spawn(
        Box::new(Gated {
            runs: Arc::clone(&runs),
            gate,
            started: started_tx,
        }),
        ReaderSeams {
            // The book's look once the first read is in the slot holds the reader there until
            // the test has kicked.
            book: Box::new(move || {
                if looks.fetch_add(1, Ordering::SeqCst) == 1 {
                    let _ = looked_tx.send(());
                    let _ = go_on.recv_timeout(hang);
                }
                BookFacts { in_fleet: true }
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    started
        .recv_timeout(hang)
        .expect("the issuer reads at once");
    open.send(()).unwrap();
    looked.recv_timeout(hang).expect("the read is over");
    assert!(
        glance.copy().tried_ms.is_some(),
        "its result is in the slot"
    );
    glance.kick(Kick::Moved);
    go_on_tx.send(()).unwrap();
    started.recv_timeout(hang).expect("the kick reads again");
    open.send(()).unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// A MOVE WHILE A READ RUNS IS NOT ANSWERED BY IT: that read went out on the old network, so once
/// it is over the reader reads again, as a move does at any other time, though the clock says the
/// last read was just now.
/// NEGATIVE CONTROL: dropping a move with the kicks a read answered reads nothing more.
#[test]
fn a_move_while_a_read_runs_reads_again_once_it_is_over() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let runs = Arc::new(AtomicUsize::new(0));
    let (open, gate) = std::sync::mpsc::channel();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(Gated {
            runs: Arc::clone(&runs),
            gate,
            started: started_tx,
        }),
        ReaderSeams {
            now_ms: Box::new(|| 1_000_000),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    started
        .recv_timeout(hang)
        .expect("the issuer reads at once");
    glance.kick(Kick::Moved);
    open.send(()).unwrap();
    started
        .recv_timeout(hang)
        .expect("the move reads again on the new network");
    open.send(()).unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

struct Empty;

impl FleetRunner for Empty {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        parse("[]", own_node, now_ms())
    }
}

#[test]
fn the_app_is_told_when_the_last_read_stops_counting() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let now = Arc::clone(&clock);
    let glance = spawn(
        Box::new(Empty),
        ReaderSeams {
            tick: Duration::from_millis(20),
            now_ms: Box::new(move || {
                let _ = looks_tx.send(());
                now.load(Ordering::SeqCst)
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    // One read, then the watch goes off: the clock stands still until then, so no tick reads.
    glance.set_watch(false);
    let read_at = glance.copy().snapshot.expect("read").read_at_ms;
    while looks.try_recv().is_ok() {}
    for _ in 0..3 {
        looks.recv_timeout(hang).expect("the reader looks again");
    }
    assert!(posts.try_recv().is_err(), "a fresh read owes no post");
    clock.store(read_at - 1_000, Ordering::SeqCst);
    posts
        .recv_timeout(hang)
        .expect("the clock rollback is told");
    let rewound = glance.copy();
    assert!(!rewound.snapshot.expect("kept").fresh(read_at - 1_000));
    assert_eq!(rewound.generation, 1, "the rollback makes no new read");
    assert!(!rewound.reading, "disabled polling stays disabled");
    clock.store(read_at, Ordering::SeqCst);
    posts
        .recv_timeout(hang)
        .expect("the clock catch-up is told");
    let caught_up = glance.copy();
    assert!(caught_up.snapshot.expect("kept").fresh(read_at));
    assert_eq!(caught_up.generation, 1, "catch-up makes no new read");
    assert!(!caught_up.reading, "polling remains disabled");
    clock.store(read_at + millis(COUNT_FRESH) + 1_000, Ordering::SeqCst);
    posts.recv_timeout(hang).expect("the lapse is told");
    let slot = glance.copy();
    assert!(
        !slot
            .snapshot
            .expect("kept")
            .fresh(clock.load(Ordering::SeqCst))
    );
    assert!(!slot.reading, "the lapse is no read");
}

#[test]
fn a_lapse_that_passes_while_the_read_is_told_is_told() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let start = 1_000_000;
    let lapsed = start + millis(COUNT_FRESH) + 1_000;
    let clock = Arc::new(AtomicU64::new(start));
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let (now, moved) = (Arc::clone(&clock), Arc::clone(&clock));
    let told = AtomicUsize::new(0);
    // The watch goes off as the read's end is told, so the lapse that follows is no read.
    let watch = Arc::new(AtomicBool::new(true));
    let watched = Arc::clone(&watch);
    let glance = spawn(
        Box::new(Empty),
        ReaderSeams {
            tick: Duration::from_secs(3600),
            now_ms: Box::new(move || now.load(Ordering::SeqCst)),
            post: Box::new(move || {
                if told.fetch_add(1, Ordering::SeqCst) == 1 {
                    watched.store(false, Ordering::SeqCst);
                    moved.store(lapsed, Ordering::SeqCst);
                }
                posts_tx.send(()).is_ok()
            }),
            watch,
            ..seams(true, std::sync::mpsc::channel().0)
        },
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    let read = glance.copy().snapshot.expect("read");
    assert!(read.fresh(start), "CONTROL: fresh when its end was told");
    posts.recv_timeout(hang).expect("the lapse is told");
    let slot = glance.copy();
    assert!(!slot.snapshot.expect("kept").fresh(lapsed));
    assert_eq!(slot.generation, 1, "CONTROL: the lapse is no read");
    assert!(!slot.reading);
}

/// A CHILD THAT NEVER ANSWERS IS KILLED AT THE CAP and the read fails: the
/// slot keeps no snapshot, says why, and the App is told.
#[cfg(unix)]
#[test]
fn a_child_that_never_answers_is_killed_and_posts_its_failure() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(ChildRunner {
            program: PathBuf::from("/bin/sleep"),
            args: vec!["600".to_string()],
            cap: Duration::from_millis(200),
            sighted: |_| {},
        }),
        seams(true, posts_tx),
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    let slot = glance.copy();
    assert!(slot.snapshot.is_none());
    assert!(slot.error.is_some(), "{slot:?}");
    assert_eq!(slot.word(), "failed");
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(ChildRunner {
            program: PathBuf::from("/bin/echo"),
            args: vec!["[]".to_string()],
            cap: Duration::from_secs(60),
            sighted: |_| {},
        }),
        seams(true, posts_tx),
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert!(glance.copy().snapshot.is_some());
    assert!(glance.copy().word().starts_with("as_of:"));
}

fn inert_seams() -> ReaderSeams {
    ReaderSeams {
        book: Box::new(BookFacts::default),
        watch: Arc::new(AtomicBool::new(false)),
        own_node: None,
        now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
        post: Box::new(|| true),
        tick: Duration::from_secs(3600),
        moved: None,
        sharing: Box::new(|| true),
        lan: Box::new(|| true),
        away: Box::new(|| false),
    }
}

#[test]
fn a_headless_app_starts_no_fleet_glance() {
    let mut app = crate::App::headless_for_test();
    app.start_fleet_glance_with(Box::new(Empty), inert_seams());
    app.start_fleet_glance();
    assert!(app.fleet_glance.is_none());
    assert!(app.status_item_chrome_lines()[0].contains(" fleet=off "));
    let mut windowed = crate::App::headless_for_test();
    windowed.headless = false;
    windowed.start_fleet_glance_with(Box::new(Empty), inert_seams());
    assert_eq!(windowed.fleet_glance.is_some(), cfg!(target_os = "macos"));
    if let Some(first) = windowed.fleet_glance.as_ref().map(|f| Arc::clone(&f.slot)) {
        windowed.start_fleet_glance_with(Box::new(Empty), inert_seams());
        let again = windowed.fleet_glance.as_ref().map(|f| Arc::clone(&f.slot));
        assert!(
            again.is_some_and(|slot| Arc::ptr_eq(&slot, &first)),
            "never twice"
        );
    }
}

fn glance_over(slot: FleetSlot) -> FleetGlance {
    let (kick, _) = std::sync::mpsc::channel();
    FleetGlance {
        slot: Arc::new(Mutex::new(slot)),
        kick,
        watch: Arc::new(AtomicBool::new(false)),
    }
}

/// The menu-bar item knows this Mac is in a fleet from the book the fleet glance last read,
/// and from nothing where no glance runs: the lock shows and the menu has its levels only then.
#[test]
fn the_status_glance_is_in_a_fleet_where_the_glances_book_is() {
    let mut app = crate::App::headless_for_test();
    assert!(!app.status_glance().in_fleet, "no glance");
    app.fleet_glance = Some(glance_over(FleetSlot::default()));
    assert!(!app.status_glance().in_fleet, "a book with no fleet");
    app.fleet_glance = Some(glance_over(slot_of(0, Vec::new(), Vec::new())));
    let glance = app.status_glance();
    assert!(glance.in_fleet);
    assert_eq!(
        glance.button_face().lock,
        Some(glance.access_shown),
        "the lock shows"
    );
}

/// A fleet read owes the menu-bar item a rebuild whether or not the Network State band is drawn;
/// a drawn band's next service turn is due at once, and a band nobody draws keeps the poll it had.
#[test]
fn a_fleet_read_dirties_the_status_item_and_calls_a_drawn_band_now() {
    let mut app = crate::App::headless_for_test();
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    let due = std::time::Instant::now() + std::time::Duration::from_secs(60);

    assert!(!app.network_band_drawn());
    app.network_next_poll = Some(due);
    let _ = app.take_status_rebuild();
    app.on_fleet_read();
    assert!(
        app.take_status_rebuild(),
        "a fleet read dirties the status item"
    );
    assert_eq!(
        app.network_next_poll,
        Some(due),
        "a band nobody draws keeps its poll"
    );

    assert!(app.open_network_lens());
    assert!(app.network_band_drawn());
    app.network_next_poll = Some(due);
    let _ = app.take_status_rebuild();
    app.on_fleet_read();
    assert!(
        app.take_status_rebuild(),
        "a fleet read dirties the status item with the band drawn"
    );
    assert_eq!(
        app.network_next_poll, None,
        "a drawn band reads the new slot on its next turn"
    );
}

#[test]
fn a_bracketed_address_is_named_whole() {
    assert_eq!(short_name("[::1]:7100"), "::1");
    assert_eq!(short_name("[::ffff:127.0.0.1]:7100"), "::ffff:127.0.0.1");
}

fn mac17(read_at_ms: u64, need: bool) -> FleetMac {
    FleetMac {
        name: "mac-17".to_string(),
        dial: "mac-17.local".to_string(),
        node: Some("n17".to_string()),
        needs: need
            .then(|| FleetNeed {
                sid: "t000000000w".to_string(),
                word: NeedWord::Stalled,
                who: "claude".to_string(),
                age_ms: Some(1_000),
            })
            .into_iter()
            .collect(),
        read_at_ms,
        ..FleetMac::default()
    }
}

fn silent(dial: &str) -> Unanswered {
    Unanswered {
        name: short_name(dial),
        dial: dial.to_string(),
        why: "no connection in 3 s".to_string(),
        ..Unanswered::default()
    }
}

fn slot_of(read_at_ms: u64, macs: Vec<FleetMac>, unanswered: Vec<Unanswered>) -> FleetSlot {
    FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(FleetSnapshot {
            read_at_ms,
            macs,
            unanswered,
            seats: None,
        })),
        book: BookFacts { in_fleet: true },
        ..FleetSlot::default()
    }
}

#[test]
fn a_read_carries_a_silent_mac_while_its_rows_count() {
    let t0 = 1_000_000;
    let fresh = millis(COUNT_FRESH);
    let mut busy = mac17(0, false);
    busy.dial = "mac-busy".to_string();
    let next = |at: u64| FleetSnapshot {
        read_at_ms: at,
        macs: vec![FleetMac {
            read_at_ms: at,
            ..busy.clone()
        }],
        unanswered: vec![silent("mac-17.local")],
        seats: None,
    };
    let first = FleetSnapshot {
        read_at_ms: t0,
        macs: vec![mac17(t0, true)],
        ..FleetSnapshot::default()
    };
    let carried = carry(Some(&first), next(t0 + 60_000), t0 + 60_000);
    assert_eq!(carried.macs.len(), 2);
    let m17 = &carried.macs[1];
    assert!(m17.carried && m17.read_at_ms == t0 && m17.needs.len() == 1);
    assert_eq!((carried.answered(), carried.unanswered.len()), (1, 1));
    assert_eq!(
        carried.next_freshness_change(t0 + 60_000),
        Some(t0 + fresh + 1)
    );
    let again = carry(Some(&carried), next(t0 + 120_000), t0 + 120_000);
    assert_eq!(again.macs.len(), 2, "carried through a second miss");
    assert_eq!(again.macs[1].read_at_ms, t0);
    let lapsed = carry(Some(&again), next(t0 + fresh + 1), t0 + fresh + 1);
    assert_eq!(lapsed.macs.len(), 1, "past its own read's freshness");
    assert_eq!(carry(None, next(t0), t0).macs.len(), 1);
}

#[test]
fn future_fleet_reads_are_uncounted_and_repolled_after_a_clock_rollback() {
    let now = 1_000_000;
    let at = now + 60_000;
    let previous = FleetSnapshot {
        read_at_ms: at,
        macs: vec![mac17(at, true)],
        ..FleetSnapshot::default()
    };
    assert!(!previous.fresh(now), "a future read is not fresh");
    assert!(!previous.macs[0].fresh(now), "neither are its peer rows");
    assert_eq!(previous.next_freshness_change(now), Some(at));
    assert_eq!(
        previous.next_freshness_change(at),
        Some(at + millis(COUNT_FRESH) + 1)
    );
    let next = FleetSnapshot {
        read_at_ms: now,
        unanswered: vec![silent("mac-17.local")],
        ..FleetSnapshot::default()
    };
    assert!(carry(Some(&previous), next, now).macs.is_empty());

    let slot = slot_of(at, previous.macs.clone(), Vec::new());
    assert!(should_poll(&slot.book, true, Some(at), now, POLL_EVERY));
    assert!(!should_poll(&slot.book, false, Some(at), now, POLL_EVERY));

    let fresh = millis(COUNT_FRESH);
    assert!(previous.fresh(at));
    assert!(previous.fresh(at + fresh));
    assert!(!previous.fresh(at + fresh + 1));
}

#[test]
fn a_hold_this_mac_placed_is_read_back_whatever_its_need() {
    let mine = "0123456789abcdef0123456789abcdef";
    let theirs = "fedcba9876543210fedcba9876543210";
    let held = |sid: &'static str, node: &'static str| {
        vec![
            ("sid", sid),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "idle"),
            ("role", "review"),
            ("needs", "-"),
            ("hold_by", "remote"),
            ("held_from", node),
        ]
    };
    let text = format!(
        "[{}]",
        peer(
            "lab-mini.local",
            &pairs(&header("n4")),
            &[&held("s-a", mine), &held("s-b", theirs), QUIET]
        )
    );
    let snap = parse(&text, Some(mine), 0).expect("a survey");
    assert_eq!(
        snap.macs[0].held_here,
        [FleetHeld {
            sid: "s-a".to_string(),
            who: "review".to_string(),
        }]
    );
    assert!(snap.macs[0].needs.is_empty(), "a hold is no need");
    let blind = parse(&text, None, 0).expect("a survey");
    assert!(blind.macs[0].held_here.is_empty(), "{:?}", blind.macs[0]);
}

/// A proven peer's fleet access is its header's `access=`: a header with none is an aterm from
/// before the lock (Full Access), a word this build does not know is Read Only, and a peer
/// whose key is not proven is read as no level at all, whatever it says.
#[test]
fn the_glance_asks_the_lan_door_only_for_a_read_it_makes() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let looks = Arc::new(AtomicUsize::new(0));
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let (ticks_tx, ticks) = std::sync::mpsc::channel();
    let counted = Arc::clone(&looks);
    let _glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            sharing: Box::new(|| true),
            lan: Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                true
            }),
            book: Box::new(move || {
                let _ = ticks_tx.send(());
                BookFacts { in_fleet: true }
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    runs.recv_timeout(hang).expect("the first read");
    while ticks.try_recv().is_ok() {}
    // 25 ticks within the read's quiet pause: none reads.
    for _ in 0..25 {
        ticks.recv_timeout(hang).expect("the reader ticks again");
    }
    let made = 1 + runs.try_iter().count();
    let asked = looks.load(Ordering::SeqCst);
    assert!(
        asked <= made,
        "the glance went through the LAN door {asked} times for {made} reads"
    );
}

/// THE GLANCE READS THIS MAC'S COPY AND STARTS NOTHING (one-mesh §4.12, §4.14, §11 B9): with the
/// mesh on, the glance asks the node service's view socket for each seat's standing, presence
/// row and now/needs row. An up seat's row names a stalled worker of a manager and a session this
/// Mac holds, under the seat's build; a silent seat (whose stale row asks something) is listed
/// as not answering, its need never read; and no child starts while the glance reads. The survey's read, counted the same way, starts one: the count is no
/// count of nothing.
#[cfg(unix)]
#[test]
fn the_glance_spawns_no_child_process_and_reads_a_fixtures_now_needs_rows() {
    use aterm_mesh::presence::{Behind, Build, PresenceRow, Seat, Standing};
    use aterm_mesh::view::View;
    use aterm_metrics::row::{HeldEntry, ManagerEntry, NeedEntry, NeedsRow, Sid, Word};
    use aterm_metrics::subject::Node;
    const NOW: u64 = 1_800_000_000_000;
    let state = aterm_tempfile::Builder::new()
        .prefix("ag")
        .tempdir_in("/tmp")
        .expect("a scratch state dir");
    let mesh = state.path().join(aterm_mesh::logs::MESH_DIR);
    std::fs::create_dir_all(&mesh).expect("the mesh folder");
    let principal = |n: u8| format!("n-{}", aterm_codec::hex::encode_lower(&[n; 8]));
    let seat = |n: u8, standing: Standing| Seat {
        principal: principal(n),
        key: [n; 32],
        me: n == 1,
        standing,
        behind: Behind::InStep,
        row: Some((
            3,
            PresenceRow {
                at_ms: NOW - 1_000,
                build: Build::new("0.159.0+g4ef9cd6144").expect("a build"),
                addresses: vec!["192.0.2.7:7100".parse().expect("an address")],
                membership: None,
                tables: None,
                topics: Vec::new(),
            },
        )),
    };
    let sid = |text: &str| Sid::parse(text).expect("a sid");
    let (worker, manager, held) = (sid("t0123456789"), sid("taaaaaaaaaa"), sid("tbbbbbbbbbb"));
    let up_needs = NeedsRow {
        at: NOW / 1000 - 120,
        needs: vec![NeedEntry {
            sid: worker,
            word: Word::new("stalled").expect("a word"),
            worker_of: Some(manager),
            harness: false,
            since: Some(NOW / 1000 - 120),
        }],
        held: vec![HeldEntry {
            sid: held,
            hold: Word::new("drive").expect("a word"),
            by: Some(Node::parse(&principal(1)).expect("this Mac's node")),
            since: None,
        }],
        managers: vec![ManagerEntry {
            sid: manager,
            workers: 1,
        }],
    };
    let silent_needs = NeedsRow {
        at: NOW / 1000 - 4 * 3_600,
        needs: vec![NeedEntry {
            sid: sid("tcccccccccc"),
            word: Word::new("ask").expect("a word"),
            worker_of: None,
            harness: false,
            since: None,
        }],
        held: Vec::new(),
        managers: Vec::new(),
    };
    let view = View {
        me: principal(1),
        at_ms: NOW,
        standings: true,
        seats: vec![
            seat(1, Standing::Up),
            seat(2, Standing::Up),
            seat(
                3,
                Standing::Silent {
                    last_seen_ms: Some(NOW - 3 * 3_600_000),
                },
            ),
        ],
        needs: Some(
            [(principal(2), up_needs), (principal(3), silent_needs)]
                .into_iter()
                .collect(),
        ),
        activity: None,
    };
    let served =
        aterm_mesh::view::serve_fixture(&mesh, view.clone()).expect("the node's view served");

    // No vault: the copy reads no membership log, so every seat is the view's, unlabelled.
    let mut copy = runner(true, Some(state.path().to_path_buf()), None);
    assert!(!copy.dials(), "a read of this Mac's copy dials no Mac");
    let (read, spawned) = aterm_uds::spawnfd::spawns_during(|| copy.read(None, &|| NOW));
    drop(served);
    assert_eq!(spawned, 0, "the glance's read started a child");
    let snapshot = read.expect("the copy reads");
    let up = principal(2);
    assert_eq!(snapshot.macs.len(), 1, "{snapshot:?}");
    let mac = &snapshot.macs[0];
    assert_eq!(
        (mac.name.as_str(), mac.node.as_deref()),
        (up.as_str(), Some(up.as_str()))
    );
    assert_eq!(mac.build.as_deref(), Some("0.159.0+g4ef9cd6144"));
    assert_eq!(
        mac.needs,
        vec![FleetNeed {
            sid: worker.to_string(),
            word: NeedWord::Stalled,
            who: crate::presence::short_sid(worker.as_str()),
            age_ms: Some(120_000),
        }]
    );
    assert_eq!(
        mac.held_here
            .iter()
            .map(|h| h.sid.as_str())
            .collect::<Vec<_>>(),
        vec![held.to_string().as_str()]
    );
    assert_eq!(
        mac.managers
            .iter()
            .map(|m| (m.sid.as_str(), m.workers))
            .collect::<Vec<_>>(),
        vec![(manager.to_string().as_str(), 1)]
    );
    assert_eq!(
        snapshot
            .unanswered
            .iter()
            .map(|u| (u.name.as_str(), u.why.as_str()))
            .collect::<Vec<_>>(),
        vec![(principal(3).as_str(), "silent for 3 h")]
    );
    assert_eq!(
        snapshot.seats.as_ref().map(|s| (s.members, s.up)),
        Some((3, Some(2)))
    );

    // A view whose node has not read the standings, or could not read the needs rows, is a
    // read that failed: the last good one stands, and no up seat reads as needing nothing.
    for (unready, why) in [
        (
            View {
                standings: false,
                ..view.clone()
            },
            "standings",
        ),
        (
            View {
                needs: None,
                ..view.clone()
            },
            "needs rows",
        ),
    ] {
        let served = aterm_mesh::view::serve_fixture(&mesh, unready).expect("served");
        let read = copy.read(None, &|| NOW);
        drop(served);
        assert!(
            read.as_ref().is_err_and(|e| e.contains(why)),
            "{why}: {read:?}"
        );
    }

    let slot = FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(snapshot)),
        book: BookFacts { in_fleet: true },
        ..FleetSlot::default()
    };
    // `status`'s members= reads the copy's seats while the read stands, never once it failed or
    // went stale.
    let glance = glance_over(slot.clone());
    assert_eq!(
        glance.seats(NOW).map(|s| (s.members, s.up)),
        Some((3, Some(2)))
    );
    assert_eq!(
        glance.seats(NOW + millis(COUNT_FRESH) + 1),
        None,
        "a stale read"
    );
    let failed = glance_over(FleetSlot {
        error: Some("the node could not read the needs rows".to_owned()),
        ..slot.clone()
    });
    assert_eq!(failed.seats(NOW), None, "a failed last read");
    let snapshot = slot.snapshot.as_deref().expect("the read");
    assert!(
        snapshot
            .macs
            .iter()
            .flat_map(|mac| &mac.needs)
            .all(|need| !need.sid.contains("cccc")),
        "a silent seat's need is never read: {snapshot:?}"
    );
    let mut survey = ChildRunner {
        program: PathBuf::from("/bin/echo"),
        args: vec!["[]".to_string()],
        cap: Duration::from_secs(60),
        sighted: |_| {},
    };
    assert!(survey.dials());
    let (surveyed, spawned) = aterm_uds::spawnfd::spawns_during(|| survey.read(None, &|| NOW));
    assert!(surveyed.is_ok(), "{surveyed:?}");
    assert_eq!(spawned, 1, "the survey's read starts its child");
}

/// AN AGENT'S OWN WORD IS KEPT (Fleet Field §6, B2 #7): a Mac's agents tally by their rows'
/// `agent=` words, so an agent that asks or sits at a wall is never counted at work, a prompt its
/// harness answers (`needs=-`) is at work as `aterm ctl survey` classes it, and a shell is a
/// session and no agent.
#[test]
fn tally_keeps_agent_words() {
    fn agent<'a>(sid: &'a str, word: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("sid", sid),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", word),
            ("needs", "-"),
        ]
    }
    let words = [
        "busy",
        "prompt",
        "question",
        "idle",
        "wall:usage-weekly",
        "wall:auth",
        "unknown",
        "zzz",
        "prompt",
    ];
    let sids = [
        "s-a", "s-b", "s-c", "s-d", "s-e", "s-f", "s-g", "s-h", "s-i",
    ];
    let mut rows: Vec<Vec<(&str, &str)>> =
        sids.iter().zip(words).map(|(s, w)| agent(s, w)).collect();
    rows[0].extend([
        ("task", "exited"),
        ("task_conclusion", "failed"),
        ("ctx_left_pct", "42"),
        ("usage_7d_pct", "97"),
    ]);
    // The first prompt and the question name their need for a person; the last prompt's row
    // read its needs and found none (`needs=-`).
    rows[1][5].1 = "approval";
    rows[2][5].1 = "question";
    rows[3].push(("task", "done"));
    rows.push(QUIET.to_vec());
    let read = |rows: &[Vec<(&str, &str)>]| {
        let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
        let json = format!("[{}]", peer("m7", &pairs(&header("n7")), &refs));
        parse(&json, None, 1_000).expect("a survey")
    };
    let snap = read(&rows);
    let mac = &snap.macs[0];
    let tally = mac.activity.expect("its agents were read");
    assert_eq!(
        tally,
        AgentTally {
            agents: 9,
            busy: 2,
            idle: 1,
            waiting: 2,
            walled: 2,
            unknown: 2,
        }
    );
    assert_eq!(mac.agent_sessions[8].agent, AgentWord::Busy);
    // Negative control: the same prompt once its row names a need waits on a person.
    let mut asked = rows.clone();
    asked[8][5].1 = "approval";
    let asked = read(&asked).macs[0].activity.expect("read");
    assert_eq!((asked.busy, asked.waiting), (1, 3));
    // The shell row is a session and no agent.
    assert_eq!(mac.agent_sessions.len(), 9);
    assert!(mac.agent_sessions.iter().all(|s| s.sid != "t000000000q"));
    let first = &mac.agent_sessions[0];
    assert_eq!(
        (
            first.agent.clone(),
            first.task,
            first.ctx_left_pct,
            first.usage_5h_pct,
            first.usage_7d_pct
        ),
        (
            AgentWord::Busy,
            Some(TaskWord::Failed),
            Some(42),
            None,
            Some(97)
        )
    );
    assert_eq!(mac.agent_sessions[3].task, Some(TaskWord::Done));
    assert_eq!(
        mac.agent_sessions[4].agent,
        AgentWord::Walled("usage-weekly".to_owned())
    );
    assert_eq!(mac.agent_sessions[7].agent, AgentWord::Unknown);
    assert_eq!(mac.sessions_more, 0);

    // Past the per-Mac cap the rest are counted, never kept; the tally holds every one.
    let many: Vec<String> = (0..SESSIONS_MAX + 3).map(|n| format!("s-{n}")).collect();
    let rows: Vec<Vec<(&str, &str)>> = many.iter().map(|sid| agent(sid, "busy")).collect();
    let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let json = format!("[{}]", peer("m8", &pairs(&header("n8")), &refs));
    let snap = parse(&json, None, 1_000).expect("a survey");
    let mac = &snap.macs[0];
    assert_eq!(
        (mac.agent_sessions.len(), mac.sessions_more),
        (SESSIONS_MAX, 3)
    );
    assert_eq!(
        mac.activity.map(|t| usize::from(t.busy)),
        Some(SESSIONS_MAX + 3)
    );

    // An older aterm's agents are not read: absent, never zero.
    let older: &[(&str, &str)] = &[
        ("sid", "t0000000010"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
    ];
    let json = format!("[{}]", peer("m9", &pairs(&header("n9")), &[older]));
    let snap = parse(&json, None, 1_000).expect("a survey");
    assert_eq!(snap.macs[0].activity, None);
}

/// THE GLANCE READS THE PRODUCER'S REASON TOKEN (Fleet Field §10, B2 #11, the GUI half): every
/// token aterm-net types round-trips through [`Reach::parse`]; a token this build does not know,
/// or none, is [`Reach::Unread`] with the client's words kept; and two Macs that did not answer
/// in time at different addresses share one reason, whatever their words.
#[cfg(unix)]
#[test]
fn reasons_typed_by_producer() {
    for token in aterm_net::fleet_member::REASONS {
        assert_eq!(Reach::parse(token).token(), Some(*token), "{token}");
    }
    let row = |label: &str, why: &str, reason: Option<&str>| {
        reason.map_or_else(
            || unanswered(label, why),
            |reason| {
                format!(
                    "{{\"label\":\"{label}\",\"fleet\":true,\"error\":\"{why}\",\
                     \"reason\":\"{reason}\"}}"
                )
            },
        )
    };
    let json = format!(
        "[{}]",
        [
            row(
                "m1",
                "192.0.2.1:7100 did not answer in 3 s",
                Some("no-answer")
            ),
            row(
                "m2",
                "192.0.2.2:7100 did not answer in 3 s",
                Some("no-answer")
            ),
            row("m3", "a reason from a later build", Some("zzz-later")),
            row("m4", "an older peer's words", None),
            row(
                "m5",
                "192.0.2.5:7100 refused the connection",
                Some("nothing-listens")
            ),
        ]
        .join(",")
    );
    let snap = parse(&json, None, 1_000).expect("a survey");
    let reasons: Vec<(&str, Reach, &str)> = snap
        .unanswered
        .iter()
        .map(|u| (u.name.as_str(), u.reason, u.why.as_str()))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (
                "m1",
                Reach::NoAnswer,
                "192.0.2.1:7100 did not answer in 3 s"
            ),
            (
                "m2",
                Reach::NoAnswer,
                "192.0.2.2:7100 did not answer in 3 s"
            ),
            ("m3", Reach::Unread, "a reason from a later build"),
            ("m4", Reach::Unread, "an older peer's words"),
            (
                "m5",
                Reach::NothingListens,
                "192.0.2.5:7100 refused the connection"
            ),
        ]
    );
    // The two that did not answer group as one: their reason is one, their words are not.
    assert_ne!(snap.unanswered[0].why, snap.unanswered[1].why);
    let groups: std::collections::BTreeSet<Option<&str>> =
        snap.unanswered.iter().map(|u| u.reason.token()).collect();
    assert_eq!(groups.len(), 3, "{groups:?}");
    // The glance's own reasons name no producer token.
    for own in [
        Reach::Silent { since_ms: 1 },
        Reach::NeverUp,
        Reach::PastCap,
        Reach::NoHeader,
        Reach::Unread,
    ] {
        assert_eq!(own.token(), None, "{own:?}");
    }
    let headless = "[{\"label\":\"m6\",\"fleet\":true}]";
    let snap = parse(headless, None, 1_000).expect("a survey");
    assert_eq!(snap.unanswered[0].reason, Reach::NoHeader);
}

/// THIS MAC'S COPY JOINS `aterm fleet`'S ROWS (Fleet Field §11, B2 #13): each seat of the view
/// takes its row's log label, build (the book's where it has no presence row), update and tables,
/// and the snapshot's seats are the composition's whole; a seat whose node no row names keeps
/// its book host's name and no label, and a seat of the log the view does not hold is counted as
/// never up.
#[cfg(unix)]
#[test]
fn snapshot_of_joins_seat_rows() {
    use aterm_agent::fleet_view::{Fleet, SeatRow};
    use aterm_mesh::presence::{Behind, Build, PresenceRow, Seat, Standing};
    use aterm_mesh::view::View;
    use aterm_netplace::words::{InStep, Seats};
    const NOW: u64 = 1_800_000_000_000;
    let principal = |n: u8| format!("n-{}", aterm_codec::hex::encode_lower(&[n; 8]));
    let presence = |build: &str| PresenceRow {
        at_ms: NOW - 1_000,
        build: Build::new(build).expect("a build"),
        addresses: vec!["192.0.2.7:7100".parse().expect("an address")],
        membership: None,
        tables: None,
        topics: Vec::new(),
    };
    let lag = Behind::Behind {
        at_least: Duration::from_secs(240),
    };
    let seat = |n: u8, standing: Standing, row: Option<PresenceRow>| Seat {
        principal: principal(n),
        key: [n; 32],
        me: n == 1,
        standing,
        behind: Behind::InStep,
        row: row.map(|row| (3, row)),
    };
    let view = View {
        me: principal(1),
        at_ms: NOW,
        standings: true,
        seats: vec![
            seat(1, Standing::Up, Some(presence("0.160.0"))),
            seat(2, Standing::Up, Some(presence("0.156.0"))),
            seat(
                3,
                Standing::Silent {
                    last_seen_ms: Some(NOW - 3_600_000),
                },
                None,
            ),
            seat(4, Standing::Up, Some(presence("0.160.0"))),
        ],
        needs: Some(std::collections::BTreeMap::new().into_iter().collect()),
        activity: None,
    };
    let seat_row = |label: &str, n: Option<u8>, build: Option<&str>| SeatRow {
        label: label.to_owned(),
        name: format!("{label}.local"),
        node: n.map(principal),
        node_key: n.map(|n| [n; 32]),
        me: n == Some(1),
        presence: None,
        build: build.map(str::to_owned),
        needs_update: false,
        behind: None,
    };
    let seats = Seats {
        members: 5,
        up: Some(3),
        needs_update: vec![("m4".to_owned(), "0.147.0".to_owned())],
        in_step: Some(InStep {
            with: 1,
            of: 2,
            slowest_ms: Some(240_000),
            apart: 0,
        }),
        log: Some((5000, 65536)),
    };
    let fleet = Fleet {
        seats: seats.clone(),
        rows: vec![
            seat_row("m1", Some(1), Some("0.160.0")),
            SeatRow {
                needs_update: true,
                behind: Some(lag),
                ..seat_row("m22", Some(2), Some("0.156.0"))
            },
            // No presence row: its build is its signed book entry's, as the composition gave it.
            SeatRow {
                needs_update: true,
                ..seat_row("m4", Some(3), Some("0.147.0"))
            },
            seat_row("m9", None, Some("0.150.0")),
        ],
        unserved: None,
        observations: Default::default(),
    };
    let book_peer = |n: u8, host: &str| aterm_net::fleet_member::Peer {
        node: None,
        key: Some(aterm_net::fleet::NodeKey::from_bytes([n; 32])),
        key_from: None,
        host: host.to_owned(),
        addr: None,
        port: 7100,
        version: None,
        seen_ms: None,
        via: aterm_net::fleet_member::Via::Joined,
        fleet: None,
        signed: None,
    };
    let book = [
        book_peer(2, "mac-22.local"),
        book_peer(3, "mac-4.local"),
        book_peer(4, "studio.local"),
    ];
    let snap = snapshot_of(&view, &fleet, &book, None, NOW);
    assert_eq!(
        snap.seats.as_ref(),
        Some(&seats),
        "the composition's seats, whole"
    );

    let m22 = snap
        .macs
        .iter()
        .find(|m| m.node.as_deref() == Some(principal(2).as_str()))
        .expect("m22 is up");
    assert_eq!(
        (
            m22.name.as_str(),
            m22.label.as_deref(),
            m22.build.as_deref(),
            m22.needs_update,
            m22.tables
        ),
        ("mac-22", Some("m22"), Some("0.156.0"), true, Some(lag))
    );
    assert_eq!(m22.activity, None, "a view of no activity rows reads none");

    // Negative control: a seat whose node no row names keeps its book host's name, no label.
    let studio = snap
        .macs
        .iter()
        .find(|m| m.node.as_deref() == Some(principal(4).as_str()))
        .expect("studio is up");
    assert_eq!(
        (
            studio.name.as_str(),
            studio.label.as_deref(),
            studio.build.as_deref(),
            studio.needs_update,
            studio.tables
        ),
        ("studio", None, Some("0.160.0"), false, None)
    );
    assert_eq!(snap.macs.len(), 2, "this Mac is no seat: {:?}", snap.macs);

    let silent: Vec<_> = snap
        .unanswered
        .iter()
        .map(|u| {
            (
                u.name.as_str(),
                u.label.as_deref(),
                u.reason,
                u.build.as_deref(),
                u.silent_since_ms,
                u.why.as_str(),
                u.needs_update,
            )
        })
        .collect();
    assert_eq!(
        silent,
        vec![
            (
                "mac-4",
                Some("m4"),
                Reach::Silent {
                    since_ms: NOW - 3_600_000
                },
                Some("0.147.0"),
                Some(NOW - 3_600_000),
                "silent for 1 h",
                true,
            ),
            (
                "m9",
                Some("m9"),
                Reach::NeverUp,
                Some("0.150.0"),
                None,
                "silent, never seen up here",
                false,
            ),
        ]
    );

    // A log seat that bound no node key and names this Mac, or a Mac the view already stands
    // for, is that seat: no phantom never-up Mac.
    let mut unbound = fleet.clone();
    unbound.rows.push(seat_row("here", None, None));
    unbound.rows.push(SeatRow {
        name: "studio.local".to_owned(),
        ..seat_row("m7", None, None)
    });
    unbound.rows.push(SeatRow {
        name: "studio.office.local".to_owned(),
        ..seat_row("m10", None, None)
    });
    let snap = snapshot_of(&view, &unbound, &book, Some("here"), NOW);
    let distinct = snap
        .unanswered
        .iter()
        .find(|row| row.dial == "studio.office.local")
        .expect("a different full host is still its own unbound seat");
    assert_eq!(distinct.label.as_deref(), Some("m10"));
    assert_eq!(distinct.reason, Reach::NeverUp);
    let never: Vec<&str> = snap
        .unanswered
        .iter()
        .filter(|u| u.reason == Reach::NeverUp)
        .map(|u| u.name.as_str())
        .collect();
    assert_eq!(never, ["m9", "studio"]);
    // Only a bare LAN label has the .local alias: a multi-label host is a full host.
    let snap = snapshot_of(&view, &unbound, &book, Some("studio.office"), NOW);
    assert!(snap.unanswered.iter().any(|row| {
        row.dial == "studio.office.local"
            && row.label.as_deref() == Some("m10")
            && row.reason == Reach::NeverUp
    }));
    // Control: another Mac's host is a Mac never seen up.
    let snap = snapshot_of(&view, &unbound, &book, Some("elsewhere.local"), NOW);
    assert!(snap.unanswered.iter().any(|u| u.name == "here"));
}

/// THE GLANCE'S GENERATION IS READ WITHOUT A COPY (Fleet Field §12): the band's service turn
/// sees a new read, a failure and the fleet's book move by three words under the slot's lock.
#[test]
fn the_generation_moves_with_the_slot() {
    let slot = slot_of(1_000, vec![mac17(1_000, false)], Vec::new());
    let glance = glance_over(slot.clone());
    assert_eq!(glance.generation(), (1, false, true));
    {
        let mut held = glance.slot.lock().expect("the slot");
        held.generation = 2;
        held.error = Some("the read failed".to_owned());
    }
    assert_eq!(glance.generation(), (2, true, true));
    let outside = glance_over(FleetSlot {
        book: BookFacts::default(),
        ..slot
    });
    assert_eq!(outside.generation(), (1, false, false));
}

/// A survey that refused the whole fleet's read writes one row for it, which names no Mac: the
/// read failed, and no seat is silent. Negative control: a Mac's own `not-dialled` row is a
/// silent seat.
#[test]
fn a_refused_fleet_read_is_no_seat() {
    let row = |reason: &str| {
        format!(
            "[{{\"label\":\"the fleet\",\"fleet\":true,\"error\":\"the book cannot be read\",\
             \"reason\":\"{reason}\"}}]"
        )
    };
    let refused = parse(&row(aterm_ctl::FLEET_UNREAD), None, 1_000);
    assert_eq!(
        refused,
        Err("fleet not read: the book cannot be read".to_owned())
    );
    let one = parse(&row("not-dialled"), None, 1_000).expect("a read");
    assert_eq!(one.unanswered.len(), 1);
    assert_eq!(one.unanswered[0].reason, Reach::NotDialled);
}

/// The survey rows of `rows` (each its `key=value` fields) as a window's survey answer, and as the
/// fleet read's JSON of one peer: the two forms one count reads.
#[cfg(unix)]
fn survey_forms(rows: &[Vec<(&str, &str)>]) -> (String, String) {
    let mut wire = format!(
        "OK {}\ninstance pid=77 host=mac.local sessions={} level=facts cut=0 node=n7\n",
        rows.len() + 1,
        rows.len()
    );
    for row in rows {
        let fields: Vec<String> = row
            .iter()
            .map(|(k, v)| format!("{k}={}", aterm_codec::pct::encode(v)))
            .collect();
        wire.push_str(&format!("session {}\n", fields.join(" ")));
    }
    let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let json = format!("[{}]", peer("m7", &pairs(&header("n7")), &refs));
    (wire, json)
}

/// One agent session's survey row.
#[cfg(unix)]
fn agent_row<'a>(sid: &'a str, word: &'a str, needs: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut row = vec![
        ("sid", sid),
        ("state", "running"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", word),
    ];
    row.extend(needs.map(|needs| ("needs", needs)));
    row
}

/// THE NODE COUNTS ITS WINDOWS' AGENTS AS A PEER'S SURVEY IS COUNTED (Fleet Field §11, §14): from
/// a window's survey answer, a `prompt` whose row read no need for a person (`needs=-`) is busy,
/// one naming a need (`needs=ask`) waits, as does a `question` whose `needs` names one; a wall is
/// walled with its kind; an exited agent and a shell are not counted; and the sessions are listed
/// by their minted sids and words. The fleet read's JSON of the same rows counts the same tally.
/// An answer that is no survey, and one a row of which sends no `needs` (an older window), count
/// nothing, as an older peer's rows do; so does a partial answer, which the row cannot mark as a
/// lower bound: a window left unread (`instances_read=1` of `instances=2`) or rows the reply cap
/// cut (`sessions=` past the rows sent). Every window read, the merged answer counts.
/// NEGATIVE CONTROL: flipping one prompt's `needs=-` to `ask` moves it from busy to waiting in
/// both counts alike.
#[cfg(unix)]
#[test]
fn the_nodes_count_of_its_windows_is_the_surveys_count() {
    use aterm_metrics::row::{AgentEntry, Sid, Word};
    let sids = [
        "t0000000001",
        "t0000000002",
        "t0000000003",
        "t0000000004",
        "t0000000005",
        "t0000000006",
    ];
    let mut rows = vec![
        agent_row(sids[0], "prompt", Some("-")),
        agent_row(sids[1], "prompt", Some("ask")),
        agent_row(sids[2], "question", Some("question")),
        agent_row(sids[3], "wall:usage-weekly", Some("wall")),
        agent_row(sids[4], "idle", Some("-")),
        agent_row("s-short", "busy", Some("-")),
        QUIET.to_vec(),
    ];
    let mut exited = agent_row(sids[5], "busy", Some("-"));
    exited[1].1 = "exited";
    rows.push(exited);
    let count = |rows: &[Vec<(&str, &str)>]| {
        let (wire, json) = survey_forms(rows);
        let row = activity_of_survey(&wire, 1_793_160_000).expect("a survey counts");
        let read = parse(&json, None, 1_000).expect("a fleet read");
        (row, read.macs[0].activity.expect("its agents were read"))
    };
    let (row, tally) = count(&rows);
    assert_eq!(row.at, 1_793_160_000);
    assert_eq!(
        (row.busy, row.idle, row.waiting, row.walled, row.unknown),
        (2, 1, 2, 1, 0)
    );
    assert_eq!(AgentTally::of_row(&row), tally, "the survey's own count");
    let entry = |n: usize, word: &str, detail: Option<&str>| AgentEntry {
        sid: Sid::parse(sids[n]).expect("a sid"),
        word: Word::new(word).expect("a word"),
        detail: detail.and_then(Word::new),
    };
    assert_eq!(
        row.sessions,
        vec![
            entry(0, "busy", None),
            entry(1, "waiting", None),
            entry(2, "waiting", None),
            entry(3, "walled", Some("usage-weekly")),
            entry(4, "idle", None),
        ],
        "a sid that is no minted one is counted, not listed"
    );
    let mut asked = rows.clone();
    asked[0][5].1 = "ask";
    let (row, tally) = count(&asked);
    assert_eq!((row.busy, row.waiting), (1, 3));
    assert_eq!(AgentTally::of_row(&row), tally);

    assert_eq!(activity_of_survey("ERR busy: ask again\n", 1), None);
    let mut older = rows.clone();
    older[4].truncate(5);
    let (wire, json) = survey_forms(&older);
    assert_eq!(activity_of_survey(&wire, 1), None, "an older window's rows");
    assert_eq!(
        parse(&json, None, 1_000).expect("read").macs[0].activity,
        None
    );
    let (wire, _) = survey_forms(&[]);
    assert_eq!(
        activity_of_survey(&wire, 5).map(|row| row.agents()),
        Some(0),
        "a window of no agents"
    );
    let (wire, _) = survey_forms(&rows);
    let marked = |from: &str, to: &str| wire.replacen(from, to, 1);
    assert_eq!(
        activity_of_survey(
            &marked(" node=n7", " node=n7 instances=2 instances_read=1"),
            1
        ),
        None,
        "a window left unread"
    );
    assert_eq!(
        activity_of_survey(&marked(" node=n7", " node=n7 instances=2"), 1),
        None,
        "a window that answered alone of two"
    );
    let claimed = format!(" sessions={} ", rows.len() + 1);
    assert_eq!(
        activity_of_survey(&marked(&format!(" sessions={} ", rows.len()), &claimed), 1),
        None,
        "rows the reply cap cut"
    );
    assert_eq!(
        activity_of_survey(
            &marked(" node=n7", " node=n7 instances=2 instances_read=2"),
            1
        )
        .map(|row| row.agents()),
        Some(6),
        "every window read"
    );
}

/// ON THIS MAC'S COPY, A SEAT'S ACTIVITY IS ITS FRESH `now/activity` ROW (Fleet Field §11, §14):
/// an up seat whose row trails its presence row by no more than `ACTIVITY_FRESH_MS` reads its
/// tally and its sessions by sid and word (none of their program, task or context), the agents
/// past those listed counted; the field draws its mark (busy, or a wall over busy). A seat with no
/// row, and one whose row trails its presence row a second past the bound, read no activity, and
/// draw no activity mark; each says why (unpublished, stale by how much), as a view whose node
/// read no activity rows does. The menu's row of a read seat counts its agents and no sessions,
/// which no copy reads.
/// NEGATIVE CONTROL: the survey path, which reads its sessions, still counts them on that row.
#[cfg(unix)]
#[test]
fn a_seats_fresh_activity_row_is_its_activity_on_the_copy() {
    use aterm_mesh::presence::{Behind, Build, PresenceRow, Seat, Standing};
    use aterm_mesh::view::View;
    use aterm_metrics::row::{ACTIVITY_FRESH_MS, ActivityRow, AgentEntry, Sid, Word};
    const NOW: u64 = 1_800_000_000_000;
    let principal = |n: u8| format!("n-{}", aterm_codec::hex::encode_lower(&[n; 8]));
    let seat = |n: u8| Seat {
        principal: principal(n),
        key: [n; 32],
        me: n == 1,
        standing: Standing::Up,
        behind: Behind::InStep,
        row: Some((
            3,
            PresenceRow {
                at_ms: NOW - 1_000,
                build: Build::new("0.160.0").expect("a build"),
                addresses: Vec::new(),
                membership: None,
                tables: None,
                topics: Vec::new(),
            },
        )),
    };
    let sid = Sid::parse("t0123456789").expect("a sid");
    let fresh = ActivityRow {
        at: (NOW - 1_000 - ACTIVITY_FRESH_MS) / 1000,
        busy: 3,
        walled: 1,
        idle: 1,
        sessions: vec![
            AgentEntry {
                sid,
                word: Word::new("busy").expect("a word"),
                detail: None,
            },
            AgentEntry {
                sid,
                word: Word::new("walled").expect("a word"),
                detail: Word::new("auth"),
            },
        ],
        ..ActivityRow::default()
    };
    let stale = ActivityRow {
        at: fresh.at - 1,
        ..fresh.clone()
    };
    let view = View {
        me: principal(1),
        at_ms: NOW,
        standings: true,
        seats: vec![seat(1), seat(2), seat(3), seat(4), seat(5)],
        needs: Some(std::collections::BTreeMap::new()),
        activity: Some(
            [
                (principal(2), fresh.clone()),
                (principal(4), stale),
                (
                    principal(5),
                    ActivityRow {
                        at: fresh.at,
                        busy: 2,
                        ..ActivityRow::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
        ),
    };
    let fleet = aterm_agent::fleet_view::Fleet {
        seats: aterm_netplace::words::Seats::default(),
        rows: Vec::new(),
        unserved: None,
        observations: Default::default(),
    };
    let mut snapshot = snapshot_of(&view, &fleet, &[], None, NOW);
    let mac = |snapshot: &FleetSnapshot, n: u8| {
        snapshot
            .macs
            .iter()
            .find(|m| m.node.as_deref() == Some(principal(n).as_str()))
            .cloned()
            .expect("an up seat")
    };
    let read = mac(&snapshot, 2);
    assert_eq!(
        read.activity,
        Some(AgentTally {
            agents: 5,
            busy: 3,
            idle: 1,
            waiting: 0,
            walled: 1,
            unknown: 0,
        })
    );
    assert_eq!(
        read.agent_sessions
            .iter()
            .map(|s| (s.sid.as_str(), s.agent.clone(), s.program.is_none(), s.task))
            .collect::<Vec<_>>(),
        vec![
            (sid.to_string().as_str(), AgentWord::Busy, true, None),
            (
                sid.to_string().as_str(),
                AgentWord::Walled("auth".to_owned()),
                true,
                None
            ),
        ]
    );
    assert_eq!(read.sessions_more, 3, "the agents past those listed");
    assert_eq!(mac(&snapshot, 3).activity, None, "no activity row");
    assert_eq!(mac(&snapshot, 4).activity, None, "a row past the bound");
    assert_eq!(
        [2, 3, 4].map(|n| mac(&snapshot, n).activity_absent),
        [
            None,
            Some(ActivityAbsent::Unpublished),
            Some(ActivityAbsent::Stale {
                behind_ms: Some(ACTIVITY_FRESH_MS + 1_000)
            }),
        ]
    );
    let unread = View {
        activity: None,
        ..view.clone()
    };
    assert_eq!(
        mac(&snapshot_of(&unread, &fleet, &[], None, NOW), 2).activity_absent,
        Some(ActivityAbsent::Unread)
    );

    // The field draws the read seat's mark, and none for the seats not read.
    snapshot.read_at_ms = NOW;
    let slot = FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(snapshot.clone())),
        book: BookFacts { in_fleet: true },
        ..FleetSlot::default()
    };
    let observed = aterm_types::network_state::NetworkObservation {
        visibility: aterm_types::network_state::Visibility::Active,
        history: aterm_types::network_state::HistoryState::Ready,
        ..aterm_types::network_state::NetworkObservation::default()
    };
    let field = crate::network_deck::field::project(Some(&slot), &observed, NOW);
    let mark = |n: u8| {
        let i = snapshot
            .macs
            .iter()
            .position(|m| m.node.as_deref() == Some(principal(n).as_str()))
            .expect("listed");
        field
            .seats
            .iter()
            .find(|seat| seat.src == crate::network_deck::field::SeatSrc::Mac(i))
            .expect("a seat")
            .activity
    };
    assert_eq!(
        mark(2),
        Some(crate::network_deck::field::Activity::Walled),
        "a wall outranks the busy agents"
    );
    assert_eq!(mark(5), Some(crate::network_deck::field::Activity::Busy(2)));
    assert_eq!(mark(3), None);
    assert_eq!(mark(4), None);
}

/// A need a person decides makes an agent wait whatever its own word, as `aterm ctl survey`
/// classes it: an idle agent that ended its turn on a question is waiting, not quiet (the live
/// fleet of 2026-11-06 held two such and four busy ones a harness reported). A stuck need keeps
/// the agent's own word, and `needs=-` keeps a prompt at work.
#[test]
fn a_decided_need_waits_whatever_the_agent_says() {
    for need in ["ask", "approval", "question"] {
        for word in ["idle", "busy", "prompt", "unknown"] {
            assert_eq!(
                AgentWord::of(Some(word), Some(need)),
                AgentWord::Waiting,
                "{word} with {need}"
            );
        }
    }
    assert_eq!(
        AgentWord::of(Some("idle"), Some("-")),
        AgentWord::Idle,
        "control"
    );
    assert_eq!(
        AgentWord::of(Some("busy"), Some("-")),
        AgentWord::Busy,
        "control"
    );
    assert_eq!(AgentWord::of(Some("prompt"), Some("-")), AgentWord::Busy);
    assert_eq!(
        AgentWord::of(Some("wall:auth"), Some("login")),
        AgentWord::Walled("auth".into()),
        "a stuck need is no decision"
    );
    assert_eq!(
        AgentWord::of(Some("unknown"), Some("stalled")),
        AgentWord::Unknown
    );
}

/// The nodes a lease reads as still in the fleet come only from a fresh, whole read, a Mac
/// carried from an earlier one among them (it missed one read, and its rows still count), and a
/// Mac that did not answer is not. NEGATIVE CONTROLS: a read with an answer it cannot name (an
/// unproven node, no header) or did not keep (past the cap, or a full read), a stale read, a
/// failed one, one ahead of the clock, and a Mac in no fleet say nothing.
#[test]
fn answered_nodes_come_only_from_a_fresh_whole_read_and_count_carried_macs() {
    let t0 = 1_000_000;
    let mac = |node: Option<&str>, carried: bool| FleetMac {
        node: node.map(str::to_owned),
        read_at_ms: if carried { t0 - 1 } else { t0 },
        carried,
        unproven: node.is_none(),
        ..FleetMac::default()
    };
    let answered = vec![mac(Some("N1"), false), mac(Some("n2"), true)];
    let slot = slot_of(t0, answered.clone(), vec![silent("mac-17.local")]);
    assert_eq!(
        answered_nodes(&slot, t0 + 1),
        Some(aterm_requests::Answered {
            read_at_ms: t0,
            nodes: ["n1".to_owned(), "n2".to_owned()].into(),
        })
    );
    let mut unproven = answered.clone();
    unproven.push(mac(None, false));
    assert_eq!(
        answered_nodes(&slot_of(t0, unproven, Vec::new()), t0 + 1),
        None,
        "an unproven Mac"
    );
    for reason in [Reach::PastCap, Reach::NoHeader] {
        let kept_out = Unanswered {
            reason,
            ..silent("mac-18.local")
        };
        assert_eq!(
            answered_nodes(&slot_of(t0, answered.clone(), vec![kept_out]), t0 + 1),
            None,
            "{reason:?}"
        );
    }
    let full: Vec<FleetMac> = (0..MACS_MAX)
        .map(|n| mac(Some(&format!("{n:032x}")), false))
        .collect();
    assert_eq!(
        answered_nodes(&slot_of(t0, full, Vec::new()), t0 + 1),
        None,
        "a full read"
    );
    assert_eq!(
        answered_nodes(&slot, t0 + millis(COUNT_FRESH) + 1),
        None,
        "stale"
    );
    assert_eq!(answered_nodes(&slot, t0 - 1), None, "ahead of the clock");
    let mut failed = slot.clone();
    failed.error = Some("no route".to_owned());
    assert_eq!(answered_nodes(&failed, t0 + 1), None, "failed");
    let mut outside = slot;
    outside.book.in_fleet = false;
    assert_eq!(answered_nodes(&outside, t0 + 1), None, "in no fleet");
}

/// THE SAME ANSWER, READ WHOLE FOR THE FLEET TABLES: `sightings` keeps this Mac's own windows'
/// replies as one machine and every agent session with its account word, model, state and
/// figures, where `parse` keeps the peers alone and reads the same answer as it always has.
#[cfg(unix)]
#[test]
fn sightings_keep_this_mac_and_every_agent_row_and_parse_is_unchanged() {
    use aterm_db::fleet::read::{Answer, Answered, Harness, How, MachineSeen, State, Tally, Task};

    fn local(pid: &str, header: &[(&str, &str)], rows: &[&[(&str, &str)]]) -> String {
        peer(&format!("pid {pid}"), header, rows).replace("\"fleet\":true,", "")
    }
    fn answered(seen: &MachineSeen) -> &Answered {
        match &seen.answer {
            Answer::Answered(answered) => answered,
            Answer::Silent { why } => panic!("{:?} was silent: {why:?}", seen.label),
        }
    }
    const WORKER: &[(&str, &str)] = &[
        ("sid", "s-aaaa0001"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("agent_ms", "42000"),
        ("task", "working"),
        ("model", "fable-5.2"),
        ("ctx_left_pct", "61"),
        ("usage_5h_pct", "12"),
        ("usage_7d_pct", "40"),
        ("account", "one@lab.example"),
        ("needs", "-"),
    ];
    const WALLED: &[(&str, &str)] = &[
        ("sid", "s-aaaa0002"),
        ("state", "alive"),
        ("program", "codex-alab"),
        ("program_kind", "agent"),
        ("agent", "wall:usage-weekly"),
        ("account", "codex-one"),
        ("needs", "wall"),
    ];
    const STALLED: &[(&str, &str)] = &[
        ("sid", "s-aaaa0003"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("task", "stalled"),
        ("task_ms", "900000"),
        ("needs", "stalled"),
    ];
    const GONE: &[(&str, &str)] = &[
        ("sid", "s-aaaa0004"),
        ("state", "exited"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("needs", "-"),
    ];
    const OLDER: &[(&str, &str)] = &[
        ("sid", "s-bbbb0001"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
    ];

    // This Mac: two windows' replies, both of node `own-node`, one session listed by both.
    let mut own_header = header("own-node");
    own_header.extend([
        ("version", "0.141.0".to_string()),
        ("label", "m3".to_string()),
        ("home_free_mb", "51234".to_string()),
    ]);
    let own_header = pairs(&own_header);
    let mut m7 = header("n7");
    m7.extend([
        ("version", "0.140.0".to_string()),
        ("label", "m7".to_string()),
    ]);
    let mut cut = header("ncut");
    cut[2].1 = "5".to_string();
    let mut liar = header("nliar");
    liar.push(("label", "m3".to_string()));
    let hosts = [
        local("1", &own_header, &[WORKER, QUIET]),
        local("2", &own_header, &[WORKER, WALLED]),
        // This Mac, answering its own fleet question: the same machine, named once.
        peer("m3.local at 10.0.0.3:7100", &own_header, &[WORKER]),
        peer(
            "m7.local at 10.0.0.7:7100",
            &pairs(&m7),
            &[STALLED, GONE, QUIET],
        ),
        peer("mac-old.local", &pairs(&header("nold")), &[OLDER]),
        peer("mac-cut.local", &pairs(&cut), &[QUIET, WORKER]),
        peer("m9.local", &pairs(&liar), &[QUIET])
            .replace("\"fleet\":true,", "\"fleet\":true,\"unproven\":true,"),
        "{\"label\":\"m12.local at 10.0.0.12:7100\",\"fleet\":true,\
          \"error\":\"did not answer in 3 s at 10.0.0.12\",\"reason\":\"nothing-listens\"}"
            .to_string(),
        "{\"label\":\"m13.local\",\"fleet\":true}".to_string(),
    ];
    let json = format!("[{}]", hosts.join(","));

    // `parse` reads what it read before: the peers, not this Mac, and no account.
    let glance = parse(&json, Some("own-node"), 1_000).expect("a survey");
    let dials: Vec<&str> = glance.macs.iter().map(|mac| mac.dial.as_str()).collect();
    assert_eq!(
        dials,
        ["m7.local", "mac-old.local", "mac-cut.local", "m9.local"]
    );
    let silent: Vec<&str> = glance
        .unanswered
        .iter()
        .map(|mac| mac.dial.as_str())
        .collect();
    assert_eq!(silent, ["m12.local", "m13.local"]);
    assert_eq!(
        glance.macs[0].activity,
        Some(AgentTally {
            agents: 1,
            idle: 1,
            ..AgentTally::default()
        })
    );

    let read = sightings(&json, 1_000).expect("a survey");
    assert_eq!(read.read_at_ms, 1_000);
    assert_eq!(
        parse(&json, Some("own-node"), 1_000).expect("a survey"),
        glance
    );

    // This Mac is the first machine: its windows' replies as one, each session once.
    let own = &read.machines[0];
    assert_eq!((own.how, own.id.as_str()), (How::Own, "self"));
    assert_eq!(own.label.as_deref(), Some("m3"));
    assert_eq!(
        read.machines
            .iter()
            .filter(|seen| seen.how == How::Own)
            .count(),
        1
    );
    let mine = answered(own);
    assert_eq!(mine.build.as_deref(), Some("0.141.0"));
    assert_eq!(mine.home_free_mb, Some(51_234));
    assert_eq!(
        mine.sessions,
        Some(3),
        "the worker both windows list is one session"
    );
    assert_eq!(
        mine.tally,
        Some(Tally {
            agents: 2,
            busy: 1,
            walled: 1,
            ..Tally::default()
        })
    );
    let sids: Vec<&str> = mine.agents.iter().map(|agent| agent.sid.as_str()).collect();
    assert_eq!(sids, ["s-aaaa0001", "s-aaaa0002"]);
    let worker = &mine.agents[0];
    assert_eq!(
        (worker.harness, worker.state),
        (Harness::Claude, State::Busy)
    );
    assert_eq!(worker.state_age_ms, Some(42_000));
    assert_eq!(worker.task, Some(Task::Working));
    assert_eq!(worker.model.as_deref(), Some("fable-5.2"));
    assert_eq!(
        (
            worker.ctx_left_pct,
            worker.usage_5h_pct,
            worker.usage_7d_pct
        ),
        (Some(61), Some(12), Some(40))
    );
    assert!(
        worker.account.is_some(),
        "its account word goes on to be resolved"
    );
    assert!(
        !format!("{read:?}").contains("lab.example"),
        "and prints nowhere"
    );
    let walled = &mine.agents[1];
    assert_eq!(
        (walled.harness, walled.state),
        (Harness::CodexAlab, State::Walled)
    );
    assert_eq!(walled.state_why.as_deref(), Some("usage-weekly"));
    assert_eq!((walled.model.as_deref(), walled.ctx_left_pct), (None, None));

    // Every peer is a machine, by the name the book dials: no address is kept.
    let peers: Vec<(Option<&str>, Option<&str>)> = read.machines[1..]
        .iter()
        .map(|seen| (seen.host.as_deref(), seen.label.as_deref()))
        .collect();
    assert_eq!(
        peers,
        [
            (Some("m7"), Some("m7")),
            (Some("mac-old"), None),
            (Some("mac-cut"), None),
            // It proved no node key this Mac binds: the `m3` it claims is not taken.
            (Some("m9"), None),
            (Some("m12"), None),
            (Some("m13"), None),
        ]
    );
    assert!(
        read.machines[1..]
            .iter()
            .all(|seen| seen.how == How::Survey)
    );
    assert!(read.fleet, "the fleet was asked");
    // The peer that proved no key is marked: what it says of itself is a claim.
    let unproven: Vec<Option<&str>> = read
        .machines
        .iter()
        .filter(|seen| seen.unproven)
        .map(|seen| seen.host.as_deref())
        .collect();
    assert_eq!(unproven, [Some("m9")]);
    assert!(!format!("{read:?}").contains("10.0.0."));
    let m7 = answered(&read.machines[1]);
    assert_eq!(m7.sessions, Some(2), "an exited session is no session");
    assert_eq!(m7.agents.len(), 1);
    assert_eq!(
        (m7.agents[0].state, m7.agents[0].task),
        (State::Stuck, Some(Task::Stalled)),
        "a stalled task says more than its agent's idle"
    );
    assert_eq!(
        m7.tally.map(|tally| (tally.agents, tally.stuck)),
        Some((1, 1))
    );
    // An older aterm's agents cannot be classed, and a cut answer is a lower bound: neither is
    // a count, though the rows they sent are still rows.
    let old = answered(&read.machines[2]);
    assert_eq!(
        (old.sessions, old.tally, old.agents.len()),
        (Some(1), None, 1)
    );
    let cut = answered(&read.machines[3]);
    assert_eq!((cut.sessions, cut.tally, cut.agents.len()), (None, None, 1));
    assert_eq!(
        read.machines[5].answer,
        Answer::Silent {
            why: Some("nothing-listens".to_owned())
        },
        "the reason token, never the prose that names an address"
    );
    assert_eq!(
        read.machines[6].answer,
        Answer::Silent {
            why: Some("no-header".to_owned())
        }
    );

    // Control: with no window of this Mac replying, no machine is this Mac, and its answer to
    // its own fleet question is then a peer like any other.
    let peers_only = format!("[{}]", hosts[2..].join(","));
    let read = sightings(&peers_only, 2_000).expect("a survey");
    assert!(read.machines.iter().all(|seen| seen.how == How::Survey));
    assert_eq!(read.machines.len(), 7);
    assert_eq!(read.machines[0].label.as_deref(), Some("m3"));

    // The row that stands for a refused read of the whole fleet names no machine.
    let fleet_unread = format!(
        "{{\"label\":\"the fleet\",\"fleet\":true,\"error\":\"this Mac is away\",\"reason\":\"{}\"}}",
        aterm_ctl::FLEET_UNREAD
    );
    assert!(sightings(&format!("[{fleet_unread}]"), 3_000).is_err());
    assert!(sightings("{}", 3_000).is_err());

    // Beside it, this Mac's own windows still answered: the read is this Mac alone, with
    // every session they listed, and says the fleet was not asked.
    let away = format!("[{},{},{fleet_unread}]", hosts[0], hosts[1]);
    let read = sightings(&away, 4_000).expect("this Mac's windows answered");
    assert!(!read.fleet, "no peer was asked");
    assert_eq!(read.machines.len(), 1);
    assert_eq!(read.machines[0].how, How::Own);
    let mine = answered(&read.machines[0]);
    assert_eq!((mine.sessions, mine.agents.len()), (Some(3), 2));
    assert_eq!(mine.tally.map(|tally| tally.agents), Some(2));

    // A LOCAL aterm PROCESS THAT DID NOT ANSWER: the sessions the others listed are a lower
    // bound, so this Mac has no session count and no tally, though the rows heard are rows.
    let hung = "{\"label\":\"pid 777\",\"error\":\"pid 777 did not answer\"}";
    let partial = sightings(&format!("[{},{hung}]", hosts[0]), 5_000).expect("a survey");
    assert!(partial.fleet);
    let mine = answered(&partial.machines[0]);
    assert_eq!(
        (mine.sessions, mine.tally, mine.agents.len()),
        (None, None, 1),
        "a lower bound is no count"
    );
    // Control: the same reply with no unanswered process beside it is a count.
    let whole = sightings(&format!("[{}]", hosts[0]), 5_000).expect("a survey");
    let mine = answered(&whole.machines[0]);
    assert_eq!(mine.sessions, Some(2));
    assert_eq!(mine.tally.map(|tally| tally.agents), Some(1));
    // With no window answering at all, an unanswered process is no machine.
    let none = sightings(&format!("[{hung}]"), 5_000).expect("a survey");
    assert!(none.machines.is_empty());
}

/// The answer a sighted machine gave.
#[cfg(unix)]
fn answer_of(seen: &aterm_db::fleet::read::MachineSeen) -> &aterm_db::fleet::read::Answered {
    match &seen.answer {
        aterm_db::fleet::read::Answer::Answered(answered) => answered,
        aterm_db::fleet::read::Answer::Silent { why } => panic!("silent: {why:?}"),
    }
}

/// A header of node `node` that carries its home volume's free space and size, as
/// `SwarmSurveyFacts::header_tokens` writes them, then a whole look, as
/// `MachineFacts::header_tokens` writes one, answered at `at_ms` on its own clock.
#[cfg(unix)]
fn look_header(node: &str, at_ms: u64) -> Vec<(&'static str, String)> {
    let mut head = header(node);
    head.extend([
        ("at_ms", at_ms.to_string()),
        ("home_free_mb", "51234".to_string()),
        ("home_total_mb", "953000".to_string()),
        ("os", "macos".to_string()),
        ("target", "aarch64-apple-darwin".to_string()),
        ("m_at_ms", (at_ms - 1_500).to_string()),
        ("cores", "12".to_string()),
        ("load1_x100", "1530".to_string()),
        ("load5_x100", "900".to_string()),
        ("load15_x100", "700".to_string()),
        ("cpu_busy_x100", "1140".to_string()),
        ("rate_win_ms", "61000".to_string()),
        ("mem_total_mb", "65536".to_string()),
        ("mem_used_mb", "40000".to_string()),
        ("mem_avail_mb", "25536".to_string()),
        ("mem_pressure", "warn".to_string()),
        ("mem_psi60_x100", "-".to_string()),
        ("swap_used_mb", "0".to_string()),
        ("swap_total_mb", "4096".to_string()),
        ("gpu_cores", "38".to_string()),
        ("gpu_busy_pct", "97".to_string()),
        ("net_link_kbps", "10000000".to_string()),
        ("net_rx_kbps", "81234".to_string()),
        ("net_tx_kbps", "2345".to_string()),
        ("net_other_kbps", "12".to_string()),
        ("up_since_ms", (at_ms - 3_600_000).to_string()),
        ("cpu_lim", "1".to_string()),
        ("mem_lim", "1".to_string()),
        ("disk_lim", "0".to_string()),
        ("gpu_lim", "1".to_string()),
        ("load", "88:21".to_string()),
        ("access", "full".to_string()),
    ]);
    head
}

/// The look [`look_header`] sends, answered at `at_ms`.
#[cfg(unix)]
fn whole_look(at_ms: u64) -> aterm_db::fleet::read::Look {
    use aterm_db::fleet::read::{Limits, Load, Look, Pressure};
    Look {
        age_ms: Some(1_500),
        cores: Some(12),
        load1_x100: Some(1_530),
        cpu_busy_x100: Some(1_140),
        rate_win_ms: Some(61_000),
        mem_total_mb: Some(65_536),
        mem_used_mb: Some(40_000),
        mem_avail_mb: Some(25_536),
        swap_used_mb: Some(0),
        swap_total_mb: Some(4_096),
        disk_total_mb: Some(953_000),
        gpu_cores: Some(38),
        gpu_busy_pct: Some(97),
        net_link_kbps: Some(10_000_000),
        net_rx_kbps: Some(81_234),
        net_tx_kbps: Some(2_345),
        net_other_kbps: Some(12),
        mem_pressure: Some(Pressure::Warn),
        limits: Limits {
            cpu: Some(true),
            mem: Some(true),
            disk: Some(false),
            gpu: Some(true),
        },
        load: Some(Load {
            level_x100: 88,
            over_x100: Some(21),
        }),
        up_since_own_ms: Some(at_ms - 3_600_000),
        up_for_ms: Some(3_600_000),
        os: Some("macos"),
        target: Some("aarch64-apple-darwin".to_owned()),
    }
}

/// THE FLEET READ KEEPS EACH MACHINE'S LOOK: every figure its header sends, a read zero as a
/// zero, and every figure it does not send (an older aterm, a `-`) unread, never 0. This Mac's
/// windows are one machine with one look: the first reply that carries one.
#[cfg(unix)]
#[test]
fn sightings_keep_a_machines_look_and_an_older_header_reads_unread() {
    use aterm_db::fleet::read::Look;
    const AT: u64 = 1_800_000_060_000;
    let local = |pid: &str, head: &[(&'static str, String)]| {
        peer(&format!("pid {pid}"), &pairs(head), &[QUIET]).replace("\"fleet\":true,", "")
    };
    let whole = look_header("n1", AT);
    // A newer aterm whose host reads no GPU and no rate yet: those figures are `-`.
    let mut dashes = look_header("n2", AT);
    for (key, value) in &mut dashes {
        if matches!(
            *key,
            "gpu_cores"
                | "gpu_busy_pct"
                | "cpu_busy_x100"
                | "rate_win_ms"
                | "cpu_lim"
                | "gpu_lim"
                | "load"
        ) {
            *value = "-".to_string();
        }
    }
    let mut older = header("n3");
    older.extend([
        ("at_ms", AT.to_string()),
        ("home_free_mb", "777".to_string()),
    ]);
    // A look whose every figure went unread on its machine: each is `-`, and only its age reads.
    let mut blank = look_header("n4", AT);
    for (key, value) in &mut blank {
        if !matches!(
            *key,
            "pid" | "host" | "sessions" | "level" | "cut" | "node" | "at_ms" | "m_at_ms" | "access"
        ) {
            *value = "-".to_string();
        }
    }
    let json = format!(
        "[{},{},{},{}]",
        peer("m1.local", &pairs(&whole), &[QUIET]),
        peer("m2.local", &pairs(&dashes), &[QUIET]),
        peer("m3.local", &pairs(&older), &[QUIET]),
        peer("m4.local", &pairs(&blank), &[QUIET]),
    );
    let read = sightings(&json, 9).expect("a survey");
    let looks: Vec<&Look> = read
        .machines
        .iter()
        .map(|seen| &answer_of(seen).look)
        .collect();
    assert_eq!(*looks[0], whole_look(AT));
    let mut unread_gpu = whole_look(AT);
    unread_gpu.gpu_cores = None;
    unread_gpu.gpu_busy_pct = None;
    unread_gpu.cpu_busy_x100 = None;
    unread_gpu.rate_win_ms = None;
    unread_gpu.limits.cpu = None;
    unread_gpu.limits.gpu = None;
    unread_gpu.load = None;
    assert_eq!(*looks[1], unread_gpu, "a `-` is unread, never 0");
    assert_eq!(*looks[2], Look::UNREAD, "an older aterm sends no look");
    let older = answer_of(&read.machines[2]);
    assert_eq!(
        (older.home_free_mb, older.access, older.drive),
        (Some(777), None, None),
        "what it does send still reads"
    );
    assert_eq!(
        *looks[3],
        Look {
            age_ms: Some(1_500),
            ..Look::UNREAD
        },
        "every figure unread on its machine is unread here, never 0"
    );

    // This Mac: an older window first, then two that carry a look. The first look is the
    // machine's; the one after it is not read over it.
    let mut second = look_header("own", AT + 20_000);
    second.retain(|(key, _)| *key != "cores");
    second.push(("cores", "64".to_string()));
    let mut own_older = header("own");
    own_older.push(("at_ms", AT.to_string()));
    let json = format!(
        "[{},{},{}]",
        local("1", &own_older),
        local("2", &look_header("own", AT)),
        local("3", &second),
    );
    let read = sightings(&json, 9).expect("a survey");
    assert_eq!(read.machines.len(), 1);
    assert_eq!(answer_of(&read.machines[0]).look, whole_look(AT));
}

/// A FIGURE IS A WHOLE NUMBER IN ITS RANGE OR UNREAD: a decimal, a negative, one past `u64`, a
/// percentage over 100, a word outside its closed set and a verdict other than `0` or `1` each
/// read as nothing, and every other figure of the same header still reads.
#[cfg(unix)]
#[test]
fn a_decimal_a_negative_or_an_out_of_range_figure_is_unread() {
    const AT: u64 = 1_800_000_060_000;
    let bent = |swaps: &[(&'static str, &str)]| {
        let mut head = look_header("n1", AT);
        for &(key, value) in swaps {
            head.retain(|(k, _)| *k != key);
            head.push((key, value.to_string()));
        }
        head
    };
    let heads = [
        bent(&[
            ("cores", "12.5"),
            ("load1_x100", "-3"),
            ("cpu_busy_x100", "1e3"),
            ("mem_total_mb", "18446744073709551616"),
            ("gpu_busy_pct", "101"),
            ("mem_pressure", "high"),
            ("os", "beos"),
            ("target", "me@example.com"),
            ("cpu_lim", "2"),
            ("mem_lim", "yes"),
            ("load", "88:101"),
        ]),
        // Control: the bounds themselves read.
        bent(&[
            ("gpu_busy_pct", "100"),
            ("mem_total_mb", "18446744073709551615"),
            ("mem_pressure", "critical"),
            ("os", "linux"),
            ("load", "100:100"),
        ]),
        bent(&[("gpu_busy_pct", "0"), ("swap_used_mb", "+5")]),
        // A path and a phrase (as `aterm ctl survey --json` decodes `%2F` and `%20`) are no
        // triple, whatever the generic word allows.
        bent(&[("target", "/home/example/work")]),
        bent(&[("target", "x86_64 is down")]),
        bent(&[("target", "Aarch64-apple-darwin")]),
    ];
    let entries: Vec<String> = heads
        .iter()
        .enumerate()
        .map(|(i, head)| peer(&format!("m{i}.local"), &pairs(head), &[QUIET]))
        .collect();
    let read = sightings(&format!("[{}]", entries.join(",")), 9).expect("a survey");
    let looks: Vec<_> = read
        .machines
        .iter()
        .map(|seen| answer_of(seen).look.clone())
        .collect();

    let mut want = whole_look(AT);
    want.cores = None;
    want.load1_x100 = None;
    want.cpu_busy_x100 = None;
    want.mem_total_mb = None;
    want.gpu_busy_pct = None;
    want.mem_pressure = None;
    want.os = None;
    want.target = None;
    want.limits.cpu = None;
    want.limits.mem = None;
    want.load = None;
    assert_eq!(looks[0], want);

    let mut bounds = whole_look(AT);
    bounds.gpu_busy_pct = Some(100);
    bounds.mem_total_mb = Some(u64::MAX);
    bounds.mem_pressure = Some(aterm_db::fleet::read::Pressure::Critical);
    bounds.os = Some("linux");
    bounds.load = Some(aterm_db::fleet::read::Load {
        level_x100: 100,
        over_x100: Some(100),
    });
    assert_eq!(looks[1], bounds);

    let mut zero = whole_look(AT);
    zero.gpu_busy_pct = Some(0);
    zero.swap_used_mb = None;
    assert_eq!(looks[2], zero, "a read 0 is 0; a sign is no digit");

    let mut no_target = whole_look(AT);
    no_target.target = None;
    for (at, look) in looks.iter().enumerate().skip(3) {
        assert_eq!(*look, no_target, "head {at}");
    }
}

/// THE FLEET READ KEEPS EACH MACHINE'S LOAD SCORE BESIDE ITS VERDICTS: `load=<level>:<over>`
/// reads as the machine's own dial in hundredths; `load=-` (a dial with no level), an older
/// header with no `load=` and a malformed one (`x:y`, one part, a part past 100) read as no
/// score, never 0, while the machine's `cpu_lim` and its other figures still read. A read
/// `0:0` is a machine at rest; `72:-` a level whose over its machine could not judge.
/// NEGATIVE CONTROLS (2026-11-13): with the key not read, the sent score reads unread; with a
/// malformed or unread score read as 0, `load=-` and `x:y` read as a machine at rest.
#[cfg(unix)]
#[test]
fn the_fleet_read_keeps_a_machines_load_score_beside_its_verdicts() {
    use aterm_db::fleet::read::Load;
    const AT: u64 = 1_800_000_060_000;
    let with_load = |node: &str, load: Option<&str>| {
        let mut head = look_header(node, AT);
        head.retain(|(key, _)| *key != "load");
        if let Some(load) = load {
            head.push(("load", load.to_string()));
        }
        head
    };
    let heads = [
        with_load("n0", Some("72:15")),
        with_load("n1", Some("0:0")),
        with_load("n2", Some("72:-")),
        with_load("n3", Some("-")),
        with_load("n4", None),
        with_load("n5", Some("x:y")),
        with_load("n6", Some("72")),
        with_load("n7", Some("72:15:1")),
        with_load("n8", Some("101:0")),
        with_load("n9", Some("-:15")),
    ];
    let entries: Vec<String> = heads
        .iter()
        .enumerate()
        .map(|(i, head)| peer(&format!("m{i}.local"), &pairs(head), &[QUIET]))
        .collect();
    let read = sightings(&format!("[{}]", entries.join(",")), 9).expect("a survey");
    let looks: Vec<_> = read
        .machines
        .iter()
        .map(|seen| answer_of(seen).look.clone())
        .collect();
    assert_eq!(looks.len(), heads.len());
    assert_eq!(
        looks[0].load,
        Some(Load {
            level_x100: 72,
            over_x100: Some(15),
        })
    );
    assert_eq!(
        looks[1].load,
        Some(Load {
            level_x100: 0,
            over_x100: Some(0),
        }),
        "a read 0 is a machine at rest"
    );
    assert_eq!(
        looks[2].load,
        Some(Load {
            level_x100: 72,
            over_x100: None,
        }),
        "an over the machine could not judge is unread beside its level, never 0"
    );
    for (at, look) in looks.iter().enumerate().skip(3) {
        assert_eq!(look.load, None, "head {at}: no score, never 0");
        assert_eq!(
            look.limits.cpu,
            Some(true),
            "head {at}: cpu_lim still reads"
        );
        let mut want = whole_look(AT);
        want.load = None;
        assert_eq!(*look, want, "head {at}: the rest as it was sent");
    }
}

/// A LOOK'S AGE IS TWO TIMES ON THE ANSWERER'S OWN CLOCK: its header's `at_ms=` less its
/// `m_at_ms=`, and how long it has been up its `at_ms=` less its `up_since_ms=`. This Mac's
/// clock (the read's time) enters neither, so a peer whose clock is hours off still reads its
/// true ages; an age that would be negative, or one with either time unread, is unread.
#[cfg(unix)]
#[test]
fn a_looks_age_is_the_answerers_own_clock_difference() {
    // The answerer's clock is about three hours behind this Mac's.
    const THEIRS: u64 = 1_800_000_000_000;
    const OURS: u64 = THEIRS + 3 * 3_600_000 + 17;
    let set = |head: &mut Vec<(&'static str, String)>, key: &'static str, value: String| {
        head.retain(|(k, _)| *k != key);
        head.push((key, value));
    };
    let mut ahead = look_header("n2", THEIRS);
    set(&mut ahead, "m_at_ms", (THEIRS + 1).to_string());
    set(&mut ahead, "up_since_ms", (THEIRS + 5).to_string());
    let mut timeless = look_header("n3", THEIRS);
    timeless.retain(|(key, _)| *key != "at_ms");
    let mut unbooted = look_header("n4", THEIRS);
    set(&mut unbooted, "up_since_ms", "0".to_string());
    let heads = [look_header("n1", THEIRS), ahead, timeless, unbooted];
    let entries: Vec<String> = heads
        .iter()
        .enumerate()
        .map(|(i, head)| peer(&format!("m{i}.local"), &pairs(head), &[QUIET]))
        .collect();
    let read = sightings(&format!("[{}]", entries.join(",")), OURS).expect("a survey");
    let ages: Vec<(Option<u64>, Option<u64>, Option<u64>)> = read
        .machines
        .iter()
        .map(|seen| {
            let look = &answer_of(seen).look;
            (look.age_ms, look.up_for_ms, look.up_since_own_ms)
        })
        .collect();
    assert_eq!(
        ages,
        [
            (Some(1_500), Some(3_600_000), Some(THEIRS - 3_600_000)),
            // Its look and its wake after its own answer: no age, its own value kept.
            (None, None, Some(THEIRS + 5)),
            (None, None, Some(THEIRS - 3_600_000)),
            // An OS that says it never booted names no stay.
            (Some(1_500), None, None),
        ]
    );
}

/// THE FLEET READ KEEPS EACH MACHINE'S FLEET ACCESS AND WHETHER IT LETS THIS MAC DRIVE IT: the
/// header's `access=` word where this build knows it, and `drive=1` as yes, its absence beside
/// an `access=` as no. A header with no `access=` (an aterm that does not say) has neither, and a
/// word this build does not know is no level, never the lowest one.
#[cfg(unix)]
#[test]
fn sightings_keep_the_access_word_and_whether_this_mac_may_drive() {
    use aterm_db::fleet::read::Access;
    let said = |node: &str, access: Option<&str>, drive: bool| {
        let mut head = header(node);
        head.extend(access.map(|word| ("access", word.to_string())));
        if drive {
            head.push(("drive", "1".to_string()));
        }
        peer(&format!("{node}.local"), &pairs(&head), &[QUIET])
    };
    let table = [
        ((Some("full"), true), (Some(Access::Full), Some(true))),
        ((Some("write"), false), (Some(Access::Write), Some(false))),
        ((Some("read"), false), (Some(Access::Read), Some(false))),
        ((Some("read"), true), (Some(Access::Read), Some(true))),
        ((None, false), (None, None)),
        ((Some("wide-open"), true), (None, Some(true))),
        ((Some("Full"), false), (None, Some(false))),
    ];
    let entries: Vec<String> = table
        .iter()
        .enumerate()
        .map(|(i, ((access, drive), _))| said(&format!("n{i}"), *access, *drive))
        .collect();
    let read = sightings(&format!("[{}]", entries.join(",")), 9).expect("a survey");
    assert_eq!(
        read.machines
            .iter()
            .map(|seen| {
                let answered = answer_of(seen);
                (answered.access, answered.drive)
            })
            .collect::<Vec<_>>(),
        table.map(|(_, kept)| kept)
    );

    // This Mac's own windows answer a local read, which asks no grant: their headers say
    // `access=` without `drive=1`, and that says nothing of whether this Mac may drive itself.
    // CONTROL: a peer's header with the same words is a peer's refusal of the grant.
    let mut head = header("n-own");
    head.push(("access", "full".to_string()));
    let own = peer("pid 77", &pairs(&head), &[QUIET]).replace("\"fleet\":true,", "");
    let read =
        sightings(&format!("[{own},{}]", said("n9", Some("full"), false)), 9).expect("a survey");
    assert_eq!(read.machines.len(), 2);
    let (own, other) = (answer_of(&read.machines[0]), answer_of(&read.machines[1]));
    assert_eq!(
        (own.access, own.drive),
        (Some(Access::Full), None),
        "this Mac's access is as read; no grant was asked of it"
    );
    assert_eq!(
        (other.access, other.drive),
        (Some(Access::Full), Some(false))
    );
    assert_eq!(
        Access::WORDS,
        aterm_types::fleet_access::FleetAccess::ALL.map(|level| level.word()),
        "the words are the fleet access levels', in rank"
    );
    assert!(
        Access::WORDS
            .iter()
            .all(|word| Access::of(word).map(Access::word) == Some(*word))
    );
}

/// EVERY READ IS HANDED ON FOR THE FLEET TABLES: the answer with the time it came, and a read
/// that gave none (an empty answer, a child killed at its cap) as none.
#[cfg(unix)]
#[test]
fn every_read_is_handed_on_for_the_fleet_tables_answered_empty_or_failed() {
    const AT: u64 = 1_800_000_000_000;
    static HANDED: Mutex<Vec<Option<(String, u64)>>> = Mutex::new(Vec::new());
    fn sink(answer: Option<(&str, u64)>) {
        HANDED
            .lock()
            .expect("the tally")
            .push(answer.map(|(json, at)| (json.to_owned(), at)));
    }
    let runner = |program: &str, args: &[&str], cap: Duration| ChildRunner {
        program: PathBuf::from(program),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cap,
        sighted: sink,
    };
    let hang = aterm_spec::wait::MUST_HAPPEN;
    assert!(
        runner("/bin/echo", &["[]"], hang)
            .read(None, &|| AT)
            .is_ok()
    );
    assert!(
        runner("/usr/bin/true", &[], hang)
            .read(None, &|| AT)
            .is_err(),
        "an empty answer is no survey"
    );
    assert!(
        runner("/bin/sleep", &["600"], Duration::from_millis(200))
            .read(None, &|| AT)
            .is_err(),
        "a child killed at its cap answered nothing"
    );
    assert_eq!(
        *HANDED.lock().expect("the tally"),
        [Some(("[]\n".to_owned(), AT)), None, None]
    );
    // The read the status item makes hands its answers to this process's tables.
    assert!(std::ptr::fn_addr_eq(
        ChildRunner::fleet().sighted,
        tables_sink()
    ));
    assert!(!std::ptr::fn_addr_eq(
        ChildRunner::fleet().sighted,
        sink as fn(Option<(&str, u64)>)
    ));
}

/// WHAT THE TABLES MAKE OF EACH READ HANDED ON: an answer is the live reading; a refused read
/// of the fleet gives this Mac its own answer and holds the peers; a read that gave no answer,
/// or none that is a survey, holds everything read before it.
#[cfg(unix)]
#[test]
fn a_read_handed_on_is_folded_live_held_for_the_fleet_or_held_whole() {
    use aterm_db::fleet::{HELD_FLEET_UNREAD, HELD_READ_FAILED};

    let own = |sessions: &[&[(&str, &str)]]| {
        peer("pid 1", &pairs(&header("own-node")), sessions).replace("\"fleet\":true,", "")
    };
    let m7 = peer("m7.local", &pairs(&header("n7")), &[QUIET]);
    let folder = aterm_db::fleet::Folder::new();
    let tables = |now_ms: u64| {
        folder.fold_now(now_ms);
        folder.tables().value
    };
    // This Mac's sessions, then the peer's (with no registry read, this Mac's row is `self`).
    let sessions = |tables: &aterm_db::fleet::fold::FleetTables| -> [Option<u32>; 2] {
        let of = |own: bool| {
            tables
                .machines
                .iter()
                .find(|row| (row.machine == "self") == own)
                .and_then(|row| row.sessions.map(|at| at.v))
        };
        [of(true), of(false)]
    };

    offer_sightings_to(&folder, Some((&format!("[{},{m7}]", own(&[QUIET])), 1_000)));
    let first = tables(1_000);
    assert_eq!(first.sources.read.word(), "live");
    assert_eq!(sessions(&first), [Some(1), Some(1)]);

    // This Mac goes away: its windows answer, with one session more, and the fleet does not.
    let second: &[(&str, &str)] = &[
        ("sid", "s-2"),
        ("state", "alive"),
        ("program", "zsh"),
        ("program_kind", "shell"),
        ("agent", "-"),
        ("needs", "-"),
    ];
    let away = format!(
        "[{},{{\"label\":\"the fleet\",\"fleet\":true,\"error\":\"this Mac is away\",\"reason\":\"{}\"}}]",
        own(&[QUIET, second]),
        aterm_ctl::FLEET_UNREAD
    );
    offer_sightings_to(&folder, Some((&away, 61_000)));
    let held = tables(61_000);
    assert_eq!(
        (held.sources.read.why(), held.sources.read.at_ms()),
        (Some(HELD_FLEET_UNREAD), Some(1_000))
    );
    assert_eq!(
        sessions(&held),
        [Some(2), Some(1)],
        "this Mac's row takes the answer its windows just gave; the peer's stands"
    );
    let mine = held
        .machines
        .iter()
        .find(|row| row.machine == "self")
        .expect("this Mac is a row");
    assert_eq!(mine.sessions.map(|at| at.at_ms), Some(61_000));

    // A read that gave no answer, and one that is no survey: everything read stays, held.
    for (n, nothing) in [None, Some(("{}", 122_000))].into_iter().enumerate() {
        offer_sightings_to(&folder, nothing);
        let failed = tables(122_000);
        assert_eq!(failed.sources.read.why(), Some(HELD_READ_FAILED), "{n}");
        assert_eq!(sessions(&failed), [Some(2), Some(1)], "{n}");
    }
}

/// THE FLEET READ KEEPS EACH AGENT'S PIN as its row says it: `pinned=1` pinned, `pinned=0` not,
/// and a row with no `pinned=` (an aterm before the pin) or with another word unread, never
/// unpinned. CONTROL: the key not read leaves the pinned agent unread.
#[cfg(unix)]
#[test]
fn the_fleet_read_keeps_each_agents_pin_and_an_older_row_is_unread() {
    let agent = |sid: &'static str, pinned: Option<&'static str>| {
        let mut row = vec![
            ("sid", sid),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "busy"),
            ("needs", "-"),
        ];
        if let Some(pinned) = pinned {
            row.push(("pinned", pinned));
        }
        row
    };
    let rows = [
        agent("s-p1", Some("1")),
        agent("s-p2", Some("0")),
        agent("s-p3", None),
        agent("s-p4", Some("yes")),
    ];
    let rows: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let entry = peer("m1.local", &pairs(&header("n1")), &rows);
    let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
    let pins: Vec<(&str, Option<bool>)> = answer_of(&read.machines[0])
        .agents
        .iter()
        .map(|agent| (agent.sid.as_str(), agent.pinned))
        .collect();
    assert_eq!(
        pins,
        [
            ("s-p1", Some(true)),
            ("s-p2", Some(false)),
            ("s-p3", None),
            ("s-p4", None)
        ]
    );
}

/// A Claude Code agent's session row, saying `agent=<word>` and `needs=<needs>`, with `extra`.
#[cfg(unix)]
fn turn_row<'a>(
    sid: &'a str,
    word: &'a str,
    needs: &'a str,
    extra: &[(&'a str, &'a str)],
) -> Vec<(&'a str, &'a str)> {
    let mut row = vec![
        ("sid", sid),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", word),
        ("needs", needs),
    ];
    row.extend_from_slice(extra);
    row
}

/// THE FLEET READ KEEPS A ROW'S TURN, OUTPUT AND COUNTERS, AND MARKS AN OLDER ROW: every key a
/// row sends of its turn (`turn= turn_ms= turn_end_ms= step_ms= out_ms= out_src= bg=`) and its
/// counters (`tok_in= tok_out= tok_for_ms= turns= tools=`) as numbers and closed words, each
/// `-` or malformed one unread; a row with no `turn=` is an older aterm's (`older`), whose turn
/// its agent's own word says (`busy` and a box, `prompt`, open one; `question` closes it); the
/// agent's work class is decided from them; and the header's `at_ms=` is the answer's own time
/// and each agent's. CONTROLS: `older` false for a row with no `turn` key is caught; an older
/// row's agent at a box classed by its state alone (in no turn) is caught.
#[cfg(unix)]
#[test]
fn sightings_keep_a_rows_turn_output_and_counters_and_mark_an_older_row() {
    use aterm_db::fleet::read::{OutSrc, State, TurnFacts, Work};
    let whole = turn_row(
        "s-t1",
        "busy",
        "-",
        &[
            ("bg", "1"),
            ("turn", "1"),
            ("turn_ms", "250000"),
            ("turn_end_ms", "900000"),
            ("step_ms", "-"),
            ("out_ms", "42000"),
            ("out_src", "transcript"),
            ("tok_in", "1200000"),
            ("tok_out", "56000"),
            ("tok_for_ms", "3600000"),
            ("turns", "14"),
            ("tools", "203"),
        ],
    );
    let dashes = turn_row(
        "s-t2",
        "idle",
        "-",
        &[
            ("bg", "-"),
            ("turn", "0"),
            ("turn_ms", "-"),
            ("turn_end_ms", "12.5"),
            ("step_ms", "-3"),
            ("out_ms", "-"),
            ("out_src", "pty"),
            ("tok_in", "-"),
            ("tok_out", "-"),
            ("tok_for_ms", "-"),
            ("turns", "0"),
            ("tools", "-"),
        ],
    );
    let older = turn_row("s-t3", "busy", "-", &[("bg", "0")]);
    let at_a_box = turn_row("s-t4", "prompt", "approval", &[]);
    let ended_asking = turn_row("s-t5", "question", "question", &[]);
    let rows = [whole, dashes, older, at_a_box, ended_asking];
    let rows: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let mut head = header("n1");
    head.push(("at_ms", "1800000000123".to_string()));
    let entry = peer("m1.local", &pairs(&head), &rows);
    let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
    let answer = answer_of(&read.machines[0]);
    assert_eq!(answer.answer_at_ms, Some(1_800_000_000_123));
    let [whole, dashes, older, at_a_box, ended_asking] = answer.agents.as_slice() else {
        panic!("five agents: {:?}", answer.agents);
    };
    assert!(
        answer
            .agents
            .iter()
            .all(|agent| agent.answer_at_ms == Some(1_800_000_000_123)),
        "each agent is timed by the reply that listed it"
    );
    assert_eq!(
        whole.turn,
        TurnFacts {
            turn_open: Some(true),
            turn_age_ms: Some(250_000),
            turn_end_age_ms: Some(900_000),
            step_age_ms: None,
            out_age_ms: Some(42_000),
            out_src: Some(OutSrc::Transcript),
            bg: Some(1),
            older: false,
            word_turn: true,
        }
    );
    assert_eq!(
        (
            whole.tok_in,
            whole.tok_out,
            whole.tok_for_ms,
            whole.turns,
            whole.tools
        ),
        (
            Some(1_200_000),
            Some(56_000),
            Some(3_600_000),
            Some(14),
            Some(203)
        )
    );
    assert_eq!((whole.work, whole.rate), (Work::WithOutput, None));
    assert_eq!(
        dashes.turn,
        TurnFacts {
            turn_open: Some(false),
            older: false,
            ..TurnFacts::OLDER
        },
        "a `-` or a malformed figure is unread, never 0"
    );
    assert_eq!(
        (
            dashes.tok_in,
            dashes.tok_out,
            dashes.tok_for_ms,
            dashes.turns,
            dashes.tools
        ),
        (None, None, None, Some(0), None)
    );
    assert_eq!(dashes.work, Work::NoTurn);
    assert_eq!(
        older.turn,
        TurnFacts {
            bg: Some(0),
            word_turn: true,
            ..TurnFacts::OLDER
        },
        "a row with no `turn=` is an older aterm's"
    );
    assert_eq!(
        older.work,
        Work::OutputUnread,
        "an older busy row is in a turn"
    );
    assert_eq!(
        (at_a_box.state, at_a_box.turn.older, at_a_box.work),
        (State::Waiting, true, Work::Asking),
        "an older row at a box is asking"
    );
    assert_eq!(
        (ended_asking.state, ended_asking.work),
        (State::Waiting, Work::NoTurn),
        "an older row that ended its turn on a question is in none"
    );
}

/// AN UNREAD CLOCK IS NO TIME: a reply whose header says `at_ms=0` (its writer's clock unread)
/// has no answer time, its agents none, and its look no ages, never a time of 1970 that a later
/// answer's interval would run from. CONTROL: `at_ms=0` read as a time is caught.
#[cfg(unix)]
#[test]
fn an_unread_clock_is_no_answer_time() {
    let mut head = look_header("n1", 1_800_000_060_000);
    for (key, value) in &mut head {
        if *key == "at_ms" {
            *value = "0".to_string();
        }
    }
    let row = turn_row("s-c1", "busy", "-", &[("turn", "1"), ("tok_in", "5")]);
    let entry = peer("m1.local", &pairs(&head), &[&row]);
    let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
    let answer = answer_of(&read.machines[0]);
    assert_eq!(answer.answer_at_ms, None);
    assert_eq!(answer.agents.len(), 1, "{answer:?}");
    assert_eq!(answer.agents[0].answer_at_ms, None);
    assert_eq!((answer.look.age_ms, answer.look.up_for_ms), (None, None));
}

/// EACH OF THIS MAC'S AGENTS IS TIMED BY ITS OWN WINDOW'S REPLY: this Mac's windows answer one
/// read each, at their own times; the machine's answer time is the first reply's, and each
/// agent keeps the time of the reply that listed it, where its counters were read. CONTROL:
/// every agent given the first reply's time is caught.
#[cfg(unix)]
#[test]
fn each_of_this_macs_agents_is_timed_by_its_own_windows_reply() {
    fn local(pid: &str, header: &[(&str, &str)], rows: &[&[(&str, &str)]]) -> String {
        peer(&format!("pid {pid}"), header, rows).replace("\"fleet\":true,", "")
    }
    let window = |node: &str, at_ms: &str| {
        let mut head = header(node);
        head.push(("at_ms", at_ms.to_string()));
        head
    };
    let first = turn_row("s-r1", "busy", "-", &[("turn", "1")]);
    let second = turn_row("s-r2", "busy", "-", &[("turn", "1")]);
    let entries = [
        local("101", &pairs(&window("n1", "1000000")), &[&first]),
        local("102", &pairs(&window("n1", "1009000")), &[&second]),
    ];
    let read = sightings(&format!("[{}]", entries.join(",")), 9).expect("a survey");
    let answer = answer_of(&read.machines[0]);
    assert_eq!(answer.answer_at_ms, Some(1_000_000));
    let times: Vec<(&str, Option<u64>)> = answer
        .agents
        .iter()
        .map(|agent| (agent.sid.as_str(), agent.answer_at_ms))
        .collect();
    assert_eq!(
        times,
        [("s-r1", Some(1_000_000)), ("s-r2", Some(1_009_000))]
    );
}

/// THE WORK TALLY COUNTS AGENTS THE ANSWER DOES NOT LIST: past [`AGENTS_MAX`] listed agents
/// the rest are counted in `agents_more`, and in the work tally by their class, as in the state
/// tally; a partial answer and an older aterm's (no `needs`) have none. CONTROL: classes
/// counted from the listed agents (a lower bound past 128) is caught.
#[cfg(unix)]
#[test]
fn the_work_tally_counts_agents_the_answer_does_not_list() {
    use aterm_db::fleet::read::{AGENTS_MAX, WorkTally};
    let sids: Vec<String> = (0..AGENTS_MAX + 3).map(|n| format!("s-w{n:04}")).collect();
    let writing = [
        ("bg", "0"),
        ("turn", "1"),
        ("out_ms", "30000"),
        ("out_src", "transcript"),
    ];
    let resting = [("bg", "0"), ("turn", "0")];
    let asking = [("bg", "0"), ("turn", "1"), ("step_ms", "9000")];
    let mut rows: Vec<Vec<(&str, &str)>> = sids[..AGENTS_MAX]
        .iter()
        .map(|sid| turn_row(sid, "busy", "-", &writing))
        .collect();
    rows.push(turn_row(&sids[AGENTS_MAX], "idle", "-", &resting));
    rows.push(turn_row(&sids[AGENTS_MAX + 1], "idle", "-", &resting));
    rows.push(turn_row(
        &sids[AGENTS_MAX + 2],
        "question",
        "question",
        &asking,
    ));
    let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let mut head = header("n1");
    let count = rows.len().to_string();
    head[2].1 = count;
    let entry = peer("m1.local", &pairs(&head), &refs);
    let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
    let answer = answer_of(&read.machines[0]);
    assert_eq!((answer.agents.len(), answer.agents_more), (AGENTS_MAX, 3));
    assert_eq!(
        answer.work,
        Some(WorkTally {
            with_output: u32::try_from(AGENTS_MAX).expect("128"),
            no_turn: 2,
            asking: 1,
            ..WorkTally::default()
        })
    );
    assert_eq!(answer.tally.map(|tally| tally.agents), Some(131));

    // An older aterm's rows (no `needs`) and a partial answer have no work tally.
    let older = vec![("sid", "s-o1"), ("state", "alive"), ("program", "claude")];
    let older = [
        older,
        vec![("program_kind", "agent"), ("agent", "busy"), ("turn", "1")],
    ]
    .concat();
    let entry = peer("m2.local", &pairs(&header("n2")), &[&older]);
    let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
    assert_eq!(answer_of(&read.machines[0]).work, None);
    let mut cut = header("n3");
    cut[2].1 = "5".to_string();
    let entry = peer("m3.local", &pairs(&cut), &[&rows[0]]);
    let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
    assert_eq!(answer_of(&read.machines[0]).work, None);
}

/// THE FLEET READ COUNTS THE SESSIONS WAITING FOR MEMORY AND THEIR CLAIMS: every counted row,
/// an agent's or a shell's, that says `wait=memory`, and what its `wait_gib=` claims in MiB;
/// with none waiting a counted 0. A claim its machine did not read (`wait_gib=-`) or one that
/// is no `<digits>.<digit>` leaves the sum unread while the waiter still counts; a row with no
/// `wait=` (an older aterm) and a partial answer have no count. CONTROL: a `-` size summed as
/// 0 is caught.
#[cfg(unix)]
#[test]
fn sightings_count_the_sessions_waiting_for_memory_and_their_claims() {
    use aterm_db::fleet::read::WaitTally;
    let shell = |sid: &'static str, wait: &'static str, gib: &'static str| {
        vec![
            ("sid", sid),
            ("state", "alive"),
            ("program", "targo"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("needs", "-"),
            ("wait", wait),
            ("wait_gib", gib),
            ("wait_ahead", "1"),
        ]
    };
    let mut agent = turn_row("s-a1", "busy", "-", &[]);
    agent.extend([("wait", "-"), ("wait_gib", "-"), ("wait_ahead", "-")]);
    let wait_of = |rows: &[Vec<(&str, &str)>], sessions: &str| {
        let mut head = header("n1");
        head[2].1 = sessions.to_string();
        let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
        let entry = peer("m1.local", &pairs(&head), &refs);
        let read = sightings(&format!("[{entry}]"), 9).expect("a survey");
        answer_of(&read.machines[0]).wait
    };
    assert_eq!(
        wait_of(
            &[
                shell("s-b1", "memory", "4.0"),
                shell("s-b2", "memory", "2.5"),
                agent.clone(),
            ],
            "3"
        ),
        Some(WaitTally {
            waiting: 2,
            wait_mb: Some(4_096 + 2_560),
        })
    );
    for unread in ["-", "4", "4.25", "-1.0", ".5", "x.y"] {
        assert_eq!(
            wait_of(
                &[
                    shell("s-b1", "memory", "4.0"),
                    shell("s-b2", "memory", unread),
                ],
                "2"
            ),
            Some(WaitTally {
                waiting: 2,
                wait_mb: None,
            }),
            "{unread}"
        );
    }
    assert_eq!(
        wait_of(&[shell("s-b1", "-", "-"), agent.clone()], "2"),
        Some(WaitTally::NONE)
    );
    assert_eq!(
        WaitTally::NONE.wait_mb,
        Some(0),
        "none waiting is a counted 0"
    );
    // A row with no `wait=` (an older aterm): no count.
    let older = turn_row("s-o1", "busy", "-", &[]);
    assert_eq!(wait_of(&[shell("s-b1", "memory", "4.0"), older], "2"), None);
    // A partial answer: no count.
    assert_eq!(wait_of(&[shell("s-b1", "memory", "4.0")], "2"), None);
}

/// A refused full config may leave the UI on synthesized update-only text. Automatic fleet
/// reads still use the original file's sharing authority before invoking their runner.
#[cfg(unix)]
#[test]
fn automatic_fleet_reads_keep_sharing_authority_after_config_refusal() {
    use std::os::unix::fs::PermissionsExt as _;

    for (bytes, mode, reads) in [
        (None, 0o600, true),
        (Some(b"".as_slice()), 0o600, true),
        (Some(b"font_px = \"bad\"\n".as_slice()), 0o600, true),
        (
            Some(b"font_px = \"bad\"\n[swarm]\nenabled = true\n".as_slice()),
            0o600,
            true,
        ),
        (
            Some(b"font_px = \"bad\"\n[swarm]\nenabled = false\n".as_slice()),
            0o600,
            false,
        ),
        (Some(b"[swarm\n".as_slice()), 0o600, false),
        (Some(b"[swarm]\nenabled = 1\n".as_slice()), 0o600, false),
        (Some(b"[swarm]\nextra = true\n".as_slice()), 0o600, false),
        (Some(b"[swarm]\nenabled = true\n".as_slice()), 0o664, false),
        (Some(b"\xff".as_slice()), 0o600, false),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let path = aterm_secrets::swarm::config_in_home(&home).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some(bytes) = bytes {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        // The same observation, typed admission and update-only fallback as warm_config;
        // the test constructor keeps asset discovery inert.
        let loaded =
            crate::native_config_service::VersionedConfigService::observe_path(&path, true)
                .and_then(|observed| {
                    crate::native_config_service::VersionedConfigService::new(observed.text)
                });
        let service = loaded.unwrap_or_else(|_| {
            let fallback = crate::app_config::update_switches_stand_in(Some(&path));
            crate::native_config_service::VersionedConfigService::new(fallback.text).unwrap()
        });
        let config = service.snapshot().config;
        let runs = Arc::new(AtomicUsize::new(0));
        let lan = Arc::new(AtomicUsize::new(0));
        let seen_lan = Arc::clone(&lan);
        let (gate, wait) = std::sync::mpsc::channel();
        drop(gate);
        let (started, _started) = std::sync::mpsc::channel();
        let runner = Gated {
            runs: Arc::clone(&runs),
            gate: wait,
            started,
        };
        let mut seams = ReaderSeams {
            book: Box::new(|| BookFacts { in_fleet: true }),
            watch: Arc::new(AtomicBool::new(config.fleet_watch_enabled())),
            sharing: Box::new(automatic_sharing(Some(home))),
            now_ms: Box::new(|| 1_000),
            post: Box::new(|| true),
            lan: Box::new(move || {
                seen_lan.fetch_add(1, Ordering::SeqCst);
                true
            }),
            ..inert_seams()
        };
        let slot = Arc::new(Mutex::new(FleetSlot::default()));
        let (kick, kicks) = std::sync::mpsc::channel();
        drop(kick);
        reader_loop(Box::new(runner), &mut seams, &slot, &kicks);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            usize::from(reads),
            "{bytes:?}, mode {mode:o}"
        );
        assert_eq!(
            lan.load(Ordering::SeqCst),
            usize::from(reads),
            "{bytes:?}, mode {mode:o}"
        );
        let slot = slot.lock().unwrap();
        assert_eq!(
            slot.tried_ms,
            reads.then_some(1_000),
            "{bytes:?}, mode {mode:o}"
        );
        assert_eq!(
            slot.generation,
            u16::from(reads),
            "{bytes:?}, mode {mode:o}"
        );
    }
}

#[test]
fn a_read_names_two_tabs_sharing_five_characters_apart_once_their_ids_are_known() {
    let one = "tm4r7aaaaaa";
    let two = "tm4r7abbbbb";
    let manager = |sid: &str, label: &str| FleetManager {
        label: label.to_owned(),
        sid: sid.to_owned(),
        workers: 1,
    };
    // As a read parses: the twins named at five, before this aterm knows either.
    let mut snapshot = FleetSnapshot {
        macs: vec![FleetMac {
            managers: vec![
                manager(one, "tm4r7"),
                manager(two, "tm4r7"),
                manager("tm4r8aaaaaa", "lead"),
            ],
            ..FleetMac::default()
        }],
        ..FleetSnapshot::default()
    };
    for sid in [one, two] {
        let id = aterm_types::tab_id::TabId::parse(sid).expect("a tab id");
        crate::tab_ids::note_here(&aterm_session::SessionId::from_tab(id));
    }
    snapshot.rename_by_id();
    let labels: Vec<&str> = snapshot.macs[0]
        .managers
        .iter()
        .map(|m| m.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["tm4r7aa", "tm4r7ab", "lead"],
        "each twin is named as far as tells it apart; a role is kept"
    );
}

/// A 0.198.0 window's reply at the facts level, as that release's writers made it (`v0.198.0`:
/// `aterm-gui/src/survey.rs` `survey_rows` and `session_row`, `aterm-agent/src/survey_facts.rs`
/// `header_tokens`): its header has no look, no `access=`, no home size and no updater words;
/// its session row has no turn, output or token keys, no `pinned=` and no stamp of its own. Frozen:
/// it never changes with this build's writers, so a reader of it is held to what a fleet of
/// mixed builds still sends.
#[cfg(unix)]
const REPLY_0198: &str = "OK 2\n\
instance pid=4198 host=old.local version=0.198.0 sessions=1 level=facts cut=0 \
at_ms=1800000060000 node=n0198 instances=1 label=m198 window=1 window_last=1800000000 \
ssh_host_key=- accounts=0 accounts_unread=- moves_7d=- move_tries_7d=- last_move=- \
roster_unread=0\n\
session local=1 sid=s-mixed state=alive program=claude program_kind=agent wait=- wait_gib=- \
wait_ahead=- agent=busy agent_detail=- agent_ms=4000 human_ms=- bg=1 task=- task_rev=0 \
task_from=- task_ms=- task_conclusion=- model=claude-opus effort=- ctx_left_pct=62 \
usage_5h_pct=40 usage_7d_pct=55 account=- auth=- token=- detail=- doing=- role=- \
supervisor=- attention=- goal=- needs=- needs_by=- hold_by=- held_from=- typed_by=- \
typed_ms=- bells=0 bells_sounded=0 last_bell=-\n";

/// This build's reply of the same session at [`look_header`]'s figures, made by this build's own
/// writers where a test can reach them: the header by the survey facts'
/// (`SwarmSurveyFacts::to_survey_reply_at`, the tokens a window's `survey_rows` sends), with a
/// whole look, the updater's words, the home build and the worst store; the row's turn, output
/// and token keys by the row's own (`survey::Progress::fields`), the rest of the row as
/// `session_row` writes it for a session with no request, and the stamp a node's merge gives a
/// row its window answered at another time (`node_serve::row_stamp`).
#[cfg(unix)]
fn reply_now(at_ms: u64) -> String {
    reply_now_with(at_ms, PROGRESS_NOW)
}

/// The turn, output and token figures [`reply_now`]'s row sends: a turn open on a step, a job
/// running (the row's `bg=1`) and transcript output 42 s old.
#[cfg(unix)]
const PROGRESS_NOW: crate::survey::Progress = crate::survey::Progress {
    turn: Some(true),
    turn_ms: Some(250_000),
    turn_end_ms: Some(900_000),
    step_ms: Some(30_000),
    out_ms: Some(42_000),
    out_src: Some("transcript"),
    tok_in: Some(1_200_000),
    tok_out: Some(56_000),
    tok_for_ms: Some(3_600_000),
    turns: Some(14),
    tools: Some(203),
};

/// The updater standing [`reply_now`]'s header says, every field of it said: a downloaded build
/// held after a network failure, tried twice, with automatic checks off.
#[cfg(unix)]
fn update_now() -> aterm_types::update_standing::UpdateFact {
    use aterm_types::update_standing::{StandingWord, UpdateFact, UpdateStanding};
    UpdateFact {
        standing: StandingWord::Known(UpdateStanding::Downloaded),
        checked_at: Some(1_800_000_000),
        to: Some("0.206.0".to_owned()),
        until: Some(1_800_003_600),
        why: Some("network"),
        tries: Some(2),
        checks: Some(false),
        said_at: None,
    }
}

/// [`reply_now`] with its row's turn, output and token figures `progress`.
#[cfg(unix)]
fn reply_now_with(at_ms: u64, progress: crate::survey::Progress) -> String {
    use aterm_agent::survey_facts::{HomeBuild, MachineFacts, StoreWorst, SwarmSurveyFacts};
    use aterm_capacity::machine_look::{Limits, LoadScore, MachineLook};
    let look = MachineLook {
        at_unix_ms: Some(at_ms - 1_500),
        cores: Some(12),
        load_x100: Some([1_530, 900, 700]),
        cpu_busy_x100: Some(1_140),
        rate_win_ms: Some(61_000),
        mem_total_mb: Some(65_536),
        mem_used_mb: Some(40_000),
        mem_avail_mb: Some(25_536),
        mem_pressure: Some(aterm_sysprobe::MemoryLevel::Warn),
        mem_psi60_x100: None,
        swap_used_mb: Some(0),
        swap_total_mb: Some(4_096),
        disk_total_mb: Some(953_000),
        disk_free_mb: Some(51_234),
        gpu_cores: Some(38),
        gpu_busy_pct: Some(97),
        net_link_kbps: Some(10_000_000),
        net_rx_kbps: Some(81_234),
        net_tx_kbps: Some(2_345),
        net_other_kbps: Some(12),
        limits: Limits {
            cpu: Some(true),
            mem: Some(true),
            disk: Some(false),
            gpu: Some(true),
        },
        load: Some(LoadScore {
            level_x100: 88,
            over_x100: Some(21),
        }),
        ..MachineLook::UNREAD
    };
    let facts = SwarmSurveyFacts {
        operation_lines: Vec::new(),
        node: Some("n-now".to_owned()),
        label: Some("m-now".to_owned()),
        roster_unread: false,
        version: "0.204.0".to_owned(),
        window: true,
        window_last: Some(1_800_000_000),
        sessions: 1,
        ssh_host_key: None,
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        moves: None,
        server_heard: Vec::new(),
        home: None,
        home_dir: None,
        home_free_mb: Some(51_234),
        home_total_mb: Some(953_000),
        home_build: Some(HomeBuild {
            mb: 2_048,
            at_ms: at_ms - 60_000,
            partial: false,
        }),
        store_worst: Some(StoreWorst {
            name: "journal".to_owned(),
            pct: 12,
        }),
        machine: Some(MachineFacts {
            look,
            up_since_ms: Some(at_ms - 3_600_000),
            os: Some("macos"),
            target: Some("aarch64-apple-darwin".to_owned()),
        }),
        repos: None,
        update: Some(update_now()),
    };
    let header = facts.to_survey_reply_at(
        false,
        4204,
        Some("now.local"),
        at_ms,
        1,
        (aterm_types::fleet_access::FleetAccess::Full, true),
    );
    let row = format!(
        "session local=1 sid=s-mixed state=alive program=claude program_kind=agent wait=- \
         wait_gib=- wait_ahead=- agent=busy agent_detail=- agent_ms=4000 human_ms=- bg=1 \
         task=- task_rev=0 task_from=- task_ms=- task_conclusion=- model=claude-opus effort=- \
         ctx_left_pct=62 usage_5h_pct=40 usage_7d_pct=55 account=- auth=- token=- upgrade=- \
         detail=- doing=- role=- supervisor=- attention=- goal=- needs=- needs_by=- hold_by=- \
         held_from=- typed_by=- typed_ms=- request=- request_from=- request_why=- bells=0 \
         bells_sounded=0 last_bell=- {} pinned=1 opened_ms={} at_ms={}",
        progress.fields(),
        at_ms - 7_200_000,
        at_ms - 5_000,
    );
    // The header's count names the lines after it; the row is one more.
    let (count, rest) = header
        .strip_prefix("OK ")
        .and_then(|reply| reply.split_once('\n'))
        .expect("an OK reply");
    let count: usize = count.parse().expect("a line count");
    let (instance, extra) = rest.split_once('\n').expect("an instance line");
    format!("OK {}\n{instance}\n{row}\n{extra}", count + 1)
}

/// The keys of a reply's `instance` and `session` lines, each with the word its line starts with.
#[cfg(unix)]
fn reply_keys(reply: &str) -> Vec<(&str, &str)> {
    reply
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(kind, _)| matches!(*kind, "instance" | "session"))
        .flat_map(|(kind, rest)| {
            rest.split(' ')
                .filter_map(|token| token.split_once('='))
                .map(move |(key, _)| (kind, key))
        })
        .collect()
}

/// `reply` with `key` of its `kind` line given `value`, or left out where `value` is `None`.
#[cfg(unix)]
fn reply_with(reply: &str, kind: &str, key: &str, value: Option<&str>) -> String {
    let mut out = String::new();
    for line in reply.lines() {
        if line.split_once(' ').is_some_and(|(k, _)| k == kind) {
            let tokens: Vec<String> = line
                .split(' ')
                .filter_map(|token| match token.split_once('=') {
                    Some((k, _)) if k == key => value.map(|value| format!("{key}={value}")),
                    _ => Some(token.to_owned()),
                })
                .collect();
            out.push_str(&tokens.join(" "));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// What the sightings read of a fleet whose peers sent `replies`, each through the survey's JSON
/// form as this build's `aterm ctl survey --fleet --json` prints it.
#[cfg(unix)]
fn read_of_replies(replies: &[(&str, &str)]) -> Vec<aterm_db::fleet::read::Answered> {
    let json = aterm_ctl::survey_json_of_replies(true, replies);
    let read = sightings(&json, 9).expect("a survey");
    assert_eq!(read.machines.len(), replies.len(), "{json}");
    read.machines
        .iter()
        .map(|seen| answer_of(seen).clone())
        .collect()
}

/// THE WIRE'S MIXED-VERSION PROMISE: a fleet of this build and of 0.198.0 reads both ways. Each
/// reply goes through the survey's JSON form and the sightings, as a fleet read takes it. This
/// build's reply, made by its own writers, reads every figure it sends. The frozen 0.198.0 reply
/// reads every field newer than it unread and everything else as this build's. And each key this
/// build sends that 0.198.0 did not, left out in turn, reads as exactly the figure it carries
/// unread (never 0 or false) and every other figure as sent: `m_at_ms=` unreads the whole look,
/// `access=` the access and the drive grant, `turn=` marks the row older, a row's `at_ms=` leaves
/// it its header's time, and a key the sightings do not read leaves the read whole. A key it
/// names no expectation for fails it. Each agent's class is stated, through a row for each of its
/// branches; and on this Mac, a newer window that answers after an older one is read whole.
#[cfg(unix)]
#[test]
fn a_mixed_version_fleet_reads_both_ways_and_an_unsent_field_is_unread_never_zero() {
    use aterm_db::fleet::read::{Access, Look, OutSrc, TurnFacts, Work, WorkTally};
    const AT: u64 = 1_800_000_060_000;
    // A machine of one agent counts that agent's class. Each class below is stated, never
    // derived by the rule the reader uses (`Work::of`), so a default inside that rule fails too.
    let tally_of = |class: Work| {
        let mut tally = WorkTally::default();
        tally.add(class);
        Some(tally)
    };
    let now = reply_now(AT);
    let [mixed_now, mixed_old] = read_of_replies(&[("m1.local", &now), ("m2.local", REPLY_0198)])
        .try_into()
        .expect("two machines");

    // This build: every figure its writers sent.
    assert_eq!(mixed_now.build.as_deref(), Some("0.204.0"));
    assert_eq!(mixed_now.answer_at_ms, Some(AT));
    assert_eq!(mixed_now.home_free_mb, Some(51_234));
    // Its standing, said at its header's `at_ms=`, every field read.
    let update = aterm_types::update_standing::UpdateFact {
        said_at: Some(AT / 1_000),
        ..update_now()
    };
    assert_eq!(mixed_now.update.as_ref(), Some(&update));
    assert_eq!(mixed_now.look, whole_look(AT));
    assert_eq!(
        (mixed_now.access, mixed_now.drive),
        (Some(Access::Full), Some(true))
    );
    let [agent] = mixed_now.agents.as_slice() else {
        panic!("one agent: {:?}", mixed_now.agents);
    };
    let turn = TurnFacts {
        turn_open: Some(true),
        turn_age_ms: Some(250_000),
        turn_end_age_ms: Some(900_000),
        step_age_ms: Some(30_000),
        out_age_ms: Some(42_000),
        out_src: Some(OutSrc::Transcript),
        bg: Some(1),
        older: false,
        word_turn: true,
    };
    assert_eq!(
        (
            agent.turn,
            agent.tok_in,
            agent.tok_out,
            agent.tok_for_ms,
            agent.turns,
            agent.tools,
            agent.pinned,
        ),
        (
            turn,
            Some(1_200_000),
            Some(56_000),
            Some(3_600_000),
            Some(14),
            Some(203),
            Some(true),
        )
    );
    assert_eq!(agent.work, Work::Tool, "a turn open on a background job");
    assert_eq!(
        agent.answer_at_ms,
        Some(AT - 5_000),
        "a row its node stamped says its own window's time"
    );

    // 0.198.0: what it sent reads as this build's does; every field newer than it is unread.
    let mut want = mixed_now.clone();
    want.build = Some("0.198.0".to_owned());
    want.home_free_mb = None;
    want.update = None;
    want.look = Look::UNREAD;
    (want.access, want.drive) = (None, None);
    let agent = &mut want.agents[0];
    // It stamps no row: each was read at its header's time.
    agent.answer_at_ms = Some(AT);
    agent.turn = TurnFacts {
        turn_open: None,
        turn_age_ms: None,
        turn_end_age_ms: None,
        step_age_ms: None,
        out_age_ms: None,
        out_src: None,
        older: true,
        ..turn
    };
    (agent.tok_in, agent.tok_out, agent.tok_for_ms) = (None, None, None);
    (agent.turns, agent.tools, agent.pinned) = (None, None, None);
    // No turn word, step or output age: its agent's `busy` opens a turn whose output is unread.
    agent.work = Work::OutputUnread;
    want.work = tally_of(Work::OutputUnread);
    assert_eq!(
        mixed_old, want,
        "a 0.198.0 reply: its newer fields unread, never 0"
    );
    assert_eq!(
        mixed_old.agents[0].work,
        Work::OutputUnread,
        "a row with no output key is output unread, never no output"
    );

    // Each key since 0.198.0, left out of this build's reply in turn, reads as that one figure
    // unread and every other as before. A key the sightings read names the figure it carries; a
    // key they do not read leaves the read whole. A key named nowhere here fails the test, so a
    // key a later build sends is placed here before that build ships.
    // The keys the sightings leave to other readers: the home build, the other load averages,
    // the pressure stall share, the worst store, and a row's upgrade, request and open time.
    const NOT_READ: &[(&str, &str)] = &[
        ("instance", "home_build_mb"),
        ("instance", "home_build_at_ms"),
        ("instance", "home_build_partial"),
        ("instance", "load5_x100"),
        ("instance", "load15_x100"),
        ("instance", "mem_psi60_x100"),
        ("instance", "store_worst"),
        ("instance", "store_worst_pct"),
        ("session", "upgrade"),
        ("session", "request"),
        ("session", "request_from"),
        ("session", "request_why"),
        ("session", "opened_ms"),
    ];
    let old: std::collections::BTreeSet<(&str, &str)> =
        reply_keys(REPLY_0198).into_iter().collect();
    let since: Vec<(&str, &str)> = reply_keys(&now)
        .into_iter()
        .filter(|key| !old.contains(key))
        .collect();
    for key in [
        ("instance", "m_at_ms"),
        ("instance", "load"),
        ("instance", "access"),
        ("instance", "update"),
        ("session", "turn"),
        ("session", "tok_in"),
        ("session", "pinned"),
        ("session", "at_ms"),
    ] {
        assert!(
            since.contains(&key),
            "{key:?} is a key since 0.198.0: {since:?}"
        );
    }
    let [full] = read_of_replies(&[("m1.local", &now)])
        .try_into()
        .expect("one");
    fn update_of(
        answer: &mut aterm_db::fleet::read::Answered,
    ) -> &mut aterm_types::update_standing::UpdateFact {
        answer
            .update
            .as_mut()
            .expect("this build says its standing")
    }
    let mut unplaced = Vec::new();
    for (kind, key) in since {
        let [left_out] = read_of_replies(&[("m1.local", &reply_with(&now, kind, key, None))])
            .try_into()
            .expect("one");
        let mut want = full.clone();
        let look = &mut want.look;
        match (kind, key) {
            ("instance", "m_at_ms") => *look = Look::UNREAD,
            ("instance", "cores") => look.cores = None,
            ("instance", "load1_x100") => look.load1_x100 = None,
            ("instance", "cpu_busy_x100") => look.cpu_busy_x100 = None,
            ("instance", "rate_win_ms") => look.rate_win_ms = None,
            ("instance", "mem_total_mb") => look.mem_total_mb = None,
            ("instance", "mem_used_mb") => look.mem_used_mb = None,
            ("instance", "mem_avail_mb") => look.mem_avail_mb = None,
            ("instance", "swap_used_mb") => look.swap_used_mb = None,
            ("instance", "swap_total_mb") => look.swap_total_mb = None,
            ("instance", "home_total_mb") => look.disk_total_mb = None,
            ("instance", "gpu_cores") => look.gpu_cores = None,
            ("instance", "gpu_busy_pct") => look.gpu_busy_pct = None,
            ("instance", "net_link_kbps") => look.net_link_kbps = None,
            ("instance", "net_rx_kbps") => look.net_rx_kbps = None,
            ("instance", "net_tx_kbps") => look.net_tx_kbps = None,
            ("instance", "net_other_kbps") => look.net_other_kbps = None,
            ("instance", "mem_pressure") => look.mem_pressure = None,
            ("instance", "cpu_lim") => look.limits.cpu = None,
            ("instance", "mem_lim") => look.limits.mem = None,
            ("instance", "disk_lim") => look.limits.disk = None,
            ("instance", "gpu_lim") => look.limits.gpu = None,
            ("instance", "load") => look.load = None,
            ("instance", "up_since_ms") => (look.up_since_own_ms, look.up_for_ms) = (None, None),
            ("instance", "os") => look.os = None,
            ("instance", "target") => look.target = None,
            ("instance", "home_free_mb") => want.home_free_mb = None,
            ("instance", "update") => want.update = None,
            ("instance", "update_at") => update_of(&mut want).checked_at = None,
            ("instance", "update_to") => update_of(&mut want).to = None,
            ("instance", "update_until") => update_of(&mut want).until = None,
            ("instance", "update_why") => update_of(&mut want).why = None,
            ("instance", "update_tries") => update_of(&mut want).tries = None,
            ("instance", "update_checks") => update_of(&mut want).checks = None,
            ("instance", "access") => (want.access, want.drive) = (None, None),
            // Its writer spells no drive grant as no key (`survey_rows`).
            ("instance", "drive") => want.drive = Some(false),
            ("session", "turn") => {
                want.agents[0].turn.turn_open = None;
                want.agents[0].turn.older = true;
            }
            ("session", "turn_ms") => want.agents[0].turn.turn_age_ms = None,
            ("session", "turn_end_ms") => want.agents[0].turn.turn_end_age_ms = None,
            ("session", "step_ms") => want.agents[0].turn.step_age_ms = None,
            ("session", "out_ms") => want.agents[0].turn.out_age_ms = None,
            ("session", "out_src") => want.agents[0].turn.out_src = None,
            ("session", "tok_in") => want.agents[0].tok_in = None,
            ("session", "tok_out") => want.agents[0].tok_out = None,
            ("session", "tok_for_ms") => want.agents[0].tok_for_ms = None,
            ("session", "turns") => want.agents[0].turns = None,
            ("session", "tools") => want.agents[0].tools = None,
            ("session", "pinned") => want.agents[0].pinned = None,
            // A row with no stamp of its own was read at its header's time.
            ("session", "at_ms") => want.agents[0].answer_at_ms = full.answer_at_ms,
            key if NOT_READ.contains(&key) => {}
            key => {
                unplaced.push(key);
                continue;
            }
        }
        // A step open keeps the row on its tool whatever else is unread; without the step, its
        // transcript output 42 s old is output.
        let class = if (kind, key) == ("session", "step_ms") {
            Work::WithOutput
        } else {
            Work::Tool
        };
        want.agents[0].work = class;
        want.work = tally_of(class);
        assert_eq!(
            left_out, want,
            "{kind} {key} left out: that figure alone unread, never 0, the rest as sent"
        );
    }
    assert!(
        unplaced.is_empty(),
        "keys since 0.198.0 with no expectation here: {unplaced:?}"
    );

    // Each branch of an agent's class, through rows this build's writer spells.
    let open = |out_ms: Option<u64>| crate::survey::Progress {
        step_ms: None,
        out_ms,
        ..PROGRESS_NOW
    };
    let closed = |turn_end_ms: Option<u64>| crate::survey::Progress {
        turn: Some(false),
        step_ms: None,
        turn_end_ms,
        ..PROGRESS_NOW
    };
    for (progress, class, why) in [
        (
            closed(None),
            Work::WithOutput,
            "a closed turn with a job running and output since, where no close time reads, is in a turn",
        ),
        (
            closed(Some(30_000)),
            Work::NoTurn,
            "output older than the turn's close is that turn's own",
        ),
        (
            open(None),
            Work::OutputUnread,
            "a turn with no output age (its writer's clock unread) is output unread, never output",
        ),
        (
            open(Some(700_000)),
            Work::NoOutput,
            "output older than ten minutes",
        ),
        (
            open(Some(42_000)),
            Work::WithOutput,
            "transcript output within ten minutes",
        ),
    ] {
        let [answer] = read_of_replies(&[("m1.local", &reply_now_with(AT, progress))])
            .try_into()
            .expect("one");
        assert_eq!(
            (answer.agents[0].work, answer.work),
            (class, tally_of(class)),
            "{why}: {}",
            progress.fields()
        );
    }

    // This Mac's windows with an older one answering first, as an update leaves them: the newer
    // window's figures are read, never left unread by the older one's silence.
    let older_window = REPLY_0198.replace("sid=s-mixed", "sid=s-older");
    let json = aterm_ctl::survey_json_of_replies(
        false,
        &[("pid 4198", &older_window), ("pid 4204", &now)],
    );
    let read = sightings(&json, 9).expect("a survey");
    let [own] = read.machines.as_slice() else {
        panic!("this Mac alone: {json}");
    };
    let own = answer_of(own);
    assert_eq!(own.look, whole_look(AT), "the newer window's look");
    assert_eq!(own.home_free_mb, Some(51_234));
    // A local read asks for no drive grant, so this Mac's is unread whatever its windows say.
    assert_eq!((own.access, own.drive), (Some(Access::Full), None));
    let tokens: Vec<(&str, Option<u64>)> = own
        .agents
        .iter()
        .map(|agent| (agent.sid.as_str(), agent.tok_in))
        .collect();
    assert_eq!(tokens, [("s-mixed", Some(1_200_000)), ("s-older", None)]);
}

/// A ROW'S ACTIVITY WORD AND ITS NEED ARE READ AS IT SENT THEM: a `word=` (CX-09's, which no
/// build sends yet) is kept as sent, and a word outside the activity grammar, or none sent, is
/// unread; `needs=-` is no need, a need word is that need, and a word outside the need grammar
/// is unread. CONTROLS: a word invented where none was sent (from the agent's own word) and a
/// `needs=-` read as unread are caught.
#[cfg(unix)]
#[test]
fn a_rows_activity_word_and_its_need_are_read_as_it_sent_them() {
    use aterm_types::needs::NeedWord;
    const AT: u64 = 1_800_000_060_000;
    let now = reply_now(AT);
    assert!(
        now.contains(" needs=-"),
        "this build's row says no need: {now}"
    );
    let agent_of = |reply: &str| {
        let [answer] = read_of_replies(&[("m1.local", reply)])
            .try_into()
            .expect("one");
        let agent = answer.agents.first().expect("an agent").clone();
        (agent.activity, agent.need)
    };
    assert_eq!(agent_of(&now), (None, Some(None)), "no word sent, no need");
    let with = |key: &str, value: &str| now.replacen(" needs=-", &format!(" {key}={value}"), 1);
    let with_word = |word: &str| now.replacen(" needs=-", &format!(" word={word} needs=-"), 1);
    for word in ["busy", "wall:usage-weekly", "on-track"] {
        assert_eq!(
            agent_of(&with_word(word)).0.as_deref(),
            Some(word),
            "{word} as sent"
        );
    }
    for word in ["-", "Busy", "42", &"w".repeat(33)] {
        assert_eq!(
            agent_of(&with_word(word)).0,
            None,
            "{word} is no activity word"
        );
    }
    assert_eq!(
        agent_of(&with("needs", "question")).1,
        Some(Some(NeedWord::Question))
    );
    assert_eq!(
        agent_of(&with("needs", "rebuilding")).1,
        None,
        "a newer need word"
    );
    let without = reply_with(&now, "session", "needs", None);
    assert_eq!(agent_of(&without).1, None, "no needs= key");
}
