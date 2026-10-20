// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node log's own laws (5J-1, ruling T27): what it takes, its bound and
//! exact counts, its cursor resets, and that none of its records is a bus
//! or turn record (D3, D4: local, additive, RAM only).

use aterm_slog::{
    Actor, Conf, Cursor, EncodeError, Fields, GapWhy, Key, Kind, Mark, NODE_LOG_RECORDS, Node,
    NodeLog, Refused, Src, Tier, decode, encode, encode_mirror,
};

use crate::support::at;

const EP: u32 = 1_760_000_000;

fn link(state: &str, generation: &str) -> Mark {
    Mark::new(Kind::Link, Src::Fabric, Conf::Exact)
        .by(Actor::Bridge)
        .fields(
            Fields::new()
                .word(Key::State, state)
                .and_then(|f| f.id(Key::Gen, generation))
                .expect("words"),
        )
}

fn config(key: &str) -> Mark {
    Mark::new(Kind::Config, Src::Aterm, Conf::Exact)
        .by(Actor::Aterm)
        .fields(Fields::new().id(Key::CfgKey, key).expect("a key name"))
}

fn started(cap: usize) -> NodeLog {
    let mut log = NodeLog::bounded(EP, cap);
    let f = Fields::new().word(Key::PrevEnd, "-").expect("a word");
    assert_eq!(log.start(f, at(0)), Ok(1));
    log
}

/// The start is the first record, once; marks wait for it; only `link` and
/// `config` marks are taken, and only on node `n`; the log owns its counts.
#[test]
fn node_log_takes_its_start_first_then_link_and_config_marks() {
    let mut log = NodeLog::new(EP);
    assert_eq!(log.capacity(), NODE_LOG_RECORDS);
    assert_eq!(
        log.mark(link("connected", "1"), at(1)),
        Err(Refused::NotOpen(Node::INSTANCE)),
        "no mark before the start"
    );
    assert_eq!(
        log.amend_start(Fields::new(), at(1)),
        Err(Refused::NotIssued(Node::INSTANCE))
    );
    let f = Fields::new().word(Key::PrevEnd, "-").expect("a word");
    assert_eq!(log.start(f.clone(), at(1)), Ok(1));
    assert_eq!(log.start(f, at(2)), Err(Refused::AlreadyOpen), "once");
    let amend = Fields::new()
        .word(Key::PrevEnd, "signal")
        .and_then(|f| f.i(Key::Code, 11))
        .expect("words");
    assert_eq!(log.amend_start(amend, at(3)), Ok(2));
    assert_eq!(log.mark(link("stalled", "2"), at(4)), Ok(3));
    assert_eq!(log.mark(config("privacy.enabled"), at(5)), Ok(4));
    for kind in [Kind::Bell, Kind::Need, Kind::Mail, Kind::Task, Kind::Start] {
        assert_eq!(
            log.mark(Mark::new(kind, Src::Aterm, Conf::Exact), at(6)),
            Err(Refused::KindDoesNotFit(kind)),
            "{kind:?} is no node mark"
        );
    }
    assert_eq!(
        log.mark(link("connected", "3").at(Node::SESSION), at(6)),
        Err(Refused::NotIssued(Node::SESSION)),
        "a node mark is node n's"
    );
    assert_eq!(log.last_seq(), 4, "refusals changed nothing");
    let lines: Vec<String> = log
        .window()
        .map(|r| encode(r, Tier::Content).expect("fits"))
        .collect();
    let t = |ms: u64| crate::support::WALL0 + ms;
    assert_eq!(
        lines,
        [
            format!(
                "1 open n start prev_end=- by=aterm src=aterm conf=exact t={}",
                t(1)
            ),
            format!(
                "2 amend n start prev_end=signal code=11 by=aterm src=aterm conf=exact t={}",
                t(3)
            ),
            format!(
                "3 mark n link state=stalled gen=2 by=bridge src=fabric conf=exact t={}",
                t(4)
            ),
            format!(
                "4 mark n config key=privacy.enabled by=aterm src=aterm conf=exact t={}",
                t(5)
            ),
        ]
    );
    for (r, line) in log.window().zip(&lines) {
        assert_eq!(decode(line).as_ref(), Ok(r), "{line}");
    }
}

/// D3 and D4 (T27): no node-log record is a turn record or a bus record,
/// so the bus mirror's encoder refuses each one, whatever its fields.
#[test]
fn no_node_log_record_is_bus_or_turn_class() {
    let mut log = started(8);
    let amend = Fields::new().word(Key::PrevEnd, "clean").expect("a word");
    log.amend_start(amend, at(1)).expect("amended");
    for state in ["connected", "disconnected", "stalled", "absent"] {
        log.mark(link(state, "7"), at(2)).expect("a link state");
    }
    log.mark(config("font.size"), at(3)).expect("a config key");
    assert_eq!(log.len(), 7);
    let mut ops = std::collections::BTreeSet::new();
    for r in log.window() {
        assert_eq!(r.node, Node::INSTANCE);
        assert!(!r.is_bus_class() && !r.is_turn_class(), "{r:?}");
        assert_eq!(
            encode_mirror(r, Tier::WireMeta, 1, Some(1), &|_| None),
            Err(EncodeError::NotBusClass),
            "{r:?}"
        );
        ops.insert(format!("{:?}", r.op));
    }
    assert_eq!(ops.len(), 3, "open, amend and mark all checked: {ops:?}");
}

/// The bound: past [`NODE_LOG_RECORDS`] the oldest goes, counted; a reader
/// from nothing is told exactly the evicted, a reader cut by `max` is told
/// only what its read covered, and a later read nothing twice.
#[test]
fn node_log_is_bounded_and_counts_every_eviction_once() {
    let mut log = started(NODE_LOG_RECORDS);
    for i in 0..NODE_LOG_RECORDS as u64 + 9 {
        log.mark(link("connected", &i.to_string()), at(i + 1))
            .expect("a mark");
    }
    assert_eq!(log.len(), NODE_LOG_RECORDS);
    assert_eq!(log.lost(), 10, "the start and nine marks went");
    assert_eq!(log.oldest_seq(), 11);
    let all = log.read(Cursor::new(EP, 0), usize::MAX);
    let g = all.gap.expect("told");
    assert_eq!((g.why, g.lost, g.resync_to), (GapWhy::Evicted, 10, 10));
    assert_eq!(all.recs.len(), NODE_LOG_RECORDS);
    assert_eq!(all.next, Cursor::new(EP, log.last_seq()));

    let cut = log.read(Cursor::new(EP, 4), 3);
    assert_eq!(cut.gap.map(|g| g.lost), Some(6), "seqs 5..=10 only");
    assert_eq!(
        cut.recs.iter().map(|r| r.seq).collect::<Vec<_>>(),
        [11, 12, 13]
    );
    let after = log.read(cut.next, 3);
    assert_eq!(after.gap, None, "nothing twice");
    assert_eq!(after.recs[0].seq, 14);

    // A probe (`max = 0`) says the gap ahead and moves past it.
    let probe = log.read(Cursor::new(EP, 0), 0);
    assert_eq!(probe.gap.map(|g| (g.lost, g.resync_to)), Some((10, 10)));
    assert!(probe.recs.is_empty());
    assert_eq!(probe.next.seq, 10);
}

/// A cursor ahead of the head, or of another epoch with a seq in it, is
/// another seq space: reset, every live record from the oldest, its count
/// every evicted seq. A cursor at seq 0 has read nothing, whatever epoch.
#[test]
fn a_cursor_of_another_process_reads_reset() {
    let mut log = started(2);
    for g in ["1", "2"] {
        log.mark(link("connected", g), at(1)).expect("a mark");
    }
    for c in [
        Cursor::new(EP - 1, 2),
        Cursor::new(EP, 9),
        Cursor::new(EP + 1, 1),
    ] {
        let r = log.read(c, usize::MAX);
        let g = r.gap.expect("reset");
        assert_eq!((g.why, g.lost), (GapWhy::Reset, 1), "{c:?}");
        assert_eq!(r.recs.iter().map(|r| r.seq).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(r.next, Cursor::new(EP, 3));
    }
    let fresh = log.read(Cursor::new(0, 0), usize::MAX);
    assert_eq!(fresh.gap.map(|g| g.why), Some(GapWhy::Evicted), "not reset");
    let current = log.read(Cursor::new(EP, 3), usize::MAX);
    assert_eq!((current.gap, current.recs.len()), (None, 0));
}
