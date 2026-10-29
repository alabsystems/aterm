// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end tests for inbox ack receipts (`inbox seen`, `post --wait-ack`) and `inbox get @<off>` retention.
#![cfg(unix)]

use crate::harness;

use harness::{FLEET, PERIODIC_DEADLINE, World, id_at_off, kv, off_of, until, until_within};

/// How many records sit on one lane, by `Fetch` from zero — the authority a
/// duplicate or a missing publish cannot hide behind.
fn lane_len(w: &World, lane: &str) -> usize {
    let (rows, _) = w.read(|c| c.fetch(0, lane, 256)).expect("fetch the lane");
    rows.len()
}

/// The `msg` rows of `sid`'s inbox whose text names `off` as their `re=`.
fn replies_to(w: &World, sid: &str, off: u64) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| kv(r, "re") == Some(&off.to_string()))
        .collect()
}

/// **A `handled` VERDICT ON AN ASK ACKS THE SENDER WITH ONE `ack re=<off>`, AND
/// `refused`/`deferred` CARRY THEIR OWN WORD.**
#[test]
fn a_verdict_on_an_ask_acks_the_sender_once_and_a_note_acks_nothing() {
    let w = World::boot("r15ack", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);

    let mut off = Vec::new();
    for q in ["q-handled", "q-refused", "q-deferred"] {
        let r = w.verb(&format!("@{a} post to=@{b} kind=ask --wait=60000 {q}"));
        assert!(r.ok(), "ask {q}: {} — log:\n{}", r.header(), w.log_tail());
        off.push(off_of(r.header()));
    }
    let note = w.verb(&format!("@{a} post to=@{b} kind=note --wait=60000 fyi"));
    assert!(note.ok(), "note: {}", note.header());
    let note_off = off_of(note.header());

    let _first_id = until("the first ask to reach B", || id_at_off(&w, &b, off[0]));
    assert!(w.verb(&format!("@{b} inbox --peek")).ok());
    w.drained(&b);
    assert_eq!(lane_len(&w, &ack_lane), 0, "inbox --peek earns no receipt");

    for (i, verdict) in ["handled", "refused", "deferred"].iter().enumerate() {
        let id = until("the ask to reach B", || id_at_off(&w, &b, off[i]));
        let seen = w.verb(&format!("@{b} inbox seen {id} {verdict}"));
        assert!(seen.ok(), "inbox seen {id} {verdict}: {}", seen.header());
    }
    let note_id = until("the note to reach B", || id_at_off(&w, &b, note_off));
    assert!(w.verb(&format!("@{b} inbox seen {note_id} handled")).ok());

    for (i, verdict) in ["handled", "refused", "deferred"].iter().enumerate() {
        let row = until(&format!("the {verdict} receipt to reach A"), || {
            replies_to(&w, &a, off[i])
                .into_iter()
                .find(|r| kv(r, "kind") == Some("ack"))
        });
        assert!(
            row.contains(&format!("verdict={verdict}")) && row.contains("re-id="),
            "the ack carries the verdict and resolves to A's own post: {row}"
        );
    }
    assert_eq!(
        lane_len(&w, &ack_lane),
        3,
        "exactly one ack per decision, never a repeat"
    );
    assert!(
        replies_to(&w, &a, note_off)
            .iter()
            .all(|r| kv(r, "kind") != Some("ack")),
        "a note earns no receipt"
    );

    let latched = w.verb(&format!("@{a} await inbox re={} timeout=60000", off[0]));
    assert!(
        latched.header().starts_with("OK inbox "),
        "await inbox re= latches on the ack: {}",
        latched.header()
    );

    let first_ask = id_at_off(&w, &b, off[0]).expect("the first ask row");
    assert!(w.verb(&format!("@{b} inbox seen {first_ask} handled")).ok());
    w.drained(&b);
    assert_eq!(
        lane_len(&w, &ack_lane),
        3,
        "the same verdict again (and the note's) publishes no further ack"
    );
}

/// **`post … --wait-ack` RETURNS THE RECIPIENT'S VERDICT, END TO END.** A posts
/// an ask and blocks for the receipt; a helper connection, as B, finds the row
/// and marks it `handled`; the parked post answers `OK <id> off=<n>
/// ack=handled msg=<row>`.
#[test]
fn post_wait_ack_returns_the_recipients_verdict() {
    let w = World::boot("r15waitack", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let sock = w.ctl_sock.clone();
    let token = w.token.clone();
    let b_sid = b.clone();
    let acker = std::thread::spawn(move || {
        let id = until("the ask to reach B", || {
            let mut ctl = harness::connect(&sock, &token).expect("connect");
            let listing = ctl
                .request(&format!("@{b_sid} inbox --peek"))
                .expect("inbox");
            listing.rows().iter().rev().find_map(|r| {
                (kv(r, "kind") == Some("ask"))
                    .then(|| r.split_whitespace().nth(1)?.parse::<u64>().ok())
                    .flatten()
            })
        });
        let mut ctl = harness::connect(&sock, &token).expect("connect");
        let seen = ctl
            .request(&format!("@{b_sid} inbox seen {id} handled"))
            .expect("inbox seen");
        assert!(seen.ok(), "inbox seen: {}", seen.header());
    });

    let posted = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl=60000 --wait-ack=60000 ship it"
    ));
    acker.join().expect("the acker finished");
    assert!(
        posted.ok() && posted.header().contains("ack=handled") && posted.header().contains("msg="),
        "the parked post returns the recipient's verdict: {} — log:\n{}",
        posted.header(),
        w.log_tail()
    );
}

/// **A BRIDGE TOLD `--no-receipts` SENDS NO ACK.** The same `inbox seen
/// handled` on an ask that acks by default publishes nothing once the flag
/// turns it off — the decision is still recorded locally, the sender simply
/// never hears it.
#[test]
fn receipts_off_sends_no_ack() {
    let w = World::boot_flags("r15noreceipts", &["--no-receipts"]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);

    let asked = w.verb(&format!("@{a} post to=@{b} kind=ask --wait=60000 anyone?"));
    assert!(
        asked.ok(),
        "ask: {} — log:\n{}",
        asked.header(),
        w.log_tail()
    );
    let off = off_of(asked.header());

    let id = until("the ask to reach B", || id_at_off(&w, &b, off));
    assert!(w.verb(&format!("@{b} inbox seen {id} handled")).ok());

    w.drained(&b);
    assert_eq!(
        lane_len(&w, &ack_lane),
        0,
        "receipts off: no ack lane record"
    );
    assert!(
        replies_to(&w, &a, off)
            .iter()
            .all(|r| kv(r, "kind") != Some("ack")),
        "receipts off: nothing in the sender's inbox"
    );
}

/// **`inbox get @<off>` FETCHES A RECORD THE BOUNDED RING DROPPED, THE HEADER
/// NAMES THE OLDEST OFFSET STILL FETCHABLE, AND ANOTHER SESSION'S RECORD IS
/// REFUSED (E3).**
#[test]
fn inbox_get_at_an_offset_fetches_a_record_the_ring_dropped() {
    let w = World::boot("r15get", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    const SRCS: u64 = 16;
    let total: u64 = 560;
    let first_text = "the-oldest-body-please-return-me-in-full";
    let first_off = {
        let mut seq = [0u64; SRCS as usize];
        let fill: Vec<harness::Record> = (0..total)
            .map(|i| {
                let s = (i % SRCS) as usize;
                seq[s] += 1;
                let src = format!("a-fill{s}");
                let lane = format!("/f/{FLEET}/in/{}/{b}/{src}/note", w.node);
                let mut body = aterm_link::body::Body::new(0);
                body.text = if i == 0 {
                    first_text.to_string()
                } else {
                    format!("filler-{i}")
                };
                (
                    astream_cap::producer_id_of(&src),
                    seq[s],
                    lane,
                    body.encode(),
                )
            })
            .collect();
        harness::publish_pipelined(&mut w.god(), &fill)[0].0
    };
    let header = until_within(
        PERIODIC_DEADLINE,
        "B's ring to evict its oldest rows",
        || {
            let h = w.verb(&format!("@{b} inbox --peek")).header().to_string();
            let dropped: u64 = kv(&h, "dropped").and_then(|n| n.parse().ok()).unwrap_or(0);
            (dropped > 0).then_some(h)
        },
    );
    assert!(
        header.contains(&format!("oldest_on_bus=@{first_off}")),
        "the evicted record's offset is still the oldest on the bus: {header}"
    );

    let got = w.verb(&format!("@{b} inbox get @{first_off}"));
    assert!(
        got.ok() && got.header().contains(&format!("off={first_off}")),
        "inbox get @{first_off}: {} — log:\n{}",
        got.header(),
        w.log_tail()
    );
    assert_eq!(
        String::from_utf8_lossy(got.body()),
        first_text,
        "the full body comes back from the log"
    );
    assert!(
        got.header().contains("from=a-fill0") && got.header().contains("kind=note"),
        "classified exactly as at delivery: {}",
        got.header()
    );

    let cross = w.verb(&format!("@{a} inbox get @{first_off}"));
    assert!(
        cross.header().starts_with("ERR no such record"),
        "a fetch of another session's record is refused: {}",
        cross.header()
    );

    let head = {
        let mut c = w.god();
        let (_, (_, head)) = c.fetch(0, &format!("/f/{FLEET}/in/>"), 0).expect("head");
        head
    };
    let missing = w.verb(&format!("@{b} inbox get @{}", head + 10_000));
    assert!(
        missing.header().starts_with("ERR no such record"),
        "an offset past the head is no record: {}",
        missing.header()
    );
}

/// **A RECEIPT TO A SENDER THE LIVE ROSTER NO LONGER LISTS IS NOT ROUTED BY THE
/// ROSTER THE BRIDGE REMEMBERS.**
#[test]
fn a_receipt_to_a_bare_session_sender_whose_exit_line_never_arrives_is_not_routed_to_its_face() {
    let w = World::boot_flags_with(
        "r15ackgone",
        &[],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    std::fs::write(w.state.join("drop-session-exited"), b"1\n").expect("arm the dropped line");
    let (a, b) = w.two_sessions();
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the sender's presence row to say live", || {
        w.row(&presence)
            .filter(|body| body.contains("state=live"))
            .map(|_| ())
    });

    let asked_off = {
        let mut g = w.god();
        let subject = format!("/f/{FLEET}/in/{}/{a}/{b}/ask", w.node);
        let mut body = aterm_link::body::Body::new(0);
        body.text = "decide-me".to_string();
        let (off, _) = g
            .publish(astream_cap::producer_id_of(&b), 1, &subject, &body.encode())
            .expect("publish the ask");
        off
    };
    let id = until("the ask to reach A", || id_at_off(&w, &a, asked_off));
    let row = w
        .inbox(&a)
        .into_iter()
        .find(|r| kv(r, "off") == Some(&asked_off.to_string()))
        .expect("the ask row");
    assert!(
        kv(&row, "from") == Some(b.as_str()) && kv(&row, "kind") == Some("ask"),
        "the sender is rendered as its bare sid, and the ask waits: {row}"
    );

    assert!(w.verb(&format!("@{b} close")).ok(), "close the sender");
    until("the sender to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });

    let seen = w.verb(&format!("@{a} inbox seen {id} handled"));
    assert!(seen.ok(), "inbox seen: {}", seen.header());
    let named = |e: &String| e.contains(&format!("re={asked_off} "));
    let settled = until("the receipt to be settled", || {
        w.ev()
            .into_iter()
            .find(|e| (e.starts_with("unacked ") || e.starts_with("ack ")) && named(e))
    });
    assert!(
        settled.starts_with("unacked ")
            && settled.contains("verdict=handled")
            && settled.contains("reason=no-lane"),
        "a sender aterm's own roster no longer lists has no lane: {settled}"
    );
    let ack_lane = format!("/f/{FLEET}/in/{}/{b}/{}/ack", w.node, w.node);
    assert_eq!(
        lane_len(&w, &ack_lane),
        0,
        "no receipt published onto the face of a session that has gone"
    );

    let later = w.verb(&format!("@{a} post to=@{a} kind=note --wait=60000 later"));
    assert!(later.ok(), "a later post drains: {}", later.header());
    let evs = w.ev();
    assert_eq!(
        evs.iter()
            .filter(|e| e.starts_with("unacked ") && named(e))
            .count(),
        1,
        "the receipt is retired once: {evs:?}"
    );
    assert!(
        !evs.iter().any(|e| e.starts_with("ack ") && named(e)),
        "and never published: {evs:?}"
    );
}
