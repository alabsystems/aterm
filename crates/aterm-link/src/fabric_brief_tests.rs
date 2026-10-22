// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric brief`'s fold over bodies a real session log's mirror makes:
//! per seq space (ruling Q9), deduped, partial without its start, `node-gone`
//! after its node's will (ruling Q4), opaque bodies counted, hostile bytes
//! made safe.

use super::*;
use aterm_slog::{
    Actor, Close, Conf, Fields, Kind, Log, Mark, Open, Out, RunOut, Src, Stamp, Tier,
};

const FLEET: &str = "f1";

fn face(node: &str, sid: &str) -> String {
    format!("/f/{FLEET}/pub/{node}/{sid}/ev")
}

fn cfg() -> Config {
    crate::cli::parse(&[
        "--fleet".to_string(),
        FLEET.to_string(),
        "--broker".to_string(),
        "/tmp/none.sock".to_string(),
    ])
    .expect("a config")
    .cfg
}

/// A session log of epoch `ep`: its open, a command that failed, a turn.
fn session(ep: u32, at: u64) -> Log {
    let mut log = Log::new(ep);
    let t = |ms: u64| Stamp::new(at + ms, ms);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        t(0),
    )
    .expect("opens");
    let c = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), t(1))
        .expect("cmd");
    log.close(
        c,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact),
        t(2),
    )
    .expect("closes");
    log
}

/// Everything `log` has, taken and encoded as the mirror's bodies.
fn bodies(log: &mut Log) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let take = log
            .bus_copy(64, Stamp::new(9, 9))
            .encode(Tier::Wire, &|_| None);
        if take.slots.is_empty() {
            return out;
        }
        for s in &take.slots {
            out.push(s.body.clone());
        }
        log.land(take.slots.last().expect("a slot").at)
            .expect("lands");
        if !take.more {
            return out;
        }
    }
}

fn read_of(items: &[(&str, &str, Vec<String>)]) -> Read {
    let mut r = Read {
        head: 1000,
        ..Read::default()
    };
    let mut off = 0;
    for (node, sid, bodies) in items {
        for b in bodies {
            off += 1;
            r.ingest(FLEET, None, off, &face(node, sid), b.as_bytes())
                .expect("within the read's session bound");
        }
    }
    r
}

/// The fold shows the session's state from the bus alone: its need, deduped
/// when the bus holds a body twice, the bridge's own `v=1` bodies opaque.
#[test]
fn the_brief_folds_each_session_from_the_bus_alone() {
    let mut a = session(1, 1_760_000_000_000);
    let mut bs = bodies(&mut a);
    let twice = bs.clone();
    bs.extend(twice);
    bs.push("v=1 t=5 ev=undeliverable%20off=3".to_string());
    let r = read_of(&[("n-1", "s-a", bs)]);
    let text = render(&cfg(), &BriefOpts::default(), &r, 1_760_000_001_000);
    let header = text.lines().next().expect("a header");
    assert!(header.starts_with("OK "), "{text}");
    assert!(
        header.contains(" sessions=1 partial=0 opaque=1 "),
        "{header}"
    );
    assert!(header.contains(" dup="), "{header}");
    assert!(
        !header.contains(" dup=0 "),
        "the second copies were counted: {header}"
    );
    assert!(text.contains("@s-a@n-1 - ep=1 as_of="), "{text}");
    assert!(
        text.contains("need=failure"),
        "the run's failure, from the bus: {text}"
    );
}

/// A SPAWNED SESSION'S OPEN STILL FOLDS (session log Phase 6 step 6E-4):
/// its open names its parent locally (`parent=`, HOST since this step), the
/// mirror's wire body carries the open without it, and the brief folds the
/// session whole from that body (`partial=0`), its parent named nowhere.
/// NEGATIVE CONTROL, run: `parent=` marked W fails it.
#[test]
fn a_spawned_sessions_open_folds_without_its_parent() {
    let parent = "s-parentqzp6";
    let mut log = Log::new(1);
    let f = Fields::new()
        .id(aterm_slog::Key::Parent, parent)
        .expect("fields");
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        f,
        Stamp::new(1_760_000_000_000, 0),
    )
    .expect("opens");
    let bs = bodies(&mut log);
    assert!(
        bs.iter()
            .all(|b| !b.contains("parent=") && !b.contains(parent)),
        "{bs:#?}"
    );
    let r = read_of(&[("n-1", "s-a", bs)]);
    let text = render(&cfg(), &BriefOpts::default(), &r, 1_760_000_001_000);
    let header = text.lines().next().expect("a header");
    assert!(header.contains(" sessions=1 partial=0 "), "{header}");
    assert!(text.contains("@s-a@n-1 - ep=1 as_of="), "{text}");
    assert!(!text.contains(parent), "{text}");
}

/// RULING Q9: a keeper-recovered session restarts its seqs in a new space;
/// the brief folds the newest space on its own (never dropping its records
/// as reordered behind the old space's higher seqs), and says the session
/// has two.
#[test]
fn a_keeper_recovered_sid_folds_its_new_space_alone() {
    let mut old = session(1, 1_760_000_000_000);
    for _ in 0..20 {
        old.mark(
            Mark::new(Kind::Seen, Src::Status, Conf::Strong),
            Stamp::new(1_760_000_000_100, 100),
        )
        .expect("a seen");
    }
    let mut old_bodies = bodies(&mut old);
    // The crash recovery: a new log, a new epoch, seqs from 1.
    let mut new = Log::new(1_760_000_500);
    new.gap(
        aterm_slog::GapWhy::Crash,
        0,
        Stamp::new(1_760_000_500_000, 0),
    );
    let mut new_bodies = bodies(&mut new);
    // Interleaved on the bus as a late mirror could leave them.
    old_bodies.append(&mut new_bodies);
    let r = read_of(&[("n-1", "s-k", old_bodies)]);
    let s = &r.sessions[&("n-1".to_string(), "s-k".to_string())];
    assert_eq!(s.spaces.len(), 2, "two seq spaces");
    let (ep, space) = s.spaces.iter().next_back().expect("the newest");
    assert_eq!(*ep, 1_760_000_500);
    assert_eq!(space.reordered, 0, "reordered counts within one space only");
    let text = render(&cfg(), &BriefOpts::default(), &r, 1_760_000_600_000);
    assert!(text.contains(" ep=1760000500 "), "{text}");
    assert!(text.contains(" spaces=2"), "{text}");
    assert!(
        !text.contains("need=failure"),
        "the old space's need is not the new one's: {text}"
    );
}

/// RULING Q4: a session whose node's presence ended without the session's
/// close is `partial why=node-gone`; one whose window holds neither its start
/// nor a snapshot is `partial why=no-start`.
#[test]
fn a_session_of_a_gone_node_or_without_its_start_is_partial() {
    let mut a = session(1, 1_760_000_000_000);
    let bs = bodies(&mut a);
    // s-b's window holds neither its start nor a snapshot.
    let late: Vec<String> = bs[2..]
        .iter()
        .filter(|b| !b.contains(" op=snap "))
        .cloned()
        .collect();
    let mut r = read_of(&[("n-1", "s-a", bs.clone()), ("n-2", "s-b", late.clone())]);
    r.nodes_gone.push("n-1".to_string());
    // The window began past the log's start: what it does not reach is its
    // cut, said `no-start`, never `holes=` (fixer F8).
    r.from = 1;
    let text = render(&cfg(), &BriefOpts::default(), &r, 1_760_000_001_000);
    assert!(
        text.contains("@s-a@n-1 - ep=1 as_of=") && text.contains("partial why=node-gone"),
        "{text}"
    );
    let b_line = text
        .lines()
        .find(|l| l.starts_with("@s-b@n-2"))
        .expect("s-b");
    assert!(b_line.contains("partial why=no-start"), "{b_line}");
    assert!(
        !b_line.contains(" holes="),
        "the window's cut is no loss nobody said: {b_line}"
    );
    // A window from the log's very start that lacks the space's first ms:
    // that is loss nobody said.
    let whole = read_of(&[("n-2", "s-b", late)]);
    let text = render(&cfg(), &BriefOpts::default(), &whole, 1_760_000_001_000);
    assert!(text.contains(" holes=1..2"), "{text}");
    // With a snapshot in the window, the same late reader seeds and is whole.
    let seeded = read_of(&[("n-2", "s-c", bs[2..].to_vec())]);
    let text = render(&cfg(), &BriefOpts::default(), &seeded, 1_760_000_001_000);
    assert!(!text.contains("partial why=no-start"), "{text}");
    assert!(text.lines().next().unwrap().contains(" partial="), "{text}");
}

/// A NEWER WRITER'S WORDS ARE NO HOLE (§3.4: a new word never changes `v`):
/// bodies this build cannot place — a mark kind, an operation, a node level
/// it does not know — are counted `foreign=`, never `opaque=`, and their
/// `ms` are held, so the brief prints no `holes=` for a record that is on
/// the bus.
#[test]
fn a_newer_writers_words_are_foreign_and_no_hole() {
    let bodies: Vec<String> = [
        "v=2 t=1 ep=1 seq=1 ms=1 op=open node=- k=spawn src=aterm conf=exact",
        "v=2 t=2 ep=1 seq=2 ms=2 op=mark node=- k=zzznew src=mark conf=claim",
        "v=2 t=3 ep=1 seq=3 ms=3 op=zzzop node=- k=turn src=aterm conf=exact",
        "v=2 t=4 ep=1 seq=4 ms=4 op=open node=q1 par=- k=turn src=aterm conf=exact",
        "v=2 t=5 ep=1 seq=5 ms=5 op=mark node=- k=seen src=status conf=strong",
    ]
    .iter()
    .map(|b| (*b).to_string())
    .collect();
    let r = read_of(&[("n-1", "s-a", bodies)]);
    let text = render(&cfg(), &BriefOpts::default(), &r, 2);
    let header = text.lines().next().expect("a header");
    assert!(header.contains(" opaque=0 foreign=3 "), "{header}");
    assert!(header.contains(" partial=0 "), "{header}");
    assert!(!text.contains(" holes="), "{text}");
}

/// A hostile node's bodies: a control byte in a field is never printed raw.
#[test]
fn a_hostile_body_is_printed_safe() {
    let body = "v=2 t=1 ep=1 seq=1 ms=1 op=open node=- k=spawn src=aterm conf=exact".to_string();
    let r = read_of(&[("n-1", "s-\u{1b}[2J", vec![body])]);
    let text = render(&cfg(), &BriefOpts::default(), &r, 2);
    assert!(!text.contains('\u{1b}'), "{text:?}");
}

/// The words: a session, its node, a window and a budget within bounds.
#[test]
fn the_brief_reads_its_words_within_their_bounds() {
    let o = BriefOpts::parse(&[
        "@s-a@n-1".to_string(),
        "--window".to_string(),
        "1024".to_string(),
        "--budget".to_string(),
        "500".to_string(),
    ])
    .expect("reads");
    assert_eq!(
        (o.sid.as_deref(), o.node.as_deref(), o.window, o.budget),
        (Some("s-a"), Some("n-1"), Some(1024), Some(500))
    );
    for bad in [
        vec!["--window", "0"],
        vec!["--window", "2000000"],
        vec!["--budget", "5"],
        vec!["--frob"],
        vec!["--window"],
    ] {
        let words: Vec<String> = bad.iter().map(|s| (*s).to_string()).collect();
        assert!(BriefOpts::parse(&words).is_err(), "{bad:?}");
    }
}

/// RULING Q4 PAST A RELAUNCH (fixer F7): a session whose close never reached
/// the bus is partial when its own presence says it exited (`no-close`), or
/// when its presence row is an earlier incarnation's than its node's live one
/// (`node-gone`: the relaunch never re-announced it). A live session of the
/// node's current incarnation is not.
#[test]
fn a_session_without_its_close_past_a_relaunch_is_partial() {
    let mut a = session(1, 1_760_000_000_000);
    let bs = bodies(&mut a);
    let mut r = read_of(&[
        ("n-1", "s-x", bs.clone()),
        ("n-1", "s-y", bs.clone()),
        ("n-1", "s-z", bs),
    ]);
    r.node_inc.insert("n-1".to_string(), 7);
    for (sid, state, inc) in [("s-x", "exited", 6), ("s-y", "live", 6), ("s-z", "live", 7)] {
        let s = r
            .sessions
            .get_mut(&("n-1".to_string(), sid.to_string()))
            .expect("the session");
        s.presence = Some(state.to_string());
        s.presence_inc = Some(inc);
    }
    let text = render(&cfg(), &BriefOpts::default(), &r, 1_760_000_001_000);
    let line = |sid: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("@{sid}@n-1")))
            .unwrap_or_else(|| panic!("{sid}: {text}"))
            .to_string()
    };
    assert!(line("s-x").contains("partial why=no-close"), "{text}");
    assert!(line("s-y").contains("partial why=node-gone"), "{text}");
    assert!(!line("s-z").contains("partial"), "{text}");
}

/// THE BUDGET IS THE WHOLE BRIEF'S (fixer F23): many sessions under a small
/// budget are shown newest first while they fit, each head line counted, and
/// the rest are `more=sessions:<k>`; `used=` never passes `budget=`.
#[test]
fn many_sessions_are_held_to_the_budget() {
    let items: Vec<(String, Vec<String>)> = (0..60)
        .map(|i| {
            let mut log = session(1, 1_760_000_000_000 + i);
            (format!("s-{i:02}"), bodies(&mut log))
        })
        .collect();
    let refs: Vec<(&str, &str, Vec<String>)> = items
        .iter()
        .map(|(sid, b)| ("n-1", sid.as_str(), b.clone()))
        .collect();
    let r = read_of(&refs);
    let opts = BriefOpts {
        budget: Some(150),
        ..BriefOpts::default()
    };
    let text = render(&cfg(), &opts, &r, 1_760_000_001_000);
    let header = text.lines().next().expect("a header");
    let used: usize = header
        .split(' ')
        .find_map(|t| t.strip_prefix("used="))
        .and_then(|v| v.parse().ok())
        .expect("used=");
    assert!(used <= 150, "{header}");
    let body: usize = text
        .lines()
        .skip(1)
        .map(|l| aterm_slog::tokens(l.len() + 1))
        .sum();
    assert_eq!(used, body, "used= counts every line under the header");
    let more = text
        .lines()
        .find_map(|l| l.strip_prefix("more=sessions:"))
        .and_then(|k| k.parse::<usize>().ok())
        .expect("more=sessions");
    let shown = text.lines().filter(|l| l.starts_with("@s-")).count();
    assert_eq!(shown + more, 60, "{text}");
    assert!(shown > 0);
    assert!(text.contains("@s-59@n-1"), "the newest first: {text}");
}

/// The real roster and fold share a bounded read. Existing entries still
/// accept updated presence and records when full; another session or node
/// refuses the read rather than silently rendering a complete-looking subset.
#[test]
fn a_brief_refuses_a_roster_or_fold_past_its_session_bound() {
    let body = b"v=2 t=1 ep=1 seq=1 ms=1 op=open node=- k=spawn src=aterm conf=exact";
    let mut r = Read::default();
    r.presence("n-1", "s-0000", b"state=live inc=1").unwrap();
    r.ingest(FLEET, None, 1, &face("n-1", "s-0000"), body)
        .unwrap();
    assert_eq!(r.sessions.len(), 1);
    assert_eq!(
        r.sessions[&("n-1".to_string(), "s-0000".to_string())]
            .spaces
            .len(),
        1
    );
    for i in 1..SESSIONS_MAX {
        r.presence("n-1", &format!("s-{i:04}"), b"state=live inc=1")
            .unwrap();
    }
    assert_eq!(r.sessions.len(), SESSIONS_MAX);
    r.presence("n-1", "s-0000", b"state=exited inc=2").unwrap();
    r.ingest(FLEET, None, 2, &face("n-1", "s-0000"), body)
        .unwrap();
    let s = &r.sessions[&("n-1".to_string(), "s-0000".to_string())];
    assert_eq!(
        (s.presence.as_deref(), s.presence_inc, s.newest_off),
        (Some("exited"), Some(2), 2)
    );
    let roster_refused = r
        .presence("n-1", "s-overflow", b"state=live inc=1")
        .is_err();
    let fold_refused = r
        .ingest(FLEET, None, 3, &face("n-1", "s-overflow"), body)
        .is_err();

    // An event-only session has the same admission boundary as a roster row.
    let mut records = Read::default();
    for i in 0..SESSIONS_MAX {
        records
            .ingest(
                FLEET,
                None,
                i as u64,
                &face("n-1", &format!("s-{i:04}")),
                body,
            )
            .unwrap();
    }
    records
        .ingest(
            FLEET,
            None,
            SESSIONS_MAX as u64,
            &face("n-1", "s-0000"),
            body,
        )
        .unwrap();
    let records_refused = records
        .ingest(FLEET, None, 9, &face("n-1", "s-overflow"), body)
        .is_err();

    let mut nodes = Read::default();
    for i in 0..SESSIONS_MAX {
        nodes
            .presence(&format!("n-{i:04}"), "node", b"state=live inc=1")
            .unwrap();
    }
    nodes.presence("n-0000", "node", b"state=gone").unwrap();
    nodes
        .presence("n-0000", "node", b"state=live inc=2")
        .unwrap();
    assert_eq!(nodes.node_inc.get("n-0000"), Some(&2));
    let nodes_refused = nodes.presence("n-overflow", "node", b"state=gone").is_err();
    assert!(
        roster_refused
            && fold_refused
            && records_refused
            && nodes_refused
            && r.sessions.len() == SESSIONS_MAX
            && records.sessions.len() == SESSIONS_MAX
            && nodes.node_inc.len() + nodes.nodes_gone.len() == SESSIONS_MAX,
        "BRIEF_ROSTER_AND_FOLD_MUST_SHARE_THE_BOUND"
    );
}

/// A WINDOW BELOW A RETAINING BROKER'S FLOOR READS FROM IT (session log Phase 6
/// step 6E-2): a session's bodies on its `ev` face, the log then cut below the
/// second. The brief's window reaches offset 0, below what the broker keeps; the
/// read folds from the floor (`from=` says where), never refuses the whole
/// brief. NEGATIVE CONTROL, run: the plain `fetch` the read made before is
/// refused `retained-from=`, and the brief with it.
#[cfg(unix)]
#[test]
fn a_window_below_the_retained_floor_reads_from_it() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-brief-kept")
        .tempdir_in("/tmp")
        .expect("scratch");
    let sock = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _served = broker.serve(&sock).expect("serve the broker");
    let (mut conn, _closer) =
        transport::connect(&sock.to_string_lossy(), aterm_spec::wait::MUST_HAPPEN)
            .expect("connect");
    let mut log = session(1, 1_760_000_000_000);
    let offs: Vec<u64> = bodies(&mut log)
        .iter()
        .zip(1u64..)
        .map(|(b, seq)| {
            conn.publish(91, seq, &face("n-a", "s-a"), b.as_bytes())
                .expect("publish")
                .0
        })
        .collect();
    assert!(offs.len() >= 2, "{offs:?}");
    broker.retain_before(offs[1]).expect("the cut");

    let mut c = cfg();
    c.broker = sock.to_string_lossy().into_owned();
    let r = read(&c, &BriefOpts::default()).expect("the brief reads on from the floor");
    let last = offs[offs.len() - 1];
    assert_eq!((r.from, r.head), (offs[1], last + 1));
    assert_eq!(r.sessions.len(), 1, "the session it kept the rest of");
}
