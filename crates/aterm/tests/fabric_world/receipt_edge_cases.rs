// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Receipts and deadlines at their edges.** Every count is records on the BUS, read with `Fetch`.

#![cfg(unix)]

use crate::harness;

use aterm_link::body::Body;
use harness::{
    DEADLINE, FLEET, PERIODIC_DEADLINE, World, id_at_off, kv, off_of, until, until_within,
};
use std::time::{Duration, Instant};

/// Every record on `lane`, by `Fetch` from zero, paged.
fn lane_rows(w: &World, lane: &str) -> Vec<(u64, String, Vec<u8>)> {
    w.read(|c| {
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let (rows, (next, head)) = c.fetch(from, lane, 256)?;
            out.extend(rows);
            if next >= head || next <= from {
                return Ok(out);
            }
            from = next;
        }
    })
    .expect("fetch the lane")
}

fn lane_len(w: &World, lane: &str) -> usize {
    lane_rows(w, lane).len()
}

/// Records on `lane` whose body names `re=<off>`.
fn lane_re(w: &World, lane: &str, off: u64) -> usize {
    lane_rows(w, lane)
        .iter()
        .filter(|(_, _, raw)| Body::decode(raw).0.re == Some(off))
        .count()
}

/// Poll `f` for the fair wait, the harness's minute; `None` when it never
/// answered (NOT a panic) — for the counts that must be read after a fair wait
/// whatever they are.
fn poll<T>(f: impl FnMut() -> Option<T>) -> Option<T> {
    aterm_spec::wait::within(DEADLINE, Duration::from_millis(50), f)
}

/// X1. A RECEIPT GIVEN WHILE THE LINK IS DOWN IS ACKED ONCE WHEN IT RETURNS —
/// and the exactly-once re-post under `ERR fabric stalled queued=1` and a
/// broker restart still holds.
#[test]
fn x1_receipt_given_while_the_link_is_down_is_not_lost() {
    let mut w = World::boot_flags("rvx1", &["--receipts"]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);
    let note_lane = format!("/f/{FLEET}/in/{}/{b}/{}/note", w.node, w.node);

    let r = w.verb(&format!(
        "@{a} post to=@{b} kind=ask --wait=60000 take-this"
    ));
    assert!(r.ok(), "ask: {}", r.header());
    let off = off_of(r.header());
    let id = until("the ask to reach B", || id_at_off(&w, &b, off));

    w.stop_broker();

    let seen = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(seen.ok(), "inbox seen: {}", seen.header());

    let p1 = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-stall --wait=5000 stalled-once"
    ));
    let p2 = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-stall --wait=5000 stalled-once"
    ));
    eprintln!("stalled re-posts: {:?} / {:?}", p1.header(), p2.header());

    w.start_broker();
    until("the stalled note to reach B", || {
        w.inbox(&b)
            .iter()
            .any(|r| r.contains("text=stalled-once"))
            .then_some(())
    });
    let dup = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-stall --wait=60000 stalled-once"
    ));
    let notes = lane_len(&w, &note_lane);

    let _ = poll(|| (lane_len(&w, &ack_lane) > 0).then_some(()));
    let again = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(again.ok());
    w.drained(&b);
    let acks_after = lane_len(&w, &ack_lane);
    eprintln!(
        "re-post after restart: {} ; notes={notes} ; acks={acks_after}",
        dup.header()
    );

    assert_eq!(
        notes, 1,
        "a keyed re-post on a stalled fabric is ONE record"
    );
    assert!(dup.header().contains("dup=1"), "{}", dup.header());
    assert_eq!(
        acks_after, 1,
        "a verdict given while the link was down is acked exactly once"
    );
}

/// X1b. A RECEIPT GIVEN WHILE THE BRIDGE IS BEING REPLACED IS ACKED BY ITS
/// REPLACEMENT.
#[test]
fn x1b_receipt_given_while_the_bridge_restarts_is_not_lost() {
    let w = World::boot_flags("rvx1b", &["--receipts"]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);
    let r = w.verb(&format!(
        "@{a} post to=@{b} kind=ask --wait=60000 take-this"
    ));
    assert!(r.ok(), "ask: {}", r.header());
    let off = off_of(r.header());
    let id = until("the ask to reach B", || id_at_off(&w, &b, off));

    let pid = w.bridge_pid();
    harness::kill(pid, 9);
    let seen = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(seen.ok(), "inbox seen: {}", seen.header());
    until("a replacement bridge", || {
        let back: i32 = std::fs::read_to_string(w.state.join("pid"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (back != pid).then_some(())
    });
    until("the replacement bridge to attach", || {
        w.verb("status")
            .header()
            .contains("fabric=connected")
            .then_some(())
    });
    let _ = poll(|| (lane_len(&w, &ack_lane) > 0).then_some(()));
    let acks = lane_len(&w, &ack_lane);
    let again = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(again.ok());
    w.drained(&b);
    let acks_after = lane_len(&w, &ack_lane);
    eprintln!(
        "ack records on A's lane: after relaunch={acks} after same-word re-seen={acks_after}"
    );
    assert_eq!(
        (acks, acks_after),
        (1, 1),
        "a verdict given while the bridge was being replaced is acked once by its replacement"
    );
    let pins = std::fs::read_dir(w.state.join("acks"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(pins, 0, "the pinned sequence is forgotten at retirement");
}

/// X2. `truncated=1` IS FETCHABLE WHOLE BY OFFSET: the bus holds the full body,
/// and `inbox get @<off>` returns all of it — while the ring still holds the cut
/// row, and after the ring let it go.
#[test]
fn x2_a_truncated_body_is_fetchable_whole_by_offset() {
    let w = World::boot("rvx2", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let mut text = "x ".repeat(19_999);
    text.push('x');
    assert_eq!(text.len(), 39_999);
    let r = w
        .ctl()
        .request_with_body(
            &format!(
                "@{a} post to=@{b} kind=note --wait=60000 len={}",
                text.len()
            ),
            text.as_bytes(),
        )
        .expect("post frame");
    assert!(r.ok(), "post: {}", r.header());
    let off = off_of(r.header());
    let lane = format!("/f/{FLEET}/in/{}/{b}/{}/note", w.node, w.node);
    let rows = lane_rows(&w, &lane);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        Body::decode(&rows[0].2).0.text.len(),
        39_999,
        "the bus holds it whole"
    );

    let id = until("the note to reach B", || id_at_off(&w, &b, off));
    let by_id = w.verb(&format!("@{b} inbox get {id}"));
    assert!(
        by_id.header().contains("truncated=1 len=39999"),
        "the DELIVERY cut it: {}",
        by_id.header()
    );
    let ring = w.verb(&format!("@{b} inbox get @{off}"));
    eprintln!(
        "inbox get @{off} (ring row cut): {} (body {} bytes)",
        ring.header(),
        ring.body().len()
    );
    assert!(ring.ok(), "{}", ring.header());
    assert!(!ring.header().contains("truncated=1"), "{}", ring.header());
    assert_eq!(ring.body(), text.as_bytes(), "whole, byte for byte");

    const SRCS: u64 = 16;
    {
        let mut seq = [0u64; SRCS as usize];
        let fill: Vec<harness::Record> = (0..560u64)
            .map(|i| {
                let s = (i % SRCS) as usize;
                seq[s] += 1;
                let src = format!("a-fill{s}");
                let l = format!("/f/{FLEET}/in/{}/{b}/{src}/note", w.node);
                let mut body = Body::new(0);
                body.text = format!("filler-{i}");
                (astream_cap::producer_id_of(&src), seq[s], l, body.encode())
            })
            .collect();
        harness::publish_pipelined(&mut w.god(), &fill);
    }
    let hdr = until_within(PERIODIC_DEADLINE, "B's ring to drop rows", || {
        let h = w.verb(&format!("@{b} inbox --peek")).header().to_string();
        let dropped: u64 = kv(&h, "dropped").and_then(|n| n.parse().ok()).unwrap_or(0);
        (dropped > 0 && id_at_off(&w, &b, off).is_none()).then_some(h)
    });
    eprintln!("B's header after the fill: {hdr}");
    let bus = w.verb(&format!("@{b} inbox get @{off}"));
    eprintln!(
        "inbox get @{off} (bus): {} (body {} bytes)",
        bus.header(),
        bus.body().len()
    );

    let head = {
        let mut c = w.god();
        let (_, (_, head)) = c.fetch(0, &format!("/f/{FLEET}/in/>"), 0).expect("head");
        head
    };
    let beyond = w.verb(&format!("@{b} inbox get @{}", head + 10_000));
    assert!(
        beyond.header().starts_with("ERR no such record"),
        "{}",
        beyond.header()
    );

    assert!(bus.ok(), "{}", bus.header());
    assert_eq!(
        bus.body().len(),
        39_999,
        "inbox get @<off> from the log returns the body whole: {}",
        bus.header()
    );
}

/// X3. A REPLY ADDRESSED TO SOMEBODY ELSE DOES NOT CANCEL MY `expired`. A asks
/// B twice; B answers X — to C.
#[test]
fn x3_a_reply_to_another_session_does_not_cancel_my_expired() {
    let w = World::boot_with(
        "rvx3",
        &[],
        &[("ATERM_LINK_FAULT", "expire-deadlines-at-once-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let c = {
        let r = w.verb("spawn");
        assert!(r.ok(), "spawn: {}", r.header());
        r.header()
            .split_whitespace()
            .nth(1)
            .expect("sid")
            .to_string()
    };
    until("three sessions", || (w.sessions().len() >= 3).then_some(()));
    let expired_lane = format!("/f/{FLEET}/in/{}/{a}/{}/expired", w.node, w.node);
    let a_lane = format!("/f/{FLEET}/in/{}/{a}/>", w.node);

    const DL_MS: u64 = 600_000;
    let x = off_of(
        w.verb(&format!(
            "@{a} post to=@{b} kind=ask dl={DL_MS} --wait=60000 q-x"
        ))
        .header(),
    );
    let y = off_of(
        w.verb(&format!(
            "@{a} post to=@{b} kind=ask dl={DL_MS} --wait=60000 q-y"
        ))
        .header(),
    );
    let ans = w.verb(&format!(
        "@{b} post to=@{c} kind=answer re={x} --wait=60000 misrouted"
    ));
    assert!(ans.ok(), "answer: {}", ans.header());
    until("the misrouted answer to reach C", || {
        w.inbox(&c)
            .iter()
            .any(|r| r.contains("text=misrouted"))
            .then_some(())
    });
    assert_eq!(
        (lane_re(&w, &expired_lane, x), lane_re(&w, &expired_lane, y)),
        (0, 0),
        "no sweep may have run before the marker"
    );
    std::fs::write(w.state.join("expire-deadlines-now"), b"1\n")
        .expect("arm the impatient deadline sweep");
    until_within(PERIODIC_DEADLINE, "expired re=Y on A's lane", || {
        (lane_re(&w, &expired_lane, y) == 1).then_some(())
    });
    let _ = poll(|| (lane_re(&w, &expired_lane, x) > 0).then_some(()));
    let expired_x = lane_re(&w, &expired_lane, x);
    let replies_x_on_a = lane_rows(&w, &a_lane)
        .iter()
        .filter(|(_, s, raw)| !s.ends_with("/expired") && Body::decode(raw).0.re == Some(x))
        .count();
    eprintln!("A's lane: expired re=X -> {expired_x}, any reply re=X -> {replies_x_on_a}");
    assert_eq!(replies_x_on_a, 0, "A never heard back on X");
    assert_eq!(
        expired_x, 1,
        "A's ask X was answered to someone else, so A's bridge owes one `expired`"
    );
    let row = w
        .inbox(&c)
        .into_iter()
        .find(|r| r.contains("text=misrouted"))
        .expect("C's row");
    assert!(!row.contains("late=1"), "{row}");
}

/// X3, reached by a stranger: an `answer re=<X>` published onto B's lane (not
/// A's) settles nothing of A's.
#[test]
fn a_strangers_reply_to_someone_else_does_not_settle_the_askers_deadline() {
    const DL_MS: u64 = 600_000;
    let w = World::boot_with(
        "r15st",
        &[],
        &[("ATERM_LINK_FAULT", "expire-deadlines-at-once-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let expired_lane = format!("/f/{FLEET}/in/{}/{a}/{}/expired", w.node, w.node);
    let r = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl={DL_MS} --wait=60000 unanswered"
    ));
    assert!(r.ok(), "{}", r.header());
    let x = off_of(r.header());
    until("the ask at B", || id_at_off(&w, &b, x));
    let mut god = w.god();
    let mut body = Body::new(1);
    body.re = Some(x);
    body.text = "not-for-a".to_string();
    let (ans, _) = god
        .publish(
            astream_cap::producer_id_of("a-other"),
            1,
            &format!("/f/{FLEET}/in/{}/{b}/a-other/answer", w.node),
            &body.encode(),
        )
        .expect("answer to B");
    until("the stranger's answer at B", || id_at_off(&w, &b, ans));
    assert_eq!(
        lane_re(&w, &expired_lane, x),
        0,
        "no sweep may have run before the marker"
    );
    std::fs::write(w.state.join("expire-deadlines-now"), b"1\n")
        .expect("arm the impatient deadline sweep");
    let expired = until_within(PERIODIC_DEADLINE, "A's expired row", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| kv(r, "kind") == Some("expired") && kv(r, "re") == Some(&x.to_string()))
    });
    eprintln!("A's expired row: {expired}");
    let y = off_of(
        w.verb(&format!(
            "@{a} post to=@{b} kind=ask dl={DL_MS} --wait=60000 barrier"
        ))
        .header(),
    );
    until_within(PERIODIC_DEADLINE, "the barrier ask's expired row", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| kv(r, "kind") == Some("expired") && kv(r, "re") == Some(&y.to_string()))
    });
    let n = w
        .inbox(&a)
        .iter()
        .filter(|r| kv(r, "kind") == Some("expired") && kv(r, "re") == Some(&x.to_string()))
        .count();
    assert_eq!(
        n, 1,
        "A was never answered, so its bridge owes exactly one `expired`"
    );
}

/// X4. AN `ask dl=` DEDUPED ONTO A `note` UNDER THE SAME KEY RECORDS NO
/// `expired`: the re-post appended nothing, so there is no ask to expire.
#[test]
fn x4_an_ask_deduped_onto_a_note_records_no_expired() {
    let w = World::boot("rvx4", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let expired_lane = format!("/f/{FLEET}/in/{}/{a}/{}/expired", w.node, w.node);
    let note = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-kind --wait=60000 just-a-note"
    ));
    let x = off_of(note.header());
    let ask = w.verb(&format!(
        "@{a} post to=@{b} kind=ask key=k-kind dl=300 --wait=60000 an-ask"
    ));
    assert!(
        ask.header().contains(&format!("off={x}")) && ask.header().contains("dup=1"),
        "{}",
        ask.header()
    );
    let y = off_of(
        w.verb(&format!(
            "@{a} post to=@{b} kind=ask dl=300 --wait=60000 control"
        ))
        .header(),
    );
    until_within(PERIODIC_DEADLINE, "the control ask to expire", || {
        (lane_re(&w, &expired_lane, y) == 1).then_some(())
    });
    let n = lane_re(&w, &expired_lane, x);
    let b_asks = lane_len(&w, &format!("/f/{FLEET}/in/{}/{b}/{}/ask", w.node, w.node));
    eprintln!("expired re=<note off> on A's lane: {n}; ask records on B's lane: {b_asks}");
    assert_eq!(b_asks, 1, "only the control ask was ever published");
    assert_eq!(n, 0, "no expired verdict for a note that never waited");
    let dl = std::fs::read_to_string(w.state.join("deadlines")).unwrap_or_default();
    assert!(
        !dl.contains(&format!("off={x} ")),
        "no deadline was ever held for the note's offset: {dl:?}"
    );
}

/// X5. `post --wait-ack` with `dl=` and no receipt answers `ERR expired` when
/// the deadline passes first.
#[test]
fn x5_wait_ack_past_its_deadline_answers_expired() {
    let w = World::boot_flags("rvx5", &["--receipts"]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let answers: Vec<(String, bool)> = std::thread::scope(|scope| {
        let posts: Vec<_> = [700u64, 1500]
            .into_iter()
            .enumerate()
            .map(|(i, dl)| {
                let (w, a, b) = (&w, &a, &b);
                scope.spawn(move || {
                    let t = Instant::now();
                    let r = w.verb(&format!(
                        "@{a} post to=@{b} kind=ask dl={dl} --wait-ack nobody-acks-{i}"
                    ));
                    let took = t.elapsed();
                    let own = kv(r.header(), "off").is_some_and(|off| {
                        w.inbox(a).iter().any(|row| {
                            kv(row, "kind") == Some("expired") && kv(row, "re") == Some(off)
                        })
                    });
                    eprintln!(
                        "--wait-ack dl={dl} #{i}: {} after {took:?}; its own expired in A's inbox: {own}",
                        r.header()
                    );
                    (r.header().to_string(), own)
                })
            })
            .collect();
        posts
            .into_iter()
            .map(|post| post.join().expect("a --wait-ack post"))
            .collect()
    });
    {
        let mut c = w.god();
        let src = "n-00000000000000aa";
        let lane = format!("/f/{FLEET}/in/{}/{b}/{src}/ask", w.node);
        let mut body = Body::new(aterm_link::now_ms());
        body.from = Some("s-00000000000000beef00".to_string());
        body.text = "from-a-node-that-is-gone".to_string();
        let (off, _) = c
            .publish(astream_cap::producer_id_of(src), 1, &lane, &body.encode())
            .expect("publish the ghost ask");
        let id = until("the ghost ask to reach B", || id_at_off(&w, &b, off));
        assert!(w.verb(&format!("@{b} inbox seen {id} handled")).ok());
        let ghost_lane = format!(
            "/f/{FLEET}/in/{src}/{}/{}/ack",
            body.from.as_deref().expect("from"),
            w.node
        );
        let got = poll(|| (lane_len(&w, &ghost_lane) > 0).then_some(()));
        assert!(got.is_some(), "the ack to a gone node is queued on the bus");
    }
    assert!(
        answers
            .iter()
            .all(|(h, own)| h.starts_with("ERR expired") && *own),
        "--wait-ack bounded by dl= answers, each released by its own `expired`: {answers:?}"
    );
}

/// THE PRIVACY/LOAD PASS'S D1: a record on A's OWN lane whose body is over the
/// endpoint's 256 KiB `BODY_MAX` — refused at delivery, so exactly the row
/// `inbox get @<off>` exists for — is answered `truncated=1 len=` with ONE bus
/// read, not re-fetched on every drain until the 10 s wait times out (it was:
/// `ERR timeout` after 10 s and 35 `fetched … refused` lines).
#[test]
fn an_oversize_record_fetch_is_answered_once_truncated() {
    let w = World::boot("r15ov", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let mut god = w.god();
    let mut body = Body::new(1);
    body.text = "Z".repeat(300 * 1024);
    let (x, _) = god
        .publish(
            astream_cap::producer_id_of("a-big"),
            1,
            &format!("/f/{FLEET}/in/{}/{a}/a-big/note", w.node),
            &body.encode(),
        )
        .expect("publish the big note");
    until_within(DEADLINE, "the bridge to refuse delivering it", || {
        w.ev()
            .into_iter()
            .find(|e| e.contains(&format!("undeliverable off={x}")))
    });
    let t = Instant::now();
    let r = w.verb(&format!("@{a} inbox get @{x}"));
    let took = t.elapsed();
    let log = std::fs::read_to_string(&w.gui_log).unwrap_or_default();
    let refused = log.matches(&format!("fetched {x} for {a} refused")).count();
    eprintln!(
        "A inbox get @{x} (300 KiB body): {} after {took:?}; refused {refused} times",
        r.header()
    );
    assert!(r.ok(), "{}", r.header());
    assert!(took < DEADLINE, "promptly: {took:?}");
    assert_eq!(refused, 0, "the endpoint takes the answer the first time");
    assert!(
        r.header()
            .contains(&format!("truncated=1 len={}", 300 * 1024)),
        "{}",
        r.header()
    );
    assert_eq!(
        r.body().len(),
        256 * 1024,
        "cut at the endpoint's own bound"
    );
    assert!(r.body().iter().all(|b| *b == b'Z'));
}
