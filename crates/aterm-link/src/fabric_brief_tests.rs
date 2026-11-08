// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm fabric brief`'s fold over session-log mirror bodies.

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
    let mut new = Log::new(1_760_000_500);
    new.gap(
        aterm_slog::GapWhy::Crash,
        0,
        Stamp::new(1_760_000_500_000, 0),
    );
    let mut new_bodies = bodies(&mut new);
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

#[test]
fn a_session_of_a_gone_node_or_without_its_start_is_partial() {
    let mut a = session(1, 1_760_000_000_000);
    let bs = bodies(&mut a);
    let late: Vec<String> = bs[2..]
        .iter()
        .filter(|b| !b.contains(" op=snap "))
        .cloned()
        .collect();
    let mut r = read_of(&[("n-1", "s-a", bs.clone()), ("n-2", "s-b", late.clone())]);
    r.nodes_gone.push("n-1".to_string());
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
    let whole = read_of(&[("n-2", "s-b", late)]);
    let text = render(&cfg(), &BriefOpts::default(), &whole, 1_760_000_001_000);
    assert!(text.contains(" holes=1..2"), "{text}");
    let seeded = read_of(&[("n-2", "s-c", bs[2..].to_vec())]);
    let text = render(&cfg(), &BriefOpts::default(), &seeded, 1_760_000_001_000);
    assert!(!text.contains("partial why=no-start"), "{text}");
    assert!(text.lines().next().unwrap().contains(" partial="), "{text}");
}

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

#[test]
fn a_hostile_body_is_printed_safe() {
    let body = "v=2 t=1 ep=1 seq=1 ms=1 op=open node=- k=spawn src=aterm conf=exact".to_string();
    let r = read_of(&[("n-1", "s-\u{1b}[2J", vec![body])]);
    let text = render(&cfg(), &BriefOpts::default(), &r, 2);
    assert!(!text.contains('\u{1b}'), "{text:?}");
}

#[test]
fn the_brief_reads_its_words_within_their_bounds() {
    let o = BriefOpts::parse(&[
        "@s-a@n-1".to_string(),
        "--window".to_string(),
        "1024".to_string(),
        "--budget".to_string(),
        "500".to_string(),
        "--json".to_string(),
    ])
    .expect("reads");
    assert_eq!(
        (
            o.sid.as_deref(),
            o.node.as_deref(),
            o.window,
            o.budget,
            o.json
        ),
        (Some("s-a"), Some("n-1"), Some(1024), Some(500), true)
    );
    assert!(
        !BriefOpts::parse(&[]).expect("reads").json,
        "text by default"
    );
    let overview: Vec<String> = ["--json", "--budget", "100000"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let o = BriefOpts::parse(&overview).expect("the Overview's read is the brief's");
    assert_eq!((o.json, o.budget), (true, Some(100_000)));
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

#[test]
fn a_nodes_will_of_either_word_makes_its_unclosed_sessions_partial_node_gone() {
    for will in ["away", "gone"] {
        let mut a = session(1, 1_760_000_000_000);
        let mut r = read_of(&[("n-1", "s-a", bodies(&mut a))]);
        let mut row = |sid: &str, body: &str| r.presence("n-1", sid, body.as_bytes()).unwrap();
        row("node", "v=1 t=1 state=live inc=1");
        row("s-a", "v=1 t=1 inc=1 state=live");
        let partial = |r: &Read| {
            let text = render_json(&cfg(), &json_opts(), r, 1_760_000_001_000, None);
            let v = json_of(&text);
            let s = &sessions_of(&v)[0];
            assert_eq!(
                s.get("need").and_then(aterm_json::Value::as_str),
                Some("failure"),
                "{text}"
            );
            s.get("partial")
                .and_then(aterm_json::Value::as_array)
                .expect("partial")
                .iter()
                .filter_map(aterm_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert!(partial(&r).is_empty(), "{will}: live");
        r.presence(
            "n-1",
            "node",
            format!("v=1 t=2 state={will} inc=1 fabric=disconnected").as_bytes(),
        )
        .unwrap();
        assert_eq!(
            partial(&r),
            ["node-gone"],
            "{will}: a node's will, either word, leaves its unclosed session's need to nobody"
        );
        let text = render(&cfg(), &BriefOpts::default(), &r, 1_760_000_001_000);
        assert!(text.contains("partial why=node-gone"), "{will}: {text}");
        r.presence("n-1", "node", b"v=1 t=3 state=live inc=2")
            .unwrap();
        r.presence("n-1", "s-a", b"v=1 t=3 inc=2 state=live")
            .unwrap();
        assert!(partial(&r).is_empty(), "{will}: a reattach restores it");
    }
}

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

/// A WINDOW BELOW A RETAINING BROKER'S FLOOR READS FROM IT:
/// a session's bodies on its `ev` face, the log then cut below the
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
    broker
        .retain_before_carrying_state(offs[1])
        .expect("the cut");

    let mut c = cfg();
    c.broker = sock.to_string_lossy().into_owned();
    let r = read(&c, &BriefOpts::default()).expect("the brief reads on from the floor");
    let last = offs[offs.len() - 1];
    assert_eq!((r.from, r.head), (offs[1], last + 1));
    assert_eq!(r.sessions.len(), 1, "the session it kept the rest of");
}

fn in_approval(at: u64, tool: &str) -> Log {
    let mut log = Log::new(1);
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
    let run = log
        .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), t(1))
        .expect("the run");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            t(2),
        )
        .expect("the turn");
    let tool = Fields::new()
        .word(aterm_slog::Key::Box, tool)
        .expect("a box word");
    log.open(
        Open::new(Kind::Approval, Src::Screen, Conf::Strong)
            .under(turn)
            .fields(tool),
        t(3),
    )
    .expect("the box");
    log
}

const SCHEMA: [&str; 13] = [
    "node", "sid", "this", "ep", "need", "detail", "open", "mail_in", "mail_out", "last_seq",
    "age_s", "partial", "closed",
];

/// The keys the brief writes of its own reading, never a body's field: `closed` is the fold's
/// close's `out=` word, a `&'static` word of the closed vocabulary (fix unit Q1).
const DERIVED: [&str; 9] = [
    "node", "sid", "this", "ep", "level", "last_seq", "age_s", "partial", "closed",
];

fn json_of(text: &str) -> aterm_json::Value {
    aterm_json::from_str(text).unwrap_or_else(|e| panic!("one JSON object: {e}: {text}"))
}

fn sessions_of(v: &aterm_json::Value) -> &Vec<aterm_json::Value> {
    v.get("sessions")
        .and_then(aterm_json::Value::as_array)
        .expect("a sessions array")
}

fn json_opts() -> BriefOpts {
    BriefOpts {
        json: true,
        ..BriefOpts::default()
    }
}

#[test]
fn the_json_brief_holds_each_session_in_its_closed_schema() {
    let now = 1_760_000_100_000;
    let mut appr = in_approval(1_760_000_060_000, "bash");
    let mut failed = session(1, 1_760_000_000_000);
    let r = read_of(&[
        ("n-2", "s-f", bodies(&mut failed)),
        ("n-1", "s-a", bodies(&mut appr)),
    ]);
    let text = render_json(&cfg(), &json_opts(), &r, now, Some("n-2"));
    assert!(text.contains(r#""need":"approval:bash""#), "{text}");
    assert!(!text.contains('\n'), "one line: {text}");
    let v = json_of(&text);
    assert_eq!(v.get("v").and_then(aterm_json::Value::as_u64), Some(2));
    assert_eq!(v.get("more").and_then(aterm_json::Value::as_u64), Some(0));
    let sessions = sessions_of(&v);
    assert_eq!(sessions.len(), 2, "{text}");
    for s in sessions {
        let keys: Vec<&str> = s
            .as_object()
            .expect("a session object")
            .keys()
            .map(String::as_str)
            .collect();
        let mut want = SCHEMA.to_vec();
        want.sort_unstable();
        assert_eq!(keys, want, "the closed schema: {text}");
    }
    let plain = render(&cfg(), &BriefOpts::default(), &r, now);
    let as_of = |sid: &str| -> u64 {
        plain
            .lines()
            .find(|l| l.starts_with(&format!("@{sid}@")))
            .and_then(|l| l.split(' ').find_map(|t| t.strip_prefix("as_of=")))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{sid}'s as_of: {plain}"))
    };
    let get = |s: &aterm_json::Value, k: &str| s.get(k).cloned().expect(k);
    let (a, f) = (&sessions[0], &sessions[1]);
    assert_eq!(
        (get(a, "node"), get(a, "sid"), get(a, "this"), get(a, "ep")),
        (
            aterm_json::Value::from("n-1"),
            aterm_json::Value::from("s-a"),
            aterm_json::Value::from(false),
            aterm_json::Value::from(1u64)
        ),
        "the newest first: {text}"
    );
    assert_eq!(get(a, "need").as_str(), Some("approval:bash"));
    assert!(get(a, "detail").is_null());
    let open: Vec<(String, String)> = get(a, "open")
        .as_array()
        .expect("the open path")
        .iter()
        .map(|n| {
            let w = |k: &str| n.get(k).and_then(aterm_json::Value::as_str).expect(k);
            (w("level").to_string(), w("kind").to_string())
        })
        .collect();
    let want: Vec<(String, String)> = [("run", "agent"), ("turn", "turn"), ("step", "approval")]
        .iter()
        .map(|(l, k)| ((*l).to_string(), (*k).to_string()))
        .collect();
    assert_eq!(open, want, "{text}");
    assert_eq!(get(a, "last_seq").as_u64(), Some(as_of("s-a")));
    assert_eq!(
        get(a, "age_s").as_u64(),
        Some(39),
        "100 s less the box's 60.003 s"
    );
    assert_eq!(
        (get(a, "mail_in").as_u64(), get(a, "mail_out").as_u64()),
        (Some(0), Some(0))
    );
    assert_eq!(get(a, "partial").as_array().map(Vec::len), Some(0));
    assert_eq!(
        (get(f, "sid").as_str(), get(f, "this").as_bool()),
        (Some("s-f"), Some(true))
    );
    assert_eq!(get(f, "need").as_str(), Some("failure"));
    assert_eq!(get(f, "open").as_array().map(Vec::len), Some(0), "{text}");
    assert_eq!(get(f, "last_seq").as_u64(), Some(as_of("s-f")));
    assert_eq!(get(f, "age_s").as_u64(), Some(99));
}

#[test]
fn the_json_brief_names_no_claimed_need() {
    let at = 1_760_000_000_000;
    let claim = |log: &mut Log, ms: u64| {
        log.mark(
            Mark::new(Kind::Need, Src::Mark, Conf::Claim)
                .by(Actor::Ctl)
                .fields(
                    Fields::new()
                        .word(aterm_slog::Key::Word, "help")
                        .expect("a need word"),
                ),
            Stamp::new(at + ms, ms),
        )
        .expect("a claim");
    };
    let mut only = Log::new(1);
    only.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        Stamp::new(at, 0),
    )
    .expect("opens");
    claim(&mut only, 5);
    assert_eq!(
        only.fold().state().top_need().map(|n| n.word),
        Some("help"),
        "PRECONDITION: the claim is the session's top need"
    );
    let mut beside = session(1, at);
    claim(&mut beside, 5);
    let r = read_of(&[
        ("n-1", "s-only", bodies(&mut only)),
        ("n-1", "s-beside", bodies(&mut beside)),
    ]);
    let text = render_json(&cfg(), &json_opts(), &r, at + 60_000, None);
    assert!(!text.contains("help"), "{text}");
    let v = json_of(&text);
    let need = |sid: &str| -> aterm_json::Value {
        sessions_of(&v)
            .iter()
            .find(|s| s.get("sid").and_then(aterm_json::Value::as_str) == Some(sid))
            .and_then(|s| s.get("need").cloned())
            .unwrap_or_else(|| panic!("{sid}'s need: {text}"))
    };
    assert!(need("s-only").is_null(), "a claim alone: {text}");
    assert_eq!(need("s-beside").as_str(), Some("failure"), "{text}");
}

/// NEITHER BRIEF PRINTS A HOST OR CONTENT FIELD A RAW BODY CARRIES (session log Phase 6 fix
/// unit F6, PRIV-3). Hand-written v=2 `ev` bodies, as a writer of another build could publish
/// them, go through the real ingest path (`Read::ingest`, then `BusSpace::offer`): a run open
/// carrying `cwd=`, `title=` and `from=`, an observed `need` mark (the meta `escalated`, the one
/// mark a fold raises as an observed need) carrying a Host and a Content key, and a
/// `seen` mark (a bus mark the text brief prints as a line) carrying both too. A presence row
/// carrying `attention=` and `title=` goes through `Read::presence`. Each is folded (the run, the
/// need, the `seen` line and the presence state are in the briefs), and neither brief prints a
/// sentinel or a key above `WireMeta`.
///
/// The guards are layered, and each has its own test with its own negative control: the receive
/// side's strip in `BusSpace::offer` is aterm-slog's tests/mirror.rs
/// `a_hostile_body_is_foreign_or_stripped_never_folded_as_is`, and the text renderer's mark-line
/// filter is tests/brief_budget.rs `a_mark_line_prints_no_key_above_wire_meta`. The text half here
/// fails only when both go. The JSON brief prints a fixed schema (`session_json`), so its leak
/// guard is the printed-key loop below, which fails on any key the bus does not carry.
#[test]
fn neither_brief_prints_a_host_or_content_field_a_raw_body_carries() {
    const T0: u64 = 1_760_000_000_000;
    let body = |seq: u64, rest: &str| format!("v=2 t={} ep=1 seq={seq} ms={seq} {rest}", T0 + seq);
    let ev = vec![
        body(1, "op=open node=- k=spawn src=aterm conf=exact"),
        body(
            2,
            "op=open node=r1 par=- k=agent agent=claude cwd=%2Fprivate%2Ftmp%2Fx%2FQZP6WORD1 \
             title=QZP6WORD2 from=QZP6WORD3 src=status conf=strong",
        ),
        body(
            3,
            "op=mark node=r1 k=need word=escalated cwd=%2Fprivate%2Ftmp%2Fx%2FQZP6WORD4 \
             title=QZP6WORD5 src=meta conf=strong",
        ),
        body(
            4,
            "op=mark node=- k=seen text=QZP6WORD6 attn_text=QZP6WORD7 detail=qzp6meta \
             src=status conf=strong",
        ),
    ];
    for b in &ev {
        let d = aterm_slog::decode_bus(b).unwrap_or_else(|e| panic!("{e:?}: {b}"));
        assert!(d.rec.is_bus_class(), "PRECONDITION: a bus record: {b}");
    }
    let mut r = read_of(&[("n-1", "s-a", ev)]);
    r.presence(
        "n-1",
        "s-a",
        b"v=1 t=1 state=live inc=1 attention=QZP6WORD8 title=QZP6WORD9",
    )
    .expect("a presence row");
    let now = T0 + 60_000;

    let text = render(&cfg(), &BriefOpts::default(), &r, now);
    assert!(!text.contains("QZP6WORD"), "{text}");
    assert!(text.contains("@s-a@n-1 live ep=1 "), "the presence: {text}");
    assert!(text.contains("- claude r1 "), "the run: {text}");
    assert!(text.contains(" need=escalated "), "the need: {text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("4 seen ") && l.contains(" detail=qzp6meta ")),
        "the seen line, its Wire Meta key printed: {text}"
    );
    for line in text.lines().skip(1) {
        for tok in line.split_whitespace() {
            let Some((k, _)) = tok.split_once('=') else {
                continue;
            };
            assert!(
                aterm_slog::Key::from_word(k).is_none_or(|key| key.tier() <= Tier::WireMeta),
                "`{k}` is no key the bus carries: {line}"
            );
        }
    }

    let json = render_json(&cfg(), &json_opts(), &r, now, None);
    assert!(!json.contains("QZP6WORD"), "{json}");
    let v = json_of(&json);
    let sessions = sessions_of(&v);
    assert_eq!(sessions.len(), 1, "{json}");
    assert_eq!(
        sessions[0].get("need").and_then(aterm_json::Value::as_str),
        Some("escalated"),
        "the need: {json}"
    );
    fn keys(v: &aterm_json::Value, out: &mut Vec<String>) {
        if let Some(m) = v.as_object() {
            for (k, v) in m {
                out.push(k.clone());
                keys(v, out);
            }
        } else if let Some(a) = v.as_array() {
            for v in a {
                keys(v, out);
            }
        }
    }
    let mut printed = Vec::new();
    keys(&sessions[0], &mut printed);
    for k in &printed {
        let carried = aterm_slog::Key::from_word(k).is_some_and(|key| key.tier() <= Tier::WireMeta);
        assert!(
            DERIVED.contains(&k.as_str()) || carried,
            "`{k}` is no key the bus carries: {json}"
        );
    }
}

#[test]
fn the_json_brief_is_held_to_the_budget() {
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
        budget: Some(300),
        ..json_opts()
    };
    let text = render_json(&cfg(), &opts, &r, 1_760_000_001_000, None);
    let v = json_of(&text);
    let n = |k: &str| v.get(k).and_then(aterm_json::Value::as_u64).expect(k);
    assert!(n("used") <= 300, "{text}");
    let sessions = sessions_of(&v);
    assert!(!sessions.is_empty(), "{text}");
    assert_eq!(sessions.len() as u64 + n("more"), 60, "{text}");
    assert_eq!(
        sessions[0].get("sid").and_then(aterm_json::Value::as_str),
        Some("s-59"),
        "the newest first"
    );
}

#[test]
fn a_follower_full_of_nodes_forgets_the_one_heard_from_least_recently() {
    let node = |i: usize| format!("n-{i:04}");
    let mut r = Read::following();
    for i in (0..SESSIONS_MAX).rev() {
        r.presence(&node(i), "node", b"v=1 t=1 state=live inc=1")
            .expect("a node row");
    }
    r.presence(&node(SESSIONS_MAX - 1), "node", b"v=1 t=2 state=live inc=2")
        .expect("the first node heard from again");
    r.presence(&node(SESSIONS_MAX - 2), "node", b"v=1 t=2 state=gone")
        .expect("the second goes");
    r.presence("n-new", "node", b"v=1 t=3 state=live inc=1")
        .expect("one node more");
    assert_eq!(
        r.node_inc.len() + r.nodes_gone.len(),
        SESSIONS_MAX,
        "held to its bound"
    );
    assert!(r.node_inc.contains_key("n-new"), "the new node is held");
    assert!(
        r.node_inc.contains_key(&node(0)),
        "the node whose id sorts first was heard from last: kept"
    );
    assert_eq!(
        r.node_inc.get(&node(SESSIONS_MAX - 1)),
        Some(&2),
        "a node heard from again is fresh"
    );
    assert!(
        r.nodes_gone.contains(&node(SESSIONS_MAX - 2)),
        "a gone node heard from recently is kept"
    );
    assert!(
        !r.node_inc.contains_key(&node(SESSIONS_MAX - 3)),
        "the one heard from least recently is forgotten"
    );
    assert_eq!(r.node_heard.len(), SESSIONS_MAX, "and its stamp with it");
}

/// The successor a relaunch's restore opens in the pane `prev` held (aterm-gui's
/// `note_restored` before its open): `open - restore prev=<prev> lineage=<prev>`.
fn successor(prev: &str, at: u64) -> Log {
    let mut log = Log::new(2);
    let f = Fields::new()
        .id(aterm_slog::Key::Prev, prev)
        .and_then(|f| f.id(aterm_slog::Key::Lineage, prev))
        .expect("the restore's fields");
    log.open_session(
        Kind::Restore,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        f,
        Stamp::new(at, 0),
    )
    .expect("opens");
    log
}

/// The `sessions` object of `sid` in a JSON brief.
fn session_obj(text: &str, sid: &str) -> aterm_json::Value {
    sessions_of(&json_of(text))
        .iter()
        .find(|s| s.get("sid").and_then(aterm_json::Value::as_str) == Some(sid))
        .cloned()
        .unwrap_or_else(|| panic!("{sid}: {text}"))
}

/// A session object's open path, `(level, kind)` outermost first.
fn open_of(s: &aterm_json::Value) -> Vec<(String, String)> {
    s.get("open")
        .and_then(aterm_json::Value::as_array)
        .expect("the open path")
        .iter()
        .map(|n| {
            let w = |k: &str| n.get(k).and_then(aterm_json::Value::as_str).expect(k);
            (w("level").to_string(), w("kind").to_string())
        })
        .collect()
}

/// A session object's `partial` words.
fn partial_of(s: &aterm_json::Value) -> Vec<String> {
    s.get("partial")
        .and_then(aterm_json::Value::as_array)
        .expect("partial")
        .iter()
        .filter_map(aterm_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

/// A session object's `closed` word (its close's `out=`), `None` while it is open.
fn closed_of(s: &aterm_json::Value) -> Option<String> {
    s.get("closed")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string)
}

/// A session object's `need` and `detail`.
fn need_of(s: &aterm_json::Value) -> (Option<String>, Option<String>) {
    let w = |k: &str| {
        s.get(k)
            .and_then(aterm_json::Value::as_str)
            .map(str::to_string)
    };
    (w("need"), w("detail"))
}

/// `in_approval`'s open path: an agent run, its turn and the box.
fn in_approval_path() -> Vec<(String, String)> {
    [("run", "agent"), ("turn", "turn"), ("step", "approval")]
        .iter()
        .map(|(l, k)| ((*l).to_string(), (*k).to_string()))
        .collect()
}

/// The text brief's lines of `sid`: its head, then its body, up to the next session's head.
fn lines_of(text: &str, sid: &str) -> Vec<String> {
    let mut lines = text
        .lines()
        .skip_while(|l| !l.starts_with(&format!("@{sid}@")))
        .map(str::to_string);
    let head = lines.next().unwrap_or_else(|| panic!("{sid}: {text}"));
    std::iter::once(head)
        .chain(lines.take_while(|l| !l.starts_with('@') && !l.starts_with("more=")))
        .collect()
}

/// A DEAD SESSION READS CLOSED, DERIVED AND NEVER WRITTEN (fix unit E9, finding E2E-5). A
/// session waits in a box under an open agent run and turn; its node's bridge dies (its will,
/// `away inc=1`), the node relaunches (`live inc=2`), and a later incarnation's ghost sweep
/// retires the session's row (`exited inc=3`), whose close never reached the bus. Until the
/// row says it exited, the will and the relaunch derive nothing (a reattach may restore it).
/// Then the brief's JSON reads `open []`, need `exited` detail `gone`, `partial` `no-close`
/// (the close was never on the bus) and `last_seq` the space's own head; the text reads
/// `closed out=gone` with no open turn. The derived records (`gap why=crash after=<head>`,
/// the box's, the turn's, the run's and the session's `gone` closes, by aterm) are numbered
/// past the head, and the space itself holds exactly what the bus carried. NEGATIVE CONTROL,
/// run: with no derivation the open path stays `[run agent, turn, step approval]`.
#[test]
fn a_session_a_later_incarnation_retired_reads_closed_out_gone_derived_never_written() {
    let at = 1_760_000_000_000;
    let now = at + 600_000;
    let mut dead = in_approval(at, "bash");
    let mut r = read_of(&[("n-1", "s-dead", bodies(&mut dead))]);
    let key = ("n-1".to_string(), "s-dead".to_string());
    let carried: Vec<aterm_slog::Rec> = r.sessions[&key].spaces[&1].records().cloned().collect();
    let head = carried.last().map(|r| r.seq).expect("records");
    for (sid, row) in [
        ("node", "v=1 t=1 state=live inc=1"),
        ("s-dead", "v=1 t=1 inc=1 state=live"),
        ("node", "v=1 t=2 state=away inc=1 fabric=disconnected"),
        ("node", "v=1 t=3 state=live inc=2"),
    ] {
        r.presence("n-1", sid, row.as_bytes()).expect("a row");
        let s = session_obj(&render_json(&cfg(), &json_opts(), &r, now, None), "s-dead");
        assert_eq!(
            open_of(&s),
            in_approval_path(),
            "after {row}: a will or a relaunch derives nothing"
        );
        assert_eq!(closed_of(&s), None, "after {row}: open, so not closed");
    }
    r.presence("n-1", "s-dead", b"v=1 t=4 inc=3 state=exited")
        .expect("the sweep's row");

    let json = render_json(&cfg(), &json_opts(), &r, now, None);
    let s = session_obj(&json, "s-dead");
    assert_eq!(open_of(&s), Vec::new(), "nothing open: {json}");
    assert_eq!(
        need_of(&s),
        (Some("exited".to_string()), Some("gone".to_string())),
        "{json}"
    );
    assert_eq!(partial_of(&s), ["no-close"], "{json}");
    assert_eq!(
        closed_of(&s).as_deref(),
        Some("gone"),
        "the derived close: {json}"
    );
    assert_eq!(
        s.get("last_seq").and_then(aterm_json::Value::as_u64),
        Some(head),
        "the space's own head, never a derived seq: {json}"
    );
    let text = render(&cfg(), &BriefOpts::default(), &r, now);
    let lines = lines_of(&text, "s-dead");
    assert!(
        lines[0].starts_with(&format!("@s-dead@n-1 exited ep=1 as_of={head} "))
            && lines[0].contains(" partial why=no-close"),
        "{text}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("- closed out=gone ")),
        "{text}"
    );
    assert!(
        lines.iter().all(|l| !l.contains(" open ")),
        "no turn or box reads open: {text}"
    );

    let f = settled_fold(&r, "n-1", &r.sessions[&key], Derive::Shown).expect("a fold");
    assert_eq!(f.died, Some(Died::Retired));
    let shape: Vec<(u64, Op, Lvl, Option<&str>)> = f
        .derived
        .iter()
        .map(|d| (d.seq, d.op, d.node.lvl, d.f.get_word(aterm_slog::Key::Out)))
        .collect();
    assert_eq!(
        shape,
        [
            (head + 1, Op::Gap, Lvl::Session, None),
            (head + 2, Op::Close, Lvl::Step, Some("gone")),
            (head + 3, Op::Close, Lvl::Turn, Some("gone")),
            (head + 4, Op::Close, Lvl::Run, Some("gone")),
            (head + 5, Op::Close, Lvl::Session, Some("gone")),
        ],
        "§5.4's crash closes, innermost first, past the head"
    );
    let gap = &f.derived[0].f;
    assert_eq!(
        (
            gap.get_word(aterm_slog::Key::Why),
            gap.get_u(aterm_slog::Key::After)
        ),
        (Some("crash"), Some(head))
    );
    assert!(
        f.derived
            .iter()
            .all(|d| (d.src, d.conf, d.by) == (Src::Aterm, Conf::Exact, Actor::Aterm)),
        "{:?}",
        f.derived
    );
    let held: Vec<aterm_slog::Rec> = r.sessions[&key].spaces[&1].records().cloned().collect();
    assert_eq!(
        held, carried,
        "the space holds what the bus carried, nothing derived"
    );
}

/// The rows a crash leaves on `node`'s faces for `sid`: its node and the session live under
/// `inc=1`, the node's will, and its relaunch live under `inc=2`, the session's row as the dead
/// bridge last wrote it.
const CRASH_ROWS: [(&str, &str); 4] = [
    ("node", "v=1 t=1 state=live inc=1"),
    ("", "v=1 t=1 inc=1 state=live"),
    ("node", "v=1 t=2 state=away inc=1 fabric=disconnected"),
    ("node", "v=1 t=3 state=live inc=2"),
];

/// [`CRASH_ROWS`] into `r` for `sid` of `node` (the empty sid is the session's own row).
fn crashed(r: &mut Read, node: &str, sid: &str) {
    for (who, row) in CRASH_ROWS {
        let who = if who.is_empty() { sid } else { who };
        r.presence(node, who, row.as_bytes()).expect("a row");
    }
}

/// A SUCCESSOR'S RESTORE NAMES A DEAD SESSION (fix unit E9). Its aterm died ([`CRASH_ROWS`]:
/// the session's row live under `inc=1`, its node's will, the node live under `inc=2`), and the
/// relaunch's new shell opens `restore prev=s-dead` on the dead session's node: the brief
/// derives s-dead's close (its `partial` says `node-gone`: the close never reached the bus, and
/// a restore is a relaunch of its node's), and keeps deriving it in a read narrowed to
/// `@s-dead`, which reads its node's other sessions' restores for it. A session of another
/// node naming the same sid derives nothing, nor does the successor itself, which folds as any
/// session. NEGATIVE CONTROLS, run: with no derivation, and with a narrowed read that skips
/// other sids' bodies, s-dead's open path stays `[run agent, turn, step approval]`.
#[test]
fn a_successors_restore_derives_its_predecessors_close_in_a_narrowed_read_too() {
    let at = 1_760_000_000_000;
    let now = at + 600_000;
    let dead = bodies(&mut in_approval(at, "bash"));
    let far = bodies(&mut successor("s-dead", at + 60_000));
    let next = bodies(&mut successor("s-dead", at + 61_000));
    assert!(
        next.iter()
            .any(|b| b.contains(" k=restore ") && b.contains(" prev=s-dead")),
        "PRECONDITION: the restore crosses the bus naming its predecessor: {next:?}"
    );
    let mut r = read_of(&[
        ("n-1", "s-dead", dead.clone()),
        ("n-2", "s-far", far.clone()),
    ]);
    crashed(&mut r, "n-1", "s-dead");
    let s = session_obj(&render_json(&cfg(), &json_opts(), &r, now, None), "s-dead");
    assert_eq!(
        open_of(&s),
        in_approval_path(),
        "another node's restore names another session"
    );
    let mut r = read_of(&[
        ("n-1", "s-dead", dead.clone()),
        ("n-2", "s-far", far),
        ("n-1", "s-next", next.clone()),
    ]);
    crashed(&mut r, "n-1", "s-dead");
    let json = render_json(&cfg(), &json_opts(), &r, now, None);
    let s = session_obj(&json, "s-dead");
    assert_eq!(open_of(&s), Vec::new(), "{json}");
    assert_eq!(
        need_of(&s),
        (Some("exited".to_string()), Some("gone".to_string())),
        "{json}"
    );
    assert_eq!(partial_of(&s), ["node-gone"], "{json}");
    let successor = session_obj(&json, "s-next");
    assert_eq!(
        (need_of(&successor), partial_of(&successor)),
        ((None, None), Vec::<String>::new()),
        "the successor folds as any session: {json}"
    );
    let key = ("n-1".to_string(), "s-dead".to_string());
    assert_eq!(
        settled_fold(&r, "n-1", &r.sessions[&key], Derive::Paged).and_then(|f| f.died),
        Some(Died::Restored),
        "the one signal a page rests on"
    );

    let mut narrow = Read {
        head: 1000,
        ..Read::default()
    };
    crashed(&mut narrow, "n-1", "s-dead");
    let mut off = 0;
    for (sid, bs) in [("s-dead", &dead), ("s-next", &next)] {
        for b in bs {
            off += 1;
            narrow
                .ingest(FLEET, Some("s-dead"), off, &face("n-1", sid), b.as_bytes())
                .expect("within the bound");
        }
    }
    assert_eq!(
        narrow.sessions.len(),
        1,
        "a narrowed read folds its one session"
    );
    let opts = BriefOpts {
        sid: Some("s-dead".to_string()),
        json: true,
        ..BriefOpts::default()
    };
    let s = session_obj(&render_json(&cfg(), &opts, &narrow, now, None), "s-dead");
    assert_eq!(open_of(&s), Vec::new(), "narrowed too");
    assert_eq!(partial_of(&s), ["node-gone"]);
}

/// A CLEAN CLOSE NEVER DERIVES (fix unit E9). A session whose close reached the bus reads
/// exactly as it did before E9, though a successor's restore names it and its row says it
/// exited under a later incarnation: the same JSON object as a read with neither signal, its
/// own close's `out=`, no `partial` word and no derived record. NEGATIVE CONTROL, run: deriving
/// over a fold that holds its close reads `closed out=gone` and differs.
#[test]
fn a_session_whose_close_reached_the_bus_reads_as_it_did_whatever_says_it_died() {
    let at = 1_760_000_000_000;
    let now = at + 600_000;
    let mut done = in_approval(at, "bash");
    done.close_session(
        Close::new(
            Out::Session(aterm_slog::SessionOut::ShellExit),
            Src::Shell,
            Conf::Exact,
        ),
        Stamp::new(at + 10, 10),
    )
    .expect("closes");
    let done = bodies(&mut done);
    let next = bodies(&mut successor("s-done", at + 60_000));
    let plain = read_of(&[("n-1", "s-done", done.clone())]);
    let want = session_obj(
        &render_json(&cfg(), &json_opts(), &plain, now, None),
        "s-done",
    );
    let mut r = read_of(&[("n-1", "s-done", done), ("n-1", "s-next", next)]);
    for (sid, row) in [
        ("s-done", "v=1 t=1 inc=1 state=live"),
        ("node", "v=1 t=3 state=live inc=3"),
        ("s-done", "v=1 t=4 inc=3 state=exited"),
    ] {
        r.presence("n-1", sid, row.as_bytes()).expect("a row");
    }
    let json = render_json(&cfg(), &json_opts(), &r, now, None);
    assert_eq!(session_obj(&json, "s-done"), want, "{json}");
    assert_eq!(need_of(&want), (Some("exited".to_string()), None));
    let f = settled_fold(
        &r,
        "n-1",
        &r.sessions[&("n-1".to_string(), "s-done".to_string())],
        Derive::Shown,
    )
    .expect("a fold");
    assert!(
        f.died.is_none() && f.derived.is_empty() && f.closed_on_bus(),
        "{:?}",
        f.derived
    );
    let text = render(&cfg(), &BriefOpts::default(), &r, now);
    let lines = lines_of(&text, "s-done");
    assert!(!lines[0].contains("partial"), "{text}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("- closed out=shell-exit ")),
        "its own close: {text}"
    );
}

/// A CLEAN QUIT'S CLOSE ON THE BUS READS CLOSED `out=app-quit`, NEVER GONE (fix unit Q1). A
/// session whose quit closed it on the bus before its bridge's `bye quit` reads its own close,
/// with no `partial` word and no derived record, whether or not the bridge's withdrawal landed
/// after it, though its node's will fired, the node relaunched and a successor's restore names
/// it: in the JSON read it is `closed` `app-quit` with no `partial` word, and in the text read
/// `- closed out=app-quit`. NEGATIVE CONTROLS, run: deriving over a fold that holds its close
/// reads `closed out=gone` for the read without the withdrawal; a JSON `closed` that is always
/// null fails the machine form's word.
#[test]
fn a_clean_quits_close_on_the_bus_reads_closed_out_app_quit_never_gone() {
    let at = 1_760_000_000_000;
    let now = at + 600_000;
    let mut quit = in_approval(at, "bash");
    quit.close_session(
        Close::new(
            Out::Session(aterm_slog::SessionOut::AppQuit),
            Src::Aterm,
            Conf::Exact,
        )
        .by(Actor::Aterm),
        Stamp::new(at + 10, 10),
    )
    .expect("the quit closes");
    let quit = bodies(&mut quit);
    let next = bodies(&mut successor("s-quit", at + 60_000));
    for withdrawn in [true, false] {
        let mut r = read_of(&[
            ("n-1", "s-quit", quit.clone()),
            ("n-1", "s-next", next.clone()),
        ]);
        let mut rows = vec![
            ("node", "v=1 t=1 state=live inc=1"),
            ("s-quit", "v=1 t=1 inc=1 state=live"),
        ];
        if withdrawn {
            rows.push(("s-quit", "v=1 t=2 inc=1 state=exited"));
        }
        rows.extend([
            ("node", "v=1 t=2 state=away inc=1 fabric=disconnected"),
            ("node", "v=1 t=3 state=live inc=2"),
        ]);
        for (sid, row) in rows {
            r.presence("n-1", sid, row.as_bytes()).expect("a row");
        }
        let f = settled_fold(
            &r,
            "n-1",
            &r.sessions[&("n-1".to_string(), "s-quit".to_string())],
            Derive::Paged,
        )
        .expect("a fold");
        assert!(
            f.died.is_none() && f.derived.is_empty() && f.closed_on_bus(),
            "withdrawn {withdrawn}: {:?}",
            f.derived
        );
        let json = render_json(&cfg(), &json_opts(), &r, now, None);
        let s = session_obj(&json, "s-quit");
        assert_eq!(
            partial_of(&s),
            Vec::<String>::new(),
            "withdrawn {withdrawn}: {json}"
        );
        assert_eq!(
            closed_of(&s).as_deref(),
            Some("app-quit"),
            "withdrawn {withdrawn}: the machine form says the quit's own close: {json}"
        );
        let text = render(&cfg(), &BriefOpts::default(), &r, now);
        let lines = lines_of(&text, "s-quit");
        assert!(
            !lines[0].contains("partial"),
            "withdrawn {withdrawn}: {text}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("- closed out=app-quit ")),
            "withdrawn {withdrawn}: its own close: {text}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("out=gone")),
            "withdrawn {withdrawn}: never gone: {text}"
        );
    }
}

/// NEITHER A WILL, NOR A RELAUNCH, NOR AN EXIT UNDER ITS OWN INCARNATION DERIVES A CLOSE (fix
/// unit E9). A node's will (a reattach may restore the session), the relaunch moment (the node
/// live under a newer `inc=`, the session's row not yet republished), an `exited` row written
/// under the incarnation that hosted it (its close may still be on its way), and an `exited`
/// row with no earlier row the read saw (it cannot tell which incarnation wrote it) each leave
/// the session's fold as the bus carried it, with the `partial` word that says why it is
/// judged gone. NEGATIVE CONTROL, run: an `exited` row that derives whatever incarnation
/// wrote it fails the last two cases (the first two read no signal [`died`] takes; the first
/// test's rows before the sweep's hold them too).
#[test]
fn neither_a_will_nor_a_relaunch_nor_an_exit_under_its_own_incarnation_derives_a_close() {
    let at = 1_760_000_000_000;
    let now = at + 600_000;
    let dead = bodies(&mut in_approval(at, "bash"));
    for (case, rows, why) in [
        (
            "a will",
            &[
                ("node", "v=1 t=1 state=live inc=1"),
                ("s-a", "v=1 t=1 inc=1 state=live"),
                ("node", "v=1 t=2 state=away inc=1 fabric=disconnected"),
            ][..],
            "node-gone",
        ),
        (
            "the relaunch moment",
            &[
                ("s-a", "v=1 t=1 inc=1 state=live"),
                ("node", "v=1 t=2 state=live inc=2"),
            ][..],
            "node-gone",
        ),
        (
            "an exit under its own incarnation",
            &[
                ("node", "v=1 t=1 state=live inc=1"),
                ("s-a", "v=1 t=1 inc=1 state=live"),
                ("s-a", "v=1 t=2 inc=1 state=exited"),
            ][..],
            "no-close",
        ),
        (
            "an exit with no earlier row",
            &[
                ("node", "v=1 t=1 state=live inc=3"),
                ("s-a", "v=1 t=2 inc=3 state=exited"),
            ][..],
            "no-close",
        ),
    ] {
        let mut r = read_of(&[("n-1", "s-a", dead.clone())]);
        for (sid, row) in rows {
            r.presence("n-1", sid, row.as_bytes()).expect("a row");
        }
        let json = render_json(&cfg(), &json_opts(), &r, now, None);
        let s = session_obj(&json, "s-a");
        assert_eq!(open_of(&s), in_approval_path(), "{case}: {json}");
        assert_eq!(
            need_of(&s),
            (Some("approval:bash".to_string()), None),
            "{case}: {json}"
        );
        assert_eq!(partial_of(&s), [why], "{case}: {json}");
        let f = settled_fold(
            &r,
            "n-1",
            &r.sessions[&("n-1".to_string(), "s-a".to_string())],
            Derive::Shown,
        )
        .expect("a fold");
        assert!(f.died.is_none() && f.derived.is_empty(), "{case}");
    }
}

/// A ONE-SHOT BRIEF READS WHICH INCARNATION HOSTED A SESSION (fix unit E9). The last-value
/// walk hands a read only a session's newest row, `exited inc=2`; the session lived under
/// `inc=1`, in a row the bus still holds. The read takes the window's presence rows for such a
/// session (`lived_history`), so a later incarnation's retire derives its close, in a read of
/// the fleet and in one narrowed to the session. NEGATIVE CONTROL, run: a read that takes no
/// earlier row leaves its open path `[run agent, turn, step approval]`.
#[cfg(unix)]
#[test]
fn a_one_shot_brief_reads_the_row_that_says_which_incarnation_hosted_a_session() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-brief-lived")
        .tempdir_in("/tmp")
        .expect("scratch");
    let sock = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _served = broker.serve(&sock).expect("serve the broker");
    let (mut conn, _closer) =
        transport::connect(&sock.to_string_lossy(), aterm_spec::wait::MUST_HAPPEN)
            .expect("connect");
    let at = 1_760_000_000_000;
    let mut seq = 0;
    let mut publish = |conn: &mut transport::Conn, subject: &str, body: &str| {
        seq += 1;
        conn.publish(91, seq, subject, body.as_bytes())
            .expect("publish");
    };
    let row = |sid: &str| format!("/f/{FLEET}/pub/n-a/{sid}/presence");
    publish(&mut conn, &row("node"), "v=1 t=1 state=live inc=1");
    publish(&mut conn, &row("s-dead"), "v=1 t=1 inc=1 state=live");
    for b in bodies(&mut in_approval(at, "bash")) {
        publish(&mut conn, &face("n-a", "s-dead"), &b);
    }
    publish(
        &mut conn,
        &row("node"),
        "v=1 t=2 state=away inc=1 fabric=disconnected",
    );
    publish(&mut conn, &row("node"), "v=1 t=3 state=live inc=2");
    publish(&mut conn, &row("s-dead"), "v=1 t=4 inc=2 state=exited");

    let mut c = cfg();
    c.broker = sock.to_string_lossy().into_owned();
    for opts in [
        BriefOpts::default(),
        BriefOpts {
            sid: Some("s-dead".to_string()),
            node: Some("n-a".to_string()),
            ..BriefOpts::default()
        },
    ] {
        let r = read(&c, &opts).expect("the brief reads");
        let s = &r.sessions[&("n-a".to_string(), "s-dead".to_string())];
        assert_eq!(
            (s.presence.as_deref(), s.presence_inc, s.lived_inc),
            (Some("exited"), Some(2), Some(1)),
            "{opts:?}"
        );
        let json = render_json(&c, &json_opts(), &r, at + 600_000, None);
        let obj = session_obj(&json, "s-dead");
        assert_eq!(open_of(&obj), Vec::new(), "{opts:?}: {json}");
        assert_eq!(partial_of(&obj), ["no-close"], "{opts:?}: {json}");
    }
}

/// A RESTORE SAYS A SESSION DIED ONLY WHEN ITS INCARNATION ENDED WITHOUT WITHDRAWING IT (fix
/// unit E9, the lander's review). aterm writes `restore prev=<sid>` whenever a new shell fills a
/// pane, so four sessions waiting in a box are each named by a successor's restore: s-crash's
/// aterm died ([`CRASH_ROWS`]), and its close is derived; s-quit's quit withdrew it (`exited`
/// under `inc=1`, the incarnation that hosted it) before its node's will and relaunch; s-tab's
/// closed tab was reopened under its own incarnation, which still lives; and the read knows no
/// incarnation that hosted s-none. Those three derive nothing, for a reader or a watch: their
/// open path stays `[run agent, turn, step approval]`, their need `approval:bash`, and only
/// s-quit is partial (`no-close`). NEGATIVE CONTROLS, run: a restore that counts whatever
/// withdrew the session derives s-quit's close; one that counts while the session's own
/// incarnation lives derives s-tab's; one that takes an unknown incarnation as ended derives
/// s-none's.
#[test]
fn a_restore_says_a_session_died_only_when_its_incarnation_ended_without_withdrawing_it() {
    let at = 1_760_000_000_000;
    let now = at + 600_000;
    let waiting = bodies(&mut in_approval(at, "bash"));
    let mut items = Vec::new();
    for (node, sid, next) in [
        ("n-1", "s-crash", "s-c2"),
        ("n-2", "s-quit", "s-q2"),
        ("n-3", "s-tab", "s-t2"),
        ("n-4", "s-none", "s-n2"),
    ] {
        items.push((node, sid, waiting.clone()));
        items.push((node, next, bodies(&mut successor(sid, at + 60_000))));
    }
    let mut r = read_of(&items);
    crashed(&mut r, "n-1", "s-crash");
    for (node, sid, row) in [
        ("n-2", "node", "v=1 t=1 state=live inc=1"),
        ("n-2", "s-quit", "v=1 t=1 inc=1 state=live"),
        ("n-2", "s-quit", "v=1 t=2 inc=1 state=exited"),
        (
            "n-2",
            "node",
            "v=1 t=2 state=away inc=1 fabric=disconnected",
        ),
        ("n-2", "node", "v=1 t=3 state=live inc=2"),
        ("n-3", "node", "v=1 t=1 state=live inc=1"),
        ("n-3", "s-tab", "v=1 t=1 inc=1 state=live"),
        ("n-4", "node", "v=1 t=3 state=live inc=2"),
    ] {
        r.presence(node, sid, row.as_bytes()).expect("a row");
    }
    let json = render_json(&cfg(), &json_opts(), &r, now, None);
    let text = render(&cfg(), &BriefOpts::default(), &r, now);
    let crash = session_obj(&json, "s-crash");
    assert_eq!(open_of(&crash), Vec::new(), "the crash derives: {json}");
    assert_eq!(
        need_of(&crash),
        (Some("exited".to_string()), Some("gone".to_string())),
        "{json}"
    );
    let crash_key = ("n-1".to_string(), "s-crash".to_string());
    assert_eq!(
        settled_fold(&r, "n-1", &r.sessions[&crash_key], Derive::Paged).and_then(|f| f.died),
        Some(Died::Restored)
    );
    for (node, sid, why) in [
        ("n-2", "s-quit", &["no-close"][..]),
        ("n-3", "s-tab", &[][..]),
        ("n-4", "s-none", &[][..]),
    ] {
        let s = session_obj(&json, sid);
        assert_eq!(open_of(&s), in_approval_path(), "{sid}: {json}");
        assert_eq!(
            need_of(&s),
            (Some("approval:bash".to_string()), None),
            "{sid}: {json}"
        );
        assert_eq!(partial_of(&s), why, "{sid}: {json}");
        assert!(
            lines_of(&text, sid)
                .iter()
                .all(|l| !l.contains("closed out=gone")),
            "{sid}: {text}"
        );
        let key = (node.to_string(), sid.to_string());
        for by in [Derive::Shown, Derive::Paged] {
            let f = settled_fold(&r, node, &r.sessions[&key], by).expect("a fold");
            assert!(f.died.is_none() && f.derived.is_empty(), "{sid} {by:?}");
        }
    }
}

/// A DERIVED CLOSE CLOSES EVERY NODE THE FOLD HOLDS OPEN, INNERMOST FIRST (fix unit E9, the
/// lander's review). A session runs a command and, beside it, an agent run with a turn open
/// under it (a new run hangs from the session even while another is open, so the two are
/// siblings); its aterm dies and a successor's restore names it. The derived records close the
/// turn, then the agent run, then the command's run, then the session, each past the one
/// before it, and the fold holds nothing open after them. NEGATIVE CONTROL, run: closing the
/// open path once leaves the command's run with no close.
#[test]
fn a_derived_close_closes_every_node_the_fold_holds_open_innermost_first() {
    let at = 1_760_000_000_000;
    let mut log = Log::new(1);
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
    log.open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), t(1))
        .expect("the command's run");
    let agent = log
        .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), t(2))
        .expect("the agent run");
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(agent),
        t(3),
    )
    .expect("the turn");
    let mut r = read_of(&[
        ("n-1", "s-two", bodies(&mut log)),
        (
            "n-1",
            "s-next",
            bodies(&mut successor("s-two", at + 60_000)),
        ),
    ]);
    crashed(&mut r, "n-1", "s-two");
    let s = &r.sessions[&("n-1".to_string(), "s-two".to_string())];
    let head = s.spaces[&1]
        .records()
        .map(|r| r.seq)
        .last()
        .expect("records");
    let f = settled_fold(&r, "n-1", s, Derive::Shown).expect("a fold");
    assert_eq!(f.died, Some(Died::Restored));
    let shape: Vec<(u64, Op, Lvl, Kind)> = f
        .derived
        .iter()
        .map(|d| (d.seq, d.op, d.node.lvl, d.k))
        .collect();
    assert_eq!(
        shape,
        [
            (head + 1, Op::Gap, Lvl::Session, Kind::Gap),
            (head + 2, Op::Close, Lvl::Turn, Kind::Turn),
            (head + 3, Op::Close, Lvl::Run, Kind::Agent),
            (head + 4, Op::Close, Lvl::Run, Kind::Cmd),
            (head + 5, Op::Close, Lvl::Session, Kind::Spawn),
        ],
        "every open node closes, innermost first"
    );
    assert!(
        f.fold.state().open.iter().all(Option::is_none) && f.fold.state().closed.is_some(),
        "{:?}",
        f.fold.state().open
    );
}

/// A DERIVED RECORD TAKES NO SEQ A BODY OF THE SPACE HOLDS (fix unit E9, the lander's review). A
/// dead session's space holds, past its last record this build folds, a body of a newer
/// writer's class (`k=zzznew`), which the space keeps unfolded. The derived gap is numbered past
/// that body's seq and names it as the last the space holds (`after=`), while the brief still
/// hands out the fold's own head (`as_of=`). NEGATIVE CONTROL, run: numbering past the folded
/// records alone gives the gap that body's seq.
#[test]
fn a_derived_record_takes_no_seq_a_body_of_the_space_holds() {
    let at = 1_760_000_000_000;
    let key = ("n-1".to_string(), "s-dead".to_string());
    let mut dead = bodies(&mut in_approval(at, "bash"));
    let plain = read_of(&[("n-1", "s-dead", dead.clone())]);
    let space = &plain.sessions[&key].spaces[&1];
    let as_of = space.fold_seeded().0.state().as_of;
    let head = space
        .records()
        .map(|r| r.seq)
        .last()
        .unwrap_or(0)
        .max(space.snap().map_or(0, |snap| snap.seq))
        .max(as_of);
    dead.push(format!(
        "v=2 t={} ep=1 seq={} ms={} op=mark node=- k=zzznew src=mark conf=claim",
        at + 9,
        head + 1,
        space.max_ms() + 1
    ));
    let mut r = read_of(&[
        ("n-1", "s-dead", dead),
        (
            "n-1",
            "s-next",
            bodies(&mut successor("s-dead", at + 60_000)),
        ),
    ]);
    crashed(&mut r, "n-1", "s-dead");
    let s = &r.sessions[&key];
    assert_eq!(
        (s.spaces[&1].foreign, s.spaces[&1].len()),
        (1, space.len()),
        "PRECONDITION: the space holds one body more, which it does not fold"
    );
    let f = settled_fold(&r, "n-1", s, Derive::Shown).expect("a fold");
    let gap = f.derived.first().expect("a derived gap");
    assert_eq!(
        (gap.seq, gap.f.get_u(aterm_slog::Key::After)),
        (head + 2, Some(head + 1)),
        "past every seq the space holds"
    );
    assert_eq!(f.as_of, as_of, "the fold's own head");
}

/// A ONE-SHOT BRIEF TELLS A QUIT'S WITHDRAWAL FROM A CRASH BY THE ROWS ITS WINDOW HOLDS (fix unit
/// E9, the lander's review). On n-a the aterm crashed: s-crash's row was later retired by the
/// next incarnation's sweep (`exited inc=2`), and s-given's too, whose row the bridge then gave
/// back (no last value). On n-b the aterm quit: its bridge withdrew s-quit and s-left (`exited
/// inc=1`, the incarnation that hosted them), and gave s-left's row back. A successor's restore
/// names each. The read takes the window's presence rows for each such session it knows no
/// hosting incarnation of, so s-crash and s-given read closed (their restore says they died),
/// and s-quit and s-left derive nothing (their quit withdrew them), in a read of the fleet and in
/// one narrowed to the sid. NEGATIVE CONTROLS, run: a read that takes the earlier rows only for
/// a session whose last row says `exited` leaves s-given open; one whose earlier rows record no
/// exit derives s-left's close.
#[cfg(unix)]
#[test]
fn a_one_shot_brief_tells_a_quits_withdrawal_from_a_crash_by_the_rows_its_window_holds() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-brief-quit")
        .tempdir_in("/tmp")
        .expect("scratch");
    let sock = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _served = broker.serve(&sock).expect("serve the broker");
    let (mut conn, _closer) =
        transport::connect(&sock.to_string_lossy(), aterm_spec::wait::MUST_HAPPEN)
            .expect("connect");
    let at = 1_760_000_000_000;
    let mut seq = 0;
    let mut publish = |conn: &mut transport::Conn, subject: &str, body: &str| {
        seq += 1;
        conn.publish(91, seq, subject, body.as_bytes())
            .expect("publish");
    };
    let row = |node: &str, sid: &str| format!("/f/{FLEET}/pub/{node}/{sid}/presence");
    let waiting = bodies(&mut in_approval(at, "bash"));
    for (node, sids) in [
        ("n-a", ["s-crash", "s-given"]),
        ("n-b", ["s-quit", "s-left"]),
    ] {
        publish(&mut conn, &row(node, "node"), "v=1 t=1 state=live inc=1");
        for sid in sids {
            publish(&mut conn, &row(node, sid), "v=1 t=1 inc=1 state=live");
            for b in &waiting {
                publish(&mut conn, &face(node, sid), b);
            }
        }
    }
    for sid in ["s-quit", "s-left"] {
        publish(&mut conn, &row("n-b", sid), "v=1 t=2 inc=1 state=exited");
    }
    for node in ["n-a", "n-b"] {
        publish(
            &mut conn,
            &row(node, "node"),
            "v=1 t=2 state=away inc=1 fabric=disconnected",
        );
        publish(&mut conn, &row(node, "node"), "v=1 t=3 state=live inc=2");
    }
    for (node, sid, next) in [
        ("n-a", "s-crash", "s-c2"),
        ("n-a", "s-given", "s-g2"),
        ("n-b", "s-quit", "s-q2"),
        ("n-b", "s-left", "s-l2"),
    ] {
        for b in bodies(&mut successor(sid, at + 60_000)) {
            publish(&mut conn, &face(node, next), &b);
        }
    }
    for sid in ["s-crash", "s-given"] {
        publish(&mut conn, &row("n-a", sid), "v=1 t=4 inc=2 state=exited");
    }
    for (node, sid) in [("n-a", "s-given"), ("n-b", "s-left")] {
        conn.retire_subject(91, &row(node, sid))
            .expect("the row given back");
    }

    let mut c = cfg();
    c.broker = sock.to_string_lossy().into_owned();
    for (node, sid, dies) in [
        ("n-a", "s-crash", true),
        ("n-a", "s-given", true),
        ("n-b", "s-quit", false),
        ("n-b", "s-left", false),
    ] {
        for opts in [
            BriefOpts::default(),
            BriefOpts {
                sid: Some(sid.to_string()),
                ..BriefOpts::default()
            },
        ] {
            let r = read(&c, &opts).expect("the brief reads");
            let s = &r.sessions[&(node.to_string(), sid.to_string())];
            assert_eq!(s.lived_inc, Some(1), "{sid} {opts:?}");
            let f = settled_fold(&r, node, s, Derive::Paged).expect("a fold");
            assert_eq!(
                f.died,
                dies.then_some(Died::Restored),
                "{sid} {opts:?}: {:?}",
                (s.presence.as_deref(), s.exited_inc)
            );
            let json = render_json(&c, &json_opts(), &r, at + 600_000, None);
            let obj = session_obj(&json, sid);
            let open = if dies { Vec::new() } else { in_approval_path() };
            assert_eq!(open_of(&obj), open, "{sid} {opts:?}: {json}");
        }
    }
}
