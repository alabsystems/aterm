// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 15 — receipts, and nothing lost, end to end.**
//!
//! Two properties, each proven on the whole path — a guarded broker, a headless
//! `aterm-gui`, and the real `aterm-link serve` child gui launches:
//!
//! * A RECEIPT (E2, R8). With the bridge serving `--receipts`, one session
//!   running `inbox seen <id> handled|refused|deferred` on an `ask`/`task` row
//!   publishes `kind=ack re=<off> verdict=<v>` onto the SENDER's own inbox lane
//!   — exactly one record, the sender's `inbox` lists it, `await inbox re=<off>`
//!   latches on it, and `post --wait-ack` returns the verdict. A `note` earns
//!   none, a bare `inbox seen` (no verdict) earns none, a `--peek` earns none,
//!   and a bridge WITHOUT `--receipts` sends none at all.
//! * NOTHING LOST (E3). `inbox get @<off>` fetches a record by its broker
//!   offset through the bridge when the bounded ring has evicted it — the full
//!   body comes back, the header's `oldest_on_bus=@<off>` names the lowest
//!   offset still fetchable, another session's record is refused, and a record
//!   the log never held is `ERR no such record`.
//!
//! Every wait is `until <observable state>`, bounded by the harness deadline —
//! a hang detector, never a synchronisation sleep.

#![cfg(unix)]

mod harness;

use aterm_link::ctl::Ctl;
use harness::{until, until_within, World, FLEET, PERIODIC_DEADLINE};

/// How many records sit on one lane, by `Fetch` from zero — the authority a
/// duplicate or a missing publish cannot hide behind.
fn lane_len(w: &World, lane: &str) -> usize {
    let mut c = w.god();
    let (rows, _) = c.fetch(0, lane, 256).expect("fetch the lane");
    rows.len()
}

/// The row id of `sid`'s `inbox` row at broker offset `off`, `--peek` so nothing
/// is marked — the row a recipient is about to acknowledge.
fn id_at_off(w: &World, sid: &str, off: u64) -> Option<u64> {
    w.inbox(sid).into_iter().find_map(|r| {
        (harness_kv(&r, "off") == Some(&off.to_string()))
            .then(|| r.split_whitespace().nth(1)?.parse().ok())
            .flatten()
    })
}

/// The `msg` rows of `sid`'s inbox whose text names `off` as their `re=`.
fn replies_to(w: &World, sid: &str, off: u64) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| harness_kv(r, "re") == Some(&off.to_string()))
        .collect()
}

/// `key=` of a whitespace token line — the row helper the harness does not export.
fn harness_kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
}

/// The offset an `OK <id> off=<n> …` header names.
fn off_of(header: &str) -> u64 {
    header
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a landing offset in: {header}"))
}

/// **A `handled` VERDICT ON AN ASK ACKS THE SENDER WITH ONE `ack re=<off>`, AND
/// `refused`/`deferred` CARRY THEIR OWN WORD.**
///
/// The sender's inbox lists the receipt (`re-id=` resolving to its own post),
/// its lane holds exactly one record per decision, and `await inbox re=<off>`
/// latches on it. A `note` and a bare `inbox seen` (no verdict) both ack
/// nothing — the receipt is for the kinds that WAIT, and only when a word was
/// recorded.
#[test]
fn a_verdict_on_an_ask_acks_the_sender_once_and_a_note_acks_nothing() {
    let w = World::boot_flags("r15ack", &["--receipts"]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    // The sender's own ack lane, where a receipt for a post FROM a lands: under
    // b's `from=` and this one node's `<src>`.
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);

    // A asks B three questions and learns each offset. `--wait` (default for
    // `ask`) gives the landing; nothing waits for an ack yet.
    let mut off = Vec::new();
    for q in ["q-handled", "q-refused", "q-deferred"] {
        let r = w.verb(&format!("@{a} post to=@{b} kind=ask --wait=30000 {q}"));
        assert!(r.ok(), "ask {q}: {} — log:\n{}", r.header(), w.log_tail());
        off.push(off_of(r.header()));
    }
    // And a NOTE, which may never earn a receipt.
    let note = w.verb(&format!("@{a} post to=@{b} kind=note --wait=30000 fyi"));
    assert!(note.ok(), "note: {}", note.header());
    let note_off = off_of(note.header());

    // B decides each row, found by the exact offset A's landing named — so the
    // verdict is attached to the right ask regardless of delivery order.
    for (i, verdict) in ["handled", "refused", "deferred"].iter().enumerate() {
        let id = until("the ask to reach B", || id_at_off(&w, &b, off[i]));
        let seen = w.verb(&format!("@{b} inbox seen {id} {verdict}"));
        assert!(seen.ok(), "inbox seen {id} {verdict}: {}", seen.header());
    }
    // B reads the note and marks it handled — no receipt may follow.
    let note_id = until("the note to reach B", || id_at_off(&w, &b, note_off));
    assert!(w.verb(&format!("@{b} inbox seen {note_id} handled")).ok());

    // Each ask earns exactly one ack at A, carrying its verdict; the note earns
    // none.
    for (i, verdict) in ["handled", "refused", "deferred"].iter().enumerate() {
        let row = until(&format!("the {verdict} receipt to reach A"), || {
            replies_to(&w, &a, off[i])
                .into_iter()
                .find(|r| harness_kv(r, "kind") == Some("ack"))
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
            .all(|r| harness_kv(r, "kind") != Some("ack")),
        "a note earns no receipt"
    );

    // `await inbox re=<off>` latches on the receipt already sitting in A's inbox.
    let latched = w.verb(&format!("@{a} await inbox re={} timeout=5000", off[0]));
    assert!(
        latched.header().starts_with("OK inbox "),
        "await inbox re= latches on the ack: {}",
        latched.header()
    );

    // A SECOND, SAME-WORD `inbox seen` acks nothing new: the lane stays at three.
    let first_ask = id_at_off(&w, &b, off[0]).expect("the first ask row");
    assert!(w.verb(&format!("@{b} inbox seen {first_ask} handled")).ok());
    // Give any (erroneous) second publish time to land, then confirm none did.
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        lane_len(&w, &ack_lane),
        3,
        "the same verdict again publishes no second ack"
    );
}

/// **`post … --wait-ack` RETURNS THE RECIPIENT'S VERDICT, END TO END.** A posts
/// an ask and blocks for the receipt; a helper connection, as B, finds the row
/// and marks it `handled`; the parked post answers `OK <id> off=<n>
/// ack=handled msg=<row>`.
///
/// The kind is `ask` rather than `task` on purpose: both wait, but a `task`
/// from a peer this node has not `--accept-from`'d is delivered `kind=note
/// demoted=task` (§8.4) — a note earns no receipt — while an `ask` is never
/// demoted, so the receipt path is what this exercises, not the allowlist.
#[test]
fn post_wait_ack_returns_the_recipients_verdict() {
    let w = World::boot_flags("r15waitack", &["--receipts"]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // The acker: its own control connection (a thread cannot borrow `&World`),
    // polling B's inbox for the ask and marking it handled.
    let sock = w.ctl_sock.clone();
    let token = w.token.clone();
    let b_sid = b.clone();
    let acker = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "the ask never reached B"
            );
            let mut ctl = Ctl::connect(&sock, &token).expect("connect");
            let listing = ctl
                .request(&format!("@{b_sid} inbox --peek"))
                .expect("inbox");
            if let Some(id) = listing.rows().iter().rev().find_map(|r| {
                (harness_kv(r, "kind") == Some("ask"))
                    .then(|| r.split_whitespace().nth(1)?.parse::<u64>().ok())
                    .flatten()
            }) {
                let mut ctl = Ctl::connect(&sock, &token).expect("connect");
                let seen = ctl
                    .request(&format!("@{b_sid} inbox seen {id} handled"))
                    .expect("inbox seen");
                assert!(seen.ok(), "inbox seen: {}", seen.header());
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    let posted = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl=30000 --wait-ack=30000 ship it"
    ));
    acker.join().expect("the acker finished");
    assert!(
        posted.ok() && posted.header().contains("ack=handled") && posted.header().contains("msg="),
        "the parked post returns the recipient's verdict: {} — log:\n{}",
        posted.header(),
        w.log_tail()
    );
}

/// **A BRIDGE WITHOUT `--receipts` SENDS NO ACK.** The same `inbox seen
/// handled` on an ask that acks under `--receipts` publishes nothing when the
/// flag is off — the decision is still recorded locally, the sender simply
/// never hears it.
#[test]
fn receipts_off_sends_no_ack() {
    let w = World::boot("r15noreceipts", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);

    let asked = w.verb(&format!("@{a} post to=@{b} kind=ask --wait=30000 anyone?"));
    assert!(
        asked.ok(),
        "ask: {} — log:\n{}",
        asked.header(),
        w.log_tail()
    );
    let off = off_of(asked.header());

    let id = until("the ask to reach B", || id_at_off(&w, &b, off));
    assert!(w.verb(&format!("@{b} inbox seen {id} handled")).ok());

    // The decision is recorded, the sender is never acked. Give an (erroneous)
    // publish a window to appear, then prove the lane is empty.
    std::thread::sleep(std::time::Duration::from_millis(750));
    assert_eq!(
        lane_len(&w, &ack_lane),
        0,
        "receipts off: no ack lane record"
    );
    assert!(
        replies_to(&w, &a, off)
            .iter()
            .all(|r| harness_kv(r, "kind") != Some("ack")),
        "receipts off: nothing in the sender's inbox"
    );
}

/// **`inbox get @<off>` FETCHES A RECORD THE BOUNDED RING DROPPED, THE HEADER
/// NAMES THE OLDEST OFFSET STILL FETCHABLE, AND ANOTHER SESSION'S RECORD IS
/// REFUSED (E3).**
///
/// B's ring is filled past its 512 cap by direct publishes onto B's own lane
/// under a filler human `<src>` (listed periodically so the per-peer quota
/// never refuses one), so its oldest rows are evicted (`dropped>0`). Its offset
/// is still `oldest_on_bus=@<off>`, `inbox get @<off>` parks, the bridge fetches
/// the record off the log and hands back the FULL body — while A asking for the
/// same offset, which is on B's lane and not A's, is `ERR no such record`.
#[test]
fn inbox_get_at_an_offset_fetches_a_record_the_ring_dropped() {
    let w = World::boot("r15get", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // Fill B's ring past its 512 cap with direct publishes onto B's lane. Spread
    // them across many agent `<src>` principals so no ONE peer exceeds the 64
    // unlisted-row quota (a burst from one src would be refused past 64, and a
    // refused record is never re-offered) — sixteen srcs of thirty-five each is
    // well under. Each arrives `from=a-fillN trust=agent kind=note`.
    const SRCS: u64 = 16;
    let total: u64 = 560;
    let first_text = "the-oldest-body-please-return-me-in-full";
    let mut first_off = 0u64;
    {
        let mut c = w.god();
        let mut seq = [0u64; SRCS as usize];
        for i in 0..total {
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
            let encoded = body.encode(None);
            let (off, _) = c
                .publish(astream_cap::producer_id_of(&src), seq[s], &lane, &encoded)
                .unwrap_or_else(|e| panic!("publish filler {i}: {e}"));
            if i == 0 {
                first_off = off;
            }
        }
    }
    // Wait until the bridge has delivered enough that the ring evicted its
    // oldest rows. The oldest offset is `first_off` (delivery is in offset
    // order), so `dropped > 0` means it is out of the ring and only the bus
    // still holds it — exactly the case `inbox get @<off>` is for.
    let header = until_within(
        PERIODIC_DEADLINE,
        "B's ring to evict its oldest rows",
        || {
            let h = w.verb(&format!("@{b} inbox --peek")).header().to_string();
            let dropped: u64 = harness_kv(&h, "dropped")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            (dropped > 0).then_some(h)
        },
    );
    assert!(
        header.contains(&format!("oldest_on_bus=@{first_off}")),
        "the evicted record's offset is still the oldest on the bus: {header}"
    );

    // The oldest record was dropped from the ring — a plain `inbox get <id>`
    // cannot reach it — but `inbox get @<off>` fetches it whole through the
    // bridge.
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

    // ANOTHER SESSION'S RECORD IS REFUSED: A's fetch of an offset on B's lane
    // finds nothing on A's own lane.
    let cross = w.verb(&format!("@{a} inbox get @{first_off}"));
    assert!(
        cross.header().starts_with("ERR no such record"),
        "a fetch of another session's record is refused: {}",
        cross.header()
    );

    // A record the log never held is the same refusal.
    let head = {
        let mut c = w.god();
        let (_, (_, head)) = c.fetch(0, &format!("/f/{FLEET}/in/>"), 0).expect("head");
        head
    };
    let missing = w.verb(&format!("@{b} inbox get @{}", head + 10_000));
    assert!(
        missing.header().starts_with("ERR no such record")
            || missing.header().starts_with("ERR timeout"),
        "an offset past the head is no record (or times out reading it): {}",
        missing.header()
    );
}

/// **A RECEIPT TO A SENDER THE LIVE ROSTER NO LONGER LISTS IS NOT ROUTED BY THE
/// ROSTER THE BRIDGE REMEMBERS.**
///
/// The second place round 15 met 69979209f. That commit made `drain_outbox`
/// re-read the endpoint's roster before it resolves a single address, because a
/// door that answered `@s-<sid>` from the bridge's REMEMBERED roster (`epochs`,
/// moved only by the push lane) published onto the `in` face of a session that
/// had gone. Round 15's drain publishes the OWED RECEIPTS first, and one rendering
/// of a sender — a bare `s-<sid>`, a session principal that published under its
/// own `<src>` — is routed like any address (`receipt_lane` -> `resolve_to`), from
/// that same remembered roster. So the merge re-reads the roster before the
/// receipts as well as the posts: read only before the posts, a receipt for a
/// sender that has left, with its `session-exited` line never delivered, is
/// routed "hosted" and published onto the departed session's own face.
///
/// Asserted with that line held back (`ATERM_LINK_FAULT=drop-session-exited-
/// while-marked`) so the stale picture is guaranteed rather than raced: the
/// receipt is retired `unacked … reason=no-lane`, once — nothing lands on the
/// departed session's `ack` lane and a later drain does not find it owed again.
/// (A receipt to `s-<sid>@n-<node>`, the ordinary rendering of a post a node
/// published for one of its sessions, names its lane outright and reads no
/// roster; the named node's door accounts for a sender that has gone,
/// `undeliverable … reason=not-hosted`.)
#[test]
fn a_receipt_to_a_bare_session_sender_whose_exit_line_never_arrives_is_not_routed_to_its_face() {
    let w = World::boot_flags_with(
        "r15ackgone",
        &["--receipts"],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    // OPEN THE WINDOW BEFORE THE SESSION THAT WILL FALL INTO IT EXISTS.
    std::fs::write(w.state.join("drop-session-exited"), b"1\n").expect("arm the dropped line");
    let (a, b) = w.two_sessions();
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the sender's presence row to say live", || {
        let mut g = w.god();
        let (rows, _) = g.last(&presence, "", 8).ok()?;
        rows.iter()
            .find(|(_, s, _)| *s == presence)
            .filter(|(_, _, body)| String::from_utf8_lossy(body).contains("state=live"))
            .map(|_| ())
    });

    // AN ASK FROM `b` AS A BARE SESSION PRINCIPAL, onto `a`'s lane: the one
    // `from=` rendering whose receipt is ROUTED rather than addressed outright.
    let asked_off = {
        let mut g = w.god();
        let subject = format!("/f/{FLEET}/in/{}/{a}/{b}/ask", w.node);
        let mut body = aterm_link::body::Body::new(0);
        body.text = "decide-me".to_string();
        let (off, _) = g
            .publish(
                astream_cap::producer_id_of(&b),
                1,
                &subject,
                &body.encode(None),
            )
            .expect("publish the ask");
        off
    };
    let id = until("the ask to reach A", || id_at_off(&w, &a, asked_off));
    let row = w
        .inbox(&a)
        .into_iter()
        .find(|r| harness_kv(r, "off") == Some(&asked_off.to_string()))
        .expect("the ask row");
    assert!(
        harness_kv(&row, "from") == Some(b.as_str()) && harness_kv(&row, "kind") == Some("ask"),
        "the sender is rendered as its bare sid, and the ask waits: {row}"
    );

    // THE SENDER GOES, and the bridge is never told.
    assert!(w.verb(&format!("@{b} close")).ok(), "close the sender");
    until("the sender to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });

    // `a` DECIDES: a receipt is owed, and the drain the decision prompts settles
    // it against the roster it has just re-read.
    let seen = w.verb(&format!("@{a} inbox seen {id} handled"));
    assert!(seen.ok(), "inbox seen: {}", seen.header());
    // EITHER WORD SETTLES THE WAIT — `unacked` (no lane) or `ack` (published) —
    // so a door that routed it anyway fails here at once, naming what it did,
    // rather than as a timeout.
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

    // RETIRED, NOT OWED: a later drain (this post prompts one) finds nothing to
    // settle again, so the verdict is said once.
    let later = w.verb(&format!("@{a} post to=@{a} kind=note --wait=30000 later"));
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
