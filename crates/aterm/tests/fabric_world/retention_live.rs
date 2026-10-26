// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BROKER'S OWN CUT, UNDER LIVE TRAFFIC (B04 B3): a world's broker retained by aterm's
//! rules scaled to a few records (`aterm_link::broker_rules::rules_scaled`: every filter,
//! class, reader and age of the rules `aterm link broker` opens with, the bytes small), its
//! retention tick every 100 ms (`Broker::set_retention_interval`) instead of astream's
//! minute, so the real ticker cuts while the bridge, the mirror and the sessions run. No
//! test cut: every compaction is the tick's, state-carrying, the one production makes.
//!
//! Every wait is a one-minute hang detector (the ring's eviction, which the bridge's drain
//! paces, the periodic one). A reader that wedges under a cut fails a wait here. A walk the
//! broker refuses past the cut takes the link down, as `Bridge::judge` does with any refusal
//! of a read it names, and the same bridge redials: the node log's link marks show that
//! move, though the end state would not. And the read of an evicted mail, which must answer
//! `retained-from=`, fails on a page that starts later. The cursor readers that read on from
//! the floor (`transport::fetch_kept`) clamp to it by design, and that passes.

use std::time::Duration;

use super::harness::{
    FLEET, PERIODIC_DEADLINE, World, broker_opts, kv, presence_body, until, until_within,
};

/// Every filler record's bytes: the bulk stream keeps eight of them.
const BODY: usize = 1024;
/// More mail than a session's ring holds (`aterm_gui_fabric::RING_CAP`, 512), so its oldest
/// row is evicted and `inbox get @<off>` goes to the broker for it.
const MAILS: u64 = 560;

/// The bridge's retire refusals, each by two fragments of the line it prints: the mirror's
/// (`bridge_mirror.rs`, `mirror_retire_face`) and the reclaim's two (`Bridge::retire_held`).
const RETIRE_REFUSED: [(&str, &str); 3] = [
    ("aterm-link: mirror: retiring ", " failed: "),
    ("aterm-link: could not give ", " back: "),
    (
        "aterm-link: this log's retention cut keeps every subject charged",
        "",
    ),
];

/// The broker's base, as its stats say.
fn base(w: &World) -> u64 {
    w.broker
        .as_ref()
        .expect("the world's own broker")
        .stats()
        .log
        .base
        .0
}

/// The node log's cursor now, as `since=<seq> ep=<epoch>`.
fn node_log_cursor(w: &World) -> String {
    let r = w.verb("log n n=1");
    let h = r.header();
    assert!(r.ok(), "log n: {h}");
    let field = |k: &str| kv(h, k).unwrap_or_else(|| panic!("log n names no {k}=: {h}"));
    format!("since={} ep={}", field("as_of"), field("ep"))
}

/// Every link mark the node log wrote past `cursor`: each stored move of the link, from the
/// bridge's own `link down` or `link up`, its lane's loss or a new incarnation's attach. A
/// reset of the cursor answers a `gap` row, which counts too.
fn link_moves_since(w: &World, cursor: &str) -> Vec<String> {
    let r = w.verb(&format!("log n kinds=link {cursor}"));
    assert!(r.ok(), "log n kinds=link {cursor}: {}", r.header());
    r.rows().to_vec()
}

/// **EVERY READER READS ON PAST THE BROKER'S OWN CUT**: two sessions; 560 mails to `a`, which
/// the bridge drains (the node's inbox group commits past them) until `a`'s ring evicts the
/// oldest; then the bulk stream past its bytes, and the tick cuts past every mail by itself.
/// After the cut:
/// - `inbox get @<oldest>`, evicted from the ring and read by the node, answers
///   `ERR retained-from=<floor>` above it, and so does a broker fetch of `a`'s lane from 0:
///   the read mail went with the cut, said, never a page that starts later;
/// - a mail posted after the cut lands and is delivered (the bridge's cursors below the
///   cut read on), and `inbox get <id>` reads it, unread, from the ring;
/// - presence still routes: both sessions' rows and the node's say live;
/// - `b` closes: its `ev` face and its presence row are retired (the mirror's and the
///   reclaim's retires). An uncarried cut would refuse them for good, and the wait fails;
///   a refusal the reclaim asks past later prints a line of [`RETIRE_REFUSED`], and the
///   closing scan fails;
/// - the link never dropped: the node log holds no link mark from before the first mail to
///   the end, the bridge is the same process, `fabric=connected`, and the mirror on.
///
/// PLANTED CONTROLS (run, scratch-reverted):
/// - a bridge that reads no `retained-from=` refusal (`transport::retained_past` answering
///   none) takes the fetch's refusal for the link down, and `inbox get @<oldest>` answers
///   `ERR fabric stalled` instead;
/// - a bridge that takes the link down once after the cut through `Bridge::judge`, as a
///   refused walk does (planted at the reclaim's first retire), and redials passes every
///   check before the node log's, which holds a `link state=stalled` and a `link
///   state=connected` mark;
/// - a reclaim whose first retire is refused and asked again (`Bridge::retire_held`
///   answering none once, its `could not give … back` line printed) passes every wait and
///   fails the closing scan.
#[test]
fn every_reader_reads_on_past_the_brokers_own_cut() {
    // The broker `aterm fabric on` runs: guarded, Ordered with aterm's escalation table.
    let w = World::boot_arm(
        "rlive",
        broker_opts(aterm_link::durability::Durability::Ordered),
        true,
        &[],
        &[("ATERM_LINK_RECLAIM_AFTER_MS", "200")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    for sid in [&a, &b] {
        until(&format!("{sid}'s presence row to say live"), || {
            presence_body(&w, sid)
                .filter(|p| p.contains("state=live"))
                .map(|_| ())
        });
    }
    until("the mirror to run over the retaining broker", || {
        w.verb("fabric status")
            .header()
            .contains(" mirror=on")
            .then_some(())
    });
    let bridge = w.bridge_pid();
    let link_since = node_log_cursor(&w);
    let broker = w.broker.as_ref().expect("the world's own broker");
    broker
        .set_retention(aterm_link::broker_rules::rules_scaled(
            8 * BODY as u64,
            64 * 1024 * 1024,
            32 * 1024 * 1024,
        ))
        .expect("the scaled rules");
    broker.set_retention_interval(Duration::from_millis(100));

    // From sixteen senders, each within a session's quota of one sender's rows
    // (`aterm_gui_fabric::SENDER_QUOTA`, 64).
    const SRCS: u64 = 16;
    let first_text = "the-oldest-mail-the-node-read";
    let mut seqs = [0u64; SRCS as usize];
    let mails: Vec<super::harness::Record> = (0..MAILS)
        .map(|i| {
            let s = (i % SRCS) as usize;
            seqs[s] += 1;
            let src = format!("a-fill{s}");
            let mut body = aterm_link::body::Body::new(0);
            body.text = if i == 0 {
                first_text.to_string()
            } else {
                format!("filler-{i}")
            };
            (
                astream_cap::producer_id_of(&src),
                seqs[s],
                format!("/f/{FLEET}/in/{}/{a}/{src}/note", w.node),
                body.encode(),
            )
        })
        .collect();
    let offs = super::harness::publish_pipelined(&mut w.god(), &mails);
    let (first, last) = (offs[0].0, offs[offs.len() - 1].0);
    until_within(
        PERIODIC_DEADLINE,
        "every mail delivered to a, its ring past its oldest",
        || {
            let h = w.verb(&format!("@{a} inbox --peek")).header().to_string();
            let n = |k: &str| kv(&h, k).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
            let held: u64 = h
                .strip_prefix("OK ")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            (n("dropped") > 0 && n("dropped") + held >= MAILS).then_some(())
        },
    );

    let filler: Vec<super::harness::Record> = (1..=64u64)
        .map(|seq| {
            (
                astream_cap::producer_id_of("n-filler"),
                seq,
                format!("/f/{FLEET}/pub/n-filler/s-filler/ev"),
                vec![b'e'; BODY],
            )
        })
        .collect();
    super::harness::publish_pipelined(&mut w.god(), &filler);
    until("the broker's own tick to cut past every mail", || {
        (base(&w) > last).then_some(())
    });

    // The node's inbox group commits once the endpoint took a delivery; a tick after that
    // commit gives the read mail up (one before it holds it, unread), so the answer turns.
    let floor = until(
        "inbox get of the evicted, read mail to answer retained-from=",
        || {
            let evicted = w.verb(&format!("@{a} inbox get @{first}"));
            let header = evicted.header().to_string();
            if evicted.ok() {
                assert_eq!(
                    String::from_utf8_lossy(evicted.body()),
                    first_text,
                    "held, it reads whole: {header}"
                );
                return None;
            }
            let floor = header
                .strip_prefix("ERR retained-from=")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or_else(|| panic!("inbox get @{first}: {header} — log:\n{}", w.log_tail()));
            Some(floor)
        },
    );
    assert!(floor > first, "retained-from={floor} above @{first}");
    let refused = w
        .read(|c| c.fetch(0, &format!("/f/{FLEET}/in/{}/{a}/>", w.node), 8))
        .expect_err("a fetch of the read mail from 0 is refused");
    let from = astream_broker::broker_refusal(&refused)
        .and_then(astream_broker::BrokerRefusal::retained_from)
        .unwrap_or_else(|| panic!("a fetch below the cut: {refused}"));
    assert!(from > first, "retained-from={from}");

    let (_, unread) = w.post_landed(&b, &a, "note", "", "unread-after-the-cut");
    let row = until("the post after the cut in a's inbox", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| kv(r, "off") == Some(unread.to_string().as_str()))
    });
    let id = row
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("a msg row's id: {row}"))
        .to_string();
    let got = w.verb(&format!("@{a} inbox get {id}"));
    assert!(got.ok(), "inbox get {id}: {}", got.header());
    assert_eq!(String::from_utf8_lossy(got.body()), "unread-after-the-cut");

    for sid in [&a, &b] {
        let p = presence_body(&w, sid).unwrap_or_default();
        assert!(p.contains("state=live"), "{sid}'s presence: {p:?}");
    }
    let node = w
        .row(&format!("/f/{FLEET}/pub/{}/node/presence", w.node))
        .unwrap_or_default();
    assert!(node.contains("state=live"), "the node's presence: {node:?}");

    let face = format!("/f/{FLEET}/pub/{}/{b}/ev", w.node);
    until("b's face on the bus before it closes", || {
        w.row(&face).map(|_| ())
    });
    assert!(w.verb(&format!("@{b} close")).ok(), "close {b}");
    until("b's ev face and presence row retired", || {
        (w.row(&face).is_none() && presence_body(&w, &b).is_none()).then_some(())
    });

    assert_eq!(w.bridge_pid(), bridge, "the same bridge: it never died");
    let moves = link_moves_since(&w, &link_since);
    assert!(
        moves.is_empty(),
        "the link moved under the cut ({link_since}): {moves:#?}\n{}",
        w.log_tail()
    );
    let fs = w.verb("fabric status").header().to_string();
    assert_eq!(kv(&fs, "state"), Some("connected"), "{fs}");
    assert!(fs.contains(" mirror=on"), "the mirror runs on: {fs}");
    let log = std::fs::read_to_string(&w.gui_log).unwrap_or_default();
    let refused: Vec<&str> = log
        .lines()
        .filter(|l| {
            RETIRE_REFUSED
                .iter()
                .any(|(head, tail)| l.contains(head) && l.contains(tail))
        })
        .collect();
    assert!(
        refused.is_empty(),
        "a refused retire in the log: {refused:#?}"
    );
}
